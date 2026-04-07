use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub enum AppError {
    PayloadTooLarge(String),
    BadRequest(String),
    TooManyRequests(String),
    ServiceUnavailable(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::PayloadTooLarge(message) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(ErrorResponse {
                    error: "payload_too_large",
                    message,
                }),
            )
                .into_response(),
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "bad_request",
                    message,
                }),
            )
                .into_response(),
            Self::TooManyRequests(message) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "too_many_requests",
                    message,
                }),
            )
                .into_response(),
            Self::ServiceUnavailable(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "service_unavailable",
                    message,
                }),
            )
                .into_response(),
        }
    }
}
