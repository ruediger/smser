use crate::modem::{self, BoxType, Error as ModemError, SortType}; // Import modem module and alias Error
use axum::http::StatusCode; // For HTTP status codes
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use crate::metrics::RateLimiter;
use metrics::counter;
use metrics_exporter_prometheus::PrometheusHandle;
use axum::response::Html;

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
}

use tokio::sync::oneshot; // New import

pub async fn start_server(
    listener: TcpListener,
    modem_url: String,
    shutdown_signal: oneshot::Receiver<()>,
    handle: PrometheusHandle,
    rate_limiter: RateLimiter,
) {
    let app_state = AppState {
        modem_url: modem_url.clone(),
        rate_limiter,
        prometheus_handle: handle,
    };

    let app = Router::new()
        .route("/", get(handler))
        .route("/send-sms", post(send_sms_handler))
        .route("/get-sms", get(get_sms_handler))
        .route("/metrics", get(metrics_handler))
        .route("/status", get(status_handler))
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
    "Hello, Axum!".to_string()
}

async fn metrics_handler(State(state): State<AppState>) -> String {
    state.prometheus_handle.render()
}

async fn status_handler(State(state): State<AppState>) -> Html<String> {
    let status = state.rate_limiter.get_status();
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
    </div>
    <div class="card">
        <h2>Rate Limits</h2>
        <div class="stat"><span class="label">Hourly Usage:</span> {} / {}</div>
        <div class="stat"><span class="label">Daily Usage:</span> {} / {}</div>
    </div>
</body>
</html>"#,
        state.modem_url,
        status.hourly_usage, status.hourly_limit,
        status.daily_usage, status.daily_limit
    );
    Html(html)
}

async fn send_sms_handler(
    State(state): State<AppState>,
    Json(payload): Json<SendSmsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Check rate limit
    if let Err(e) = state.rate_limiter.check_and_increment() {
        eprintln!("Rate limit exceeded: {}", e);
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("Rate limit exceeded: {}", e),
        ));
    }

    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            eprintln!("Error getting session info: {}", e);
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
            counter!("smser_sms_sent_total").increment(1);
            Ok(Json(
                serde_json::json!({"status": "success", "message": "SMS sent successfully!"}),
            ))
        }
        Err(e) => {
            eprintln!("Error sending SMS: {}", e);
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
    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            eprintln!("Error getting session info: {}", e);
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

    match modem::get_sms_list(
        &state.modem_url,
        &session_id,
        &token,
        sms_params,
    )
    .await
    {
        Ok(response) => Ok(Json(
            serde_json::json!({"status": "success", "messages": response.messages.message}),
        )),
        Err(e) => {
            eprintln!("Error receiving SMS: {}", e);
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
            start_server(listener, modem_url, rx, handle, rate_limiter).await; // Pass listener
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
            start_server(listener, modem_url, rx, handle, rate_limiter).await;
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
            start_server(listener, modem_url, rx, handle, rate_limiter).await;
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
            start_server(listener, modem_url, rx, handle, rate_limiter).await; // Pass listener
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
