//! The published interface: server instructions and the `tools/list`
//! catalogue. Every argument and result field is described, and the edit
//! variants mirror `IntentOp`, `XlsxIntent` and `PptxIntent` (a test builds a payload
//! from each advertised schema and checks the intent APIs accept it).

use jade_xlsx_collab::BorderStyle;
use serde_json::{json, Value};

/// Usage notes returned from `initialize`.
pub(crate) const INSTRUCTIONS: &str = "\
jade-collab-mcp lets you co-edit .docx, .xlsx and .pptx documents hosted by a jade-collab server, \
live, alongside people and other agents. You join a document as a replica: your edits apply \
to your view at once and the server orders them with everyone else's.

Workflow: `list_documents` (given the server's http:// or https:// address) names the documents and their \
ws:// or wss:// URLs; `join_document` returns a `session_id`; `read_document` shows your current view; \
`apply_edits` submits edits and waits for the server's verdict on each; `get_edit_status` queries its receipt; `wait_for_changes` \
blocks until someone else's edit arrives; `leave_document` ends the session.

Addressing: docx `paragraph` is a zero-based index over every paragraph `read_document` lists, \
in its order: the body's paragraphs with table cells row by row, each text box's paragraphs right \
after the paragraph that anchors it, each footnote's after the paragraph that first references it, \
then the other footnotes and the endnotes; each paragraph says which story it belongs to. `offset` \
is a UTF-8 byte offset on a character boundary; text boxes and note references take no text \
(a paragraph lists where they sit). xlsx cells use A1 references, 1-based rows and column letters. Indices shift \
as the document changes, so pass read_document's snapshot token to apply_edits to refuse \
edits when the local replica has changed since that read; read again after refusal. \
The edits of one accepted call are resolved in order, \
each against the view the previous ones left.

Outcomes: `applied` (committed by the server), `rejected` (the document refused it — e.g. a \
table between two paragraphs, an Excel table's header row — with a reason), `invalid` (it could \
not be understood or does not address anything in your view), `pending` (no verdict before the \
timeout; it may still commit). Use apply_edits receipt with get_edit_status to learn late verdicts without submitting again. Receipts are held only in this process/session; an expired receipt or a connection error does not mean the edit failed.";

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": description })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn signed_integer(description: &str) -> Value {
    json!({"type":"integer","description":description})
}

fn pptx_char_format() -> Value {
    let variant = |attribute: &str, properties: Value, required: &[&str]| {
        let mut properties = properties.as_object().unwrap().clone();
        properties.insert(
            "attribute".into(),
            json!({"type":"string","const":attribute,"description":"The character attribute."}),
        );
        let mut fields = vec!["attribute"];
        fields.extend_from_slice(required);
        object(
            "One explicit character attribute.",
            Value::Object(properties),
            &fields,
        )
    };
    json!({"description":"One character attribute, tagged by attribute. Different attributes compose; the newest write wins for the same attribute.","anyOf":[
        variant("bold",json!({"value":{"type":"boolean","description":"Bold on or off."}}), &["value"]),
        variant("italic",json!({"value":{"type":"boolean","description":"Italic on or off."}}), &["value"]),
        variant("underline",json!({"value":{"type":"boolean","description":"Underline on or off."}}), &["value"]),
        variant("size",json!({"hundredths":{"type":"integer","minimum":100,"maximum":400000,"description":"Font size in hundredths of a point: 1800 is 18pt."}}), &["hundredths"]),
        variant("font",json!({"typeface":string("Latin typeface.")}), &["typeface"]),
        variant("east_asian_font",json!({"typeface":string("East Asian typeface.")}), &["typeface"]),
        variant("color",json!({"color":string("Colour as #RRGGBB.")}), &["color"]),
    ]})
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

fn docx_character_change() -> Value {
    let mut schema = docx_format_change();
    schema["description"] = json!("One explicit collaborative character attribute. Missing attributes inherit the file's base formatting.");
    schema["anyOf"].as_array_mut().unwrap().retain(|v| {
        matches!(
            v["title"].as_str(),
            Some(
                "bold"
                    | "italic"
                    | "underline"
                    | "strike"
                    | "font_size"
                    | "font_family"
                    | "font_color"
                    | "highlight"
            )
        )
    });
    schema
}

fn native_character_style() -> Value {
    let flag = |description| json!({"type":"boolean","description":description});
    let text = |description| json!({"type":["string","null"],"description":description});
    object("Resolved editable Word run properties: document defaults, paragraph style, character style, direct properties and collaborative marks. Unresolved font/colour values remain null; this does not describe rendered theme or conditional appearance.", json!({
        "bold":flag("Bold."), "italic":flag("Italic."),
        "bold_complex":flag("Complex-script bold, independently of bold."),
        "italic_complex":flag("Complex-script italic, independently of italic."),
        "underline":flag("Underline."), "underline_style":text("Underline kind; null with enabled underline means single."),
        "underline_color":text("Underline colour token, possibly auto; null follows the text colour."),
        "strike":flag("Single strikethrough."), "double_strike":flag("Double strikethrough."),
        "font_size":json!({"type":["number","null"],"description":"Font size in points; format edits use half_points instead."}),
        "font_family":text("Resolved font family, or null when unresolved."),
        "font_color":text("Resolved colour token, possibly auto; null when unresolved."),
        "highlight":text("Named Word highlight colour, or null if unset."),
        "character_style":text("Declared character style id, or null."),
    }), &["bold","italic","bold_complex","italic_complex","underline","underline_style","underline_color","strike","double_strike","font_size","font_family","font_color","highlight","character_style"])
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
            "border_left": border_line("Every cell's left edge."),
            "border_right": border_line("Every cell's right edge."),
            "border_top": border_line("Every cell's top edge."),
            "border_bottom": border_line("Every cell's bottom edge."),
            "border_diagonal": border_line("Every cell's diagonal line (shown in the directions diagonal_up / diagonal_down turn on)."),
            "diagonal_up": flag("Show the diagonal from bottom-left to top-right."),
            "diagonal_down": flag("Show the diagonal from top-left to bottom-right."),
        },
        "required": [],
        "additionalProperties": false,
    })
}

/// One border line: style and colour, last-writer-wins together per edge.
fn border_line(what: &str) -> Value {
    let styles: Vec<&str> = BorderStyle::ALL.iter().map(|s| s.as_str()).collect();
    json!({
        "type": "object",
        "description": format!("{what} `none` removes the line."),
        "properties": {
            "style": { "type": "string", "enum": styles, "description": "Line style, as Excel names it." },
            "color": string("`RRGGBB`; omit for Excel's automatic colour."),
        },
        "required": ["style"],
        "additionalProperties": false,
    })
}

fn xlsx_range_borders() -> Value {
    json!({
        "type": "object",
        "description": "Lines over the range, as Excel's border dialog draws them; an edge of its own wins over `outline`, a direction of its own over `inside`.",
        "properties": {
            "outline": border_line("The range's four outer edges."),
            "inside": border_line("The lines between its cells, both ways."),
            "top": border_line("The range's top edge."),
            "bottom": border_line("The range's bottom edge."),
            "left": border_line("The range's left edge."),
            "right": border_line("The range's right edge."),
            "inside_horizontal": border_line("The lines between its rows."),
            "inside_vertical": border_line("The lines between its columns."),
            "diagonal": border_line("Every cell's diagonal line."),
            "diagonal_up": { "type": "boolean", "description": "Show the diagonal from bottom-left to top-right." },
            "diagonal_down": { "type": "boolean", "description": "Show the diagonal from top-left to bottom-right." },
        },
        "required": [],
        "additionalProperties": false,
    })
}

fn edits() -> Value {
    let paragraph = || integer("Zero-based paragraph index in your current view.");
    let offset = || integer("UTF-8 byte offset inside the paragraph, on a character boundary.");
    let sheet = || string("The sheet's name.");
    let slide = || {
        let mut value = integer("The slide's number, from 1.");
        value["minimum"] = json!(1);
        value
    };
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
                edit("replace_range", "docx: replace a range inside one story (the body, one table cell, note or text box) with `text`; crossing paragraph ends joins them.", json!({
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
                edit("format", "docx: change character or paragraph formatting of a range (a range crossing a table formats every story it crosses). Bold, italic, underline, strike, font size and family, colour and highlight apply at once and never conflict: concurrent changes of one attribute settle on the newest everywhere, and text typed at the end of the range takes the formatting, text typed before it does not. `clear_formatting` (keeps the highlight) and paragraph changes go through the server and can be rejected.", json!({
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
                edit("update_comment", "docx: replace a comment's text (a thread root or a reply).", json!({
                    "comment_id": string("The comment's id."),
                    "text": string("The new text: no line breaks, nor whitespace at either end."),
                }), &["comment_id", "text"]),
                edit("insert_footnote", "docx: add a footnote at a caret of a body paragraph or table cell (before any text box or note reference already at that offset). Its paragraph is listed after the one that first references it; its text starts with a tab after the number, as Word's does.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "text": string("The note's text (no line breaks); default empty."),
                }), &["paragraph", "offset"]),
                edit("insert_endnote", "docx: add an endnote at a caret of a body paragraph or table cell, like insert_footnote; endnotes are listed after everything else.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "text": string("The note's text (no line breaks); default empty."),
                }), &["paragraph", "offset"]),
                edit("delete_footnote", "docx: delete a footnote and every reference to it.", json!({
                    "footnote_id": string("The footnote's number (`note` in read_document's anchors and stories)."),
                }), &["footnote_id"]),
                edit("delete_endnote", "docx: delete an endnote and every reference to it.", json!({
                    "endnote_id": string("The endnote's number (`note` in read_document's anchors and stories)."),
                }), &["endnote_id"]),
                edit("insert_text_box", "docx: add a text box (a rectangle holding one paragraph) at a caret of a body paragraph or table cell where no text box or note reference sits; its paragraphs are listed right after the one that anchors it.", json!({
                    "paragraph": paragraph(),
                    "offset": offset(),
                    "text": string("The box's text (no line breaks); default empty."),
                    "width_px": integer("Width in pixels; default 216."),
                    "height_px": integer("Height in pixels; default 96."),
                }), &["paragraph", "offset"]),
                edit("delete_text_box", "docx: delete a text box with its content and the comments in it.", json!({
                    "paragraph": integer("The paragraph that anchors it."),
                    "text_box": integer("Which of that paragraph's text boxes (`text_box` in its anchors), zero-based."),
                }), &["paragraph", "text_box"]),
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
                edit("set_tracking", "docx: switch Track Changes on or off for the whole document. While it is on, typing and deleting become revisions authored by your token's name (deleting your own tracked insertion removes it; text already deleted is left as it is); paragraph breaks, joins, tables, formatting and comments are not tracked.", json!({
                    "on": json!({ "type": "boolean", "description": "Tracking on (true) or off." }),
                }), &["on"]),
                edit("accept_revisions", "docx: accept whole revisions: an insertion becomes plain text, a deletion's text goes. Name them by id (read_document lists them), or take those touching a paragraph range, or (neither) all; a revision someone makes meanwhile is not included.", json!({
                    "revision_ids": json!({ "type": "array", "items": { "type": "string" }, "description": "Revision ids (`id` or the file's `file_id`, as read_document lists them)." }),
                    "start_paragraph": integer("First paragraph whose revisions to take."),
                    "end_paragraph": integer("Last paragraph whose revisions to take (inclusive)."),
                }), &[]),
                edit("reject_revisions", "docx: reject whole revisions: an insertion's text goes, a deletion's text stays. Chosen as for accept_revisions.", json!({
                    "revision_ids": json!({ "type": "array", "items": { "type": "string" }, "description": "Revision ids (`id` or the file's `file_id`, as read_document lists them)." }),
                    "start_paragraph": integer("First paragraph whose revisions to take."),
                    "end_paragraph": integer("Last paragraph whose revisions to take (inclusive)."),
                }), &[]),
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
                edit("set_format", "xlsx: format a cell, a range, or whole rows or columns; each attribute is last-writer-wins, and a cell shows the newest of its own, its row's and its column's.", json!({
                    "sheet": sheet(),
                    "range": string("`B2` or `B2:D5`; whole rows `3:5` or columns `B:D` (also the cells they get later)."),
                    "change": xlsx_format_change(),
                }), &["sheet", "range", "change"]),
                edit("set_borders", "xlsx: draw border lines over a range (outline, inside lines, single edges, diagonals); each cell's edge is last-writer-wins.", json!({
                    "sheet": sheet(),
                    "range": string("`B2` or `B2:D5`."),
                    "borders": xlsx_range_borders(),
                }), &["sheet", "range", "borders"]),
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
                    "x": signed_integer("Left, in EMU; negative positions are allowed."),
                    "y": signed_integer("Top, in EMU; negative positions are allowed."),
                    "width": integer("Width, in EMU."),
                    "height": integer("Height, in EMU."),
                    "rotation": signed_integer("Rotation, in 1/60000 degree; negative rotates counterclockwise."),
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
                    "x": signed_integer("Left, in EMU; negative positions are allowed."),
                    "y": signed_integer("Top, in EMU; negative positions are allowed."),
                    "width": integer("Width, in EMU."),
                    "height": integer("Height, in EMU."),
                    "text": string("Its text."),
                }), &["slide", "x", "y", "width", "height"]),
                edit("delete_shape", "pptx: delete a shape.", json!({
                    "slide": slide(),
                    "shape": integer("The shape's number."),
                }), &["slide", "shape"]),
                edit("format_text", "pptx: apply one character attribute to a shape's UTF-8 range. The newest write wins per attribute; typing at the end inherits it. Notes cannot be formatted by this operation.", json!({
                    "slide":slide(), "shape":integer("The shape number from read_document."),
                    "start_paragraph":paragraph(), "start_offset":offset(), "end_paragraph":paragraph(), "end_offset":offset(),
                    "format":pptx_char_format(),
                }), &["slide","shape","start_paragraph","start_offset","end_paragraph","end_offset","format"]),
                edit("align_paragraphs", "pptx: align whole paragraphs of a shape; the range is inclusive and judged by the server.", json!({
                    "slide":slide(), "shape":integer("The shape number from read_document."),
                    "start_paragraph":paragraph(), "end_paragraph":paragraph(),
                    "alignment":{"type":"string","enum":["left","center","right","justify","distributed"],"description":"Paragraph alignment."},
                }), &["slide","shape","start_paragraph","end_paragraph","alignment"]),
                edit("insert_picture", "pptx: insert a PNG or JPEG picture on top of the slide. Omit both dimensions to use its pixel size at 96 dpi; one dimension preserves the image ratio. Later geometry edits respect its aspect lock.", json!({
                    "slide":slide(), "x":signed_integer("Left, in EMU."), "y":signed_integer("Top, in EMU."),
                    "width":integer("Optional width in EMU."), "height":integer("Optional height in EMU."),
                    "image":json!({"type":"string","contentEncoding":"base64","description":"PNG or JPEG bytes, encoded as standard base64, at most 8 MiB decoded.","examples":["iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="]}),
                }), &["slide","x","y","image"]),
                edit("set_slide_hidden", "pptx: hide a slide from the slide show or show it again. It remains in reads and keeps its slide number.", json!({
                    "slide":slide(), "hidden":{"type":"boolean","description":"True to hide, false to show."},
                }), &["slide","hidden"]),
            ],
        },
    })
}

fn pptx_native_paragraph() -> Value {
    let style = object("Resolved authoring character properties before display autofit; theme colours and font references are resolved.", json!({
        "bold":{"type":"boolean","description":"Resolved bold, including explicit false."},
        "italic":{"type":"boolean","description":"Resolved italic, including explicit false."},
        "underline":{"type":"boolean","description":"Resolved underline, including explicit false."},
        "size_pt":{"type":"number","exclusiveMinimum":0,"description":"Authoring font size in points, before autofit."},
        "font":{"type":["string","null"],"description":"Resolved Latin font, or null when unspecified."},
        "east_asian_font":{"type":["string","null"],"description":"Resolved East Asian font, or null when unspecified."},
        "color":string("Resolved colour, such as #A020F0."),
    }), &["bold","italic","underline","size_pt","font","east_asian_font","color"]);
    let spacing = json!({"type":["object","null"],"description":"Spacing as {mode: percent, value: 100} for single spacing or {mode: points, value: 6}; null when unspecified.","properties":{"mode":{"type":"string","enum":["percent","points"],"description":"Whether the value is a percentage or points."},"value":{"type":"number","description":"Amount in the specified units."}},"required":["mode","value"]});
    let paragraph = object("The native paragraph containing this text segment; soft-break segments share paragraph properties.", json!({
        "alignment":{"type":["string","null"],"enum":["left","center","right","justify","distributed",null],"description":"Resolved horizontal paragraph alignment."},
        "bullet":{"type":["string","null"],"enum":["none","character","numbered",null],"description":"Resolved bullet type."},
        "character":{"type":["string","null"],"description":"Bullet character for character bullets."},
        "scheme":{"type":["string","null"],"description":"Numbering scheme for numbered bullets."},
        "start_at":{"type":["integer","null"],"description":"Restart value for a numbered paragraph, or null."},
        "bullet_font":{"type":["string","null"],"description":"Resolved bullet font, or null when unspecified."},
        "level":{"type":["integer","null"],"description":"Zero-based outline level."},
        "margin_left_pt":{"type":["number","null"],"description":"Left paragraph margin in points, or null when unspecified."},
        "margin_right_pt":{"type":["number","null"],"description":"Right paragraph margin in points, or null when unspecified."},
        "indent_pt":{"type":["number","null"],"description":"First-line indent in points; negative means hanging. Null when unspecified."},
        "line_spacing":spacing, "space_before":spacing, "space_after":spacing,
    }), &["alignment","bullet","character","scheme","start_at","bullet_font","level","margin_left_pt","margin_right_pt","indent_pt","line_spacing","space_before","space_after"]);
    object("With include_base_formatting: one zero-based paragraph of the shape's newline-separated text.", json!({
        "index":integer("Paragraph number used by text edits."),
        "text":string("Text without its terminating newline."),
        "paragraph_format":paragraph,
        "resolved_formatting":{"type":"array","description":"Contiguous maximal UTF-8 runs partitioning this paragraph; an empty paragraph has no runs.","items":object("A run with one resolved style.",json!({"start":integer("First UTF-8 byte."),"end":integer("Exclusive end UTF-8 byte."),"style":style}),&["start","end","style"])},
    }), &["index","text","paragraph_format","resolved_formatting"])
}

fn pptx_shape_read() -> Value {
    object(
        "One addressable shape.",
        json!({
            "shape":integer("The shape number edits take."),
            "kind":string("shape, picture, group, table or chart."),
            "text":json!({"type":["string","null"],"description":"Text with paragraphs joined by newlines; null when there is no text body."}),
            "editable":json!({"type":"boolean","description":"Whether its text can be edited."}),
            "deletable":json!({"type":"boolean","description":"Whether the shape can be deleted."}),
            "aspect_locked":json!({"type":"boolean","description":"Width/height changes retain the aspect ratio and travel through sequenced resize."}),
            "group":json!({"type":["integer","null"],"description":"The nearest addressable enclosing group's shape number on this slide, or null when none is known."}),
            "geometry":json!({"type":["object","null"],"description":"Top-level placement: x/y/width/height in EMU and rotation in 1/60000 degree; null when it cannot be placed."}),
            "formatting":{
                "type":"array","description":"Collaborative character overrides, ordered by paragraph, attribute, then byte offset. Runs of different attributes may overlap; the file's base formatting is not included.",
                "items":object("A maximal run of one character attribute.", json!({
                    "paragraph":integer("Zero-based paragraph in this shape's text."),
                    "start":integer("First UTF-8 byte within that paragraph."),
                    "end":integer("Exclusive end UTF-8 byte within that paragraph."),
                    "format":pptx_char_format(),
                }), &["paragraph","start","end","format"]),
            },
            "paragraphs":{"type":"array","description":"Present for text bodies with include_base_formatting: authoritative resolved character and paragraph properties. Omitted for pictures/groups/tables/charts and shapes without a text body. Notes are plain text only.","items":pptx_native_paragraph()},
        }),
        &[
            "shape",
            "kind",
            "text",
            "editable",
            "deletable",
            "aspect_locked",
            "group",
            "geometry",
            "formatting",
        ],
    )
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

fn receipt_output() -> Value {
    object(
        "Retained outcomes for one accepted apply_edits call, with live session progress.",
        json!({
            "receipt":string("Opaque get_edit_status token; session- and MCP-instance-scoped."),
            "complete":{"type":"boolean","description":"Every submitted operation in this batch has been answered. Invalid edits and no-ops have no unanswered operations."},
            "unresolved":integer("Submitted operations in this batch whose verdict is still unknown; zero when complete."),
            "results":{"type":"array","description":"One entry per edit, in input order.","items":object("One edit's outcome.",json!({
                "index":integer("Position in the original edits array."),
                "status":{"type":"string","enum":["applied","rejected","invalid","pending"],"description":"Applied when all its operations committed, rejected when any refused, invalid when parsing/resolution failed, or pending without a verdict. No-op edits are applied."},
                "reason":string("Rejected: why the edit was refused."),
                "error":string("Invalid: what is wrong with the edit."),
            }), &["index","status"])},
            "seq":seq(), "pending":pending(),
            "connection_error":string("The connection failed while verdicts were outstanding. Pending means unknown, not rejected; a submitted edit may have committed."),
        }),
        &[
            "receipt",
            "complete",
            "unresolved",
            "results",
            "seq",
            "pending",
        ],
    )
}

fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_documents",
            title: "List hosted documents",
            description: "List the documents a jade-collab server hosts, with the ws:// or wss:// URL to join each.",
            input: object("Arguments.", json!({
                "server": string("The server's address, `http://host:port` or `https://host:port`. HTTPS discovery returns wss:// socket URLs."),
                "token": string("Access token, when the server requires one (defaults to $JADE_COLLAB_TOKEN)."),
            }), &["server"]),
            output: object("The hosted documents.", json!({
                "documents": {
                    "type": "array",
                    "description": "One entry per document, by name.",
                    "items": object("A hosted document.", json!({
                        "name": string("The document's name."),
                        "format": string("`docx`, `xlsx` or `pptx`."),
                        "url": string("The ws:// or wss:// socket URL join_document takes."),
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
                "url": string("The document's socket URL from list_documents. The name is one percent-encoded UTF-8 path segment; preserve its encoding. A ?token= query is supported."),
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
            description: "Read a page of your live view, or pass the snapshot token from a previous read to keep that same immutable view across pages. docx: paragraphs (index, text, which story — body, a table cell, a text box, a footnote or an endnote — where its text boxes and note references sit, and which bytes tracked changes cover: deleted text stays in `text` until the deletion is accepted), explicit collaborative character-format spans (UTF-8 offsets; base and paragraph styles are not included), the comments, the top-level tables, whether Track Changes is on and every revision with its author; xlsx: every sheet's size and default style, with one sparse page across cells (value, note and resolved base format), row styles, column styles and merge ranges, in sheet order. Follow next until null; entry_count counts these items and cell_count counts non-blank values. Pass the same snapshot on every page for a stable walk while edits continue; pptx: slides (numbered from 1) with their shapes' numbers, text, collaborative formatting, geometry, aspect locks and enclosing group numbers, plus speaker notes and the slide's hidden flag. Hidden slides remain in the read order. For DOCX and PPTX, include_base_formatting captures the committed server model with resolved native character and paragraph styles at the same seq (PPTX shape text only; notes remain plain text); this view is authoritative and pending is zero. Default live reads include your own unanswered edits. The process retains one snapshot per session: reading without snapshot replaces it, and leaving removes it. Pinned reads keep seq and pending as captured; apply_edits and wait_for_changes continue to use the live replica.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "snapshot": string("Optional opaque token returned by read_document. Omit for a fresh live view, replacing this session's previous snapshot. Pass it unchanged for subsequent pages or retries; expired/foreign-session tokens fail explicitly."),
                "include_base_formatting": json!({"type":"boolean","description":"DOCX and PPTX, default false. Capture the server's committed model and resolved native character/paragraph formatting at one log position. Earlier queued edits are processed first; pending is zero and view is authoritative. On later pages omit this option or keep its original value; a snapshot's mode cannot change."}),
                "start": integer("First paragraph (docx), sparse entry (xlsx: cell, row style, column style or merge), or slide (pptx), zero-based; default 0."),
                "limit": json!({
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("How many to list; default {}.", crate::DEFAULT_LIMIT),
                }),
            }), &["session_id"]),
            output: object("The captured view.", json!({
                "snapshot": string("Opaque session-scoped token for this retained immutable read view. Pass it to read_document on subsequent pages."),
                "format": string("`docx`, `xlsx` or `pptx`."),
                "view": json!({"type":"string","const":"authoritative","description":"Present for include_base_formatting: the server's committed view at seq, excluding local edits submitted after the capture request."}),
                "seq": integer("Replication sequence at snapshot capture."),
                "pending": integer("Unanswered optimistic operations included at snapshot capture."),
                "start": integer("The `start` this page begins at."),
                "next": next(),
                "paragraph_count": integer("docx: how many paragraphs there are."),
                "formatting_scope": json!({
                    "type": "string", "const": "collaborative_overrides",
                    "description": "docx/pptx: formatting lists explicit collaborative character overrides, including false values. It excludes file base styles and paragraph styles; an omitted attribute inherits the base and does not mean false. DOCX clears remove overrides except highlight.",
                }),
                "paragraphs": {
                    "type": "array",
                    "description": "docx: the paragraphs of this page.",
                    "items": object("A paragraph.", json!({
                        "index": integer("Its index, as edits address it."),
                        "text": string("Its text."),
                        "resolved_formatting": {
                            "type":"array",
                            "description":"With include_base_formatting: contiguous maximal runs of the same resolved native style. They partition this paragraph's UTF-8 text; empty paragraphs have no runs.",
                            "items": object("A native character style run.", json!({
                                "start":integer("First UTF-8 byte."), "end":integer("Exclusive end UTF-8 byte."),
                                "style":native_character_style(),
                            }), &["start","end","style"]),
                        },
                        "paragraph_format": object("With include_base_formatting: the native paragraph style and formatting.", json!({
                            "style_id":json!({"type":["string","null"],"description":"Declared paragraph style id; null uses the document default."}),
                            "alignment":json!({"type":["string","null"],"description":"Paragraph alignment token, or null when unset."}),
                            "indent":json!({"type":["object","null"],"description":"Indent in points: nullable left, right, first_line and hanging."}),
                            "spacing":json!({"type":["object","null"],"description":"Nullable before/after spacing in points, line, and line_rule (auto: line is a multiplier; exact/at_least: points)."}),
                        }), &["style_id","alignment","indent","spacing"]),
                        "formatting": {
                            "type": "array",
                            "description": "Explicit collaborative character changes, as maximal UTF-8 byte runs per attribute. Runs of different attributes may overlap; runs of one attribute are disjoint. Ordered by bold, italic, underline, strike, font size, family, colour, highlight, then start. The file's base formatting is not included; an empty array does not mean unformatted text.",
                            "items": object("One character attribute over a half-open byte range of text.", json!({
                                "start": integer("First byte of the run, on a UTF-8 boundary."),
                                "end": integer("Exclusive end byte, on a UTF-8 boundary."),
                                "change": docx_character_change(),
                            }), &["start", "end", "change"]),
                        },
                        "story": {
                            "type": "object",
                            "description": "Where it is: `{kind: body}`, `{kind: table_cell, table, row, column}` (zero-based), or `{kind: nested_table_cell}`.",
                        },
                        "revisions": {
                            "type": "array",
                            "description": "Tracked changes in it, if any: `{kind: insertion|deletion, revision, start, end}` (UTF-8 byte offsets into `text`).",
                        },
                    }), &["index", "text", "story", "formatting"]),
                },
                "comments": {
                    "type": "array",
                    "description": "docx: every live comment, oldest first.",
                    "items": object("A comment.", json!({
                        "id": string("The id edits name it by (`lamport@rN`)."),
                        "file_id": string("Its `w:id` in the file (edits take it too), or null while the server has not numbered it yet."),
                        "reply_to": string("The id of the thread's root, or null for a root."),
                        "text": string("Its text."),
                        "author": string("Its author, or null."),
                        "resolved": json!({ "type": "boolean", "description": "Whether its thread is resolved." }),
                    }), &["id", "file_id", "reply_to", "text", "author", "resolved"]),
                },
                "tracking": json!({ "type": "boolean", "description": "docx: whether Track Changes is on." }),
                "revisions": {
                    "type": "array",
                    "description": "docx: every tracked change with text left, by id.",
                    "items": object("A revision.", json!({
                        "id": string("Its id, as accept_revisions and reject_revisions take it."),
                        "kind": string("`insertion` or `deletion`."),
                        "author": json!({ "type": ["string", "null"], "description": "Who made it (the token's name); null if anonymous." }),
                        "date": json!({ "type": ["string", "null"], "description": "When (`w:date`); may be null." }),
                        "file_id": json!({ "type": ["string", "null"], "description": "Its `w:id` if it came with the file; null otherwise." }),
                        "moved": json!({ "type": "boolean", "description": "A move (moveFrom / moveTo) from the file." }),
                        "text": string("Its text."),
                        "paragraphs": json!({ "type": "array", "items": { "type": "integer" }, "description": "The paragraphs holding it." }),
                    }), &["id", "kind", "text", "paragraphs"]),
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
                "entry_count": integer("xlsx: how many sparse read items all sheets hold. start/next index these items, including blank cells with styles or notes, axis styles and merge geometry."),
                "sheets": {
                    "type": "array",
                    "description": "xlsx: every sheet, with only this page's cells, axis styles and merge geometry. Default style and dimensions are repeated metadata.",
                    "items": object("A sheet.", json!({
                        "name": string("Its name."),
                        "rows": integer("Rows in the editable grid."),
                        "columns": integer("Columns in the editable grid."),
                        "cells": {
                            "type": "object",
                            "description": "A1 reference → value (`{kind, value}`, as set_cell takes it).",
                        },
                        "formats": {
                            "type": "object",
                            "description": "A1 reference → the known resolved base format of a cell on this page, including cells with values, explicit styles or merge anchors. Shares one page item with its value. Conditional styles are not included; covered cells use the anchor named by a merge range.",
                        },
                        "notes": {
                            "type": "object",
                            "description": "A1 reference → legacy note text for cells on this page, including notes on blank cells. Shares the cell's page item with its value and format.",
                        },
                        "merges": {
                            "type": "array",
                            "description": "Merged A1 ranges on this page. Each range occupies one item; covered coordinates are not expanded. The top-left cell is a separate cell item with the resolved format.",
                            "items": string("An inclusive A1 range, e.g. E1:F1."),
                        },
                        "default_format": json!({
                            "type": "object",
                            "description": "The imported default cell style, before explicit cell/row/column styles and subsequent collaborative changes. Unresolved attributes are omitted.",
                        }),
                        "row_formats": {
                            "type": "object",
                            "description": "Row number (`3`) → the imported intentional row style and collaborative changes to that row; only row-style items on this page.",
                        },
                        "column_formats": {
                            "type": "object",
                            "description": "Column letter (`B`) → the imported column style and collaborative changes to that column; only column-style items on this page.",
                        },
                    }), &["name", "rows", "columns", "cells", "formats", "default_format", "row_formats", "column_formats", "merges", "notes"]),
                },
                "slide_count": integer("pptx: how many slides there are."),
                "slides": {
                    "type": "array",
                    "description": "pptx: the slides of this page.",
                    "items": object("A slide.", json!({
                        "slide": integer("Its number, as edits address it (from 1)."),
                        "hidden": json!({"type":"boolean","description":"Skipped in the slide show; it still appears in reads and keeps its number."}),
                        "shapes": {
                            "type": "array",
                            "description": "Its shapes, including group children, in depth-first order.",
                            "items": pptx_shape_read(),
                        },
                        "notes": string("Speaker notes, paragraphs joined with newlines."),
                        "notes_editable": json!({ "type": "boolean", "description": "Whether the notes can be edited." }),
                    }), &["slide", "hidden", "shapes", "notes", "notes_editable"]),
                },
            }), &["format", "seq", "pending", "start", "next", "snapshot"]),
            read_only: true,
            destructive: false,
            idempotent: true,
        },
        Tool {
            name: "apply_edits",
            title: "Apply edits",
            description: "Submit edits and wait for the server's verdict on each: `applied`, `rejected` (with the reason), `invalid` (with the error), or `pending` if the timeout passed first. The receipt token permits later get_edit_status calls without resending. Up to 64 receipts are retained per session; completed ones may expire, and a full cache of unanswered receipts refuses a new call before allocation/submission. Connection loss preserves known results and reports unanswered operations as pending with connection_error. Pass snapshot from read_document to require that this session's local replica is unchanged since that read; a stale, expired or foreign token refuses the whole call before allocation or submission. The version check, edit resolution and queueing hold one replica lock. Without snapshot, use the current local view. Once accepted, edits run in order against the evolving view; a failed edit does not stop the rest or roll back earlier edits. This is a local version precondition, not a server-wide transaction.",
            input: object("Arguments.", json!({
                "session_id": session_id(),
                "edits": edits(),
                "snapshot": string("Optional read_document token. Refuse the entire call if the local replica changed since capture. Read again and reconsider edits after refusal. The token remains readable; submitting an operation makes it stale for further editing."),
                "timeout_ms": timeout_ms(crate::DEFAULT_TIMEOUT_MS),
            }), &["session_id", "edits"]),
            output: receipt_output(),
            read_only: false,
            destructive: true,
            idempotent: false,
        },
        Tool {
            name: "get_edit_status",
            title: "Get edit status",
            description: "Look up an apply_edits receipt without resubmitting edits. A receipt is scoped to its session and this MCP process. Default timeout is zero; optionally wait for that batch's submitted operations, including rejections that do not advance seq. Later batches do not delay it. Complete means every submitted operation in the batch has an answer; a rejected edit may still have other submitted operations pending. Unknown outcomes stay pending if the connection fails, with connection_error; they may have committed. Up to 64 receipts are retained per session; only completed ones may be evicted, and leaving removes them. An expired receipt does not establish failure.",
            input: object("Arguments.",json!({
                "session_id":session_id(),
                "receipt":string("The opaque token returned by apply_edits in this session."),
                "timeout_ms":timeout_ms(0),
            }), &["session_id","receipt"]),
            output:receipt_output(),
            read_only:true,
            destructive:false,
            idempotent:true,
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
