//! Count and wire-byte bounded document input. Reserve before decoding JSON;
//! keep the byte lease through processing. Shutdown drains committed envelopes
//! without waiting for uncommitted reservations on a blocked Tokio executor.
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};

/// Queued/reserved document commands; excludes the one being processed.
pub const DEFAULT_INCOMING_LIMIT: usize = 128;
/// Combined encoded frame bytes retained by admission and processing.
pub const DEFAULT_INCOMING_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    #[error("the document stopped")]
    Closed,
    #[error("one message exceeds the {limit}-byte document input limit")]
    TooLarge { limit: usize },
}

struct Shared {
    closed: Mutex<bool>,
    changed: Notify,
    bytes: Arc<Semaphore>,
    budget: usize,
}
impl Shared {
    fn close(&self) {
        let changed = {
            let mut closed = self.closed.lock().unwrap();
            !std::mem::replace(&mut *closed, true)
        };
        if changed {
            self.bytes.close();
            self.changed.notify_waiters();
        }
    }
    async fn stopped(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if *self.closed.lock().unwrap() {
                return;
            }
            notified.await;
        }
    }
}

pub(crate) struct Sender<T> {
    sender: mpsc::Sender<Envelope<T>>,
    shared: Arc<Shared>,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            shared: self.shared.clone(),
        }
    }
}
pub(crate) struct Inbox<T> {
    receiver: mpsc::Receiver<Envelope<T>>,
    shared: Arc<Shared>,
    draining: bool,
}
pub(crate) struct Envelope<T> {
    pub value: T,
    pub _bytes: OwnedSemaphorePermit,
}
pub(crate) struct Permit<T> {
    slot: mpsc::OwnedPermit<Envelope<T>>,
    bytes: OwnedSemaphorePermit,
    shared: Arc<Shared>,
}

pub(crate) fn channel<T>(count: usize, bytes: usize) -> (Sender<T>, Inbox<T>) {
    let budget = bytes.clamp(1, Semaphore::MAX_PERMITS.min(u32::MAX as usize));
    let (sender, receiver) = mpsc::channel(count.max(1));
    let shared = Arc::new(Shared {
        closed: Mutex::new(false),
        changed: Notify::new(),
        bytes: Arc::new(Semaphore::new(budget)),
        budget,
    });
    (
        Sender {
            sender,
            shared: shared.clone(),
        },
        Inbox {
            receiver,
            shared,
            draining: false,
        },
    )
}
impl<T> Sender<T> {
    pub fn close(&self) {
        self.shared.close();
    }
    pub async fn stopped(&self) {
        self.shared.stopped().await;
    }
    pub async fn reserve(&self, wire_bytes: usize) -> Result<Permit<T>, Error> {
        if *self.shared.closed.lock().unwrap() {
            return Err(Error::Closed);
        }
        let weight = wire_bytes.max(1);
        if weight > self.shared.budget {
            return Err(Error::TooLarge {
                limit: self.shared.budget,
            });
        }
        let acquire = async {
            let bytes = self
                .shared
                .bytes
                .clone()
                .acquire_many_owned(weight as u32)
                .await
                .map_err(|_| Error::Closed)?;
            let slot = self
                .sender
                .clone()
                .reserve_owned()
                .await
                .map_err(|_| Error::Closed)?;
            Ok(Permit {
                slot,
                bytes,
                shared: self.shared.clone(),
            })
        };
        tokio::select! {biased; _=self.shared.stopped()=>Err(Error::Closed), result=acquire=>result}
    }
    pub async fn send(&self, value: T) -> Result<(), Error> {
        self.reserve(1).await?.send(value)
    }
    #[cfg(test)]
    pub fn blocking_send(&self, value: T) -> Result<(), Error> {
        futures::executor::block_on(self.send(value))
    }
    #[cfg(test)]
    pub fn bytes_in_use(&self) -> usize {
        self.shared.budget - self.shared.bytes.available_permits()
    }
}
impl<T> Permit<T> {
    pub fn send(self, value: T) -> Result<(), Error> {
        // Linearize closure and publication together. Thus shutdown's drain
        // cannot miss a message committed just before the gate closed.
        let closed = self.shared.closed.lock().unwrap();
        if *closed {
            return Err(Error::Closed);
        }
        self.slot.send(Envelope {
            value,
            _bytes: self.bytes,
        });
        Ok(())
    }
}
impl<T> Inbox<T> {
    pub async fn recv(&mut self) -> Option<Envelope<T>> {
        if self.draining {
            return self.receiver.try_recv().ok();
        }
        tokio::select! {
            biased;
            _=self.shared.stopped()=>{
                self.receiver.close();self.draining=true;
                self.receiver.try_recv().ok()
            }
            message=self.receiver.recv()=>message,
        }
    }
    pub fn blocking_recv(&mut self) -> Option<Envelope<T>> {
        futures::executor::block_on(self.recv())
    }
}
impl<T> Drop for Inbox<T> {
    fn drop(&mut self) {
        self.shared.close();
    }
}

#[cfg(test)]
mod tests;
