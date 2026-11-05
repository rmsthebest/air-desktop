//! Bridge between GPUI's executors and the tokio runtime that Air's core
//! client needs. Modeled on Zed's `gpui_tokio` crate.
//!
//! Anything that touches `aircoreclient` (sqlx, tonic, reqwest) must run on
//! tokio. GPUI code awaits the returned [`TokioTask`], which works from any
//! executor. Dropping it aborts the tokio task immediately, independent of
//! when any executor next polls it.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Context as _;
use gpui_kit::{App, Global};
use tokio::runtime::{Handle, Runtime};
use tokio::task::JoinHandle;

// Wired into the session layer in milestone 3.
#[allow(dead_code)]
pub struct Tokio {
    runtime: Option<Runtime>,
}

impl Global for Tokio {}

impl Drop for Tokio {
    fn drop(&mut self) {
        // Dropping a runtime blocks on its tasks; at app exit we don't wait.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Starts the tokio runtime and stores it as a GPUI global.
pub fn init(cx: &mut App) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("air-tokio")
        .enable_all()
        .build()
        .context("failed to start tokio runtime")?;
    cx.set_global(Tokio {
        runtime: Some(runtime),
    });
    Ok(())
}

#[allow(dead_code)]
impl Tokio {
    pub fn handle(cx: &App) -> Handle {
        cx.global::<Self>()
            .runtime
            .as_ref()
            .expect("tokio runtime is alive while the app runs")
            .handle()
            .clone()
    }

    /// Runs `future` on tokio. The task resolves with its output, or an
    /// error if the tokio task panicked or was cancelled.
    pub fn spawn<F, R>(cx: &App, future: F) -> TokioTask<R>
    where
        F: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        TokioTask(Self::handle(cx).spawn(future))
    }

    /// Like [`Tokio::spawn`] for fallible futures, flattening the result.
    pub fn spawn_result<F, R>(
        cx: &App,
        future: F,
    ) -> impl Future<Output = anyhow::Result<R>> + use<F, R>
    where
        F: Future<Output = anyhow::Result<R>> + Send + 'static,
        R: Send + 'static,
    {
        let task = Self::spawn(cx, future);
        async move { task.await? }
    }
}

/// Handle to a future running on tokio; aborts it when dropped.
#[must_use = "dropping a TokioTask cancels it"]
pub struct TokioTask<R>(JoinHandle<R>);

impl<R> Future for TokioTask<R> {
    type Output = anyhow::Result<R>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|result| result.context("tokio task failed"))
    }
}

impl<R> Drop for TokioTask<R> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use gpui::TestAppContext;

    use super::*;

    fn setup(cx: &mut TestAppContext) {
        // Tokio tasks complete on other threads, outside the test scheduler.
        cx.executor().allow_parking();
        cx.update(|cx| init(cx).unwrap());
    }

    #[gpui::test]
    async fn spawned_future_runs_inside_tokio(cx: &mut TestAppContext) {
        setup(cx);
        // `Handle::try_current` only succeeds inside a tokio runtime, which is
        // what sqlx/tonic/reqwest in Air's core require.
        let task = cx.update(|cx| Tokio::spawn(cx, async { Handle::try_current().is_ok() }));
        assert!(task.await.unwrap());
    }

    #[gpui::test]
    async fn spawn_result_flattens_errors(cx: &mut TestAppContext) {
        setup(cx);
        let task = cx
            .update(|cx| Tokio::spawn_result(cx, async { Err::<(), _>(anyhow::anyhow!("boom")) }));
        assert_eq!(task.await.unwrap_err().to_string(), "boom");
    }

    #[gpui::test]
    async fn dropping_task_aborts_tokio_future(cx: &mut TestAppContext) {
        setup(cx);
        let finished = Arc::new(AtomicBool::new(false));
        let task = cx.update(|cx| {
            let finished = finished.clone();
            Tokio::spawn(cx, async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                finished.store(true, Ordering::SeqCst);
            })
        });
        drop(task);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!finished.load(Ordering::SeqCst));
    }
}
