//! Tracked changes (Word's Track Changes) in collaborative docx editing.
//!
//! First what jade itself does with a revision session — the behaviour the
//! materializer builds on — then the model: revisions are attributes of
//! atoms (an insertion revision, a grow-only set of deletion revisions),
//! authors come from the server, decisions name whole revisions; then the
//! intent API with tracking on, and model/jade agreement on every
//! character's insertion and deletion, scripted and random.

mod common;

use std::collections::BTreeSet;

use common::{
    assert_revisions_agree, assert_same_order, assert_stories_agree, commit_as, context, fixture,
    generate_as, jade_marks, model_marks, open, revision_kinds,
};
use futures::executor::block_on;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{resolve_with, Context, IntentError, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, RevisionKind, StoryId};
use jade_docx_document_core::document::RevisionKind as JadeKind;
use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
use jade_docx_document_core::word_document::WordDocument;
use jade_docx_document_core::Options;
use jade_docx_headless_api::{
    HeadlessCommand, HeadlessDeleteDirection, HeadlessEditRequest, HeadlessRevisionTarget,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// What jade does.

fn word(name: &str) -> WordDocument {
    block_on(WordDocument::load(&fixture(name), &Options::default())).expect("jade loads it")
}

fn run(word: &mut WordDocument, commands: Vec<HeadlessCommand>) {
    apply_headless_commands_to_document(word, &HeadlessEditRequest { commands })
        .unwrap_or_else(|e| panic!("{e}"));
}

fn track(author: &str) -> HeadlessCommand {
    HeadlessCommand::StartChangeTracking {
        author: Some(author.into()),
        initials: None,
        date: Some("2026-10-18T00:00:00Z".into()),
        date_utc: None,
    }
}

fn delete(paragraph: usize, start: usize, end: usize) -> HeadlessCommand {
    HeadlessCommand::ReplaceRange {
        start_paragraph: paragraph,
        start_offset: start,
        end_paragraph: paragraph,
        end_offset: end,
        text: String::new(),
    }
}

fn insert(paragraph: usize, offset: usize, text: &str) -> HeadlessCommand {
    HeadlessCommand::InsertText {
        paragraph,
        offset,
        text: text.into(),
    }
}

/// `text[kind author (inserted-by author)]` for each run of a paragraph.
fn runs(word: &WordDocument, paragraph: usize) -> Vec<String> {
    word.paragraph(paragraph)
        .unwrap()
        .runs
        .iter()
        .filter(|r| !r.text.is_empty())
        .map(|r| match &r.revision {
            None => r.text.clone(),
            Some(m) => {
                let kind = match m.kind {
                    JadeKind::Inserted => "ins",
                    JadeKind::Deleted => "del",
                    JadeKind::MovedFrom => "from",
                    JadeKind::MovedTo => "to",
                };
                let author = m.author.clone().unwrap_or_default();
                match &m.insertion {
                    Some(i) => format!(
                        "{}[{kind} {author} in ins {}]",
                        r.text,
                        i.author.clone().unwrap_or_default()
                    ),
                    None => format!("{}[{kind} {author}]", r.text),
                }
            }
        })
        .collect()
}

fn decide(accept: bool, targets: Vec<HeadlessRevisionTarget>) -> HeadlessCommand {
    if accept {
        HeadlessCommand::AcceptRevisions {
            kind: None,
            start_paragraph: None,
            end_paragraph: None,
            revision_id: None,
            revision_ids: None,
            revision_targets: Some(targets),
        }
    } else {
        HeadlessCommand::RejectRevisions {
            kind: None,
            start_paragraph: None,
            end_paragraph: None,
            revision_id: None,
            revision_ids: None,
            revision_targets: Some(targets),
        }
    }
}

fn reject_all() -> HeadlessCommand {
    HeadlessCommand::RejectRevisions {
        kind: None,
        start_paragraph: None,
        end_paragraph: None,
        revision_id: None,
        revision_ids: None,
        revision_targets: None,
    }
}

fn run_id(word: &WordDocument, paragraph: usize, text: &str) -> Option<String> {
    word.paragraph(paragraph)
        .unwrap()
        .runs
        .iter()
        .find(|r| r.text == text)
        .and_then(|r| r.revision.as_ref())
        .and_then(|m| m.id.clone())
}

#[test]
fn jade_tracks_typing_as_insertion_runs_of_their_own() {
    let mut w = word("jade-core/hello.docx");
    run(&mut w, vec![track("A"), insert(1, 0, "XY")]);
    assert_eq!(runs(&w, 1), ["XY[ins A]", "World"]);
    // B typing inside A's insertion splits it; A typing on gets a new run.
    run(&mut w, vec![track("B"), insert(1, 1, "b")]);
    run(&mut w, vec![track("A"), insert(1, 3, "Z")]);
    assert_eq!(
        runs(&w, 1),
        ["X[ins A]", "b[ins B]", "Y[ins A]", "Z[ins A]", "World"]
    );
}

#[test]
fn jade_keeps_deleted_text_in_the_paragraph_and_its_first_author() {
    let mut w = word("jade-core/hello.docx");
    run(&mut w, vec![track("A"), delete(1, 1, 3)]);
    assert_eq!(runs(&w, 1), ["W", "or[del A]", "ld"]);
    // Offsets still count the deleted text.
    run(&mut w, vec![track("B"), delete(1, 0, 4)]);
    assert_eq!(runs(&w, 1), ["W[del B]", "or[del A]", "l[del B]", "d"]);
    // Text typed inside a deletion, tracked or not, is not deleted.
    run(&mut w, vec![insert(1, 2, "q")]);
    run(
        &mut w,
        vec![HeadlessCommand::StopChangeTracking, insert(1, 1, "u")],
    );
    assert_eq!(
        runs(&w, 1),
        ["W[del B]", "u", "o[del A]", "q[ins B]", "r[del A]", "l[del B]", "d"]
    );
}

/// Word nests `<w:ins><w:del>` when someone deletes text that is still a
/// tracked insertion; rejecting every change then removes the text, which
/// nobody accepted. jade used to replace the insertion by the deletion.
#[test]
fn jade_keeps_the_insertion_when_inserted_text_is_deleted() {
    let mut w = word("jade-core/hello.docx");
    run(&mut w, vec![track("A"), insert(1, 0, "XYZ")]);
    run(&mut w, vec![track("B"), delete(1, 1, 2)]);
    assert_eq!(
        runs(&w, 1),
        ["X[ins A]", "Y[del B in ins A]", "Z[ins A]", "World"]
    );
    // Through the file and back.
    let bytes = w.export_docx().unwrap();
    let mut w = block_on(WordDocument::load(&bytes, &Options::default())).unwrap();
    assert_eq!(
        runs(&w, 1),
        ["X[ins A]", "Y[del B in ins A]", "Z[ins A]", "World"]
    );
    // Rejecting the deletion restores the insertion; rejecting everything
    // removes the inserted text.
    let id = run_id(&w, 1, "Y").unwrap();
    run(
        &mut w,
        vec![decide(
            false,
            vec![HeadlessRevisionTarget {
                paragraph: 1,
                start_offset: 1,
                end_offset: 2,
                paragraph_mark: false,
                id: Some(id),
            }],
        )],
    );
    assert_eq!(runs(&w, 1), ["X[ins A]", "Y[ins A]", "Z[ins A]", "World"]);
    run(&mut w, vec![track("B"), delete(1, 0, 3)]);
    run(&mut w, vec![reject_all()]);
    assert_eq!(runs(&w, 1), ["World"]);
}

/// With tracking off, text typed at the end of (or inside) a tracked
/// insertion is ordinary text, as in Word; jade used to put it into the
/// insertion.
#[test]
fn jade_types_plain_text_beside_an_insertion_with_tracking_off() {
    let mut w = word("jade-core/hello.docx");
    run(&mut w, vec![track("A"), insert(1, 0, "XY")]);
    run(
        &mut w,
        vec![HeadlessCommand::StopChangeTracking, insert(1, 2, "u")],
    );
    run(&mut w, vec![insert(1, 1, "v")]);
    assert_eq!(runs(&w, 1), ["X[ins A]", "v", "Y[ins A]", "uWorld"]);
}

/// A revision target may name part of a run: only that part is decided.
#[test]
fn jade_decides_part_of_a_revision_run() {
    let mut w = word("jade-core/hello.docx");
    run(
        &mut w,
        vec![track("A"), insert(1, 0, "ABCD"), delete(1, 4, 9)],
    );
    let ins = run_id(&w, 1, "ABCD").unwrap();
    let del = run_id(&w, 1, "World").unwrap();
    run(
        &mut w,
        vec![decide(
            true,
            vec![HeadlessRevisionTarget {
                paragraph: 1,
                start_offset: 1,
                end_offset: 3,
                paragraph_mark: false,
                id: Some(ins),
            }],
        )],
    );
    assert_eq!(runs(&w, 1), ["A[ins A]", "BC", "D[ins A]", "World[del A]"]);
    run(
        &mut w,
        vec![decide(
            false,
            vec![HeadlessRevisionTarget {
                paragraph: 1,
                start_offset: 5,
                end_offset: 7,
                paragraph_mark: false,
                id: Some(del),
            }],
        )],
    );
    assert_eq!(
        runs(&w, 1),
        ["A[ins A]", "BC", "D[ins A]", "W[del A]", "or", "ld[del A]"]
    );
}

// ---------------------------------------------------------------------------
// The model.

fn ctx(origin: u64) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq: 0,
        seq: Some(1),
    }
}

fn server() -> ApplyContext {
    ctx(0)
}

fn name(replica: u64, name: &str) -> DocxOp {
    DocxOp::SetAuthor {
        replica: ReplicaId(replica),
        name: name.into(),
    }
}

fn id(lamport: u64, replica: u64) -> Id {
    Id::new(lamport, ReplicaId(replica))
}

/// One paragraph's characters with their marks, as `text[+author][-author]`.
fn shown(model: &DocxModel, paragraph: usize) -> Vec<String> {
    let p = &model.paragraphs()[paragraph];
    let author = |id: Option<Id>| {
        id.map(|id| {
            model
                .revision(id)
                .unwrap()
                .author
                .clone()
                .unwrap_or_else(|| "?".into())
        })
    };
    p.text
        .chars()
        .zip(&p.tracked)
        .map(|(c, t)| {
            let mut s = c.to_string();
            if let Some(a) = author(t.inserted) {
                s += &format!("+{a}");
            }
            if let Some(a) = author(t.deleted) {
                s += &format!("-{a}");
            }
            s
        })
        .collect()
}

#[test]
fn tracked_operations_travel_the_right_lanes() {
    let story = StoryId::Body;
    assert_eq!(
        DocxModel::lane(&DocxOp::TrackInsertText {
            story,
            parent: None,
            first: id(9, 1),
            text: "x".into(),
            date: None,
        }),
        Lane::Convergent
    );
    assert_eq!(
        DocxModel::lane(&DocxOp::TrackDeleteText {
            story,
            ids: vec![],
            revision: id(9, 1),
            date: None,
        }),
        Lane::Convergent
    );
    for op in [
        DocxOp::SetTracking { on: true },
        name(1, "Ann"),
        DocxOp::AcceptRevisions { revisions: vec![] },
        DocxOp::RejectRevisions { revisions: vec![] },
    ] {
        assert_eq!(DocxModel::lane(&op), Lane::Sequenced, "{op:?}");
    }
}

#[test]
fn only_the_server_names_authors_and_revisions_take_the_name_then_in_force() {
    let mut model = DocxModel::from_paragraphs(&["Hello"]);
    assert!(model.apply(&name(1, "Mallory"), &ctx(1)).is_err());
    assert_eq!(model.author(ReplicaId(1)), None);
    model.apply(&name(1, "Ann"), &server()).unwrap();
    assert_eq!(model.author(ReplicaId(1)), Some("Ann"));
    let chars = model.paragraphs()[0].chars.clone();
    model
        .apply(
            &DocxOp::TrackInsertText {
                story: StoryId::Body,
                parent: Some(chars[4]),
                first: id(10, 1),
                text: "!".into(),
                date: Some("2026-10-18T09:00:00Z".into()),
            },
            &ctx(1),
        )
        .unwrap();
    // Replica 2 has no name: it authors anonymously.
    model
        .apply(
            &DocxOp::TrackDeleteText {
                story: StoryId::Body,
                ids: vec![chars[0]],
                revision: id(11, 2),
                date: None,
            },
            &ctx(2),
        )
        .unwrap();
    assert_eq!(shown(&model, 0), ["H-?", "e", "l", "l", "o", "!+Ann"]);
    // A later rename does not rewrite revisions already made.
    model.apply(&name(1, "Ann Smith"), &server()).unwrap();
    let insertion = model.revision(id(10, 1)).unwrap();
    assert_eq!(insertion.author.as_deref(), Some("Ann"));
    assert_eq!(insertion.date.as_deref(), Some("2026-10-18T09:00:00Z"));
    assert_eq!(insertion.replica, ReplicaId(1));
    assert_eq!(insertion.kind, RevisionKind::Insertion);
    assert_eq!(
        revision_kinds(&model),
        [RevisionKind::Insertion, RevisionKind::Deletion]
    );
    let listed = model.revisions();
    assert_eq!(listed[0].text, "!");
    assert_eq!(listed[1].text, "H");
    assert_eq!(listed[1].paragraphs, [0]);
}

#[test]
fn tracked_deletions_keep_the_text_and_tracking_is_a_document_mode() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "World"]);
    assert!(!model.tracking());
    model
        .apply(&DocxOp::SetTracking { on: true }, &ctx(1))
        .unwrap();
    assert!(model.tracking());
    let world = model.paragraphs()[1].chars.clone();
    model
        .apply(
            &DocxOp::TrackDeleteText {
                story: StoryId::Body,
                ids: world[1..3].to_vec(),
                revision: id(20, 1),
                date: None,
            },
            &ctx(1),
        )
        .unwrap();
    // Still in the text (jade counts it), shown as deleted.
    assert_eq!(model.paragraph_texts(), ["Hello", "World"]);
    assert_eq!(shown(&model, 1), ["W", "o-?", "r-?", "l", "d"]);
    // Deleting a mark or an unknown character is refused as for DeleteText.
    let mark = model.paragraphs()[0].mark.unwrap();
    assert!(model
        .apply(
            &DocxOp::TrackDeleteText {
                story: StoryId::Body,
                ids: vec![mark],
                revision: id(21, 1),
                date: None,
            },
            &ctx(1),
        )
        .is_err());
    // A revision id is used once.
    assert!(model
        .apply(
            &DocxOp::TrackDeleteText {
                story: StoryId::Body,
                ids: vec![world[0]],
                revision: id(20, 1),
                date: None,
            },
            &ctx(1),
        )
        .is_err());
}

fn apply_all(model: &mut DocxModel, ops: &[(DocxOp, u64)]) {
    for (op, origin) in ops {
        model
            .apply(op, &ctx(*origin))
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    }
}

#[test]
fn concurrent_tracked_deletions_of_a_character_converge_on_the_least_revision() {
    let mut base = DocxModel::from_paragraphs(&["abcd"]);
    base.apply(&name(1, "Ann"), &server()).unwrap();
    base.apply(&name(2, "Bob"), &server()).unwrap();
    let c = base.paragraphs()[0].chars.clone();
    let ann = DocxOp::TrackDeleteText {
        story: StoryId::Body,
        ids: c[0..3].to_vec(),
        revision: id(30, 1),
        date: None,
    };
    let bob = DocxOp::TrackDeleteText {
        story: StoryId::Body,
        ids: c[1..4].to_vec(),
        revision: id(29, 2),
        date: None,
    };
    let (mut one, mut two) = (base.clone(), base.clone());
    apply_all(&mut one, &[(ann.clone(), 1), (bob.clone(), 2)]);
    apply_all(&mut two, &[(bob, 2), (ann, 1)]);
    assert_eq!(one, two);
    assert_eq!(shown(&one, 0), ["a-Ann", "b-Bob", "c-Bob", "d-Bob"]);
    // Rejecting Bob's deletion leaves Ann's in force where she deleted too.
    one.apply(
        &DocxOp::RejectRevisions {
            revisions: vec![id(29, 2)],
        },
        &ctx(1),
    )
    .unwrap();
    assert_eq!(shown(&one, 0), ["a-Ann", "b-Ann", "c-Ann", "d"]);
}

#[test]
fn a_tracked_deletion_and_an_insertion_inside_it_converge() {
    let mut base = DocxModel::from_paragraphs(&["abcd"]);
    base.apply(&name(1, "Ann"), &server()).unwrap();
    base.apply(&name(2, "Bob"), &server()).unwrap();
    let c = base.paragraphs()[0].chars.clone();
    let delete = DocxOp::TrackDeleteText {
        story: StoryId::Body,
        ids: c.clone(),
        revision: id(40, 1),
        date: None,
    };
    let insert = DocxOp::TrackInsertText {
        story: StoryId::Body,
        parent: Some(c[1]),
        first: id(41, 2),
        text: "XY".into(),
        date: None,
    };
    let (mut one, mut two) = (base.clone(), base.clone());
    apply_all(&mut one, &[(delete.clone(), 1), (insert.clone(), 2)]);
    apply_all(&mut two, &[(insert, 2), (delete, 1)]);
    assert_eq!(one, two);
    assert_eq!(
        shown(&one, 0),
        ["a-Ann", "b-Ann", "X+Bob", "Y+Bob", "c-Ann", "d-Ann"]
    );
}

#[test]
fn accepting_and_rejecting_whole_revisions() {
    let mut model = DocxModel::from_paragraphs(&["abcd"]);
    model.apply(&name(1, "Ann"), &server()).unwrap();
    model.apply(&name(2, "Bob"), &server()).unwrap();
    let c = model.paragraphs()[0].chars.clone();
    apply_all(
        &mut model,
        &[
            (
                DocxOp::TrackInsertText {
                    story: StoryId::Body,
                    parent: Some(c[3]),
                    first: id(50, 1),
                    text: "EF".into(),
                    date: None,
                },
                1,
            ),
            (
                DocxOp::TrackDeleteText {
                    story: StoryId::Body,
                    ids: c[0..2].to_vec(),
                    revision: id(52, 2),
                    date: None,
                },
                2,
            ),
        ],
    );
    let ef = model.paragraphs()[0].chars[4..6].to_vec();
    // Bob deletes part of Ann's insertion: it stays inserted and is deleted.
    model
        .apply(
            &DocxOp::TrackDeleteText {
                story: StoryId::Body,
                ids: vec![ef[1]],
                revision: id(53, 2),
                date: None,
            },
            &ctx(2),
        )
        .unwrap();
    assert_eq!(
        shown(&model, 0),
        ["a-Bob", "b-Bob", "c", "d", "E+Ann", "F+Ann-Bob"]
    );
    // An empty decision or an unknown revision is refused.
    assert!(model
        .apply(&DocxOp::AcceptRevisions { revisions: vec![] }, &ctx(1))
        .is_err());
    assert!(model
        .apply(
            &DocxOp::AcceptRevisions {
                revisions: vec![id(999, 9)]
            },
            &ctx(1)
        )
        .is_err());
    // Rejecting Ann's insertion removes it, deleted part included.
    let mut rejected = model.clone();
    rejected
        .apply(
            &DocxOp::RejectRevisions {
                revisions: vec![id(50, 1)],
            },
            &ctx(1),
        )
        .unwrap();
    assert_eq!(shown(&rejected, 0), ["a-Bob", "b-Bob", "c", "d"]);
    // Accepting it makes it plain text, still deleted where Bob deleted.
    model
        .apply(
            &DocxOp::AcceptRevisions {
                revisions: vec![id(50, 1)],
            },
            &ctx(1),
        )
        .unwrap();
    assert_eq!(shown(&model, 0), ["a-Bob", "b-Bob", "c", "d", "E", "F-Bob"]);
    // Accepting Bob's deletions removes the text; rejecting is then a no-op
    // for what is gone, and accepting again is allowed (it names a known
    // revision) and changes nothing.
    model
        .apply(
            &DocxOp::AcceptRevisions {
                revisions: vec![id(52, 2), id(53, 2)],
            },
            &ctx(1),
        )
        .unwrap();
    assert_eq!(model.paragraph_texts(), ["cdE"]);
    assert!(model.revisions().is_empty());
    let before = model.clone();
    model
        .apply(
            &DocxOp::AcceptRevisions {
                revisions: vec![id(52, 2)],
            },
            &ctx(1),
        )
        .unwrap();
    assert_eq!(model, before);
}

#[test]
fn a_files_revisions_load_as_tracked_characters() {
    let (model, jade) = open("review.docx");
    assert_eq!(model.paragraph_texts(), ["Hello World", ""]);
    assert_eq!(
        shown(&model, 0),
        [
            "H+植木 智之",
            "e+植木 智之",
            "l+植木 智之",
            "l+植木 智之",
            "o+植木 智之",
            " +植木 智之",
            "W-植木 智之",
            "o-植木 智之",
            "r-植木 智之",
            "l-植木 智之",
            "d-植木 智之",
        ]
    );
    let revisions = model.revisions();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].revision.file_id.as_deref(), Some("0"));
    assert_eq!(
        revisions[0].revision.date.as_deref(),
        Some("2025-11-12T19:17:00Z")
    );
    assert_eq!(revisions[1].text, "World");
    assert_revisions_agree(&model, &jade, "on load");
    // Moves load as moved deletions and insertions.
    let (model, jade) = open("review-kinds.docx");
    assert_revisions_agree(&model, &jade, "on load");
    let moved: Vec<(RevisionKind, String)> = model
        .revisions()
        .into_iter()
        .filter(|r| r.revision.moved)
        .map(|r| (r.revision.kind, r.text))
        .collect();
    assert_eq!(
        moved,
        [
            (RevisionKind::Deletion, "moved words".to_string()),
            (RevisionKind::Insertion, "moved words".to_string()),
        ]
    );
}

/// Any convergent operation commutes with any text or revision operation
/// made concurrently from the same state — the condition the client fast
/// path needs (`view == rebuilt view`). An operation that does not apply in
/// one order is skipped, as a replica's rebuild skips it.
#[test]
fn convergent_tracked_operations_commute_with_every_text_and_revision_operation() {
    let names = [None, Some("Ann"), Some("Bob")];
    proptest!(ProptestConfig::with_cases(400), |(
        fixture in prop::sample::select(vec!["jade-core/hello.docx", "review.docx", "review-kinds.docx"]),
        history in prop::collection::vec((1u64..3, any::<u64>()), 0..12),
        a in (1u64..3, any::<u64>()),
        b in (1u64..3, any::<u64>()),
    )| {
        let (mut model, _) = open(fixture);
        for (replica, n) in names.iter().enumerate() {
            if let Some(n) = n {
                model.apply(&name(replica as u64, n), &server()).unwrap();
            }
        }
        let mut next = 1_000_000u64;
        let mut fresh = |replica: u64, n: u64| {
            let id = Id::new(next, ReplicaId(replica));
            next += n;
            id
        };
        for (replica, choice) in history {
            let ctx_ = context(ReplicaId(replica), choice);
            let op = generate_as(&model, choice, &ctx_, &mut |n| fresh(replica, n));
            if let Some(op) = op {
                let _ = model.apply(&op, &ctx(replica));
            }
        }
        let make = |(replica, choice): (u64, u64), model: &DocxModel, fresh: &mut dyn FnMut(u64, u64) -> Id| {
            let ctx_ = context(ReplicaId(replica), choice);
            generate_as(model, choice, &ctx_, &mut |n| fresh(replica, n)).map(|op| (op, replica))
        };
        let Some((op_a, ra)) = make(a, &model, &mut fresh) else { return Ok(()); };
        let Some((op_b, rb)) = make(b, &model, &mut fresh) else { return Ok(()); };
        prop_assume!(DocxModel::lane(&op_a) == Lane::Convergent);
        prop_assume!(matches!(
            op_b,
            DocxOp::InsertText { .. } | DocxOp::DeleteText { .. } | DocxOp::SplitParagraph { .. }
                | DocxOp::JoinParagraph { .. } | DocxOp::TrackInsertText { .. }
                | DocxOp::TrackDeleteText { .. } | DocxOp::AcceptRevisions { .. }
                | DocxOp::RejectRevisions { .. } | DocxOp::SetTracking { .. }
        ));
        let mut ab = model.clone();
        let _ = ab.apply(&op_a, &ctx(ra));
        let _ = ab.apply(&op_b, &ctx(rb));
        let mut ba = model.clone();
        let _ = ba.apply(&op_b, &ctx(rb));
        let _ = ba.apply(&op_a, &ctx(ra));
        prop_assert_eq!(ab, ba, "{:?} and {:?} do not commute", op_a, op_b);
    });
}

// ---------------------------------------------------------------------------
// The intent API with tracking on.

fn tracking_on(mut model: DocxModel) -> DocxModel {
    model
        .apply(&DocxOp::SetTracking { on: true }, &ctx(1))
        .unwrap();
    model
}

fn as_replica(replica: u64) -> Context {
    Context {
        replica: Some(ReplicaId(replica)),
        date: Some("2026-10-18T12:00:00Z".into()),
    }
}

fn ids_from(start: u64, replica: u64) -> impl FnMut(u64) -> Id {
    let mut next = start;
    move |n| {
        let id = Id::new(next, ReplicaId(replica));
        next += n;
        id
    }
}

#[test]
fn with_tracking_on_typing_and_deleting_become_tracked() {
    let model = tracking_on(DocxModel::from_paragraphs(&["Hello", "World"]));
    let ops = resolve_with(
        &model,
        &IntentOp::InsertText {
            paragraph: 0,
            offset: 5,
            text: "!".into(),
        },
        &as_replica(1),
        &mut ids_from(100, 1),
    )
    .unwrap();
    assert!(
        matches!(&ops[..], [DocxOp::TrackInsertText { text, date, .. }]
            if text == "!" && date.as_deref() == Some("2026-10-18T12:00:00Z")),
        "{ops:?}"
    );
    // A deletion across a paragraph end: the characters are tracked, the
    // join is not.
    let ops = resolve_with(
        &model,
        &IntentOp::ReplaceRange {
            start_paragraph: 0,
            start_offset: 3,
            end_paragraph: 1,
            end_offset: 1,
            text: "p".into(),
        },
        &as_replica(1),
        &mut ids_from(200, 1),
    )
    .unwrap();
    assert!(
        matches!(
            &ops[..],
            [
                DocxOp::TrackDeleteText { ids, .. },
                DocxOp::JoinParagraph { .. },
                DocxOp::TrackInsertText { .. },
            ] if ids.len() == 3
        ),
        "{ops:?}"
    );
    // Splitting stays untracked.
    let ops = resolve_with(
        &model,
        &IntentOp::InsertParagraphBreak {
            paragraph: 0,
            offset: 2,
            count: 1,
        },
        &as_replica(1),
        &mut ids_from(300, 1),
    )
    .unwrap();
    assert!(matches!(&ops[..], [DocxOp::SplitParagraph { .. }]));
    // With tracking off nothing is tracked.
    let plain = DocxModel::from_paragraphs(&["Hello"]);
    let ops = resolve_with(
        &plain,
        &IntentOp::DeleteText {
            paragraph: 0,
            offset: 0,
            direction: HeadlessDeleteDirection::Forward,
            count: 2,
        },
        &as_replica(1),
        &mut ids_from(400, 1),
    )
    .unwrap();
    assert!(matches!(&ops[..], [DocxOp::DeleteText { .. }]));
}

#[test]
fn deleting_ones_own_insertion_removes_it_and_deleted_text_is_skipped() {
    let mut model = tracking_on(DocxModel::from_paragraphs(&["abc"]));
    model.apply(&name(1, "Ann"), &server()).unwrap();
    model.apply(&name(2, "Bob"), &server()).unwrap();
    let mut ann = ids_from(100, 1);
    // Ann types "XY" after "c" and Bob deletes "a".
    for op in resolve_with(
        &model,
        &IntentOp::InsertText {
            paragraph: 0,
            offset: 3,
            text: "XY".into(),
        },
        &as_replica(1),
        &mut ann,
    )
    .unwrap()
    {
        model.apply(&op, &ctx(1)).unwrap();
    }
    for op in resolve_with(
        &model,
        &IntentOp::DeleteText {
            paragraph: 0,
            offset: 0,
            direction: HeadlessDeleteDirection::Forward,
            count: 1,
        },
        &as_replica(2),
        &mut ids_from(200, 2),
    )
    .unwrap()
    {
        model.apply(&op, &ctx(2)).unwrap();
    }
    assert_eq!(shown(&model, 0), ["a-Bob", "b", "c", "X+Ann", "Y+Ann"]);
    // Ann deletes everything: "a" is already deleted (kept as Bob's), "b"
    // and "c" are tracked, her own "XY" disappears.
    let ops = resolve_with(
        &model,
        &IntentOp::ReplaceRange {
            start_paragraph: 0,
            start_offset: 0,
            end_paragraph: 0,
            end_offset: 5,
            text: String::new(),
        },
        &as_replica(1),
        &mut ann,
    )
    .unwrap();
    for op in &ops {
        model.apply(op, &ctx(1)).unwrap();
    }
    assert_eq!(shown(&model, 0), ["a-Bob", "b-Ann", "c-Ann"]);
    // Bob deleting Ann's insertion tracks it.
    let mut model2 = tracking_on(DocxModel::from_paragraphs(&["abc"]));
    model2.apply(&name(1, "Ann"), &server()).unwrap();
    model2.apply(&name(2, "Bob"), &server()).unwrap();
    for op in resolve_with(
        &model2,
        &IntentOp::InsertText {
            paragraph: 0,
            offset: 3,
            text: "XY".into(),
        },
        &as_replica(1),
        &mut ids_from(100, 1),
    )
    .unwrap()
    {
        model2.apply(&op, &ctx(1)).unwrap();
    }
    for op in resolve_with(
        &model2,
        &IntentOp::DeleteText {
            paragraph: 0,
            offset: 5,
            direction: HeadlessDeleteDirection::Backward,
            count: 1,
        },
        &as_replica(2),
        &mut ids_from(200, 2),
    )
    .unwrap()
    {
        model2.apply(&op, &ctx(2)).unwrap();
    }
    assert_eq!(shown(&model2, 0), ["a", "b", "c", "X+Ann", "Y+Ann-Bob"]);
}

#[test]
fn revisions_are_accepted_and_rejected_by_id_by_paragraphs_or_all() {
    let (model, _) = open("review-kinds.docx");
    let all: BTreeSet<Id> = model.revisions().iter().map(|r| r.id).collect();
    let decided = |op: &IntentOp| -> Vec<Id> {
        match &resolve_with(&model, op, &as_replica(1), &mut ids_from(1, 1)).unwrap()[..] {
            [DocxOp::AcceptRevisions { revisions }] | [DocxOp::RejectRevisions { revisions }] => {
                revisions.clone()
            }
            other => panic!("{other:?}"),
        }
    };
    let accept_all = IntentOp::AcceptRevisions {
        revision_ids: None,
        start_paragraph: None,
        end_paragraph: None,
    };
    assert_eq!(
        decided(&accept_all).into_iter().collect::<BTreeSet<_>>(),
        all
    );
    // By the file's w:id or the model's id.
    let first = &model.revisions()[0];
    let by_file = IntentOp::RejectRevisions {
        revision_ids: Some(vec![first.revision.file_id.clone().unwrap()]),
        start_paragraph: None,
        end_paragraph: None,
    };
    assert_eq!(decided(&by_file), [first.id]);
    let by_model = IntentOp::RejectRevisions {
        revision_ids: Some(vec![first.id.to_string()]),
        start_paragraph: None,
        end_paragraph: None,
    };
    assert_eq!(decided(&by_model), [first.id]);
    // The revisions touching paragraph 0.
    let in_first = IntentOp::AcceptRevisions {
        revision_ids: None,
        start_paragraph: Some(0),
        end_paragraph: Some(0),
    };
    let expected: Vec<Id> = model
        .revisions()
        .into_iter()
        .filter(|r| r.paragraphs.contains(&0))
        .map(|r| r.id)
        .collect();
    assert_eq!(decided(&in_first), expected);
    assert!(matches!(
        resolve_with(
            &model,
            &IntentOp::AcceptRevisions {
                revision_ids: Some(vec!["nope".into()]),
                start_paragraph: None,
                end_paragraph: None,
            },
            &as_replica(1),
            &mut ids_from(1, 1),
        ),
        Err(IntentError::NoSuchRevision(_))
    ));
    // Nothing to decide is an error, not an empty operation.
    let clean = DocxModel::from_paragraphs(&["x"]);
    assert!(matches!(
        resolve_with(&clean, &accept_all, &as_replica(1), &mut ids_from(1, 1)),
        Err(IntentError::NoRevisions)
    ));
}

#[test]
fn headless_change_tracking_commands_map_to_intents() {
    assert_eq!(
        IntentOp::try_from(HeadlessCommand::StartChangeTracking {
            author: Some("ignored: the server names authors".into()),
            initials: None,
            date: None,
            date_utc: None,
        }),
        Ok(IntentOp::SetTracking { on: true })
    );
    assert_eq!(
        IntentOp::try_from(HeadlessCommand::StopChangeTracking),
        Ok(IntentOp::SetTracking { on: false })
    );
    assert_eq!(
        IntentOp::try_from(HeadlessCommand::AcceptRevisions {
            kind: None,
            start_paragraph: Some(1),
            end_paragraph: Some(2),
            revision_id: Some("7".into()),
            revision_ids: None,
            revision_targets: None,
        }),
        Ok(IntentOp::AcceptRevisions {
            revision_ids: Some(vec!["7".into()]),
            start_paragraph: Some(1),
            end_paragraph: Some(2),
        })
    );
    assert_eq!(
        IntentOp::try_from(reject_all()),
        Ok(IntentOp::RejectRevisions {
            revision_ids: None,
            start_paragraph: None,
            end_paragraph: None,
        })
    );
}

// ---------------------------------------------------------------------------
// The model and jade agree.

/// The model, jade and the author table of a scripted session: r1 is Ann,
/// r2 Bob, r3 has no name.
fn session(fixture_name: &str) -> (DocxModel, JadeDocx) {
    let (mut model, mut jade) = open(fixture_name);
    for op in [name(1, "Ann"), name(2, "Bob")] {
        commit_as(&mut model, &mut jade, op, ReplicaId::ORIGIN);
    }
    (model, jade)
}

/// Resolves `intent` for `replica` with tracking as the model says and
/// commits the operations.
fn edit(model: &mut DocxModel, jade: &mut JadeDocx, replica: u64, intent: IntentOp) {
    let mut next = model.max_lamport() + 1;
    let ops = resolve_with(model, &intent, &as_replica(replica), &mut |n| {
        let id = Id::new(next, ReplicaId(replica));
        next += n;
        id
    })
    .unwrap_or_else(|e| panic!("{intent:?}: {e}"));
    for op in ops {
        commit_as(model, jade, op, ReplicaId(replica));
    }
}

fn reload_agrees(model: &DocxModel, jade: &JadeDocx) {
    let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
    assert_same_order(model, &reloaded, "against the reloaded file");
    let again = DocxModel::from_word_document(reloaded.word()).unwrap();
    assert_eq!(
        model_marks(&again),
        model_marks(model),
        "marks after reloading"
    );
    assert_eq!(jade_marks(&again, &reloaded), model_marks(model));
}

#[test]
fn tracked_edits_are_realised_in_jade_with_their_authors() {
    let (mut model, mut jade) = session("jade-core/hello.docx");
    commit_as(
        &mut model,
        &mut jade,
        DocxOp::SetTracking { on: true },
        ReplicaId(1),
    );
    // Ann types into "World", Bob deletes part of it and of Ann's text.
    edit(
        &mut model,
        &mut jade,
        1,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "New ".into(),
        },
    );
    edit(
        &mut model,
        &mut jade,
        2,
        IntentOp::ReplaceRange {
            start_paragraph: 1,
            start_offset: 2,
            end_paragraph: 1,
            end_offset: 6,
            text: "!".into(),
        },
    );
    // Replica 3 (no name) deletes Ann's remaining "N"; Ann removes her own
    // "e" outright.
    edit(
        &mut model,
        &mut jade,
        3,
        IntentOp::DeleteText {
            paragraph: 1,
            offset: 0,
            direction: HeadlessDeleteDirection::Forward,
            count: 1,
        },
    );
    edit(
        &mut model,
        &mut jade,
        1,
        IntentOp::DeleteText {
            paragraph: 1,
            offset: 1,
            direction: HeadlessDeleteDirection::Forward,
            count: 1,
        },
    );
    reload_agrees(&model, &jade);
    // Accept Bob's insertion, reject everything else.
    let bob: Vec<Id> = model
        .revisions()
        .into_iter()
        .filter(|r| r.revision.author.as_deref() == Some("Bob"))
        .filter(|r| r.revision.kind == RevisionKind::Insertion)
        .map(|r| r.id)
        .collect();
    commit_as(
        &mut model,
        &mut jade,
        DocxOp::AcceptRevisions { revisions: bob },
        ReplicaId(1),
    );
    let rest: Vec<Id> = model.revisions().iter().map(|r| r.id).collect();
    commit_as(
        &mut model,
        &mut jade,
        DocxOp::RejectRevisions { revisions: rest },
        ReplicaId(2),
    );
    assert!(model.revisions().is_empty());
    assert_eq!(model.paragraph_texts()[1], "Wo!rld");
    reload_agrees(&model, &jade);
}

/// A deletion with a smaller revision id arriving later in the log takes
/// over the characters it shares with an earlier one, and gives them back
/// when it is rejected: jade shows the model's author throughout.
#[test]
fn jade_shows_the_least_deletion_whatever_the_log_order() {
    let (mut model, mut jade) = session("jade-core/hello.docx");
    let world = model.paragraphs()[1].chars.clone();
    let later = DocxOp::TrackDeleteText {
        story: StoryId::Body,
        ids: world[0..3].to_vec(),
        revision: id(500, 1),
        date: None,
    };
    let earlier = DocxOp::TrackDeleteText {
        story: StoryId::Body,
        ids: world[2..5].to_vec(),
        revision: id(400, 2),
        date: Some("2026-10-17T00:00:00Z".into()),
    };
    commit_as(&mut model, &mut jade, later, ReplicaId(1));
    commit_as(&mut model, &mut jade, earlier, ReplicaId(2));
    assert_eq!(
        shown(&model, 1),
        ["W-Ann", "o-Ann", "r-Bob", "l-Bob", "d-Bob"]
    );
    commit_as(
        &mut model,
        &mut jade,
        DocxOp::RejectRevisions {
            revisions: vec![id(400, 2)],
        },
        ReplicaId(3),
    );
    assert_eq!(shown(&model, 1), ["W-Ann", "o-Ann", "r-Ann", "l", "d"]);
    reload_agrees(&model, &jade);
}

#[test]
fn a_files_revisions_are_accepted_and_rejected_in_jade() {
    for fixture_name in [
        "review.docx",
        "review-kinds.docx",
        "numbering-with-review.docx",
    ] {
        for accept in [true, false] {
            let (mut model, mut jade) = session(fixture_name);
            let first = model.revisions()[0].id;
            let op = |revisions| {
                if accept {
                    DocxOp::AcceptRevisions { revisions }
                } else {
                    DocxOp::RejectRevisions { revisions }
                }
            };
            commit_as(&mut model, &mut jade, op(vec![first]), ReplicaId(1));
            let rest: Vec<Id> = model.revisions().iter().map(|r| r.id).collect();
            if !rest.is_empty() {
                commit_as(&mut model, &mut jade, op(rest), ReplicaId(1));
            }
            assert!(model.revisions().is_empty(), "{fixture_name}");
            reload_agrees(&model, &jade);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random edits by three authors (Ann, Bob, and one with no name) —
    /// tracked and untracked, accepting and rejecting — keep every
    /// character's tracked insertion and deletion the same in the model and
    /// in jade, and in the file jade writes.
    #[test]
    fn random_tracked_edits_keep_model_and_jade_in_agreement(
        fixture_name in prop::sample::select(vec![
            "jade-core/hello.docx",
            "review.docx",
            "review-kinds.docx",
            "review-duplicate-ids.docx",
            "footnote.docx",
            "table-cell-spacing.docx",
            "textbox.docx",
        ]),
        steps in prop::collection::vec((1u64..4, any::<u64>()), 1..25),
    ) {
        let (mut model, mut jade) = session(fixture_name);
        for (replica, choice) in steps {
            let mut next = model.max_lamport() + 1;
            let ctx_ = context(ReplicaId(replica), choice);
            let op = generate_as(&model, choice, &ctx_, &mut |n| {
                let id = Id::new(next, ReplicaId(replica));
                next += n;
                id
            });
            let Some(op) = op else { continue };
            let lane = DocxModel::lane(&op);
            let mut trial = model.clone();
            if trial.apply(&op, &ctx(replica)).is_err() {
                continue;
            }
            if lane == Lane::Sequenced
                && jade_collab_sync::Materializer::admit(&jade, &model, &op).is_err()
            {
                continue;
            }
            commit_as(&mut model, &mut jade, op, ReplicaId(replica));
        }
        assert_stories_agree(&model, &jade, "at the end");
        reload_agrees(&model, &jade);
    }
}
