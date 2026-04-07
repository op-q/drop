#![allow(dead_code)]

use std::net::SocketAddr;

use api::{app_state::AppState, build_app, build_state, serve, start_background_services};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{Request, Response},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tower::ServiceExt;

pub fn test_app(ip: SocketAddr) -> Router {
    build_app(build_state()).layer(MockConnectInfo(ip))
}

pub async fn send(app: &Router, request: Request<Body>) -> Response<Body> {
    app.clone()
        .oneshot(request)
        .await
        .expect("expected test request to succeed")
}

pub async fn response_text(response: Response<Body>) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("expected response body bytes");

    String::from_utf8(bytes.to_vec()).expect("expected utf8 response body")
}

pub struct NetworkTestServer {
    pub addr: SocketAddr,
}

impl NetworkTestServer {
    pub fn ws_url(&self, path: &str) -> String {
        format!("ws://{}{}", self.addr, path)
    }

    pub async fn send_raw_http(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(self.addr)
            .await
            .expect("expected test server connection");

        stream
            .write_all(request.as_bytes())
            .await
            .expect("expected request to be written");
        stream
            .shutdown()
            .await
            .expect("expected stream shutdown to succeed");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("expected response to be read");

        String::from_utf8(response).expect("expected utf8 response")
    }
}

pub async fn spawn_network_test_server() -> NetworkTestServer {
    spawn_network_test_server_with_state(build_state()).await
}

pub async fn spawn_network_test_server_with_state(state: AppState) -> NetworkTestServer {
    start_background_services(state.clone());

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("expected test listener to bind");
    let addr = listener
        .local_addr()
        .expect("expected local address for test listener");

    tokio::spawn(async move {
        serve(listener, app)
            .await
            .expect("expected test server to run");
    });

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    NetworkTestServer { addr }
}

pub fn response_status(response: &str) -> &str {
    response
        .lines()
        .next()
        .expect("expected HTTP status line in response")
}

pub fn response_body(response: &str) -> &str {
    response
        .split("\r\n\r\n")
        .nth(1)
        .expect("expected HTTP response body")
}
