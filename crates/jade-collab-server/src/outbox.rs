//! A connection's outgoing queue, bounded. The document's thread never
//! waits on a connection: a message that does not fit marks the connection
//! as overloaded, the document stops sending to it, and the connection
//! closes; its replica resumes on a new one.

use std::sync::Arc;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::Notify;

/// How many messages wait for a connection before it counts as too slow.
pub const DEFAULT_OUTGOING_LIMIT: usize = 1024;

/// The sending end, held by the connection and the document's thread.
#[derive(Clone, Debug)]
pub(crate) struct Outbox {
    sender: mpsc::Sender<String>,
    overloaded: Arc<Notify>,
}

/// Fires once a message did not fit.
pub(crate) struct Overloaded(Arc<Notify>);

impl Overloaded {
    pub async fn wait(&self) {
        self.0.notified().await;
    }
}

/// A queue of `limit` messages (at least one).
pub(crate) fn outbox(limit: usize) -> (Outbox, mpsc::Receiver<String>, Overloaded) {
    let (sender, receiver) = mpsc::channel(limit.max(1));
    let overloaded = Arc::new(Notify::new());
    (
        Outbox {
            sender,
            overloaded: overloaded.clone(),
        },
        receiver,
        Overloaded(overloaded),
    )
}

impl Outbox {
    /// Queues `message`; `Err` when the connection is gone or too far
    /// behind, and the sender should stop sending to it.
    pub fn send(&self, message: String) -> Result<(), ()> {
        match self.sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.overloaded.notify_one();
                Err(())
            }
            Err(TrySendError::Closed(_)) => Err(()),
        }
    }

    pub fn same_channel(&self, other: &Outbox) -> bool {
        self.sender.same_channel(&other.sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_full_queue_refuses_and_says_so_once_read_it_takes_again() {
        let (outbox, mut queue, overloaded) = outbox(2);
        assert!(outbox.send("a".into()).is_ok());
        assert!(outbox.send("b".into()).is_ok());
        assert!(outbox.send("c".into()).is_err());
        // The notification was kept for whoever waits next.
        tokio::time::timeout(std::time::Duration::from_secs(1), overloaded.wait())
            .await
            .expect("overloaded was not signalled");
        assert_eq!(queue.recv().await.as_deref(), Some("a"));
        assert!(outbox.send("d".into()).is_ok());
    }

    #[tokio::test]
    async fn a_closed_queue_refuses_without_overload() {
        let (outbox, queue, overloaded) = outbox(2);
        drop(queue);
        assert!(outbox.send("a".into()).is_err());
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(50), overloaded.wait()).await;
        assert!(waited.is_err(), "a closed connection is not overloaded");
    }
}
