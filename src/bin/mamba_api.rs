#[tokio::main]
async fn main() -> anyhow::Result<()> {
    mamba::api::run_from_env().await
}
