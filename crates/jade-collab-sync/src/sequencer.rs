use std::collections::BTreeMap;

use jade_collab_core::id::ReplicaId;
use serde::{Deserialize, Serialize};

use crate::protocol::{Committed, Downstream, Rejected, Resume, Submission, Welcome};
use crate::{ApplyContext, Document, Lane, Materializer, NoMaterializer, Rejection, SyncError};

/// The server core for one document: a single totally ordered log.
///
/// Every submission is applied to the authoritative model in arrival order.
/// A convergent operation applies wherever it lands; a sequenced one is
/// validated against everything committed before it and rejected if it no
/// longer fits. Committed effects go to the [`Materializer`] in log order.
pub struct Sequencer<D: Document, M = NoMaterializer> {
    document: D,
    materializer: M,
    /// The log position `log` starts after: everything up to it was dropped
    /// by [`Sequencer::compact`] (or lives in the checkpoint restored from).
    log_start: u64,
    log: Vec<Committed<D::Op>>,
    /// The newest client sequence number handled per replica.
    processed: BTreeMap<ReplicaId, u64>,
    next_replica: u64,
    faults: Vec<String>,
    /// Operations derived since the last [`Sequencer::take_derived`].
    derived: Vec<Committed<D::Op>>,
}

/// Everything a sequencer is at one log position, except the materializer
/// (whose state the server keeps as a file) and the log before it. With the
/// [`Record`]s journaled after it, [`Sequencer::restore`] and
/// [`Sequencer::replay`] rebuild the sequencer exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint<D> {
    pub seq: u64,
    /// The newest client sequence number handled per replica; every replica
    /// that ever joined is listed.
    pub processed: BTreeMap<ReplicaId, u64>,
    /// The id the next joining replica gets.
    pub next_replica: u64,
    pub faults: Vec<String>,
    pub document: D,
}

/// One change to a sequencer's state, in the order it happened: what a
/// durable server appends to its journal before answering anyone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record<Op> {
    Joined {
        replica: ReplicaId,
    },
    Committed(Committed<Op>),
    /// A rejection changes nothing but the replica's processed client
    /// sequence number, which decides what a resume reports.
    Rejected {
        replica: ReplicaId,
        client_seq: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SubmitOutcome<Op> {
    /// Broadcast to every replica (for the origin it is the acknowledgement).
    Committed(Committed<Op>),
    /// Sent back to the origin only.
    Rejected(Rejected),
    /// Already handled; nothing to send.
    Duplicate,
}

impl<Op: Clone> SubmitOutcome<Op> {
    pub fn downstream(&self) -> Option<Downstream<Op>> {
        match self {
            SubmitOutcome::Committed(c) => Some(Downstream::Committed(c.clone())),
            SubmitOutcome::Rejected(r) => Some(Downstream::Rejected(r.clone())),
            SubmitOutcome::Duplicate => None,
        }
    }

    /// What the sequencer must journal for this outcome of a submission
    /// `from` a replica; nothing for a duplicate.
    pub fn record(&self, from: ReplicaId) -> Option<Record<Op>> {
        match self {
            SubmitOutcome::Committed(c) => Some(Record::Committed(c.clone())),
            SubmitOutcome::Rejected(r) => Some(Record::Rejected {
                replica: from,
                client_seq: r.client_seq,
            }),
            SubmitOutcome::Duplicate => None,
        }
    }
}

impl<D: Document> Sequencer<D, NoMaterializer> {
    pub fn new(document: D) -> Self {
        Self::with_materializer(document, NoMaterializer)
    }
}

impl<D: Document, M: Materializer<D>> Sequencer<D, M> {
    pub fn with_materializer(document: D, materializer: M) -> Self {
        Self {
            document,
            materializer,
            log_start: 0,
            log: Vec::new(),
            processed: BTreeMap::new(),
            next_replica: ReplicaId::ORIGIN.0 + 1,
            faults: Vec::new(),
            derived: Vec::new(),
        }
    }

    /// The sequencer `checkpoint` was taken of, with `materializer` holding
    /// the real document at the same log position. Its log starts there.
    pub fn restore(checkpoint: Checkpoint<D>, materializer: M) -> Self {
        Self {
            document: checkpoint.document,
            materializer,
            log_start: checkpoint.seq,
            log: Vec::new(),
            processed: checkpoint.processed,
            next_replica: checkpoint.next_replica,
            faults: checkpoint.faults,
            derived: Vec::new(),
        }
    }

    pub fn checkpoint(&self) -> Checkpoint<D> {
        Checkpoint {
            seq: self.seq(),
            processed: self.processed.clone(),
            next_replica: self.next_replica,
            faults: self.faults.clone(),
            document: self.document.clone(),
        }
    }

    /// [`Sequencer::restore`], with the `tail` of the log that was kept
    /// before the checkpoint (see [`Sequencer::compact_keeping`]): replicas
    /// within it can resume. The tail must end at the checkpoint, without
    /// gaps.
    pub fn restore_with_tail(
        checkpoint: Checkpoint<D>,
        tail: Vec<Committed<D::Op>>,
        materializer: M,
    ) -> Result<Self, SyncError> {
        let journal = |reason: String| SyncError::Journal { reason };
        let start = checkpoint
            .seq
            .checked_sub(tail.len() as u64)
            .ok_or_else(|| {
                journal(format!(
                    "a tail of {} operations cannot end at log position {}",
                    tail.len(),
                    checkpoint.seq
                ))
            })?;
        for (expected, committed) in (start + 1..).zip(&tail) {
            if committed.seq != expected {
                return Err(journal(format!(
                    "the tail holds log position {} where {expected} belongs",
                    committed.seq
                )));
            }
        }
        let mut sequencer = Self::restore(checkpoint, materializer);
        sequencer.log_start = start;
        sequencer.log = tail;
        Ok(sequencer)
    }

    /// Drops the log up to the current position, once a checkpoint of it is
    /// safe. Replicas that are further behind can no longer resume.
    pub fn compact(&mut self) {
        self.compact_keeping(0);
    }

    /// Like [`Sequencer::compact`], but keeps the last `keep` committed
    /// operations (or as many as there are), so that a replica at most
    /// `keep` operations behind can always still resume.
    pub fn compact_keeping(&mut self, keep: usize) {
        let dropped = self.log.len().saturating_sub(keep);
        self.log.drain(..dropped);
        self.log_start += dropped as u64;
    }

    /// Applies a journaled record, as the sequencer did when it was
    /// recorded. Committed operations are applied and materialized without
    /// being admitted again: they are facts.
    pub fn replay(&mut self, record: Record<D::Op>) -> Result<(), SyncError> {
        match record {
            Record::Joined { replica } => {
                if replica.0 != self.next_replica {
                    return Err(SyncError::Journal {
                        reason: format!(
                            "{replica} joined, but the next replica to join is r{}",
                            self.next_replica
                        ),
                    });
                }
                self.join();
            }
            Record::Committed(committed) => {
                let derived = committed.origin == ReplicaId::ORIGIN;
                if !derived && !self.processed.contains_key(&committed.origin) {
                    return Err(SyncError::UnknownReplica(committed.origin));
                }
                if committed.seq != self.seq() + 1 {
                    return Err(SyncError::OutOfOrder {
                        expected: self.seq() + 1,
                        received: committed.seq,
                    });
                }
                let ctx = ApplyContext {
                    origin: committed.origin,
                    base_seq: committed.base_seq,
                    seq: Some(committed.seq),
                };
                let effect = self.document.apply(&committed.op, &ctx).map_err(|reason| {
                    SyncError::Diverged {
                        seq: committed.seq,
                        reason: reason.to_string(),
                    }
                })?;
                self.materialize(effect, &ctx);
                if !derived {
                    self.processed
                        .insert(committed.origin, committed.client_seq);
                }
                self.log.push(committed);
            }
            Record::Rejected {
                replica,
                client_seq,
            } => {
                // Like `submit`, which ignores strangers.
                if let Some(processed) = self.processed.get_mut(&replica) {
                    *processed = client_seq;
                }
            }
        }
        Ok(())
    }

    /// Commits what the real document did on its own after `cause`, as
    /// operations of [`ReplicaId::ORIGIN`]. A derived operation the model
    /// refuses is a fault.
    fn derive(&mut self, cause: &D::Op) {
        for op in self.materializer.derived(&self.document, cause) {
            let seq = self.seq() + 1;
            let ctx = ApplyContext {
                origin: ReplicaId::ORIGIN,
                base_seq: seq - 1,
                seq: Some(seq),
            };
            match self.document.apply(&op, &ctx) {
                Ok(effect) => {
                    self.materialize(effect, &ctx);
                    let committed = Committed {
                        seq,
                        origin: ReplicaId::ORIGIN,
                        client_seq: 0,
                        base_seq: seq - 1,
                        op,
                    };
                    self.log.push(committed.clone());
                    self.derived.push(committed);
                }
                Err(reason) => self.faults.push(format!(
                    "seq {seq}: derived {op:?} does not apply: {reason}"
                )),
            }
        }
    }

    /// Commits `op` on behalf of [`ReplicaId::ORIGIN`], outside any
    /// replica's submission — what the server itself knows and the model
    /// must hear in log order (who a replica is, say). Applied and
    /// materialized like a commit; the caller journals and broadcasts it.
    /// Nothing is admitted or derived: the server is the author.
    pub fn commit_origin(&mut self, op: D::Op) -> Result<Committed<D::Op>, Rejection> {
        let seq = self.seq() + 1;
        let ctx = ApplyContext {
            origin: ReplicaId::ORIGIN,
            base_seq: seq - 1,
            seq: Some(seq),
        };
        let effect = self.document.apply(&op, &ctx)?;
        self.materialize(effect, &ctx);
        let committed = Committed {
            seq,
            origin: ReplicaId::ORIGIN,
            client_seq: 0,
            base_seq: seq - 1,
            op,
        };
        self.log.push(committed.clone());
        Ok(committed)
    }

    /// The operations derived since the last call, already in the log, for
    /// the caller to journal and broadcast after the commit that caused them.
    pub fn take_derived(&mut self) -> Vec<Committed<D::Op>> {
        std::mem::take(&mut self.derived)
    }

    fn materialize(&mut self, effect: D::Effect, ctx: &ApplyContext) {
        if let Err(fault) = self.materializer.materialize(&self.document, effect, ctx) {
            let seq = ctx.seq.expect("only committed operations are materialized");
            self.faults.push(format!("seq {seq}: {fault}"));
        }
    }

    pub fn document(&self) -> &D {
        &self.document
    }

    pub fn materializer(&self) -> &M {
        &self.materializer
    }

    pub fn materializer_mut(&mut self) -> &mut M {
        &mut self.materializer
    }

    /// The position of the last committed operation (0 for none).
    pub fn seq(&self) -> u64 {
        self.log_start + self.log.len() as u64
    }

    /// The committed operations after [`Sequencer::log_start`].
    pub fn log(&self) -> &[Committed<D::Op>] {
        &self.log
    }

    /// The log position the kept log starts after.
    pub fn log_start(&self) -> u64 {
        self.log_start
    }

    pub fn processed_client_seq(&self, replica: ReplicaId) -> u64 {
        self.processed.get(&replica).copied().unwrap_or(0)
    }

    /// Materialization failures. Each is a bug — the model accepted an
    /// operation the real document could not take — and leaves the file
    /// behind the model; they are kept rather than hidden.
    pub fn faults(&self) -> &[String] {
        &self.faults
    }

    /// Admits a new replica.
    pub fn join(&mut self) -> Welcome<D> {
        let replica = ReplicaId(self.next_replica);
        self.next_replica += 1;
        self.processed.insert(replica, 0);
        Welcome {
            replica,
            seq: self.seq(),
            snapshot: self.document.clone(),
        }
    }

    /// What `replica`, last at log position `seq`, missed.
    pub fn resume(&self, replica: ReplicaId, seq: u64) -> Result<Resume<D::Op>, SyncError> {
        let processed_client_seq = *self
            .processed
            .get(&replica)
            .ok_or(SyncError::UnknownReplica(replica))?;
        if seq > self.seq() {
            return Err(SyncError::FutureSeq {
                requested: seq,
                current: self.seq(),
            });
        }
        if seq < self.log_start {
            return Err(SyncError::Compacted {
                requested: seq,
                oldest: self.log_start,
            });
        }
        Ok(Resume {
            processed_client_seq,
            missed: self.log[(seq - self.log_start) as usize..].to_vec(),
        })
    }

    pub fn submit(
        &mut self,
        from: ReplicaId,
        submission: Submission<D::Op>,
    ) -> SubmitOutcome<D::Op> {
        let Some(processed) = self.processed.get_mut(&from) else {
            return SubmitOutcome::Rejected(Rejected {
                client_seq: submission.client_seq,
                reason: Rejection::new(SyncError::UnknownReplica(from).to_string()),
            });
        };
        if submission.client_seq <= *processed {
            return SubmitOutcome::Duplicate;
        }
        *processed = submission.client_seq;

        let seq = self.seq() + 1;
        let ctx = ApplyContext {
            origin: from,
            base_seq: submission.base_seq,
            seq: Some(seq),
        };
        let admitted = match D::lane(&submission.op) {
            Lane::Sequenced => self.materializer.admit(&self.document, &submission.op),
            Lane::Convergent => Ok(()),
        };
        match admitted.and_then(|()| self.document.apply(&submission.op, &ctx)) {
            Ok(effect) => {
                self.materialize(effect, &ctx);
                let committed = Committed {
                    seq,
                    origin: from,
                    client_seq: submission.client_seq,
                    base_seq: submission.base_seq,
                    op: submission.op,
                };
                self.log.push(committed.clone());
                self.derive(&committed.op);
                SubmitOutcome::Committed(committed)
            }
            Err(reason) => SubmitOutcome::Rejected(Rejected {
                client_seq: submission.client_seq,
                reason,
            }),
        }
    }
}
