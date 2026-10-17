#![allow(dead_code)]

use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, JadeDocx, StoryId};

pub fn jade_core() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core")
}

pub fn fixture(name: &str) -> Vec<u8> {
    let root = jade_core();
    // "jade-core/x.docx" is at the submodule root; bare names are fixtures.
    let path = if let Some(rest) = name.strip_prefix("jade-core/") {
        root.join(rest)
    } else {
        root.join("public/fixtures").join(name)
    };
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Loads a fixture into jade and builds the model from it.
pub fn open(name: &str) -> (DocxModel, JadeDocx) {
    let jade = JadeDocx::load(&fixture(name)).expect("jade loads the fixture");
    let model = DocxModel::from_word_document(jade.word()).expect("model builds");
    (model, jade)
}

/// Turns a random `choice` into an operation on `view`. `ids(n)` reserves `n`
/// consecutive identifiers. Returns `None` when the choice does not fit the
/// document (e.g. no tables to edit).
pub fn generate(view: &DocxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<DocxOp> {
    let paragraphs = view.paragraphs();
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let words = ["x", "日本", "é", "Lorem ", "  ", "ab"];
    match choice % 16 {
        // Typing at a random caret, through the index-addressed intent API.
        0..=4 => {
            let p = pick(paragraphs.len(), 8);
            let text = &paragraphs[p].text;
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let offset = boundaries[pick(boundaries.len(), 16)];
            let op = IntentOp::InsertText {
                paragraph: p,
                offset,
                text: words[pick(words.len(), 24)].to_string(),
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Deleting a few characters somewhere.
        5..=6 => {
            let p = pick(paragraphs.len(), 8);
            let atoms = &paragraphs[p].chars;
            if atoms.is_empty() {
                return None;
            }
            // Up to four characters, `stride` apart: a stride over 1 deletes
            // several separate ranges in one operation.
            let start = pick(atoms.len(), 16);
            let len = 1 + pick(4, 24);
            let stride = 1 + pick(3, 28);
            let ids: Vec<Id> = atoms[start..]
                .iter()
                .step_by(stride)
                .take(len)
                .copied()
                .collect();
            Some(DocxOp::DeleteText {
                story: paragraphs[p].story,
                ids,
            })
        }
        7..=8 => {
            let p = pick(paragraphs.len(), 8);
            let text = &paragraphs[p].text;
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let op = IntentOp::InsertParagraphBreak {
                paragraph: p,
                offset: boundaries[pick(boundaries.len(), 16)],
                count: 1,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        9 => {
            let marked: Vec<_> = paragraphs.iter().filter(|p| p.mark.is_some()).collect();
            if marked.is_empty() {
                return None;
            }
            let p = marked[pick(marked.len(), 8)];
            Some(DocxOp::JoinParagraph {
                story: p.story,
                mark: p.mark.unwrap(),
            })
        }
        10 => {
            let marks: Vec<Id> = paragraphs
                .iter()
                .filter(|p| p.story == StoryId::Body)
                .filter_map(|p| p.mark)
                .collect();
            let anchor = if marks.is_empty() || pick(4, 8) == 0 {
                Anchor::Start
            } else {
                Anchor::After(marks[pick(marks.len(), 12)])
            };
            let rows = 1 + pick(2, 20) as u64;
            let columns = 1 + pick(3, 24) as u64;
            let first = ids(1 + rows + columns);
            Some(DocxOp::InsertTable {
                anchor,
                id: first,
                rows: (1..=rows).map(|k| first.offset(k)).collect(),
                columns: (1..=columns).map(|k| first.offset(rows + k)).collect(),
            })
        }
        11..=14 => {
            let tables = view.tables();
            let editable: Vec<_> = tables.iter().filter(|t| !t.locked).collect();
            if editable.is_empty() {
                return None;
            }
            let table = editable[pick(editable.len(), 8)];
            let row = table.rows[pick(table.rows.len(), 12)];
            let column = table.columns[pick(table.columns.len(), 16)];
            let op = match choice % 16 {
                11 => GridOp::InsertRow {
                    id: ids(1),
                    after: (pick(3, 20) != 0).then_some(row),
                },
                12 => GridOp::DeleteRow { id: row },
                13 => GridOp::InsertColumn {
                    id: ids(1),
                    after: (pick(3, 20) != 0).then_some(column),
                },
                _ => GridOp::DeleteColumn { id: column },
            };
            Some(DocxOp::Table {
                table: table.id,
                op,
            })
        }
        _ => {
            let tables = view.tables();
            if tables.is_empty() || pick(3, 8) != 0 {
                return None;
            }
            Some(DocxOp::DeleteTable {
                table: tables[pick(tables.len(), 12)].id,
            })
        }
    }
}
