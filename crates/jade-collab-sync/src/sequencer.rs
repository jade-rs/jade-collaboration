use std::collections::BTreeMap;

use jade_collab_core::id::ReplicaId;

use crate::protocol::{Committed, Downstream, Rejected, Resume, Submission, Welcome};
use crate::{ApplyContext, Document, Materializer, NoMaterializer, Rejection, SyncError};

/// The server core for one document: a single totally ordered log.
///
/// Every submission is applied to the authoritative model in arrival order.
/// A convergent operation applies wherever it lands; a sequenced one is
/// validated against everything committed before it and rejected if it no
/// longer fits. Committed effects go to the [`Materializer`] in log order.
pub struct Sequencer<D: Document, M = NoMaterializer> {
    document: D,
    materializer: M,
    log: Vec<Committed<D::Op>>,
    /// The newest client sequence number handled per replica.
    processed: BTreeMap<ReplicaId, u64>,
    next_replica: u64,
    faults: Vec<String>,
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
            log: Vec::new(),
            processed: BTreeMap::new(),
            next_replica: ReplicaId::ORIGIN.0 + 1,
            faults: Vec::new(),
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
        self.log.len() as u64
    }

    pub fn log(&self) -> &[Committed<D::Op>] {
        &self.log
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
        Ok(Resume {
            processed_client_seq,
            missed: self.log[seq as usize..].to_vec(),
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
        match self.document.apply(&submission.op, &ctx) {
            Ok(effect) => {
                if let Err(fault) = self.materializer.materialize(&self.document, effect, &ctx) {
                    self.faults.push(format!("seq {seq}: {fault}"));
                }
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
