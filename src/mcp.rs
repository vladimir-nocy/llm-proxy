use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext,
};
use serde_json::Value;

use crate::{NAME, VERSION, tools::ToolRegistry};

#[derive(Clone)]
struct McpGateway(Arc<ToolRegistry>);

impl ServerHandler for McpGateway {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        config.server_info = Implementation::new(NAME, VERSION);
        config
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.0.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let result = self
            .0
            .call(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
            )
            .await;
        Ok(match result {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&value).expect("JSON value is serializable"),
            )]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
        .into())
    }
}

pub async fn serve(tools: Arc<ToolRegistry>) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("{NAME} [{} adapter] MCP ready", tools.client.config.name());
    let service = McpGateway(tools).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
