#[cfg(feature = "server")]
use crate::metrics::{RateLimiter, setup_metrics, update_limits_metrics};
use crate::modem::{self, BoxType, SortType};
use clap::Parser;
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
    #[arg(long, default_value = "http://192.168.8.1")]
    pub modem_url: String,

    /// The URL of a remote smser server (e.g., "http://localhost:8080")
    #[arg(long)]
    pub remote_url: Option<String>,

    #[command(subcommand)]
    pub command: SmsCommand,
}

#[derive(clap::Subcommand, Debug, PartialEq)]
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

        /// The phone number to send alerts to
        #[cfg(feature = "alertmanager")]
        #[arg(long)]
        alert_to: Option<String>,

        /// Hourly SMS limit
        #[arg(long, default_value_t = 100)]
        hourly_limit: u32,

        /// Daily SMS limit
        #[arg(long, default_value_t = 1000)]
        daily_limit: u32,
    },
}

pub async fn run() {
    let args = Args::parse();

    match args.command {
        SmsCommand::Send {
            to,
            message,
            dry_run,
        } => {
            if let Some(remote_url) = args.remote_url {
                if dry_run {
                    println!("DRY RUN: Not sending message.");
                    return;
                }
                let client = reqwest::Client::new();
                let url = format!("{}/send-sms", remote_url.trim_end_matches('/'));
                let payload = serde_json::json!({
                    "to": to,
                    "message": message
                });

                match client.post(&url).json(&payload).send().await {
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
                let (session_id, token) = match modem::get_session_info(&args.modem_url).await {
                    Ok((s, t)) => (s, t),
                    Err(e) => {
                        eprintln!("Error getting session info: {}", e);
                        return;
                    }
                };
                println!("Session ID: {}", session_id);
                println!("Token: {}", token);

                if let Err(e) =
                    modem::send_sms(&args.modem_url, &session_id, &token, &to, &message, dry_run)
                        .await
                {
                    eprintln!("Error sending SMS: {}", e);
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
            let messages = if let Some(remote_url) = args.remote_url {
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
                                let msgs: Vec<crate::modem::SmsMessage> =
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
                let (session_id, token) = match modem::get_session_info(&args.modem_url).await {
                    Ok((s, t)) => (s, t),
                    Err(e) => {
                        eprintln!("Error getting session info: {}", e);
                        return;
                    }
                };
                println!("Session ID: {}", session_id);
                println!("Token: {}", token);

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
            hourly_limit,
            daily_limit,
        } => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::EnvFilter::new(
                    std::env::var("RUST_LOG")
                        .unwrap_or_else(|_| "smser=debug,tower_http=debug".into()),
                ))
                .with(tracing_subscriber::fmt::layer())
                .init();

            // Call server start function here
            println!("Starting server on port {}", port);

            let handle = setup_metrics();
            update_limits_metrics(hourly_limit, daily_limit);
            let rate_limiter = RateLimiter::new(hourly_limit, daily_limit);

            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = TcpListener::bind(&addr)
                .await
                .expect("Failed to bind to port");
            let (_tx, rx) = tokio::sync::oneshot::channel(); // Create a channel
            crate::server::start_server(
                listener,
                args.modem_url,
                rx,
                handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_to,
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem;

    #[test]
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
            } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert!(!dry_run);
            }
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
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
            } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert!(dry_run);
            }
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
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
    #[cfg(feature = "server")]
    fn test_args_parsing_serve() {
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
            } => {
                assert_eq!(port, 9000);
                #[cfg(feature = "alertmanager")]
                assert_eq!(alert_to, None);
                assert_eq!(hourly_limit, 50);
                assert_eq!(daily_limit, 500);
            }
            _ => panic!("Expected Serve command"),
        }
    }

    // These tests rely on the modem being unavailable, which is typically true during CI/CD or local development without a modem.
    // They verify that the error handling paths are correctly triggered.

    #[tokio::test]
    async fn test_get_session_info_error() {
        let result = modem::get_session_info("http://nonexistent.com").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_sms_dry_run() {
        let result = modem::send_sms(
            "http://nonexistent.com",
            "dummy_session_id",
            "dummy_token",
            "+1234567890",
            "Test message",
            true,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
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
}
