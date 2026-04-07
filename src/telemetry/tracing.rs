use axum::{extract::MatchedPath, http::Request};
use tracing::{Span, info_span};

pub fn transfer_span(kind: &'static str, code: &str) -> Span {
    info_span!("transfer", transfer_kind = kind, session_code = %code)
}

pub fn make_http_span<B>(request: &Request<B>) -> Span {
    let matched_path = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_else(|| request.uri().path());

    info_span!(
        "http_request",
        method = %request.method(),
        matched_path = matched_path,
        version = ?request.version()
    )
}
