//! Count full projections, independent of wall-clock speed and CI CPU load.
use super::*;
use std::cell::Cell;
thread_local! {
    static WALK_CALLS:Cell<usize>=const {Cell::new(0)};
    static REPAIRED:Cell<usize>=const {Cell::new(0)};
    static FORCE_WALK:Cell<bool>=const {Cell::new(false)};
}
pub(super) fn forced_walk() -> bool {
    FORCE_WALK.with(Cell::get)
}
pub(super) fn repairing(atoms: usize) {
    REPAIRED.with(|value| value.set(value.get() + atoms));
}
fn canonical<T>(action: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_WALK.with(|flag| flag.set(false));
        }
    }
    FORCE_WALK.with(|flag| flag.set(true));
    let _reset = Reset;
    action()
}
pub(super) fn walked() {
    WALK_CALLS.with(|calls| calls.set(calls.get() + 1));
}
fn walks() -> usize {
    WALK_CALLS.with(Cell::get)
}
fn reset() {
    WALK_CALLS.with(|calls| calls.set(0));
    REPAIRED.with(|count| count.set(0));
}
fn context() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    }
}
#[test]
fn ordinary_typing_reuses_positions_instead_of_rewalking_unrelated_paragraphs() {
    let mut model = DocxModel::from_paragraphs(&vec!["seed"; 5000]);
    let mut next = model.max_lamport() + 1;
    let mut parent = model.paragraphs()[2500].chars.last().copied();
    for step in 0..12 {
        let first = Id::new(next, ReplicaId(1));
        next += 1;
        reset();
        let effects = model
            .apply(
                &DocxOp::InsertText {
                    story: StoryId::Body,
                    parent,
                    first,
                    text: "🙂".into(),
                },
                &context(),
            )
            .unwrap();
        assert!(matches!(
            effects.as_slice(),
            [DocxEffect::InsertText {
                paragraph: ParagraphStory::Body { index: 2500 },
                ..
            }]
        ));
        assert!(
            walks() <= usize::from(step == 0),
            "one warm-up projection, then no full scans: step={step}, walks={}",
            walks()
        );
        assert!(
            REPAIRED.with(Cell::get) <= 5 + step,
            "repair work must stay in the edited paragraph"
        );
        parent = Some(first);
    }
}

#[path = "../../tests/common/mod.rs"]
mod common;
use proptest::prelude::*;

fn compare(model: &mut DocxModel, op: &DocxOp, ctx: &ApplyContext) {
    let mut reference = model.clone();
    let expected = canonical(|| reference.apply(op, ctx));
    let actual = model.apply(op, ctx);
    assert_eq!(actual, expected, "operation {op:?}");
    assert_eq!(model, &reference);
    assert_eq!(
        serde_json::to_value(&*model).unwrap(),
        serde_json::to_value(reference).unwrap()
    );
}
#[test]
fn anchored_stories_and_equal_byte_offsets_match_the_canonical_effects() {
    let mut model = DocxModel::from_paragraphs(&["AB", "C"]);
    let ctx = context();
    for op in [
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: Some(Id::new(1, ReplicaId::ORIGIN)),
            id: Id::new(100, ReplicaId(1)),
            text: "note".into(),
        },
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: Some(Id::new(1, ReplicaId::ORIGIN)),
            id: Id::new(200, ReplicaId(1)),
            text: "box".into(),
            width_px: 100,
            height_px: 60,
        },
    ] {
        compare(&mut model, &op, &ctx);
    }
    let note = StoryId::Footnote {
        id: Id::new(100, ReplicaId(1)),
    };
    for (i, (story, parent)) in [
        (StoryId::Body, Some(Id::new(1, ReplicaId::ORIGIN))),
        (StoryId::Body, Some(Id::new(200, ReplicaId(1)))),
        (StoryId::Body, Some(Id::new(100, ReplicaId(1)))),
        (StoryId::Body, Some(Id::new(3, ReplicaId::ORIGIN))),
        (note, None),
        (
            StoryId::TextBox {
                id: Id::new(200, ReplicaId(1)),
            },
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let op = DocxOp::InsertText {
            story,
            parent,
            first: Id::new(300 + i as u64 * 3, ReplicaId(1)),
            text: "é🙂".into(),
        };
        compare(&mut model, &op, &ctx);
    }
    compare(
        &mut model,
        &DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: Id::new(100, ReplicaId(1)),
        },
        &ctx,
    );
    compare(
        &mut model,
        &DocxOp::InsertText {
            story: note,
            parent: None,
            first: Id::new(400, ReplicaId(1)),
            text: "hidden".into(),
        },
        &ctx,
    );
    compare(
        &mut model,
        &DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: Id::new(500, ReplicaId(1)),
            text: "visible".into(),
        },
        &ctx,
    );
}
#[test]
fn runtime_positions_preserve_model_identity_serialization_and_send_sync() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<DocxModel>();
    let mut model = DocxModel::from_paragraphs(&["abc", "def"]);
    compare(
        &mut model,
        &DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: Id::new(100, ReplicaId(1)),
            text: "🙂".into(),
        },
        &context(),
    );
    let bytes = serde_json::to_value(&model).unwrap();
    assert!(bytes.get("positions").is_none());
    let cloned = model.clone();
    let restored: DocxModel = serde_json::from_value(bytes.clone()).unwrap();
    assert_eq!(model, cloned);
    assert_eq!(model, restored);
    assert_eq!(serde_json::to_value(cloned).unwrap(), bytes);
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn stateful_cached_positions_match_canonical_effects_through_structure_revisions_and_restore(
        steps in prop::collection::vec(any::<u64>(),1..100),
    ) {
        let mut model=DocxModel::from_paragraphs(&["AB 日本","middle","é🙂 tail"]);
        let mut next=model.max_lamport()+1;
        for choice in steps {
            let owner=ReplicaId(1+choice%3);
            let ctx=ApplyContext {origin:owner,base_seq:0,seq:if choice%2==0 {Some(1)} else {None}};
            let mut allocate=|n:u64| {let first=Id::new(next,owner);next+=n;first};
            if let Some(op)=common::generate_as(&model,choice,&common::context(owner,choice),&mut allocate) {
                compare(&mut model,&op,&ctx);
            }
            // Exercise several warm transitions after every structural edit;
            // use different stories/carets and UTF-8 widths each time.
            for burst in 0..3 {
                let paragraphs=model.paragraphs();
                if paragraphs.is_empty() {break;}
                let paragraph=&paragraphs[((choice>>8) as usize+burst)%paragraphs.len()];
                let caret=((choice>>16) as usize+burst)%(paragraph.chars.len()+1);
                let offset=paragraph.text.char_indices().map(|(i,_)|i).chain([paragraph.text.len()]).nth(caret).unwrap();
                let intent=crate::intent::IntentOp::InsertText {paragraph:((choice>>8) as usize+burst)%paragraphs.len(),offset,text:["x","é","🙂"][burst].into()};
                next=next.max(model.max_lamport()+1);
                let ops=crate::intent::resolve_with(&model,&intent,&common::context(owner,choice),&mut |n| {let first=Id::new(next,owner);next+=n;first});
                if let Ok(ops)=ops {for op in ops {compare(&mut model,&op,&ctx);}}
            }
            if choice%7==0 {
                let restored:DocxModel=serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap();
                prop_assert_eq!(&model,&restored);model=restored;
            } else if choice%7==1 {model=model.clone();}
            next=next.max(model.max_lamport()+1);
        }
    }
}

#[test]
fn optimistic_edits_do_not_build_a_throwaway_whole_document_index() {
    let mut model = DocxModel::from_paragraphs(&vec!["seed"; 1000]);
    let op = DocxOp::InsertText {
        story: StoryId::Body,
        parent: None,
        first: Id::new(model.max_lamport() + 1, ReplicaId(1)),
        text: "x".into(),
    };
    let mut ctx = context();
    ctx.seq = None;
    reset();
    model.apply(&op, &ctx).unwrap();
    assert_eq!(
        walks(),
        2,
        "optimistic plans retain their canonical linear walk"
    );
    assert_eq!(REPAIRED.with(Cell::get), 0);
}

#[test]
fn ambiguous_legacy_atom_ids_use_the_canonical_walk_until_topology_changes() {
    let mut model = DocxModel::from_paragraphs(&["AB", "C"]);
    let ctx = context();
    compare(
        &mut model,
        &DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: Id::new(100, ReplicaId(1)),
            text: "note".into(),
        },
        &ctx,
    );
    // A legacy/raw model may contain aliases between independent story RGAs.
    // The accelerator must never invent a different interpretation of them.
    model
        .footnotes
        .get_mut(&Id::new(100, ReplicaId(1)))
        .unwrap()
        .story
        .flow
        .insert(Id::new(1, ReplicaId::ORIGIN), None, Atom::Char('z'))
        .unwrap();
    for n in 0..2 {
        let op = DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: Id::new(1000 + n, ReplicaId(1)),
            text: "é".into(),
        };
        compare(&mut model, &op, &ctx);
    }
    compare(
        &mut model,
        &DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: Id::new(100, ReplicaId(1)),
        },
        &ctx,
    );
    for n in 0..2 {
        let op = DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: Id::new(2000 + n, ReplicaId(1)),
            text: "🙂".into(),
        };
        reset();
        model.apply(&op, &ctx).unwrap();
        assert_eq!(
            walks(),
            usize::from(n == 0),
            "structural invalidation allows a fresh eligible index"
        );
    }
}

#[test]
fn refused_insertions_never_build_a_cold_index_or_discard_a_warm_one() {
    let mut model = DocxModel::from_paragraphs(&vec!["seed"; 5000]);
    let ctx = context();
    let first = Id::new(model.max_lamport() + 1, ReplicaId(1));
    let missing = DocxOp::InsertText {
        story: StoryId::Body,
        parent: Some(Id::new(999_999, ReplicaId(2))),
        first,
        text: "x".into(),
    };
    let overflow = DocxOp::InsertText {
        story: StoryId::Body,
        parent: None,
        first: Id::new(u64::MAX, ReplicaId(1)),
        text: "ab".into(),
    };
    for op in [&missing, &overflow] {
        reset();
        assert!(model.apply(op, &ctx).is_err());
        assert_eq!(walks(), 0, "refuse before projecting a cold document");
    }
    model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Body,
                parent: None,
                first,
                text: "x".into(),
            },
            &ctx,
        )
        .unwrap();
    for op in [&missing, &overflow] {
        reset();
        assert!(model.apply(op, &ctx).is_err());
        assert_eq!(walks(), 0);
    }
    reset();
    model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Body,
                parent: Some(first),
                first: first.offset(1),
                text: "y".into(),
            },
            &ctx,
        )
        .unwrap();
    assert_eq!(walks(), 0, "rejection must retain the healthy warm index");
}
