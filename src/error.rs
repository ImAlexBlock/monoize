use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub error_type: String,
    pub param: Option<String>,
    pub upstream_status: Option<u16>,
    pub upstream_code: Option<String>,
    pub upstream_type: Option<String>,
    pub upstream_param: Option<String>,
    /// When set, request logs use this instead of `message` so the client
    /// receives sanitized text while internal logs retain full detail.
    pub internal_message: Option<String>,
    pub downstream_stream_terminal_sent: bool,
}

impl AppError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            error_type: "invalid_request_error".to_string(),
            param: None,
            upstream_status: None,
            upstream_code: None,
            upstream_type: None,
            upstream_param: None,
            internal_message: None,
            downstream_stream_terminal_sent: false,
        }
    }

    pub fn with_internal_message(mut self, msg: impl Into<String>) -> Self {
        self.internal_message = Some(msg.into());
        self
    }
    pub fn with_downstream_stream_terminal_sent(mut self, sent: bool) -> Self {
        self.downstream_stream_terminal_sent = sent;
        self
    }

    pub fn with_type(mut self, error_type: impl Into<String>) -> Self {
        self.error_type = error_type.into();
        self
    }

    pub fn with_param(mut self, param: impl Into<String>) -> Self {
        self.param = Some(param.into());
        self
    }

    pub fn with_upstream_error(
        mut self,
        status: Option<StatusCode>,
        code: Option<String>,
        error_type: Option<String>,
        param: Option<String>,
    ) -> Self {
        self.upstream_status = status.map(|status| status.as_u16());
        self.upstream_code = code;
        self.upstream_type = error_type;
        self.upstream_param = param;
        self
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    param: Option<String>,
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_param: Option<String>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // SAN-12a: an upstream chooses these values, so a non-enumerated one is dropped
        // rather than published. `upstream_status` is a bounded integer and is kept (SAN-12).
        let enum_shaped =
            |value: Option<String>| value.filter(|v| crate::error_sanitize::is_enum_shaped(v));
        // SAN-12a: the top-level `code` and `type` are normally Monoize-authored, but the
        // exhausted-routing path and `with_type` both copy an upstream value into them. The
        // same shape gate applies at the boundary, so no construction path can bypass it.
        let code = if crate::error_sanitize::is_enum_shaped(&self.code) {
            self.code
        } else {
            "upstream_error".to_string()
        };
        let error_type = if crate::error_sanitize::is_enum_shaped(&self.error_type) {
            self.error_type
        } else {
            "invalid_request_error".to_string()
        };
        let body = ErrorEnvelope {
            error: ErrorBody {
                message: self.message,
                error_type,
                param: self.param,
                code,
                upstream_status: self.upstream_status,
                upstream_code: enum_shaped(self.upstream_code),
                upstream_type: enum_shaped(self.upstream_type),
                upstream_param: enum_shaped(self.upstream_param),
            },
        };
        (self.status, axum::Json(body)).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
