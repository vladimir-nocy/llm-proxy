#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    claude_codex_api::run("claude", false).await
}
