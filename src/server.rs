use crate::alertmanager::{self, AlertManagerWebhook};
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
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

#[derive(Debug, Deserialize, Serialize)]
pub struct SendSmsRequest {
    pub to: String,
    pub message: String,
}

#[derive(Clone)]
struct AppState {
    modem_url: String,
    rate_limiter: RateLimiter,
    prometheus_handle: PrometheusHandle,
    alert_phone_number: Option<String>,
    start_time: Instant,
}

use tokio::sync::oneshot; // New import

pub async fn start_server(
    listener: TcpListener,
    modem_url: String,
    shutdown_signal: oneshot::Receiver<()>,
    handle: PrometheusHandle,
    rate_limiter: RateLimiter,
    alert_phone_number: Option<String>,
) {
    let start_time = Instant::now();
    let start_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    // Set the start time metric
    gauge!("smser_start_time_seconds").set(start_timestamp);

    let app_state = AppState {
        modem_url: modem_url.clone(),
        rate_limiter,
        prometheus_handle: handle,
        alert_phone_number,
        start_time,
    };

    let app = Router::new()
        .route("/", get(handler))
        .route("/send-sms", post(send_sms_handler))
        .route("/get-sms", get(get_sms_handler))
        .route("/metrics", get(metrics_handler))
        .route("/status", get(status_handler))
        .route("/alertmanager", post(alertmanager_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(app_state); // Pass state to the router

    // run it
    let addr = listener.local_addr().unwrap();
    println!("listening on {}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal.await.ok();
        })
        .await
        .unwrap();
}

async fn handler() -> String {
    counter!("smser_http_requests_total", "endpoint" => "/").increment(1);
    "Hello, Axum!".to_string()
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
    let uptime = state.start_time.elapsed();
    let uptime_str = format_uptime(uptime);

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>SMS Server Status</title>
    <style>
        body {{ font-family: sans-serif; margin: 2rem; }}
        h1 {{ color: #333; }}
        .card {{ border: 1px solid #ddd; padding: 1rem; border-radius: 4px; margin-bottom: 1rem; }}
        .stat {{ margin: 0.5rem 0; }}
        .label {{ font-weight: bold; }}
    </style>
</head>
<body>
    <h1>SMS Server Status</h1>
    <div class="card">
        <h2>Configuration</h2>
        <div class="stat"><span class="label">Modem URL:</span> {}</div>
        <div class="stat"><span class="label">Uptime:</span> {}</div>
    </div>
    <div class="card">
        <h2>Rate Limits</h2>
        <div class="stat"><span class="label">Hourly Usage:</span> {} / {}</div>
        <div class="stat"><span class="label">Daily Usage:</span> {} / {}</div>
    </div>
</body>
</html>"#,
        state.modem_url,
        uptime_str,
        status.hourly_usage,
        status.hourly_limit,
        status.daily_usage,
        status.daily_limit
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

    info!("Received request to send SMS to {}", payload.to);

    // Check rate limit
    if let Err(e) = state.rate_limiter.check_and_increment() {
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
            info!("SMS sent successfully to {}", payload.to);
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

    // Check rate limit
    if let Err(e) = state.rate_limiter.check_and_increment() {
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
            info!("Alert SMS sent successfully to {}", to);
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
        Ok(response) => Ok(Json(
            serde_json::json!({"status": "success", "messages": response.messages.message}),
        )),
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
            let rate_limiter = RateLimiter::new(100, 1000);
            start_server(listener, modem_url, rx, handle, rate_limiter, None).await; // Pass listener // Pass listener
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
        assert_eq!(body, "Hello, Axum!");
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
            let rate_limiter = RateLimiter::new(100, 1000);
            start_server(listener, modem_url, rx, handle, rate_limiter, None).await; // Pass listener
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
            let rate_limiter = RateLimiter::new(100, 1000);
            start_server(listener, modem_url, rx, handle, rate_limiter, None).await; // Pass listener
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
    async fn test_alertmanager_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let handle = setup_metrics();
            let rate_limiter = RateLimiter::new(100, 1000);
            start_server(
                listener,
                modem_url,
                rx,
                handle,
                rate_limiter,
                Some("+441234567890".to_string()),
            )
            .await;
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
            let rate_limiter = RateLimiter::new(100, 1000);
            start_server(listener, modem_url, rx, handle, rate_limiter, None).await; // Pass listener // Pass listener
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
}
