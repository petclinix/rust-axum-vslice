//! Typed error → HTTP mapping (PLAN.md §6). Handlers return
//! `Result<_, AppError>` and use `?`; this is the one place that decides the
//! JSON body and status code for every failure mode.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("email already registered")]
    EmailTaken,

    #[error("invalid email or password")]
    InvalidCredentials,

    #[error("account is deactivated")]
    AccountDeactivated,

    #[error("invalid request: {0}")]
    Validation(String),

    #[error("storage error: {0}")]
    Storage(#[from] std::io::Error),

    #[error("internal error")]
    Internal,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: String,
}

impl AppError {
    fn code(&self) -> &'static str {
        match self {
            AppError::EmailTaken => "EMAIL_TAKEN",
            AppError::InvalidCredentials => "INVALID_CREDENTIALS",
            AppError::AccountDeactivated => "ACCOUNT_DEACTIVATED",
            AppError::Validation(_) => "VALIDATION_ERROR",
            AppError::Storage(_) => "STORAGE_ERROR",
            AppError::Internal => "INTERNAL_ERROR",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            AppError::EmailTaken => StatusCode::CONFLICT,
            AppError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            AppError::AccountDeactivated => StatusCode::FORBIDDEN,
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::Storage(_) | AppError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();

        // Storage/internal failures might embed filesystem paths or other
        // detail that shouldn't reach a client; log the real cause and
        // return a generic message for anything in that bucket.
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self, "internal error");
            "internal server error".to_string()
        } else {
            self.to_string()
        };

        let body = ErrorBody {
            error: message,
            code: self.code().to_string(),
        };

        (status, Json(body)).into_response()
    }
}
