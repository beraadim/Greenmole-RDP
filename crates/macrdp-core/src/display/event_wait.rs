use std::future::Future;
use std::time::Duration;

pub(super) enum DisplayWake<T> {
    Capture(T),
    Cursor,
    Idle,
}

async fn wait_deadline(deadline: Option<Duration>) {
    match deadline {
        Some(deadline) => tokio::time::sleep(deadline).await,
        None => std::future::pending().await,
    }
}

/// Return to the display loop on every deadline, even without a new capture.
pub(super) async fn next_event<F: Future>(
    capture: F,
    idle_deadline: Option<Duration>,
    cursor_deadline: Option<Duration>,
) -> DisplayWake<F::Output> {
    tokio::select! {
        capture = capture => DisplayWake::Capture(capture),
        _ = wait_deadline(cursor_deadline) => DisplayWake::Cursor,
        _ = wait_deadline(idle_deadline) => DisplayWake::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_capture_yields_to_cursor_then_waits_for_its_next_deadline() {
        assert!(matches!(
            next_event(std::future::pending::<()>(), None, Some(Duration::ZERO)).await,
            DisplayWake::Cursor
        ));
        // The outer loop services the cursor and supplies its new deadline.
        // The wait must not retain the expired timer or wake on its own.
        assert!(tokio::time::timeout(
            Duration::from_millis(2),
            next_event(
                std::future::pending::<()>(),
                None,
                Some(Duration::from_secs(1))
            )
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn encode_completion_is_deliverable_without_a_capture() {
        let (sender, mut completion) = tokio::sync::watch::channel(false);
        let wait = async {
            let wake = next_event(completion.wait_for(|ready| *ready), None, None).await;
            assert!(matches!(wake, DisplayWake::Capture(Ok(_))));
        };
        let complete = async {
            tokio::task::yield_now().await;
            sender.send_replace(true);
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(wait, complete);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn idle_keepalive_does_not_wait_for_cursor_or_capture() {
        assert!(matches!(
            next_event(std::future::pending::<()>(), Some(Duration::ZERO), None).await,
            DisplayWake::Idle
        ));
    }
}
