mod common;

use std::net::SocketAddr;

use api::config::MAX_UPLOAD_SIZE_BYTES;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{response_text, send, test_app};
use serde_json::{Value, json};

fn create_session_request(ciphertext_size: u64) -> Request<Body> {
    let body = json!({
        "ciphertext_size": ciphertext_size,
    })
    .to_string();

    Request::builder()
        .method(Method::POST)
        .uri("/api/session/create")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("expected request builder")
}

#[tokio::test]
async fn create_session_returns_a_one_time_code() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 4000)));

    let response = send(&app, create_session_request(1024)).await;

    assert_eq!(response.status(), StatusCode::OK);

    let body: Value =
        serde_json::from_str(&response_text(response).await).expect("expected JSON response body");
    let code = body["code"].as_str().expect("expected session code");

    assert_eq!(code.len(), 6);
    assert!(
        code.chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    );
}

#[tokio::test]
async fn create_session_rejects_payloads_over_limit() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 4001)));

    let response = send(&app, create_session_request(MAX_UPLOAD_SIZE_BYTES + 1)).await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let body: Value =
        serde_json::from_str(&response_text(response).await).expect("expected JSON response body");
    assert_eq!(body["error"], "payload_too_large");
}

#[tokio::test]
async fn create_session_rate_limits_after_ten_requests_per_minute() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 4002)));

    for _ in 0..10 {
        let response = send(&app, create_session_request(1024)).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = send(&app, create_session_request(1024)).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let body: Value =
        serde_json::from_str(&response_text(response).await).expect("expected JSON response body");
    assert_eq!(body["error"], "too_many_requests");
}
