//! Bridges native dialog callbacks to async IPC without blocking an executor thread.

use jarvis_core::{ErrorCode, ErrorPayload};
use tauri_plugin_dialog::FilePath;
use tokio::sync::oneshot;

type SelectionCallback = Box<dyn FnOnce(Option<FilePath>) + Send>;

/// `None` is user cancellation; a lost callback is an unavailable dialog.
/// Dropping this future does not close the native dialog. Its late callback is safe.
pub(crate) async fn selection(
    open: impl FnOnce(SelectionCallback),
) -> Result<Option<FilePath>, ErrorPayload> {
    let (sender, receiver) = oneshot::channel();
    open(Box::new(move |picked| {
        let _ = sender.send(picked);
    }));
    receiver.await.map_err(|_| {
        ErrorPayload::new(
            ErrorCode::Unavailable,
            "The file dialog closed without a result",
        )
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn selection_allows_other_work_on_the_same_executor() {
        let picked = tokio::time::timeout(
            Duration::from_secs(1),
            selection(|complete| {
                let _worker = tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    complete(Some(FilePath::Path(PathBuf::from("chosen.gguf"))));
                });
            }),
        )
        .await
        .expect("callback ran on the same executor")
        .expect("dialog")
        .expect("selection");
        assert_eq!(
            picked.into_path().expect("path"),
            PathBuf::from("chosen.gguf")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_is_not_an_error() {
        assert!(
            selection(|complete| complete(None))
                .await
                .expect("cancel")
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lost_callback_is_reported_instead_of_hanging() {
        let error = selection(drop).await.expect_err("lost callback");
        assert_eq!(error.code, ErrorCode::Unavailable);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn callback_after_the_caller_is_dropped_is_safe() {
        let (send, receive) = oneshot::channel();
        let waiting = tokio::spawn(selection(move |complete| {
            let _ = send.send(complete);
        }));
        let complete = receive.await.expect("callback registered");
        waiting.abort();
        assert!(waiting.await.expect_err("aborted").is_cancelled());
        complete(None);
    }
}
