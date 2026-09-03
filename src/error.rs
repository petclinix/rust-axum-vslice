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

    #[error("authentication required")]
    Unauthenticated,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("requested slot is not available")]
    SlotUnavailable,

    #[error("invalid status transition: {0}")]
    InvalidTransition(String),

    #[error("cancellation cutoff has passed")]
    CancellationCutoffPassed,

    #[error("conflict: {0}")]
    Conflict(String),

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
            AppError::Unauthenticated => "UNAUTHENTICATED",
            AppError::Forbidden(_) => "FORBIDDEN",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::SlotUnavailable => "SLOT_UNAVAILABLE",
            AppError::InvalidTransition(_) => "INVALID_TRANSITION",
            AppError::CancellationCutoffPassed => "CANCELLATION_CUTOFF_PASSED",
            AppError::Conflict(_) => "CONFLICT",
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
            AppError::Unauthenticated => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::SlotUnavailable => StatusCode::CONFLICT,
            AppError::InvalidTransition(_) => StatusCode::CONFLICT,
            AppError::CancellationCutoffPassed => StatusCode::CONFLICT,
            AppError::Conflict(_) => StatusCode::CONFLICT,
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
