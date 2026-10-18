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
    let jade = JadeDocx::load(&jade.export().unwrap()).unwrap();
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

    // A second note at the same caret lands before the first, in jade and
    // in the model.
    edit(&mut model, &mut jade, insert_footnote(0, 2, ""));
    let second = model.paragraphs()[1].story;
    let second_number = model.note_id(second).unwrap().to_string();
    assert_eq!(
        layout(&model, 0),
        format!("He[{second_number}][{number}]llo\u{c}")
    );
    assert_eq!(model.paragraphs()[1].text, "\t");
    assert_eq!(model.paragraphs()[2].text, "\tfirst");

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
            paragraph: 1,
            offset: 1,
            text: "second".into(),
        },
    );
    assert_eq!(model.paragraphs()[1].text, "\tsecond");
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
    let endnote = model.note_id(model.paragraphs()[last - 1].story).unwrap();
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteEndnote {
            endnote_id: endnote.to_string(),
        },
    );
    assert_eq!(layout(&model, 0), format!("He[{second_number}]llo\u{c}"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));

    // jade reuses a freed number; the model follows it.
    edit(&mut model, &mut jade, insert_footnote(1, 5, "again"));
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
    // "Host" ¹ [text box] " after box" ²: a new note at offset 4 lands
    // before both anchors there, as jade puts it.
    let host = 9;
    edit(&mut model, &mut jade, insert_footnote(host, 4, "new"));
    let number = model.note_id(model.paragraphs()[host + 1].story).unwrap();
    assert_eq!(
        layout(&model, host),
        format!("Host[{number}][5][box] after box[3]")
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
        format!("Host[{number}][box] after box[3]")
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
        // Right after a reference at the same offset: jade always puts a new
        // reference before the ones already there.
        note(StoryId::Body, Some(host.anchors[0].id)),
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

/// At a paragraph's start a new note goes before the anchors there (jade
/// splits there and puts the reference first); a caret after text names the
/// character before it.
#[test]
fn a_note_at_a_paragraph_start_goes_before_its_anchors() {
    let (mut model, mut jade) = open("footnote.docx");
    // "Hello" ¹ — split before the reference, so it starts a paragraph.
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
        .unwrap();
    assert_eq!(layout(&model, 1), format!("[{number}][1]"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}
