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

Excel tables (ListObjects) rewrite what is typed into their header and totals
rows (a blank header becomes `Column1`) and name or fill new rows and columns
inserted inside them. The model cannot predict that, so those cells are
read-only and structure inside a table is refused — on every replica, from
state loaded with the file, so the refusal never needs the server.

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
jade addresses paragraphs by its flattened *paragraph surface* index; the
model mirrors the body/table part of that enumeration and a test checks the
two agree on real fixtures. For pptx, effects name slides by position and
text by paragraph and byte offset; the materializer turns those into jade's
UTF-16 ranges on jade's own text.

Clients do not need jade: an AI agent, a CLI or a test works on the model. A
jade editor client additionally applies the same effects to its local
`WordDocument`.

The real document can know things the model does not. A `Materializer` may
therefore **refuse a sequenced operation before it is applied**
(`Materializer::admit`); the rejection travels back like any other. jade, for
instance, lists a text box's or a note's paragraphs between two body
paragraphs, and a range across them would delete those paragraphs, so the
docx materializer refuses to join paragraphs across them; the pptx
materializer dry-runs shape deletions, slide deletions and duplications and
text-box insertions on the current file. `admit` is never consulted for
convergent operations — they must commit wherever they land; the pptx model
therefore refuses text and geometry jade would refuse (characters, the
32-bit EMU range, locks) with jade's own validators, on every replica.

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
| `jade-collab-sync` | `Document` trait, lanes, wire protocol, `Sequencer` (server core), `Replica` (client core), deterministic network simulator |
| `jade-docx-collab` | docx model + operations, loading from a jade `WordDocument`, materialization to `HeadlessCommand`s, index-addressed intent API (migrated from jade-core) |
| `jade-xlsx-collab` | xlsx model + operations, loading from / materializing into a jade `HeadlessWorkbook` |
| `jade-pptx-collab` | pptx model + operations (slides, shape and notes text, geometry, text boxes), loading through jade's reader/writer, materialization through jade-pptx-writer's `EditSession`, slide-number / shape-id intent API |
| `jade-collab-server` | WebSocket server (one actor thread per document, since jade documents are `!Send`), WebSocket client, `jade-collab` CLI |

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
   invariants under random structural operations, and docx/xlsx/pptx model ↔
   jade agreement after random edit sequences.
3. **Example tests** — protocol edge cases, real fixtures through jade, and an
   end-to-end WebSocket session with a human-like and an agent-like client.

## 8. Status and next steps

Working: the two-lane protocol over WebSockets; docx text, paragraph
split/join, table insertion/deletion and row/column structure on simple
tables, text in any table cell; xlsx values, formulas (with rebasing), and
row/column structure; pptx slide insert/delete/move/duplicate, text in
shapes, group children and speaker notes, shape geometry, text boxes and shape
deletion (model and materializer; not yet hosted by the server);
index/A1/slide-number-addressed intent APIs for people and agents;
the `jade-collab` CLI; resume after a disconnect.

Not yet:

* Formatting (bold, styles, …) — planned as sequenced operations addressed
  by character ids, then a Peritext-style mark CRDT for the convergent lane.
* docx merged cells and structure of merged/ragged/nested tables (their
  text is editable; the model already validates merges in `Grid`), text
  boxes, notes, comments, tracked changes.
* xlsx merges, sheets added/renamed, Excel table structure.
* An MCP server so agents join as replicas without a shell.
* Persistence of the log (today: export on demand, `--save` on exit),
  snapshots and log compaction, authentication and authorization, presence.
* Performance: RGA lookups are linear and paragraph positions are recomputed
  by walking the model on each operation; jade's xlsx editor rereads the
  workbook on every write. Fine for the first milestone, not for large
  documents.
* pptx in the server (the crate is ready: a `PptxModel` and a `JadePptx`
  materializer with `open` / `export`), tables and formatting in slides,
  hidden slides, layouts, pictures and grouping. jade's pptx writer rewrites
  and re-reads the package on every edit.
