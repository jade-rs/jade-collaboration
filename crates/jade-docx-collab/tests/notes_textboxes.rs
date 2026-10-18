//! Footnotes, endnotes and text boxes are stories of their own. The text box
//! or note reference that places one is a zero-width anchor in its host
//! paragraph's RGA: never deleted by text deletion, it moves with the text
//! around it through splits and joins exactly as jade moves the run, and the
//! paragraphs of the story it places follow it in jade's surface order.

mod common;

use common::{assert_same_order, commit, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{
    CommentContent, DocxEffect, DocxModel, DocxOp, JadeDocx, ParagraphStory, StoryId,
};
use proptest::prelude::*;

const MIXED: &str = "footnote-table-textbox-order.docx";
/// "Host" ¹ [text box] " after box" ² — the paragraph of `MIXED` that anchors
/// two footnotes and a text box.
const HOST: usize = 9;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    }
}

fn fresh(replica: u64) -> impl FnMut(u64) -> Id {
    let mut next = 1_000_000u64;
    move |n| {
        let id = Id::new(next, ReplicaId(replica));
        next += n;
        id
    }
}

/// Resolves `intent` against `model` and commits the operations it makes.
fn edit(model: &mut DocxModel, jade: &mut JadeDocx, intent: IntentOp) -> Vec<DocxOp> {
    let mut next = model.max_lamport() + 1;
    let mut ids = |n: u64| {
        let id = Id::new(next, ReplicaId(1));
        next += n;
        id
    };
    let ops = resolve(model, &intent, &mut ids).expect("the intent resolves");
    for op in &ops {
        commit(model, jade, op.clone());
    }
    ops
}

/// A paragraph's story as a short label, for readable expectations.
fn label(model: &DocxModel, story: StoryId) -> String {
    match story {
        StoryId::Body => "body".into(),
        StoryId::Cell { .. } | StoryId::Split { .. } => "cell".into(),
        StoryId::Footnote { .. } => format!("footnote {}", model.note_id(story).unwrap()),
        StoryId::Endnote { .. } => format!("endnote {}", model.note_id(story).unwrap()),
        StoryId::TextBox { .. } => "text box".into(),
    }
}

/// `text` with its anchors written in: `[n]` for note n, `[box]`.
fn layout(model: &DocxModel, paragraph: usize) -> String {
    let p = &model.paragraphs()[paragraph];
    let mut out = String::new();
    let mut at = 0;
    for anchor in &p.anchors {
        out.push_str(&p.text[at..anchor.offset]);
        at = anchor.offset;
        out.push_str(&match anchor.story {
            StoryId::TextBox { .. } => "[box]".to_string(),
            story => format!("[{}]", model.note_id(story).unwrap()),
        });
    }
    out.push_str(&p.text[at..]);
    out
}

fn reloaded(jade: &JadeDocx) -> (DocxModel, JadeDocx) {
    // These tests check text and anchors, which the base file holds.
    let jade = JadeDocx::load(&jade.export_base().unwrap()).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    assert_same_order(&model, &jade, "after reloading");
    (model, jade)
}

fn layouts(model: &DocxModel) -> Vec<String> {
    (0..model.paragraphs().len())
        .map(|p| layout(model, p))
        .collect()
}

#[test]
fn notes_and_text_boxes_load_as_stories_in_jade_order() {
    let (model, jade) = open(MIXED);
    let listed: Vec<(String, String)> = model
        .paragraphs()
        .iter()
        .map(|p| (label(&model, p.story), p.text.clone()))
        .collect();
    let expected = [
        ("body", "Intro"),
        ("footnote 9", " Note for Intro"),
        ("cell", "Cell A"),
        ("footnote 8", " Note for Cell A"),
        ("cell", "Cell B"),
        ("footnote 7", " Note for Cell B"),
        ("cell", "Cell C"),
        ("cell", "Cell D"),
        ("footnote 6", " Note for Cell D"),
        ("body", "Host after box"),
        ("text box", "Box text"),
        ("footnote 5", " Note for Host"),
        ("footnote 3", " Note after box"),
        ("body", "Tail"),
        ("footnote 2", " Note for Tail"),
        // Referenced from the text box only: after the body, by w:id.
        ("footnote 4", " Note for Box text"),
        // Endnotes last, in order of first reference.
        ("endnote 5", " Endnote for Cell B"),
        ("endnote 4", " Endnote for Tail"),
    ]
    .map(|(a, b)| (a.to_string(), b.to_string()));
    assert_eq!(listed, expected);
    assert_eq!(layout(&model, HOST), "Host[5][box] after box[3]");
    assert_eq!(layout(&model, 10), "Box text[4]");
    assert_eq!(
        model.paragraphs()[10].at,
        ParagraphStory::TextBox {
            host: 5,
            text_box: 0,
            paragraph: 0
        }
    );
    assert_eq!(
        model.paragraphs()[16].at,
        ParagraphStory::Endnote {
            note_id: "5".into(),
            paragraph: 0
        }
    );
    assert_same_order(&model, &jade, "on load");
    // Each placed story is an anchor atom of its host's RGA.
    let anchors = &model.paragraphs()[HOST].anchors;
    assert_eq!(anchors.len(), 3);
    assert_eq!(anchors[1].story, model.paragraphs()[10].story);
}

/// Splitting before the anchors takes them — and the text box and notes
/// they place — to the new paragraph, as jade's split takes the runs.
#[test]
fn anchors_follow_a_split_as_jade_moves_them() {
    let (mut model, mut jade) = open(MIXED);
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: HOST,
            offset: 4,
            count: 1,
        },
    );
    assert_eq!(layout(&model, HOST), "Host");
    assert_eq!(layout(&model, HOST + 1), "[5][box] after box[3]");
    assert_eq!(model.paragraphs()[HOST + 2].text, "Box text");
    assert_eq!(
        model.paragraphs()[HOST + 2].at,
        ParagraphStory::TextBox {
            host: 6,
            text_box: 0,
            paragraph: 0
        }
    );
    // Between the box and the last reference: only that note moves on.
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: HOST + 1,
            offset: " after box".len(),
            count: 1,
        },
    );
    let p = model.paragraphs();
    assert_eq!(layout(&model, HOST + 1), "[5][box] after box");
    assert_eq!(p[HOST + 2].text, "Box text");
    assert_eq!(p[HOST + 3].text, " Note for Host");
    assert_eq!(layout(&model, HOST + 4), "[3]");
    assert_eq!(p[HOST + 5].text, " Note after box");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// jade puts text typed where an anchor sits before it, except at the start
/// of a paragraph, where it goes after the anchors there; the caret the
/// intent API resolves says the same.
#[test]
fn typing_next_to_an_anchor_lands_where_jade_puts_it() {
    let (mut model, mut jade) = open(MIXED);
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: HOST,
            offset: 4,
            text: "xy".into(),
        },
    );
    assert_eq!(layout(&model, HOST), "Hostxy[5][box] after box[3]");
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: HOST,
            offset: 6,
            count: 1,
        },
    );
    let ops = edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: HOST + 1,
            offset: 0,
            text: "z".into(),
        },
    );
    // The caret's parent is the last anchor before the paragraph's text.
    let text_box_anchor = model.paragraphs()[HOST + 1].anchors[1].id;
    assert!(matches!(
        &ops[..],
        [DocxOp::InsertText { parent: Some(parent), .. }] if *parent == text_box_anchor
    ));
    assert_eq!(layout(&model, HOST + 1), "[5][box]z after box[3]");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// Text on both sides of an anchor is deleted as two ranges, one on each
/// side, so jade never gets a range that holds the anchor's run; an anchor
/// itself cannot be deleted as text.
#[test]
fn deleting_around_an_anchor_keeps_it() {
    let (mut model, mut jade) = open(MIXED);
    let host = model.paragraphs()[HOST].clone();
    let around = vec![host.chars[3], host.chars[4]]; // "t" and " "
    let effects = commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: around,
        },
    );
    assert_eq!(
        effects,
        vec![
            DocxEffect::DeleteRange {
                paragraph: ParagraphStory::Body { index: 5 },
                start: 4,
                end: 5,
                anchors: 2,
            },
            DocxEffect::DeleteRange {
                paragraph: ParagraphStory::Body { index: 5 },
                start: 3,
                end: 4,
                anchors: 0,
            },
        ]
    );
    assert_eq!(layout(&model, HOST), "Hos[5][box]after box[3]");
    let anchor = host.anchors[0].id;
    let before = model.clone();
    assert!(model
        .apply(
            &DocxOp::DeleteText {
                story: StoryId::Body,
                ids: vec![anchor],
            },
            &ctx(),
        )
        .is_err());
    assert_eq!(model, before);
    // Everything up to the anchors, then everything after them.
    let host = model.paragraphs()[HOST].clone();
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: host.chars.clone(),
        },
    );
    assert_eq!(layout(&model, HOST), "[5][box][3]");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// jade lists a paragraph's text boxes and first-referenced footnotes
/// between it and the next paragraph and cannot join across them; the model
/// refuses those joins itself. Without anything in between, a join keeps
/// the second paragraph's anchors.
#[test]
fn joins_keep_anchors_and_refuse_what_jade_cannot_join() {
    let (mut model, mut jade) = open(MIXED);
    let host = model.paragraphs()[HOST].clone();
    let before = model.clone();
    let join = DocxOp::JoinParagraph {
        story: StoryId::Body,
        mark: host.mark.unwrap(),
    };
    assert!(model.apply(&join, &ctx()).is_err());
    assert_eq!(model, before);

    // "Tail" ⁶ ⁱ: split before its anchors, then join again.
    let tail = 13;
    assert_eq!(layout(&model, tail), "Tail[2][4]");
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: tail,
            offset: 4,
            count: 1,
        },
    );
    assert_eq!(layout(&model, tail + 1), "[2][4]");
    let mark = model.paragraphs()[tail].mark.unwrap();
    commit(
        &mut model,
        &mut jade,
        DocxOp::JoinParagraph {
            story: StoryId::Body,
            mark,
        },
    );
    assert_eq!(layout(&model, tail), "Tail[2][4]");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

#[test]
fn notes_are_typed_split_and_joined_through_export() {
    let (mut model, mut jade) = open("footnote-long-continuation.docx");
    let first_line = model
        .paragraphs()
        .iter()
        .position(|p| p.text == " Long note line 1.")
        .unwrap();
    assert!(matches!(
        model.paragraphs()[first_line].story,
        StoryId::Footnote { .. }
    ));
    // Offset 0 of a note's first paragraph is after its number.
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: first_line,
            offset: 0,
            text: "N:".into(),
        },
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteText {
            paragraph: first_line,
            offset: "N: Long note line 1.".len(),
            direction: jade_docx_headless_api::HeadlessDeleteDirection::Forward,
            count: 1,
        },
    );
    assert_eq!(
        model.paragraphs()[first_line].text,
        "N: Long note line 1. Long note line 2."
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: first_line,
            offset: 3,
            count: 2,
        },
    );
    assert_eq!(model.paragraphs()[first_line + 1].text, "");
    assert_eq!(
        model.paragraphs()[first_line + 2].at,
        ParagraphStory::Footnote {
            note_id: "2".into(),
            paragraph: 2
        }
    );
    let (again, _) = reloaded(&jade);
    assert_eq!(again.paragraph_texts(), model.paragraph_texts());
}

/// jade writes a text box's text to the file but not new or removed
/// paragraphs in it (see `jade_drops_text_box_paragraph_breaks_on_export`),
/// so a text box's text is editable and its paragraphs are fixed.
#[test]
fn text_box_text_is_editable_and_its_paragraphs_are_fixed() {
    let (mut model, mut jade) = open(MIXED);
    let box_text = 10;
    edit(
        &mut model,
        &mut jade,
        IntentOp::ReplaceRange {
            start_paragraph: box_text,
            start_offset: 0,
            end_paragraph: box_text,
            end_offset: 3,
            text: "Small".into(),
        },
    );
    assert_eq!(layout(&model, box_text), "Small text[4]");
    let p = model.paragraphs()[box_text].clone();
    let before = model.clone();
    let split = DocxOp::SplitParagraph {
        story: p.story,
        parent: Some(p.chars[2]),
        id: Id::new(5_000_000, ReplicaId(1)),
    };
    assert!(model.apply(&split, &ctx()).is_err());
    assert_eq!(model, before);
    assert!(resolve(
        &model,
        &IntentOp::InsertParagraphBreak {
            paragraph: box_text,
            offset: 2,
            count: 1
        },
        &mut fresh(1)
    )
    .is_err());
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// Why text-box paragraphs are fixed: jade splits the paragraph in memory
/// but writes the text box's original paragraphs.
#[test]
fn jade_drops_text_box_paragraph_breaks_on_export() {
    use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
    use jade_docx_headless_api::{HeadlessCommand, HeadlessEditRequest};
    let (_, mut jade) = open("textbox.docx");
    assert_eq!(jade.paragraph_texts(), ["Hello", "Hello", "World"]);
    let mut word = jade.word().clone();
    apply_headless_commands_to_document(
        &mut word,
        &HeadlessEditRequest {
            commands: vec![HeadlessCommand::InsertParagraphBreak {
                paragraph: 1,
                offset: 2,
                count: 1,
            }],
        },
    )
    .unwrap();
    jade = JadeDocx::new(word);
    assert_eq!(jade.paragraph_texts(), ["Hello", "He", "llo", "World"]);
    let reloaded = JadeDocx::load(&jade.export_base().unwrap()).unwrap();
    assert_eq!(reloaded.paragraph_texts(), ["Hello", "Hello", "World"]);
}

/// A comment in a text box works in jade; jade cannot anchor one in a note,
/// and the model refuses it there itself.
#[test]
fn comments_go_in_text_boxes_but_not_in_notes() {
    let (mut model, mut jade) = open(MIXED);
    let box_text = model.paragraphs()[10].clone();
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: box_text.story,
            start: box_text.chars[0],
            end: box_text.chars[2],
            id: Id::new(5_000_000, ReplicaId(1)),
            content: CommentContent::new("boxed"),
        },
    );
    let note = model.paragraphs()[11].clone();
    let before = model.clone();
    let op = DocxOp::AddComment {
        story: note.story,
        start: note.chars[1],
        end: note.chars[3],
        id: Id::new(5_000_001, ReplicaId(1)),
        content: CommentContent::new("noted"),
    };
    assert!(model.apply(&op, &ctx()).is_err());
    assert_eq!(model, before);
}

/// jade's comment splits the host paragraph's runs without relisting its
/// text box; the materializer has jade relist it, so the box stays editable.
#[test]
fn a_comment_on_a_text_box_host_keeps_the_box_editable() {
    let (mut model, mut jade) = open("textbox.docx");
    let host = model.paragraphs()[0].clone();
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: host.chars[1],
            end: host.chars[3],
            id: Id::new(5_000_000, ReplicaId(1)),
            content: CommentContent::new("host"),
        },
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 5,
            text: " box".into(),
        },
    );
    assert_eq!(model.paragraph_texts(), ["Hello", "Hello box", "World"]);
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// Text typed after a reference that is alone in its paragraph, then
/// formatted in part: jade keeps one reference (jade-core gives the text a
/// run of its own; typed into the reference's run, a later split of that
/// run copied the reference).
#[test]
fn text_after_a_lone_reference_keeps_one_reference_when_formatted() {
    let (mut model, mut jade) = open("footnote.docx");
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: 0,
            offset: 5,
            count: 1,
        },
    );
    assert_eq!(layout(&model, 1), "[1]");
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "Lorem".into(),
        },
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::Format {
            start_paragraph: 1,
            start_offset: 1,
            end_paragraph: 1,
            end_offset: 3,
            change: jade_docx_collab::FormatChange::Italic { value: true },
        },
    );
    assert_eq!(layout(&model, 1), "[1]Lorem");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// Text typed at an anchor while someone splits there: whichever reaches
/// the server first, and whichever is newer, every replica orders the two
/// the same way and jade ends up with the same document — even when the
/// text belongs before anchors at the start of the new paragraph, where
/// jade's own typing would put it after them.
#[test]
fn typing_and_a_split_at_one_anchor_agree_in_any_order() {
    let (model, _) = open(MIXED);
    let t = model.paragraphs()[HOST].chars[3];
    for (split_id, text_id) in [(1_001, 1_000), (1_000, 1_001)] {
        let split = DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: Some(t),
            id: Id::new(split_id, ReplicaId(1)),
        };
        let typed = DocxOp::InsertText {
            story: StoryId::Body,
            parent: Some(t),
            first: Id::new(text_id, ReplicaId(2)),
            text: "xy".into(),
        };
        let mut results = Vec::new();
        for order in [[&split, &typed], [&typed, &split]] {
            let (mut model, mut jade) = open(MIXED);
            for op in order {
                commit(&mut model, &mut jade, op.clone());
            }
            let (again, _) = reloaded(&jade);
            assert_eq!(layouts(&again), layouts(&model));
            results.push(model);
        }
        assert_eq!(results[0], results[1]);
        let expected = if split_id > text_id {
            ["Host", "xy[5][box] after box[3]"]
        } else {
            ["Hostxy", "[5][box] after box[3]"]
        };
        assert_eq!(
            [layout(&results[0], HOST), layout(&results[0], HOST + 1)],
            expected
        );
    }
}

/// The character after an anchor deleted while someone types after it: the
/// text stays after the anchor in every order.
#[test]
fn typing_after_a_deleted_character_stays_after_the_anchor() {
    let (model, _) = open(MIXED);
    let space = model.paragraphs()[HOST].chars[4];
    let delete = DocxOp::DeleteText {
        story: StoryId::Body,
        ids: vec![space],
    };
    let typed = DocxOp::InsertText {
        story: StoryId::Body,
        parent: Some(space),
        first: Id::new(1_000, ReplicaId(2)),
        text: "z".into(),
    };
    let mut results = Vec::new();
    for order in [[&delete, &typed], [&typed, &delete]] {
        let (mut model, mut jade) = open(MIXED);
        for op in order {
            commit(&mut model, &mut jade, op.clone());
        }
        assert_eq!(layout(&model, HOST), "Host[5][box]zafter box[3]");
        let (again, _) = reloaded(&jade);
        assert_eq!(layouts(&again), layouts(&model));
        results.push(model);
    }
    assert_eq!(results[0], results[1]);
}

/// What jade cannot do: end a paragraph with an anchor through a split.
/// When the character after an anchor is deleted before a split right after
/// it reaches the server, the model keeps the anchor in the first half and
/// jade could only put it in the second; the materializer reports that
/// instead of writing a different document, and leaves jade as it was.
#[test]
fn a_split_jade_cannot_place_is_reported_not_guessed() {
    let (mut model, mut jade) = open(MIXED);
    let space = model.paragraphs()[HOST].chars[4];
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![space],
        },
    );
    let texts = jade.paragraph_texts();
    let effects = model
        .apply(
            &DocxOp::SplitParagraph {
                story: StoryId::Body,
                parent: Some(space),
                id: Id::new(1_000, ReplicaId(2)),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(layout(&model, HOST), "Host[5][box]");
    let error = jade.materialize(&model, effects, &ctx()).unwrap_err();
    assert!(error.contains("cannot"), "{error}");
    assert_eq!(jade.paragraph_texts(), texts);
}

/// Every anchor of the file, as (story it places, host story) pairs.
fn anchors(model: &DocxModel) -> Vec<(StoryId, StoryId)> {
    let mut out: Vec<(StoryId, StoryId)> = model
        .paragraphs()
        .iter()
        .flat_map(|p| p.anchors.iter().map(move |a| (a.story, p.story)))
        .collect();
    out.sort();
    out
}

/// The stories walked, each once, in order of first appearance; and whether
/// each note's and text box's paragraphs are listed together.
fn walked_stories(model: &DocxModel) -> (Vec<StoryId>, bool) {
    let mut order: Vec<StoryId> = Vec::new();
    let mut contiguous = true;
    for p in model.paragraphs() {
        match order.iter().position(|s| *s == p.story) {
            None => order.push(p.story),
            Some(at) => {
                let placed = matches!(
                    p.story,
                    StoryId::Footnote { .. } | StoryId::Endnote { .. } | StoryId::TextBox { .. }
                );
                if placed && at + 1 != order.len() {
                    contiguous = false;
                }
            }
        }
    }
    order.sort();
    (order, contiguous)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Concurrent convergent edits by three replicas — typing, deleting and
    /// splitting around anchors, in the body, notes and text boxes — applied
    /// in two different interleavings (each replica's own edits in order)
    /// give the same model, and no anchor is lost or duplicated: every text
    /// box and note is still placed exactly once, by the story it was placed
    /// by, and its paragraphs are listed together.
    #[test]
    fn anchors_converge_under_concurrent_edits(
        fixture in prop::sample::select(vec![MIXED, "footnote.docx", "textbox.docx", "floating-wrap-sides.docx"]),
        choices in prop::collection::vec((0..3usize, any::<u64>()), 1..24),
        schedule in any::<u64>(),
    ) {
        let (base, _) = open(fixture);
        let mut views = vec![base.clone(); 3];
        let mut queues: Vec<Vec<DocxOp>> = vec![Vec::new(); 3];
        let mut clocks: Vec<_> = (1..=3u64).map(|r| {
            let mut next = base.max_lamport() + 1;
            move |n: u64| { let id = Id::new(next, ReplicaId(r)); next += n; id }
        }).collect();
        for (replica, choice) in choices {
            // A replica sees the file and its own edits only.
            let Some(op) = common::generate(&views[replica], choice, &mut clocks[replica]) else { continue };
            if DocxModel::lane(&op) != Lane::Convergent { continue; }
            if views[replica].apply(&op, &ctx()).is_ok() {
                queues[replica].push(op);
            }
        }
        // Round robin, and an interleaving the schedule bits pick.
        let interleave = |pick: &dyn Fn(usize) -> usize| {
            let mut next = [0usize; 3];
            let mut out = Vec::new();
            let total: usize = queues.iter().map(Vec::len).sum();
            for step in 0..total {
                let mut r = pick(step) % 3;
                while next[r] == queues[r].len() { r = (r + 1) % 3; }
                out.push(queues[r][next[r]].clone());
                next[r] += 1;
            }
            out
        };
        let first = interleave(&|step| step);
        let second = interleave(&|step| (schedule >> (step % 32 * 2)) as usize);
        let mut a = base.clone();
        let mut b = base.clone();
        for op in &first { prop_assert!(a.apply(op, &ctx()).is_ok(), "{:?}", op); }
        for op in &second { prop_assert!(b.apply(op, &ctx()).is_ok(), "{:?}", op); }
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(anchors(&a), anchors(&base));
        let (stories, contiguous) = walked_stories(&a);
        prop_assert_eq!(stories, walked_stories(&base).0);
        prop_assert!(contiguous, "a note's or text box's paragraphs were split up");
    }
}
