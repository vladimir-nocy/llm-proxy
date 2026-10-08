#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    llm_proxy::run("rest", true).await
}
