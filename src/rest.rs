use crate::{
    error::Result,
    tools::{ApiRequest, ToolDef, string},
};
use serde_json::json;

pub fn tools() -> Result<Vec<ToolDef>> {
    let path = json!({"type": "string", "pattern": "^/", "description": "Endpoint path starting with /, e.g. /repos/facebook/react"});
    let query = json!({"type": "object", "additionalProperties": {"type": "string"}, "description": "Query parameters, e.g. {\"page\":\"2\"}"});
    Ok(vec![
        ToolDef::new(
            "rest_request",
            "Call any endpoint of the configured API: method, path, query and JSON body. Use this for PATCH, PUT, DELETE or other calls.",
            json!({"method": {"type": "string", "enum": ["GET", "POST", "PATCH", "PUT", "DELETE"]}, "path": path, "query": query, "body": {"description": "JSON request body"}}),
            &["method", "path"],
            |a| {
                Ok(ApiRequest {
                    method: string(a, "method"),
                    path: string(a, "path"),
                    query: a.get("query").and_then(|v| v.as_object()).cloned(),
                    body: a.get("body").cloned(),
                })
            },
        )?,
        ToolDef::new(
            "rest_get",
            "GET any endpoint of the configured API.",
            json!({"path": path, "query": query}),
            &["path"],
            |a| {
                Ok(ApiRequest {
                    method: "GET".into(),
                    path: string(a, "path"),
                    query: a.get("query").and_then(|v| v.as_object()).cloned(),
                    body: None,
                })
            },
        )?,
        ToolDef::new(
            "rest_post",
            "POST a JSON body to any endpoint of the configured API.",
            json!({"path": path, "query": query, "body": {}}),
            &["path"],
            |a| {
                Ok(ApiRequest {
                    method: "POST".into(),
                    path: string(a, "path"),
                    query: a.get("query").and_then(|v| v.as_object()).cloned(),
                    body: a.get("body").cloned(),
                })
            },
        )?,
    ])
}
