use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::{
    client::ApiClient,
    config::AdapterConfig,
    error::{GatewayError, Result},
};
pub type Args = Map<String, Value>;

/// An adapter maps validated tool arguments into one upstream request.
pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub query: Option<Args>,
    pub body: Option<Value>,
}

impl ApiRequest {
    pub fn get(path: impl Into<String>, query: Args) -> Result<Self> {
        Ok(Self {
            method: "GET".into(),
            path: path.into(),
            query: Some(query),
            body: None,
        })
    }

    pub fn post(path: impl Into<String>, body: Value) -> Result<Self> {
        Ok(Self {
            method: "POST".into(),
            path: path.into(),
            query: None,
            body: Some(body),
        })
    }
}

pub struct ToolDef {
    pub tool: Tool,
    validator: jsonschema::Validator,
    build: fn(&Args) -> Result<ApiRequest>,
}

impl ToolDef {
    pub fn new(
        name: &'static str,
        description: &'static str,
        properties: Value,
        required: &[&str],
        build: fn(&Args) -> Result<ApiRequest>,
    ) -> Result<Self> {
        let schema = json!({"type": "object", "properties": properties, "required": required});
        let validator = jsonschema::validator_for(&schema).map_err(|e| {
            GatewayError::bad_request(format!("Invalid tool schema for {name}: {e}"))
        })?;
        let tool = Tool::new(
            name,
            description,
            Arc::new(schema.as_object().unwrap().clone()),
        );
        Ok(Self {
            tool,
            validator,
            build,
        })
    }
}

pub struct ToolRegistry {
    pub client: Arc<ApiClient>,
    definitions: Vec<ToolDef>,
}

impl ToolRegistry {
    pub fn new(client: Arc<ApiClient>) -> Result<Self> {
        let definitions = match &client.config {
            AdapterConfig::Rest(_) => crate::rest::tools()?,
            AdapterConfig::Claude(_) | AdapterConfig::Codex(_) => {
                return Err(GatewayError::bad_request(
                    "The subscription adapters are proxies, not tool registries",
                ));
            }
        };
        Ok(Self {
            client,
            definitions,
        })
    }

    pub fn tools(&self) -> Vec<Tool> {
        self.definitions.iter().map(|d| d.tool.clone()).collect()
    }

    pub async fn call(&self, name: &str, args: Value) -> Result<Value> {
        let definition = self
            .definitions
            .iter()
            .find(|d| d.tool.name == name)
            .ok_or_else(|| {
                GatewayError::bad_request(format!("Unknown tool {name:?}. See GET /tools."))
            })?;
        if let Err(error) = definition.validator.validate(&args) {
            return Err(GatewayError::bad_request(format!(
                "Invalid arguments for {name}: {error}"
            )));
        }
        let request = (definition.build)(args.as_object().expect("validated object"))?;
        self.client
            .request(
                &request.method,
                &request.path,
                request.query.as_ref(),
                request.body.as_ref(),
            )
            .await
    }
}

pub fn string(args: &Args, key: &str) -> String {
    args[key]
        .as_str()
        .expect("validated string argument")
        .to_owned()
}

pub fn segment(args: &Args, key: &str) -> String {
    percent_encoding::utf8_percent_encode(&string(args, key), percent_encoding::NON_ALPHANUMERIC)
        .to_string()
}

pub fn select(args: &Args, keys: &[&str]) -> Args {
    keys.iter()
        .filter_map(|key| args.get(*key).map(|v| ((*key).to_owned(), v.clone())))
        .collect()
}
