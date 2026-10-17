use std::collections::VecDeque;

use jade_collab_core::id::{Clock, Id, ReplicaId};

use crate::protocol::{Committed, Downstream, Resume, Submission, Welcome};
use crate::{ApplyContext, Document, Lane, Rejection, SyncError};

/// The client core: an optimistic view over the server's log.
///
/// `confirmed` is the document after every committed operation received so
/// far; `view` is `confirmed` with the pending local operations replayed on
/// top. A remote convergent operation commutes with everything pending, so it
/// is applied to both directly (Lean: `fast_path_sound`); a remote sequenced
/// operation may change what the pending ones mean, so the view is rebuilt.
pub struct Replica<D: Document> {
    id: ReplicaId,
    clock: Clock,
    confirmed: D,
    view: D,
    seq: u64,
    pending: VecDeque<Pending<D::Op>>,
    next_client_seq: u64,
}

struct Pending<Op> {
    submission: Submission<Op>,
    /// Whether it applied in the current view. A pending operation can stop
    /// applying after a rebuild; the server will reject it too.
    applied: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplicaEvent<Op> {
    Applied {
        origin: ReplicaId,
        seq: u64,
    },
    Acknowledged {
        client_seq: u64,
    },
    Rejected {
        client_seq: u64,
        op: Op,
        reason: Rejection,
    },
}

impl<D: Document> Replica<D> {
    pub fn new(id: ReplicaId, seq: u64, snapshot: D) -> Self {
        let mut clock = Clock::new(id);
        clock.observe_lamport(snapshot.max_lamport());
        Self {
            id,
            clock,
            view: snapshot.clone(),
            confirmed: snapshot,
            seq,
            pending: VecDeque::new(),
            next_client_seq: 1,
        }
    }

    pub fn from_welcome(welcome: Welcome<D>) -> Self {
        Self::new(welcome.replica, welcome.seq, welcome.snapshot)
    }

    pub fn id(&self) -> ReplicaId {
        self.id
    }

    /// The last log position received.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// What the user sees: confirmed state plus pending local operations.
    pub fn view(&self) -> &D {
        &self.view
    }

    pub fn confirmed(&self) -> &D {
        &self.confirmed
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn next_client_seq(&self) -> u64 {
        self.next_client_seq
    }

    /// Everything still unanswered, to resend after a reconnect.
    pub fn pending_submissions(&self) -> Vec<Submission<D::Op>> {
        self.pending.iter().map(|p| p.submission.clone()).collect()
    }

    /// A fresh identifier, newer than anything in the view.
    pub fn next_id(&mut self) -> Id {
        self.reserve(1)
    }

    /// Reserves `n` consecutive identifiers (for a typed run) and returns the first.
    pub fn reserve(&mut self, n: u64) -> Id {
        self.clock.observe_lamport(self.view.max_lamport());
        self.clock.reserve(n)
    }

    fn tentative(&self, base_seq: u64) -> ApplyContext {
        ApplyContext {
            origin: self.id,
            base_seq,
            seq: None,
        }
    }

    /// Applies a local operation to the view and returns the submission to
    /// send. An operation that does not apply locally is refused here and
    /// never sent.
    pub fn submit(&mut self, op: D::Op) -> Result<Submission<D::Op>, Rejection> {
        let ctx = self.tentative(self.seq);
        self.view.apply(&op, &ctx)?;
        let submission = Submission {
            client_seq: self.next_client_seq,
            base_seq: self.seq,
            op,
        };
        self.next_client_seq += 1;
        self.pending.push_back(Pending {
            submission: submission.clone(),
            applied: true,
        });
        Ok(submission)
    }

    pub fn receive(
        &mut self,
        message: Downstream<D::Op>,
    ) -> Result<ReplicaEvent<D::Op>, SyncError> {
        match message {
            Downstream::Committed(committed) => self.receive_committed(committed),
            Downstream::Rejected(rejected) => {
                let front = self.pending.front().map(|p| p.submission.client_seq);
                if front != Some(rejected.client_seq) {
                    return Err(SyncError::UnexpectedReply {
                        client_seq: rejected.client_seq,
                    });
                }
                let pending = self.pending.pop_front().expect("checked above");
                if pending.applied {
                    self.rebuild();
                }
                Ok(ReplicaEvent::Rejected {
                    client_seq: rejected.client_seq,
                    op: pending.submission.op,
                    reason: rejected.reason,
                })
            }
        }
    }

    fn receive_committed(
        &mut self,
        committed: Committed<D::Op>,
    ) -> Result<ReplicaEvent<D::Op>, SyncError> {
        if committed.seq != self.seq + 1 {
            return Err(SyncError::OutOfOrder {
                expected: self.seq + 1,
                received: committed.seq,
            });
        }
        let ctx = ApplyContext {
            origin: committed.origin,
            base_seq: committed.base_seq,
            seq: Some(committed.seq),
        };
        if committed.origin == self.id {
            let front = self.pending.front().map(|p| p.submission.client_seq);
            if front != Some(committed.client_seq) {
                return Err(SyncError::UnexpectedReply {
                    client_seq: committed.client_seq,
                });
            }
        }
        self.confirmed
            .apply(&committed.op, &ctx)
            .map_err(|reason| SyncError::Diverged {
                seq: committed.seq,
                reason: reason.to_string(),
            })?;
        self.seq = committed.seq;

        if committed.origin == self.id {
            let pending = self.pending.pop_front().expect("checked above");
            // A convergent op lands the same tentatively and committed. A
            // sequenced one may not — it may have failed in the view, or its
            // result may depend on where it was committed — so the view is
            // rebuilt from the authoritative application.
            if !pending.applied || D::lane(&committed.op) == Lane::Sequenced {
                self.rebuild();
            }
            return Ok(ReplicaEvent::Acknowledged {
                client_seq: committed.client_seq,
            });
        }
        match D::lane(&committed.op) {
            Lane::Convergent => {
                if self.view.apply(&committed.op, &ctx).is_err() {
                    self.rebuild();
                }
            }
            Lane::Sequenced => self.rebuild(),
        }
        Ok(ReplicaEvent::Applied {
            origin: committed.origin,
            seq: committed.seq,
        })
    }

    /// Catches up after a reconnect. Pending submissions the server handled
    /// but did not commit were rejected while the replica was away.
    pub fn resume(&mut self, resume: Resume<D::Op>) -> Result<Vec<ReplicaEvent<D::Op>>, SyncError> {
        let mut events = Vec::new();
        for committed in resume.missed {
            if committed.origin == self.id {
                events.extend(self.drop_rejected_before(committed.client_seq));
            }
            events.push(self.receive_committed(committed)?);
        }
        events.extend(self.drop_rejected_before(resume.processed_client_seq + 1));
        Ok(events)
    }

    fn drop_rejected_before(&mut self, client_seq: u64) -> Vec<ReplicaEvent<D::Op>> {
        let mut events = Vec::new();
        let mut rebuild = false;
        while self
            .pending
            .front()
            .is_some_and(|p| p.submission.client_seq < client_seq)
        {
            let pending = self.pending.pop_front().expect("checked above");
            rebuild |= pending.applied;
            events.push(ReplicaEvent::Rejected {
                client_seq: pending.submission.client_seq,
                op: pending.submission.op,
                reason: Rejection::new("rejected while disconnected"),
            });
        }
        if rebuild {
            self.rebuild();
        }
        events
    }

    fn rebuild(&mut self) {
        self.view = self.confirmed.clone();
        let id = self.id;
        for pending in &mut self.pending {
            let ctx = ApplyContext {
                origin: id,
                base_seq: pending.submission.base_seq,
                seq: None,
            };
            pending.applied = self.view.apply(&pending.submission.op, &ctx).is_ok();
        }
    }

    /// `confirmed` with the pending operations replayed — what [`Replica::view`]
    /// must always equal. For tests and assertions.
    pub fn rebuilt_view(&self) -> D {
        let mut view = self.confirmed.clone();
        for pending in &self.pending {
            let ctx = self.tentative(pending.submission.base_seq);
            let _ = view.apply(&pending.submission.op, &ctx);
        }
        view
    }
}
