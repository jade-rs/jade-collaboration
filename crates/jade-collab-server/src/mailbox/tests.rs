use super::*;

#[tokio::test(flavor = "current_thread")]
async fn byte_leases_cover_processing_and_cancelled_reservations_release_every_resource() {
    let (sender, mut inbox) = channel(2, 10);
    sender.reserve(6).await.unwrap().send(1).unwrap();
    let active = inbox.recv().await.unwrap();
    assert_eq!(sender.bytes_in_use(), 6, "processing still owns the bytes");
    let waiting = sender.reserve(5);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(&mut waiting).is_pending());
    tokio::task::yield_now().await;
    assert_eq!(
        sender.bytes_in_use(),
        10,
        "Tokio fairly reserves the remaining four bytes for this waiter"
    );
    drop(active);
    let reserved = waiting.await.unwrap();
    assert_eq!(sender.bytes_in_use(), 5);
    drop(reserved);
    assert_eq!(sender.bytes_in_use(), 0);
    let exact = sender.reserve(10).await.unwrap();
    drop(exact);
    assert!(matches!(
        sender.reserve(11).await,
        Err(Error::TooLarge { limit: 10 })
    ));
}

#[tokio::test]
async fn cancellation_does_not_enqueue_and_count_capacity_is_reusable() {
    let (sender, mut inbox) = channel(1, 16);
    sender.send(1).await.unwrap();
    {
        let waiting = sender.reserve(3);
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        assert_eq!(sender.bytes_in_use(), 4);
    }
    assert_eq!(sender.bytes_in_use(), 1);
    let entry = inbox.recv().await.unwrap();
    assert_eq!(entry.value, 1);
    drop(entry);
    sender.send(2).await.unwrap();
    assert_eq!(inbox.recv().await.unwrap().value, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn stopping_drains_committed_fifo_but_does_not_wait_for_uncommitted_permits() {
    let (sender, mut inbox) = channel(3, 16);
    sender.send(1).await.unwrap();
    sender.send(2).await.unwrap();
    let uncommitted = sender.reserve(4).await.unwrap();
    sender.close();
    assert_eq!(inbox.recv().await.unwrap().value, 1);
    assert_eq!(inbox.recv().await.unwrap().value, 2);
    assert!(
        inbox.recv().await.is_none(),
        "an outstanding permit cannot deadlock actor join on this executor"
    );
    assert_eq!(uncommitted.send(3), Err(Error::Closed));
    assert_eq!(sender.bytes_in_use(), 0);
}

#[tokio::test]
async fn receiver_failure_and_stop_wake_both_byte_and_count_waiters() {
    for stop in [false, true] {
        let (sender, inbox) = channel::<u64>(1, 8);
        sender.reserve(7).await.unwrap().send(1).unwrap();
        let first = sender.reserve(2);
        tokio::pin!(first);
        assert!(futures_util::poll!(&mut first).is_pending());
        if stop {
            sender.close();
        } else {
            drop(inbox);
        }
        assert!(matches!(first.await, Err(Error::Closed)));
        let (sender, inbox) = channel::<u64>(1, 8);
        sender.send(1).await.unwrap();
        let second = sender.reserve(2);
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        if stop {
            sender.close();
        } else {
            drop(inbox);
        }
        assert!(matches!(second.await, Err(Error::Closed)));
    }
}

// Independent state machine: reservations, queued values, processing, release,
// cancellation, and stop interleave. Pending attempts are dropped immediately.
proptest::proptest! {
    #[test]
    fn stateful_weighted_fifo_matches_resource_model(
        capacity in 1usize..6, budget in 1usize..32,
        actions in proptest::collection::vec((0u8..7, 0usize..40), 1..220),
    ) {
        use futures::FutureExt;
        use std::collections::VecDeque;
        let (sender, mut inbox) = channel(capacity,budget);
        let mut reserved = Vec::<(usize,Permit<usize>)>::new();
        let mut queued = VecDeque::<(usize,usize)>::new();
        let mut active = Vec::<Envelope<usize>>::new();
        let mut active_weights = Vec::<usize>::new();
        let mut closed=false;
        let mut serial=0;
        let stop_after=actions.len()/2;
        for (step,(action,arg)) in actions.into_iter().enumerate() {
            let used=reserved.iter().map(|x|x.0).sum::<usize>()+queued.iter().map(|x|x.1).sum::<usize>()+active_weights.iter().sum::<usize>();
            match action {
                0 | 1 => {
                    let weight=arg.max(1);
                    let got=sender.reserve(arg).now_or_never();
                    if closed { proptest::prop_assert!(matches!(got,Some(Err(Error::Closed)))); }
                    else if weight>budget {proptest::prop_assert!(matches!(got,Some(Err(Error::TooLarge { .. }))),"oversize must fail");}
                    else if weight<=budget-used && reserved.len()+queued.len()<capacity {
                        let Some(Ok(permit))=got else {proptest::prop_assert!(false,"an admissible reservation waited");unreachable!()};
                        reserved.push((weight,permit));
                    } else {proptest::prop_assert!(got.is_none(),"capacity was exceeded");}
                }
                2 if !reserved.is_empty() => {
                    let (weight,permit)=reserved.swap_remove(arg%reserved.len());
                    let result=permit.send(serial);
                    if closed {proptest::prop_assert_eq!(result,Err(Error::Closed));}
                    else {proptest::prop_assert_eq!(result,Ok(()));queued.push_back((serial,weight));serial+=1;}
                }
                3 if !reserved.is_empty() => {drop(reserved.swap_remove(arg%reserved.len()));}
                4 => {
                    let got=inbox.recv().now_or_never();
                    if let Some((value,weight))=queued.pop_front() {
                        let Some(Some(envelope))=got else {proptest::prop_assert!(false,"committed input was lost");unreachable!()};
                        proptest::prop_assert_eq!(envelope.value,value);
                        active.push(envelope);active_weights.push(weight);
                    } else if closed {proptest::prop_assert!(matches!(got,Some(None)));}
                    else {proptest::prop_assert!(got.is_none());}
                }
                5 if !active.is_empty() => {let at=arg%active.len();active.swap_remove(at);active_weights.swap_remove(at);}
                6 if step>=stop_after => {sender.close();closed=true;}
                _ => {}
            }
            let used=reserved.iter().map(|x|x.0).sum::<usize>()+queued.iter().map(|x|x.1).sum::<usize>()+active_weights.iter().sum::<usize>();
            proptest::prop_assert_eq!(sender.bytes_in_use(),used);
            proptest::prop_assert!(used<=budget);
            proptest::prop_assert!(reserved.len()+queued.len()<=capacity);
        }
        sender.close();drop(reserved);drop(active);
        let mut drained=Vec::new();
        while let Some(entry)=inbox.recv().now_or_never().flatten() {drained.push(entry.value);}
        proptest::prop_assert_eq!(drained,queued.into_iter().map(|x|x.0).collect::<Vec<_>>());
        proptest::prop_assert_eq!(sender.bytes_in_use(),0);
    }
}

#[test]
fn closure_and_publication_races_drain_exactly_the_committed_envelopes() {
    for value in 0..200 {
        let (sender, mut inbox) = channel(1, 8);
        let permit = futures::executor::block_on(sender.reserve(8)).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let gate = barrier.clone();
        let publisher = std::thread::spawn(move || {
            gate.wait();
            permit.send(value)
        });
        barrier.wait();
        sender.close();
        let published = publisher.join().unwrap();
        let drained = inbox.blocking_recv().map(|entry| entry.value);
        assert_eq!(drained, published.is_ok().then_some(value));
        assert!(inbox.blocking_recv().is_none());
        assert_eq!(sender.bytes_in_use(), 0);
    }
}
