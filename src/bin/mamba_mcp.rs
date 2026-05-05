#[tokio::main]
async fn main() -> anyhow::Result<()> {
    mamba::mcp::run_from_env().await
}
