use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use common::types::{error_codes::*, ApiError, ApiErrorBody};

#[derive(Debug)]
pub struct Error(pub &'static str, pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: &'static str) -> Self {
        Self(code, code.replace('_', " "))
    }
    pub fn malformed(message: impl ToString) -> Self {
        Self(MALFORMED_REQUEST, message.to_string())
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self.0 {
            MALFORMED_REQUEST => StatusCode::BAD_REQUEST,
            INVALID_SIGNATURE | UNKNOWN_TRADER | STALE_NONCE => StatusCode::UNAUTHORIZED,
            NOT_YOUR_ORDER | NOT_YOUR_RFQ => StatusCode::FORBIDDEN,
            RFQ_NOT_FOUND | ORDER_NOT_FOUND => StatusCode::NOT_FOUND,
            RFQ_NOT_OPEN | NOT_AWAITING_ACCEPT => StatusCode::CONFLICT,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        (
            status,
            Json(ApiError {
                error: ApiErrorBody {
                    code: self.0.into(),
                    message: self.1,
                },
            }),
        )
            .into_response()
    }
}
