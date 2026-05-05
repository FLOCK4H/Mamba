use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::Mutex;

/// Simple async rate limiter that enforces a maximum number of calls per second.
#[derive(Clone)]
pub struct Squeezer {
    max_rps: Option<u64>,
    calls: Arc<Mutex<VecDeque<Instant>>>,
}

impl Squeezer {
    /// `max_rps=0` disables throttling and allows pass-through execution.
    pub fn new(max_rps: u64) -> Self {
        Self {
            max_rps: if max_rps == 0 { None } else { Some(max_rps) },
            calls: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.max_rps.is_some()
    }

    pub fn configured_max_rps(&self) -> Option<u64> {
        self.max_rps
    }

    /// Waits until the call can proceed under the configured RPS and returns the
    /// current in-window request count. Returns `0` when throttling is disabled.
    pub async fn wait(&self) -> u64 {
        let Some(max_rps) = self.max_rps else {
            return 0;
        };

        let window = Duration::from_secs(1);
        loop {
            let mut calls = self.calls.lock().await;
            let now = Instant::now();
            while let Some(front) = calls.front().copied() {
                if now.duration_since(front) >= window {
                    calls.pop_front();
                } else {
                    break;
                }
            }

            if calls.len() as u64 >= max_rps
                && let Some(front) = calls.front().copied()
            {
                let sleep_dur = window.saturating_sub(now.duration_since(front));
                drop(calls);
                tokio::time::sleep(sleep_dur).await;
                continue;
            }

            calls.push_back(Instant::now());
            return calls.len() as u64;
        }
    }

    /// Runs the provided async closure once a slot is available.
    pub async fn run<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        self.wait().await;
        f().await
    }

    /// Like `run`, but preserves Result types.
    pub async fn run_result<F, Fut, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        self.wait().await;
        f().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_wait_is_pass_through_when_disabled() {
        let squeezer = Squeezer::new(0);
        assert!(!squeezer.is_enabled());
        assert_eq!(squeezer.configured_max_rps(), None);

        let started = Instant::now();
        let in_window = squeezer.wait().await;
        assert_eq!(in_window, 0);
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn test_wait_is_enabled_when_max_rps_positive() {
        let squeezer = Squeezer::new(9);
        assert!(squeezer.is_enabled());
        assert_eq!(squeezer.configured_max_rps(), Some(9));
    }
}
