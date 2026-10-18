//! The published interface: server instructions and the `tools/list`
//! catalogue. Every argument and result field is described, and the edit
//! variants mirror `IntentOp` and `XlsxIntent` (a test builds a payload
//! from each advertised schema and checks the intent APIs accept it).

use serde_json::{json, Value};

/// Usage notes returned from `initialize`.
pub(crate) const INSTRUCTIONS: &str = "\
jade-collab-mcp lets you co-edit .docx and .xlsx documents hosted by a jade-collab server, \
live, alongside people and other agents. You join a document as a replica: your edits apply \
to your view at once and the server orders them with everyone else's.

Workflow: `list_documents` (given the server's http:// address) names the documents and their \
ws:// URLs; `join_document` returns a `session_id`; `read_document` shows your current view; \
`apply_edits` submits edits and waits for the server's verdict on each; `wait_for_changes` \
blocks until someone else's edit arrives; `leave_document` ends the session.

Addressing: docx `paragraph` is a zero-based index over the body's paragraphs, table cells \
included in document order (notes and text boxes excluded); `offset` is a UTF-8 byte offset on a \
character boundary. xlsx cells use A1 references, 1-based rows and column letters. Indices shift \
as the document changes, so read before editing; the edits of one call are resolved in order, \
each against the view the previous ones left.

Outcomes: `applied` (committed by the server), `rejected` (the document refused it — e.g. a \
table between two paragraphs, an Excel table's header row — with a reason), `invalid` (it could \
not be understood or does not address anything in your view), `pending` (no verdict before the \
timeout; it may still commit).";

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": description })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn object(description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// One `type`-tagged edit.
fn edit(name: &str, description: &str, mut properties: Value, required: &[&str]) -> Value {
    properties["type"] = json!({
        "type": "string",
        "const": name,
        "description": "Which edit this is.",
    });
    let mut required: Vec<&str> = required.to_vec();
    required.insert(0, "type");
    let mut schema = object(description, properties, &required);
    schema["title"] = json!(name);
    schema
}

fn cell_value() -> Value {
    let kind = |kind: &str| json!({ "type": "string", "const": kind, "description": "The kind of value." });
    let variant = |name: &str, value: Value| {
        json!({
            "type": "object",
            "title": name,
            "properties": { "kind": kind(name), "value": value },
            "required": ["kind", "value"],
            "additionalProperties": false,
        })
    };
    json!({
        "description": "The value to store, tagged by `kind`. Formulas may start with `=`; empty text clears the cell.",
        "anyOf": [
            variant("number", json!({ "type": "number", "description": "The number." })),
            variant("text", json!({ "type": "string", "description": "The text." })),
            variant("bool", json!({ "type": "boolean", "description": "The boolean." })),
            variant("formula", json!({ "type": "string", "description": "The formula, e.g. `=SUM(B2:B4)`." })),
            {
                "type": "object",
                "title": "blank",
                "properties": { "kind": kind("blank") },
                "required": ["kind"],
                "additionalProperties": false,
            },
        ],
    })
}

fn edits() -> Value {
    let paragraph = || integer("Zero-based paragraph index in your current view.");
    let offset = || integer("UTF-8 byte offset inside the paragraph, on a character boundary.");
    let sheet = || string("The sheet's name.");
    json!({
        "type": "array",
        "description": "Edits applied in order, each resolved against the view the previous ones left. Use the docx kinds for a .docx session and the xlsx kinds for a .xlsx one; anything else is reported as invalid.",
        "items": {
            "description": "One edit.",
            "anyOf": [
                edit("insert_text", "docx: insert `text` at a caret.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "text": string("Text to insert (no paragraph breaks; use insert_paragraph_break)."),
                }), &["paragraph", "offset", "text"]),
                edit("replace_range", "docx: replace a range inside one story (the body or one table cell) with `text`; crossing paragraph ends joins them.", json!({
                    "start_paragraph": integer("Paragraph where the range starts."),
                    "start_offset": integer("Byte offset where the range starts."),
                    "end_paragraph": integer("Paragraph where the range ends."),
                    "end_offset": integer("Byte offset where the range ends (exclusive)."),
                    "text": string("Replacement text; empty deletes the range."),
                }), &["start_paragraph", "start_offset", "end_paragraph", "end_offset", "text"]),
                edit("delete_text", "docx: delete `count` characters from a caret; crossing a paragraph end joins the paragraphs.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "direction": {
                        "type": "string",
                        "enum": ["backward", "forward"],
                        "description": "Delete before the caret (`backward`, the default) or after it.",
                    },
                    "count": integer("Characters (Unicode scalar values) to delete; default 1."),
                }), &["paragraph", "offset"]),
                edit("insert_paragraph_break", "docx: split the paragraph at a caret.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "count": integer("Breaks to insert; default 1."),
                }), &["paragraph", "offset"]),
                edit("set_cell", "xlsx: store a value or formula in a cell.", json!({
                    "sheet": sheet(),
                    "reference": string("The cell, in A1 form (`B3`)."),
                    "value": cell_value(),
                }), &["sheet", "reference", "value"]),
                edit("insert_row", "xlsx: insert a row that becomes row `before`; the rows from there move down.", json!({
                    "sheet": sheet(),
                    "before": json!({ "type": "integer", "minimum": 1, "description": "1-based row number the new row takes." }),
                }), &["sheet", "before"]),
                edit("delete_row", "xlsx: delete a row; the rows below move up.", json!({
                    "sheet": sheet(),
                    "row": json!({ "type": "integer", "minimum": 1, "description": "1-based row number." }),
                }), &["sheet", "row"]),
                edit("insert_column", "xlsx: insert a column that becomes column `before`; the columns from there move right.", json!({
                    "sheet": sheet(),
                    "before": string("Column letters the new column takes (`C`)."),
                }), &["sheet", "before"]),
                edit("delete_column", "xlsx: delete a column; the columns to its right move left.", json!({
                    "sheet": sheet(),
                    "column": string("Column letters (`C`)."),
                }), &["sheet", "column"]),
            ],
        },
    })
}

fn session_id() -> Value {
    string("The session returned by join_document.")
}

fn timeout_ms(default: u64) -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "description": format!("How long to wait, in milliseconds (default {default}, at most {}).", crate::MAX_TIMEOUT_MS),
    })
}

fn seq() -> Value {
    integer("The last position of the document's log this session has received.")
}

fn pending() -> Value {
    integer("Your operations the server has not answered yet.")
}

fn next() -> Value {
    json!({
        "type": ["integer", "null"],
        "description": "`start` of the next page, or null after the last one.",
    })
}

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    input: Value,
    output: Value,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_documents",
            title: "List hosted documents",
            description: "List the documents a jade-collab server hosts, with the ws:// URL to join each.",
            input: object("Arguments.", json!({
                "server": string("The server's address, `http://host:port`."),
            }), &["server"]),
            output: object("The hosted documents.", json!({
                "documents": {
                    "type": "array",
                    "description": "One entry per document, by name.",
                    "items": object("A hosted document.", json!({
                        "name": string("The document's name."),
                        "format": string("`docx` or `xlsx`."),
                        "url": string("The ws:// URL join_document takes."),
                    }), &["name", "format", "url"]),
                },
            }), &["documents"]),
            read_only: true,
            destructive: false,
            idempotent: true,
        },
        Tool {
            name: "join_document",
            title: "Join a document",
            description: "Join a hosted document as a new replica and return the `session_id` the other tools take. The format comes from the document's name.",
            input: object("Arguments.", json!({
                "url": string("The document's socket, `ws://host:port/documents/<name>/ws` (as list_documents gives it)."),
            }), &["url"]),
            output: object("The new session.", json!({
                "session_id": session_id(),
                "document": string("The document's name."),
                "format": string("`docx` or `xlsx`."),
                "replica": integer("The replica id the server gave this session."),
                "seq": seq(),
            }), &["session_id", "document", "format", "replica", "seq"]),
            read_only: false,
            destructive: false,
            idempotent: false,
        },
        Tool {
            name: "read_document",
            title: "Read a document",
            description: "Your current view, a page at a time. docx: paragraphs (index, text, which story — body or a table cell) and the top-level tables; xlsx: every sheet's size and its non-blank cells in A1 form, paged over all sheets in order. Your own unanswered edits are included.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "start": integer("First paragraph (docx) or cell (xlsx) to list; default 0."),
                "limit": json!({
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("How many to list; default {}.", crate::DEFAULT_LIMIT),
                }),
            }), &["session_id"]),
            output: object("The view.", json!({
                "format": string("`docx` or `xlsx`."),
                "seq": seq(),
                "pending": pending(),
                "start": integer("The `start` this page begins at."),
                "next": next(),
                "paragraph_count": integer("docx: how many paragraphs there are."),
                "paragraphs": {
                    "type": "array",
                    "description": "docx: the paragraphs of this page.",
                    "items": object("A paragraph.", json!({
                        "index": integer("Its index, as edits address it."),
                        "text": string("Its text."),
                        "story": {
                            "type": "object",
                            "description": "Where it is: `{kind: body}`, `{kind: table_cell, table, row, column}` (zero-based), or `{kind: nested_table_cell}`.",
                        },
                    }), &["index", "text", "story"]),
                },
                "tables": {
                    "type": "array",
                    "description": "docx: the top-level tables in order.",
                    "items": object("A table.", json!({
                        "index": integer("Its order among top-level tables."),
                        "rows": integer("Row count."),
                        "columns": integer("Column count."),
                        "locked": json!({ "type": "boolean", "description": "Merged, ragged or nested: its text is editable, its structure is not." }),
                    }), &["index", "rows", "columns", "locked"]),
                },
                "cell_count": integer("xlsx: how many non-blank cells all sheets hold."),
                "sheets": {
                    "type": "array",
                    "description": "xlsx: every sheet, with the cells of this page.",
                    "items": object("A sheet.", json!({
                        "name": string("Its name."),
                        "rows": integer("Rows in the editable grid."),
                        "columns": integer("Columns in the editable grid."),
                        "cells": {
                            "type": "object",
                            "description": "A1 reference → value (`{kind, value}`, as set_cell takes it).",
                        },
                    }), &["name", "rows", "columns", "cells"]),
                },
            }), &["format", "seq", "pending", "start", "next"]),
            read_only: true,
            destructive: false,
            idempotent: true,
        },
        Tool {
            name: "apply_edits",
            title: "Apply edits",
            description: "Submit edits and wait for the server's verdict on each: `applied`, `rejected` (with the reason), `invalid` (with the error), or `pending` if the timeout passed first. Edits are applied in order against your view as it evolves; a failed one does not stop the rest.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "edits": edits(),
                "timeout_ms": timeout_ms(crate::DEFAULT_TIMEOUT_MS),
            }), &["session_id", "edits"]),
            output: object("What became of each edit.", json!({
                "results": {
                    "type": "array",
                    "description": "One entry per edit, in order.",
                    "items": object("An edit's outcome.", json!({
                        "index": integer("The edit's position in `edits`."),
                        "status": json!({
                            "type": "string",
                            "enum": ["applied", "rejected", "invalid", "pending"],
                            "description": "What became of it.",
                        }),
                        "reason": string("rejected: why the document refused it."),
                        "error": string("invalid: what is wrong with it."),
                    }), &["index", "status"]),
                },
                "seq": seq(),
                "pending": pending(),
            }), &["results", "seq", "pending"]),
            read_only: false,
            destructive: true,
            idempotent: false,
        },
        Tool {
            name: "wait_for_changes",
            title: "Wait for changes",
            description: "Wait until this session has received log entries after `after_seq` (someone's edit, or your own) or the timeout passes. Pass the `seq` of your last read to watch for other people's edits.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "after_seq": integer("Return once the session's seq is greater than this."),
                "timeout_ms": timeout_ms(crate::DEFAULT_TIMEOUT_MS),
            }), &["session_id", "after_seq"]),
            output: object("Where the session is now.", json!({
                "changed": json!({ "type": "boolean", "description": "Whether seq is now greater than after_seq." }),
                "seq": seq(),
            }), &["changed", "seq"]),
            read_only: true,
            destructive: false,
            idempotent: true,
        },
        Tool {
            name: "leave_document",
            title: "Leave a document",
            description: "Disconnect a session. Edits already submitted are still decided by the server.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
            }), &["session_id"]),
            output: object("The session that ended.", json!({
                "session_id": session_id(),
                "left": json!({ "type": "boolean", "description": "Always true." }),
                "unanswered": integer("Edits the server had not answered when the session left."),
            }), &["session_id", "left", "unanswered"]),
            read_only: false,
            destructive: false,
            idempotent: false,
        },
    ]
}

/// The `tools/list` payload.
pub(crate) fn catalogue() -> Value {
    Value::Array(
        tools()
            .into_iter()
            .map(|tool| {
                let mut input = tool.input;
                let mut output = tool.output;
                for schema in [&mut input, &mut output] {
                    schema.as_object_mut().unwrap().remove("description");
                }
                output["additionalProperties"] = json!(true);
                json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": input,
                    "outputSchema": output,
                    "annotations": {
                        "title": tool.title,
                        "readOnlyHint": tool.read_only,
                        "destructiveHint": tool.destructive,
                        "idempotentHint": tool.idempotent,
                        // Every tool talks to the collaboration server.
                        "openWorldHint": true,
                    },
                })
            })
            .collect(),
    )
}
