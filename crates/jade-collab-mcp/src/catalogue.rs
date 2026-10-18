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

fn docx_format_change() -> Value {
    let kind =
        |name: &str| json!({ "type": "string", "const": name, "description": "The change." });
    let variant = |name: &str, properties: Value, required: &[&str]| {
        let mut props = properties.as_object().cloned().unwrap_or_default();
        props.insert("kind".into(), kind(name));
        let mut req: Vec<&str> = vec!["kind"];
        req.extend_from_slice(required);
        json!({
            "type": "object",
            "title": name,
            "properties": props,
            "required": req,
            "additionalProperties": false,
        })
    };
    json!({
        "description": "What to change, tagged by `kind`.",
        "anyOf": [
            variant("bold", json!({ "value": { "type": "boolean", "description": "On or off." } }), &["value"]),
            variant("italic", json!({ "value": { "type": "boolean", "description": "On or off." } }), &["value"]),
            variant("underline", json!({
                "value": { "type": "boolean", "description": "On or off." },
                "style": string("Word underline style (`single`, `double`, …)."),
            }), &["value"]),
            variant("strike", json!({
                "value": { "type": "boolean", "description": "On or off." },
                "double": { "type": "boolean", "description": "Double strikethrough." },
            }), &["value"]),
            variant("font_size", json!({ "half_points": integer("Size in half points (21 is 10.5pt).") }), &["half_points"]),
            variant("font_family", json!({ "family": string("Font family name.") }), &["family"]),
            variant("font_color", json!({ "color": string("`#RRGGBB`.") }), &["color"]),
            variant("highlight", json!({ "color": string("One of Word's highlight colour names (`yellow`, …).") }), &["color"]),
            variant("clear_formatting", json!({}), &[]),
            variant("alignment", json!({ "alignment": {
                "type": "string",
                "enum": ["left", "center", "right", "justify", "distribute"],
                "description": "Paragraph alignment.",
            } }), &["alignment"]),
            variant("paragraph_style", json!({ "style_id": string("Style id; omit for the default paragraph style.") }), &[]),
        ],
    })
}

fn xlsx_format_change() -> Value {
    let flag = |what: &str| json!({ "type": "boolean", "description": what });
    json!({
        "type": "object",
        "description": "The attributes to set; the others keep what the cells have.",
        "properties": {
            "bold": flag("Bold."),
            "italic": flag("Italic."),
            "underline": flag("Underline."),
            "strike": flag("Strikethrough."),
            "font_color": string("`RRGGBB`."),
            "font_size": { "type": "number", "description": "Points, 1 to 409." },
            "font_name": string("Font name."),
            "fill_color": string("`RRGGBB`, or empty for no fill."),
            "number_format": string("Number format code (`0.00`, `yyyy/mm/dd`, `General`)."),
            "horizontal": { "type": "string", "enum": ["General", "Left", "Center", "Right"], "description": "Horizontal alignment." },
            "vertical": { "type": "string", "enum": ["Top", "Center", "Bottom"], "description": "Vertical alignment." },
            "wrap_text": flag("Wrap text."),
        },
        "required": [],
        "additionalProperties": false,
    })
}

fn edits() -> Value {
    let paragraph = || integer("Zero-based paragraph index in your current view.");
    let offset = || integer("UTF-8 byte offset inside the paragraph, on a character boundary.");
    let sheet = || string("The sheet's name.");
    let slide = || integer("The slide's number, from 1.");
    let target = || {
        json!({
            "description": "`\"notes\"` for the speaker notes, or `{\"shape\": N}` for shape N's text.",
            "anyOf": [
                { "type": "string", "const": "notes", "description": "The speaker notes." },
                object("A shape's text.", json!({ "shape": integer("The shape's number.") }), &["shape"]),
            ],
        })
    };
    json!({
        "type": "array",
        "description": "Edits applied in order, each resolved against the view the previous ones left. Use the kinds of your session's format (each description starts with docx:, xlsx: or pptx:); anything else is reported as invalid.",
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
                edit("format", "docx: change character or paragraph formatting of a range inside one story.", json!({
                    "start_paragraph": integer("Paragraph where the range starts."),
                    "start_offset": integer("Byte offset where the range starts."),
                    "end_paragraph": integer("Paragraph where the range ends."),
                    "end_offset": integer("Byte offset where the range ends (exclusive)."),
                    "change": docx_format_change(),
                }), &["start_paragraph", "start_offset", "end_paragraph", "end_offset", "change"]),
                edit("format_paragraphs", "docx: change paragraph formatting (alignment, style) of whole paragraphs.", json!({
                    "start_paragraph": integer("First paragraph."),
                    "end_paragraph": integer("Last paragraph (inclusive)."),
                    "change": docx_format_change(),
                }), &["start_paragraph", "end_paragraph", "change"]),
                edit("add_comment", "docx: comment on a range (whole paragraphs when the offsets are omitted).", json!({
                    "start_paragraph": integer("Paragraph where the range starts."),
                    "start_offset": integer("Byte offset where the range starts."),
                    "end_paragraph": integer("Paragraph where the range ends."),
                    "end_offset": integer("Byte offset where the range ends (exclusive)."),
                    "text": string("The comment."),
                    "author": string("Author name."),
                    "initials": string("Author initials."),
                    "date": string("ISO 8601 date."),
                }), &["start_paragraph", "end_paragraph", "text"]),
                edit("reply_to_comment", "docx: reply to a comment thread.", json!({
                    "comment_id": string("The comment's id (`lamport@rN`, or the file's id for a loaded comment)."),
                    "text": string("The reply."),
                    "author": string("Author name."),
                    "initials": string("Author initials."),
                    "date": string("ISO 8601 date."),
                }), &["comment_id", "text"]),
                edit("resolve_comment", "docx: mark a comment thread resolved or open.", json!({
                    "comment_id": string("The comment's id."),
                    "resolved": { "type": "boolean", "description": "Resolved or reopened." },
                }), &["comment_id", "resolved"]),
                edit("delete_comment", "docx: delete a comment (a thread root takes its replies).", json!({
                    "comment_id": string("The comment's id."),
                }), &["comment_id"]),
                edit("merge_table_cells", "docx: merge a rectangle of a top-level table's cells; the top-left cell shows their content, row by row.", json!({
                    "table": integer("The table's index (read_document lists them)."),
                    "top_row": integer("First row, zero-based."),
                    "left_column": integer("First column, zero-based."),
                    "bottom_row": integer("Last row (inclusive)."),
                    "right_column": integer("Last column (inclusive)."),
                }), &["table", "top_row", "left_column", "bottom_row", "right_column"]),
                edit("unmerge_table_cells", "docx: undo the merge whose top-left cell is (row, column); the freed cells come back empty.", json!({
                    "table": integer("The table's index."),
                    "row": integer("The merge's first row, zero-based."),
                    "column": integer("The merge's first column, zero-based."),
                }), &["table", "row", "column"]),
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
                edit("set_note", "xlsx: put a note on a cell.", json!({
                    "sheet": sheet(),
                    "reference": string("The cell (`B3`)."),
                    "text": string("The note."),
                }), &["sheet", "reference", "text"]),
                edit("delete_note", "xlsx: remove a cell's note.", json!({
                    "sheet": sheet(),
                    "reference": string("The cell (`B3`)."),
                }), &["sheet", "reference"]),
                edit("set_format", "xlsx: format a cell or range; each attribute is last-writer-wins.", json!({
                    "sheet": sheet(),
                    "range": string("`B2` or `B2:D5`."),
                    "change": xlsx_format_change(),
                }), &["sheet", "range", "change"]),
                edit("merge_cells", "xlsx: merge a range; the top-left value stays, the others are cleared.", json!({
                    "sheet": sheet(),
                    "range": string("`B2:C3`."),
                }), &["sheet", "range"]),
                edit("unmerge_cells", "xlsx: undo the merge starting at the range's top-left cell.", json!({
                    "sheet": sheet(),
                    "range": string("The merged range or its top-left cell."),
                }), &["sheet", "range"]),
                edit("add_sheet", "xlsx: add an empty sheet.", json!({
                    "name": string("The new sheet's name."),
                    "after": string("The sheet to put it after; omit for the end."),
                }), &["name"]),
                edit("rename_sheet", "xlsx: rename a sheet; formulas that name it follow.", json!({
                    "sheet": sheet(),
                    "name": string("The new name."),
                }), &["sheet", "name"]),
                edit("delete_sheet", "xlsx: delete a sheet; references into it become #REF!.", json!({
                    "sheet": sheet(),
                }), &["sheet"]),
                edit("move_sheet", "xlsx: move a sheet to another tab position.", json!({
                    "sheet": sheet(),
                    "to": integer("Tab position, 0 being the first."),
                }), &["sheet", "to"]),
                edit("insert_text", "pptx: insert `text` in a shape or the notes; a newline splits paragraphs.", json!({
                    "slide": slide(),
                    "target": target(),
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "text": string("Text to insert."),
                }), &["slide", "target", "paragraph", "offset", "text"]),
                edit("replace_range", "pptx: replace a range of a shape's text or the notes.", json!({
                    "slide": slide(),
                    "target": target(),
                    "start_paragraph": integer("Paragraph where the range starts."),
                    "start_offset": integer("Byte offset where the range starts."),
                    "end_paragraph": integer("Paragraph where the range ends."),
                    "end_offset": integer("Byte offset where the range ends (exclusive)."),
                    "text": string("Replacement text; empty deletes the range."),
                }), &["slide", "target", "start_paragraph", "start_offset", "end_paragraph", "end_offset", "text"]),
                edit("delete_text", "pptx: delete `count` characters from a caret.", json!({
                    "slide": slide(),
                    "target": target(),
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "direction": {
                        "type": "string",
                        "enum": ["forward", "backward"],
                        "description": "Delete after the caret (`forward`, the default) or before it.",
                    },
                    "count": integer("Characters to delete; default 1."),
                }), &["slide", "target", "paragraph", "offset"]),
                edit("insert_paragraph_break", "pptx: split a paragraph of a shape or the notes.", json!({
                    "slide": slide(),
                    "target": target(),
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "count": integer("Breaks to insert; default 1."),
                }), &["slide", "target", "paragraph", "offset"]),
                edit("set_geometry", "pptx: move, resize or rotate a shape; each attribute is last-writer-wins.", json!({
                    "slide": slide(),
                    "shape": integer("The shape's number (read_document lists them)."),
                    "x": integer("Left, in EMU."),
                    "y": integer("Top, in EMU."),
                    "width": integer("Width, in EMU."),
                    "height": integer("Height, in EMU."),
                    "rotation": integer("Rotation, in 1/60000 degree."),
                }), &["slide", "shape"]),
                edit("insert_slide", "pptx: insert a slide that becomes slide `at`.", json!({
                    "at": integer("The new slide's number (from 1)."),
                }), &["at"]),
                edit("delete_slide", "pptx: delete a slide.", json!({ "slide": slide() }), &["slide"]),
                edit("move_slide", "pptx: move a slide so it becomes slide `to`.", json!({
                    "slide": slide(),
                    "to": integer("Its new number (from 1)."),
                }), &["slide", "to"]),
                edit("duplicate_slide", "pptx: copy a slide right after it.", json!({ "slide": slide() }), &["slide"]),
                edit("insert_text_box", "pptx: add a text box.", json!({
                    "slide": slide(),
                    "x": integer("Left, in EMU."),
                    "y": integer("Top, in EMU."),
                    "width": integer("Width, in EMU."),
                    "height": integer("Height, in EMU."),
                    "text": string("Its text."),
                }), &["slide", "x", "y", "width", "height"]),
                edit("delete_shape", "pptx: delete a shape.", json!({
                    "slide": slide(),
                    "shape": integer("The shape's number."),
                }), &["slide", "shape"]),
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
                "token": string("Access token, when the server requires one (defaults to $JADE_COLLAB_TOKEN)."),
            }), &["server"]),
            output: object("The hosted documents.", json!({
                "documents": {
                    "type": "array",
                    "description": "One entry per document, by name.",
                    "items": object("A hosted document.", json!({
                        "name": string("The document's name."),
                        "format": string("`docx`, `xlsx` or `pptx`."),
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
                "token": string("Access token, when the server requires one (defaults to $JADE_COLLAB_TOKEN)."),
            }), &["url"]),
            output: object("The new session.", json!({
                "session_id": session_id(),
                "document": string("The document's name."),
                "format": string("`docx`, `xlsx` or `pptx`."),
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
            description: "Your current view, a page at a time. docx: paragraphs (index, text, which story — body or a table cell) and the top-level tables; xlsx: every sheet's size and its non-blank cells in A1 form, paged over all sheets in order; pptx: slides (numbered from 1) with their shapes' numbers, text and geometry and the speaker notes. Your own unanswered edits are included.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "start": integer("First paragraph (docx), cell (xlsx) or slide (pptx, zero-based) to list; default 0."),
                "limit": json!({
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("How many to list; default {}.", crate::DEFAULT_LIMIT),
                }),
            }), &["session_id"]),
            output: object("The view.", json!({
                "format": string("`docx`, `xlsx` or `pptx`."),
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
                        "locked": json!({ "type": "boolean", "description": "Ragged or nested: its text is editable, its structure is not." }),
                        "merges": {
                            "type": "array",
                            "description": "Merged regions: `{top, left, bottom, right}` zero-based rows and columns.",
                        },
                    }), &["index", "rows", "columns", "locked", "merges"]),
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
                "slide_count": integer("pptx: how many slides there are."),
                "slides": {
                    "type": "array",
                    "description": "pptx: the slides of this page.",
                    "items": object("A slide.", json!({
                        "slide": integer("Its number, as edits address it (from 1)."),
                        "shapes": {
                            "type": "array",
                            "description": "Its shapes: `shape` (the number edits take), `kind`, `text` (null when jade cannot edit it), `editable`, `deletable`, `geometry` (EMU; rotation in 1/60000 degree).",
                        },
                        "notes": string("Speaker notes, paragraphs joined with newlines."),
                        "notes_editable": json!({ "type": "boolean", "description": "Whether the notes can be edited." }),
                    }), &["slide", "shapes", "notes", "notes_editable"]),
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
