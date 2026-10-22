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
    /// Stable authenticated principals, independent of credentials and roles.
    owners: BTreeMap<ReplicaId, String>,
    next_replica: u64,
    faults: Vec<String>,
    /// A live native error quarantines mutable model/file state. Never
    /// serialized: a failed instance cannot produce a checkpoint.
    failure: Option<SyncError>,
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
    /// Missing in legacy checkpoints: those replicas have anonymous ownership.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub owners: BTreeMap<ReplicaId, String>,
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
    /// Journal atomically with Joined, before sending the welcome. Never holds
    /// a bearer token; ownership cannot be reassigned by later submissions.
    Owned {
        replica: ReplicaId,
        principal: String,
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
    /// The native file/model may be partially written. Close transports and
    /// recover; this is neither an acknowledgement nor a durable refusal.
    Failed(SyncError),
}

impl<Op: Clone> SubmitOutcome<Op> {
    pub fn downstream(&self) -> Option<Downstream<Op>> {
        match self {
            SubmitOutcome::Committed(c) => Some(Downstream::Committed(c.clone())),
            SubmitOutcome::Rejected(r) => Some(Downstream::Rejected(r.clone())),
            SubmitOutcome::Duplicate | SubmitOutcome::Failed(_) => None,
        }
    }

    /// What the sequencer must journal for this outcome of a submission
    /// `from` a replica; nothing for a duplicate or unknown native outcome.
    pub fn record(&self, from: ReplicaId) -> Option<Record<Op>> {
        match self {
            SubmitOutcome::Committed(c) => Some(Record::Committed(c.clone())),
            SubmitOutcome::Rejected(r) => Some(Record::Rejected {
                replica: from,
                client_seq: r.client_seq,
            }),
            SubmitOutcome::Duplicate | SubmitOutcome::Failed(_) => None,
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
            owners: BTreeMap::new(),
            next_replica: ReplicaId::ORIGIN.0 + 1,
            faults: Vec::new(),
            failure: None,
            derived: Vec::new(),
        }
    }

    /// The sequencer `checkpoint` was taken of, with `materializer` holding
    /// the real document at the same log position. Its log starts there.
    /// Legacy checkpoints containing native faults produce a fenced instance;
    /// checked startup should use `restore_with_tail` and handle its error.
    pub fn restore(checkpoint: Checkpoint<D>, materializer: M) -> Self {
        let failure = checkpoint
            .faults
            .last()
            .map(|reason| SyncError::Materialization {
                seq: checkpoint.seq,
                reason: format!(
                    "checkpoint contains native faults; repair before restoring: {reason}"
                ),
            });
        Self {
            document: checkpoint.document,
            materializer,
            log_start: checkpoint.seq,
            log: Vec::new(),
            processed: checkpoint.processed,
            owners: checkpoint.owners,
            next_replica: checkpoint.next_replica,
            faults: checkpoint.faults,
            failure,
            derived: Vec::new(),
        }
    }

    /// Checked capture; a partially materialized state can never become durable.
    pub fn try_checkpoint(&self) -> Result<Checkpoint<D>, SyncError> {
        self.check_health()?;
        Ok(Checkpoint {
            seq: self.seq(),
            processed: self.processed.clone(),
            owners: self.owners.clone(),
            next_replica: self.next_replica,
            faults: self.faults.clone(),
            document: self.document.clone(),
        })
    }

    /// Captures healthy state. Use `try_checkpoint` when handling native errors.
    /// Panics if this instance has been fenced after a materialization failure.
    pub fn checkpoint(&self) -> Checkpoint<D> {
        self.try_checkpoint()
            .expect("cannot checkpoint a failed sequencer")
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
        Self::validate_metadata(
            &checkpoint.processed,
            &checkpoint.owners,
            checkpoint.next_replica,
        )?;
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
        let mut previous = BTreeMap::new();
        for (index, committed) in tail.iter().enumerate() {
            // `start + tail.len() == checkpoint.seq`; enumerate only actual
            // entries, without forming MAX + 1 for an empty or completed tail.
            let expected = start + index as u64 + 1;
            if committed.seq != expected {
                return Err(journal(format!(
                    "the tail holds log position {} where {expected} belongs",
                    committed.seq
                )));
            }
            if committed.origin == ReplicaId::ORIGIN {
                if committed.client_seq != 0 {
                    return Err(journal("an origin commit has a client ticket".into()));
                }
            } else {
                let watermark = checkpoint
                    .processed
                    .get(&committed.origin)
                    .ok_or(SyncError::UnknownReplica(committed.origin))?;
                let prior = previous.entry(committed.origin).or_insert(0);
                if committed.client_seq <= *prior || committed.client_seq > *watermark {
                    return Err(journal(format!(
                        "tail ticket {} for {} does not fit its processed watermark {watermark}",
                        committed.client_seq, committed.origin
                    )));
                }
                *prior = committed.client_seq;
            }
        }
        let mut sequencer = Self::restore(checkpoint, materializer);
        sequencer.check_health()?;
        sequencer.log_start = start;
        sequencer.log = tail;
        Ok(sequencer)
    }

    fn validate_metadata(
        processed: &BTreeMap<ReplicaId, u64>,
        owners: &BTreeMap<ReplicaId, String>,
        next_replica: u64,
    ) -> Result<(), SyncError> {
        if next_replica == 0
            || processed.contains_key(&ReplicaId::ORIGIN)
            || processed
                .keys()
                .next_back()
                .is_some_and(|id| id.0 >= next_replica)
            || owners.keys().any(|id| !processed.contains_key(id))
        {
            return Err(SyncError::Journal {
                reason: "invalid replica allocation or ownership metadata".into(),
            });
        }
        Ok(())
    }

    /// Drops the log up to the current position, once a checkpoint of it is
    /// safe. Replicas that are further behind can no longer resume.
    pub fn compact(&mut self) {
        self.compact_keeping(0);
    }

    /// Like [`Sequencer::compact`], but keeps the last `keep` committed
    /// operations (or as many as there are), so that a replica at most
    /// `keep` operations behind can always still resume.
    /// Panics after native failure, because no healthy checkpoint can be taken.
    pub fn compact_keeping(&mut self, keep: usize) {
        self.check_health()
            .expect("cannot compact a failed sequencer");
        let dropped = self.log.len().saturating_sub(keep);
        self.log.drain(..dropped);
        self.log_start += dropped as u64;
    }

    /// Applies a journaled record, as the sequencer did when it was
    /// recorded. Committed operations are applied and materialized without
    /// being admitted again: they are facts.
    pub fn replay(&mut self, record: Record<D::Op>) -> Result<(), SyncError> {
        self.check_health()?;
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
                self.try_join()?;
            }
            Record::Owned { replica, principal } => {
                if !self.processed.contains_key(&replica) {
                    return Err(SyncError::UnknownReplica(replica));
                }
                if self.owners.contains_key(&replica) || self.processed_client_seq(replica) != 0 {
                    return Err(SyncError::Journal {
                        reason: format!("cannot rebind the owner of {replica}"),
                    });
                }
                self.owners.insert(replica, principal);
            }
            Record::Committed(committed) => {
                let derived = committed.origin == ReplicaId::ORIGIN;
                if !derived && !self.processed.contains_key(&committed.origin) {
                    return Err(SyncError::UnknownReplica(committed.origin));
                }
                let expected = self.seq().checked_add(1).ok_or(SyncError::LogExhausted)?;
                if committed.seq != expected {
                    return Err(SyncError::OutOfOrder {
                        expected,
                        received: committed.seq,
                    });
                }
                if derived {
                    if committed.client_seq != 0 {
                        return Err(SyncError::Journal {
                            reason: "an origin commit has a client ticket".into(),
                        });
                    }
                } else if committed.client_seq <= self.processed_client_seq(committed.origin) {
                    return Err(SyncError::Journal {
                        reason: format!(
                            "ticket {} for {} was already processed",
                            committed.client_seq, committed.origin
                        ),
                    });
                }
                let ctx = ApplyContext {
                    origin: committed.origin,
                    base_seq: committed.base_seq,
                    seq: Some(committed.seq),
                };
                let effect =
                    self.document
                        .apply_recorded(&committed.op, &ctx)
                        .map_err(|reason| SyncError::Diverged {
                            seq: committed.seq,
                            reason: reason.to_string(),
                        })?;
                self.materialize(effect, &ctx)?;
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
                    if client_seq <= *processed {
                        return Err(SyncError::Journal {
                            reason: format!(
                                "ticket {client_seq} for {replica} was already processed"
                            ),
                        });
                    }
                    *processed = client_seq;
                }
            }
        }
        Ok(())
    }

    /// Builds a staged derived suffix. Reserve all its positions before
    /// applying even the first child; an error discards the whole stage.
    fn derive(&mut self, cause: &D::Op) -> Result<(), Rejection> {
        let operations = self.materializer.derived(&self.document, cause)?;
        let count = u64::try_from(operations.len())
            .map_err(|_| Rejection::new("derived suffix is too large"))?;
        self.seq()
            .checked_add(count)
            .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
        for op in operations {
            let seq = self
                .seq()
                .checked_add(1)
                .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
            let ctx = ApplyContext {
                origin: ReplicaId::ORIGIN,
                base_seq: seq - 1,
                seq: Some(seq),
            };
            let effect = self.document.apply(&op, &ctx).map_err(|reason| {
                Rejection::new(format!("derived {op:?} does not apply: {reason}"))
            })?;
            self.materialize(effect, &ctx)?;
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
        Ok(())
    }

    /// Commits `op` on behalf of [`ReplicaId::ORIGIN`], outside any
    /// replica's submission — what the server itself knows and the model
    /// must hear in log order (who a replica is, say). Applied and
    /// materialized like a commit; the caller journals and broadcasts it.
    /// Nothing is admitted or derived: the server is the author.
    pub fn commit_origin(&mut self, op: D::Op) -> Result<Committed<D::Op>, Rejection> {
        self.check_health()?;
        let seq = self
            .seq()
            .checked_add(1)
            .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
        let ctx = ApplyContext {
            origin: ReplicaId::ORIGIN,
            base_seq: seq - 1,
            seq: Some(seq),
        };
        let effect = self.document.apply(&op, &ctx)?;
        self.materialize(effect, &ctx)?;
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

    fn materialize(&mut self, effect: D::Effect, ctx: &ApplyContext) -> Result<(), SyncError> {
        if let Err(reason) = self.materializer.materialize(&self.document, effect, ctx) {
            let seq = ctx.seq.expect("only committed operations are materialized");
            let failure = SyncError::Materialization { seq, reason };
            self.faults.push(failure.to_string());
            self.failure = Some(failure.clone());
            return Err(failure);
        }
        Ok(())
    }

    /// The first unexpected live native failure; it is sticky until this
    /// instance is discarded and rebuilt from a known healthy file/checkpoint.
    pub fn failure(&self) -> Option<&SyncError> {
        self.failure.as_ref()
    }

    /// Refuses access to a model/file with an unknown native write outcome.
    /// Recovery must load an independent healthy checkpoint/file and journal.
    pub fn check_health(&self) -> Result<(), SyncError> {
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// Reads the authoritative model only while its native file is healthy.
    pub fn try_document(&self) -> Result<&D, SyncError> {
        self.check_health()?;
        Ok(&self.document)
    }
    /// Healthy model access. Panics after native failure; checked callers use `try_document`.
    pub fn document(&self) -> &D {
        self.try_document().expect("cannot read a failed sequencer")
    }

    /// Reads the native file only while its materialization outcome is known.
    pub fn try_materializer(&self) -> Result<&M, SyncError> {
        self.check_health()?;
        Ok(&self.materializer)
    }
    /// Healthy native access. Panics after failure; use `try_materializer` to handle errors.
    pub fn materializer(&self) -> &M {
        self.try_materializer()
            .expect("cannot read a failed native file")
    }

    /// Access for healthy native housekeeping; cannot repair a failed instance.
    pub fn try_materializer_mut(&mut self) -> Result<&mut M, SyncError> {
        self.check_health()?;
        Ok(&mut self.materializer)
    }
    /// Healthy native access. Panics after failure; use the checked variant to handle errors.
    pub fn materializer_mut(&mut self) -> &mut M {
        self.try_materializer_mut()
            .expect("cannot mutate a failed native file")
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

    /// Native failure diagnostics. A live error fences the instance; these
    /// details do not make its partially written model/file safe to publish.
    pub fn faults(&self) -> &[String] {
        &self.faults
    }

    /// Admits a new replica. Panics at identifier exhaustion or invalid restored
    /// metadata, or after native failure; transports should use [`Sequencer::try_join`].
    pub fn join(&mut self) -> Welcome<D> {
        self.try_join().expect("cannot allocate a replica")
    }

    /// Admits a new replica without wrapping, reusing ids or changing state on
    /// refusal. MAX is the exhausted next-id sentinel, as for client tickets.
    pub fn try_join(&mut self) -> Result<Welcome<D>, SyncError> {
        self.check_health()?;
        Self::validate_metadata(&self.processed, &self.owners, self.next_replica)?;
        let next = self
            .next_replica
            .checked_add(1)
            .ok_or(SyncError::ReplicaIdExhausted)?;
        let replica = ReplicaId(self.next_replica);
        self.next_replica = next;
        self.processed.insert(replica, 0);
        Ok(Welcome {
            replica,
            seq: self.seq(),
            snapshot: self.document.clone(),
        })
    }

    /// Joins for a trusted transport principal. Equal names identify the same
    /// principal across credential/role changes. None is anonymous access.
    pub fn join_as(&mut self, principal: Option<String>) -> Welcome<D> {
        self.try_join_as(principal)
            .expect("cannot allocate a replica")
    }

    /// Checked authenticated admission. Failure never publishes an owner or
    /// consumes an identifier; already joined replicas can still resume.
    pub fn try_join_as(&mut self, principal: Option<String>) -> Result<Welcome<D>, SyncError> {
        let welcome = self.try_join()?;
        if let Some(principal) = principal {
            self.owners.insert(welcome.replica, principal);
        }
        Ok(welcome)
    }

    /// A trusted transport must authorize resumption before replacing sockets,
    /// presence or any ticket state. Legacy unbound replicas only match None.
    pub fn resume_as(
        &self,
        replica: ReplicaId,
        seq: u64,
        principal: Option<&str>,
    ) -> Result<Resume<D::Op>, SyncError> {
        if !self.processed.contains_key(&replica) {
            return Err(SyncError::UnknownReplica(replica));
        }
        if self.owners.get(&replica).map(String::as_str) != principal {
            return Err(SyncError::ReplicaOwnerMismatch { replica });
        }
        self.resume(replica, seq)
    }

    /// What `replica`, last at log position `seq`, missed. This trusted core
    /// lookup does not authorize a caller; authenticated transports use resume_as.
    pub fn resume(&self, replica: ReplicaId, seq: u64) -> Result<Resume<D::Op>, SyncError> {
        self.check_health()?;
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

    /// Refuses an issued submission at the access-control boundary without
    /// decoding, admitting or materializing its operation. Journal the outcome
    /// before replying, just as for submit; duplicate tickets stay duplicates.
    pub fn reject(
        &mut self,
        from: ReplicaId,
        client_seq: u64,
        reason: Rejection,
    ) -> SubmitOutcome<D::Op> {
        if let Err(error) = self.check_health() {
            return SubmitOutcome::Failed(error);
        }
        if let Some(outcome) = self.process_ticket(from, client_seq) {
            return outcome;
        }
        SubmitOutcome::Rejected(Rejected { client_seq, reason })
    }

    fn process_ticket(&mut self, from: ReplicaId, client_seq: u64) -> Option<SubmitOutcome<D::Op>> {
        let Some(processed) = self.processed.get_mut(&from) else {
            return Some(SubmitOutcome::Rejected(Rejected {
                client_seq,
                reason: Rejection::new(SyncError::UnknownReplica(from).to_string()),
            }));
        };
        if client_seq <= *processed {
            return Some(SubmitOutcome::Duplicate);
        }
        *processed = client_seq;
        None
    }

    /// Applies a new ticket. A native fast-path error returns `Failed` without
    /// advancing the log/processed watermark; discard the fenced instance.
    /// Errors isolated to an independent stage return a durable `Rejected`.
    pub fn submit(
        &mut self,
        from: ReplicaId,
        submission: Submission<D::Op>,
    ) -> SubmitOutcome<D::Op> {
        if let Err(error) = self.check_health() {
            return SubmitOutcome::Failed(error);
        }
        let previous = self.processed_client_seq(from);
        if let Some(outcome) = self.process_ticket(from, submission.client_seq) {
            return outcome;
        }

        let client_seq = submission.client_seq;
        if let Err(reason) = self.document.validate_submission(&submission.op, from) {
            return SubmitOutcome::Rejected(Rejected { client_seq, reason });
        }
        let result = if self.materializer.may_derive(&self.document, &submission.op) {
            self.submit_staged(from, submission)
        } else {
            self.apply_submission(from, submission)
        };
        match result {
            Ok(committed) => SubmitOutcome::Committed(committed),
            Err(reason) => match &self.failure {
                Some(error) => {
                    // No durable verdict was reached. Preserve the last
                    // healthy watermark so recovery can retry this ticket.
                    self.processed.insert(from, previous);
                    SubmitOutcome::Failed(error.clone())
                }
                None => SubmitOutcome::Rejected(Rejected { client_seq, reason }),
            },
        }
    }

    fn submit_staged(
        &mut self,
        from: ReplicaId,
        submission: Submission<D::Op>,
    ) -> Result<Committed<D::Op>, Rejection> {
        // Refuse a full log before exporting/copying the file.
        self.seq()
            .checked_add(1)
            .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
        let materializer = self
            .materializer
            .fork(&self.document)
            .map_err(|reason| Rejection::new(format!("cannot stage the document: {reason}")))?;
        let mut stage = Self::with_materializer(self.document.clone(), materializer);
        stage.log_start = self.seq();
        let committed = stage.apply_submission(from, submission)?;
        stage.derive(&committed.op)?;
        // Metadata and the old log stay owned by the live sequencer. Failed
        // plans advance only its processed ticket, which is journaled as a
        // refusal; successful plans publish every file/model change together.
        self.document = stage.document;
        self.materializer = stage.materializer;
        self.log.extend(stage.log);
        self.derived.extend(stage.derived);
        Ok(committed)
    }

    fn apply_submission(
        &mut self,
        from: ReplicaId,
        submission: Submission<D::Op>,
    ) -> Result<Committed<D::Op>, Rejection> {
        let seq = self
            .seq()
            .checked_add(1)
            .ok_or_else(|| Rejection::new(SyncError::LogExhausted.to_string()))?;
        let ctx = ApplyContext {
            origin: from,
            base_seq: submission.base_seq,
            seq: Some(seq),
        };
        if D::lane(&submission.op) == Lane::Sequenced {
            self.materializer.admit(&self.document, &submission.op)?;
        }
        let effect = self.document.apply(&submission.op, &ctx)?;
        self.materialize(effect, &ctx)?;
        let committed = Committed {
            seq,
            origin: from,
            client_seq: submission.client_seq,
            base_seq: submission.base_seq,
            op: submission.op,
        };
        self.log.push(committed.clone());
        Ok(committed)
    }
}
