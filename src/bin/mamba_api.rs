fn runtime_worker_threads(name: &str, minimum_default: usize) -> anyhow::Result<usize> {
    let detected = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let Some(raw) = std::env::var(name).ok() else {
        return Ok(detected.max(minimum_default));
    };

    let workers = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("{name} must be an integer from 1 to 64"))?;
    anyhow::ensure!(
        (1..=64).contains(&workers),
        "{name} must be an integer from 1 to 64"
    );
    Ok(workers)
}

fn main() -> anyhow::Result<()> {
    let market_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_worker_threads(
            "MAMBA_API_MARKET_WORKER_THREADS",
            2,
        )?)
        .thread_name("mamba-market")
        .enable_all()
        .build()?;
    let market_handle = market_runtime.handle().clone();
    let api_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_worker_threads("MAMBA_API_WORKER_THREADS", 4)?)
        .thread_name("mamba-api")
        .enable_all()
        .build()?;

    api_runtime.block_on(mamba::api::run_from_env_with_market_runtime(market_handle))
}
