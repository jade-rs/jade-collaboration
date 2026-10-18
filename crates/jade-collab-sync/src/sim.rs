//! A deterministic in-memory network: one sequencer, N replicas, and FIFO
//! queues in both directions whose delivery the caller schedules. Property
//! tests drive it with random schedules to explore message interleavings,
//! checkpoints, reconnects and server restarts.

use std::collections::VecDeque;

use crate::protocol::{Committed, Downstream, Submission};
use crate::{
    Checkpoint, Document, Lane, Materializer, NoMaterializer, Record, Rejection, Replica,
    Sequencer, SubmitOutcome, SyncError,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    committed: [u64; 2],
    rejected: [u64; 2],
}

fn slot(lane: Lane) -> usize {
    match lane {
        Lane::Convergent => 0,
        Lane::Sequenced => 1,
    }
}

impl Stats {
    pub fn committed(&self, lane: Lane) -> u64 {
        self.committed[slot(lane)]
    }

    pub fn rejected(&self, lane: Lane) -> u64 {
        self.rejected[slot(lane)]
    }
}

pub struct Simulation<D: Document, M = NoMaterializer> {
    server: Sequencer<D, M>,
    clients: Vec<Replica<D>>,
    up: Vec<VecDeque<Submission<D::Op>>>,
    down: Vec<VecDeque<Downstream<D::Op>>>,
    /// What a durable server would have journaled since the last checkpoint.
    journal: Vec<Record<D::Op>>,
    stats: Stats,
}

impl<D: Document> Simulation<D, NoMaterializer> {
    pub fn new(document: D, clients: usize) -> Self {
        Self::with_materializer(document, NoMaterializer, clients)
    }
}

impl<D: Document, M: Materializer<D>> Simulation<D, M> {
    pub fn with_materializer(document: D, materializer: M, clients: usize) -> Self {
        let server = Sequencer::with_materializer(document, materializer);
        let mut sim = Self {
            server,
            clients: Vec::new(),
            up: vec![VecDeque::new(); clients],
            down: vec![VecDeque::new(); clients],
            journal: Vec::new(),
            stats: Stats::default(),
        };
        sim.clients = (0..clients).map(|_| sim.join()).collect();
        sim
    }

    fn join(&mut self) -> Replica<D> {
        let welcome = self.server.join();
        self.journal.push(Record::Joined {
            replica: welcome.replica,
        });
        Replica::from_welcome(welcome)
    }

    pub fn server(&self) -> &Sequencer<D, M> {
        &self.server
    }

    pub fn client(&self, index: usize) -> &Replica<D> {
        &self.clients[index]
    }

    pub fn client_mut(&mut self, index: usize) -> &mut Replica<D> {
        &mut self.clients[index]
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The records since the last [`Simulation::checkpoint`] (or since the
    /// start), in the order they happened.
    pub fn journal(&self) -> &[Record<D::Op>] {
        &self.journal
    }

    /// Checkpoints and compacts the server, as a durable server does every
    /// so often; the journal starts over.
    pub fn checkpoint(&mut self) -> Checkpoint<D> {
        self.checkpoint_keeping(0).0
    }

    /// Like [`Simulation::checkpoint`], but the server keeps the last `keep`
    /// committed operations, which are returned with the checkpoint (a
    /// durable server stores them with it).
    pub fn checkpoint_keeping(&mut self, keep: usize) -> (Checkpoint<D>, Vec<Committed<D::Op>>) {
        let checkpoint = self.server.checkpoint();
        self.server.compact_keeping(keep);
        self.journal.clear();
        (checkpoint, self.server.log().to_vec())
    }

    /// Every connection drops and comes back: messages in flight are lost,
    /// each replica resumes and resends what is still pending. A replica
    /// that can no longer resume joins again as a new one.
    pub fn reconnect(&mut self) -> Result<(), SyncError> {
        for client in 0..self.clients.len() {
            self.up[client].clear();
            self.down[client].clear();
            let replica = &self.clients[client];
            match self.server.resume(replica.id(), replica.seq()) {
                Ok(resume) => {
                    let replica = &mut self.clients[client];
                    replica.resume(resume)?;
                    self.up[client].extend(replica.pending_submissions());
                }
                Err(SyncError::Compacted { .. } | SyncError::UnknownReplica(_)) => {
                    self.clients[client] = self.join();
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// The server goes down and `server` — restored from its checkpoint and
    /// journal — takes its place; every client reconnects.
    pub fn restart(&mut self, server: Sequencer<D, M>) -> Result<(), SyncError> {
        self.server = server;
        self.reconnect()
    }

    /// A local edit on `client`, queued for the server.
    pub fn local(&mut self, client: usize, op: D::Op) -> Result<(), Rejection> {
        let submission = self.clients[client].submit(op)?;
        self.up[client].push_back(submission);
        Ok(())
    }

    /// Delivers `client`'s oldest submission to the server.
    pub fn deliver_up(&mut self, client: usize) -> bool {
        let Some(submission) = self.up[client].pop_front() else {
            return false;
        };
        let lane = D::lane(&submission.op);
        let from = self.clients[client].id();
        let outcome = self.server.submit(from, submission);
        self.journal.extend(outcome.record(from));
        match &outcome {
            SubmitOutcome::Committed(_) => {
                self.stats.committed[slot(lane)] += 1;
                let message = outcome.downstream().expect("a commit is broadcast");
                for queue in &mut self.down {
                    queue.push_back(message.clone());
                }
            }
            SubmitOutcome::Rejected(_) => {
                self.stats.rejected[slot(lane)] += 1;
                self.down[client].push_back(outcome.downstream().expect("a rejection is answered"));
            }
            SubmitOutcome::Duplicate => {}
        }
        true
    }

    /// Delivers the oldest server message waiting for `client`.
    pub fn deliver_down(&mut self, client: usize) -> Result<bool, SyncError> {
        let Some(message) = self.down[client].pop_front() else {
            return Ok(false);
        };
        self.clients[client].receive(message)?;
        Ok(true)
    }

    pub fn deliver_all_down(&mut self) -> Result<(), SyncError> {
        for client in 0..self.clients.len() {
            while self.deliver_down(client)? {}
        }
        Ok(())
    }

    /// Delivers everything until no message is in flight.
    pub fn quiesce(&mut self) -> Result<(), SyncError> {
        loop {
            let mut progressed = false;
            for client in 0..self.clients.len() {
                while self.deliver_up(client) {
                    progressed = true;
                }
            }
            for client in 0..self.clients.len() {
                while self.deliver_down(client)? {
                    progressed = true;
                }
            }
            if !progressed {
                return Ok(());
            }
        }
    }
}
