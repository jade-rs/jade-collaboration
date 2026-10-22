use super::*;
use proptest::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct Counter(u64);
impl Document for Counter {
    type Op = bool;
    type Effect = ();
    fn lane(_: &bool) -> Lane {
        Lane::Convergent
    }
    fn apply(&mut self, op: &bool, _: &ApplyContext) -> Result<(), Rejection> {
        if !op {
            return Err(Rejection::new("invalid"));
        }
        self.0 += 1;
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

fn same_replica(a: &Replica<Counter>, b: &Replica<Counter>) {
    assert_eq!(a.id(), b.id());
    assert_eq!(a.seq(), b.seq());
    assert_eq!(a.view(), b.view());
    assert_eq!(a.confirmed(), b.confirmed());
    assert_eq!(a.next_client_seq(), b.next_client_seq());
    assert_eq!(a.pending_submissions(), b.pending_submissions());
    assert_eq!(a.clock.now(), b.clock.now());
}

proptest! {
    #[test]
    fn individual_pending_lookup_matches_fifo_after_replies_and_near_ticket_exhaustion(
        history in proptest::collection::vec((0u8..3, any::<u64>()), 1..100),
        near_limit in any::<bool>(),
    ) {
        let mut replica = Replica::new(ReplicaId(1), 0, Counter(0));
        if near_limit { replica.next_client_seq = u64::MAX - 12; }
        for (kind, choice) in history {
            if kind == 0 { let _ = replica.submit(true); }
            else if kind == 1 {
                if let Some(client_seq) = replica.first_pending_client_seq() {
                    replica.receive(Downstream::Rejected(crate::protocol::Rejected {client_seq, reason: Rejection::new("refused")})).unwrap();
                }
            }
            let all = replica.pending_submissions();
            prop_assert_eq!(replica.first_pending_client_seq(), all.first().map(|s| s.client_seq));
            for submission in &all {
                let found = replica.pending_submission(submission.client_seq);
                prop_assert_eq!(found.as_ref(), Some(submission));
            }
            for ticket in [0, choice, replica.next_client_seq(), u64::MAX] {
                let found = replica.pending_submission(ticket);
                prop_assert_eq!(found.as_ref(), all.iter().find(|s| s.client_seq == ticket));
            }
        }
    }
}

#[test]
fn impossible_resume_watermarks_refuse_without_dropping_pending_edits() {
    for processed in [2, u64::MAX] {
        let mut replica = Replica::new(ReplicaId(1), 0, Counter(0));
        replica.submit(true).unwrap();
        let before = replica.clone();
        assert!(replica
            .resume(Resume {
                processed_client_seq: processed,
                missed: vec![]
            })
            .is_err());
        same_replica(&replica, &before);
    }
}

#[test]
fn invalid_catch_up_after_a_valid_prefix_leaves_the_whole_replica_unchanged() {
    let mut replica = Replica::new(ReplicaId(1), 0, Counter(0));
    replica.submit(true).unwrap();
    let before = replica.clone();
    let missed = vec![
        Committed {
            seq: 1,
            origin: ReplicaId(2),
            client_seq: 1,
            base_seq: 0,
            op: true,
        },
        Committed {
            seq: 3,
            origin: ReplicaId(2),
            client_seq: 2,
            base_seq: 0,
            op: true,
        },
    ];
    assert!(replica
        .resume(Resume {
            processed_client_seq: 0,
            missed
        })
        .is_err());
    same_replica(&replica, &before);
}

#[test]
fn resume_cannot_forget_a_previously_answered_submission_or_contradict_a_commit() {
    let mut replica = Replica::new(ReplicaId(1), 0, Counter(0));
    replica.submit(true).unwrap();
    let before = replica.clone();
    assert!(replica
        .resume(Resume {
            processed_client_seq: 0,
            missed: vec![Committed {
                seq: 1,
                origin: replica.id(),
                client_seq: 1,
                base_seq: 0,
                op: true,
            }]
        })
        .is_err());
    same_replica(&replica, &before);
    replica
        .receive(Downstream::Rejected(crate::protocol::Rejected {
            client_seq: 1,
            reason: Rejection::new("refused"),
        }))
        .unwrap();
    let before = replica.clone();
    assert!(replica
        .resume(Resume {
            processed_client_seq: 0,
            missed: vec![]
        })
        .is_err());
    same_replica(&replica, &before);
}

#[test]
fn catch_up_at_the_last_log_position_refuses_without_wrapping() {
    let mut replica = Replica::new(ReplicaId(1), u64::MAX, Counter(0));
    let before = replica.clone();
    assert!(replica
        .resume(Resume {
            processed_client_seq: 0,
            missed: vec![Committed {
                seq: 0,
                origin: ReplicaId(2),
                client_seq: 1,
                base_seq: 0,
                op: true,
            }]
        })
        .is_err());
    same_replica(&replica, &before);
}

proptest! {
    #[test]
    fn failed_catch_up_and_retry_preserve_the_reference_pending_history(
        steps in prop::collection::vec((0u8..7,any::<bool>()),1..100),
    ) {
        let mut replica = Replica::new(ReplicaId(1),0,Counter(0));
        let mut log = Vec::new();
        let mut processed = 0;
        let mut pending = Vec::new();
        let mut confirmed = 0;
        let mut observed_seq = 0;
        let mut remote_ticket = 0;
        for (kind,accepted) in steps.into_iter().chain([(3,true)]) {
            match kind {
                0 => {pending.push(replica.submit(true).unwrap().client_seq);}
                1 => {
                    if let Some(&ticket) = pending.iter().find(|&&ticket| ticket>processed) {
                        processed = ticket;
                        if accepted {log.push(Committed {seq:log.len() as u64+1,origin:ReplicaId(1),client_seq:ticket,base_seq:0,op:true});}
                    }
                }
                2 => {
                    remote_ticket += 1;
                    log.push(Committed {seq:log.len() as u64+1,origin:ReplicaId(2),client_seq:remote_ticket,base_seq:0,op:true});
                }
                3..=5 => {
                    let before = replica.clone();
                    let mut response = Resume {processed_client_seq:processed,missed:log.iter().filter(|c|c.seq>observed_seq).cloned().collect()};
                    if kind==4 {response.processed_client_seq=if accepted {u64::MAX} else {replica.next_client_seq()};}
                    if kind==5 {response.missed.push(Committed {seq:log.len() as u64+2,origin:ReplicaId(2),client_seq:remote_ticket+1,base_seq:0,op:true});}
                    let answer = replica.resume(response);
                    if kind==3 {
                        answer.unwrap();
                        pending.retain(|&ticket|ticket>processed);
                        confirmed = log.len() as u64;
                        observed_seq = confirmed;
                    } else {
                        prop_assert!(answer.is_err());
                        same_replica(&replica,&before);
                    }
                }
                _ => {replica.reserve(if accepted {3} else {1});}
            }
            prop_assert_eq!(replica.seq(),observed_seq);
            prop_assert_eq!(replica.confirmed().0,confirmed);
            prop_assert_eq!(replica.view().0,confirmed+pending.len() as u64);
            prop_assert_eq!(replica.pending_submissions().iter().map(|s|s.client_seq).collect::<Vec<_>>(),pending.clone());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnnotatedText {
    visible: bool,
    annotation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AnnotationOp {
    Delete,
    Annotate,
}

impl Document for AnnotatedText {
    type Op = AnnotationOp;
    type Effect = ();
    fn lane(op: &AnnotationOp) -> Lane {
        match op {
            AnnotationOp::Delete => Lane::Convergent,
            AnnotationOp::Annotate => Lane::Sequenced,
        }
    }
    fn apply(&mut self, op: &AnnotationOp, _: &ApplyContext) -> Result<(), Rejection> {
        match op {
            AnnotationOp::Delete => self.visible = false,
            AnnotationOp::Annotate if self.visible => self.annotation = true,
            AnnotationOp::Annotate => return Err(Rejection::new("the range was deleted")),
        }
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

#[test]
fn remote_deletion_replays_pending_sequenced_annotation() {
    let mut replica = Replica::new(
        ReplicaId(1),
        0,
        AnnotatedText {
            visible: true,
            annotation: false,
        },
    );
    let submission = replica.submit(AnnotationOp::Annotate).unwrap();
    replica
        .receive(Downstream::Committed(Committed {
            seq: 1,
            origin: ReplicaId(2),
            client_seq: 1,
            base_seq: 0,
            op: AnnotationOp::Delete,
        }))
        .unwrap();
    assert_eq!(replica.view(), &replica.rebuilt_view());
    assert_eq!(
        replica.view(),
        &AnnotatedText {
            visible: false,
            annotation: false
        }
    );
    assert_eq!(
        replica.pending_len(),
        1,
        "the server still decides the pending operation"
    );
    replica
        .receive(Downstream::Rejected(crate::protocol::Rejected {
            client_seq: submission.client_seq,
            reason: Rejection::new("range deleted"),
        }))
        .unwrap();
    assert_eq!(replica.pending_len(), 0);
    assert_eq!(replica.view(), &replica.rebuilt_view());
}

proptest! {
    #[test]
    fn sequenced_guards_replay_through_concurrent_convergent_changes(
        steps in prop::collection::vec((0usize..3,0u8..6),1..60),
    ) {
        let mut sim = crate::sim::Simulation::new(AnnotatedText {visible:true,annotation:false},3);
        for (client,kind) in steps {
            match kind {
                0 => {let _ = sim.local(client,AnnotationOp::Annotate);}
                1 => {let _ = sim.local(client,AnnotationOp::Delete);}
                2 => {sim.deliver_up(client);}
                3 => {sim.deliver_down(client).unwrap();}
                4 => {sim.reconnect().unwrap();}
                _ => {let checkpoint=sim.checkpoint();sim.restart(crate::Sequencer::restore(checkpoint,crate::NoMaterializer)).unwrap();}
            }
            for index in 0..3 {prop_assert_eq!(sim.client(index).view(),&sim.client(index).rebuilt_view());}
        }
        sim.quiesce().unwrap();
        for index in 0..3 {prop_assert_eq!(sim.client(index).view(),sim.server().document());}
    }

    #[test]
    fn prefix_waits_match_all_pending_tickets_through_ack_reject_and_remote_histories(
        steps in prop::collection::vec((0u8..5,any::<u64>()),1..100),
        near_limit in any::<bool>(),
    ) {
        let mut replica = Replica::new(ReplicaId(1),0,Counter(0));
        if near_limit {replica.next_client_seq=u64::MAX-12;}
        let mut pending = VecDeque::new();
        let mut seq = 0;
        for (kind,bits) in steps {
            match kind {
                0|1 => {if let Ok(submission) = replica.submit(kind==0) {pending.push_back(submission.client_seq);}}
                2 => {
                    if let Some(ticket) = pending.pop_front() {
                        seq+=1;
                        replica.receive(Downstream::Committed(Committed {seq,origin:ReplicaId(1),client_seq:ticket,base_seq:0,op:true})).unwrap();
                    }
                }
                3 => {if let Some(ticket) = pending.pop_front() {replica.receive(Downstream::Rejected(crate::protocol::Rejected {client_seq:ticket,reason:Rejection::new("refused")})).unwrap();}}
                _ => {seq+=1; replica.receive(Downstream::Committed(Committed {seq,origin:ReplicaId(2),client_seq:0,base_seq:0,op:true})).unwrap();}
            }
            let boundary = match bits%4 {0=>0,1=>replica.next_client_seq().saturating_sub(1),2=>replica.next_client_seq(),_=>u64::MAX};
            prop_assert_eq!(replica.pending_through(boundary),pending.iter().any(|&ticket|ticket<=boundary));
            prop_assert_eq!(replica.pending_len(),pending.len());
        }
    }

    #[test]
    fn client_sequence_exhaustion_is_atomic_and_never_reuses_a_ticket(
        remaining in 0u64..8, steps in prop::collection::vec(any::<bool>(), 1..30),
    ) {
        let mut replica = Replica::new(ReplicaId(1), 0, Counter(0));
        replica.next_client_seq = u64::MAX - remaining;
        let mut reference = u128::from(replica.next_client_seq);
        let mut accepted = 0;
        for valid in steps {
            let result = replica.submit(valid);
            if valid && reference < u128::from(u64::MAX) {
                prop_assert_eq!(result.unwrap().client_seq, reference as u64);
                reference += 1; accepted += 1;
            } else { prop_assert!(result.is_err()); }
            prop_assert_eq!(u128::from(replica.next_client_seq()), reference);
            prop_assert_eq!(replica.view().0, accepted);
            prop_assert_eq!(replica.pending_len(), accepted as usize);
        }
    }
}
