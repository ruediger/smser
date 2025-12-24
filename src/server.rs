use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use axum::http::StatusCode; // For HTTP status codes
use serde::{Deserialize, Serialize};
use crate::modem::{self, BoxType, SortType, Error as ModemError}; // Import modem module and alias Error
use tokio::net::TcpListener;

#[derive(Debug, Deserialize, Serialize)]
pub struct SendSmsRequest {
    pub to: String,
    pub message: String,
}

#[derive(Clone)]
struct AppState {
    modem_url: String,
}

use tokio::sync::oneshot; // New import

pub async fn start_server(listener: TcpListener, modem_url: String, shutdown_signal: oneshot::Receiver<()>) {
    let app_state = AppState { modem_url: modem_url.clone() };

    let app = Router::new()
        .route("/", get(handler))
        .route("/send-sms", post(send_sms_handler))
        .route("/get-sms", get(get_sms_handler))
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

async fn send_sms_handler(
    State(state): State<AppState>,
    Json(payload): Json<SendSmsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            eprintln!("Error getting session info: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get session info: {}", e)));
        }
    };

    match modem::send_sms(&state.modem_url, &session_id, &token, &payload.to, &payload.message, false).await {
        Ok(_) => Ok(Json(serde_json::json!({"status": "success", "message": "SMS sent successfully!"}))),
        Err(e) => {
            eprintln!("Error sending SMS: {}", e);
            let status = match e {
                ModemError::ModemError { code: _, message: _ } => StatusCode::BAD_REQUEST, // Or map specific modem codes
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

fn default_count() -> u32 { 20 }
fn default_box_type() -> BoxType { BoxType::LocalInbox }
fn default_sort_type() -> SortType { SortType::Date }

async fn get_sms_handler(
    State(state): State<AppState>,
    Query(params): Query<GetSmsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (session_id, token) = match modem::get_session_info(&state.modem_url).await {
        Ok((s, t)) => (s, t),
        Err(e) => {
            eprintln!("Error getting session info: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get session info: {}", e)));
        }
    };

    match modem::get_sms_list(
        &state.modem_url,
        &session_id,
        &token,
        params.box_type,
        params.sort_by,
        params.count,
        params.ascending,
        params.unread_preferred,
    ).await {
        Ok(response) => Ok(Json(serde_json::json!({"status": "success", "messages": response.messages.message}))),
        Err(e) => {
            eprintln!("Error receiving SMS: {}", e);
            let status = match e {
                ModemError::ModemError { code: _, message: _ } => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, format!("Failed to get SMS list: {}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;
    use std::time::Duration;
    use axum::http::StatusCode; // For StatusCode in tests

    #[tokio::test]
    async fn test_start_server_hello_world() {
        // Find an available port for testing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://localhost:8080".to_string(); // Dummy URL for the test

        let (tx, rx) = tokio::sync::oneshot::channel(); // New
        // Spawn the server in a background task
        let server_handle = tokio::spawn(async move {
            start_server(listener, modem_url, rx).await; // Pass listener
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
    async fn test_get_sms_endpoint_error() {
        // Find an available port for testing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let modem_url = "http://nonexistent.com".to_string(); // Simulate unavailable modem

        let (tx, rx) = tokio::sync::oneshot::channel(); // New
        // Spawn the server in a background task
        let server_handle = tokio::spawn(async move {
            start_server(listener, modem_url, rx).await; // Pass listener
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Make a request to the server
        let client = Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{}/get-sms?count=1&box_type=1", port))
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