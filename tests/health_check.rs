mod common;

use std::net::SocketAddr;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::{response_text, send, test_app};
use serde_json::Value;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 3000)));

    let response = send(
        &app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("expected request builder"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_text(response).await, "ok");
}

#[tokio::test]
async fn readiness_endpoint_tracks_draining_state() {
    let state = api::build_state();
    let app = api::build_app(state.clone()).layer(axum::extract::connect_info::MockConnectInfo(
        SocketAddr::from(([127, 0, 0, 1], 3003)),
    ));

    let ready = send(
        &app,
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .expect("expected request builder"),
    )
    .await;
    assert_eq!(ready.status(), StatusCode::OK);

    state.begin_draining();

    let draining = send(
        &app,
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .expect("expected request builder"),
    )
    .await;
    assert_eq!(draining.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// The relay serves no web page. The root says so in words rather than as a
/// bare 404, because the operator is the one person who will open it.
#[tokio::test]
async fn the_root_explains_what_this_host_is() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 3001)));

    let response = send(
        &app,
        Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("expected request builder"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_text(response).await;
    assert!(body.contains("Drop relay"), "unexpected body: {body}");
    assert!(
        body.contains("github.com/op-q/drop"),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn metrics_endpoint_returns_snapshot() {
    let app = test_app(SocketAddr::from(([127, 0, 0, 1], 3002)));

    let response = send(
        &app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("expected request builder"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body: Value =
        serde_json::from_str(&response_text(response).await).expect("expected JSON response body");

    assert_eq!(body["active_sessions"], 0);
    assert_eq!(body["total_bytes_relayed"], 0);
    assert_eq!(body["total_transfer_failures"], 0);
}
