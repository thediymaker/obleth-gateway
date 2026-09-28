//! Transfer completion ownership before awaiting bookkeeping so cancellation
//! cannot lose it or run it twice. The callback also covers an unpolled body.

pub(crate) struct CompletionGuard {
    on_cancel: Option<Box<dyn FnOnce() + Send>>,
}

impl CompletionGuard {
    pub(crate) fn new(on_cancel: impl FnOnce() + Send + 'static) -> Self {
        Self {
            on_cancel: Some(Box::new(on_cancel)),
        }
    }

    /// Replace the cancellation callback without running the old one, so the
    /// fallback can track what is known as the request progresses (before
    /// upstream headers nothing was generated; after them, the estimate
    /// stands in for undelivered usage). Still runs at most once.
    pub(crate) fn rearm(&mut self, on_cancel: impl FnOnce() + Send + 'static) {
        self.on_cancel = Some(Box::new(on_cancel));
    }

    pub(crate) fn complete(
        mut self,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let task = tokio::spawn(future);
        self.on_cancel = None;
        task
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if let Some(on_cancel) = self.on_cancel.take() {
            on_cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn unpolled_response_runs_cancellation_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let guard = CompletionGuard::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let body = async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        };
        drop(body);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn partially_consumed_stream_runs_cancellation_once() {
        use futures_util::StreamExt;
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let guard = CompletionGuard::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let mut stream = Box::pin(async_stream::stream! {
            let _guard = guard;
            yield 1;
            std::future::pending::<()>().await;
        });
        assert_eq!(stream.next().await, Some(1));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(stream);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rearmed_guard_runs_only_the_new_callback_once() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let (f, s) = (first.clone(), second.clone());
        let mut guard = CompletionGuard::new(move || {
            f.fetch_add(1, Ordering::SeqCst);
        });
        guard.rearm(move || {
            s.fetch_add(1, Ordering::SeqCst);
        });
        drop(guard);
        assert_eq!(first.load(Ordering::SeqCst), 0);
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn completed_rearmed_guard_never_runs_a_callback() {
        let count = Arc::new(AtomicUsize::new(0));
        let (a, b) = (count.clone(), count.clone());
        let mut guard = CompletionGuard::new(move || {
            a.fetch_add(100, Ordering::SeqCst);
        });
        guard.rearm(move || {
            b.fetch_add(100, Ordering::SeqCst);
        });
        let counter = count.clone();
        guard
            .complete(async move {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn settlement_survives_consumer_drop_without_fallback() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let guard = CompletionGuard::new(move || {
            counter.fetch_add(100, Ordering::SeqCst);
        });
        let (release, wait) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let counter = count.clone();
        let task = guard.complete(async move {
            wait.await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            done.send(()).unwrap();
        });
        drop(task);
        release.send(()).unwrap();
        finished.await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
