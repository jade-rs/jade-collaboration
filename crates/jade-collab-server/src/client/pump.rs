//! The replica is the edit queue. Keep only a cursor and one in-flight wire
//! message; inspections capture an exclusive ticket barrier under its lock.
use super::*;
use futures_util::Sink;

pub(super) struct Cursor {
    next: u64,
    last_was_presence: bool,
}

enum Outgoing<Op> {
    Submit(Submission<Op>),
    Inspect(u64),
    Presence(String),
}

impl Cursor {
    pub(super) fn new(next: u64) -> Self {
        Self {
            next,
            last_was_presence: false,
        }
    }

    fn take<D: Document>(&mut self, shared: &Shared<D>) -> Option<Outgoing<D::Op>> {
        let replica = shared.replica.lock().unwrap();
        let error = shared.error.lock().unwrap();
        if error.is_some() {
            return None;
        }
        let mut inspections = shared.inspections.lock().unwrap();
        self.next = self.next.max(
            replica
                .first_pending_client_seq()
                .unwrap_or(replica.next_client_seq()),
        );
        let inspection = inspections.waiting.iter_mut().find(|(_, p)| !p.sent);
        let inspection_ready = inspection
            .as_ref()
            .is_some_and(|(_, p)| p.next_client_seq <= self.next);
        let edit_ready = self.next < replica.next_client_seq();
        // At most one presence update between durable/control messages: a
        // continuous cursor producer cannot starve document edits.
        if !self.last_was_presence || (!inspection_ready && !edit_ready) {
            if let Some(message) = shared.latest_presence.lock().unwrap().take() {
                self.last_was_presence = true;
                return Some(Outgoing::Presence(message));
            }
        }
        self.last_was_presence = false;
        if inspection_ready {
            let (request, pending) = inspection.unwrap();
            pending.sent = true;
            return Some(Outgoing::Inspect(*request));
        }
        let submission = replica.pending_submission(self.next)?;
        // Replica refuses issuing u64::MAX, so an issued ticket has a successor.
        self.next = submission.client_seq + 1;
        Some(Outgoing::Submit(submission))
    }

    pub(super) fn message<D>(&mut self, shared: &Shared<D>) -> Result<Option<Message>, ClientError>
    where
        D: Document + DeserializeOwned + Send + 'static,
        D::Op: Serialize + DeserializeOwned + Send + 'static,
    {
        self.take(shared)
            .map(|outgoing| match outgoing {
                Outgoing::Submit(submission) => Client::<D>::submit_message(submission),
                Outgoing::Inspect(request) => Ok(text(&ClientMessage::Inspect { request })),
                Outgoing::Presence(message) => Ok(Message::Text(message.into())),
            })
            .transpose()
    }
}

impl<D: Document> Shared<D> {
    // First terminal cause wins. Registration takes error -> inspections too,
    // so no inspection can miss this drain and wait forever.
    pub(super) fn fail(&self, failure: ClientError) {
        let mut error = self.error.lock().unwrap();
        if error.is_some() {
            return;
        }
        *error = Some(failure.clone());
        let waiting = std::mem::take(&mut self.inspections.lock().unwrap().waiting);
        drop(error);
        for (_, pending) in waiting {
            let _ = pending.answer.send(Err(failure.clone()));
        }
        self.changed.notify_waiters();
        self.outgoing.notify_one();
    }

    pub(super) async fn failed(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.error.lock().unwrap().is_some() {
                return;
            }
            notified.await;
        }
    }
}

pub(super) async fn write<D, S>(shared: Arc<Shared<D>>, mut cursor: Cursor, mut sink: S)
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    loop {
        let notified = shared.outgoing.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let message = match cursor.message(&shared) {
            Ok(Some(message)) => message,
            Ok(None) => {
                if shared.error.lock().unwrap().is_some() {
                    return;
                }
                notified.await;
                continue;
            }
            Err(error) => {
                shared.fail(error);
                return;
            }
        };
        let result = tokio::select! {
            biased;
            _ = shared.failed() => return,
            result = sink.send(message) => result,
        };
        if let Err(error) = result {
            shared.fail(ClientError::Protocol(format!(
                "cannot send message: {error}"
            )));
            return;
        }
    }
}
