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
  split, character formatting (Peritext-style marks over the RGA) and cell
  values (last-writer-wins registers).
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
order. Anchors are zero-width and are never deleted as text: `DeleteText`
refuses them, so a concurrent deletion of the text around one cannot take
it, and a split or join moves it with the text around it. Note separators
are not modelled.

**Creating and deleting** them is sequenced. `InsertNote` (footnote or
endnote) and `InsertTextBox` put an anchor atom after a caret's atom of a
body or table-cell story — jade inserts them nowhere else — and create the
story it places, with every identifier derived from the operation's `id`
(the anchor and the story are `id`, the characters `id.offset(1)`, …), so
every replica builds the same thing. A new note holds what jade writes into
one: a tab after the number, then the text; a new text box (jade's
`insert_shape`, a rectangle 12–4096 pixels a side) one paragraph of text.
`DeleteNote` tombstones every reference to the note and the note,
`DeleteTextBox` the box's anchor; the stories stay, invisible, so typing
into them that was concurrent with the deletion still applies and commutes
(as with a deleted table's cells), and the comments in a deleted text box go
with it, as in jade. jade numbers a new note itself (reusing freed numbers);
the server commits the number as a derived operation (§5), so the walk —
which lists unreferenced notes by number — and a restored server both know
it.

Whether a note or text box is accepted depends only on structure that
convergent edits cannot change (its story, its parent atom, its id):
the session property test showed that refusing one beside another anchor —
a rule that depends on the text around it — let a replica's optimistic view
differ from rebuilding it once a concurrent deletion arrived. It goes
wherever its parent atom puts it, among the anchors at its offset too.
jade puts a new reference or shape *before* the anchors already at its
offset, so the materializer reaches any other place by typing a placeholder
after the anchors that come first, inserting right after it and deleting it
again.

With tracked changes: creating or deleting a note or text box is not a
revision (Word tracks neither as one, and jade's commands write none), but
text typed into one while tracking is on is; tracked text that lands among
the anchors at an offset is typed right after the placeholder with jade's
revision session on (jade's `insert_text_among_anchors` writes plain text).
Deleting a note or text box takes the tracked changes inside it with its
story. Both tracks commit origin operations — the authors the server names
(`SetAuthor`, committed directly) and jade's numbers (`Numbered`, derived) —
and both sit in the journal and the checkpoint's model, so a restored
server keeps both (`tests/notes_tracked.rs`).

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

* jade's text offsets alone cannot name a side of a zero-width run. Typing
  at an offset where an anchor sits goes right after the character before
  it — *before* the anchor — except at a paragraph's start, where it goes
  after the anchors there. The intent API resolves a caret the same way (at a
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

A text box's paragraphs split and join like any story's: jade-core used to
change only its in-memory model and kept the original paragraphs in the
file, and now writes them (`w:txbxContent` of the DrawingML box and its VML
fallback). Formatting works in every story. jade cannot anchor a comment in
a note — its `add_comment`, `reply_to_comment` and `delete_comment` edit
`document.xml` only, and a note's paragraphs and runs carry no XML binding
into the notes part, which jade rewrites from the runs by position — so the
model refuses comments in notes on every replica; supporting them needs that
binding in jade-core first. jade's `add_comment`
(and a reply, which it anchors the same way) splits the runs of the
paragraph it anchors to without relisting that paragraph's text boxes,
whose paragraphs then point at a stale run until the
file is read again; the materializer has jade relist them by restoring
jade's own snapshot. A jade-core bug surfaced here — text typed after a note
reference or text box alone in its paragraph went into that run, and the
next edit that split the run (formatting, a comment) copied the reference
into both halves — and is fixed on jade-core's `feat/paragraph-story-api`
with the story API the model needs (`paragraph_story`,
`paragraph_story_surface`, `paragraph_anchors`). Creating notes and text
boxes surfaced more, fixed on jade-core's `feat/collab-notes-comments`
(jade-rs/jade-core#31): text box paragraph splits and joins never reached
the file; a note paragraph split after a body split wrote its halves to
the wrong paragraphs; `insert_shape` read its offset as one into the runs'
text rather than a rendered offset, so a shape after a note reference
landed a character too far on; and two commands were missing —
`delete_text_box`, and `insert_text_among_anchors` (below).

**Concurrency around anchors.** jade's placement depends on what it sees
when an edit arrives, the RGA's does not: text typed after a character that
was concurrently deleted, right after an anchor, belongs after the anchor;
text typed where a concurrent split put an anchor at the start of the new
paragraph belongs before it; and once a deletion brings two anchors to one
offset, text typed after the deleted character lands *between* them. jade's
typing puts text before all the anchors at an offset (at a paragraph's
start after them), so the materializer writes every other order with
jade-core's `insert_text_among_anchors`, which names how many of the
anchors come first. A split that must leave some of the anchors at its
offset in the first half (a split right after a concurrently deleted
character that followed an anchor) goes through the same placeholder as a
note placed after them: jade's split takes the anchors at its offset to
the second half, so the materializer types a placeholder after the ones
that stay, splits right after it and deletes it. A paragraph that ends
with an anchor is joined by deleting the break from the next paragraph's
start: jade's ranged join starts at the rendered end, before the anchor,
and dropped it. The materializer thereby realises every order the model
produces; a property test runs concurrent sessions, server restarts
included, and requires no materialization fault.

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

**Rows and columns through merged regions** behave as in Word and Excel, and
as jade does in both formats (pinned by probe tests that drive jade's own
commands, `jade_table_merges.rs` and the xlsx merge tests):

* a row (column) inserted *strictly inside* a merge — after its top row or
  a row below it, before or at its bottom row — joins it: the merge grows;
  one inserted above its top or below its bottom does not;
* deleting a line takes it out of every merge: deleting a merge's top or
  bottom (left or right) line moves that edge inward to the next alive line,
  deleting an inner line shrinks it;
* a merge left with fewer than two cells is dropped — deleting the last line
  it had on an axis, or collapsing it to a single cell, which is then an
  ordinary cell.

The grid needs no special case for this: a merge is named by its corner
lines, so a line inserted between the corners belongs to it, and deletion
normalizes each merge to the first and last alive lines between its corners
(the corners are what `Grid::merge_at`, the merge covering a cell, reads).
Lean pins the behaviour down on the cells a merge covers
(`Grid.insertRow_covers`, `insertCol_covers`: corners kept, every other line
in or out as before, the new line inside exactly when inserted after a
merge line other than its last; `deleteRow_covers`, `deleteCol_covers`: a
surviving merge covers exactly its old alive cells minus the deleted line,
and a merge is dropped exactly when fewer than two cells are left;
`mergeAt_spec`: in a valid grid one merge at most covers a cell).

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
test `merges_commute_with_cell_text`). Rows and columns go through merged
cells as the grid says (above), and the layout follows jade's `w:tc`: a new
line's cells a merge covers get a story that no position shows (the new
row's cell in the merge's left column is an empty follower, as Word's
`w:vMerge` continuation is); deleting a merge's top row deletes what the
merged cell shows, which lives in that row's `w:tc`, and the follower below
becomes the merged cell; deleting its left column keeps the `w:tc`, one
column narrower, so what the merged cell and its followers show moves to
the next column. Stories still never move, so these commute with cell
text too (`structure_through_merges_commutes_with_cell_text`). Tables whose merges come from the file
(`w:gridSpan`, `w:vMerge`) load unlocked; ragged or nested tables stay
structurally read-only. A jade-core bug surfaced here — a merge wrote an
empty paragraph into each follower's XML but not into the model, so a save
added a paragraph — and is fixed in jade-rs/jade-core#12.

### Formatting and comments (`jade-docx-collab`)

**Character formatting is convergent: marks (`jade-collab-core::marks`).**
Bold, italic, underline, strike, font size, font family, font colour and
highlight — simple run properties whose new value replaces the old one —
travel as `Mark { story, id, start, end, change }`, a Peritext mark (Litt,
Lim, Kleppmann, van Hardenberg 2022). `start` and `end` are *boundaries*: a
side of an atom of the story's RGA (`before` / `after` a character, a
paragraph mark or an anchor) or an end of the story. RGA atoms are never
removed — deleted text is a tombstone — and inserts never reorder the atoms
that are there, so a mark's anchors never vanish and never move. An atom is
covered when it lies strictly between the boundaries in the story's order,
tombstones included, and for each attribute it shows the covering mark with
the greatest id (last-writer-wins per attribute and character). The state
is the *set* of marks, so marks commute with each other and with every text
edit: a replica applies a remote mark straight onto its view, and replicas
converge on every character's formatting whatever order the operations
travel in (Lean: `Marks.format_converges`).

*Expand rules* are in the anchors, chosen by the intent API when the mark is
made: a mark over the characters `first … last` starts `before first`
(typing in front of formatted text is not covered) and, for every one of
these attributes, ends `before` the atom that followed `last` in the
story's order — or at the story's end — so text typed at the end of
formatted text takes its formatting, as in Word. A link-like attribute would
end `after last` instead (`Expand::None` in the core; no docx attribute
uses it yet). Lean proves the consequences for any insert, concurrent with
the mark or not, whose typist had seen the atoms already typed after the same
character: inside the range covered, right after the last character covered
exactly when the attribute expands, right before the first never
(`typed_inside_rga`, `typed_after_end_rga`, `typed_before_start_rga`), and
inserting never changes the formatting of existing text
(`effective_preserved_rga`, with no proviso). The proviso is the RGA's: a
character typed after `last` by a replica that had not seen a newer
character typed there lands after that one — after the mark's end, so it is
not covered (`concurrent_typing_lands_after_unseen_sibling`). The model refuses a
mark whose anchors it does not have, that covers nothing, that reuses an id
or carries an invalid value — all decided the same way on every replica.

**Loaded formatting: jade is the base, marks are overrides.** The model
stores only the marks made in the session. A character no mark of an
attribute covers keeps what jade has for it: the file's formatting, jade's
typing rules for text typed in the session, and the sequenced formatting
below. The server's jade document holds exactly that *base* — every text
and structural edit and every sequenced format, in log order, but no marks.
The marks are realised when the file is written (`JadeDocx::formatted`,
`JadeDocx::export(model)`): a copy of the base gets each maximal run of
characters a mark sets (`DocxModel::formatting`) as an explicit value. jade's
typing rule therefore only ever sees base formatting, never a mark's run:
text typed right before bold text at a paragraph's start — where jade alone
would copy the bold run — stays as its surroundings are, and text typed at
the end of a bold mark is bold because the mark covers it, not because jade
guessed. A test checks after every random edit, in concurrent sessions and
after export that the written file shows every character's mark values and,
for every attribute no mark sets, jade's own value. A checkpoint stores the
base file next to the model (which holds the marks); downloads get the
formatted file. Where no mark sets an attribute the model promises only
"jade's base": for text typed in the session that is jade's typing rule
applied in log order on the server (the neighbour's base formatting), which
replicas without jade do not predict — as before marks existed. Making it
fully determined would take loading the file's run formatting as origin
marks, and jade-core commands that remove one direct property (a mark
falling back to the style's value); not done.

**Sequenced formatting stays for what is not a simple attribute.**
`Format { story, start, end, change }` — paragraph alignment, paragraph
style and clearing (and, for older clients, any character change, which
then edits the base) — is resolved **when it is applied, in log order**:
`start` and `end` are atom ids (characters, or a paragraph mark to reach an
empty paragraph) of one story, inclusive, and the range is the visible
atoms between them. Characters deleted before it commits shrink it,
characters inserted inside it before it commits are covered, and a range
with nothing visible left does nothing. A character change becomes one jade
command per paragraph slice; a paragraph change one per paragraph holding a
visible atom of the range. The model does not store this formatting: it
validates the value (values jade would write verbatim but Word cannot read,
such as a hex highlight, are refused on every lane) and reports where jade
must format, and jade applies Word's own rules — a paragraph style
re-cascades run properties, which explicit mark values then override, like
direct formatting in Word.

**Clearing** is both. The intent API turns a clear into a convergent
`Mark { change: ClearFormatting }`, which *unsets* every attribute but the
highlight on its range (Word's Ctrl+Space keeps the highlight) — a mark
whose value is "back to the base", expanding like the attributes it unsets
— plus a sequenced `Format { ClearFormatting }`, which clears the file's
direct formatting in jade at its log position. The unset mark takes part in
last-writer-wins like any other: marks older than the clear lose to it on
its range, a newer mark wins over it, and a concurrent one is ordered by
id, the same everywhere. The sequenced half needs no id and cannot conflict
with marks: it only changes the base.

Comments are sequenced too. `AddComment` anchors to the visible characters of
an id range (refused when none are left); `ReplyToComment`, `ResolveComment`,
`UpdateComment` and `DeleteComment` name a comment by its model id. The model
keeps a comment table (thread root, text, resolved, deleted, and jade's
`w:id`); like Word and jade, threads are flat (a reply to a reply answers the
root) and deleting a root deletes its replies. A comment's text is one
value replaced by `UpdateComment` — the last in log order wins — rather than
an RGA story: jade replaces a comment's text as a whole and keeps no
paragraphs in it, so a character-level merge would have nothing to realise
it. Text jade would not read back as written — it keeps line breaks and
whitespace at the ends while the file is open, but reads them back as
spaces and trimmed — is refused for every comment operation, so a restored
server and the replicas never disagree on it.

jade numbers the comments (and notes) made in the session itself, reusing
freed numbers, so the materializer records the number each one got instead
of predicting it, and the server commits it as a derived `Numbered`
operation right after the one that made it (§5). The number is then in the
model, every replica's and the checkpoint's: a server restored from a
checkpoint and the file it exported, whose jade has just read that file,
still addresses every comment and note (a property test restarts the
server at random points of concurrent sessions). Replaying a journal checks
that jade numbered alike.

`admit` preflights sequenced formatting, comment, note and text box
operations on a clone of the jade document and refuses what jade cannot do
— for instance a reply whose root's text was deleted (jade re-anchors
replies on the root's text). The agreement property test fails if jade
refuses a note, text box or comment edit the model accepts. Marks are never
preflighted: they must commit wherever they land, and the model refuses
everything jade could not write (invalid values) itself.

### Tracked changes (`jade-docx-collab`)

Word's Track Changes in a shared document, edited by people and agents at
once. What jade does, found by probing it (`tests/tracked_changes.rs`):
with a revision session (`start_change_tracking`, one author and date for
the whole document) typed text becomes a `w:ins` run of its own (a new
`w:id` per edit, so two authors typing into each other's insertions stay
apart), and a deletion becomes a `w:del` run whose text stays in the
paragraph — every jade offset counts deleted text. Deleting text that is
already deleted keeps its first author, and deleting with tracking off
leaves deleted text alone (it goes only when its deletion is accepted).
Four things jade got wrong, all fixed on jade-core's
`feat/collab-tracked-changes` (below): deleting inserted text *replaced*
the insertion by the deletion, so rejecting all changes kept text nobody
had accepted (Word nests `<w:ins><w:del>`); text typed with tracking off
at the end of an insertion joined it; a revision could be accepted or
rejected only as whole runs; and a move decided after its paragraph was
joined kept its `w:moveTo` in the file. jade also does
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
number format, horizontal/vertical alignment, wrap, the border edges —
exactly jade's `FormatChange`): jade's `set_format` is a partial change that keeps every
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

**Borders** (jade-rs/jade-core#21 added them to jade's `set_format` and
`format_at`) are five more attributes, one per edge — left, right, top,
bottom and the diagonal — each a register holding the line's style *and*
colour, plus one per diagonal direction. Style and colour share a
register because they are one look: a newer automatic-colour line must
not keep an older line's red. Different edges are different registers,
so one person drawing a cell's top edge while another draws its left edge
both survive. `SetBorders` is Excel's border dialog over a range — the
outline, the lines inside, single edges, the diagonals — expanded into
per-cell edges by each cell's place in the row and column lists the
operation names (never by the grid it lands in), so it stays convergent;
the server realises it with one `set_format` per rectangle of cells that
won the same edges. Excel stores a border on each cell, so a line between
two cells may be the bottom edge of one and the top edge of the other;
like Excel, `SetBorders` draws an inside line on both sides and the
outline only on the range's own cells. On structure edits the model
follows jade, which here differs from Excel's default: Excel gives an
inserted row the format of the row above ("Format Same As Above"), jade
leaves it unformatted, so a row inserted inside a box is open on the
sides — the model knows no format for its cells, as for any line made
after a range was drawn. A deleted edge row takes its edge with it.
Merging copies the anchor's edges into the covered cells, as jade (and
Excel) copy the anchor's style; jade reports a merged cell's borders at
the anchor, and so does the model.

**Whole rows and columns** (Excel's `<row s=…>` / `<col style=…>`, which
jade writes for `set_format` on `3:5` or `B:D`, together with every stored
cell and row/column intersection on them) are registers per line and
attribute, written by the convergent `SetLineFormat`. A cell shows, per
attribute, the newest write among its own, its row's and its column's.
jade only sees the writes that won their register, in log order, and a
line write patches every cell of the line, so when it arrives the server
puts back, cell by cell, the attributes where a newer cell or crossing
line write shows; a cell write older than its row's or column's reaches
jade not at all. Lines inserted later behave as in jade: a new row has no
row format and shows its columns' formats, a new column shows its rows'
(jade widens a multi-column `<col>` span around a column inserted inside
it; the model does not claim that format, so the two never disagree on
what it knows). A merge copies what shows at the anchor, its row's and
column's formats included.

jade reports a merged cell's format at the merge's anchor, so the model
loads the file's merges into the sheet grid, which moves them exactly as
jade does (the rules above: a line inserted inside joins the merge, a
deleted corner moves to the next line, a single cell is no merge — jade's
reference shifter drops a range that shrinks to one cell). The agreement
tests compare every sheet's merged ranges with jade's. Formatting a covered cell is kept and shows once a
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

A docx mark has no effect on jade's document: the server's jade holds the
base formatting and the marks are realised on a copy whenever the file is
written (§4, "Loaded formatting"), so jade's own typing rules never act on
a mark's formatting.

Clients do not need jade: an AI agent, a CLI or a test works on the model. A
jade editor client additionally applies the same effects to its local
`WordDocument`.

**Derived operations.** The real document also *does* things the model
cannot predict (Excel naming a new table column). After committing an
operation the sequencer asks the materializer what jade did on its own
(`Materializer::derived`) and commits each answer as an operation of the
document's origin (replica 0), right after its cause; the server journals
and broadcasts them like any commit, and a replayed journal applies them
without asking again. The docx materializer derives the numbers jade gives
new comments and notes this way. Only the origin may commit them (the model refuses
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
text (rejecting an insertion, deleting outright) is an untracked
`ReplaceRange`, except for text a deletion holds, which jade's deletion
leaves: it goes by accepting that deletion. Accepting an insertion or a
deletion and restoring a deletion are jade's `accept_revisions` /
`reject_revisions`
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
* A mark's boundaries are chosen by its author and travel with it. Deriving
  them where an operation is applied — "before the atom after the range"
  computed from a sequenced clear's ids — would put a concurrent insert on
  one side of the boundary on a replica that applied the insert straight
  onto its view and on the other side in the rebuilt view; so a clear's
  unset half is a convergent mark of its own.

## 6. Components

| Crate | Role |
| --- | --- |
| `jade-collab-core` | identifiers, Lamport clock, RGA (+ executable spec), marks (Peritext formatting over the RGA, + executable spec), LWW register, table grid |
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
   the table valid — and `check_iff_valid` for the executable checker; rows
   and columns go through merged regions as in Word and Excel, stated on the
   cells each merge covers (`insertRow_covers`, `insertCol_covers`,
   `deleteRow_covers`, `deleteCol_covers`, `mergeAt_spec`); cell
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
   compares every decision, the final state and which merge covers every
   alive cell (`mergeAt` against `Grid::merge_at`; CI job `lean-oracle`).
   For formatting (`Marks.lean`): a character's formatting is a function of
   the sets of inserts and marks (any order, any duplication; deletions play
   no part), so rich text converges (`format_converges`,
   `richRender_converges`); inserted atoms never reformat existing ones
   (`effective_preserved`); a new atom gets exactly the formatting of the
   gap it lands in (`effective_in_gap`), and for the marks the intent API
   makes that means: inside the range covered, right after the end covered
   iff the attribute expands, right before the start never (`typed_inside`,
   `typed_after_end`, `typed_before_start`, `typed_at_story_start`). The
   RGA's own part is proved from `Rga.walk` (`RgaWalk.lean`,
   `MarksRga.lean`) for every state `Rga::insert` can reach (unique ids,
   parents present and older): a later state keeps the relative order of
   existing atoms (`extends_order`), and an atom lands right after the atom
   it was typed after when it is newer than the atoms already typed there
   (`inGap_order`; `concurrent_typing_lands_after_unseen_sibling` shows the
   proviso is needed), so the expand rules hold for RGA histories without
   hypotheses (`typed_after_end_rga`, …). `marks-oracle` runs the
   Lean RGA walk and `effective`; a property test compares it with the Rust
   `Rga` + `MarkSet` on random concurrent histories (same CI job).
   For tracked changes (`Revision.lean`): a model of the per-character
   state (tombstone, insertion revision, deletion set) and of every
   operation on it, with `convergent_comm` — each convergent operation
   (tracked deletion, removal, insertion of fresh characters) commutes with
   every operation that does not decide the revision it creates, sequenced
   accept/reject included — `convergent_grows` (deletion sets only grow on
   the convergent lane), `converges` / `shown_converges` (any order of the
   same tracked deletions and removals gives the same state, hence the same
   attributed revision) and `rejectDel_keeps_others`.
2. **Property-based tests** (`proptest`) — the implementation against the
   specification (RGA and mark-sweep differential tests), convergence of
   random concurrent histories across N replicas under random message
   scheduling (text and marks peer to peer, and docx formatting through the
   server, whose written file must show every character's formatting), the
   expand rules for concurrent typing at a mark's edges, grid
   invariants under random structural operations and the covered cells of
   every merge following Word's rules under them, docx/xlsx/pptx model ↔
   jade agreement after random edit sequences (docx tables compared cell by
   cell and xlsx merged ranges compared, with generators biased to rows and
   columns through merged cells; every docx character's tracked insertion
   and deletion, with author and date, compared too), convergent docx
   operations commuting with every text and revision operation,
   concurrent xlsx border, cell, row and
   column formats ending — on every replica and in jade — at the newest
   write per cell, edge and attribute, a restarted server
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

### Slow connections

The document's thread never waits on a connection. Each connection has a
bounded queue of outgoing messages (1024 by default, `--outgoing-limit`);
the thread offers each message without blocking, and a message that does not
fit means the client reads too slowly. The thread then stops sending to that
connection (it leaves the subscribers, so the others are not held up), and
the connection says `overloaded {message}` and closes with 1013 (try again
later). Its replica is not lost: it resumes on a new connection like after
any disconnect and catches up from the log (or rejoins if what it missed was
compacted). A test floods a client that reads nothing with presence and
checks that the flooding client's operation still commits, that the slow one
is told and closed without being sent everything, and that it resumes; a
mutation that stops reporting the overflow fails it.

### Audit log

`--audit-log FILE` appends one JSON object per line for every access, each
with `at` (milliseconds since the Unix epoch) and `event`: `connected`
(document, token name, role), `joined` / `resumed` (the replica), `closed`
(the replica and why: `client`, `revoked`, `overloaded`, `stopped`),
`refused` (status 401 with the request path, or 403 with the document and
token name), `uploaded` and `downloaded`. A token is recorded by its grant's
name and never by itself, and request URLs are reduced to their path, since
a socket may carry its token in the query; a test checks that no secret
reaches the file.

## 9. Status and next steps

Working: the two-lane protocol over WebSockets; docx text, paragraph
split/join, table insertion/deletion and row/column structure (through
merged cells too, as Word does), merging and unmerging cells, text in any
table cell; text and paragraphs of footnotes, endnotes and text boxes
(§4), their anchors following splits, joins and deletions as jade moves
them, in every concurrent order; creating and deleting footnotes, endnotes
and text boxes; docx character formatting as convergent
Peritext marks (bold, italic, underline, strike, size, family, colour,
highlight; expanding at their end; newest wins per attribute; realised on
the written file over jade's base formatting) and clearing as an unset mark
plus a sequenced clear of the file's formatting; paragraph formatting and
comments (add, reply, resolve, edit, delete; also comments loaded from the
file; in the body, table cells and text boxes) as sequenced operations on id
ranges resolved in log order (§4), with jade's numbers for new comments and
notes committed as derived operations so a restored server keeps
addressing them;
docx tracked changes (§4): a document-wide tracking mode, tracked typing
and deleting by people and agents with the server naming each revision's
author from the token, Word's own-insertion rule, concurrent tracked
deletions converging on the least revision, accepting and rejecting whole
revisions by id, paragraph range or all, a file's revisions (moves
included) loaded, listed and decided, and every character's revision
agreeing with jade and the written file; xlsx
values, formulas (with rebasing), row/column structure, notes, cell
formatting (per-attribute LWW, A1 ranges), cell borders (a register per
edge; Excel's outline/inside lines over a range), whole-row and
whole-column formats, merged cells and sheet
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
connection; a bounded outgoing queue per connection (a client that reads too
slowly is closed and resumes); an audit log of every access.

Not yet:

* Marks for attributes with structure: links and hyperlink styles
  (non-expanding marks; the core supports them), character styles, comments
  as marks (they are sequenced); a mark cannot set the formatting of an
  empty paragraph's mark (Word's paragraph-mark run properties). A jade
  editor client would realise marks the way the server's writer does
  (`JadeDocx::formatted`), not by applying them to its own document.
  `read_document` (MCP) does not report formatting yet.
  Formatting a story's last paragraph while it is empty (it has no atom to
  name it by); comments in notes (jade-core's comment commands edit
  `document.xml` only, and note runs carry no XML binding) and headers.
  Headers and footers are not modelled.
* xlsx conditional formats and threaded comments; borders on whole rows
  and columns through `set_borders` (`set_format` takes edges for them);
  the formats a file already had are not loaded into the model, so reads
  list only what was written in the collaboration; note authors are not
  modelled (jade records `jade`).
* Tracked changes: paragraph splits and joins, tables and formatting are
  not tracked (jade cannot yet decide every paragraph-mark revision, and
  formatting revisions — `w:rPrChange` — are not modelled; convergent
  marks format deleted and inserted text alike, untracked); a file's
  paragraph-mark and formatting revisions are kept in jade but neither
  shown nor decided; the tracking mode is the model's and is not written to
  `settings.xml` (`w:trackRevisions`) nor read from it; decisions take whole
  revisions, not part of one; jade's own editor still marks one's own
  insertion deleted instead of removing it (the intent API removes it).
* docx structure of ragged and nested tables (their text is editable);
  text boxes inside text boxes or notes, and their geometry.
* Access: reloading tokens on SIGHUP is Unix-only (the file is still polled
  everywhere); the audit log is a plain file that is never rotated.
* Performance: the RGA is indexed (§4), but paragraph positions are still
  recomputed by walking the model on each operation; sequenced formatting, comment,
  note and text box operations are preflighted on a copy of the jade document; marks are
  realised on a fresh copy of the base document every time the file is
  written (one jade command per run of one value), and their coverage is
  recomputed by a sweep over the story (`O((n + m) log m)`) when asked;
  jade's xlsx editor rereads the workbook on every write. Fine for the first
  milestone, not for large documents.
* pptx tables and formatting in slides, hidden slides, layouts, pictures
  and grouping. jade's pptx writer rewrites and re-reads the package on
  every edit.
