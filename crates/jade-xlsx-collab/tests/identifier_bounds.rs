use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_xlsx_collab::{XlsxModel, XlsxOp};

#[test]
fn an_overflowing_sheet_id_range_is_rejected_atomically() {
    let mut model = XlsxModel::blank(&[("Sheet1", 2, 2)]);
    let before = model.clone();
    let op = XlsxOp::AddSheet {
        id: Id::new(u64::MAX, ReplicaId(1)),
        name: "New".into(),
        after: None,
        rows: 1,
        columns: 1,
    };
    assert!(model
        .apply(
            &op,
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None
            }
        )
        .is_err());
    assert_eq!(model, before);
}

#[test]
fn checked_sheet_creation_refuses_an_exhausted_allocator_without_advancing_it() {
    use jade_collab_core::id::{Clock, IdError};
    use jade_xlsx_collab::intent::{try_resolve, XlsxIntent, XlsxIntentError};
    let model = XlsxModel::blank(&[("Sheet1", 2, 2)]);
    let mut clock = Clock::new(ReplicaId(1));
    clock.observe_lamport(u64::MAX - 2);
    let error = try_resolve(
        &model,
        &XlsxIntent::AddSheet {
            name: "New".into(),
            after: None,
        },
        &mut |n| clock.try_reserve(n),
    )
    .unwrap_err();
    assert_eq!(error, XlsxIntentError::Identifiers(IdError::Exhausted));
    assert_eq!(clock.now(), u64::MAX - 2);
}
