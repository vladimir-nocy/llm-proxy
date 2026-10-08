use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::{
    Method, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Map, Value};

use crate::{
    config::{AdapterConfig, RestConfig},
    error::{GatewayError, Result},
};

/// HTTP client for the REST adapter: static auth headers, strict URL checks.
pub struct ApiClient {
    pub config: AdapterConfig,
    http: reqwest::Client,
    base: Url,
    headers: HeaderMap,
}

impl ApiClient {
    pub fn new(config: AdapterConfig) -> Result<Self> {
        let AdapterConfig::Rest(rest) = &config else {
            return Err(GatewayError::bad_request(
                "ApiClient serves the REST adapter only",
            ));
        };
        let (base, headers) = build(rest)?;
        Ok(Self {
            config,
            http: reqwest::Client::builder()
                // Do not forward configured secrets to a redirect destination.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()?,
            base,
            headers,
        })
    }

    fn url(&self, path: &str, query: Option<&Map<String, Value>>) -> Result<Url> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('#')
            || path.contains('\\')
            || path.chars().any(char::is_control)
        {
            return Err(GatewayError::bad_request(
                "path must start with / and stay within the configured API",
            ));
        }
        let raw_path = path.split('?').next().unwrap_or(path);
        let decoded = percent_decode_str(raw_path).decode_utf8_lossy();
        if decoded.contains('\\')
            || decoded.chars().any(char::is_control)
            || decoded.split('/').any(|s| s == "." || s == "..")
        {
            return Err(GatewayError::bad_request("path traversal is not allowed"));
        }
        let mut url = Url::parse(&format!(
            "{}{}",
            self.base.as_str().trim_end_matches('/'),
            path
        ))
        .map_err(|_| GatewayError::bad_request("Invalid API path"))?;
        if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
            return Err(GatewayError::bad_request(
                "path must stay within the configured API",
            ));
        }
        if let Some(query) = query {
            // Match URLSearchParams.set: explicit query arguments replace embedded values.
            let mut pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            for (key, value) in query {
                if value.is_null() || value.as_str() == Some("") {
                    continue;
                }
                pairs.retain(|(k, _)| k != key);
                pairs.push((
                    key.clone(),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                ));
            }
            url.set_query(None);
            if !pairs.is_empty() {
                url.query_pairs_mut().extend_pairs(pairs);
            }
        }
        Ok(url)
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        query: Option<&Map<String, Value>>,
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = self.url(path, query)?;
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| GatewayError::bad_request("Invalid HTTP method"))?;
        let response = self.send(method, url, body).await?;
        if !response.status().is_success() {
            return Err(GatewayError::upstream(response).await);
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let bytes = response.bytes().await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            GatewayError::new(
                reqwest::StatusCode::BAD_GATEWAY,
                "Upstream returned an invalid JSON response",
            )
        })
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let mut headers = self.headers.clone();
        if body.is_some() {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        let mut request = self.http.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(request.send().await?)
    }
}

fn build(config: &RestConfig) -> Result<(Url, HeaderMap)> {
    let base_url = config.base_url.trim_end_matches('/');
    let base_path = config.base_path.trim_end_matches('/');
    if !base_path.is_empty() && !base_path.starts_with('/') {
        return Err(GatewayError::bad_request("API base path must start with /"));
    }
    let base = Url::parse(&format!("{base_url}{base_path}/"))
        .map_err(|_| GatewayError::bad_request("API base URL is invalid"))?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || base.query().is_some()
        || base.fragment().is_some()
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(GatewayError::bad_request(
            "API base URL must be HTTP(S), without credentials, query, or fragment",
        ));
    }
    let mut headers = HeaderMap::new();
    for (key, value) in &config.headers {
        insert_header(&mut headers, key, value)?;
    }
    if let Some(token) = &config.token {
        insert_header(&mut headers, "authorization", &format!("Bearer {token}"))?;
    }
    if let Some(key) = &config.api_key {
        insert_header(&mut headers, &config.api_key_header, key)?;
    }
    Ok((base, headers))
}

fn insert_header(headers: &mut HeaderMap, key: &str, value: &str) -> Result<()> {
    let name = HeaderName::from_bytes(key.as_bytes())
        .map_err(|_| GatewayError::bad_request("Invalid configured header name"))?;
    let value = HeaderValue::from_str(value)
        .map_err(|_| GatewayError::bad_request("Invalid configured header value"))?;
    headers.insert(name, value);
    Ok(())
}
