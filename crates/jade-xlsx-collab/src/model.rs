use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::grid::{Grid, GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::lww::LwwRegister;
use jade_collab_sync::{ApplyContext, Document, Lane, Rejection};
use jade_xlsx_headless_api::{CellInput, CellKind, CellView};
use jade_xlsx_headless_editor::{FormatChange, HeadlessWorkbook};
use jade_xlsx_reader::{
    shift_formula, Axis, HorizontalAlignment, StructureChange, VerticalAlignment,
};
use serde::{Deserialize, Serialize};

/// How much of each sheet the model covers: the used range and its merged
/// regions, at least this big (so there is room to type), at most
/// [`LoadOptions::MAX_ROWS`] × [`LoadOptions::MAX_COLUMNS`]. Cells outside
/// are left to jade untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadOptions {
    pub min_rows: u32,
    pub min_columns: u32,
}

impl LoadOptions {
    pub const MAX_ROWS: u32 = 10_000;
    /// With `MAX_ROWS`, keeps a sheet within jade's one-million-cell range read.
    pub const MAX_COLUMNS: u32 = 100;
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            min_rows: 100,
            min_columns: 26,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read sheet {sheet}: {message}")]
    Sheet { sheet: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Sheet {
    id: Id,
    name: String,
    grid: Grid,
    #[serde(with = "pairs")]
    cells: BTreeMap<(Id, Id), LwwRegister<CellInput>>,
    /// Cell notes (legacy comments); `None` once deleted.
    #[serde(with = "pairs")]
    notes: BTreeMap<(Id, Id), LwwRegister<Option<String>>>,
    /// The format attributes written through the model. What the file had
    /// is left to jade: an attribute nobody wrote here is not modelled.
    #[serde(with = "pairs")]
    formats: BTreeMap<(Id, Id), CellFormat>,
    /// Header and totals cells of Excel tables. Excel rewrites what is typed
    /// there (a blank header becomes "Column1"), so they are read-only.
    protected: BTreeSet<(Id, Id)>,
    /// Excel tables. Rows and columns cannot be inserted or deleted inside
    /// one: Excel would name new columns and fill calculated ones.
    tables: Vec<TableRegion>,
}

/// Calls `$then!` with every attribute of jade's [`FormatChange`]. The
/// users destructure and build `FormatChange` without `..`, so an attribute
/// jade adds is a compile error here rather than one silently dropped.
macro_rules! each_attribute {
    ($then:ident) => {
        $then!(
            bold,
            italic,
            underline,
            strike,
            font_color,
            font_size,
            font_name,
            fill_color,
            number_format,
            horizontal,
            vertical,
            wrap_text
        )
    };
}

/// One cell's format as a last-writer-wins register per attribute.
///
/// jade's `set_format` is a partial change — every attribute it does not
/// name keeps what the cell has — so attributes are independent, and a
/// register each makes concurrent changes to different attributes of one
/// cell both survive while each attribute keeps its newest write. Registers
/// commute and are keyed by row and column ids, so formatting is
/// convergent: it never depends on where in the log it lands. jade sees,
/// in log order, exactly the writes that won when they arrived; the last of
/// those for an attribute is the newest, so jade ends where the registers do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CellFormat {
    bold: LwwRegister<bool>,
    italic: LwwRegister<bool>,
    underline: LwwRegister<bool>,
    strike: LwwRegister<bool>,
    font_color: LwwRegister<String>,
    font_size: LwwRegister<f32>,
    font_name: LwwRegister<String>,
    fill_color: LwwRegister<String>,
    number_format: LwwRegister<String>,
    horizontal: LwwRegister<HorizontalAlignment>,
    vertical: LwwRegister<VerticalAlignment>,
    wrap_text: LwwRegister<bool>,
}

impl CellFormat {
    /// Writes every attribute `change` names with `stamp`; returns the ones
    /// that won.
    fn apply(&mut self, stamp: Id, change: &FormatChange) -> FormatChange {
        macro_rules! apply {
            ($($name:ident),*) => {{
                let FormatChange { $($name),* } = change;
                FormatChange {
                    $($name: match $name {
                        Some(value) if self.$name.apply(stamp, value.clone()) => Some(value.clone()),
                        _ => None,
                    },)*
                }
            }};
        }
        each_attribute!(apply)
    }

    fn current(&self) -> FormatChange {
        macro_rules! current {
            ($($name:ident),*) => {
                FormatChange { $($name: self.$name.get().cloned(),)* }
            };
        }
        each_attribute!(current)
    }
}

/// `change` as jade's `format_at` reads it back — colours as upper-case
/// `RRGGBB`, General for an empty or any-case `general` number format — or
/// why `set_format` would refuse it. Checked on every replica, from the
/// operation alone, so a convergent write never reaches a server whose jade
/// cannot realise it.
fn canonical_format(change: &FormatChange) -> Result<FormatChange, Rejection> {
    fn rgb(text: &str) -> Result<String, Rejection> {
        let hex = text.strip_prefix('#').unwrap_or(text);
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(hex.to_ascii_uppercase())
        } else {
            Err(reject(format!("{text:?} is not an RRGGBB colour")))
        }
    }
    if change.is_empty() {
        return Err(reject("a format change has to name an attribute"));
    }
    let mut change = change.clone();
    if let Some(color) = &mut change.font_color {
        *color = rgb(color)?;
    }
    if let Some(color) = change.fill_color.as_mut().filter(|c| !c.is_empty()) {
        *color = rgb(color)?;
    }
    if let Some(code) = &mut change.number_format {
        if code.is_empty() || code.eq_ignore_ascii_case("General") {
            *code = "General".to_string();
        }
    }
    // Excel's limits, which jade enforces.
    if change
        .font_size
        .is_some_and(|size| !size.is_finite() || !(1.0..=409.0).contains(&size))
    {
        return Err(reject("font sizes run from 1 to 409 points"));
    }
    if change.font_name.as_deref().is_some_and(str::is_empty) {
        return Err(reject("a font needs a name"));
    }
    Ok(change)
}

/// Cells and the attributes each won, as few `set_format` calls as equal
/// changes allow: runs along each row, stacked while consecutive rows have
/// the same run. The rectangles are disjoint, so their order is immaterial.
fn rectangles(sheet: &str, cells: BTreeMap<(usize, usize), FormatChange>) -> Vec<XlsxEffect> {
    let mut runs: Vec<(usize, usize, usize, FormatChange)> = Vec::new();
    for ((r, c), change) in cells {
        match runs.last_mut() {
            Some((row, _, right, last)) if *row == r && *right + 1 == c && *last == change => {
                *right = c
            }
            _ => runs.push((r, c, c, change)),
        }
    }
    // (top, bottom, left, right, change)
    let mut rectangles: Vec<(usize, usize, usize, usize, FormatChange)> = Vec::new();
    for (r, left, right, change) in runs {
        let above = rectangles.iter_mut().find(|(_, bottom, l, rt, c)| {
            *bottom + 1 == r && *l == left && *rt == right && *c == change
        });
        match above {
            Some(rectangle) => rectangle.1 = r,
            None => rectangles.push((r, r, left, right, change)),
        }
    }
    rectangles
        .into_iter()
        .map(|(top, bottom, left, right, change)| {
            let corner = |r: usize, c: usize| format!("{}{}", column_label(c as u32), r + 1);
            let range = if (top, left) == (bottom, right) {
                corner(top, left)
            } else {
                format!("{}:{}", corner(top, left), corner(bottom, right))
            };
            XlsxEffect::SetFormat {
                sheet: sheet.to_string(),
                range,
                change,
            }
        })
        .collect()
}

impl Sheet {
    /// `B3` for a cell in alive lines.
    fn reference(&self, row: Id, column: Id) -> Option<String> {
        let r = self.grid.row_index(row)?;
        let c = self.grid.column_index(column)?;
        Some(format!("{}{}", column_label(c as u32), r + 1))
    }

    fn contains(&self, row: Id, column: Id) -> bool {
        self.grid.contains_row(row) && self.grid.contains_column(column)
    }

    /// The cell whose format shows at `(row, column)`: the top-left cell of
    /// the merged region covering it, as in Excel and jade's `format_at`.
    /// The grid moves merges as jade does — a line inserted inside one joins
    /// it, a deleted corner moves to the next line, a merge left with one
    /// cell is gone.
    fn format_anchor(&self, row: Id, column: Id) -> (Id, Id) {
        let (Some(r), Some(c)) = (self.grid.row_index(row), self.grid.column_index(column)) else {
            return (row, column);
        };
        let span = |first: Option<usize>, last: Option<usize>, at: usize| {
            first.zip(last).is_some_and(|(a, b)| a <= at && at <= b)
        };
        self.grid
            .merges()
            .iter()
            .find(|m| {
                span(self.grid.row_index(m.top), self.grid.row_index(m.bottom), r)
                    && span(
                        self.grid.column_index(m.left),
                        self.grid.column_index(m.right),
                        c,
                    )
            })
            .map_or((row, column), |m| (m.top, m.left))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TableRegion {
    top: Id,
    bottom: Id,
    left: Id,
    right: Id,
}

/// A structural change at its place in the log, kept to rebase formulas
/// written before it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct StructureEntry {
    /// `None` while tentative on its origin.
    seq: Option<u64>,
    origin: ReplicaId,
    change: StructureChange,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct XlsxModel {
    sheets: Vec<Sheet>,
    structure: Vec<StructureEntry>,
    max_lamport: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetView {
    pub id: Id,
    pub name: String,
    /// Alive rows, top to bottom; row `i` is spreadsheet row `i + 1`.
    pub rows: Vec<Id>,
    pub columns: Vec<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum XlsxOp {
    /// Convergent for values, sequenced for formulas. `stamp` orders
    /// concurrent writes to the same cell (last writer wins).
    SetCell {
        sheet: Id,
        row: Id,
        column: Id,
        stamp: Id,
        value: CellInput,
    },
    /// Sequenced: insert or delete a row or column.
    Structure { sheet: Id, op: GridOp },
    /// Convergent: the cell's note, a last-writer-wins register like a
    /// value; `None` (or empty text) deletes it.
    SetNote {
        sheet: Id,
        row: Id,
        column: Id,
        stamp: Id,
        note: Option<String>,
    },
    /// Convergent: every attribute `change` names, on every cell of
    /// `rows` × `columns`, each a last-writer-wins register of its own.
    ///
    /// A range carries the ids of the cells its author saw rather than its
    /// corners: corners resolved at apply time would take in rows another
    /// replica inserted concurrently or not depending on where the two land
    /// in the log, which a convergent operation must not depend on.
    SetFormat {
        sheet: Id,
        rows: Vec<Id>,
        columns: Vec<Id>,
        stamp: Id,
        change: FormatChange,
    },
}

/// What jade must do; indices are 0-based.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum XlsxEffect {
    SetCell {
        sheet: String,
        reference: String,
        value: CellInput,
    },
    InsertRow {
        sheet: String,
        at: u32,
    },
    DeleteRow {
        sheet: String,
        at: u32,
    },
    InsertColumn {
        sheet: String,
        at: u32,
    },
    DeleteColumn {
        sheet: String,
        at: u32,
    },
    /// `None` deletes the note.
    SetNote {
        sheet: String,
        reference: String,
        note: Option<String>,
    },
    /// `range` is `B3` or `B3:D5`.
    SetFormat {
        sheet: String,
        range: String,
        change: FormatChange,
    },
}

/// `0` → `A`, `25` → `Z`, `26` → `AA`.
pub fn column_label(index: u32) -> String {
    let mut n = index + 1;
    let mut label = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        label.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    label.reverse();
    String::from_utf8(label).expect("ASCII")
}

/// `B3` → `(2, 1)`: 0-based (row, column). `$` signs are ignored.
pub fn parse_reference(reference: &str) -> Option<(u32, u32)> {
    let reference = reference.replace('$', "").to_ascii_uppercase();
    let split = reference.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = reference.split_at(split);
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let column = letters.bytes().try_fold(0u32, |acc, b| {
        acc.checked_mul(26)?.checked_add(u32::from(b - b'A' + 1))
    })? - 1;
    let row: u32 = digits.parse().ok()?;
    Some((row.checked_sub(1)?, column))
}

fn reject(message: impl Into<String>) -> Rejection {
    Rejection::new(message)
}

/// Formulas are stored without their `=`, as jade reports them; empty text
/// is a blank cell, as in Excel.
fn normalize(value: &CellInput) -> CellInput {
    match value {
        CellInput::Formula(f) => CellInput::Formula(f.strip_prefix('=').unwrap_or(f).to_string()),
        CellInput::Text(t) if t.is_empty() => CellInput::Blank,
        other => other.clone(),
    }
}

fn from_view(view: &CellView) -> Option<CellInput> {
    if let Some(formula) = &view.formula {
        return Some(CellInput::Formula(formula.clone()));
    }
    match view.kind {
        CellKind::Number => view.number.map(CellInput::Number),
        CellKind::Text => view.text.clone().map(CellInput::Text),
        CellKind::Bool => view.number.map(|n| CellInput::Bool(n != 0.0)),
        CellKind::Empty | CellKind::Error => None,
    }
}

impl XlsxModel {
    /// Empty sheets of the given size, for tests and new workbooks.
    pub fn blank(sheets: &[(&str, u32, u32)]) -> Self {
        let mut next = 0u64;
        let mut id = || {
            next += 1;
            Id::new(next, ReplicaId::ORIGIN)
        };
        let sheets = sheets
            .iter()
            .map(|(name, rows, columns)| Sheet {
                id: id(),
                name: name.to_string(),
                grid: Grid::new(
                    (0..*rows).map(|_| id()).collect::<Vec<_>>(),
                    (0..*columns).map(|_| id()).collect::<Vec<_>>(),
                ),
                cells: BTreeMap::new(),
                notes: BTreeMap::new(),
                formats: BTreeMap::new(),
                protected: BTreeSet::new(),
                tables: Vec::new(),
            })
            .collect();
        Self {
            sheets,
            structure: Vec::new(),
            max_lamport: next,
        }
    }

    /// The model of `book`, with ids owned by [`ReplicaId::ORIGIN`].
    pub fn from_workbook(book: &HeadlessWorkbook, options: LoadOptions) -> Result<Self, LoadError> {
        let summary = book.summary();
        let merges_of = |name: &str| {
            book.workbook()
                .sheets
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.merges.as_slice())
                .unwrap_or_default()
        };
        let shapes: Vec<(&str, u32, u32)> = summary
            .sheets
            .iter()
            .map(|s| {
                let merges = merges_of(&s.name);
                let rows = merges.iter().map(|m| m.end.row + 1);
                let columns = merges.iter().map(|m| m.end.column + 1);
                (
                    s.name.as_str(),
                    rows.fold(s.used_rows.max(options.min_rows), u32::max)
                        .min(LoadOptions::MAX_ROWS),
                    columns
                        .fold(s.used_columns.max(options.min_columns), u32::max)
                        .min(LoadOptions::MAX_COLUMNS),
                )
            })
            .collect();
        let mut model = Self::blank(&shapes);
        let mut stamp = model.max_lamport;
        for (sheet, (name, rows, columns)) in model.sheets.iter_mut().zip(&shapes) {
            if *rows == 0 || *columns == 0 {
                continue;
            }
            let reference = format!("A1:{}{}", column_label(columns - 1), rows);
            let range = book.range(name, &reference).map_err(|e| LoadError::Sheet {
                sheet: name.to_string(),
                message: e.to_string(),
            })?;
            let row_ids = sheet.grid.rows();
            let column_ids = sheet.grid.columns();
            // Table name -> (top, bottom, left, right) indices.
            let mut tables: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
            for (r, cells) in range.rows.iter().enumerate() {
                for (c, view) in cells.iter().enumerate() {
                    if let Some(table) = &view.table {
                        if table.header || table.totals {
                            sheet.protected.insert((row_ids[r], column_ids[c]));
                        }
                        let span = tables.entry(table.name.clone()).or_insert((r, r, c, c));
                        *span = (span.0.min(r), span.1.max(r), span.2.min(c), span.3.max(c));
                    }
                    if let Some(value) = from_view(view) {
                        stamp += 1;
                        sheet.cells.insert(
                            (row_ids[r], column_ids[c]),
                            LwwRegister::new(Id::new(stamp, ReplicaId::ORIGIN), value),
                        );
                    }
                    if let Some(note) = &view.comment {
                        stamp += 1;
                        sheet.notes.insert(
                            (row_ids[r], column_ids[c]),
                            LwwRegister::new(
                                Id::new(stamp, ReplicaId::ORIGIN),
                                Some(note.text.clone()),
                            ),
                        );
                    }
                }
            }
            // The file's merges, clipped to the part of the sheet modelled.
            let merges = merges_of(name)
                .iter()
                .filter(|m| m.start.row < *rows && m.start.column < *columns)
                .map(|m| {
                    let bottom = m.end.row.min(rows - 1) as usize;
                    let right = m.end.column.min(columns - 1) as usize;
                    Merge {
                        top: row_ids[m.start.row as usize],
                        left: column_ids[m.start.column as usize],
                        bottom: row_ids[bottom],
                        right: column_ids[right],
                    }
                })
                .filter(|m| (m.top, m.left) != (m.bottom, m.right))
                .collect();
            sheet.grid = Grid::with_merges_unchecked(row_ids.clone(), column_ids.clone(), merges);
            sheet.tables = tables
                .into_values()
                .map(|(top, bottom, left, right)| TableRegion {
                    top: row_ids[top],
                    bottom: row_ids[bottom],
                    left: column_ids[left],
                    right: column_ids[right],
                })
                .collect();
        }
        model.max_lamport = stamp;
        Ok(model)
    }

    pub fn sheets(&self) -> Vec<SheetView> {
        self.sheets
            .iter()
            .map(|s| SheetView {
                id: s.id,
                name: s.name.clone(),
                rows: s.grid.rows(),
                columns: s.grid.columns(),
            })
            .collect()
    }

    /// The value a cell holds (also for cells in deleted rows or columns).
    pub fn value(&self, sheet: Id, row: Id, column: Id) -> Option<&CellInput> {
        self.sheets
            .iter()
            .find(|s| s.id == sheet)?
            .cells
            .get(&(row, column))?
            .get()
    }

    /// The non-blank cells of `sheet` in its current grid, row by row, with
    /// their A1 references.
    pub fn filled_cells(&self, sheet: Id) -> Vec<(String, &CellInput)> {
        let Some(sheet) = self.sheets.iter().find(|s| s.id == sheet) else {
            return Vec::new();
        };
        let position = |lines: Vec<Id>| -> BTreeMap<Id, usize> {
            lines
                .into_iter()
                .enumerate()
                .map(|(i, id)| (id, i))
                .collect()
        };
        let rows = position(sheet.grid.rows());
        let columns = position(sheet.grid.columns());
        let mut cells: Vec<((usize, usize), &CellInput)> = sheet
            .cells
            .iter()
            .filter_map(|((row, column), register)| {
                let value = register.get()?;
                let at = (*rows.get(row)?, *columns.get(column)?);
                (*value != CellInput::Blank).then_some((at, value))
            })
            .collect();
        cells.sort_by_key(|(at, _)| *at);
        cells
            .into_iter()
            .map(|((r, c), value)| (format!("{}{}", column_label(c as u32), r + 1), value))
            .collect()
    }

    /// The note on a cell (also for cells in deleted rows or columns).
    pub fn note(&self, sheet: Id, row: Id, column: Id) -> Option<&str> {
        self.sheets
            .iter()
            .find(|s| s.id == sheet)?
            .notes
            .get(&(row, column))?
            .get()?
            .as_deref()
    }

    /// The format attributes written through the model that show at a cell:
    /// its own, or its merged region's anchor's, as jade's `format_at` and
    /// Excel show it. An attribute nobody wrote is `None` — it is whatever
    /// the file has.
    pub fn format(&self, sheet: Id, row: Id, column: Id) -> FormatChange {
        let Some(s) = self.sheets.iter().find(|s| s.id == sheet) else {
            return FormatChange::default();
        };
        s.formats
            .get(&s.format_anchor(row, column))
            .map(CellFormat::current)
            .unwrap_or_default()
    }

    /// `(sheet, row, column)` ids of `reference` (`B3`) on the sheet named `sheet`.
    pub fn locate(&self, sheet: &str, reference: &str) -> Option<(Id, Id, Id)> {
        let s = self.sheets.iter().find(|s| s.name == sheet)?;
        let (row, column) = parse_reference(reference)?;
        Some((
            s.id,
            *s.grid.rows().get(row as usize)?,
            *s.grid.columns().get(column as usize)?,
        ))
    }

    fn sheet_index(&self, sheet: Id) -> Result<usize, Rejection> {
        self.sheets
            .iter()
            .position(|s| s.id == sheet)
            .ok_or_else(|| reject(format!("sheet {sheet} does not exist")))
    }

    fn set_cell(
        &mut self,
        sheet: Id,
        row: Id,
        column: Id,
        stamp: Id,
        value: &CellInput,
        ctx: &ApplyContext,
    ) -> Result<Vec<XlsxEffect>, Rejection> {
        let index = self.sheet_index(sheet)?;
        let name = self.sheets[index].name.clone();
        let grid = &self.sheets[index].grid;
        if !grid.contains_row(row) || !grid.contains_column(column) {
            return Err(reject("no such cell"));
        }
        if self.sheets[index].protected.contains(&(row, column)) {
            return Err(reject(
                "cells in an Excel table's header or totals row are read-only",
            ));
        }
        let mut value = normalize(value);
        if let CellInput::Formula(formula) = &mut value {
            // Move the references through every structural change the author
            // had not seen when typing it.
            for entry in &self.structure {
                let unseen =
                    entry.origin != ctx.origin && entry.seq.is_none_or(|seq| seq > ctx.base_seq);
                if unseen {
                    *formula = shift_formula(formula, &name, &entry.change);
                }
            }
        }
        self.max_lamport = self.max_lamport.max(stamp.lamport);
        let sheet = &mut self.sheets[index];
        if !sheet
            .cells
            .entry((row, column))
            .or_default()
            .apply(stamp, value.clone())
        {
            return Ok(Vec::new());
        }
        let (Some(r), Some(c)) = (sheet.grid.row_index(row), sheet.grid.column_index(column))
        else {
            return Ok(Vec::new());
        };
        Ok(vec![XlsxEffect::SetCell {
            sheet: name,
            reference: format!("{}{}", column_label(c as u32), r + 1),
            value,
        }])
    }

    fn set_note(
        &mut self,
        sheet: Id,
        row: Id,
        column: Id,
        stamp: Id,
        note: &Option<String>,
    ) -> Result<Vec<XlsxEffect>, Rejection> {
        let index = self.sheet_index(sheet)?;
        if !self.sheets[index].contains(row, column) {
            return Err(reject("no such cell"));
        }
        let note = note.clone().filter(|text| !text.is_empty());
        self.max_lamport = self.max_lamport.max(stamp.lamport);
        let sheet = &mut self.sheets[index];
        if !sheet
            .notes
            .entry((row, column))
            .or_default()
            .apply(stamp, note.clone())
        {
            return Ok(Vec::new());
        }
        Ok(sheet
            .reference(row, column)
            .map(|reference| XlsxEffect::SetNote {
                sheet: sheet.name.clone(),
                reference,
                note,
            })
            .into_iter()
            .collect())
    }

    fn set_format(
        &mut self,
        sheet: Id,
        rows: &[Id],
        columns: &[Id],
        stamp: Id,
        change: &FormatChange,
    ) -> Result<Vec<XlsxEffect>, Rejection> {
        let index = self.sheet_index(sheet)?;
        let change = canonical_format(change)?;
        let s = &self.sheets[index];
        if rows.is_empty()
            || columns.is_empty()
            || !rows.iter().all(|row| s.grid.contains_row(*row))
            || !columns.iter().all(|column| s.grid.contains_column(*column))
        {
            return Err(reject("no such cell"));
        }
        self.max_lamport = self.max_lamport.max(stamp.lamport);
        let sheet = &mut self.sheets[index];
        // Alive cells by index, with the attributes they won.
        let mut won = BTreeMap::new();
        for row in rows {
            for column in columns {
                let attributes = sheet
                    .formats
                    .entry((*row, *column))
                    .or_default()
                    .apply(stamp, &change);
                let at = sheet
                    .grid
                    .row_index(*row)
                    .zip(sheet.grid.column_index(*column));
                if let (false, Some(at)) = (attributes.is_empty(), at) {
                    won.insert(at, attributes);
                }
            }
        }
        Ok(rectangles(&sheet.name, won))
    }

    fn structure(
        &mut self,
        sheet: Id,
        op: &GridOp,
        ctx: &ApplyContext,
    ) -> Result<Vec<XlsxEffect>, Rejection> {
        let index = self.sheet_index(sheet)?;
        let s = &self.sheets[index];
        let name = s.name.clone();
        let mut grid = s.grid.clone();
        let (axis, inserted, at, new) = match op {
            GridOp::Merge(_) | GridOp::Unmerge { .. } => {
                return Err(reject("merged cells are not supported for xlsx yet"))
            }
            GridOp::InsertRow { id, .. } => {
                grid.apply(op)?;
                (Axis::Rows, true, grid.row_index(*id), Some(*id))
            }
            GridOp::InsertColumn { id, .. } => {
                grid.apply(op)?;
                (Axis::Columns, true, grid.column_index(*id), Some(*id))
            }
            GridOp::DeleteRow { id } => {
                let at = grid.row_index(*id);
                grid.apply(op)?;
                (Axis::Rows, false, at, None)
            }
            GridOp::DeleteColumn { id } => {
                let at = grid.column_index(*id);
                grid.apply(op)?;
                (Axis::Columns, false, at, None)
            }
        };
        let at = at.expect("the grid validated the line");
        // Inside an Excel table: strictly below its header (an insert) or on
        // any of its rows (a delete), and likewise for columns.
        let inside = s.tables.iter().any(|t| {
            let (first, last) = match axis {
                Axis::Rows => (s.grid.row_index(t.top), s.grid.row_index(t.bottom)),
                Axis::Columns => (s.grid.column_index(t.left), s.grid.column_index(t.right)),
            };
            let (Some(first), Some(last)) = (first, last) else {
                return false;
            };
            if inserted {
                first < at && at <= last
            } else {
                first <= at && at <= last
            }
        });
        if inside {
            return Err(reject(
                "rows and columns inside an Excel table cannot be inserted or deleted yet",
            ));
        }
        self.sheets[index].grid = grid;
        let at = at as u32;
        let change = StructureChange {
            sheet: name.clone(),
            axis,
            at,
            count: 1,
            inserted,
        };
        for sheet in &mut self.sheets {
            for register in sheet.cells.values_mut() {
                if let Some(CellInput::Formula(formula)) = register.value_mut() {
                    *formula = shift_formula(formula, &sheet.name, &change);
                }
            }
        }
        self.structure.push(StructureEntry {
            seq: ctx.seq,
            origin: ctx.origin,
            change,
        });
        if let Some(id) = new {
            self.max_lamport = self.max_lamport.max(id.lamport);
        }
        Ok(vec![match (axis, inserted) {
            (Axis::Rows, true) => XlsxEffect::InsertRow { sheet: name, at },
            (Axis::Rows, false) => XlsxEffect::DeleteRow { sheet: name, at },
            (Axis::Columns, true) => XlsxEffect::InsertColumn { sheet: name, at },
            (Axis::Columns, false) => XlsxEffect::DeleteColumn { sheet: name, at },
        }])
    }
}

impl Document for XlsxModel {
    type Op = XlsxOp;
    type Effect = Vec<XlsxEffect>;

    fn lane(op: &XlsxOp) -> Lane {
        match op {
            XlsxOp::SetCell {
                value: CellInput::Formula(_),
                ..
            } => Lane::Sequenced,
            XlsxOp::SetCell { .. } => Lane::Convergent,
            XlsxOp::Structure { .. } => Lane::Sequenced,
            // Id-keyed last-writer-wins registers (per attribute for
            // formats) commute and do not depend on the log position.
            XlsxOp::SetNote { .. } | XlsxOp::SetFormat { .. } => Lane::Convergent,
        }
    }

    fn apply(&mut self, op: &XlsxOp, ctx: &ApplyContext) -> Result<Vec<XlsxEffect>, Rejection> {
        match op {
            XlsxOp::SetCell {
                sheet,
                row,
                column,
                stamp,
                value,
            } => self.set_cell(*sheet, *row, *column, *stamp, value, ctx),
            XlsxOp::Structure { sheet, op } => self.structure(*sheet, op, ctx),
            XlsxOp::SetNote {
                sheet,
                row,
                column,
                stamp,
                note,
            } => self.set_note(*sheet, *row, *column, *stamp, note),
            XlsxOp::SetFormat {
                sheet,
                rows,
                columns,
                stamp,
                change,
            } => self.set_format(*sheet, rows, columns, *stamp, change),
        }
    }

    fn max_lamport(&self) -> u64 {
        self.max_lamport
    }
}

/// Serializes a map with non-string keys as a list of pairs.
mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}
