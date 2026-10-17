mod common;

use common::{origin, ToyDoc, ToyOp};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_sync::{Replica, ReplicaEvent, Sequencer};

fn pair() -> (Sequencer<ToyDoc>, Replica<ToyDoc>, Replica<ToyDoc>) {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = Replica::from_welcome(server.join());
    let b = Replica::from_welcome(server.join());
    (server, a, b)
}

fn merge(top: u64, bottom: u64) -> ToyOp {
    ToyOp::Grid(GridOp::Merge(Merge {
        top: origin(top),
        left: origin(20),
        bottom: origin(bottom),
        right: origin(21),
    }))
}

#[test]
fn local_ops_show_immediately_and_stay_pending_until_acknowledged() {
    let (mut server, mut a, _) = pair();
    let id = a.next_id();
    let submission = a
        .submit(ToyOp::Insert {
            id,
            parent: Some(origin(2)),
            ch: 'c',
        })
        .unwrap();
    assert_eq!(a.view().string(), "abc");
    assert_eq!(a.confirmed().string(), "ab");
    assert_eq!(a.pending_len(), 1);
    assert_eq!(submission.client_seq, 1);
    assert_eq!(submission.base_seq, 0);

    let ack = server.submit(a.id(), submission).downstream().unwrap();
    let event = a.receive(ack).unwrap();
    assert_eq!(event, ReplicaEvent::Acknowledged { client_seq: 1 });
    assert_eq!(a.pending_len(), 0);
    assert_eq!(a.confirmed().string(), "abc");
    assert_eq!(a.seq(), 1);
}

#[test]
fn locally_invalid_ops_are_refused_before_sending() {
    let (_, mut a, _) = pair();
    let err = a.submit(merge(10, 10)).unwrap_err();
    assert!(!err.message().is_empty());
    assert_eq!(a.pending_len(), 0);
    assert_eq!(a.next_client_seq(), 1, "a refused op does not use up a sequence number");
}

#[test]
fn remote_convergent_ops_apply_on_top_of_pending_ones() {
    let (mut server, mut a, mut b) = pair();
    let ida = a.next_id();
    let sa = a
        .submit(ToyOp::Insert {
            id: ida,
            parent: Some(origin(2)),
            ch: 'A',
        })
        .unwrap();
    let idb = b.next_id();
    let sb = b
        .submit(ToyOp::Insert {
            id: idb,
            parent: Some(origin(2)),
            ch: 'B',
        })
        .unwrap();
    // b's insert reaches the server first.
    let commit_b = server.submit(b.id(), sb).downstream().unwrap();
    let commit_a = server.submit(a.id(), sa).downstream().unwrap();

    let event = a.receive(commit_b.clone()).unwrap();
    assert!(matches!(event, ReplicaEvent::Applied { .. }));
    assert_eq!(a.pending_len(), 1);
    a.receive(commit_a.clone()).unwrap();
    b.receive(commit_b).unwrap();
    b.receive(commit_a).unwrap();
    assert_eq!(a.view(), b.view());
    assert_eq!(a.view(), server.document());
}

#[test]
fn a_rejected_op_is_rolled_back_and_reported() {
    let (mut server, mut a, mut b) = pair();
    let sa = a.submit(merge(10, 11)).unwrap();
    let sb = b.submit(merge(10, 11)).unwrap();
    assert_eq!(b.view().grid.merges().len(), 1, "optimistically merged");

    let commit = server.submit(a.id(), sa).downstream().unwrap();
    let rejection = server.submit(b.id(), sb).downstream().unwrap();

    b.receive(commit.clone()).unwrap();
    let event = b.receive(rejection).unwrap();
    let ReplicaEvent::Rejected { client_seq, op, .. } = event else {
        panic!("expected a rejection, got {event:?}");
    };
    assert_eq!(client_seq, 1);
    assert_eq!(op, merge(10, 11));
    assert_eq!(b.pending_len(), 0);
    a.receive(commit).unwrap();
    assert_eq!(b.view(), server.document());
    assert_eq!(a.view(), server.document());
}

#[test]
fn a_remote_structural_op_rebases_the_pending_queue() {
    let (mut server, mut a, mut b) = pair();
    // a merges the first row; b, concurrently, merges the whole table.
    // A one-row merge across two columns covers two cells.
    let sa = a.submit(merge(10, 10)).unwrap();
    let sb = b.submit(merge(10, 11)).unwrap();
    let commit_b = server.submit(b.id(), sb).downstream().unwrap();
    // a learns about b's merge while its own is still pending: its view now
    // shows b's merge, and its own overlapping merge drops out of the view.
    a.receive(commit_b.clone()).unwrap();
    assert_eq!(a.pending_len(), 1);
    assert_eq!(a.view().grid.merges(), server.document().grid.merges());

    let rejection = server.submit(a.id(), sa).downstream().unwrap();
    assert!(matches!(
        a.receive(rejection).unwrap(),
        ReplicaEvent::Rejected { .. }
    ));
    b.receive(commit_b).unwrap();
    assert_eq!(a.view(), server.document());
    assert_eq!(b.view(), server.document());
}

#[test]
fn ids_minted_after_receiving_are_newer_than_everything_seen() {
    let (mut server, mut a, mut b) = pair();
    for _ in 0..5 {
        let id = b.next_id();
        let s = b
            .submit(ToyOp::Insert {
                id,
                parent: None,
                ch: 'x',
            })
            .unwrap();
        let c = server.submit(b.id(), s).downstream().unwrap();
        a.receive(c.clone()).unwrap();
        b.receive(c).unwrap();
    }
    let id = a.next_id();
    assert!(id.lamport > server.document().text.max_lamport());
}

#[test]
fn resuming_drops_pending_ops_the_server_already_handled() {
    let (mut server, mut a, _) = pair();
    let s1 = a.submit(merge(10, 11)).unwrap();
    // A second identical merge already overlaps in the optimistic view.
    assert!(a.submit(merge(10, 11)).is_err());
    let id = a.next_id();
    let s3 = a
        .submit(ToyOp::Insert {
            id,
            parent: None,
            ch: 'q',
        })
        .unwrap();
    // The server handles both, but the connection drops before any reply.
    server.submit(a.id(), s1);
    server.submit(a.id(), s3);
    let resume = server.resume(a.id(), a.seq()).unwrap();
    let events = a.resume(resume).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(a.pending_len(), 0);
    assert_eq!(a.view(), server.document());
    assert!(a.pending_submissions().is_empty());
}
