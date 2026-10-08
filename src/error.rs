use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, GatewayError>;

#[derive(Debug)]
pub struct GatewayError {
    pub status: StatusCode,
    pub message: String,
    pub current: Option<Value>,
}

impl GatewayError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            current: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub async fn upstream(response: reqwest::Response) -> Self {
        let status = response.status();
        let problem: Value = response.json().await.unwrap_or(Value::Null);
        let title = problem["title"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        let mut message = title;
        if let Some(detail) = problem["detail"].as_str() {
            message.push_str(&format!(" — {detail}"));
        }
        if status == StatusCode::UNAUTHORIZED {
            message.push_str(
                " — The upstream session may have expired — check the gateway's credentials.",
            );
        }
        Self {
            status,
            message,
            current: problem.get("current").cloned(),
        }
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GatewayError {}

impl From<reqwest::Error> for GatewayError {
    fn from(error: reqwest::Error) -> Self {
        // URLs can contain sensitive query parameters; don't include them in errors.
        Self::new(StatusCode::BAD_GATEWAY, error.without_url().to_string())
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": self.message});
        if let Some(current) = self.current {
            body["current"] = current;
        }
        (self.status, Json(body)).into_response()
    }
}
