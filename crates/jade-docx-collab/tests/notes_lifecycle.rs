//! Creating and deleting footnotes and endnotes. A new note is a sequenced
//! `InsertNote`: an anchor atom placed after a caret of a body or table-cell
//! story, and a story of its own holding what jade writes into a new note
//! (a tab, then the text), with identifiers derived from the operation so
//! every replica agrees. jade numbers the note itself; the server commits
//! the number as a derived operation. `DeleteNote` takes the note's story
//! and every reference to it.

mod common;

use common::{assert_same_order, commit, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{lane_for_command, resolve, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, NoteKind, StoryId};
use jade_docx_headless_api::HeadlessCommand;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
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

fn layouts(model: &DocxModel) -> Vec<String> {
    (0..model.paragraphs().len())
        .map(|p| layout(model, p))
        .collect()
}

fn reloaded(jade: &JadeDocx) -> (DocxModel, JadeDocx) {
    let jade = JadeDocx::load(&jade.export_base().unwrap()).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    assert_same_order(&model, &jade, "after reloading");
    (model, jade)
}

fn insert_footnote(paragraph: usize, offset: usize, text: &str) -> IntentOp {
    IntentOp::InsertFootnote {
        paragraph,
        offset,
        text: text.into(),
    }
}

#[test]
fn notes_are_created_typed_and_deleted_in_jade() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    assert_eq!(model.paragraph_texts(), ["Hello\u{c}", "World"]);
    let ops = edit(&mut model, &mut jade, insert_footnote(0, 2, "first"));
    let DocxOp::InsertNote {
        kind: NoteKind::Footnote,
        story: StoryId::Body,
        parent,
        id,
        ..
    } = &ops[0]
    else {
        panic!("{ops:?}")
    };
    // Placed after the character before the caret.
    assert_eq!(*parent, Some(model.paragraphs()[0].chars[1]));
    let first = model.paragraphs()[1].story;
    assert_eq!(first, StoryId::Footnote { id: *id });
    // What jade writes into a new note: a tab after the number, then the text.
    assert_eq!(model.paragraphs()[1].text, "\tfirst");
    let number = model.note_id(first).unwrap().to_string();
    assert!(!number.is_empty(), "the server numbered the note");
    assert_eq!(layout(&model, 0), format!("He[{number}]llo\u{c}"));

    // Not at the same caret again (see `a_note_cannot_share_an_offset`),
    // but a little further on; notes are listed in reference order.
    edit(&mut model, &mut jade, insert_footnote(0, 4, ""));
    let second = model.paragraphs()[2].story;
    let second_number = model.note_id(second).unwrap().to_string();
    assert_eq!(
        layout(&model, 0),
        format!("He[{number}]ll[{second_number}]o\u{c}")
    );
    assert_eq!(model.paragraphs()[1].text, "\tfirst");
    assert_eq!(model.paragraphs()[2].text, "\t");

    // An endnote at the paragraph's start; endnotes are listed last.
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertEndnote {
            paragraph: 0,
            offset: 0,
            text: "end".into(),
        },
    );
    let last = model.paragraphs().len() - 1;
    assert_eq!(model.paragraphs()[last].text, "\tend");
    assert!(matches!(
        model.paragraphs()[last].story,
        StoryId::Endnote { .. }
    ));

    // The new notes take text like any note.
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: 2,
            offset: 1,
            text: "second".into(),
        },
    );
    assert_eq!(model.paragraphs()[2].text, "\tsecond");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));

    // Deleting a note takes its reference and its text.
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteFootnote {
            footnote_id: number.clone(),
        },
    );
    assert_eq!(layout(&model, 0).matches('[').count(), 2);
    assert!(!model.paragraph_texts().contains(&"\tfirst".to_string()));
    let endnote = model
        .note_id(model.paragraphs()[last - 1].story)
        .unwrap()
        .to_string();
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteEndnote {
            endnote_id: endnote,
        },
    );
    assert_eq!(layout(&model, 0), format!("Hell[{second_number}]o\u{c}"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));

    // jade reuses a freed number; the model follows it.
    let world = model
        .paragraph_texts()
        .iter()
        .position(|t| t == "World")
        .unwrap();
    edit(&mut model, &mut jade, insert_footnote(world, 5, "again"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// A note in a table cell is listed right after the cell's paragraph.
#[test]
fn a_note_goes_in_a_table_cell() {
    let (mut model, mut jade) = open("table-cell-spacing.docx");
    let cell = model
        .paragraphs()
        .iter()
        .position(|p| matches!(p.story, StoryId::Cell { .. }) && !p.text.is_empty())
        .expect("a cell with text");
    edit(&mut model, &mut jade, insert_footnote(cell, 1, "in a cell"));
    assert_eq!(model.paragraphs()[cell + 1].text, "\tin a cell");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// A note next to the notes already at a caret, and in loaded files.
#[test]
fn notes_are_created_next_to_existing_ones() {
    let (mut model, mut jade) = open("footnote-table-textbox-order.docx");
    // "Host" ¹ [text box] " after box" ²: a new note just before the
    // anchors. Its paragraph follows the host's text box, then the notes
    // come in reference order.
    let host = 9;
    edit(&mut model, &mut jade, insert_footnote(host, 3, "new"));
    assert_eq!(model.paragraphs()[host + 2].text, "\tnew");
    let number = model
        .note_id(model.paragraphs()[host + 2].story)
        .unwrap()
        .to_string();
    assert_eq!(
        layout(&model, host),
        format!("Hos[{number}]t[5][box] after box[3]")
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteFootnote {
            footnote_id: "5".into(),
        },
    );
    assert_eq!(
        layout(&model, host),
        format!("Hos[{number}]t[box] after box[3]")
    );
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

#[test]
fn where_a_note_cannot_go_is_refused() {
    let (mut model, _) = open("footnote-table-textbox-order.docx");
    let p = model.paragraphs();
    let before = model.clone();
    let host = &p[9];
    let note = |story, parent| DocxOp::InsertNote {
        kind: NoteKind::Footnote,
        story,
        parent,
        id: Id::new(5_000_000, ReplicaId(1)),
        text: "x".into(),
    };
    for op in [
        // In a text box or in a note: jade inserts notes in body paragraphs.
        note(p[10].story, Some(p[10].chars[0])),
        note(p[11].story, Some(p[11].chars[0])),
        // Text jade would not keep.
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: Some(host.chars[0]),
            id: Id::new(5_000_000, ReplicaId(1)),
            text: "a\nb".into(),
        },
        // An id taken by another note.
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: Some(host.chars[0]),
            id: match p[11].story {
                StoryId::Footnote { id } => id,
                other => panic!("{other:?}"),
            },
            text: "x".into(),
        },
        // Unknown notes.
        DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: Id::new(7, ReplicaId(1)),
        },
        DocxOp::DeleteNote {
            kind: NoteKind::Endnote,
            note: match p[11].story {
                StoryId::Footnote { id } => id,
                other => panic!("{other:?}"),
            },
        },
    ] {
        assert_eq!(DocxModel::lane(&op), Lane::Sequenced);
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
        assert_eq!(model, before);
    }
}

/// Deleting a note tombstones it: an edit to its text made concurrently
/// still applies (a convergent edit must), and stays invisible.
#[test]
fn text_typed_into_a_concurrently_deleted_note_lands_nowhere_visible() {
    let (mut model, _) = open("footnote.docx");
    let p = model.paragraphs();
    let note = p[1].story;
    let StoryId::Footnote { id } = note else {
        panic!("{:?}", p[1])
    };
    let typed = DocxOp::InsertText {
        story: note,
        parent: Some(p[1].chars[0]),
        first: Id::new(5_000_000, ReplicaId(2)),
        text: "late".into(),
    };
    let delete = DocxOp::DeleteNote {
        kind: NoteKind::Footnote,
        note: id,
    };
    let mut a = model.clone();
    a.apply(&typed, &ctx()).unwrap();
    a.apply(&delete, &ctx()).unwrap();
    model.apply(&delete, &ctx()).unwrap();
    model.apply(&typed, &ctx()).unwrap();
    assert_eq!(a, model);
    assert_eq!(model.paragraph_texts(), ["Hello"]);
    assert!(model.paragraphs()[0].anchors.is_empty());
    // And it cannot be deleted twice.
    assert!(model.apply(&delete, &ctx()).is_err());
}

#[test]
fn note_ops_have_a_wire_format() {
    let op: DocxOp = serde_json::from_str(
        r#"{"type":"insert_note","kind":"endnote","story":{"kind":"body"},
            "parent":{"lamport":3,"replica":1},"id":{"lamport":9,"replica":1},"text":"x"}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: Some(Id::new(3, ReplicaId(1))),
            id: Id::new(9, ReplicaId(1)),
            text: "x".into(),
        }
    );
    let op: DocxOp = serde_json::from_str(
        r#"{"type":"delete_note","kind":"footnote","note":{"lamport":9,"replica":1}}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: Id::new(9, ReplicaId(1)),
        }
    );
}

#[test]
fn note_commands_project_into_intents() {
    let cases = [
        (
            HeadlessCommand::InsertFootnote {
                paragraph: 1,
                offset: 2,
                text: "f".into(),
            },
            IntentOp::InsertFootnote {
                paragraph: 1,
                offset: 2,
                text: "f".into(),
            },
        ),
        (
            HeadlessCommand::InsertEndnote {
                paragraph: 1,
                offset: 2,
                text: String::new(),
            },
            IntentOp::InsertEndnote {
                paragraph: 1,
                offset: 2,
                text: String::new(),
            },
        ),
        (
            HeadlessCommand::DeleteFootnote {
                footnote_id: "3".into(),
            },
            IntentOp::DeleteFootnote {
                footnote_id: "3".into(),
            },
        ),
        (
            HeadlessCommand::DeleteEndnote {
                endnote_id: "3".into(),
            },
            IntentOp::DeleteEndnote {
                endnote_id: "3".into(),
            },
        ),
    ];
    for (command, intent) in cases {
        assert_eq!(lane_for_command(&command), Lane::Sequenced);
        assert_eq!(IntentOp::try_from(command).unwrap(), intent);
    }
    let intent: IntentOp =
        serde_json::from_str(r#"{"type":"insert_footnote","paragraph":0,"offset":0,"text":"x"}"#)
            .unwrap();
    assert_eq!(intent, insert_footnote(0, 0, "x"));
    // `text` may be left out.
    let intent: IntentOp =
        serde_json::from_str(r#"{"type":"insert_endnote","paragraph":0,"offset":0}"#).unwrap();
    assert_eq!(
        intent,
        IntentOp::InsertEndnote {
            paragraph: 0,
            offset: 0,
            text: String::new()
        }
    );
}

/// A note goes wherever its caret's atom puts it among the anchors at an
/// offset — jade inserts a new reference before the anchors there, so the
/// materializer types a placeholder after the anchors that come first,
/// inserts the note after it and deletes it again. Acceptance therefore
/// never depends on the text around the caret, which concurrent edits
/// change: a replica's view stays what rebuilding it gives.
#[test]
fn a_note_goes_among_the_anchors_at_its_offset() {
    let (mut model, mut jade) = open("footnote.docx");
    // "Hello" ¹: a note at the caret before the reference goes first.
    edit(&mut model, &mut jade, insert_footnote(0, 5, "a"));
    let first = model
        .note_id(model.paragraphs()[1].story)
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 0), format!("Hello[{first}][1]"));
    // One placed right after the reference itself (its atom the parent).
    let reference = model.paragraphs()[0].anchors[1].id;
    let id = Id::new(model.max_lamport() + 1, ReplicaId(1));
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: Some(reference),
            id,
            text: "e".into(),
        },
    );
    let last = model.paragraphs().len() - 1;
    let endnote = model
        .note_id(model.paragraphs()[last].story)
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 0), format!("Hello[{first}][1][{endnote}]"));
    // And a text box between the two references.
    let between = model.paragraphs()[0].anchors[0].id;
    let id = Id::new(model.max_lamport() + 1, ReplicaId(1));
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: Some(between),
            id,
            text: "b".into(),
            width_px: 100,
            height_px: 50,
        },
    );
    assert_eq!(
        layout(&model, 0),
        format!("Hello[{first}][box][1][{endnote}]")
    );
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// At a paragraph's start a new note goes before the anchors there, as jade
/// puts it.
#[test]
fn a_note_at_a_paragraph_start_goes_before_its_anchors() {
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
        IntentOp::InsertEndnote {
            paragraph: 1,
            offset: 0,
            text: "e".into(),
        },
    );
    let number = model
        .note_id(model.paragraphs().last().unwrap().story)
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 1), format!("[{number}][1]"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// Typing at a caret where a note is added concurrently lands on one side of
/// the new reference or the other, by id — both of which jade can write.
#[test]
fn typing_where_a_note_is_added_concurrently_lands_beside_it() {
    for typed_first in [false, true] {
        for (lamport, replica) in [(10_000, 2), (1, 2)] {
            let (mut model, mut jade) = open("jade-core/hello.docx");
            let p = model.paragraphs();
            let d = *p[1].chars.last().unwrap();
            let note = DocxOp::InsertNote {
                kind: NoteKind::Footnote,
                story: StoryId::Body,
                parent: Some(d),
                id: Id::new(5_000, ReplicaId(1)),
                text: "n".into(),
            };
            let typed = DocxOp::InsertText {
                story: StoryId::Body,
                parent: Some(d),
                first: Id::new(model.max_lamport() + lamport, ReplicaId(replica)),
                text: "!".into(),
            };
            let ops = if typed_first {
                [typed, note]
            } else {
                [note, typed]
            };
            for op in ops {
                commit(&mut model, &mut jade, op);
            }
            let (again, _) = reloaded(&jade);
            assert_eq!(layouts(&again), layouts(&model));
        }
    }
}

/// A deletion can bring two anchors together while text typed after the
/// deleted character is on its way: the text lands between them, which
/// jade's typing cannot write — its insert_text_among_anchors can (found by
/// the session property test).
#[test]
fn text_typed_between_two_anchors_brought_together_is_written() {
    let (mut model, mut jade) = open("footnote.docx");
    // "Hello" ¹ → "Hell" ⁿ "o" ¹.
    edit(&mut model, &mut jade, insert_footnote(0, 4, "n"));
    let p = model.paragraphs();
    let o = p[0].chars[4];
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![o],
        },
    );
    // Typed right after the "o" by someone who had not seen it go.
    let first = Id::new(model.max_lamport() + 1, ReplicaId(2));
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: Some(o),
            first,
            text: "x".into(),
        },
    );
    let number = model
        .note_id(model.paragraphs()[1].story)
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 0), format!("Hell[{number}]x[1]"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// A paragraph that ends with a note reference or a text box joins the next
/// keeping it: jade's ranged join starts at the paragraph's rendered end,
/// before the trailing anchor, and would drop it, so the materializer joins
/// such a paragraph from the next one's start instead. (Refusing the join
/// would make its acceptance depend on whether text after the anchor was
/// deleted — something concurrent edits change.)
#[test]
fn a_paragraph_ending_with_an_anchor_is_joined_keeping_it() {
    for text in ["", "abc"] {
        let (mut model, mut jade) = open("jade-core/hello.docx");
        edit(
            &mut model,
            &mut jade,
            IntentOp::InsertParagraphBreak {
                paragraph: 0,
                offset: "Hello\u{c}".len(),
                count: 1,
            },
        );
        if !text.is_empty() {
            edit(
                &mut model,
                &mut jade,
                IntentOp::InsertText {
                    paragraph: 1,
                    offset: 0,
                    text: text.into(),
                },
            );
        }
        edit(
            &mut model,
            &mut jade,
            IntentOp::InsertEndnote {
                paragraph: 1,
                offset: text.len(),
                text: String::new(),
            },
        );
        let number = model
            .note_id(model.paragraphs().last().unwrap().story)
            .unwrap()
            .to_string();
        assert_eq!(layout(&model, 1), format!("{text}[{number}]"));
        let mark = model.paragraphs()[1].mark.unwrap();
        commit(
            &mut model,
            &mut jade,
            DocxOp::JoinParagraph {
                story: StoryId::Body,
                mark,
            },
        );
        assert_eq!(layout(&model, 1), format!("{text}[{number}]World"));
        let (again, _) = reloaded(&jade);
        assert_eq!(layouts(&again), layouts(&model));
    }
}
