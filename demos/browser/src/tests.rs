use super::*;
use jade_collab_sync::{protocol::Submission, Sequencer, SubmitOutcome};
use jade_docx_collab::{DocxModel, DocxOp};
use proptest::prelude::*;
use serde_json::{json, Value};

fn join(server: &mut Sequencer<DocxModel>) -> BrowserDocument {
    let welcome = server.join();
    BrowserDocument::new(&serde_json::to_string(&welcome).unwrap()).unwrap()
}

fn submit(
    server: &mut Sequencer<DocxModel>,
    clients: &mut [BrowserDocument],
    who: usize,
    wire: &str,
) {
    let messages: Vec<String> = serde_json::from_str(wire).unwrap();
    for message in messages {
        let submission: Submission<DocxOp> = serde_json::from_str(&message).unwrap();
        let id = clients[who].replica.id();
        let outcome = server.submit(id, submission);
        let reply = serde_json::to_string(&outcome.downstream().unwrap()).unwrap();
        for (i, client) in clients.iter_mut().enumerate() {
            if matches!(outcome, SubmitOutcome::Committed(_)) || i == who {
                client.receive(&reply).unwrap();
            }
        }
    }
}

fn text(client: &BrowserDocument) -> String {
    client.replica.view().paragraphs()[0].text.clone()
}

#[test]
fn concurrent_unicode_replacements_keep_both_insertions() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["A😀B"]));
    let mut clients = [join(&mut server), join(&mut server)];
    let a = clients[0].replace(0, 1, 3, "あ").unwrap();
    let b = clients[1].replace(0, 1, 3, "乙").unwrap();
    submit(&mut server, &mut clients, 1, &b);
    submit(&mut server, &mut clients, 0, &a);
    assert_eq!(text(&clients[0]), text(&clients[1]));
    assert!(text(&clients[0]).contains('あ'));
    assert!(text(&clients[0]).contains('乙'));
    assert!(!text(&clients[0]).contains('😀'));
}

#[test]
fn invalid_utf16_boundary_and_xml_leave_identifiers_and_tickets_unchanged() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["😀"]));
    let mut client = join(&mut server);
    let before = client.state().unwrap();
    assert!(client.replace(0, 1, 1, "x").is_err());
    assert!(client.replace(0, 0, 0, "\0").is_err());
    assert_eq!(client.state().unwrap(), before);
    assert_eq!(client.pending().unwrap(), "[]");
    let messages: Value = serde_json::from_str(&client.replace(0, 0, 0, "ok").unwrap()).unwrap();
    let first: Value = serde_json::from_str(messages[0].as_str().unwrap()).unwrap();
    assert_eq!(first["client_seq"], 1);
}

#[test]
fn resume_after_lost_ack_does_not_duplicate_typing() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&[""]));
    let mut client = join(&mut server);
    let messages: Vec<String> =
        serde_json::from_str(&client.replace(0, 0, 0, "再接続").unwrap()).unwrap();
    for m in messages {
        server.submit(client.replica.id(), serde_json::from_str(&m).unwrap());
    }
    let resume = server
        .resume(client.replica.id(), client.replica.seq())
        .unwrap();
    let mut wire = serde_json::to_value(resume).unwrap();
    wire["type"] = json!("resumed");
    client.receive(&wire.to_string()).unwrap();
    assert_eq!(text(&client), "再接続");
    assert_eq!(client.pending().unwrap(), "[]");
}

#[test]
fn outgoing_wire_preserves_identifiers_above_javascript_safe_integer() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["x"]));
    let mut welcome = server.join();
    welcome.replica = jade_collab_core::id::ReplicaId(9_007_199_254_740_993);
    let mut client = BrowserDocument::new(&serde_json::to_string(&welcome).unwrap()).unwrap();
    let outgoing: Vec<String> =
        serde_json::from_str(&client.replace(0, 1, 1, "😀").unwrap()).unwrap();
    assert!(outgoing[0].contains("9007199254740993"));
    assert_eq!(
        outgoing,
        serde_json::from_str::<Vec<String>>(&client.pending().unwrap()).unwrap()
    );
}

#[test]
fn refusal_rolls_back_optimistic_text_and_keeps_connection_usable() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["seed"]));
    let mut client = join(&mut server);
    client.replace(0, 4, 4, "pending").unwrap();
    let reasons = client
        .receive(r#"{"type":"rejected","client_seq":1,"reason":{"message":"viewer"}}"#)
        .unwrap();
    assert!(reasons.contains("viewer"));
    assert_eq!(text(&client), "seed");
    assert!(client.replace(0, 0, 0, "next").is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn stateful_delayed_delivery_converges(actions in prop::collection::vec((any::<bool>(), any::<bool>(), "[a-zあ😀]{1,4}"), 1..50)) {
        let mut server = Sequencer::new(DocxModel::from_paragraphs(&["seed"]));
        let mut clients = [join(&mut server), join(&mut server)];
        for (replace, reverse, value) in actions {
            let mut batch = Vec::new();
            for client in &mut clients {
                let len = text(client).encode_utf16().count();
                batch.push(client.replace(0, if replace { 0 } else { len }, len, &value).unwrap());
            }
            for who in if reverse { [1, 0] } else { [0, 1] } {
                submit(&mut server, &mut clients, who, &batch[who]);
            }
            prop_assert_eq!(text(&clients[0]), text(&clients[1]));
            prop_assert_eq!(clients[0].replica.pending_len(), 0);
            prop_assert_eq!(clients[1].replica.pending_len(), 0);
        }
    }
}

#[test]
fn peer_selection_survives_deleting_both_adjacent_characters() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["ab😀cdef"]));
    let mut clients = [join(&mut server), join(&mut server)];
    let cursor = clients[0].capture_selection(0, 4, 0, 6).unwrap();
    let wire = clients[1].replace(0, 2, 5, "").unwrap();
    submit(&mut server, &mut clients, 1, &wire);
    assert_eq!(
        clients[0].resolve_selection(&cursor).unwrap(),
        r#"{"anchor":[0,2],"focus":[0,3]}"#
    );
}

#[test]
fn peer_selection_is_compact_and_does_not_round_large_character_ids() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&[&"あ".repeat(20_000)]));
    let mut welcome = server.join();
    welcome.replica = jade_collab_core::id::ReplicaId(9_007_199_254_740_993);
    let mut client = BrowserDocument::new(&serde_json::to_string(&welcome).unwrap()).unwrap();
    client.replace(0, 0, 0, "😀").unwrap();
    let before = client.state().unwrap();
    let cursor = client.capture_selection(0, 2, 0, 3).unwrap();
    assert!(cursor.len() < 1500);
    assert!(cursor.contains("9007199254740993"));
    assert_eq!(
        client.resolve_selection(&cursor).unwrap(),
        r#"{"anchor":[0,2],"focus":[0,3]}"#
    );
    assert_eq!(client.state().unwrap(), before);
    assert!(client.capture_selection(0, 1, 0, 1).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn stateful_peer_caret_rebases_through_insertions_and_deletions(
        actions in prop::collection::vec((any::<u8>(),any::<u8>(),any::<bool>(),"[あ😀a-z]{0,4}"),1..70)
    ) {
        let mut server=Sequencer::new(DocxModel::from_paragraphs(&["あ😀abcdef"]));
        let mut clients=[join(&mut server),join(&mut server)];
        for (a,b,insert,value) in actions {
            let old=text(&clients[0]);
            let boundaries: Vec<_>=std::iter::once(0).chain(old.chars().scan(0,|n,c|{*n+=c.len_utf16();Some(*n)})).collect();
            let start=boundaries[a as usize % boundaries.len()];
            let end=if insert { start } else { boundaries[b as usize % boundaries.len()].max(start) };
            let at=boundaries[(a as usize+b as usize)%boundaries.len()];
            let saved=clients[0].capture_selection(0,at,0,at).unwrap();
            let wire=clients[1].replace(0,start,end,&value).unwrap();
            submit(&mut server,&mut clients,1,&wire);
            let resolved: Value=serde_json::from_str(&clients[0].resolve_selection(&saved).unwrap()).unwrap();
            prop_assert_ne!(&resolved, &Value::Null);
            prop_assert_eq!(&resolved["anchor"], &resolved["focus"]);
            let offset=resolved["focus"][1].as_u64().unwrap() as usize;
            prop_assert!(utf16_byte(&text(&clients[0]),offset).is_ok());
            if end < at {prop_assert_eq!(offset,at-(end-start)+value.encode_utf16().count());}
            if start > at {prop_assert_eq!(offset,at);}
        }
    }
}

#[test]
fn presence_waits_for_its_edit_and_keeps_server_identity_exact() {
    let mut server = Sequencer::new(DocxModel::from_paragraphs(&["x"]));
    let mut clients = [join(&mut server), join(&mut server)];
    let wire = clients[0].replace(0, 0, 0, "😀").unwrap();
    let cursor = clients[0].capture_selection(0, 2, 0, 2).unwrap();
    assert_eq!(clients[1].resolve_selection(&cursor).unwrap(), "null");
    submit(&mut server, &mut clients, 0, &wire);
    assert_eq!(
        clients[1].resolve_selection(&cursor).unwrap(),
        r#"{"anchor":[0,2],"focus":[0,2]}"#
    );
    assert_eq!(
        clients[1]
            .presence_id(r#"{"replica":9007199254740993}"#)
            .unwrap(),
        "9007199254740993"
    );
}
