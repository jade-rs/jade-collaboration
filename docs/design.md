# jade-collaboration design

Real-time co-editing of `.docx`, `.xlsx` and `.pptx` by any mix of
people and AI agents, on top of the jade engine
([jade-rs/jade-core](https://github.com/jade-rs/jade-core)).

## 1. Why neither a pure CRDT nor pure OT

A CRDT guarantees that replicas which have seen the same operations hold the
same state. It does not guarantee that this state is a *valid document*.
Convergence is only free for operations that are **invariant-confluent**
(Bailis et al., VLDB 2015): merging two valid states must give a valid state.
Plain text insertion and deletion are; most office structure is not.

| Concurrent pair | Each alone | Both merged by a CRDT |
| --- | --- | --- |
| merge `A1:B2` ∥ merge `B2:C3` | valid | overlapping merge regions — no file format can express it |
| join paragraph P with Q ∥ insert a table between P and Q | valid | a table in the middle of a paragraph |
| insert row above row 5 ∥ type `=A5` | valid | the formula points at the wrong row |
| delete column C ∥ merge `B1:D1` | valid | a merge anchored on a deleted column |
| sort range ∥ edit a cell in it | valid | the edit lands on a different record |

`lean/JadeCollab/Sequencer.lean` proves the first row mechanically
(`crdt_union_breaks_invariant`).

Operational Transformation with a central server can keep invariants, but its
transform functions are quadratic in the number of operation kinds and
notoriously hard to get right for trees and tables; every structural command
jade already has (≈100 `HeadlessCommand`s) would need a transform against every
other.

## 2. The hybrid: two lanes, one log

Every operation is addressed by **stable identifiers** (CRDT style), never by
index, and is classified into one of two lanes:

* **Convergent lane** — invariant-confluent operations. They commute with every
  other operation, so a replica applies them immediately and never rolls them
  back. Text insert/delete inside a paragraph (an RGA sequence CRDT), paragraph
  split, and cell values (last-writer-wins registers).
* **Sequenced lane** — operations that need coordination. A replica applies them
  tentatively; the central **sequencer** validates them against the
  authoritative state in arrival order and either commits or rejects them.
  Table rows/columns/merges, paragraph join, table insertion, formulas, and
  anything that rewrites references.

There is still exactly one totally ordered log per document, kept by the
server. The lanes only decide *whether an operation may be rejected* and
*whether a replica has to rebase when it arrives*:

```
             client A                         server (per document)                  client B
  local op ─▶ view.apply (optimistic)          ┌────────────────────────┐
          └─▶ Submit{client_seq, base_seq, op} ─▶│ dedup (replica, seq)   │
                                               │ validate + apply on     │
                                               │   authoritative model   │──▶ Sequenced{seq, op} ─▶ confirmed.apply
                                               │ materialize into jade   │                          convergent: view.apply
                                               │   (WordDocument / xlsx) │                          sequenced : view = confirmed + pending
                                               │ append to log           │
                                               └────────────────────────┘
                                                   └─▶ Rejected{client_seq, reason} (sequenced lane only)
```

Why keep a CRDT when a server orders everything anyway?

1. **No index transformation.** Operations name the characters, rows and
   columns they touch, so an operation computed against an old view is still
   meaningful against a newer state. There are no OT transform functions.
2. **Typing never rebases.** Remote convergent operations are applied straight
   to the optimistic view (they commute with the pending local operations —
   `lean/JadeCollab/Reconcile.lean`, `fast_path_sound`). Only the rarer
   structural operations make a replica replay its pending queue.
3. **Rejections are rare and local.** Only sequenced operations can be
   rejected, and only when a concurrent structural change made them invalid.
4. **Offline / peer text merges remain possible** for the convergent lane.

Why keep the server when a CRDT handles text?

1. Invariants that need coordination (§1) are checked in one place, in one
   order.
2. **The server is the only component that touches OOXML.** It applies each
   committed operation to a jade `WordDocument` / `HeadlessWorkbook` in log
   order, translating identifier-addressed operations to the index-addressed
   `HeadlessCommand`s jade already implements. jade's Word-compatible editing
   (run splitting, formula rewriting on row insert, unknown XML preserved) is
   reused as is and never has to commute.
3. One authoritative log is the audit trail for AI edits, the unit of
   persistence and the basis for undo and access control.

## 3. Identifiers and clocks

* `ReplicaId(u64)` — handed out by the server on join, never reused for a
  document. Replica `0` is the document's *origin* (content loaded from the
  file).
* `Id { lamport, replica }` — totally ordered (lamport first). Every replica
  keeps a Lamport clock that is advanced past every identifier it observes, so
  a child is always created with a larger identifier than its parent. The RGA
  integration rule depends on this.
* `client_seq` — per replica, strictly increasing. `(replica, client_seq)`
  identifies a submission; the sequencer drops duplicates, so a client can
  resend its pending queue after a reconnect. (This replaces the prototype's
  `op_id` / `CollaborativeSessionTracker` in jade-core.)
* `base_seq` — the last log position the client had seen when it made the
  operation. Operations whose meaning depends on index-based text (formulas)
  use it to detect that a structural change slipped in between.

## 4. Data model

### Text: RGA with paragraph marks (`jade-collab-core::rga`)

A *story* (the body, one table cell, one footnote or endnote, one text box)
is one RGA sequence of atoms: a character, a paragraph mark, or an *anchor*
(below). Each atom names the atom it was inserted after
(its *parent*); deletion leaves a tombstone. The visible order is a depth-first
walk of the parent tree with siblings in **descending** identifier order.

The specification (`rga::spec`, and `lean/JadeCollab/Rga.lean`) is literally
that: the state is the *set* of insert operations plus the set of deleted
identifiers, and the document is a pure function of those sets. Lean proves
that replicas which received the same operations, in any order and with any
duplication, render the same document. The production implementation keeps
the document order in chunks of at most 128 atoms with an id → chunk map and
Fenwick trees of (visible) counts, so lookups, inserts and deletes cost
O(log n + 128), and integrates with the classic RGA skip rule; property tests
check it against the specification on random concurrent histories and
against the original flat-array implementation kept as a reference.

A story's last paragraph has no mark atom (Word's final paragraph mark cannot
be deleted). Splitting a paragraph inserts a mark: convergent. Joining deletes
a mark: sequenced, because a table may be anchored to that mark.

### Notes and text boxes (`jade-docx-collab`)

Every footnote, endnote and text box loaded from the file is a story of its
own (`StoryId::Footnote`, `Endnote`, `TextBox`, keyed by an id minted on
load), edited like the body: convergent text and paragraph splits,
sequenced joins. What places it — a note's reference mark, a text box's
anchoring run — is an **anchor atom** in its host paragraph's RGA, in run
order. Anchors are zero-width and are never deleted: `DeleteText` refuses
them, so a concurrent deletion of the text around one cannot take it, and a
split or join moves it with the text around it. Creating and deleting notes
and text boxes are out of scope; note separators are not modelled.

The model's walk lists paragraphs in jade's paragraph-surface order, which
is also the order the intent API indexes: the body (table cells row by
row); after each body paragraph the text boxes it anchors (in run order)
and the footnotes it references first; then the remaining footnotes by
`w:id`; then the endnotes in order of first reference from the body or a
text box, then by `w:id`. Each paragraph carries jade's *story position*
(`ParagraphStory`, from jade-core's `paragraph_story` API): body index; text
box by host body paragraph, index among the host's text boxes and paragraph;
note by `w:id` and paragraph.

The anchors follow jade's rules, which a test checks on every fixture and
the random-edit test after every operation:

* jade's text offsets cannot name a side of a zero-width run. Typing at an
  offset where an anchor sits goes right after the character before it —
  *before* the anchor — except at a paragraph's start, where it goes after
  the anchors there. The intent API resolves a caret the same way (at a
  paragraph's start the caret's parent is the last leading anchor), and an
  RGA insert after a visible character lands before any older anchor that
  follows it, so sequential edits always agree.
* A split takes the anchors at its offset to the second paragraph; a mark
  inserted after a character lands before them as well.
* A deletion is sent to jade as ranges that never contain an anchor (split
  around each one), with rendered offsets that skip a reference's mark.
* jade lists a paragraph's text boxes and first-referenced footnotes between
  it and the next paragraph and has no command that joins across them, so
  the model refuses such a join (sequenced) and `admit` checks it again.

jade writes a text box's text to the file but not new or removed paragraphs
in it (a split is kept in memory only; shown by a test), so a text box's
paragraphs are fixed: the model refuses `SplitParagraph` and `JoinParagraph`
there, on every replica. jade cannot anchor a comment in a note; the model
refuses that too. Formatting works in every story. jade's `add_comment`
splits the runs of the paragraph it anchors to without relisting that
paragraph's text boxes, whose paragraphs then point at a stale run until the
file is read again; the materializer has jade relist them by restoring
jade's own snapshot. A jade-core bug surfaced here — text typed after a note
reference or text box alone in its paragraph went into that run, and the
next edit that split the run (formatting, a comment) copied the reference
into both halves — and is fixed on jade-core's `feat/paragraph-story-api`
with the story API the model needs (`paragraph_story`,
`paragraph_story_surface`, `paragraph_anchors`).

**Concurrency around anchors.** jade's placement depends on what it sees
when an edit arrives, the RGA's does not: text typed after a character that
was concurrently deleted, right after an anchor, belongs after the anchor;
text typed where a concurrent split put an anchor at the start of the new
paragraph belongs before it. The materializer realises these orders with a
split before the anchors, typing at the start of the half that must hold
the text, and a join — possible unless a text box earlier in the paragraph
sits between the halves (jade lists the first half's text boxes between
them). One order jade cannot produce at all: a paragraph *ending* with an
anchor because of a split right after a concurrently deleted character (its
split always moves the anchor on). The materializer reports that as a fault
instead of writing a different document. Both need a jade command that
names the side of a zero-width run.

### Tables (`jade-collab-core::grid`)

A table is anchored *after* a paragraph mark of its parent story (or at the
story start), which keeps "a story ends with a paragraph" and "a table is never
inside a paragraph" true under concurrent splits. Rows and columns are ordered
lists of identifiers with tombstones; a cell is the story keyed by
`(RowId, ColId)`. Structural deletes only tombstone, so a concurrent edit to a
cell in a deleted row still commutes — it lands in an invisible story.

Merge regions are rectangles named by their corner row/column identifiers. The
grid's validity predicate (all regions alive, well-formed and pairwise
non-overlapping) is checked by the sequencer for every structural operation;
Lean proves that the grid's `apply` keeps it (`Grid.apply_preserves_valid`).

**Merged cells in docx.** A table keeps every story it ever held, by
identity, and separately *which stories each cell position shows* (jade's
`w:tc` there). Merging (sequenced, `MergeCells`) changes only the latter, as
Word and jade do: the top-left cell shows every covered cell's stories, row
by row; each cell below it in its column becomes a follower showing a new
empty story (Word keeps a paragraph in every `w:tc`); the other covered cells
go. Unmerging (`UnmergeCells`) keeps what the merged cell shows and gives the
cells it frees new empty stories. Because stories never move, text typed
into any cell's story commutes with merging and unmerging — the property the
client fast path needs (Lean: `CellCommute.cell_edit_comm_grid`; property
test `merges_commute_with_cell_text`). Rows and columns may not be inserted
or deleted through a merged region. Tables whose merges come from the file
(`w:gridSpan`, `w:vMerge`) load unlocked; ragged or nested tables stay
structurally read-only. A jade-core bug surfaced here — a merge wrote an
empty paragraph into each follower's XML but not into the model, so a save
added a paragraph — and is fixed in jade-rs/jade-core#12.

### Formatting and comments (`jade-docx-collab`)

Formatting travels the sequenced lane as `Format { story, start, end,
change }`: `start` and `end` are atom ids (characters, or a paragraph mark to
reach an empty paragraph) of one story, inclusive, and `change` is one
`FormatChange` — bold, italic, underline, strike, font size (half points),
font family, font color, highlight, clear formatting, paragraph alignment and
paragraph style. The range is resolved **when the operation is applied, in
log order**: the visible atoms between the two ids in the story's RGA order.
Characters deleted before it commits shrink it, characters inserted inside
it before it commits are covered, and a range with nothing visible left does
nothing. A character change becomes one jade command per paragraph slice; a
paragraph change one per paragraph holding a visible atom of the range.

The model **does not store formatting**: it only validates the value (values
jade would write verbatim but Word cannot read, such as a hex highlight, are
refused) and reports where jade must format. jade does the formatting, so
Word's own rules — text typed next to bold text is bold, clearing keeps the
highlight, a paragraph style re-cascades run properties — are jade's, never
re-implemented. The convergent lane stays formatting-free; a Peritext-style
mark CRDT would be needed to format optimistically without the sequencer.

Comments are sequenced too. `AddComment` anchors to the visible characters of
an id range (refused when none are left); `ReplyToComment`, `ResolveComment`
and `DeleteComment` name a comment by its model id. The model keeps a comment
table (thread root, resolved, deleted, and jade's `w:id` for comments loaded
from the file); like Word and jade, threads are flat (a reply to a reply
answers the root) and deleting a root deletes its replies. jade numbers the
comments made in the session itself, reusing freed numbers, so the
materializer records the number each new comment got instead of predicting
it.

`admit` preflights formatting and comment operations on a clone of the jade
document and refuses what jade cannot do — for instance a reply whose root's
text was deleted (jade re-anchors replies on the root's text).

### Tracked changes (`jade-docx-collab`)

Word's Track Changes in a shared document, edited by people and agents at
once. What jade does, found by probing it (`tests/tracked_changes.rs`):
with a revision session (`start_change_tracking`, one author and date for
the whole document) typed text becomes a `w:ins` run of its own (a new
`w:id` per edit, so two authors typing into each other's insertions stay
apart), and a deletion becomes a `w:del` run whose text stays in the
paragraph — every jade offset counts deleted text. Deleting text that is
already deleted keeps its first author. Three things jade got wrong, all
fixed on jade-core's `feat/collab-tracked-changes` (below): deleting
inserted text *replaced* the insertion by the deletion, so rejecting all
changes kept text nobody had accepted (Word nests `<w:ins><w:del>`);
text typed with tracking off at the end of an insertion joined it; and a
revision could be accepted or rejected only as whole runs. jade also does
not remove one's own insertion on deletion, as Word does — the intent API
does that instead (below).

Before this, a file's revisions loaded as plain text: deleted text was
ordinary visible characters and nothing was locked, so editing such a file
silently disagreed with what Word shows.

**Revisions are attributes of atoms, never tombstones.** A tracked deletion
must stay visible (and rejectable), and a tracked insertion must be
removable as a unit, so the model keeps beside each story's RGA:

* `inserted: atom → revision` — set when the atom is created by a tracked
  insertion (or loaded from a `w:ins` run); cleared when the insertion is
  accepted.
* `deleted: atom → set of revisions` — every tracked deletion that named
  the atom. A **grow-only set** under the convergent lane; only a sequenced
  rejection removes a member.
* `revisions: id → {kind, author, date, replica, file w:id, moved}` — one
  entry per tracked operation (its first atom id, or a fresh id for a
  deletion), or per (kind, `w:id`, author, date) loaded from the file.

A character is *in jade's text* while it is not tombstoned; it *shows as
deleted* while its set is non-empty, attributed to the set's **least**
revision id. Two concurrent tracked deletions of the same character are
both kept, so rejecting one leaves the other in force; which one Word shows
is a pure function of the set (jade applies them in log order and keeps the
first, so the materializer re-attributes when the log order and the id
order disagree — below).

| Operation | Lane | Effect on the atoms |
| --- | --- | --- |
| `TrackInsertText {story, parent, first, text, date}` | convergent | new atoms, `inserted = first` |
| `TrackDeleteText {story, ids, revision, date}` | convergent | `deleted[a] ∪= {revision}` for each named character (also for tombstoned ones, so it commutes with removals) |
| `InsertText` / `DeleteText` | convergent | as before: plain atoms; a tombstone (removing tracked text outright) |
| `AcceptRevisions {revisions}` | sequenced | insertion: `inserted` cleared; deletion: its atoms tombstoned |
| `RejectRevisions {revisions}` | sequenced | insertion: its atoms tombstoned; deletion: removed from every set |
| `SetTracking {on}` | sequenced | the document's mode (Word's *Track Changes* button) |
| `SetAuthor {replica, name}` | sequenced, server only | who `replica` is |

**Convergence.** The state is (tombstones, `inserted`, `deleted`): set
union, map insertion of fresh keys, and removals keyed by a revision id.
Every convergent operation commutes with every other operation — the
client fast path's condition (§2) — provided a decision names a revision
other than the one a concurrent tracked deletion creates, which always
holds (a revision is decided only after its author's operation committed).
`lean/JadeCollab/Revision.lean` proves it: `trackDelete_comm_*` for every
pair, `converges` for any order of convergent operations, and that the
attributed revision (the set's minimum) is the same everywhere. Decisions
name **whole revisions by id**, never ranges: a range resolved in log order
would take in a revision inserted concurrently inside it on one replica and
not on another, and the per-step `view == rebuilt view` check would catch
it. The intent API turns "accept the changes in paragraphs 3–5" or "reject
all" into the ids the author's view shows; a revision made concurrently is
left for the next decision, as in Word when someone else's change arrives
after you clicked.

A concurrent tracked deletion and an insertion inside the deleted range:
the new atoms sit between deleted ones (RGA), keep their own `inserted`
revision (or none), and are not deleted — the deletion named only the
characters its author saw. jade gets the same: an insertion inside a
`w:del` run splits it.

**Authors.** A tracked operation carries a date but no author: a client
could claim any name, and a convergent operation cannot be rejected after
the fact. The author is the operation's origin replica, resolved through
`authors` when the operation is applied and stored in the revision. Only
the server writes `authors`: when a replica joins or resumes over a socket
whose token has a name — a person, or an agent's MCP token — the server
commits `SetAuthor` as an origin operation right after the join. Every
replica applies a replica's operations after the `SetAuthor`s committed
before them (the server commits them before reading anything that replica
sends, and a replica rebuilds its view when one arrives), so the snapshot is
the same everywhere. A replica with no name (no tokens) authors anonymously
(jade writes no `w:author`). "Own" — for Word's rule that deleting your own
insertion removes it — means the same author name, or the same replica when
there is no name.

**What each edit becomes with tracking on** (the intent API reads the
mode from the view; the operations never depend on it, so a concurrent
`SetTracking` cannot change what an edit already sent means):

* typing → `TrackInsertText`;
* deleting → characters already shown as deleted are skipped (Word keeps
  their first author); characters of one's own *still tracked* insertion
  go in a plain `DeleteText`; the rest in one `TrackDeleteText`;
* paragraph splits and joins, tables, formatting and comments stay
  untracked (jade's paragraph-mark revisions cannot all be decided yet, and
  Word's formatting revisions are not modelled).

A file's existing revisions load as these attributes (`w:moveFrom` /
`w:moveTo` as moved deletions / insertions), are listed, and can be
accepted and rejected like session-made ones. Paragraph-mark revisions in a
file are left in jade untouched and not shown.

### Spreadsheets (`jade-xlsx-collab`)

Sheets have row and column identifier lists (sequenced lane) and cells are
last-writer-wins registers keyed by `(RowId, ColId)` (convergent lane; empty
text is normalised to a blank cell, as Excel does).

Formulas are the one place where index-addressed content survives: `=A5`
names a row by position. They travel the sequenced lane and are kept in step
with jade by reusing jade's own reference shifter
(`jade_xlsx_reader::shift_formula`) rather than re-implementing it:

* every structural change moves the references of every formula in the
  model, exactly as jade moves them in the file;
* a formula whose author had not seen some structural changes (committed
  after its `base_seq` by other replicas) is moved through those changes, in
  log order, before it is stored. The server and every replica do the same
  thing at the same log position, so the result is deterministic — this is
  the only transformation in the system, and it is jade's.

Notes and cell formats are convergent too. A note is a last-writer-wins
register per cell, like a value. A format is one register per cell *and
attribute* (bold, italic, underline, strike, font colour/size/name, fill,
number format, horizontal/vertical alignment, wrap — exactly jade's
`FormatChange`): jade's `set_format` is a partial change that keeps every
attribute it does not name, so attributes are independent, concurrent
changes to different attributes of one cell both survive, and each
attribute converges to its newest write. jade is only told the attributes a
write won when it arrived, so in log order the last write it sees for an
attribute is the newest one. The model records only what was written
through it; the formats the file already had stay in jade. Values are
canonicalised to what `format_at` reads back (`#ff0000` → `FF0000`, an
empty number format → `General`) and changes jade would refuse (a bad
colour, a font size outside 1–409) are refused on every replica, so a
convergent write never reaches a server whose jade cannot realise it.

A range is formatted by one operation that names the row and column ids
its author saw, not its corners: corners resolved at apply time would take
in a row another replica inserted concurrently or not depending on where
the two land in the log, which a convergent operation must not depend on.
The server realises it with one `set_format` per rectangle of cells that
won the same attributes.

jade reports a merged cell's format at the merge's anchor, so the model
loads the file's merges into the sheet grid, which moves them exactly as
jade does (a line inserted inside joins the merge, a deleted corner moves
to the next line). Formatting a covered cell is kept and shows once a
deletion makes it the anchor, as in jade. Notes and formats in an Excel
table's header or totals row are safe — jade leaves the header text and
column name alone when only the format or the note changes — so unlike
values they are not read-only there.

**Merging cells** is sequenced (two merges can overlap) and validated by
the grid (no overlap, at least two cells, not touching an Excel table). As
in Excel, jade keeps the top-left value and clears the others, and the
covered cells take the anchor's format. The model clears with a
last-writer-wins write of a blank stamped by the merge, and copies the
anchor's format the same way (an attribute the model does not know for the
anchor becomes unknown — "whatever the file has" — for the covered cells).
Written as LWW writes, the clear commutes with every concurrent cell write:
one with a newer stamp survives in whatever order a replica applies the two,
and when it reached the server first, the materializer writes it back after
jade's merge. Unmerging leaves values and formats where they are.

**Sheets** are added, renamed, deleted and moved on the sequenced lane,
through jade-core's headless editor (jade-rs/jade-core#11). Names are
validated with jade's own Excel rules on every replica. A rename rewrites
every formula naming the sheet with jade's `rename_sheet_in_formula`; a
deletion turns references into `#REF!` with `delete_sheet_in_formula`; both
are logged next to row/column changes so a formula typed before them is
rebased through them in log order, under the name its sheet had at each
change. A deleted sheet stays in the model, invisible, so a convergent
write made before the deletion was seen still lands and commutes.

Excel tables (ListObjects) do things on their own: a column inserted inside
a table gets a `ColumnN` header, a blank or repeated header is renamed, a
calculated column fills new rows. Rows and columns may be inserted and
deleted inside a table — the table grows and shrinks with them, its header
and totals rows stay, and it keeps one data row and one column — and a
header is renamed with the sequenced `SetHeader` (an intent's `set_cell` on
a header cell becomes one). Whatever Excel then fills in reaches the model
as **derived operations** (§5): the server compares the sheet's table cells
in jade with the model after each such change and commits the differences.
Writing a header or totals cell as a plain value stays refused.

### Presentations (`jade-pptx-collab`)

A deck is an ordered list of slide identifiers. Every text body jade's pptx
writer can edit — a shape's, a group child's, and each slide's speaker notes
— is a story exactly like a docx story: an RGA of characters and paragraph
marks, `\n` in jade's text being a mark. Each top-level shape's placement is
five last-writer-wins registers (x, y, width, height in EMU; rotation in
60000ths of a degree), so a move and a concurrent resize both survive and two
moves converge on the newer stamp. Which attributes a shape lets change comes
from its locks, read with the file; size is read-only under a locked aspect
ratio, since per-attribute writes would break it.

| Operation | Lane | Why |
| --- | --- | --- |
| insert / delete text, split paragraph | convergent | RGA |
| set geometry | convergent | per-attribute LWW |
| join paragraphs | sequenced | as in docx; jade keeps the first paragraph's properties, so every copy of the file must merge in the same order |
| insert slide | sequenced | jade fills it with the placeholders of the layout of the slide it lands in front of, which depends on concurrent structure |
| delete slide | sequenced | a deck keeps at least one slide: two concurrent deletions of the last two are each valid, together not |
| move slide | sequenced | concurrent moves of one slide must land in one place; ordering is simpler than a move CRDT |
| duplicate slide | sequenced | the copy holds the source as of the log position |
| insert text box | sequenced | jade picks the file id from the ids in use, and may refuse a shape tree it cannot extend |
| delete shape | sequenced | jade refuses shapes the slide depends on in ways only the file shows |

Two things need care.

* **Duplication must commute with convergent edits.** A copy keeps its
  source's character and shape identifiers (stories are keyed by slide) and
  the source remembers each copy's origin replica and log position. An edit
  to the source whose author had not seen the duplication — another replica,
  and `base_seq` before the duplication's position or the duplication still
  tentative — is applied to the copy as well, transitively. That is the
  result the author would have got had the edit been committed first, so the
  edit and the duplication commute and a replica can still apply a remote
  edit straight onto a view holding its own pending duplication.
* **What jade puts on a new slide.** The model does not re-derive it: when a
  deck is loaded, jade inserts a slide into a scratch copy for each layout in
  use and the model records the placeholders (ids, empty text, inherited
  placement) as that layout's template.

Shape numbers are the file's `cNvPr id`s for every shape loaded or placed by
a template. A text box made in the session gets the next number the model
has not used on its slide; jade may give it another id (it reuses those of
deleted shapes), and the materializer keeps the mapping.

## 5. Materialization into jade

The server owns one jade document per room and applies, for every committed
operation, the `HeadlessCommand`s (docx), `HeadlessWorkbook` calls (xlsx) or
jade-pptx-writer `EditSession` calls (pptx) that realise it, computing
paragraph indices and byte offsets from the model at that point of the log.
jade addresses paragraphs by its flattened *paragraph surface* index. Model
effects name a paragraph by jade's story position instead, which the
materializer turns into a surface index with jade's `paragraph_story_surface`
when it applies the effect, so the model never depends on jade's
enumeration order; text commands get rendered offsets
(`model_to_rendered_text_position`, plus the marks of the note references the
model says come before the position), formatting and comments model
offsets. The model mirrors jade's enumeration and a test checks the two
agree on real fixtures — in order on load and after reloading a written
file, by story position after edits: after a split jade lists the new
paragraph before the footnotes the first half references until the file is
read again. For pptx, effects name slides by position and
text by paragraph and byte offset; the materializer turns those into jade's
UTF-16 ranges on jade's own text.

Clients do not need jade: an AI agent, a CLI or a test works on the model. A
jade editor client additionally applies the same effects to its local
`WordDocument`.

**Derived operations.** The real document also *does* things the model
cannot predict (Excel naming a new table column). After committing an
operation the sequencer asks the materializer what jade did on its own
(`Materializer::derived`) and commits each answer as an operation of the
document's origin (replica 0), right after its cause; the server journals
and broadcasts them like any commit, and a replayed journal applies them
without asking again. Only the origin may commit them (the model refuses
them from anyone else), and they carry no effect — jade already holds what
it derived. Replicas need nothing new: a derived operation is a remote
sequenced operation like any other.

The real document can know things the model does not. A `Materializer` may
therefore **refuse a sequenced operation before it is applied**
(`Materializer::admit`); the rejection travels back like any other. jade, for
instance, lists a text box's or a note's paragraphs between two body
paragraphs and cannot join across them, so the docx materializer refuses
such joins (the model refuses them too, knowing the anchors); the pptx
materializer dry-runs shape deletions, slide deletions and duplications and
text-box insertions on the current file. `admit` is never consulted for
convergent operations — they must commit wherever they land; the pptx model
therefore refuses text and geometry jade would refuse (characters, the
32-bit EMU range, locks) with jade's own validators, on every replica.

**Tracked changes in jade.** jade's revision session is document-wide, so
the materializer keeps it off and switches it on, with the operation's
author and date, around exactly one jade command: the `InsertText` of a
tracked insertion (the split and join that place text beside an anchor
stay untracked) or the `ReplaceRange` of a tracked deletion. Removing
tracked text (accepting a deletion, rejecting an insertion, deleting
outright) is an untracked `ReplaceRange`; accepting an insertion and
restoring a deletion are jade's `accept_revisions` / `reject_revisions`
with *revision targets* — the parts of jade's runs under the model's
characters, read from jade with their `w:id`s, which needs jade-core's
targets on part of a run. When the least revision of a character's set
changes (a tracked deletion with a smaller id arrives later in the log, or
the least one is rejected while others remain), the model emits a restore
and a tracked deletion by the new least revision's author, so jade shows
the author the model does. A test compares, for every character of every
paragraph, the model's (inserted author/date, deleted author/date) with
jade's runs after every operation.

Tables are matched to jade's `table_order_index` by identity: the model knows
the body's table structure (nested tables included), jade's order also counts
tables inside text boxes, and the materializer pairs the two by walking
jade's parsed body.

### Replica rules learnt from the property tests

* A remote convergent operation is applied straight to the optimistic view.
* A remote sequenced operation rebuilds the view (`confirmed` + pending).
* The acknowledgement of an **own sequenced** operation also rebuilds the
  view: its committed result can depend on where it was committed (the xlsx
  structure log records the log position), so the tentative result is
  replaced by the authoritative one. A convergent operation must therefore
  never depend on its log position.

## 6. Components

| Crate | Role |
| --- | --- |
| `jade-collab-core` | identifiers, Lamport clock, RGA (+ executable spec), LWW register, table grid |
| `jade-collab-sync` | `Document` trait, lanes, wire protocol, `Sequencer` (server core, with checkpoints, journal records and replay), `Replica` (client core), deterministic network simulator |
| `jade-docx-collab` | docx model + operations, loading from a jade `WordDocument`, materialization to `HeadlessCommand`s, index-addressed intent API (migrated from jade-core) |
| `jade-xlsx-collab` | xlsx model + operations, loading from / materializing into a jade `HeadlessWorkbook` |
| `jade-pptx-collab` | pptx model + operations (slides, shape and notes text, geometry, text boxes), loading through jade's reader/writer, materialization through jade-pptx-writer's `EditSession`, slide-number / shape-id intent API |
| `jade-collab-server` | WebSocket server (one actor thread per document, since jade documents are `!Send`), document storage, bearer tokens, presence relay, WebSocket client, `jade-collab` CLI |
| `jade-collab-mcp` | MCP server (JSON-RPC over stdio): agents join documents as replicas through `Client` and edit with the intent APIs |

## 7. Verification strategy

Tests are written first. Three layers:

1. **Lean 4** (`lean/`) — properties of the *specifications*:
   strong eventual consistency of the RGA spec and of LWW registers,
   the sequencer preserving any validity predicate, a concrete proof that CRDT
   union of two valid merges breaks the table invariant, and soundness of the
   client fast path (convergent operations commute ⇒ applying a remote
   convergent operation to the optimistic view equals rebuilding it).
   For tables (`Grid.lean`, `CellCommute.lean`, `Anchor.lean`): a model of
   `jade_collab_core::grid::Grid` that behaves exactly like the Rust code
   (same accept/reject decisions, same resulting state) with
   `apply_preserves_valid` — every accepted row/column/merge operation keeps
   the table valid — and `check_iff_valid` for the executable checker; cell
   edits commute with every grid operation; a live table's anchor mark always
   exists and is visible, and convergent text operations never change
   whether a join or table operation is accepted. The proofs also sharpened
   the design: a cell edit commutes with grid operations *only when it is
   valid on the state it is applied to* (an edit to a row another replica is
   still creating is not), so the unconditional hypothesis of
   `fast_path_sound` does not hold for it — which is why a replica rebuilds
   its view whenever a remote convergent operation does not apply directly.
   The Lean model is also an executable oracle (`lake build grid-oracle`); a
   property test feeds it and the Rust grid the same random operations and
   compares every decision and the final state (CI job `lean-oracle`).
2. **Property-based tests** (`proptest`) — the implementation against the
   specification (RGA differential test), convergence of random concurrent
   histories across N replicas under random message scheduling, grid
   invariants under random structural operations, docx/xlsx/pptx model ↔
   jade agreement after random edit sequences, a restarted server
   indistinguishable from one that kept running and resuming within its
   kept tail exactly as one that never checkpointed, the journal reading
   back whatever was appended, checkpointed and torn and being synced as its
   policy says, only configured tokens being accepted and every route
   agreeing with an independent reading of random per-document token tables,
   the presence rate limiter agreeing with an exact GCRA, and presence
   settling on everyone's latest state.
3. **Example tests** — protocol edge cases, real fixtures through jade, and an
   end-to-end WebSocket session with a human-like and an agent-like client.

## 8. Durability, access and presence

### Durable documents

A server with a storage directory keeps, per document, the newest
**checkpoint** and a **journal** of what happened after it:

```
<dir>/<name>/snapshot-<n>.json    {"format": "docx", "checkpoint": {seq, processed, next_replica, faults, document}, "tail": [committed…]}
<dir>/<name>/snapshot-<n>.docx    the file jade exported at that seq
<dir>/<name>/log-<n>.jsonl        one record per line: joined | committed | rejected
```

The checkpoint holds the *model*, not just the file. Model identifiers are
minted when a file is loaded, so a model derived again from the exported
file would not match the identifiers every replica holds. Restoring loads the
model from the checkpoint and the file into the materializer, then replays the
journal (`Sequencer::restore`, `Sequencer::replay`). The journal records
joins (so replica ids are never reused), commits and rejections (a rejection
moves the replica's processed client sequence number, which is what a resume
reports). Each record is appended before anyone hears of it, so no replica can
be ahead of the journal; a torn last line is dropped.

Appending hands the record to the operating system, which survives the
process dying but not the machine losing power. `StorageOptions::sync`
chooses: `Fsync::Checkpoints` (the default) syncs only checkpoints;
`Fsync::EveryRecord` syncs every record before `append` returns, and so
before the commit is broadcast — an acknowledged edit is on the disk, at one
`fsync` per record. The store appends through a `JournalFile` (a `File` in
production), so a property test can watch every write and sync over random
appends, checkpoints and reopenings.

Every `checkpoint_every` records a new generation is written (file, empty
journal, then the `.json`, which is the commit point), older ones are removed
and the in-memory log is compacted — except for its **tail**: the last
`keep_tail` committed operations are kept in memory and written into the
snapshot (`Sequencer::compact_keeping`, `Sequencer::restore_with_tail`, which
refuses a tail that does not end at the checkpoint or has gaps). A replica at
most `keep_tail` operations behind the current position can therefore always
resume, across checkpoints and restarts; one further behind is told to join
again (`rejoin`). A property test runs two simulated sessions side by side,
one whose server restarts from what is on disk and one whose clients merely
reconnect, checkpointing with random tails, and requires them to agree at
every step; it also replays the same history into a sequencer that never
checkpoints and requires every resume from the kept tail on to be answered
exactly as that one answers it, and every resume from before it to be
refused at precisely the kept position.

### Access control

With a token list, every HTTP route and the WebSocket require a bearer token
(`Authorization: Bearer …` or `?token=…`, since browsers cannot set headers on
a WebSocket). A token grants a name and a role. A viewer joins, reads and
follows the log, but its connection answers each of its submissions with a
rejection itself, so nothing a viewer sends reaches the sequencer or the
journal.

A grant may list `documents`: exact names or globs over the whole name (`*`
any run of characters, `?` exactly one, case-sensitive), each with the
token's role or one of its own (`{"document": …, "role": …}`); the first that
matches decides, and a name none matches is closed. The format, in full:

```json
{
  "<token>": {
    "name": "carol",
    "role": "editor" | "viewer",
    "documents": ["contract.docx", "deck-*.pptx",
                  {"document": "notes.docx", "role": "viewer"}]
  }
}
```

`documents` is optional (every document); an empty list opens none.
`GET /documents` lists only what the token opens. Download, upload and the
socket answer 403 to a known token without access, checked before the
document is looked up (the socket before the upgrade), so a 403 does not tell
whether the document exists; upload needs the editor role on that name.

**Rotation.** The tokens file is the single source of truth; there is no
admin endpoint, which would be a second way in to defend. The server holds
its tokens in a watch channel: `Server::set_tokens` replaces them,
`Server::watch_tokens` polls a file and takes it whenever its contents change
(the CLI does so every second, and on SIGHUP), and a file that cannot be read
or parsed is reported and changes nothing. A request is checked against the
tokens in force when it arrives. Every socket keeps the token it presented
and re-checks it on each change before reading anything further from the
client (a biased select), so nothing sent after a change is judged by the
old tokens: a withdrawn token, or one that no longer opens the socket's
document, gets `revoked {message}` and a policy-violation close frame; a
changed role or name applies from then on.

### Presence

`presence {state}` carries arbitrary JSON (a cursor, a selection) and is
relayed to the other replicas of the document as
`presence {replica, name?, state}`, with `presence_left {replica}` when a
connection closes or the replica resumes elsewhere. The document's thread
keeps the latest presence in memory for newcomers. Presence never touches the
sequencer, the log or the disk. Because it shares the per-connection channel
with commits, it cannot reorder them.

Each connection is limited (`PresenceLimits`): a state of at most
`max_bytes` of JSON as relayed (16 KiB by default), and a token bucket of
`burst` updates refilled at `per_second` (40 and 20). The bucket keeps its
credit in billionths of an update, so refilling is exact integer arithmetic;
a property test compares it with the generic cell rate algorithm over random
limits and arrival times. An update over a limit is dropped by the
connection itself, before the document's thread sees it, and only its sender
gets `presence_rejected {reason}`; submissions go through the same loop
untouched.

## 9. Status and next steps

Working: the two-lane protocol over WebSockets; docx text, paragraph
split/join, table insertion/deletion and row/column structure on simple
tables, text in any table cell; text of footnotes, endnotes and text boxes
(§4; notes also split and joined), their anchors following splits, joins
and deletions as jade moves them; docx formatting (character and paragraph)
and comments (add, reply, resolve, delete; also comments loaded from the
file) as sequenced operations on id ranges resolved in log order (§4); xlsx
values, formulas (with rebasing), row/column structure, notes, cell
formatting (per-attribute LWW, A1 ranges), merged cells and sheet
management (add, rename, delete, move); pptx slide
insert/delete/move/duplicate, text in shapes, group children and speaker
notes, shape geometry, text boxes and shape deletion (hosted by the server,
the CLI and the MCP server like the other formats; a restored deck's
materializer re-learns which file id jade gave each session-made text box);
index/A1/slide-number-addressed intent APIs for people and agents; the
`jade-collab` CLI; resume after a disconnect; an MCP server
(`jade-collab-mcp`) through which agents join, read, edit and watch
documents as replicas without a shell; durable documents with checkpoints,
log compaction that keeps a tail for replicas slightly behind, and an
optional fsync per journal record; bearer-token authentication with editor
and viewer roles, per-document access and roles, and tokens rotated without a
restart (revoked sockets are closed); presence, limited in size and rate per
connection.

Not yet:

* Optimistic formatting on the convergent lane (a Peritext-style mark CRDT);
  formatting a story's last paragraph while it is empty (it has no atom to
  name it by); editing comment text; comments in notes and headers.
  Headers and footers are not modelled.
* xlsx borders, whole-row/column formats, conditional formats and threaded
  comments; note authors are not modelled (jade records `jade`).
* docx structure of ragged and nested tables (their text is editable),
  rows/columns through merged regions, tracked changes; creating and
  deleting notes and text boxes; splitting or joining a text box's
  paragraphs (jade does not write them); comments in notes; the concurrent
  orders around anchors jade cannot realise (§4).
* Access and durability: a connection's outgoing queue is unbounded (a
  client that stops reading makes it grow), there is no audit log of who
  connected with which token, and reloading tokens on SIGHUP is Unix-only
  (the file is still polled everywhere).
* Performance: RGA lookups are linear and paragraph positions are recomputed
  by walking the model on each operation; formatting and comment operations
  are preflighted on a copy of the jade document; jade's xlsx editor rereads
  the workbook on every write. Fine for the first milestone, not for large
  documents.
* pptx in the server (the crate is ready: a `PptxModel` and a `JadePptx`
  materializer with `open` / `export`), tables and formatting in slides,
  hidden slides, layouts, pictures and grouping. jade's pptx writer rewrites
  and re-reads the package on every edit.
