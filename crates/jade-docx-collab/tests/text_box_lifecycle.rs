//! Creating and deleting text boxes. A new text box is a sequenced
//! `InsertTextBox`: an anchor atom placed after a caret of a body or
//! table-cell story and a story of its own holding its text, with ids
//! derived from the operation's; jade draws it as a rectangle shape
//! (`insert_shape`). `DeleteTextBox` takes the anchor and with it the box's
//! story, and jade's `delete_text_box` the drawing — and the comments in it,
//! in jade as in the model.

mod common;

use common::{assert_comments_agree, assert_same_order, commit, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{lane_for_command, resolve, IntentOp};
use jade_docx_collab::{CommentContent, DocxModel, DocxOp, JadeDocx, ParagraphStory, StoryId};
use jade_docx_headless_api::HeadlessCommand;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    }
}

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

fn insert_text_box(paragraph: usize, offset: usize, text: &str) -> IntentOp {
    IntentOp::InsertTextBox {
        paragraph,
        offset,
        text: text.into(),
        width_px: None,
        height_px: None,
    }
}

#[test]
fn text_boxes_are_created_edited_and_deleted_in_jade() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    let ops = edit(&mut model, &mut jade, insert_text_box(1, 3, "boxed"));
    let DocxOp::InsertTextBox {
        story: StoryId::Body,
        parent,
        id,
        width_px,
        height_px,
        ..
    } = &ops[0]
    else {
        panic!("{ops:?}")
    };
    assert_eq!(*parent, Some(model.paragraphs()[1].chars[2]));
    assert!(*width_px > 0 && *height_px > 0);
    assert_eq!(layout(&model, 1), "Wor[box]ld");
    let p = model.paragraphs();
    assert_eq!(p[2].story, StoryId::TextBox { id: *id });
    assert_eq!(p[2].text, "boxed");
    assert_eq!(
        p[2].at,
        ParagraphStory::TextBox {
            host: 1,
            text_box: 0,
            paragraph: 0
        }
    );

    // Typed into, split, and a second box before it in the same paragraph.
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: 2,
            offset: 5,
            text: " in".into(),
        },
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertParagraphBreak {
            paragraph: 2,
            offset: 5,
            count: 1,
        },
    );
    edit(&mut model, &mut jade, insert_text_box(1, 1, ""));
    assert_eq!(layout(&model, 1), "W[box]or[box]ld");
    assert_eq!(
        model.paragraph_texts(),
        ["Hello\u{c}", "World", "", "boxed", " in"]
    );
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));

    // Deleting the first box; the second becomes the paragraph's first.
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteTextBox {
            paragraph: 1,
            text_box: 0,
        },
    );
    assert_eq!(layout(&model, 1), "Wor[box]ld");
    assert_eq!(
        model.paragraph_texts(),
        ["Hello\u{c}", "World", "boxed", " in"]
    );
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteTextBox {
            paragraph: 1,
            text_box: 0,
        },
    );
    assert_eq!(model.paragraph_texts(), ["Hello\u{c}", "World"]);
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// A text box loaded with the file goes the same way, between the note
/// references of its paragraph.
#[test]
fn a_loaded_text_box_is_deleted() {
    let (mut model, mut jade) = open("footnote-table-textbox-order.docx");
    let host = 9;
    assert_eq!(layout(&model, host), "Host[5][box] after box[3]");
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteTextBox {
            paragraph: host,
            text_box: 0,
        },
    );
    assert_eq!(layout(&model, host), "Host[5] after box[3]");
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

#[test]
fn comments_in_a_deleted_text_box_go_with_it() {
    let (mut model, mut jade) = open("textbox.docx");
    let inside = model.paragraphs()[1].clone();
    let host = model.paragraphs()[0].clone();
    for (story, start, end, id, text) in [
        (
            inside.story,
            inside.chars[0],
            inside.chars[2],
            7_000_000,
            "inside",
        ),
        (
            StoryId::Body,
            host.chars[0],
            host.chars[1],
            7_000_001,
            "host",
        ),
    ] {
        commit(
            &mut model,
            &mut jade,
            DocxOp::AddComment {
                story,
                start,
                end,
                id: Id::new(id, ReplicaId(1)),
                content: CommentContent::new(text),
            },
        );
    }
    edit(
        &mut model,
        &mut jade,
        IntentOp::DeleteTextBox {
            paragraph: 0,
            text_box: 0,
        },
    );
    let left: Vec<String> = model
        .comments()
        .into_iter()
        .map(|c| c.content.text)
        .collect();
    assert_eq!(left, ["host"]);
    assert_comments_agree(&model, &jade, "after deleting the box");
}

#[test]
fn where_a_text_box_cannot_go_is_refused() {
    let (mut model, _) = open("footnote-table-textbox-order.docx");
    let p = model.paragraphs();
    let before = model.clone();
    let host = &p[9];
    let text_box = |story, parent, width_px| DocxOp::InsertTextBox {
        story,
        parent,
        id: Id::new(5_000_000, ReplicaId(1)),
        text: "x".into(),
        width_px,
        height_px: 50,
    };
    let StoryId::TextBox { id: loaded_box } = p[10].story else {
        panic!("{:?}", p[10])
    };
    for op in [
        // In a text box or a note.
        text_box(p[10].story, Some(p[10].chars[0]), 100),
        text_box(p[11].story, Some(p[11].chars[0]), 100),
        // A size jade does not draw.
        text_box(StoryId::Body, Some(host.chars[0]), 0),
        text_box(StoryId::Body, Some(host.chars[0]), 11),
        text_box(StoryId::Body, Some(host.chars[0]), 4097),
        // Text jade would not keep.
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: Some(host.chars[0]),
            id: Id::new(5_000_000, ReplicaId(1)),
            text: "a\nb".into(),
            width_px: 100,
            height_px: 50,
        },
        // An id taken by another box.
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: Some(host.chars[0]),
            id: loaded_box,
            text: "x".into(),
            width_px: 100,
            height_px: 50,
        },
        // Unknown boxes.
        DocxOp::DeleteTextBox {
            text_box: Id::new(7, ReplicaId(1)),
        },
    ] {
        assert_eq!(DocxModel::lane(&op), Lane::Sequenced);
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
        assert_eq!(model, before);
    }
    let delete = DocxOp::DeleteTextBox {
        text_box: loaded_box,
    };
    model.apply(&delete, &ctx()).unwrap();
    assert!(model.apply(&delete, &ctx()).is_err(), "deleted twice");
}

/// A deleted text box's story stays, invisible: typing into it made
/// concurrently still applies, and commutes with the deletion.
#[test]
fn text_typed_into_a_concurrently_deleted_text_box_lands_nowhere_visible() {
    let (model, _) = open("textbox.docx");
    let p = model.paragraphs();
    let StoryId::TextBox { id } = p[1].story else {
        panic!("{:?}", p[1])
    };
    let typed = DocxOp::InsertText {
        story: p[1].story,
        parent: Some(p[1].chars[0]),
        first: Id::new(5_000_000, ReplicaId(2)),
        text: "late".into(),
    };
    let delete = DocxOp::DeleteTextBox { text_box: id };
    let (mut a, mut b) = (model.clone(), model.clone());
    a.apply(&typed, &ctx()).unwrap();
    a.apply(&delete, &ctx()).unwrap();
    b.apply(&delete, &ctx()).unwrap();
    b.apply(&typed, &ctx()).unwrap();
    assert_eq!(a, b);
    assert_eq!(b.paragraph_texts(), ["Hello", "World"]);
}

#[test]
fn text_box_ops_have_a_wire_format() {
    let op: DocxOp = serde_json::from_str(
        r#"{"type":"insert_text_box","story":{"kind":"body"},"parent":null,
            "id":{"lamport":9,"replica":1},"text":"x","width_px":120,"height_px":60}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: None,
            id: Id::new(9, ReplicaId(1)),
            text: "x".into(),
            width_px: 120,
            height_px: 60,
        }
    );
    let op: DocxOp =
        serde_json::from_str(r#"{"type":"delete_text_box","text_box":{"lamport":9,"replica":1}}"#)
            .unwrap();
    assert_eq!(
        op,
        DocxOp::DeleteTextBox {
            text_box: Id::new(9, ReplicaId(1)),
        }
    );
    let intent: IntentOp = serde_json::from_str(
        r#"{"type":"insert_text_box","paragraph":0,"offset":1,"text":"t","width_px":90}"#,
    )
    .unwrap();
    assert_eq!(
        intent,
        IntentOp::InsertTextBox {
            paragraph: 0,
            offset: 1,
            text: "t".into(),
            width_px: Some(90),
            height_px: None,
        }
    );
    let command = HeadlessCommand::DeleteTextBox {
        paragraph: 2,
        text_box: 1,
    };
    assert_eq!(lane_for_command(&command), Lane::Sequenced);
    assert_eq!(
        IntentOp::try_from(command).unwrap(),
        IntentOp::DeleteTextBox {
            paragraph: 2,
            text_box: 1
        }
    );
}

/// A text box after a note reference in its paragraph: jade's shape command
/// takes a rendered offset, in which the reference counts its mark (found
/// by the session property test; fixed in jade-core).
#[test]
fn a_text_box_goes_after_a_note_reference() {
    let (mut model, mut jade) = open("footnote.docx");
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertFootnote {
            paragraph: 0,
            offset: 2,
            text: String::new(),
        },
    );
    edit(&mut model, &mut jade, insert_text_box(0, 3, "boxed"));
    let number = model.note_id(model.paragraphs()[2].story).unwrap();
    assert_eq!(layout(&model, 0), format!("He[{number}]l[box]lo[1]"));
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}

/// jade's reply re-anchors on its root's text the way `add_comment` does,
/// splitting the runs there without relisting the paragraph's text boxes;
/// the materializer has jade relist them after a reply too (found by the
/// agreement property test once a text box could be put into a commented
/// paragraph).
#[test]
fn a_reply_keeps_the_text_boxes_of_its_paragraph_listed() {
    let (mut model, mut jade) = open("comment.docx");
    // At the end of the comment's range, "Hello comment".
    edit(&mut model, &mut jade, insert_text_box(0, 13, "箱"));
    edit(
        &mut model,
        &mut jade,
        IntentOp::ReplyToComment {
            comment_id: "0".into(),
            text: "reply".into(),
            author: None,
            initials: None,
            date: None,
        },
    );
    edit(
        &mut model,
        &mut jade,
        IntentOp::InsertText {
            paragraph: 1,
            offset: "箱".len(),
            text: "!".into(),
        },
    );
    assert_eq!(model.paragraph_texts(), ["Hello comment", "箱!"]);
    let (again, _) = reloaded(&jade);
    assert_eq!(layouts(&again), layouts(&model));
}
