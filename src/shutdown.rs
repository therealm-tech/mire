//! The flag that says the process is stopping, shared with the handlers.
//!
//! A graceful shutdown stops accepting connections and then waits for the open
//! ones to finish. `GET /api/events` never finishes on its own, so without a way
//! to tell it the process is going, one browser tab left open is enough to make
//! `mire` hang on `SIGINT` forever.

use std::sync::Arc;

use tokio::sync::watch;

/// A shared "we are stopping" flag, cloned into every task that needs to know.
#[derive(Clone, Debug)]
pub struct Shutdown {
    sender: Arc<watch::Sender<bool>>,
}

impl Shutdown {
    /// A flag nobody has raised yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sender: Arc::new(watch::Sender::new(false)),
        }
    }

    /// Raises it, waking every waiter.
    ///
    /// Idempotent: a second signal while the drain is running says nothing new.
    pub fn begin(&self) {
        self.sender.send_replace(true);
    }

    /// Completes once [`Shutdown::begin`] has been called, at once if it already
    /// has.
    pub async fn begun(&self) {
        let mut receiver = self.sender.subscribe();
        // `changed()` only reports a transition, so a task that subscribes after
        // the flag went up would sit here waiting for a second signal that never
        // comes — which is the hang this type exists to remove.
        if *receiver.borrow_and_update() {
            return;
        }
        let _ = receiver.changed().await;
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn waiting_ends_when_the_flag_goes_up() {
        let shutdown = Shutdown::new();
        let waiter = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { shutdown.begun().await }
        });

        shutdown.begin();

        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter was not woken")
            .expect("the waiter panicked");
    }

    #[tokio::test]
    async fn waiting_after_the_flag_went_up_returns_at_once() {
        let shutdown = Shutdown::new();
        shutdown.begin();
        shutdown.begin();

        tokio::time::timeout(Duration::from_secs(5), shutdown.begun())
            .await
            .expect("a late waiter was left hanging");
    }

    #[tokio::test]
    async fn waiting_does_not_end_on_its_own() {
        let shutdown = Shutdown::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), shutdown.begun())
                .await
                .is_err()
        );
    }
}
