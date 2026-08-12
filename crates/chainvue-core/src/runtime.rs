//! The one place blocking work is allowed to happen.
//!
//! `clippy.toml` bans `tokio::task::spawn_blocking` workspace-wide precisely so
//! that it happens here and nowhere else — an unbounded blocking pool is how a
//! wallet with a slow node ends up with hundreds of parked threads.

// The single allowed exception, module-scoped so it is reviewable in one place.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use tokio::sync::Semaphore;

/// Runs blocking SDK calls off the async runtime, with a ceiling.
#[derive(Clone)]
pub struct Blocking {
    handle: tokio::runtime::Handle,
    permits: Arc<Semaphore>,
}

impl Blocking {
    /// `limit` is the number of blocking calls that may be in flight at once.
    ///
    /// Eight: enough that a screen refreshing several addresses does not
    /// serialise, small enough that a hung node cannot consume Tokio's default
    /// 512-thread blocking pool.
    pub fn new(handle: tokio::runtime::Handle, limit: usize) -> Self {
        Self {
            handle,
            permits: Arc::new(Semaphore::new(limit)),
        }
    }

    /// Run `f` on the blocking pool and hand back its value.
    ///
    /// `None` means the call could not be completed — the semaphore closed, or
    /// the task panicked. A panic inside an SDK call is a bug, and is logged
    /// rather than propagated: taking the whole wallet down because one balance
    /// refresh panicked is the wrong trade.
    pub async fn run<T, F>(&self, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self.permits.clone().acquire_owned().await.ok()?;

        match self
            .handle
            .spawn_blocking(move || {
                let _permit = permit;
                f()
            })
            .await
        {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::error!(%error, "a blocking task failed");
                None
            }
        }
    }

    /// Run `f` off the actor entirely, and post its value to `sink`.
    ///
    /// # Why this exists next to [`Blocking::run`]
    ///
    /// `run` is awaited, so the actor stops until it finishes. That is right for
    /// something the next command depends on, and wrong for a balance refresh:
    /// `spendable` costs three requests plus one per young output, and a wallet
    /// that will not respond to **Lock** for eight seconds because it is
    /// counting coins has its priorities backwards.
    ///
    /// So this returns immediately and the answer arrives later as a message the
    /// actor selects on, alongside commands — which keeps the single-writer
    /// property intact: the work happens elsewhere, the *mutation* still happens
    /// in one place, in a defined order.
    ///
    /// A dropped receiver means the actor has stopped, which is not an error.
    pub fn dispatch<T, F>(&self, f: F, sink: tokio::sync::mpsc::UnboundedSender<T>)
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permits = self.permits.clone();
        let handle = self.handle.clone();

        self.handle.spawn(async move {
            let Ok(permit) = permits.acquire_owned().await else {
                return;
            };
            match handle
                .spawn_blocking(move || {
                    let _permit = permit;
                    f()
                })
                .await
            {
                Ok(value) => {
                    let _ = sink.send(value);
                }
                Err(error) => tracing::error!(%error, "a dispatched task failed"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn work_runs_and_returns() {
        let blocking = Blocking::new(tokio::runtime::Handle::current(), 2);
        assert_eq!(blocking.run(|| 2 + 2).await, Some(4));
    }

    /// A panic in one call must not take the process with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_task_is_reported_not_propagated() {
        let blocking = Blocking::new(tokio::runtime::Handle::current(), 2);
        let result: Option<()> = blocking.run(|| panic!("boom")).await;
        assert_eq!(result, None);
    }

    /// The ceiling has to actually bind, or it is decoration.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_pool_is_bounded() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let blocking = Blocking::new(tokio::runtime::Handle::current(), 2);
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let blocking = blocking.clone();
            let live = live.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                blocking
                    .run(move || {
                        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        live.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }

        assert!(
            peak.load(Ordering::SeqCst) <= 2,
            "at most 2 blocking calls should run at once, saw {}",
            peak.load(Ordering::SeqCst),
        );
    }
}
