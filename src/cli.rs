#[cfg(feature = "server")]
use crate::metrics::{
    ClientLimit, RateLimiter, setup_metrics, update_client_limits_metrics, update_limits_metrics,
};
#[cfg(feature = "modem")]
use crate::modem;
use crate::types::{BoxType, SmsMessage, SortType};
use clap::{CommandFactory, FromArgMatches, Parser};
use serde_json;
#[cfg(feature = "server")]
use std::net::SocketAddr;
#[cfg(feature = "server")]
use tokio::net::TcpListener;
#[cfg(feature = "server")]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Simple program to send SMS via a Huawei E3372 modem
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// The URL of the modem (e.g., "http://192.168.8.1")
    ///
    /// HTTP, not HTTPS, and deliberately so: the Huawei E3372 in HiLink mode
    /// serves its API on port 80 only. Port 443 is closed and TLS does not
    /// negotiate, so there is no HTTPS to opt into. Code scanning flags the
    /// resulting URLs (`rust/non-https-url`); the finding is accurate but not
    /// actionable against this hardware.
    ///
    /// The exposure is bounded by the modem being link-local -- typically a
    /// USB-attached CDC ethernet device, so the traffic never reaches a shared
    /// network. If you point this at a modem across an untrusted network, that
    /// reasoning no longer holds and the plaintext SMS content and phone
    /// numbers are genuinely exposed.
    #[cfg(feature = "modem")]
    #[arg(long, default_value = "http://192.168.8.1", env = "SMSER_MODEM_URL")]
    pub modem_url: String,

    // When the modem feature is enabled, remote_url is optional (can talk directly to modem).
    // When the modem feature is disabled, remote_url is required (client-only mode).
    /// The URL of a remote smser server (e.g., "http://localhost:8080")
    #[cfg(feature = "modem")]
    #[arg(long, env = "SMSER_REMOTE_URL")]
    pub remote_url: Option<String>,

    /// The URL of a remote smser server (e.g., "http://localhost:8080")
    #[cfg(not(feature = "modem"))]
    #[arg(long, env = "SMSER_REMOTE_URL")]
    pub remote_url: String,

    #[command(subcommand)]
    pub command: SmsCommand,
}

#[derive(clap::Subcommand, Debug, PartialEq)]
// Serve carries many more fields than Send, so the enum is sized by Serve.
// Boxing would fix the lint at the cost of indirection in code that runs once
// at startup and is never in a hot path or a collection.
#[allow(clippy::large_enum_variant)]
pub enum SmsCommand {
    /// Send an SMS message
    Send {
        /// The destination phone number
        #[arg(short, long)]
        to: String,

        /// The message to send
        #[arg(short, long)]
        message: String,

        /// Do not actually send a message
        #[arg(long)]
        dry_run: bool,

        /// Client name for per-client rate limiting
        #[arg(long)]
        client: Option<String>,
    },
    /// Receive SMS messages
    Receive {
        /// How many messages to read.
        #[arg(long, default_value_t = 20)]
        count: u32,

        /// Sort in ascending order?
        #[arg(long)]
        ascending: bool,

        /// Prefer unread messages?
        #[arg(long)]
        unread_preferred: bool,

        /// Type of message box to read from (e.g., LocalInbox, LocalSent).
        #[arg(long, default_value_t = BoxType::LocalInbox)]
        box_type: BoxType,

        /// Sort messages by (e.g., Date, Phone, Index).
        #[arg(long, default_value_t = SortType::Date)]
        sort_by: SortType,

        /// Output messages in JSON format.
        #[arg(long)]
        json: bool,
    },
    /// Start the web server
    #[cfg(feature = "server")]
    Serve {
        /// The port to listen on
        #[arg(short, long, default_value_t = 8080)]
        port: u16,

        /// The phone number to send alerts to (default receiver for /alertmanager)
        #[cfg(feature = "alertmanager")]
        #[arg(long, env = "SMSER_ALERT_TO")]
        alert_to: Option<String>,

        /// Named alert receiver in format "name:phone_number" (can be repeated).
        /// Creates /alertmanager/:name endpoints.
        #[cfg(feature = "alertmanager")]
        #[arg(long = "alert-receiver", value_parser = parse_alert_receiver)]
        alert_receivers: Vec<(String, String)>,

        /// Hourly SMS limit
        #[arg(long, default_value_t = 100)]
        hourly_limit: u32,

        /// Daily SMS limit
        #[arg(long, default_value_t = 1000)]
        daily_limit: u32,

        /// Per-client rate limit in format "name:hourly:daily" (can be repeated)
        #[arg(long = "client-limit", value_parser = parse_client_limit)]
        client_limits: Vec<ClientLimit>,

        /// Path to TLS certificate file
        #[arg(long)]
        tls_cert: Option<std::path::PathBuf>,

        /// Path to TLS key file
        #[arg(long)]
        tls_key: Option<std::path::PathBuf>,

        /// Port for HTTP to HTTPS redirect (only used when TLS is enabled)
        #[arg(long)]
        http_redirect_port: Option<u16>,

        /// Hostname to use for HTTPS redirects (defaults to request Host header)
        #[arg(long)]
        redirect_host: Option<String>,

        /// Log sensitive data (phone numbers, message content) - disable for privacy
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        log_sensitive: bool,

        /// Interval in seconds for polling new SMS messages (0 to disable)
        #[arg(long, default_value_t = 300, env = "SMSER_POLL_INTERVAL")]
        poll_interval: u64,

        /// Sender whose messages carry the account balance, e.g. "7950014111".
        ///
        /// Substring match, so the national part covers replies arriving from
        /// the international form. Required to enable balance tracking:
        /// filtering by sender is what stops a stale message from a former
        /// provider being read as a zero balance.
        #[arg(long)]
        balance_from: Option<String>,

        /// Regex with one capture group holding the numeric balance, e.g.
        /// `£([0-9]+\.[0-9]{2})`.
        ///
        /// Anchor it on the currency rather than the word "balance" -- in
        /// "balance (last usage - 28-09-2026) was £10.00" the first number
        /// after "balance" is the day of the month.
        #[arg(long)]
        balance_pattern: Option<String>,

        /// Value of the `provider` metric label.
        #[arg(long, default_value = "provider")]
        balance_provider: String,

        /// Value of the `currency` metric label. Not parsed from the message.
        #[arg(long, default_value = "GBP")]
        balance_currency: String,

        /// Send a balance request to this number on a schedule. Omit for
        /// passive operation: parse whatever arrives and never send, which
        /// costs nothing. Note this is the number you text, which is often not
        /// the number the reply comes from.
        #[arg(long)]
        balance_request_to: Option<String>,

        /// Body of the scheduled request.
        #[arg(long, default_value = "BALANCE")]
        balance_request_text: String,

        /// Days between scheduled requests.
        #[arg(long, default_value_t = 7)]
        balance_interval_days: u64,
    },
}

#[cfg(feature = "server")]
fn parse_client_limit(s: &str) -> Result<ClientLimit, String> {
    ClientLimit::parse(s)
}

#[cfg(feature = "alertmanager")]
fn parse_alert_receiver(s: &str) -> Result<(String, String), String> {
    let pos = s.find(':').ok_or_else(|| {
        format!(
            "invalid receiver format '{}', expected name:phone_number",
            s
        )
    })?;
    let name = s[..pos].to_string();
    let phone = s[pos + 1..].to_string();
    if name.is_empty() {
        return Err("receiver name must not be empty".to_string());
    }
    if phone.is_empty() {
        return Err("receiver phone number must not be empty".to_string());
    }
    Ok((name, phone))
}

/// Flags whose values are phone numbers. Masked on `/flags` unless
/// `--log-sensitive` is on.
#[cfg(feature = "server")]
const SENSITIVE_FLAGS: &[&str] = &[
    "alert_to",
    "alert_receivers",
    "balance_from",
    "balance_request_to",
];

/// Resolve every global and `serve` flag to its effective value and the
/// source of that value, for the `/flags` page.
///
/// Iterates clap's own argument list rather than the parsed struct so new
/// flags show up without touching this function, and so feature-gated flags
/// are handled by whatever clap was built with.
#[cfg(feature = "server")]
fn collect_flags(matches: &clap::ArgMatches, log_sensitive: bool) -> Vec<crate::server::Flag> {
    use clap::parser::ValueSource;

    let cmd = Args::command();
    let serve_cmd = cmd
        .find_subcommand("serve")
        .expect("serve subcommand exists");
    let serve_matches = matches
        .subcommand_matches("serve")
        .expect("called only for serve");

    let mut flags = Vec::new();
    for (cmd, matches) in [(&cmd, matches), (serve_cmd, serve_matches)] {
        for arg in cmd.get_arguments() {
            let id = arg.get_id().as_str();
            if matches!(id, "help" | "version") {
                continue;
            }
            let mask = !log_sensitive && SENSITIVE_FLAGS.contains(&id);
            let value = matches.get_raw(id).map(|values| {
                values
                    .map(|v| {
                        let v = v.to_string_lossy();
                        if !mask {
                            v.into_owned()
                        } else if id == "alert_receivers" {
                            // Keep the receiver name, it is what makes the
                            // endpoint identifiable.
                            match v.split_once(':') {
                                Some((name, _)) => format!("{}:(hidden)", name),
                                None => "(hidden)".to_string(),
                            }
                        } else {
                            "(hidden)".to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            let source = match matches.value_source(id) {
                Some(ValueSource::CommandLine) => "command line".to_string(),
                Some(ValueSource::EnvVariable) => match arg.get_env() {
                    Some(env) => format!("env {}", env.to_string_lossy()),
                    None => "env".to_string(),
                },
                Some(ValueSource::DefaultValue) => "default".to_string(),
                Some(_) => "other".to_string(),
                None => "unset".to_string(),
            };
            flags.push(crate::server::Flag {
                name: arg
                    .get_long()
                    .map(|l| format!("--{}", l))
                    .unwrap_or_else(|| id.to_string()),
                value,
                source,
            });
        }
    }
    flags
}

pub async fn run() {
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    match args.command {
        SmsCommand::Send {
            to,
            message,
            dry_run,
            client,
        } => {
            // Determine if we should use remote server
            #[cfg(feature = "modem")]
            let use_remote = args.remote_url.is_some();
            #[cfg(not(feature = "modem"))]
            let use_remote = true;

            if use_remote {
                #[cfg(feature = "modem")]
                let remote_url = args.remote_url.as_ref().unwrap();
                #[cfg(not(feature = "modem"))]
                let remote_url = &args.remote_url;

                if dry_run {
                    println!("DRY RUN: Not sending message.");
                    return;
                }
                let http_client = reqwest::Client::new();
                let url = format!("{}/send-sms", remote_url.trim_end_matches('/'));
                let payload = serde_json::json!({
                    "to": to,
                    "message": message,
                    "client": client
                });

                match http_client.post(&url).json(&payload).send().await {
                    Ok(res) => {
                        if res.status().is_success() {
                            println!("SMS sent successfully via remote server!");
                        } else {
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            eprintln!("Error sending SMS via remote server: {} - {}", status, body);
                        }
                    }
                    Err(e) => eprintln!("Failed to connect to remote server: {}", e),
                }
            } else {
                #[cfg(feature = "modem")]
                {
                    let (session_id, token) = match modem::get_session_info(&args.modem_url).await {
                        Ok((s, t)) => (s, t),
                        Err(e) => {
                            eprintln!("Error getting session info: {}", e);
                            return;
                        }
                    };

                    match modem::send_sms(
                        &args.modem_url,
                        &session_id,
                        &token,
                        &to,
                        &message,
                        dry_run,
                    )
                    .await
                    {
                        Ok(()) => {
                            if dry_run {
                                println!("DRY RUN: Not sending message.");
                            } else {
                                println!("SMS sent successfully!");
                            }
                        }
                        Err(e) => eprintln!("Error sending SMS: {}", e),
                    }
                }
            }
        }
        SmsCommand::Receive {
            count,
            ascending,
            unread_preferred,
            box_type,
            sort_by,
            json,
        } => {
            // Determine if we should use remote server
            #[cfg(feature = "modem")]
            let use_remote = args.remote_url.is_some();
            #[cfg(not(feature = "modem"))]
            let use_remote = true;

            let messages = if use_remote {
                #[cfg(feature = "modem")]
                let remote_url = args.remote_url.as_ref().unwrap();
                #[cfg(not(feature = "modem"))]
                let remote_url = &args.remote_url;

                let client = reqwest::Client::new();
                let url = format!("{}/get-sms", remote_url.trim_end_matches('/'));

                // Construct query parameters manually to match server's GetSmsRequest
                let mut params = vec![
                    ("count", count.to_string()),
                    ("ascending", ascending.to_string()),
                    ("unread_preferred", unread_preferred.to_string()),
                ];

                // For enums, we need to pass their integer values or string representations that axum expects.
                // Our server uses #[serde(default = "...")] which might expect strings if they are derived.
                // Actually BoxType and SortType derive Serialize_repr/Deserialize_repr, so they expect integers.
                params.push(("box_type", (box_type.clone() as i32).to_string()));
                params.push(("sort_by", (sort_by.clone() as i32).to_string()));

                match client.get(&url).query(&params).send().await {
                    Ok(res) => {
                        if res.status().is_success() {
                            let remote_res: serde_json::Value =
                                res.json().await.unwrap_or_default();
                            // The server returns {"status": "success", "messages": [...]}
                            if let Some(msgs_val) = remote_res.get("messages") {
                                let msgs: Vec<SmsMessage> =
                                    serde_json::from_value(msgs_val.clone()).unwrap_or_default();
                                msgs
                            } else {
                                eprintln!("Invalid response from remote server: {}", remote_res);
                                return;
                            }
                        } else {
                            let status = res.status();
                            let body = res.text().await.unwrap_or_default();
                            eprintln!(
                                "Error receiving SMS via remote server: {} - {}",
                                status, body
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to connect to remote server: {}", e);
                        return;
                    }
                }
            } else {
                #[cfg(feature = "modem")]
                {
                    let (session_id, token) = match modem::get_session_info(&args.modem_url).await {
                        Ok((s, t)) => (s, t),
                        Err(e) => {
                            eprintln!("Error getting session info: {}", e);
                            return;
                        }
                    };

                    let params = modem::SmsListParams {
                        box_type,
                        sort_type: sort_by,
                        read_count: count,
                        ascending,
                        unread_preferred,
                    };

                    match modem::get_sms_list(&args.modem_url, &session_id, &token, params).await {
                        Ok(response) => response.messages.message,
                        Err(e) => {
                            eprintln!("Error receiving SMS: {}", e);
                            return;
                        }
                    }
                }
                #[cfg(not(feature = "modem"))]
                unreachable!()
            };

            if json {
                match serde_json::to_string_pretty(&messages) {
                    Ok(json_output) => println!("{}", json_output),
                    Err(e) => eprintln!("Error serializing to JSON: {}", e),
                }
            } else {
                println!("Received {} SMS messages:", messages.len());
                for msg in messages {
                    println!("  From: {}", msg.phone);
                    println!("  Content: {}", msg.content);
                    println!("  Date: {}", msg.date);
                    println!("  Priority: {}", msg.priority);
                    println!("  SmsType: {}", msg.sms_type);
                    println!("  Smstat: {}", msg.smstat);
                    println!("  SaveType: {}", msg.save_type);
                    println!("  --------------------");
                }
            }
        }
        #[cfg(feature = "server")]
        SmsCommand::Serve {
            port,
            #[cfg(feature = "alertmanager")]
            alert_to,
            #[cfg(feature = "alertmanager")]
            alert_receivers,
            hourly_limit,
            daily_limit,
            client_limits,
            tls_cert,
            tls_key,
            http_redirect_port,
            redirect_host,
            log_sensitive,
            poll_interval,
            balance_from,
            balance_pattern,
            balance_provider,
            balance_currency,
            balance_request_to,
            balance_request_text,
            balance_interval_days,
        } => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::EnvFilter::new(
                    std::env::var("RUST_LOG")
                        .unwrap_or_else(|_| "smser=debug,tower_http=debug".into()),
                ))
                .with(tracing_subscriber::fmt::layer())
                .init();

            if http_redirect_port.is_some() && redirect_host.is_none() {
                eprintln!(
                    "Error: --http-redirect-port requires --redirect-host to avoid open redirects."
                );
                return;
            }

            // Call server start function here
            println!("Starting server on port {}", port);
            if !client_limits.is_empty() {
                println!(
                    "Per-client limits: {}",
                    client_limits
                        .iter()
                        .map(|cl| format!("{}:{}/{}", cl.name, cl.hourly_limit, cl.daily_limit))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            // Balance tracking needs both a sender to trust and a pattern.
            // Supplying one without the other is a configuration mistake worth
            // refusing rather than silently ignoring.
            #[cfg(feature = "server")]
            let balance = match (balance_from, balance_pattern) {
                (Some(from), Some(pat)) => match regex::Regex::new(&pat) {
                    Ok(re) if re.captures_len() >= 2 => Some(crate::balance::BalanceConfig {
                        from,
                        pattern: re,
                        provider: balance_provider,
                        currency: balance_currency,
                        request_to: balance_request_to,
                        request_text: balance_request_text,
                        interval: std::time::Duration::from_secs(
                            balance_interval_days.saturating_mul(86_400),
                        ),
                    }),
                    Ok(_) => {
                        eprintln!(
                            "Error: --balance-pattern must contain one capture group for the value, \
                             e.g. '£([0-9]+\\.[0-9]{{2}})'"
                        );
                        return;
                    }
                    Err(e) => {
                        eprintln!("Error: --balance-pattern is not a valid regex: {}", e);
                        return;
                    }
                },
                (None, None) => None,
                _ => {
                    eprintln!(
                        "Error: --balance-from and --balance-pattern must be given together."
                    );
                    return;
                }
            };

            let handle = setup_metrics();
            update_limits_metrics(hourly_limit, daily_limit);
            update_client_limits_metrics(&client_limits);
            let rate_limiter = RateLimiter::new(hourly_limit, daily_limit, client_limits);

            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = TcpListener::bind(&addr)
                .await
                .expect("Failed to bind to port");
            let (_tx, rx) = tokio::sync::oneshot::channel(); // Create a channel
            let config = crate::server::ServerConfig {
                modem_url: args.modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: alert_to,
                #[cfg(feature = "alertmanager")]
                alert_receivers: alert_receivers.into_iter().collect(),
                tls_cert,
                tls_key,
                http_redirect_port,
                redirect_host,
                log_sensitive,
                poll_interval,
                balance,
                flags: collect_flags(&matches, log_sensitive),
            };
            if let Some(ref b) = config.balance {
                if b.is_active() {
                    println!(
                        "Balance tracking: {} every {} day(s), replies from {}",
                        b.request_text,
                        b.interval.as_secs() / 86_400,
                        b.from
                    );
                } else {
                    println!("Balance tracking: passive, parsing replies from {}", b.from);
                }
            }
            if poll_interval > 0 {
                println!("SMS polling enabled: every {} seconds", poll_interval);
            } else {
                println!("SMS polling disabled");
            }
            crate::server::start_server(listener, rx, config).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "modem", feature = "server"))]
    use super::*;
    #[cfg(feature = "modem")]
    use crate::modem;

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_send_short_flags() {
        let args = Args::try_parse_from([
            "smser",
            "--modem-url",
            "http://test.com",
            "send",
            "-t",
            "1234567890",
            "-m",
            "Hello, world!",
        ])
        .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Send {
                to,
                message,
                dry_run,
                ..
            } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert!(!dry_run);
            }
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_send_long_flags() {
        let args = Args::try_parse_from([
            "smser",
            "--modem-url",
            "http://test.com",
            "send",
            "--to",
            "1234567890",
            "--message",
            "Hello, world!",
            "--dry-run",
        ])
        .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Send {
                to,
                message,
                dry_run,
                ..
            } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert!(dry_run);
            }
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_receive() {
        let args = Args::try_parse_from([
            "smser",
            "--modem-url",
            "http://test.com",
            "receive",
            "--count",
            "50",
            "--ascending",
            "--unread-preferred",
            "--box-type",
            "local-sent",
            "--sort-by",
            "phone",
            "--json",
        ])
        .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Receive {
                count,
                ascending,
                unread_preferred,
                box_type,
                sort_by,
                json,
            } => {
                assert_eq!(count, 50);
                assert!(ascending);
                assert!(unread_preferred);
                assert_eq!(box_type, BoxType::LocalSent);
                assert_eq!(sort_by, SortType::Phone);
                assert!(json);
            }
            _ => panic!("Expected Receive command"),
        }
    }

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_modem_url_env() {
        temp_env::with_var("SMSER_MODEM_URL", Some("http://env-modem:8080"), || {
            let args = Args::try_parse_from(["smser", "send", "-t", "1234567890", "-m", "Hello"])
                .expect("Failed to parse arguments");
            assert_eq!(args.modem_url, "http://env-modem:8080");
        });
    }

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_remote_url() {
        let args = Args::try_parse_from([
            "smser",
            "--remote-url",
            "http://remote-server:5566",
            "send",
            "-t",
            "1234567890",
            "-m",
            "Hello",
        ])
        .expect("Failed to parse arguments");
        assert_eq!(
            args.remote_url,
            Some("http://remote-server:5566".to_string())
        );
    }

    #[test]
    #[cfg(feature = "modem")]
    fn test_args_parsing_remote_url_env() {
        temp_env::with_var("SMSER_REMOTE_URL", Some("http://env-server:7788"), || {
            let args = Args::try_parse_from(["smser", "send", "-t", "1234567890", "-m", "Hello"])
                .expect("Failed to parse arguments");
            assert_eq!(args.remote_url, Some("http://env-server:7788".to_string()));
        });
    }

    #[test]
    #[cfg(feature = "server")]
    #[cfg(feature = "alertmanager")]
    fn test_args_parsing_alert_to_env() {
        temp_env::with_var("SMSER_ALERT_TO", Some("+447700900123"), || {
            let args = Args::try_parse_from(["smser", "serve"]).expect("Failed to parse arguments");
            match args.command {
                SmsCommand::Serve {
                    #[cfg(feature = "alertmanager")]
                    alert_to,
                    ..
                } => {
                    assert_eq!(alert_to, Some("+447700900123".to_string()));
                }
                _ => panic!("Expected Serve command"),
            }
        });
    }

    #[test]
    #[cfg(feature = "server")]
    #[cfg(feature = "alertmanager")]
    fn test_args_parsing_alert_receivers() {
        temp_env::with_var("SMSER_ALERT_TO", None::<String>, || {
            let args = Args::try_parse_from([
                "smser",
                "serve",
                "--alert-receiver",
                "oncall:+441234567890",
                "--alert-receiver",
                "management:+449876543210",
            ])
            .expect("Failed to parse arguments");
            match args.command {
                SmsCommand::Serve {
                    alert_receivers, ..
                } => {
                    assert_eq!(alert_receivers.len(), 2);
                    assert!(
                        alert_receivers
                            .contains(&("oncall".to_string(), "+441234567890".to_string()))
                    );
                    assert!(
                        alert_receivers
                            .contains(&("management".to_string(), "+449876543210".to_string()))
                    );
                }
                _ => panic!("Expected Serve command"),
            }
        });
    }

    #[test]
    #[cfg(feature = "server")]
    fn test_args_parsing_serve() {
        temp_env::with_vars(
            [
                ("SMSER_ALERT_TO", None::<String>),
                ("SMSER_MODEM_URL", None::<String>),
                ("SMSER_REMOTE_URL", None::<String>),
                ("SMSER_PORT", None::<String>),
            ],
            || {
                let args = Args::try_parse_from([
                    "smser",
                    "--modem-url",
                    "http://test.com",
                    "serve",
                    "--port",
                    "9000",
                    "--hourly-limit",
                    "50",
                    "--daily-limit",
                    "500",
                ])
                .expect("Failed to parse arguments");
                assert_eq!(args.modem_url, "http://test.com");
                match args.command {
                    SmsCommand::Serve {
                        port,
                        #[cfg(feature = "alertmanager")]
                        alert_to,
                        hourly_limit,
                        daily_limit,
                        tls_cert,
                        tls_key,
                        ..
                    } => {
                        assert_eq!(port, 9000);
                        #[cfg(feature = "alertmanager")]
                        assert_eq!(alert_to, None);
                        assert_eq!(hourly_limit, 50);
                        assert_eq!(daily_limit, 500);
                        assert_eq!(tls_cert, None);
                        assert_eq!(tls_key, None);
                    }
                    _ => panic!("Expected Serve command"),
                }
            },
        );
    }

    // These tests rely on the modem being unavailable, which is typically true during CI/CD or local development without a modem.
    // They verify that the error handling paths are correctly triggered.

    #[tokio::test]
    #[cfg(feature = "modem")]
    async fn test_get_session_info_error() {
        let result = modem::get_session_info("http://nonexistent.com").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "modem")]
    async fn test_send_sms_dry_run() {
        let result = modem::send_sms(
            "http://nonexistent.com",
            "dummy_session_id",
            "dummy_token",
            "+12 34 567 890",
            "Test message",
            true,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[cfg(feature = "modem")]
    async fn test_send_sms_error() {
        let result = modem::send_sms(
            "http://nonexistent.com",
            "dummy_session_id",
            "dummy_token",
            "+1234567890",
            "Test message",
            false,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "modem")]
    async fn test_get_sms_list_error() {
        let params = modem::SmsListParams {
            box_type: BoxType::LocalInbox,
            sort_type: SortType::Date,
            read_count: 20,
            ascending: false,
            unread_preferred: false,
        };
        let result = modem::get_sms_list(
            "http://nonexistent.com",
            "dummy_session_id",
            "dummy_token",
            params,
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    #[cfg(feature = "server")]
    fn test_collect_flags_sources_and_masking() {
        let argv = vec![
            "smser",
            "serve",
            "--hourly-limit",
            "5",
            "--balance-from",
            "7950014111",
        ];
        #[cfg(feature = "alertmanager")]
        let argv = [argv, vec!["--alert-receiver", "ops:+441234"]].concat();
        let matches = Args::command().try_get_matches_from(argv).unwrap();
        let find = |flags: &[crate::server::Flag], name: &str| {
            flags
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{} missing", name))
                .clone()
        };

        let flags = collect_flags(&matches, false);
        assert!(flags.iter().all(|f| f.name != "--help"));
        let hourly = find(&flags, "--hourly-limit");
        assert_eq!(hourly.value.as_deref(), Some("5"));
        assert_eq!(hourly.source, "command line");
        let daily = find(&flags, "--daily-limit");
        assert_eq!(daily.value.as_deref(), Some("1000"));
        assert_eq!(daily.source, "default");
        let tls = find(&flags, "--tls-cert");
        assert_eq!(tls.value, None);
        assert_eq!(tls.source, "unset");
        assert!(flags.iter().any(|f| f.name == "--modem-url"));
        assert_eq!(
            find(&flags, "--balance-from").value.as_deref(),
            Some("(hidden)")
        );
        #[cfg(feature = "alertmanager")]
        assert_eq!(
            find(&flags, "--alert-receiver").value.as_deref(),
            Some("ops:(hidden)")
        );

        let flags = collect_flags(&matches, true);
        assert_eq!(
            find(&flags, "--balance-from").value.as_deref(),
            Some("7950014111")
        );
        #[cfg(feature = "alertmanager")]
        assert_eq!(
            find(&flags, "--alert-receiver").value.as_deref(),
            Some("ops:+441234")
        );
    }
}
