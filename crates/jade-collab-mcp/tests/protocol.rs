//! JSON-RPC handling and the published tool catalogue.

mod common;

use common::{Agent, Hosted, DOCX};
use jade_docx_collab::intent::IntentOp;
use jade_pptx_collab::intent::PptxIntent;
use jade_xlsx_collab::intent::XlsxIntent;
use serde_json::{json, Value};

const TOOLS: &[&str] = &[
    "list_documents",
    "join_document",
    "read_document",
    "apply_edits",
    "get_edit_status",
    "wait_for_changes",
    "leave_document",
];

#[tokio::test]
async fn initialize_echoes_the_protocol_and_advertises_tools() {
    let mut agent = Agent::new();
    let response = agent
        .request("initialize", json!({ "protocolVersion": "2025-06-18" }))
        .await;
    let result = &response["result"];
    assert_eq!(result["protocolVersion"], json!("2025-06-18"));
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(result["serverInfo"]["name"], json!("jade-collab-mcp"));
    assert!(result["instructions"]
        .as_str()
        .is_some_and(|text| text.contains("join_document")));

    let response = agent.request("initialize", json!({})).await;
    assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
    assert_eq!(
        agent.request("ping", Value::Null).await["result"],
        json!({})
    );
}

#[tokio::test]
async fn notifications_and_blank_lines_get_no_answer() {
    let mut agent = Agent::new();
    assert_eq!(agent.send("").await, None);
    assert_eq!(agent.send("   ").await, None);
    assert_eq!(
        agent
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await,
        None
    );
    // A request without an id is a notification too.
    assert_eq!(
        agent
            .send(r#"{"jsonrpc":"2.0","method":"tools/list"}"#)
            .await,
        None
    );
}

#[tokio::test]
async fn malformed_messages_are_json_rpc_errors() {
    let mut agent = Agent::new();
    let response = agent.send("{not json").await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32700));
    assert_eq!(response["id"], Value::Null);

    let response = agent.send(r#"{"jsonrpc":"2.0","id":7}"#).await.unwrap();
    assert_eq!(response["error"]["code"], json!(-32600));
    assert_eq!(response["id"], json!(7));

    let response = agent.request("resources/list", json!({})).await;
    assert_eq!(response["error"]["code"], json!(-32601));

    for params in [Value::Null, json!([]), json!({ "name": 3 })] {
        let response = agent.request("tools/call", params).await;
        assert_eq!(response["error"]["code"], json!(-32602), "{response}");
    }
    let (code, message) = agent.rpc_error("format_disk", json!({})).await;
    assert_eq!(code, -32602);
    assert!(message.contains("format_disk"), "{message}");
}

fn listed(response: &Value) -> Vec<Value> {
    response["result"]["tools"].as_array().unwrap().clone()
}

fn assert_every_property_is_described(schema: &Value, path: &str) {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            let here = format!("{path}.{name}");
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|d| !d.trim().is_empty()),
                "{here} has no description"
            );
            assert_every_property_is_described(property, &here);
        }
        for required in schema["required"].as_array().into_iter().flatten() {
            assert!(
                properties.contains_key(required.as_str().unwrap()),
                "{path} requires {required}, which it does not define"
            );
        }
    }
    for key in ["items", "anyOf", "oneOf"] {
        match schema.get(key) {
            Some(Value::Array(variants)) => {
                for (i, variant) in variants.iter().enumerate() {
                    assert_every_property_is_described(variant, &format!("{path}.{key}[{i}]"));
                }
            }
            Some(inner) => assert_every_property_is_described(inner, &format!("{path}.{key}")),
            None => {}
        }
    }
}

#[tokio::test]
async fn every_tool_publishes_a_documented_interface() {
    let mut agent = Agent::new();
    let tools = listed(&agent.request("tools/list", json!({})).await);
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS);
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        for field in ["title", "description"] {
            assert!(
                tool[field].as_str().is_some_and(|t| !t.trim().is_empty()),
                "{name} has no {field}"
            );
        }
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "{name}");
        assert_eq!(
            tool["inputSchema"]["additionalProperties"],
            json!(false),
            "{name} should reject unknown arguments"
        );
        assert_eq!(tool["outputSchema"]["type"], json!("object"), "{name}");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(tool["annotations"][hint].is_boolean(), "{name}.{hint}");
        }
        assert_every_property_is_described(&tool["inputSchema"], &format!("{name}.input"));
        assert_every_property_is_described(&tool["outputSchema"], &format!("{name}.output"));
    }
    let required = |name: &str| -> Vec<String> {
        let tool = tools.iter().find(|t| t["name"] == json!(name)).unwrap();
        serde_json::from_value(tool["inputSchema"]["required"].clone()).unwrap()
    };
    assert_eq!(required("list_documents"), ["server"]);
    assert_eq!(required("join_document"), ["url"]);
    assert_eq!(required("read_document"), ["session_id"]);
    assert_eq!(required("apply_edits"), ["session_id", "edits"]);
    assert_eq!(required("get_edit_status"), ["session_id", "receipt"]);
    assert_eq!(required("wait_for_changes"), ["session_id", "after_seq"]);
    assert_eq!(required("leave_document"), ["session_id"]);
}

/// A value for a schema, using only what the schema says.
fn sample(schema: &Value) -> Value {
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    if let Some(example) = schema["examples"]
        .as_array()
        .and_then(|values| values.first())
    {
        return example.clone();
    }
    if let Some(first) = schema["enum"].as_array().and_then(|e| e.first()) {
        return first.clone();
    }
    if let Some(first) = schema["anyOf"].as_array().and_then(|v| v.first()) {
        return sample(first);
    }
    match schema["type"].as_str() {
        Some("integer") => json!(schema["minimum"].as_i64().unwrap_or(1).max(1)),
        Some("number") => json!(1.5),
        Some("boolean") => json!(true),
        Some("string") => json!("B"),
        Some("array") => json!([sample(&schema["items"])]),
        Some("object") => {
            let mut object = serde_json::Map::new();
            for name in schema["required"].as_array().into_iter().flatten() {
                let name = name.as_str().unwrap();
                object.insert(name.to_string(), sample(&schema["properties"][name]));
            }
            Value::Object(object)
        }
        other => panic!("no sample for {other:?}: {schema}"),
    }
}

#[tokio::test]
async fn pptx_geometry_schemas_allow_the_signed_coordinates_the_engine_accepts() {
    let mut agent = Agent::new();
    let tools = listed(&agent.request("tools/list", json!({})).await);
    let apply = tools.iter().find(|t| t["name"] == "apply_edits").unwrap();
    let variants = apply["inputSchema"]["properties"]["edits"]["items"]["anyOf"]
        .as_array()
        .unwrap();
    for kind in ["set_geometry", "insert_text_box", "insert_picture"] {
        let variant = variants
            .iter()
            .find(|v| {
                v["properties"]["type"]["const"] == kind
                    && v["description"].as_str().unwrap().starts_with("pptx:")
            })
            .unwrap();
        let mut payload = sample(variant);
        for coordinate in ["x", "y"] {
            assert!(
                variant["properties"][coordinate].get("minimum").is_none(),
                "{kind}.{coordinate}"
            );
            payload[coordinate] = json!(-9525);
        }
        if kind == "set_geometry" {
            assert!(variant["properties"]["rotation"].get("minimum").is_none());
            payload["rotation"] = json!(-60000);
        }
        serde_json::from_value::<PptxIntent>(payload).unwrap();
    }
}

/// The edit variants `apply_edits` advertises are exactly the intent APIs',
/// and a payload built from each advertised schema is accepted by them.
#[tokio::test]
async fn advertised_edits_match_the_intent_apis() {
    let mut agent = Agent::new();
    let tools = listed(&agent.request("tools/list", json!({})).await);
    let apply = tools
        .iter()
        .find(|t| t["name"] == json!("apply_edits"))
        .unwrap();
    let variants = apply["inputSchema"]["properties"]["edits"]["items"]["anyOf"]
        .as_array()
        .expect("edits are a list of variants");
    let mut docx = Vec::new();
    let mut xlsx = Vec::new();
    let mut pptx = Vec::new();
    for variant in variants {
        let name = variant["properties"]["type"]["const"].as_str().unwrap();
        assert_eq!(variant["additionalProperties"], json!(false), "{name}");
        let description = variant["description"].as_str().unwrap_or_default();
        let payload = sample(variant);
        // Every advertised optional property is one the API reads.
        let mut full = payload.clone();
        for (property, schema) in variant["properties"].as_object().unwrap() {
            full[property] = sample(schema);
        }
        // The description names the format; the payload must be that
        // format's intent, minimal and with every optional property.
        let format = description.split(':').next().unwrap_or_default();
        let parses = |value: &Value| match format {
            "docx" => serde_json::from_value::<IntentOp>(value.clone())
                .map(drop)
                .map_err(|e| e.to_string()),
            "xlsx" => serde_json::from_value::<XlsxIntent>(value.clone())
                .map(drop)
                .map_err(|e| e.to_string()),
            "pptx" => serde_json::from_value::<PptxIntent>(value.clone())
                .map(drop)
                .map_err(|e| e.to_string()),
            other => Err(format!("{name}: no format in {other:?}")),
        };
        parses(&payload).unwrap_or_else(|e| panic!("{name}: {payload}: {e}"));
        parses(&full).unwrap_or_else(|e| panic!("{name}: {full}: {e}"));
        match format {
            "docx" => docx.push(name.to_string()),
            "xlsx" => xlsx.push(name.to_string()),
            _ => pptx.push(name.to_string()),
        }
    }
    assert_eq!(
        pptx,
        [
            "insert_text",
            "replace_range",
            "delete_text",
            "insert_paragraph_break",
            "set_geometry",
            "insert_slide",
            "delete_slide",
            "move_slide",
            "duplicate_slide",
            "insert_text_box",
            "delete_shape",
            "format_text",
            "align_paragraphs",
            "insert_picture",
            "set_slide_hidden"
        ]
    );
    assert_eq!(
        docx,
        [
            "insert_text",
            "replace_range",
            "delete_text",
            "insert_paragraph_break",
            "format",
            "format_paragraphs",
            "add_comment",
            "reply_to_comment",
            "resolve_comment",
            "delete_comment",
            "update_comment",
            "insert_footnote",
            "insert_endnote",
            "delete_footnote",
            "delete_endnote",
            "insert_text_box",
            "delete_text_box",
            "merge_table_cells",
            "unmerge_table_cells",
            "set_tracking",
            "accept_revisions",
            "reject_revisions"
        ]
    );
    assert_eq!(
        xlsx,
        [
            "set_cell",
            "insert_row",
            "delete_row",
            "insert_column",
            "delete_column",
            "set_note",
            "delete_note",
            "set_format",
            "set_borders",
            "merge_cells",
            "unmerge_cells",
            "add_sheet",
            "rename_sheet",
            "delete_sheet",
            "move_sheet"
        ]
    );
}

/// Arguments the schema forbids never reach a tool: they are JSON-RPC
/// errors, and the server keeps answering.
#[tokio::test(flavor = "multi_thread")]
async fn invalid_arguments_are_json_rpc_errors() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let session = agent.join(&hosted.url(DOCX)).await;
    let invalid = [
        ("list_documents", json!({})),
        ("list_documents", json!({ "server": 8787 })),
        ("list_documents", json!({ "server": "ftp://127.0.0.1:21" })),
        ("join_document", json!({})),
        ("join_document", json!({ "url": ["ws://x"] })),
        (
            "join_document",
            json!({ "url": "ws://127.0.0.1:1/documents/notes.txt/ws" }),
        ),
        (
            "join_document",
            json!({ "url": "http://127.0.0.1:1/documents/a.docx/ws" }),
        ),
        ("read_document", json!({})),
        ("read_document", json!({ "session_id": 1 })),
        ("read_document", json!({ "session_id": "no-such-session" })),
        (
            "read_document",
            json!({ "session_id": session, "start": -1 }),
        ),
        (
            "read_document",
            json!({ "session_id": session, "limit": 0 }),
        ),
        ("read_document", json!({ "session_id": session, "page": 2 })),
        ("apply_edits", json!({ "session_id": session })),
        (
            "apply_edits",
            json!({ "session_id": session, "edits": "insert" }),
        ),
        (
            "apply_edits",
            json!({ "session_id": session, "edits": [], "timeout_ms": "soon" }),
        ),
        (
            "apply_edits",
            json!({ "session_id": "no-such-session", "edits": [] }),
        ),
        ("wait_for_changes", json!({ "session_id": session })),
        ("get_edit_status", json!({"session_id":session})),
        ("get_edit_status", json!({"session_id":session,"receipt":3})),
        (
            "get_edit_status",
            json!({"session_id":session,"receipt":"r","timeout_ms":-1}),
        ),
        (
            "get_edit_status",
            json!({"session_id":session,"receipt":"r","resend":true}),
        ),
        (
            "wait_for_changes",
            json!({ "session_id": session, "after_seq": -1 }),
        ),
        (
            "wait_for_changes",
            json!({ "session_id": session, "after_seq": 0, "timeout_ms": 1.5 }),
        ),
        ("leave_document", json!({ "session_id": "no-such-session" })),
        (
            "leave_document",
            json!({ "session_id": session, "force": true }),
        ),
    ];
    for (name, arguments) in invalid {
        let (code, message) = agent.rpc_error(name, arguments.clone()).await;
        assert_eq!(code, -32602, "{name} {arguments}: {message}");
        assert!(!message.is_empty());
    }
    // `arguments` that is not an object at all.
    let response = agent
        .request(
            "tools/call",
            json!({ "name": "read_document", "arguments": "s" }),
        )
        .await;
    assert_eq!(response["error"]["code"], json!(-32602));
    // The session is untouched.
    let read = agent.read_all(&session).await;
    assert_eq!(read["format"], json!("docx"));
}
