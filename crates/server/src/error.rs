use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        AppError { status, message: msg.into() }
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Nicht gefunden")
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Nicht angemeldet")
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!("{}", self.message);
        }
        (self.status, Json(shared::ApiError { error: self.message })).into_response()
    }
}

macro_rules! impl_from {
    ($t:ty) => {
        impl From<$t> for AppError {
            fn from(e: $t) -> Self {
                AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    };
}
impl_from!(sqlx::Error);
impl_from!(std::io::Error);
impl_from!(anyhow::Error);
impl_from!(tantivy::TantivyError);
impl_from!(tokio::task::JoinError);

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        AppError::bad_request(format!("Upload fehlgeschlagen: {e}"))
    }
}
