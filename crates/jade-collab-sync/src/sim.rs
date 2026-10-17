//! A deterministic in-memory network: one sequencer, N replicas, and FIFO
//! queues in both directions whose delivery the caller schedules. Property
//! tests drive it with random schedules to explore message interleavings.

use std::collections::VecDeque;

use crate::protocol::{Downstream, Submission};
use crate::{
    Document, Lane, Materializer, NoMaterializer, Rejection, Replica, Sequencer, SubmitOutcome,
    SyncError,
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
    stats: Stats,
}

impl<D: Document> Simulation<D, NoMaterializer> {
    pub fn new(document: D, clients: usize) -> Self {
        Self::with_materializer(document, NoMaterializer, clients)
    }
}

impl<D: Document, M: Materializer<D>> Simulation<D, M> {
    pub fn with_materializer(document: D, materializer: M, clients: usize) -> Self {
        let mut server = Sequencer::with_materializer(document, materializer);
        let clients: Vec<Replica<D>> = (0..clients)
            .map(|_| Replica::from_welcome(server.join()))
            .collect();
        let n = clients.len();
        Self {
            server,
            clients,
            up: vec![VecDeque::new(); n],
            down: vec![VecDeque::new(); n],
            stats: Stats::default(),
        }
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

    pub fn stats(&self) -> &Stats {
        &self.stats
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
