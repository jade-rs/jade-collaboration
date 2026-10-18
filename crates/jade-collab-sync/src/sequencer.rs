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

    /// Drops the log up to the current position, once a checkpoint of it is
    /// safe. Replicas that are further behind can no longer resume.
    pub fn compact(&mut self) {
        self.log_start = self.seq();
        self.log.clear();
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
                if !self.processed.contains_key(&committed.origin) {
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
                self.processed
                    .insert(committed.origin, committed.client_seq);
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
                SubmitOutcome::Committed(committed)
            }
            Err(reason) => SubmitOutcome::Rejected(Rejected {
                client_seq: submission.client_seq,
                reason,
            }),
        }
    }
}
