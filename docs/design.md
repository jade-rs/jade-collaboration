# jade-collaboration design

Real-time co-editing of `.docx` and `.xlsx` (later `.pptx`) by any mix of
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

A *story* (the body, one table cell, …) is one RGA sequence of atoms: a
character or a paragraph mark. Each atom names the atom it was inserted after
(its *parent*); deletion leaves a tombstone. The visible order is a depth-first
walk of the parent tree with siblings in **descending** identifier order.

The specification (`rga::spec`, and `lean/JadeCollab/Rga.lean`) is literally
that: the state is the *set* of insert operations plus the set of deleted
identifiers, and the document is a pure function of those sets. Lean proves
that replicas which received the same operations, in any order and with any
duplication, render the same document. The production implementation keeps a
linear array and integrates with the classic RGA skip rule; a property test
checks it against the specification on random concurrent histories.

A story's last paragraph has no mark atom (Word's final paragraph mark cannot
be deleted). Splitting a paragraph inserts a mark: convergent. Joining deletes
a mark: sequenced, because a table may be anchored to that mark.

### Tables (`jade-collab-core::grid`)

A table is anchored *after* a paragraph mark of its parent story (or at the
story start), which keeps "a story ends with a paragraph" and "a table is never
inside a paragraph" true under concurrent splits. Rows and columns are ordered
lists of identifiers with tombstones; a cell is the story keyed by
`(RowId, ColId)`. Structural deletes only tombstone, so a concurrent edit to a
cell in a deleted row still commutes — it lands in an invisible story.

Merge regions are rectangles named by their corner row/column identifiers. The
grid's validity predicate (all regions alive, well-formed and pairwise
non-overlapping) is checked by the sequencer for every structural operation.

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

Excel tables (ListObjects) rewrite what is typed into their header and totals
rows (a blank header becomes `Column1`) and name or fill new rows and columns
inserted inside them. The model cannot predict that, so those cells are
read-only and structure inside a table is refused — on every replica, from
state loaded with the file, so the refusal never needs the server.

## 5. Materialization into jade

The server owns one jade document per room and applies, for every committed
operation, the `HeadlessCommand`s (docx) or `HeadlessWorkbook` calls (xlsx)
that realise it, computing paragraph indices and byte offsets from the model at
that point of the log. jade addresses paragraphs by its flattened *paragraph
surface* index; the model mirrors the body/table part of that enumeration and a
test checks the two agree on real fixtures.

Clients do not need jade: an AI agent, a CLI or a test works on the model. A
jade editor client additionally applies the same effects to its local
`WordDocument`.

The real document can know things the model does not. A `Materializer` may
therefore **refuse a sequenced operation before it is applied**
(`Materializer::admit`); the rejection travels back like any other. jade, for
instance, lists a text box's or a note's paragraphs between two body
paragraphs, and a range across them would delete those paragraphs, so the
docx materializer refuses to join paragraphs across them. `admit` is never
consulted for convergent operations — they must commit wherever they land.

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
2. **Property-based tests** (`proptest`) — the implementation against the
   specification (RGA differential test), convergence of random concurrent
   histories across N replicas under random message scheduling, grid
   invariants under random structural operations, docx/xlsx model ↔ jade
   agreement after random edit sequences, a restarted server indistinguishable
   from one that kept running, the journal reading back whatever was appended,
   checkpointed and torn, only configured tokens being accepted, and presence
   settling on everyone's latest state.
3. **Example tests** — protocol edge cases, real fixtures through jade, and an
   end-to-end WebSocket session with a human-like and an agent-like client.

## 8. Durability, access and presence

### Durable documents

A server with a storage directory keeps, per document, the newest
**checkpoint** and a **journal** of what happened after it:

```
<dir>/<name>/snapshot-<n>.json    {"format": "docx", "checkpoint": {seq, processed, next_replica, faults, document}}
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
be ahead of the disk; a torn last line is dropped.

Every `checkpoint_every` records a new generation is written (file, empty
journal, then the `.json`, which is the commit point), older ones are removed
and the in-memory log is compacted. A replica further behind than the
checkpoint cannot resume and is told to join again (`rejoin`). A property
test runs two simulated sessions side by side, one whose server restarts from
what is on disk and one whose clients merely reconnect, and requires them to
agree at every step.

### Access control

With a token list, every HTTP route and the WebSocket require a bearer token
(`Authorization: Bearer …` or `?token=…`, since browsers cannot set headers on
a WebSocket). A token grants a name and a role. A viewer joins, reads and
follows the log, but its connection answers each of its submissions with a
rejection itself, so nothing a viewer sends reaches the sequencer or the
journal.

### Presence

`presence {state}` carries arbitrary JSON (a cursor, a selection) and is
relayed to the other replicas of the document as
`presence {replica, name?, state}`, with `presence_left {replica}` when a
connection closes or the replica resumes elsewhere. The document's thread
keeps the latest presence in memory for newcomers. Presence never touches the
sequencer, the log or the disk. Because it shares the per-connection channel
with commits, it cannot reorder them.

## 9. Status and next steps

Working: the two-lane protocol over WebSockets; docx text, paragraph
split/join, table insertion/deletion and row/column structure on simple
tables, text in any table cell; docx formatting (character and paragraph)
and comments (add, reply, resolve, delete; also comments loaded from the
file) as sequenced operations on id ranges resolved in log order (§4); xlsx
values, formulas (with rebasing), row/column structure, notes, cell
formatting (per-attribute LWW, A1 ranges), merged cells and sheet
management (add, rename, delete, move); index/A1-addressed intent APIs
for people and agents; the `jade-collab` CLI; resume after a disconnect; an
MCP server (`jade-collab-mcp`) through which agents join, read, edit and
watch documents as replicas without a shell; durable documents with
checkpoints and log compaction; bearer-token authentication with editor and
viewer roles; presence.

Not yet:

* Optimistic formatting on the convergent lane (a Peritext-style mark CRDT);
  formatting a story's last paragraph while it is empty (it has no atom to
  name it by); editing comment text; comments in notes and headers.
* xlsx borders, whole-row/column formats, conditional formats and threaded
  comments; note authors are not modelled (jade records `jade`).
* docx merged cells and structure of merged/ragged/nested tables (their
  text is editable; the model already validates merges in `Grid`), text
  boxes, notes, tracked changes.
* Excel table (ListObject) structure and header edits.
* Durability and access, beyond what §8 covers: per-document permissions
  (a token is valid for every document), token rotation without a restart,
  an fsync per journal record (records are written to the OS at once; only
  checkpoints are fsynced), keeping a tail of the log after a checkpoint so
  that briefly disconnected replicas need not rejoin, and limits on the size
  and rate of presence updates.
* Performance: RGA lookups are linear and paragraph positions are recomputed
  by walking the model on each operation; formatting and comment operations
  are preflighted on a copy of the jade document; jade's xlsx editor rereads
  the workbook on every write. Fine for the first milestone, not for large
  documents.
* `.pptx`.
