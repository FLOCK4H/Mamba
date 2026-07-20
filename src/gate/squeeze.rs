use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, Semaphore};

/// Simple async rate limiter that enforces a maximum number of calls per second.
#[derive(Clone)]
pub struct Squeezer {
    max_rps: Option<u64>,
    calls: Arc<Mutex<VecDeque<Instant>>>,
    max_in_flight: Option<usize>,
    permits: Option<Arc<Semaphore>>,
}

impl Squeezer {
    /// `max_rps=0` disables throttling and allows pass-through execution.
    pub fn new(max_rps: u64) -> Self {
        Self::with_limits(max_rps, 0)
    }

    /// Applies both a request start-rate limit and a concurrent in-flight limit.
    /// A value of `0` disables the corresponding limit.
    pub fn with_limits(max_rps: u64, max_in_flight: usize) -> Self {
        Self {
            max_rps: if max_rps == 0 { None } else { Some(max_rps) },
            calls: Arc::new(Mutex::new(VecDeque::new())),
            max_in_flight: if max_in_flight == 0 {
                None
            } else {
                Some(max_in_flight)
            },
            permits: if max_in_flight == 0 {
                None
            } else {
                Some(Arc::new(Semaphore::new(max_in_flight)))
            },
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.max_rps.is_some()
    }

    pub fn configured_max_rps(&self) -> Option<u64> {
        self.max_rps
    }

    pub fn configured_max_in_flight(&self) -> Option<usize> {
        self.max_in_flight
    }

    async fn acquire_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let permits = self.permits.as_ref()?.clone();
        permits.acquire_owned().await.ok()
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
        let _permit = self.acquire_permit().await;
        self.wait().await;
        f().await
    }

    /// Like `run`, but preserves Result types.
    pub async fn run_result<F, Fut, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let _permit = self.acquire_permit().await;
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
        assert_eq!(squeezer.configured_max_in_flight(), None);
    }

    #[tokio::test]
    async fn test_run_result_limits_concurrent_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let squeezer = Squeezer::with_limits(0, 2);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..8 {
            let limiter = squeezer.clone();
            let active = active.clone();
            let peak = peak.clone();
            tasks.push(tokio::spawn(async move {
                limiter
                    .run_result(|| async {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, ()>(())
                    })
                    .await
            }));
        }

        for task in tasks {
            assert!(task.await.expect("limiter task should finish").is_ok());
        }

        assert_eq!(squeezer.configured_max_in_flight(), Some(2));
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }
}
