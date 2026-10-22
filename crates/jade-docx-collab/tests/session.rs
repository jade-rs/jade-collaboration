//! Several replicas edit a real document concurrently through the sequencer;
//! the server realises the log in jade. Everyone converges, and the file the
//! server writes is the document everyone sees — footnotes, endnotes and
//! text boxes included, with their anchors where the model has them.

mod common;

use common::{
    assert_comments_agree, assert_formatting_agrees, assert_revisions_agree, assert_same_order,
    assert_stories_agree, context, generate_as, jade_comments, model_marks, open,
};
use jade_collab_core::id::ReplicaId;
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{Document, Sequencer};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    Local {
        client: usize,
        choice: u64,
    },
    Up {
        client: usize,
    },
    Down {
        client: usize,
    },
    /// The server stops and comes back from its checkpoint and the file
    /// jade wrote, as a durable server restarts.
    Restart,
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..3usize, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            2 => (0..3usize).prop_map(|client| Step::Up { client }),
            2 => (0..3usize).prop_map(|client| Step::Down { client }),
            1 => Just(Step::Restart),
        ],
        0..60,
    )
}

#[test]
fn a_text_box_between_moved_runs_keeps_tracked_deletion_after_reload() {
    use jade_collab_core::id::Id;
    use jade_docx_collab::intent::{resolve, IntentOp};
    let (mut model, mut jade) = open("review-kinds.docx");
    let moved = |model: &DocxModel| {
        model
            .revisions()
            .into_iter()
            .filter(|r| r.revision.moved)
            .map(|r| (r.revision.kind, r.revision.author, r.revision.date))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let before_moves = moved(&model);
    let mut next = model.max_lamport() + 1;
    let ops = resolve(
        &model,
        &IntentOp::InsertTextBox {
            paragraph: 1,
            offset: 4,
            text: "箱".into(),
            width_px: Some(216),
            height_px: Some(96),
        },
        &mut |n| {
            let first = Id::new(next, ReplicaId(1));
            next += n;
            first
        },
    )
    .unwrap();
    for op in ops {
        common::commit(&mut model, &mut jade, op);
    }
    jade = JadeDocx::restore(&jade.export_base().unwrap(), &model).unwrap();
    let paragraph = model
        .paragraphs()
        .into_iter()
        .find(|p| matches!(p.story, jade_docx_collab::StoryId::TextBox { .. }))
        .unwrap();
    let op = DocxOp::TrackDeleteText {
        story: paragraph.story,
        ids: paragraph.chars,
        revision: Id::new(model.max_lamport() + 1, ReplicaId(1)),
        date: None,
    };
    common::commit(&mut model, &mut jade, op);
    let reloaded = JadeDocx::load(&jade.export(&model).unwrap()).unwrap();
    assert_same_order(&model, &reloaded, "text box between moved runs");
    let imported = DocxModel::from_word_document(reloaded.word()).unwrap();
    assert_eq!(model_marks(&imported), model_marks(&model));
    assert_eq!(moved(&imported), before_moves);
    assert!(imported
        .paragraphs()
        .iter()
        .find(|p| p.text == "箱")
        .unwrap()
        .tracked[0]
        .deleted
        .is_some());
}

#[test]
fn remote_deletion_replays_pending_sequenced_comment_and_then_the_server_refuses_it() {
    let (model, jade) = open("footnote.docx");
    let paragraph = model.paragraphs().remove(0);
    let mut sim = Simulation::with_materializer(model, jade, 3);
    let comment = sim.client_mut(0).reserve(1);
    sim.local(
        0,
        DocxOp::AddComment {
            story: paragraph.story,
            start: paragraph.chars[0],
            end: *paragraph.chars.last().unwrap(),
            id: comment,
            content: jade_docx_collab::CommentContent::new("pending comment"),
        },
    )
    .unwrap();
    sim.local(
        1,
        DocxOp::DeleteText {
            story: paragraph.story,
            ids: paragraph.chars,
        },
    )
    .unwrap();
    sim.deliver_up(1);
    sim.deliver_down(0).unwrap();
    assert_eq!(sim.client(0).view(), &sim.client(0).rebuilt_view());
    assert!(sim.client(0).view().comments().is_empty());
    assert_eq!(sim.client(0).pending_len(), 1);
    sim.quiesce().unwrap();
    for client in 0..3 {
        assert_eq!(sim.client(client).view(), sim.server().document());
    }
    assert!(sim.server().faults().is_empty());
    assert!(sim.server().document().comments().is_empty());
    assert_comments_agree(
        sim.server().document(),
        sim.server().materializer(),
        "after pending comment refusal",
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn concurrent_sessions_converge_and_the_file_matches(
        fixture in prop::sample::select(vec![
            "jade-core/hello.docx",
            "table-cell-spacing.docx",
            "comment.docx",
            "footnote-table-textbox-order.docx",
            "footnote.docx",
            "textbox.docx",
            "floating-wrap-sides.docx",
            "review.docx",
            "review-kinds.docx",
        ]),
        steps in steps(),
    ) {
        let (model, jade) = open(fixture);
        let mut sim = Simulation::with_materializer(model, jade, 3);
        // The server names the first two replicas (as it does for a token
        // with a name); the third edits anonymously.
        for (client, name) in [(0, "Ann"), (1, "Bob")] {
            let replica = sim.client(client).id();
            sim.commit_origin(DocxOp::SetAuthor { replica, name: name.into() })
                .expect("the server names a replica");
        }
        for step in &steps {
            match *step {
                Step::Local { client, choice } => {
                    let view = sim.client(client).view().clone();
                    let me: ReplicaId = sim.client(client).id();
                    let ctx = context(me, choice);
                    let op = generate_as(&view, choice, &ctx, &mut |n| sim.client_mut(client).reserve(n));
                    if let Some(op) = op {
                        let _ = sim.local(client, op);
                    }
                }
                Step::Up { client } => {
                    sim.deliver_up(client);
                }
                Step::Down { client } => {
                    sim.deliver_down(client).unwrap();
                }
                Step::Restart => {
                    let bytes = sim.server().materializer().export_base().unwrap();
                    let checkpoint = sim.checkpoint();
                    let jade = JadeDocx::load(&bytes).unwrap();
                    sim.restart(Sequencer::restore(checkpoint, jade)).unwrap();
                }
            }
            for client in 0..3 {
                prop_assert_eq!(sim.client(client).view(), &sim.client(client).rebuilt_view());
            }
        }
        sim.quiesce().unwrap();
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "materialization faults: {:?}", server.faults());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
        }
        assert_stories_agree(server.document(), server.materializer(), "after the session");
        assert_comments_agree(server.document(), server.materializer(), "after the session");
        assert_revisions_agree(server.document(), server.materializer(), "after the session");
        assert_formatting_agrees(server.document(), server.materializer(), "after the session");
        let reloaded = JadeDocx::load(&server.materializer().export(server.document()).unwrap()).unwrap();
        assert_same_order(server.document(), &reloaded, "against the reloaded file");
        let reloaded_model = DocxModel::from_word_document(reloaded.word()).unwrap();
        assert_same_order(&reloaded_model, &reloaded, "after reloading");
        prop_assert_eq!(model_marks(&reloaded_model), model_marks(server.document()));
        prop_assert_eq!(jade_comments(&reloaded).len(), server.document().comments().len());
    }
}
