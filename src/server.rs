#[cfg(feature = "alertmanager")]
use crate::alertmanager::{self, AlertManagerWebhook};
use crate::buildinfo;
use crate::metrics::RateLimiter;
use crate::modem::{self, BoxType, Error as ModemError, SortType}; // Import modem module and alias Error
use axum::http::StatusCode; // For HTTP status codes
use axum::response::Html;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use metrics::{counter, gauge};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

#[cfg(feature = "server")]
use axum_server::Handle;
#[cfg(feature = "server")]
use axum_server::tls_rustls::RustlsConfig;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

#[derive(Debug, Deserialize, Serialize)]
pub struct SendSmsRequest {
    pub to: String,
    pub message: String,
    /// Optional client name for per-client rate limiting
    #[serde(default)]
    pub client: Option<String>,
}

pub struct ServerConfig {
    pub modem_url: String,
    pub prometheus_handle: PrometheusHandle,
    pub rate_limiter: RateLimiter,
    #[cfg(feature = "alertmanager")]
    pub alert_phone_number: Option<String>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Port for HTTP to HTTPS redirect (only used when TLS is enabled)
    pub http_redirect_port: Option<u16>,
    /// Hostname to use for HTTPS redirects (defaults to request Host header)
    pub redirect_host: Option<String>,
    /// Whether to log sensitive data (phone numbers, message content)
    pub log_sensitive: bool,
}

#[derive(Clone)]
struct AppState {
    modem_url: String,
    rate_limiter: RateLimiter,
    prometheus_handle: PrometheusHandle,
    #[cfg(feature = "alertmanager")]
    alert_phone_number: Option<String>,
    start_time: Instant,
    tls_enabled: bool,
    log_sensitive: bool,
}

use tokio::sync::oneshot; // New import

pub async fn start_server(
    listener: TcpListener,
    shutdown_signal: oneshot::Receiver<()>,
    config: ServerConfig,
) {
    // Install default crypto provider for rustls 0.23+
    #[cfg(feature = "server")]
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let start_time = Instant::now();
    let start_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    // Set the start time metric
    gauge!("smser_start_time_seconds").set(start_timestamp);

    // Set the version info metric
    gauge!("smser_version_info", "version" => buildinfo::version(), "git_hash" => buildinfo::git_hash()).set(1.0);

    let tls_enabled = config.tls_cert.is_some() && config.tls_key.is_some();
    let app_state = AppState {
        modem_url: config.modem_url.clone(),
        rate_limiter: config.rate_limiter,
        prometheus_handle: config.prometheus_handle,
        #[cfg(feature = "alertmanager")]
        alert_phone_number: config.alert_phone_number,
        start_time,
        tls_enabled,
        log_sensitive: config.log_sensitive,
    };

    let app = Router::new()
        .route("/", get(handler))
        .route("/send-sms", post(send_sms_handler))
        .route("/get-sms", get(get_sms_handler))
        .route("/metrics", get(metrics_handler))
        .route("/status", get(status_handler))
        .route("/statusz", get(status_handler));

    #[cfg(feature = "alertmanager")]
    let app = app.route("/alertmanager", post(alertmanager_handler));

    let app = app.layer(TraceLayer::new_for_http()).with_state(app_state); // Pass state to the router

    // run it
    let addr = listener.local_addr().unwrap();
    println!("listening on {}", addr);

    if let (Some(cert), Some(key)) = (config.tls_cert, config.tls_key) {
        let tls_config = RustlsConfig::from_pem_file(cert, key)
            .await
            .expect("Failed to load TLS certificate and key");

        // Start HTTP redirect server if configured
        if let Some(http_port) = config.http_redirect_port {
            let https_port = addr.port();
            let redirect_host = config.redirect_host.clone();
            tokio::spawn(async move {
                let redirect_app = Router::new().fallback(move |req: axum::extract::Request| {
                    let redirect_host = redirect_host.clone();
                    async move {
                        let host = if let Some(ref canonical_host) = redirect_host {
                            canonical_host.as_str()
                        } else {
                            let req_host = req
                                .headers()
                                .get("host")
                                .and_then(|h| h.to_str().ok())
                                .unwrap_or("localhost");
                            // Remove port from host if present
                            req_host.split(':').next().unwrap_or(req_host)
                        };
                        let path = req
                            .uri()
                            .path_and_query()
                            .map(|p| p.as_str())
                            .unwrap_or("/");
                        let redirect_url = format!("https://{}:{}{}", host, https_port, path);
                        axum::response::Redirect::permanent(&redirect_url)
                    }
                });
                let redirect_addr = std::net::SocketAddr::from(([0, 0, 0, 0], http_port));
                let redirect_listener = tokio::net::TcpListener::bind(&redirect_addr)
                    .await
                    .expect("Failed to bind HTTP redirect port");
                println!(
                    "HTTP redirect listening on {} -> HTTPS port {}",
                    http_port, https_port
                );
                axum::serve(redirect_listener, redirect_app).await.unwrap();
            });
        }

        let handle = Handle::new();
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            shutdown_signal.await.ok();
            handle_clone.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
        });

        axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls_config)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .unwrap();
    } else {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_signal.await.ok();
            })
            .await
            .unwrap();
    }
}

fn html_escape(s: &str) -> String {
    s.replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('"', "&#39;")
}

async fn handler() -> Html<String> {
    counter!("smser_http_requests_total", "endpoint" => "/").increment(1);
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>smser - SMS Gateway</title>
    <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.0/dist/css/bootstrap.min.css" rel="stylesheet">
    <style>
        body {{ background-color: #f8f9fa; }}
        .container {{ max-width: 800px; margin-top: 2rem; }}
        .msg-card {{ margin-bottom: 1rem; }}
        .date {{ font-size: 0.85rem; color: #6c757d; }}
    </style>
</head>
<body>
    <nav class="navbar navbar-dark bg-dark mb-4">
        <div class="container-fluid">
            <span class="navbar-brand mb-0 h1">smser Gateway</span>
            <div class="d-flex align-items-center">
                <span class="badge text-bg-secondary me-2">{}</span>
                <a href="{}" class="btn btn-outline-info btn-sm me-2" target="_blank">GitHub</a>
                <a href="/status" class="btn btn-outline-light btn-sm me-2">Status</a>
                <a href="/metrics" class="btn btn-outline-light btn-sm">Metrics</a>
            </div>
        </div>
    </nav>

    <div class="container">
        <div class="row">
            <div class="col-md-12">
                <div class="card shadow-sm mb-4">
                    <div class="card-header bg-primary text-white">
                        <h5 class="card-title mb-0">Send SMS</h5>
                    </div>
                    <div class="card-body">
                        <form id="sendForm">
                            <div class="mb-3">
                                <label for="to" class="form-label">Destination Number</label>
                                <input type="text" class="form-control" id="to" placeholder="+44..." required>
                            </div>
                            <div class="mb-3">
                                <label for="message" class="form-label">Message</label>
                                <textarea class="form-control" id="message" rows="3" required></textarea>
                            </div>
                            <button type="submit" class="btn btn-primary" id="sendBtn">Send Message</button>
                        </form>
                        <div id="sendAlert" class="mt-3 d-none alert"></div>
                    </div>
                </div>
            </div>
        </div>

        <div class="row">
            <div class="col-md-12">
                <div class="card shadow-sm">
                    <div class="card-header d-flex justify-content-between align-items-center">
                        <h5 class="card-title mb-0">Recent Messages</h5>
                        <button class="btn btn-sm btn-secondary" onclick="fetchMessages()">Refresh</button>
                    </div>
                    <div class="card-body">
                        <div id="messagesList">
                            <div class="text-center p-4">Loading messages...</div>
                        </div>
                    </div>
                </div>
            </div>
        </div>
    </div>

    <script>
        async function fetchMessages() {{
            try {{
                const response = await fetch('/get-sms?count=10');
                const data = await response.json();
                const list = document.getElementById('messagesList');

                if (data.status === 'success' && data.messages) {{
                    if (data.messages.length === 0) {{
                        list.innerHTML = '<div class="text-center p-4">No messages found.</div>';
                        return;
                    }}

                    list.innerHTML = data.messages.map(msg => `
                        <div class="card msg-card border-0 border-bottom">
                            <div class="card-body px-0">
                                <div class="d-flex justify-content-between align-items-start">
                                    <h6 class="mb-1">${{msg.Phone}}</h6>
                                    <span class="date">${{msg.Date}}</span>
                                </div>
                                <p class="card-text mb-0">${{msg.Content}}</p>
                            </div>
                        </div>
                    `).join('');
                }} else {{
                    list.innerHTML = `<div class="alert alert-danger">Error: ${{data.message || 'Failed to load'}}</div>`;
                }}
            }} catch (e) {{
                document.getElementById('messagesList').innerHTML = `<div class="alert alert-danger">Error connecting to server.</div>`;
            }}
        }}

        document.getElementById('sendForm').addEventListener('submit', async (e) => {{
            e.preventDefault();
            const btn = document.getElementById('sendBtn');
            const alert = document.getElementById('sendAlert');
            const to = document.getElementById('to').value;
            const message = document.getElementById('message').value;

            btn.disabled = true;
            alert.className = 'mt-3 d-none alert';

            try {{
                const response = await fetch('/send-sms', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ to, message, client: 'webclient' }})
                }});
                const data = await response.json();

                alert.className = `mt-3 alert alert-${{data.status === 'success' ? 'success' : 'danger'}}`;
                alert.innerText = data.message || (data.status === 'success' ? 'Sent!' : 'Failed');
                alert.classList.remove('d-none');

                if (data.status === 'success') {{
                    document.getElementById('message').value = '';
                }}
            }} catch (e) {{
                alert.className = 'mt-3 alert alert-danger';
                alert.innerText = 'Network error.';
                alert.classList.remove('d-none');
            }} finally {{
                btn.disabled = false;
            }}
        }});

        // Initial load
        fetchMessages();
    </script>
</body>
</html>"#,
        html_escape(&buildinfo::version_full()),
        html_escape(buildinfo::repository())
    );
    Html(html.to_string())
}

fn extract_country_code(phone: &str) -> String {
    if phone.starts_with('+') {
        phone.chars().take(4).collect()
    } else {
        "unknown".to_string()
    }
}

async fn metrics_handler(State(state): State<AppState>) -> String {
    counter!("smser_http_requests_total", "endpoint" => "/metrics").increment(1);
    state.prometheus_handle.render()
}

async fn status_handler(State(state): State<AppState>) -> Html<String> {
    counter!("smser_http_requests_total", "endpoint" => "/status").increment(1);
    let status = state.rate_limiter.get_status();
    let client_status = state.rate_limiter.get_client_status();
    let uptime = state.start_time.elapsed();
    let uptime_str = format_uptime(uptime);

    // Build client limits HTML
    let client_limits_html = if client_status.is_empty() {
        String::from("<p>No per-client limits configured</p>")
    } else {
        let mut html = String::from(
            r#"<table style="width: 100%; border-collapse: collapse;">
            <tr style="border-bottom: 1px solid #ddd;">
                <th style="text-align: left; padding: 0.5rem;">Client</th>
                <th style="text-align: right; padding: 0.5rem;">Hourly</th>
                <th style="text-align: right; padding: 0.5rem;">Daily</th>
            </tr>"#,
        );
        for cs in &client_status {
            html.push_str(&format!(
                r#"<tr style="border-bottom: 1px solid #eee;">
                <td style="padding: 0.5rem;">{}</td>
                <td style="text-align: right; padding: 0.5rem;">{} / {}</td>
                <td style="text-align: right; padding: 0.5rem;">{} / {}</td>
            </tr>"#,
                html_escape(&cs.name),
                cs.hourly_usage,
                cs.hourly_limit,
                cs.daily_usage,
                cs.daily_limit
            ));
        }
        html.push_str("</table>");
        html
    };

    // Build alert recipient HTML (only if alertmanager feature is enabled)
    #[cfg(feature = "alertmanager")]
    let alert_html = match &state.alert_phone_number {
        Some(phone) => format!(
            r#"<div class="stat"><span class="label">Alert Recipient:</span> {}</div>"#,
            html_escape(phone)
        ),
        None => String::from(
            r#"<div class="stat"><span class="label">Alert Recipient:</span> <em>Not configured</em></div>"#,
        ),
    };
    #[cfg(not(feature = "alertmanager"))]
    let alert_html = String::new();

    let tls_status = if state.tls_enabled {
        "Enabled"
    } else {
        "Disabled"
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>SMS Server Status</title>
    <style>
        body {{ font-family: sans-serif; margin: 2rem; background: #f5f5f5; }}
        h1 {{ color: #333; }}
        .card {{ background: white; border: 1px solid #ddd; padding: 1rem; border-radius: 4px; margin-bottom: 1rem; }}
        .stat {{ margin: 0.5rem 0; }}
        .label {{ font-weight: bold; }}
        h2 {{ margin-top: 0; color: #555; font-size: 1.1rem; }}
    </style>
</head>
<body>
    <h1>SMS Server Status</h1>
    <div class="card">
        <h2>Configuration</h2>
        <div class="stat"><span class="label">Version:</span> {version}</div>
        <div class="stat"><span class="label">Modem URL:</span> {modem_url}</div>
        <div class="stat"><span class="label">TLS:</span> {tls_status}</div>
        {alert_html}
    </div>
    <div class="card">
        <h2>Status</h2>
        <div class="stat"><span class="label">Uptime:</span> {uptime}</div>
    </div>
    <div class="card">
        <h2>Global Rate Limits</h2>
        <div class="stat"><span class="label">Hourly Usage:</span> {hourly_usage} / {hourly_limit}</div>
        <div class="stat"><span class="label">Daily Usage:</span> {daily_usage} / {daily_limit}</div>
    </div>
    <div class="card">
        <h2>Per-Client Rate Limits</h2>
        {client_limits_html}
    </div>
</body>
</html>"#,
        version = html_escape(&buildinfo::version_full()),
        modem_url = html_escape(&state.modem_url),
        tls_status = tls_status,
        alert_html = alert_html,
        uptime = uptime_str,
        hourly_usage = status.hourly_usage,
        hourly_limit = status.hourly_limit,
        daily_usage = status.daily_usage,
        daily_limit = status.daily_limit,
        client_limits_html = client_limits_html
    );
    Html(html)
}

fn format_uptime(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

async fn send_sms_handler(
    State(state): State<AppState>,
    Json(payload): Json<SendSmsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    counter!("smser_http_requests_total", "endpoint" => "/send-sms").increment(1);

    if state.log_sensitive {
        info!(
            "Received request to send SMS to {} (client: {:?}): {:?}",
            payload.to, payload.client, payload.message
        );
    } else {
        info!(
            "Received request to send SMS (client: {:?})",
            payload.client
        );
    }

    // Check rate limit
    if let Err(e) = state
        .rate_limiter
        .check_and_increment(payload.client.as_deref())
    {
        error!("Rate limit exceeded: {}", e);
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("Rate limit exceeded: {}", e),
        ));
    }

    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            error!("Error getting session info: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get session info: {}", e),
            ));
        }
    };

    match modem::send_sms(
        &state.modem_url,
        &session_id,
        &token,
        &payload.to,
        &payload.message,
        false,
    )
    .await
    {
        Ok(_) => {
            if state.log_sensitive {
                info!(
                    "SMS sent successfully to {} (client: {})",
                    payload.to,
                    payload.client.as_deref().unwrap_or("none")
                );
            } else {
                info!(
                    "SMS sent successfully (client: {})",
                    payload.client.as_deref().unwrap_or("none")
                );
            }
            counter!("smser_sms_sent_total").increment(1);
            let country_code = extract_country_code(&payload.to);
            counter!("smser_sms_country_total", "country_code" => country_code).increment(1);
            Ok(Json(
                serde_json::json!({"status": "success", "message": "SMS sent successfully!"}),
            ))
        }
        Err(e) => {
            error!("Error sending SMS: {}", e);
            let status = match e {
                ModemError::ModemError {
                    code: _,
                    message: _,
                } => StatusCode::BAD_REQUEST, // Or map specific modem codes
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, format!("Failed to send SMS: {}", e)))
        }
    }
}

#[cfg(feature = "alertmanager")]
async fn alertmanager_handler(
    State(state): State<AppState>,
    Json(payload): Json<AlertManagerWebhook>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    counter!("smser_http_requests_total", "endpoint" => "/alertmanager").increment(1);

    info!(
        "Received alert from Alert Manager: status={}",
        payload.status
    );

    let to = match &state.alert_phone_number {
        Some(phone) => phone,
        None => {
            error!("Alert Manager webhook received but no alert_phone_number configured");
            return Err((
                StatusCode::BAD_REQUEST,
                "Alert phone number not configured".to_string(),
            ));
        }
    };

    let message = alertmanager::format_alert_message(&payload);

    // Check rate limit (use "alertmanager" as client name for per-client limits)
    if let Err(e) = state.rate_limiter.check_and_increment(Some("alertmanager")) {
        error!("Rate limit exceeded for alert SMS: {}", e);
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("Rate limit exceeded: {}", e),
        ));
    }

    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            error!("Error getting session info for alert SMS: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get session info: {}", e),
            ));
        }
    };

    match modem::send_sms(&state.modem_url, &session_id, &token, to, &message, false).await {
        Ok(_) => {
            if state.log_sensitive {
                info!("Alert SMS sent successfully to {}: {:?}", to, message);
            } else {
                info!("Alert SMS sent successfully");
            }
            counter!("smser_sms_sent_total").increment(1);
            let country_code = extract_country_code(to);
            counter!("smser_sms_country_total", "country_code" => country_code).increment(1);
            Ok(Json(
                serde_json::json!({"status": "success", "message": "Alert SMS sent successfully!"}),
            ))
        }
        Err(e) => {
            error!("Error sending alert SMS: {}", e);
            let status = match e {
                ModemError::ModemError {
                    code: _,
                    message: _,
                } => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, format!("Failed to send alert SMS: {}", e)))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GetSmsRequest {
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default)]
    ascending: bool,
    #[serde(default)]
    unread_preferred: bool,
    #[serde(default = "default_box_type")]
    box_type: BoxType,
    #[serde(default = "default_sort_type")]
    sort_by: SortType,
}

fn default_count() -> u32 {
    20
}
fn default_box_type() -> BoxType {
    BoxType::LocalInbox
}
fn default_sort_type() -> SortType {
    SortType::Date
}

async fn get_sms_handler(
    State(state): State<AppState>,
    Query(params): Query<GetSmsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    counter!("smser_http_requests_total", "endpoint" => "/get-sms").increment(1);

    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            error!("Error getting session info: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get session info: {}", e),
            ));
        }
    };

    let sms_params = modem::SmsListParams {
        box_type: params.box_type,
        sort_type: params.sort_by,
        read_count: params.count,
        ascending: params.ascending,
        unread_preferred: params.unread_preferred,
    };

    match modem::get_sms_list(&state.modem_url, &session_id, &token, sms_params).await {
        Ok(response) => {
            gauge!("smser_sms_stored").set(response.count as f64);
            Ok(Json(
                serde_json::json!({"status": "success", "messages": response.messages.message}),
            ))
        }
        Err(e) => {
            error!("Error receiving SMS: {}", e);
            let status = match e {
                ModemError::ModemError {
                    code: _,
                    message: _,
                } => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, format!("Failed to get SMS list: {}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::setup_metrics;
    use axum::http::StatusCode;
    use reqwest::Client;
    use std::time::Duration; // For StatusCode in tests

    #[tokio::test]
    async fn test_start_server_hello_world() {
        // Find an available port for testing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string(); // Dummy URL for the test

        let (tx, rx) = tokio::sync::oneshot::channel(); // New
        // Spawn the server in a background task
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: None,
                tls_cert: None,
                tls_key: None,
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make a request to the server
        let client = Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{}", port))
            .send()
            .await
            .expect("Failed to send request");

        // Assert the response
        assert!(response.status().is_success());
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("smser Gateway"));
        tx.send(()).unwrap(); // New, send shutdown signal
        server_handle.await.unwrap(); // Wait for server to shut down cleanly. // New
    }

    #[tokio::test]
    async fn test_metrics_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            crate::metrics::update_limits_metrics(100, 1000);
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: None,
                tls_cert: None,
                tls_key: None,
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let client = Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{}/metrics", port))
            .send()
            .await
            .expect("Failed to send request");

        assert!(response.status().is_success());
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("smser_hourly_limit 100"));
        assert!(body.contains("smser_daily_limit 1000"));
        assert!(body.contains("smser_http_requests_total"));
        assert!(body.contains("endpoint=\"/metrics\""));
        assert!(body.contains("smser_start_time_seconds"));
        assert!(body.contains("smser_version_info"));
        assert!(body.contains("version="));

        tx.send(()).unwrap();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: None,
                tls_cert: None,
                tls_key: None,
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let client = Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{}/status", port))
            .send()
            .await
            .expect("Failed to send request");

        assert!(response.status().is_success());
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("SMS Server Status"));
        assert!(body.contains("Modem URL:</span> http://localhost:8080"));
        assert!(body.contains("Hourly Usage:</span> 0 / 100"));

        tx.send(()).unwrap();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    #[cfg(feature = "alertmanager")]
    async fn test_alertmanager_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                alert_phone_number: Some("+441234567890".to_string()),
                tls_cert: None,
                tls_key: None,
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let json = r#"{
  "version": "4",
  "groupKey": "{}:{alertname=\"TestAlert\"}",
  "truncatedAlerts": 0,
  "status": "firing",
  "receiver": "webhook",
  "groupLabels": {},
  "commonLabels": {
    "alertname": "TestAlert",
    "severity": "critical"
  },
  "commonAnnotations": {
    "summary": "Something is broken"
  },
  "externalURL": "http://localhost:9093",
  "alerts": []
}"#;

        let client = Client::new();
        let response = client
            .post(format!("http://127.0.0.1:{}/alertmanager", port))
            .header("Content-Type", "application/json")
            .body(json)
            .send()
            .await
            .expect("Failed to send request");

        // It will fail because modem is not there, but it should reach the modem call
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("Failed to get session info"));

        tx.send(()).unwrap();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_get_sms_endpoint_error() {
        // Find an available port for testing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://nonexistent.com".to_string(); // Simulate unavailable modem

        let (tx, rx) = tokio::sync::oneshot::channel(); // New
        // Spawn the server in a background task
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: None,
                tls_cert: None,
                tls_key: None,
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make a request to the server
        let client = Client::new();
        let response = client
            .get(format!(
                "http://127.0.0.1:{}/get-sms?count=1&box_type=1",
                port
            ))
            .send()
            .await
            .expect("Failed to send request");

        // Assert the response
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("Failed to get session info"));
        tx.send(()).unwrap(); // New, send shutdown signal
        server_handle.await.unwrap(); // Wait for server to shut down cleanly. // New
    }

    #[tokio::test]
    async fn test_start_server_tls() {
        // Generate a self-signed certificate
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let cert = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.signing_key.serialize_pem();

        // Write cert and key to temporary files
        let cert_path = std::env::temp_dir().join("smser_test_cert.pem");
        let key_path = std::env::temp_dir().join("smser_test_key.pem");
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&key_path, &key_pem).unwrap();

        // Find an available port for testing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let cert_path_clone = cert_path.clone();
        let key_path_clone = key_path.clone();

        // Spawn the server in a background task
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000, vec![]);
            let config = ServerConfig {
                modem_url,
                prometheus_handle: handle,
                rate_limiter,
                #[cfg(feature = "alertmanager")]
                alert_phone_number: None,
                tls_cert: Some(cert_path_clone),
                tls_key: Some(key_path_clone),
                http_redirect_port: None,
                redirect_host: None,
                log_sensitive: true,
            };
            start_server(listener, rx, config).await;
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Make a request to the server using HTTPS
        // We need to disable certificate verification because it's a self-signed cert
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        let response = client
            .get(format!("https://127.0.0.1:{}", port))
            .send()
            .await
            .expect("Failed to send request");

        // Assert the response
        assert!(response.status().is_success());
        let body = response.text().await.expect("Failed to get response body");
        assert!(body.contains("smser Gateway"));

        // Clean up
        tx.send(()).unwrap();
        server_handle.await.unwrap();
        let _ = std::fs::remove_file(cert_path);
        let _ = std::fs::remove_file(key_path);
    }
}
