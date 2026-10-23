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

* **Convergent lane** — invariant-confluent operations. They commute with other
  convergent operations, so a replica applies them immediately and never rolls them
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
                                               │ materialize into jade   │                          convergent-only pending: view.apply
                                               │   (WordDocument / xlsx) │                          sequenced : view = confirmed + pending
                                               │ append to log           │
                                               └────────────────────────┘
                                                   └─▶ Rejected{client_seq, reason} (sequenced lane only)
```

Why keep a CRDT when a server orders everything anyway?

1. **No index transformation.** Operations name the characters, rows and
   columns they touch, so an operation computed against an old view is still
   meaningful against a newer state. There are no OT transform functions.
2. **Typing-only queues use a fast path.** Remote convergent operations are
   applied straight to the optimistic view while pending operations are all
   applied convergent ones. A pending sequenced guard can depend on text: deleting
   its complete range invalidates a tentative comment. Mixed or previously
   inapplicable queues are therefore replayed on the new confirmed state.
   `lean/JadeCollab/Reconcile.lean` proves the shortcut with its actual pending
   commutation condition and the guarded choice between shortcut and replay.
3. **Rejections are rare and local.** Only sequenced operations can be
   rejected, when the authoritative state no longer admits them.
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

Identifiers have bounded `u64` Lamport time. `Id::last_in_run` and
`Clock::try_reserve` check a run's full inclusive range before allocating or
mutating anything. RGA insertion, DOCX note/text-box creation and merge/unmerge
stories, and XLSX sheet creation refuse overflowing ranges atomically.
`Replica::try_reserve`, `Client::try_reserve` and the formats' `try_resolve`
APIs propagate exhaustion as a typed error; CLI and MCP use them so an extreme
peer timestamp cannot panic their allocator or poison the replica mutex.
A resolver returns no partial operation list if a later reservation fails
(earlier reserved but unused identifiers may remain consumed).
`IdAllocation.lean` proves the bounded allocation model's range, freshness
and failure properties; its executable oracle is compared with Rust on
stateful histories biased toward the integer limits.

`Document::validate_submission` is a pure admission boundary for new user
operations. Native formats check every explicitly minted ID and LWW stamp
against the submitting replica (`check_id_owner`). Targets, parents, existing
row/column IDs, story/shape IDs and revision decisions remain shared references.
DOCX zero-allocation merges/unmerges do not check their unused `first` sentinel.
The exhaustive per-format matches force a policy decision when an operation is
added. This does not establish global freshness of timestamps or prevent an
editor from intentionally using its own extreme Lamport time.

`Replica::submit` checks before changing the optimistic view or ticket;
`Sequencer::submit` checks after processing the ticket but before even asking
whether the native materializer needs staging. A refusal therefore changes
only the processed watermark and is journaled/checkpointed like other refusals.
The socket checks before DOCX author registration as well. It deduplicates
processed tickets before decoding or registering an author: changing a refused
payload after restart cannot create log or metadata effects.

Committed replay/downstream facts and origin-derived operations bypass this
new-submission policy. Existing historical IDs keep their original logical
meaning; imported IDs and origin-generated numbering remain usable. Logical
application still checks their actual validity. Rust field-mutation matrices
cover all minted fields across DOCX/XLSX/PPTX, including multi-ID tables and
LWW stamps, while stateful native-model tests mix forged/valid submissions,
duplicates, resume and compact/restore against independent text/register
references. Raw socket tests check native file equality on refusal and legitimate
reuse by each owner; tracked DOCX tests restart through both checkpoint and
journal paths and check author-free refusals and duplicate replay. Lean's
`AllocationOwner` specifies namespace checks, reference independence, refusal
metadata and duplicate suppression; field extraction and native IO are checked
by Rust rather than proved.


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
jade's own snapshot.

The native run mapper skips `w:moveFrom`/`w:moveTo` although its parser lists
their runs. That shifted the binding of a following text-box host, so a tracked
deletion inside the box appeared in memory but vanished from the exported file
after a restart. The adapter maps against a private XML view whose move-wrapper
names are insertion/deletion names, with all node IDs and positions unchanged.
Only the model's bindings are refreshed before commands; actual XML wrappers
and move revision kinds are preserved. A minimized regression and the concurrent
editing PBT check the text, tracked layers and exported/reloaded document.

A jade-core bug surfaced here — text typed after a note
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

The mode loads from the related settings part's `w:trackRevisions` flag,
including explicit on/off values. `SetTracking` updates that flag while
preserving other settings; a missing settings part is created with its OPC
relationship and content type. Checkpoints store both the model and the
base file with the same mode. Older checkpoints whose base file predates
this support restore the model's authoritative mode before replaying edits.

**Convergence.** The state is (tombstones, `inserted`, `deleted`): set
union, map insertion of fresh keys, and removals keyed by a revision id.
These convergent revision operations commute with each other and with revision
decisions, provided a decision names a revision
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
attribute is the newest one. The model also imports the file's editable
base styles (default, column, intentional row, explicit cell), with the
cell > row > column > default priority used by jade. Source style indices
are interned: a whole-column default does not allocate cell registers for
every row. A source style is a complete layer, including unresolved
attributes; an automatic cell colour must not fall through to a red row
colour. Later collaborative writes override only the attributes they name.
`lean/JadeCollab/Style.lean` proves the source precedence, fresh-write
dominance and convergence of cross-layer updates; native fixture comparisons
and stateful property tests check the style projection and its persistence.
Models from checkpoints predating this support remain readable with their
previous knowledge of collaborative changes. Values are
canonicalised to what `format_at` reads back (`#ff0000` → `FF0000`, an
empty number format → `General`) and changes jade would refuse (a bad
colour, a font size outside 1–409) are refused on every replica, so a
convergent write never reaches a server whose jade cannot realise it.

Column styles target stable column IDs, including imported source defaults.
When the native writer stretches a `<col min=… max=…>` across an insertion,
the materializer splits that span and clears only the new column's style.
Widths, visibility and unknown column metadata survive. This prevents a
format applied to existing columns from silently reaching a new ID, and
keeps source row defaults visible at the new intersections. The stateful
workbook-agreement test retains the minimized format/insert regression;
explicit tests cover imported spans, sparse cells beyond the loaded grid,
namespace/attribute preservation and export/reimport.

MCP spreadsheet reads paginate one sparse stream in sheet order: cell
entries in numeric row-major order, row styles, column styles, then merge
ranges in top-left order. A cell's value, note and resolved format share
one entry, so blank styled/note cells are visible too. Merge geometry is
one entry plus its anchor cell rather than one entry per covered cell.
`entry_count` indexes this stream; `cell_count` still counts non-blank values.
Only the selected entries are projected to JSON, and their combined count
is at most `limit`. Sheet dimensions and default styles repeat on every
page. `lean/JadeCollab/Paging.lean` proves the finite
stream's page bound, reconstruction, cursor progress and saturated-addition
boundary; stateful Rust tests check the document-to-item mapping through
format/value/note edits, line insertion/deletion and checkpoint restoration.

MCP reads of all formats retain an immutable model snapshot, with the
replication sequence and pending count captured under the client's replica
lock (`Client::snapshot`). The response's `snapshot` token selects that same
view for later pages and retries, while live editing and acknowledgements
continue. A read without a token replaces the session's one retained view;
leaving removes it. Missing, replaced and foreign-session tokens return an
explicit tool failure. Session ids and tokens include a fresh UUID namespace
per MCP instance so another instance cannot reuse the first identity after
a restart.
Session and snapshot counters refuse exhaustion,
preserving existing identities and retained reads. `wait_for_changes` keeps
using the live replica.
`lean/JadeCollab/Snapshot.lean` proves retention, replacement, session
isolation and stability under live edits for the cache specification.
Stateful tests compare real retained payloads and cache sizes across session
lifecycle histories, and real-socket tests cover all three formats and
atomic capture of pending and committed operations.

For DOCX, `read_document(include_base_formatting: true)` sends an `Inspect`
request through the session's existing authenticated WebSocket. The actor
captures its committed model, native formatting and sequence in one command,
without logging an edit or advancing the sequence. Native style resolution
uses document defaults, paragraph style inheritance, character style and direct
properties after applying collaborative marks to a private Word document.
`resolved_formatting` partitions each paragraph's UTF-8 text into runs of equal
properties; `paragraph_format` includes style, alignment, indent and spacing.
Empty paragraphs retain paragraph metadata with no character runs. Unresolved
font/colour properties remain null, and rendered theme/conditional effects are
outside this projection.

The response carries the complete model with the formatting, so a concurrent
edit cannot mix two versions. MCP retains that pure-data capture for paging
(`view: authoritative`, `pending: 0`). It uses the existing snapshot token as
an edit precondition. The SDK captures the client ticket while enqueueing the
read, after earlier submissions; subsequent local submissions invalidate its
guard even if they have not reached the server yet. The current socket must
still own the replica, and viewer authorization permits inspection. Request
identities are checked for exhaustion; cancellations remove their waiters,
late answers cannot complete another request, and terminal connection errors
complete all waiters. Inspection failures leave earlier MCP read snapshots
retained. Native cascade fixtures, stateful network/checkpoint/reimport PBT,
request lifecycle PBT and real-socket tests cover these boundaries.
`lean/JadeCollab/NativeInspection.lean` specifies read purity, coherent capture
and invalidation after local or remote edits; it does not verify native parsing
or Rust synchronization internals.

`apply_edits.snapshot` optionally guards editing with the retained read's
local replica version. `Client::edit` checks connection identity, committed
sequence, pending count and the next client ticket before invoking the edit
planner. The ticket advances on every locally submitted operation, so a server
rejection that restores the same view, sequence and pending count cannot reuse
an old guard. Ticket exhaustion refuses submission before applying the model.
The version check, position/formula resolution, ID reservation and queueing
share one replica lock; ordinary submissions also enqueue under that lock to
preserve client-ticket order. A mismatch leaves the model, clock and outgoing
queue unchanged. Accepted batches retain per-edit failures and earlier effects;
the guard is neither rollback nor a global server compare-and-swap. Omitting it
uses the current local view. Read tokens remain readable after edits.
`lean/JadeCollab/EditGuard.lean` proves refusal without effects, competing guard
exclusion, owner isolation and protection against version reuse in the atomic
transition specification. Stateful tests include guards through edits, rejected
operations and session restarts; socket tests cover all document formats.

Dropping a client aborts its connection tasks, closing its socket even while
read snapshots survive. Callers can settle before dropping, or disconnect to
retain pending operations for resume. Acknowledgement events are published
before releasing the replica lock so observing zero pending operations also
makes their verdicts available.

`Client::try_edit` additionally plans on a cloned replica. A failed callback
discards the tentative view, clock, pending operations and client tickets,
without serializing operations. On success it installs that replica under the
same lock, so concurrent ordinary submissions or other plans cannot interleave
their ticket groups. Optional
snapshot validation happens before cloning or invoking the callback. External
callback effects are outside this boundary. Lean's `tryRun` specification
proves failed-plan rollback, successful installation and retry after failure;
stateful PBT compares the actual client with independent counters and queues
through plans, cancellations, allocations, acknowledgements and rejections.

The CLI resolves each whole edit array through `try_edit`: later intents see
earlier tentative structure, while a locally invalid intent submits nothing.
DOCX intent resolution receives the CLI replica's identity and current revision
date, so deletion of text inserted earlier in that array removes its own
tracked insertion outright, following Word's author rule.
The server still judges each operation after submission, so server rejection
can leave an accepted prefix. MCP keeps its documented per-edit outcomes and
uses `Client::edit`; an invalid intent there does not abort the whole array.

Accepted MCP edit calls retain a `receipt` before awaiting network answers.
`get_edit_status` looks up that session's batch without resubmission; its
default timeout is zero and an optional wait ends when its own submissions
are answered. `Client::settle_through` uses the FIFO pending prefix, so later
batches cannot delay an answered receipt and a rejection wakes it even without
a log-position change. Future, unissued tickets are refused. Verdict events
are published before completion becomes observable.

The receipt records every edit's submitted tickets, including an accepted
prefix when a later operation is locally refused. It reports invalid edits,
known refusals, fully acknowledged edits or pending ones. `complete` and
`unresolved` cover all submitted operations, even if one edit already has a
rejection. Reading, watching or submitting another batch collects answers into
all retained receipts, keeping late verdicts through the SDK event drain.
Known verdicts are final. After a connection failure, unanswered operations
remain unknown/pending with `connection_error`; they may have committed.
Calls on a connection already known to have failed are refused before planning.

Each session retains at most 64 receipts. Insertion evicts the oldest completed
one; unanswered receipts are never evicted. If they fill the cache, submission
is refused before reserving IDs or client tickets. Receipt identities use a
checked counter and the MCP instance UUID, are session-scoped, and are removed
on leave or process restart. Unknown/expired receipts do not establish that
their operations failed. Socket tests control delayed verdicts, closed peers,
capacity pressure and later pending batches; native tests cover all formats
and scope/expiry. Stateful PBT checks the retained-result model, duplicates,
foreign tickets, prefix refusal and eviction. Lean specifies finality, scope,
pending versus answered status and FIFO prefix waiting, with Rust checking the
actual transports, cache and ticket boundary near exhaustion.

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
(the materializer clears a widened multi-column `<col>` span's style on
the inserted column, preserving the original target IDs). A merge copies
what shows at the anchor, its row's and
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

* A remote convergent operation is applied straight to the optimistic view
  only while every pending operation is applied and convergent. A pending
  sequenced operation or an inapplicable pending operation causes replay.
  Stateful PBT found a comment tentatively anchored to text that a peer then
  deleted: the old shortcut retained the comment, while replay correctly refused
  its empty range. The minimized seed and explicit core/native regressions keep
  that boundary covered through delivery, reconnection and restart histories.
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
be ahead of the journal. A submission and all the operations jade derives
from it are written as one newline-terminated JSON array (single-record
frames retain the original object representation). A torn last frame is
dropped in full. No part of a submission is broadcast or checkpointed until
its entire frame has been written. This prevents a checkpoint from including
derived operations that are then appended again after it, and prevents a
crash from preserving a cause without its derived effects. Old object-only
journals remain readable; older servers cannot read the new array frames.

A write or checkpoint failure stops the document and explicitly wakes its
open connections so their sockets close. Dropping the subscriber map alone
cannot do this: each connection also owns its outgoing queue's sender.
Clients retain unanswered submissions and, after the server is restored,
resume from the journal to acknowledge persisted edits or resend missing ones.
Resuming also replaces the replica's current connection: late submissions
from the previous socket are refused before parsing or recording anything,
so they cannot consume the new connection's submission numbers.

The storage root is owned exclusively through an OS lock on `.lock`, acquired
before any snapshot is read or initialized. A competing server fails without
waiting or modifying documents. Clones share the locked file. Document actors
have explicit owners: dropping the last server clone sends a stop command and
joins every actor (including actors still referenced by WebSockets), then
releases the storage lock. This prevents surviving old sockets from writing
after a replacement server takes over. The lock file is never unlinked;
process death releases the OS lock, so stale-file cleanup is unnecessary and
would defeat exclusion if done while a server was running. Document names
are also reserved under the server's room mutex before loading or creating
their storage, so simultaneous uploads cannot initialize the same name twice.
The mutex covers only registry transitions: native loading, actor startup and
snapshot writes run outside it. Each reservation has an opaque owner identity;
only that owner publishes a ready actor. A dropped reservation releases its
opening entry, including on initialization errors or unwinding, and cannot
remove a ready room. Opening names conflict immediately but are hidden from
listings and return not-found to reads/connections until publication. Other
documents continue to serve joins, committed edits and exports while a load is
blocked; another document can initialize independently. The initializer's server
owner retains the storage-root lease throughout.

Different names can resolve to the same physical directory on a case-insensitive
filesystem or through aliases. Initial checkpoint creation therefore takes a
per-directory OS lock on `.initialize.lock` before checking for an existing
generation or writing one. The claim is released after initialization, and
the lock inode is left in place. A competing initializer fails rather than
overwriting the winner. This supplements the storage-root lifetime lease;
it does not replace it. The checkpoint header also records the winning
initializer's logical `name` in the same atomic JSON publication as its model.
Restoration uses that name and later checkpoints retain it, even when filesystem
normalization or an aliased path uses another directory spelling. Missing
`name` fields remain compatible with old checkpoints: they use the directory
name, which the next checkpoint records. Invalid or format-mismatched saved
names refuse startup instead of becoming routes. Moving a new-format storage
folder does not rename its hosted document.

Tests block an initializer at a controlled I/O boundary
while another document opens and a real socket edit commits, then restore both
documents. Registry stateful PBT covers interleaved claims, completions, failures
and guard drops; filesystem tests check simultaneous creators through aliases
and verify the winner's actual snapshot content.
Native tests also restore a differently spelled storage folder, and upload
stateful histories move stored folders between restarts while checking stable
public names and content markers in DOCX, XLSX and PPTX.

Appending hands the record to the operating system, which survives the
process dying but not the machine losing power. `StorageOptions::sync`
chooses: `Fsync::Checkpoints` (the default) syncs only checkpoints;
`Fsync::EveryRecord` syncs every record before `append` returns, and so
before the commit is broadcast — an acknowledged edit is on the disk, at one
`fsync` per batch. Checkpoint thresholds still count records, and are checked
after complete batches. The store appends through a `JournalFile` (a `File` in
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

### Document uploads

`PUT /documents/{name}` accepts at most 64 MiB of compressed document bytes
by default (`Server::with_upload_limit`, CLI `--upload-limit BYTES`). This is
an explicit application setting rather than the framework's 2 MiB default.
Zero refuses nonempty bodies; local files and subsequent collaborative edits
are independent of the upload bound. Authentication and per-document editor
authorization run before the body extractor, so forbidden requests do not
consume application body frames or expose a body's parse/size errors.

Axum buffers the bounded body, including streams without Content-Length,
before a worker opens the document. Exactly the configured byte count is
accepted. Excess bytes receive 413; an interrupted body receives 400. Neither
case reaches room creation or storage initialization. A valid retry can use
the same name. A valid upload to an existing name receives 409 without
replacing it. Tests send real chunked HTTP requests to the CLI and preserve
incompressible package parts over 2 MiB in DOCX/XLSX/PPTX exports. Stateful
histories mix credentials, fragmented bodies, oversize/corrupt packages,
conflicting names and server restarts against a separate admission model;
accepted content markers survive while rejected requests reserve no storage.
Lean specifies bounds independent of fragmentation, absorbing refusal and
the absence of publication before a complete authorized body.

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

### Busy document input

The actor inbox has 128 queued/reserved count slots per document and a shared
32 MiB weighted semaphore. A socket reserves its text frame's encoded length
before JSON decoding, then publishes the command synchronously. The envelope
retains its byte lease through native processing; receiving releases its count
slot. Thus there may be one processing command in addition to the 128 queued
or reserved slots. Partial byte reservations waiting on either resource count
against the byte budget. Administrative queries and disconnect cleanup charge
one byte because they carry no incoming document payload.

Capacity waits use async Tokio primitives; native actors receive on dedicated
threads. Token watches remain active while capacity or join/resume answers
are pending. Authorization is refreshed after admission and decoding, so a
waiting edit uses its current role. After 10 seconds a busy socket receives
`overloaded`, closes with 1013, and can resume/reconcile its pending tickets.
A single frame exceeding the byte budget fails before decoding with `error`
and close 1009; it must be split before retry. There is no admission timeout
for local administrative queries. Cancellation returns reservation resources
without enqueueing a command or consuming a ticket. Accepted commands may
commit after the connection closes; normal resume watermarks settle them.

Stop closes an out-of-band publication gate, wakes capacity/answer waiters,
and drains already committed envelopes in FIFO order. It never waits for
uncommitted permits held by Tokio tasks, so synchronously joining an actor
on that same executor cannot deadlock storage-lease release. Publication and
gate closure share a tiny mutex critical section. Fatal journal errors close
the receiver immediately. Connection cleanup follows accepted commands and
uses outbox identity even if a cancelled join never returned its replica id;
it cannot remove a newer resumed connection. The close notification is sent
before cleanup waits for capacity.

The fixed per-document limits are not global heap bounds: raw transport frames
(one waiting per connection), JSON allocation overhead, document growth,
HTTP/download exports and total connection counts are separate resources.
Rust stateful PBT checks weighted reservations, FIFO, cancellation, processing,
release and stop against independent lists/counters. Gated real socket tests
exercise revocation/demotion, unrelated documents, oversized invalid JSON and
cancelled join cleanup. Native DOCX/XLSX/PPTX bursts span more than 128 edits
on each side of an inspection barrier, then settle and resume. Lean's `Inbox`
specifies resource accounting and publication/drain laws; scheduling, parser
allocation and native processing are assumptions checked by Rust tests.

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
(the replica and why: `client`, `revoked`, `overloaded`, `input-overloaded`, `input-too-large`, `stopped`),
`refused` (status 401 with the request path, 403 with the document and
token name, or 400/413 for failed upload buffering), `uploaded` and
`downloaded`. A token is recorded by its grant's
name and never by itself, and request URLs are reduced to their path, since
a socket may carry its token in the query; a test checks that no secret
reaches the file. On SIGHUP (Unix) the server opens the file again at its
path, so logrotate can move it away and signal; if the new file cannot be
opened, entries go on to the old one.

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
docx tracked changes (§4): a document-wide tracking mode loaded from and
saved to Word's settings, tracked typing
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
insert/delete/move/duplicate and visibility, text in shapes, group children and speaker
notes, character marks (bold/italic/underline/size/Latin and East Asian fonts/colour),
paragraph alignment, PNG/JPEG pictures with aspect-preserving resizing,
shape geometry, text boxes and shape deletion (hosted by the server,
the CLI and the MCP server like the other formats; a restored deck's
materializer re-learns which file id jade gave each session-made text box);
index/A1/slide-number-addressed intent APIs for people and agents; the
`jade-collab` CLI; resume after a disconnect; an MCP server
(`jade-collab-mcp`) through which agents join, read, edit and watch
documents as replicas without a shell; durable documents with checkpoints,
log compaction that keeps a tail for replicas slightly behind, and an
optional fsync per journal batch; bearer-token authentication with editor
and viewer roles, per-document access and roles, and tokens rotated without a
restart (revoked sockets are closed); presence, limited in size and rate per
connection; a bounded outgoing queue per connection (a client that reads too
slowly is closed and resumes); an audit log of every access; authoritative
DOCX/PPTX formatting inspection over the joined socket, capturing committed
content, native style projection and log position together, with retained
immutable MCP pagination and conditional editing.

Not yet:

* Marks for attributes with structure: links and hyperlink styles
  (non-expanding marks; the core supports them), character styles, comments
  as marks (they are sequenced); a mark cannot set the formatting of an
  empty paragraph's mark (Word's paragraph-mark run properties). A jade
  editor client would realise marks the way the server's writer does
  (`JadeDocx::formatted`), not by applying them to its own document.
  `read_document` (MCP) reports collaborative character overrides as UTF-8
  spans, including explicit false values, with `formatting_scope` identifying
  that partial view. `include_base_formatting` reads native resolved character
  properties and paragraph styles through an authoritative server capture;
  base styles are not replicated for optimistic local reads. A missing
  collaborative override does not establish false.
  Formatting a story's last paragraph while it is empty (it has no atom to
  name it by); comments in notes (jade-core's comment commands edit
  `document.xml` only, and note runs carry no XML binding) and headers.
  Headers and footers are not modelled.
* xlsx conditional formats and threaded comments; borders on whole rows
  and columns through `set_borders` (`set_format` takes edges for them);
  only editable base styles are imported, not conditional painting;
  note authors are not modelled (jade records `jade`).
* Tracked changes: paragraph splits and joins, tables and formatting are
  not tracked (jade cannot yet decide every paragraph-mark revision, and
  formatting revisions — `w:rPrChange` — are not modelled; convergent
  marks format deleted and inserted text alike, untracked); a file's
  paragraph-mark and formatting revisions are kept in jade but neither
  shown nor decided; decisions take whole revisions, not part of one;
  jade's own editor still marks one's own
  insertion deleted instead of removing it (the intent API removes it).
* docx structure of ragged and nested tables (their text is editable);
  text boxes inside text boxes or notes, and their geometry.
* Access: SIGHUP (reloading tokens, reopening the audit log) is Unix-only;
  the tokens file is still polled everywhere.
* Performance: the RGA is indexed (§4), and consecutive committed text
  insertions reuse the paragraph position index described below. Intent
  resolution, optimistic local plans and structural operations still walk
  the model. Sequenced formatting, comment,
  note and text box operations are preflighted on a copy of the jade document;
  note paragraph joins re-load the base file to refresh jade's XML bindings;
  marks are
  realised on a fresh copy of the base document every time the file is
  written (one jade command per run of one value), and their coverage is
  recomputed by a sweep over the story (`O((n + m) log m)`) when asked;
  jade's xlsx editor rereads the workbook on every write. Fine for the first
  milestone, not for large documents.
* pptx tables, layout changes and group creation/ungrouping. Existing group
  children and pictures are supported; reads identify enclosing group numbers,
  aspect locks, hidden slides and collaborative character overrides. Imported
  inherited text styles and paragraph alignment are not replicated in the
  optimistic read. An authoritative `include_base_formatting` capture resolves
  shape character and paragraph properties through jade's native inheritance
  chain, including theme values, before display autofit. Text in existing group
  children is included; notes remain plain text. Soft breaks keep their native
  paragraph properties, while UTF-8 offsets address the model's newline-separated
  segments; fields keep cached authoring text. Native inspection formats a
  private export and invokes jade's toolbar queries per native run and paragraph;
  these queries rescan the shape, so a shape with many runs can cost quadratically.
  jade's pptx writer rewrites and re-reads the package on every edit.


### Opaque names in document URLs

Document names remain logical UTF-8 names in registries, snapshots and MCP
metadata. A URL carries one canonically percent-encoded path segment; the CLI
and MCP share `document_socket_path`, and HTTP callers use `document_path`.
Format detection parses the URI path separately from its authentication query
and strictly decodes one segment once. Invalid escapes/UTF-8, decoded path
separators and fragments are refused. A literal `%2F` therefore travels as
`%252F`; `+` and space retain distinct identities. The Lean byte specification
proves roundtrip, injectivity and delimiter exclusion, with an executable oracle
compared against Rust. Actual HTTP uploads, socket edits and filesystem restore
are checked separately by stateful PBT and process tests.


### Recoverable resumption

SDK resumption borrows the caller's replica and owns a staging copy in the
opening future. Failed authentication/transport, cancellation and timeout leave
the source available for retry. The future remains Send without requiring a
Sync model. One overall deadline covers connection, upgrade, initial response
and resending, rather than restarting at each phase; the default is ten seconds.
A successful client has a fresh connection identity for conditional edits.

Core resumption validates `answered <= processed_client_seq < next_client_seq`
and requires each missed own commit to be at or below that watermark. It applies
the complete catch-up to a clone before replacing confirmed state, optimistic
view, pending operations, ticket counter or clock. Invalid prefixes cannot
remove tentative edits before a later error is found. The watermark bound makes
`processed_client_seq + 1` safe, and receiving beyond the last log position
returns a typed error. Lean specifies the watermark, exact remaining-ticket
membership and whole-batch rollback after any valid prefix; Rust model histories
include remote commits, server refusals, malformed prefixes and retry. Socket
histories additionally exercise credentials, deadlines, cancellation, partial
resends and lost answers across DOCX/XLSX/PPTX. Retrying uses stable submission
tickets and the sequencer's existing deduplication, not a new replica identity.

Permission refusals follow the same durable ticket path as native guard refusals.
The socket sends a refusal reason to the actor; current-connection ownership is
checked before it advances the ticket, and the actor journals the rejection
before replying. It does not decode the forbidden operation or register its
author. A duplicate denied ticket cannot later commit after role promotion.
Actor stateful histories interleave guard/access refusals, duplicates, socket
replacement, checkpoints and restart. Real all-format tests lose a viewer's
answer, restore from both journal and checkpoint, promote its token to editor,
and verify the refused operation stays absent while a new ticket can commit.


### Authenticated replica ownership

A transport joins under its trusted grant name, which is the stable principal
identity as well as the author label. The sequencer persists named bindings in
the checkpoint's optional `owners` map and in an `Owned` record journaled
atomically with `Joined`. No credential is persisted. Before replacing a
replica's subscriber or clearing its presence, the actor calls `resume_as` to
check both membership and principal equality. Failed authorization changes no
room state, so a reader or another editor cannot consume the owner's tickets
by stealing its public replica ID. Legacy unbound IDs are anonymous and cannot
be claimed by an authenticated caller; users rejoin and reconcile retained
pending edits. Equal configured names share a principal across credential/role
rotation. A token reassigned to another name revokes its existing sockets.

Lean proves the ownership guard and preservation boundaries. Stateful Rust
histories check native actor behavior, refused attempts, immutable bindings,
legacy metadata and checkpoint/journal restoration. All-format socket tests
also verify active-owner edits and presence survive failed takeovers, and that
credential rotation preserves ownership after restart. Torn-batch and injected
append/fsync tests verify no welcome precedes its durable binding.


### Verified TLS transport

HTTP and WebSocket clients share TransportAddress: the absolute URI determines
host, explicit/default port and encryption. Userinfo and invalid ports are
refused. Secure endpoints load native trust roots (or explicit SSL_CERT_FILE /
SSL_CERT_DIR paths) and complete Rustls chain and hostname validation before
constructing the HTTP/WebSocket exchange. The crypto provider is explicit,
avoiding process-global provider installation. A TLS failure has no plaintext
fallback; connection/request deadlines include the handshake. Server-side TLS
is terminated by a reverse proxy to the existing plain listener.

MCP HTTPS discovery preserves the authority and emits WSS document paths. It
accepts a root server address, keeps authentication in its explicit token or
environment, and does not follow redirects. Its HTTP driver has a cancellation
guard, so completion, refusal or dropped requests release the socket. Public
URL-validation and join errors do not repeat the supplied URL or its query.
Lean specifies the security gate and scheme preservation; Rust integration
histories verify actual TLS, certificate/host failure, process/session restart,
all-format native editing and connection cleanup using per-child trust bundles.


### Bounded MCP input

Stdio and embedded requests share a 16 MiB encoded-message bound. The incremental
reader retains at most the bound plus one possible CR delimiter; once exceeded,
it frees the payload and drains through LF (or EOF), refusing the complete
record without parsing its valid-looking prefix. It recognizes CRLF even when
split across reads, keeps partial input/drain state in the reader across future
cancellation, and resets at every boundary. A rejected record returns -32600
with null id; invalid UTF-8 returns -32700 and does not end the stream. Valid
EOF-terminated requests remain supported. The counter specification and actual
byte/reference histories are checked separately from JSON-RPC/edit execution.

Document discovery bounds HTTP bodies to 8 MiB before decoding the typed list.
Actual data frames determine the bound, including chunked bodies; incomplete,
overlong or invalid bodies cannot publish a partial list. The HTTP driver guard
still closes the connection on failure/cancellation. Existing deadlines apply
to both plain and TLS transports. Tests exercise real stdio and HTTP framing;
stateful PBT covers bounded retention, reset, UTF-8 fragmentation and refusal,
while Lean reuses Upload's buffer admission proofs for record boundaries.


### SDK outbound memory and ordering

The replica's pending operations are the SDK writer's source. Its monotonic
cursor clones and serializes one submission immediately before sending, using
constant-time ticket lookup rather than copying the entire pending queue.
There is no additional queue of serialized edits; one frame may be in flight.
This bounds duplicate transport buffering, not application models, pending edit
count, individual operation size or the number of concurrent inspection callers.
Callers can use `settle_through` to limit outstanding work.

Inspection registration captures an exclusive next-ticket boundary under the
replica lock. The writer sends all earlier edits, then that inspection, before
later edits. Cancelled unsent inspections disappear from the writer's source;
in-flight responses remain correlated by request identifier. Unsent presence
updates occupy one replaceable slot. With document/control work available,
at most one presence update is sent between such messages, so continuous
cursor updates cannot starve edits.

Either socket half's terminal failure records the first cause, drains inspection
waiters and wakes settlement. It also stops the other half, including a writer
blocked on IO. Encoding failures take this same path rather than panicking with
a poisoned replica lock. Pending operations remain available for disconnect and
resume; offline optimistic editing can continue without accumulating serialized
frames. Notifications are registered before checking source/error state to avoid
lost wakeups.

`lean/JadeCollab/ClientPump.lean` proves FIFO ticket selection, inspection
boundaries, presence fairness/coalescing and preservation of the retry source in
the abstract policy. It does not prove Rust locking, serialization or socket IO.
Rust stateful PBT compares mixed submission, aborted planning, inspection,
cancellation, presence, refusal and failure histories against an independent
FIFO reference; a gated sink verifies one encoded edit during backpressure and
inspection placement through a thousand-edit burst.


### Bounded restoration and replica issuance

Durable restoration validates checkpoint allocation/ownership metadata before
publishing a room: origin is reserved, every issued replica is below the next
id, and every owner belongs to an issued replica. Retained tails must fit both
the checkpoint's contiguous log interval and its per-replica processed
watermarks. Tail iteration enumerates actual entries without constructing a
successor after the final position, so empty and nonempty tails ending at
`u64::MAX` remain readable and resumable.

Journal commits require the exact next log position. Commits and refusals for
known replicas must strictly advance their processed ticket, retaining allowed
gaps; invalid records are rejected before applying or materializing anything.
Origin commits have ticket zero. This prevents a corrupted suffix from rolling
back a refusal and reviving an answered edit. Repeated refusal records for
unknown replicas still have no effect, matching the trusted core's live refusal
semantics.

`Sequencer::try_join` and `try_join_as` refuse exhaustion or invalid allocation
metadata before changing any ids, processed entries or owner bindings. MAX is
the exhausted next-id sentinel; MAX - 1 is the last issued id. The older `join`
and `join_as` convenience methods panic on refusal; server transports use the
checked methods. A refused join returns a protocol error without stopping the
actor or replacing existing sockets. At a full log, new user operations receive
journaled refusal verdicts, while origin edits refuse without mutation; existing
replicas can still resume and export. Causes that may derive are staged with
their entire suffix before publication, as described below.

`lean/JadeCollab/RecoveryBounds.lean` proves bounded fresh issuance, monotonic
replay watermarks, refusal of repeated/older tickets and tail-position bounds,
including empty tails at the limit. It abstracts parsing, IO and materialization.
Stateful Rust histories use u128 reference counters near u64 limits across
joins, replays, compaction, corrupted suffixes and restarts. Native tests seed
boundary checkpoints for DOCX, XLSX and PPTX, retain the final admitted replica
across restarts, refuse further joins and edits, and check unchanged exports and
fault-free resumability.


### Atomic planning of native derived edits

Before applying a new submission, the sequencer asks the materializer's pure
`may_derive` classification. Potential causes execute on independent model and
file copies returned by `Materializer::fork`. DOCX clones its native document
and numbering bindings; XLSX exports and reopens its package. Normal typing,
formatting and cell values avoid this copy, as do all current PPTX operations.
The classifier must return true for every cause whose derivation can produce
operations or an error. A custom materializer must preserve this contract and
ensure writes on a fork cannot affect the original or external live resources.

The stage applies and materializes the cause, computes its actual derived
suffix, checks all remaining log positions, then applies/materializes every
child in order. Failure to fork, derive, allocate positions or materialize any
staged effect discards the whole plan. Only the live processed ticket advances,
with a durable refusal record so retries cannot revive that operation. Success
installs the staged file/model and appends the cause plus every child in one
actor journal batch before any acknowledgement or checkpoint. Earlier log
entries, pending derived buffers, owner bindings and existing faults stay owned
by the live sequencer. Replay applies recorded facts without staging or deriving
again. Unexpected live failures use the quarantine/recovery boundary below.

This uses the exact suffix rather than rejecting from an upper bound. An edit
that actually derives nothing can consume the final log position even if its
classification requires staging. XLSX origin Lamport ids use checked addition;
zero actual allocations remain valid at MAX, an exact allocation can use MAX,
and overflow discards the stage without changing the workbook. The cost of
copying/parsing a native package applies to potential derived causes, such as
DOCX comments/notes and XLSX table structure/header edits.

Materializer API changes: implementations must provide `may_derive` and `fork`;
`derived` now returns `Result<Vec<Op>, Rejection>`. Direct callers must handle
that result. The workspace adapters, tests and benchmarks use the fallible API.

`lean/JadeCollab/DerivedBatch.lean` proves all-or-none model/file/log publication,
exact-fit admission, non-revival of refused tickets, bounded origin allocation
and composition with the atomic journal batch specification. It abstracts native
fork isolation, parsing, locking and IO. Rust stateful PBT compares mixed staged
causes, invalid final children, duplicates, compaction and recovery with u128
counter/list models. Fault injection covers fork and child materialization
errors. Native socket tests cover DOCX numbering and XLSX table changes at log
and Lamport limits, exact/empty suffixes, byte-identical exports after refusal,
and both checkpoint and journal-only restart paths.


### Office package budgets at the service boundary

The current engine already enforces shared `jade_opc::PackageLoadLimits` for
DOCX, XLSX and PPTX: 128 MiB archive bytes, 10,000 ZIP entries, 64 MiB per
inflated part and 256 MiB total inflated parts. The service retains these checks
rather than inflating a second copy for preflight. Central-directory declarations
are checked before loading parts; actual decompressor output is independently
bounded with a remaining-budget read plus one sentinel byte. Failed reads do
not advance the running expanded-byte count. These limits bound package bytes,
not every allocation of ZIP metadata or the XML/document parsers.

The HTTP compressed-body bound remains separate (64 MiB by default, with 413
on excess). Engine package/parse failures return 400 through normal open
errors. Every failed upload, including native open errors and name conflicts,
is audited once with document name, configured token name and status; payloads,
raw credentials and decoder error text are omitted. No failed open publishes a
room or initializes durable document storage, and a later valid upload may use
the same name. Existing rooms remain available.

Native protocol tests construct small ZIPs with excessive declared part/total
sizes or 10,001 entries and assert each format's resource-specific refusal,
absence of storage, live-room export and retry success. Stateful histories mix
fragmented uploads, editor/viewer roles, malformed ZIPs, resource failures,
conflicts and server restarts against an independent publication model.
Additional PBT exercises the actual engine read helper with fragmented logical
parts and injected IO failures, checking accepted bytes, sentinel reads,
unchanged counters after failure and bounded successful histories.
`lean/JadeCollab/PackageBudget.lean` proves this accounting contract, exact-cap
acceptance, cap-plus-one refusal and independent metadata/actual admission.
It abstracts ZIP/XML parsing, IO scheduling and total parser heap use.


### DOCX table expansion and sparse traversal

New DOCX table operations may create/enumerate at most 10,000 cell positions.
`InsertTable` checks the row/column product with checked multiplication before
constructing stories. A new row/column counts all stored opposite-axis lines,
including tombstones: those historical positions also receive stories for
concurrent edits. Merge/unmerge validates its rectangle before collecting the
Cartesian positions. Exact fits succeed; an excessive or overflowing product
refuses without changing tables, observed Lamport time or optimistic state.
Deleting lines/tables remains available, and small incremental changes can grow
a document beyond this per-operation bound. This is not a total-document or
parser-heap limit.

The model and native materializer both check new-request admission before cell
expansion. Index-based merge/unmerge intents check before calling the ID
allocator. A merge/unmerge needing zero new stories uses an unused origin
sentinel and never reserves an ID, so it still works at clock exhaustion.

Resource policy does not invalidate historical facts. `Document::apply_recorded`
defaults to `apply`; DOCX overrides it to retain logical validation while skipping
only the new-request expansion policy. Sequencer recovery and replica downstream
application use this trusted hook after log/ticket validation. New submissions,
optimistic local operations and pending replay still use ordinary `apply`.
The hook must agree with ordinary application for currently admitted operations;
it must never be used to admit an untrusted client submission. This preserves
old large table records and existing checkpoint shapes without weakening
logical checks such as distinct IDs and valid grid operations.

Table paragraph walking maps live row/column IDs to indices, filters the stored
layout and sorts those cells in grid order. It no longer scans the Cartesian
row/column product of sparse imported merges. Tombstoned positions stay hidden,
while stories moved by merges or deletion retain their original stable IDs.

Stateful PBT checks incremental row/column edits, deletion, refused large
creation, serialization and clock invariants. Sparse traversal is compared with
a row-major reference under reordered IDs and tombstones; a 100-million-position
grid with two stored cells exercises sparse output. Native tests edit and export
a 10,001-column merged import, account for its tombstoned columns during refused
row insertion, and test durable raw-socket refusals, duplicate tickets and ID
reuse across checkpoint and journal-only restarts. `TableWork.lean` proves bounded
products/refusal, admission versus recorded meaning, zero-ID allocation and
sparse output bounds; native parser behavior and ordering remain Rust test
obligations.

### Materialized text boundaries

`jade_collab_core::text` checks the XML 1.0 scalar ranges, returning the first
invalid codepoint and UTF-8 byte offset without changing or escaping input.
DOCX checks body/tracked text, revision dates, comment text/metadata, new
note/text-box text, author names and string formatting attributes. The check
runs in pure submission validation before author registration, and in logical
application before model/clock mutation. It also applies to replayed operations:
XML-forbidden content is a logical error rather than a resource-policy override.
Existing comment/new-story whitespace and line-break rules remain in force.

XLSX checks formulas, sheet names, table headers and string format attributes.
After stripping a leading `=`, an empty/whitespace-only formula is refused;
cell numbers must be finite. Cell text and legacy notes use the engine's
SpreadsheetML xstring encoder and accept all Rust Unicode scalars, including
NUL and literal escape-shaped text. They must not be filtered by the plain XML
predicate. PPTX retains jade's existing character/paragraph policy and native
format validation; multiline intents produce individual paragraph splits.

This addresses deterministic unwritable inputs before materialization; it is
not a proof that arbitrary native editor failures or IO errors are transactional.
Independent native stages discard failed plans; unexpected live errors quarantine
the instance as specified below.
Scalar PBT uses independent numeric intervals and checks first-error byte
positions. Native stateful histories interleave good/bad text, tracked dates,
formula/cell/note writes, changed duplicate payloads, compaction and checkpoint
restore. After every transition the exported file is reopened and compared
with independent string/register references. Field mutation matrices check
metadata and format strings. CLI invalid arrays submit no valid prefix; MCP
receipts resolve refusals and keep pinned reads and subsequent edits usable.
Lean's `TextBoundary` proves scalar/field admission, refusal preservation,
duplicate suppression, valid-history preservation and agreement under the
explicit assumption of successful lossless native materialization. Rust tests
check the actual escaping, serialization, formula normalization and IO paths.


### Unexpected native failures and recovery

`Materializer::materialize` can return an error after partially changing the
native file. The logical model has already applied its effect. The sequencer
therefore records the diagnostic and fences that runtime instance. It returns
`SubmitOutcome::Failed(SyncError::Materialization)` without a downstream message
or journal record, restores the submitting replica's prior processed watermark,
and leaves the committed log prefix unchanged. This is an unknown outcome for
the client. Later submissions, access refusals, joins, resumption, replay and
origin commits cannot advance the failed instance. Checked model/native access
and checkpoint capture return the failure; legacy infallible accessors panic.
Log positions, watermarks and fault strings remain available for diagnostics.

No rollback of the partially changed live model/file is assumed. The actor
terminates before journaling, checkpointing, broadcasting or returning an edit
verdict, closing all of that room's subscribers and pending requests. Other
rooms remain usable. Native author-registration errors take the same fatal
path; a successfully journaled author commit remains a valid prefix if the
following text write fails. Failed replay aborts room opening and name
publication without rewriting stored files or journals. Checkpoints carrying
legacy native faults are refused by checked restoration.

With durable storage, restart loads an independent healthy file/checkpoint and
journal prefix. The SDK retains its disconnected replica and pending ticket;
resume reconciles that prefix and retries the unprocessed operation once.
A second retry is a duplicate. A non-durable room requires an independently
saved healthy state; its quarantined instance cannot supply one. Expected input
constraints still belong in pure validation/admission. Failed independent
stages remain atomic durable refusals, and their temporary failure diagnostics
never poison the live instance. Ordinary writes do not need native forks.

Rust stateful PBT mixes live errors before/after partial writes, staged refusals,
changed duplicates, compaction, checkpoints, replay and exact-ticket retries
against an independent sorted-multiset and counter reference. Real native
DOCX/XLSX/PPTX socket tests inject failures before and after writes on both
checkpoint and journal-only paths: observers see only the healthy prefix,
clients retain unknown edits, exports become unavailable, restart reproduces
byte-identical healthy files, and pending retries commit once. Further tests
cover failed stored replay without durable mutation and DOCX origin-author
failure on either side of successful author registration.

`lean/JadeCollab/NativeFailure.lean` proves quarantine through arbitrary later
histories, no verdict/commit/watermark advancement on live failure, snapshot
refusal, isolated stage preservation and non-revival, recovery discarding every
partial write, restored agreement and exactly-once successful retry. It models
successful native writes, fork isolation and healthy journal IO as assumptions;
Rust exercises those implementation boundaries. It does not prove native IO
transactional or recover state from a quarantined file.


### Authenticated author identity at configuration and file boundaries

`Grant::name` identifies a principal and labels its tracked DOCX revisions.
`Tokens::from_json` requires a nonblank name (`trim` tests emptiness only) and
valid XML 1.0 scalars, independently of role or document scope. Accepted names
retain their exact UTF-8 bytes. The entire table is parsed/validated before
`set_tokens` publishes it, so invalid names during reload cannot partially add
credentials, change roles or revoke the existing connections. Validation errors
identify the constraint/codepoint without including bearer credentials.

The actor checks the native health boundary after any `commit_origin` error
while registering an author. An unexpected native error still stops the room
with an unknown edit outcome. A healthy logical refusal instead processes and
journals a refusal of the client ticket before replying, and never applies the
tracked edit. Changed duplicate payloads cannot revive that refused ticket,
including after checkpoint or journal-only restart. A later valid ticket can
register the author and edit normally. Successful author metadata remains an
independent ordered commit as documented under native failure recovery.

The current native revision parser returns lexical XML attribute values.
`JadeDocx::load` decodes the just-parsed revision metadata exactly once,
including nested insertion/deletion layers, before the model groups revisions.
Equivalent entity spellings therefore identify the same author/date/file ID.
Live command values and `JadeDocx::new` are already logical strings and are not
unescaped; literal text such as `&amp;` or `&#9;` must remain literal text.

At the package boundary, literal TAB/LF/CR in author/date metadata on import
follows XML attribute normalization (including a single space for CRLF).
Character references preserve their represented whitespace. Export and base
checkpoint export encode authored TAB/LF/CR as numeric character references,
so conforming XML consumers preserve the logical label. This targets UTF-8
WordprocessingML `author`, `date` and `dateUtc` attributes; it does not rewrite
text nodes, other attributes, namespace spelling, existing entity references
or unrelated XML/binary parts. Package writes reuse unchanged compressed
payloads. The adapter uses the existing quick-xml version and leaves the native
engine submodule unchanged. Native writes keep their ordinary fast path;
package normalization runs at load/export boundaries.

Native actor regressions verify no unattributed commit, unchanged file bytes,
durable processed refusal, changed duplicate suppression and later valid edits
on both checkpoint and journal-only paths. Stateful socket PBT mixes rejected
configuration reloads before/after registration, viewer/editor transitions,
credential rotation, tracked edits and durable restarts. Independent edit
counters and tracked-character references check exact authors in the client
model and exported/reopened native file after every transition. Scalar PBT
checks admission with a numeric XML reference, and direct file tests cover
entity aliases, nested revision layers, literal entity-shaped names and
repeated whitespace-preserving export/reopen cycles.

`lean/JadeCollab/AuthorIdentity.lean` proves eighteen properties of name/table
admission, preservation of old grants on bad reload, exact accepted identities,
no author/content mutation on prerequisite or viewer refusal, duplicate ticket
non-revival, author-before-edit ordering, and a lossless attribute token codec
with one decoding pass. Unicode blankness, XML lexical tokenization/CRLF
normalization, native application, journal IO and socket delivery are abstract
boundaries checked by Rust; fatal native failures remain specified separately.


### Runtime DOCX text position index

Consecutive committed `InsertText`/`TrackInsertText` operations use an optional
position index. After a successful first insertion, its canonical walk records visible atom
locations, per-paragraph atom membership, story-first paragraphs and successors
within each story. The RGA's actual visible predecessor determines a new run's
position: a character contributes its UTF-8 width, an anchor contributes its
ordinal among zero-width anchors, and a paragraph mark selects the next
paragraph of that same story, skipping interleaved cells/notes/text boxes.

Only locations in the edited paragraph are revisited. Characters and the ending
mark at or after the insertion offset move by the inserted UTF-8 byte length.
An anchor at the exact same byte position moves only if its anchor ordinal is
after the caret. New characters get their cumulative UTF-8 offsets and the
caret's anchor count. Other paragraphs' locations and all existing anchor
counts/widths remain unchanged. Position maintenance costs
`O((P + M) log(N + M + 1))` for `P` existing atoms in the edited paragraph,
`M` inserted characters and `N` indexed atoms plus paragraphs. Empty paragraphs
therefore count in the index size too. Initial construction performs one
canonical walk, followed by `O(N log(N + 1))` indexing; the walk has its own
costs, including tombstones and stored grid structure. These bounds describe
position maintenance, not RGA chunk maintenance, native engine work or intent
resolution.

Every other operation disables and discards the index before it can query or
change topology. Internal structural queries keep using the canonical walk;
the next committed text insertion builds from the resulting document.
Ambiguous visible IDs in legacy/raw models disable acceleration rather than
changing the canonical lookup's interpretation. Invalid insertion ranges or
missing parents are refused before any cold projection. A failed RGA insertion
restores the previous warm index, so repeated refusals cannot force rebuilding
it or change logical document state.

The index is runtime-only, excluded from serialization and model equality.
Cloning starts cold rather than copying a document-sized map; immutable model
sharing retains `Send + Sync` through a private mutex. Optimistic contexts
(`seq: None`) keep the canonical walk, avoiding a new whole-document index on
every short-lived SDK edit plan. Checkpoints and restore never carry stale
positions. Normal server text editing requires no native file forks.

A regression first measured two full document projections for one insertion in
5,000 paragraphs. The accelerated path performs one warm-up projection and no
full projections for subsequent edits, with repair counters bounded by the
edited paragraph. Stateful PBT compares all returned effects and logical JSON
against a forced canonical walk, mixing generated structures/revisions with
UTF-8 typing bursts, several replicas, clone and JSON restoration. Direct cases
exercise anchors at equal byte offsets, story successors, hidden deleted notes,
legacy aliases, immutable sharing and unchanged snapshot representation.
Existing native file/session/socket histories check the same accelerator against
DOCX materialization, exports, checkpoints and reconnection.

`lean/JadeCollab/TextPositions.lean` proves twenty-two properties of offset repair,
anchor ties, UTF-8 prefix widths, unaffected paragraphs, structural invalidation,
cold/warm canonical lookup and at-most-one full projection per eligible consecutive
committed text segment. RGA predecessor lookup, canonical walk fidelity,
UTF-8 widths, map/mutex implementation and native execution remain assumptions
checked by Rust; this is not a proof of end-to-end latency.


### Global DOCX flow atom identity

DOCX paragraph location and revision maps address flow atoms by ID across all
stories. Previously, per-story RGA duplicate checks let a client reuse its own
note character ID as a new body character: the model inserted in the body while
global lookup selected the existing note location. A real socket regression
first demonstrated that this submission was committed instead of refused.

Peer admission now checks complete new atom ranges against every stored story.
This includes normal/tracked text, paragraph marks, note anchors plus their
implicit leading tab/text, and text box anchors/text. Deleted atoms and stories
retain their reservations. Checks use Unicode scalar counts and checked ID
range ends; the owner is part of identity, and unused older clock slots remain
valid. Each story probes the shorter of the new ID run or its stored atoms, so
ordinary typing uses hash lookups without walking all document atoms.

The pure check runs in SDK planning and server submission admission, before
author registration, native staging/writes or log publication. A server refusal
advances only the processed ticket, so changed duplicates cannot revive it.
Existing recorded facts and snapshots keep their original content; the policy
does not rewrite their IDs. Internal structural story keys and formatting write
stamps retain their existing allocation rules.

Field/range tests cover collisions at the start and inside implicit runs, all
story kinds, tombstones, hidden cells/notes/boxes, clone/JSON restore and exact
identifier exhaustion. Stateful native PBT uses an independent retained-ID list
while interleaving fresh edits, splits, note/box creation/deletion, collisions,
changed duplicates, SDK refusals and checkpoint restoration; exported/reopened
DOCX geometry agrees after every transition. Socket tests verify refusal before
author registration, unchanged export bytes, durable processed watermarks,
non-revival and a later valid authored edit after checkpoint/journal recovery.

`lean/JadeCollab/AtomIdentity.lean` proves sixteen properties of this admission
contract, including global uniqueness across arbitrary admitted histories and
reservation persistence through visibility changes and internal uniqueness of
contiguous runs. Native execution, Rust field extraction, bounded arithmetic and snapshot IO
are explicit assumptions checked by Rust.


### Native joins after trailing references

A stateful tracked-change history found a native backspace failure when a first
paragraph ended with an endnote reference and the second hosted a text box.
The native collapsed-delete path removed the second host while its box surfaces
were temporarily absent, then failed its paragraph-count guard before moving
the runs. Whole-file reloading did not fix that structural failure.

The adapter now reuses its anchor-aware positional placeholder helper for a
range join after all trailing references. The placeholder puts those references
strictly before the selected break; removing its exact position after the join
keeps both paragraphs' user text, revisions, references and boxes. Existing
characters equal to the placeholder are preserved. This path restores its saved
native document if a helper step fails. Ordinary joins without trailing anchors
keep their existing direct range command, and native note joins retain their
binding refresh. No native engine changes are required.

The saved PBT seed includes endnote creation/splitting, table insertion/merge, an
empty text box and a body join in a file with duplicate revision labels. A
focused regression covers empty/nonempty boxes, restore before joining and two
differently authored tracked X runs equal to the helper character. Model/file
geometry and revision metadata agree before and after export/reopen. Three
additional `TextPositions.lean` theorems prove positional cleanup's exact atom
order, anchor projection and preservation of matching user atoms; actual native
XML edits and offset mapping remain Rust verification boundaries.


### Authored edit logical admission and proof trust

An authenticated raw socket could previously submit a tracked edit with an
unknown parent, empty insertion, nonexistent story/target or an ID older than
its parent: the actor committed `SetAuthor` before the sequencer refused the
edit. One remaining log position likewise allowed the author to commit while
leaving no capacity for its edit. These logical refusals had an observable
metadata/log side effect despite leaving the file text unchanged.

When an author prerequisite is needed, the actor now checks two available log
positions and applies the author plus edit to an independent logical clone,
using the same origins, base sequence numbers and committed positions as their
actual applications. A refusal publishes neither operation and journals only
the processed client ticket. Ordinary edits with a matching author use the
existing path, needing only one position and no additional clone/file fork.
The healthy author prefix remains separately durable on unexpected native
errors, following the existing failure quarantine/recovery contract.

Fixed socket regressions cover every invalid logical shape. Stateful native
PBT mixes valid and malformed edits, changed duplicates, matching/new authors,
checkpoint/journal-only restarts, and values near `u64::MAX`; native exports,
inspection snapshots and resumed ticket watermarks agree with an independent
reference. The sixth executable Lean oracle compares the same observed traces
with `AuthoredAdmission.lean`, whose sixteen theorems establish logical refusal
atomicity, exact capacity, duplicate non-revival and bounded histories.

Lean's default build additionally audits compiled axiom dependencies for all
production namespace/module declarations, including private/generated helpers,
unused postulates and opaque transitive dependencies. Only the standard
`propext`, `Classical.choice` and `Quot.sound` axioms are allowed; an empty audit
scope fails. Negative fixtures exercise holes that a source search for proof
placeholders would miss. This does not prove native execution or Rust/IO
fidelity; the native differential histories test those explicit boundaries.


### Stable story addresses across native helper commands

[CI on the preceding commit](https://github.com/jade-rs/jade-collaboration/actions/runs/39724459767)
found a stateful native counterexample: create a footnote before body text,
split that body paragraph, then insert a note after a reference in the second
part. Native split caches can list notes after both body parts; a later
placeholder command rebuilds their order. The helper reused its old surface
ordinal for the next command and cleanup, which could now address a different
story. The saved seed `8efc1307470d2a563adab68a7dff0b3802cc1f39d2a8fb6324eeb4edc45ea409`
reproduces the failure locally too.

Anchor-aware helpers now keep the `ParagraphStory` address and resolve its
current surface before the callback and before placeholder cleanup. Callbacks
receive that current document/surface; joins additionally resolve the other
endpoint and recheck adjacency then. Existing rollback protects a failed helper.
This avoids a new full-document surface rebuild on every ordinary keystroke.
The native engine submodule stays unchanged.

A focused native matrix covers footnotes, endnotes, boxes, splits, plain text
and tracked text after the split, with and without restore. Clone comparisons
use story geometry because native surface order can legitimately lag until a
later edit; exports/reopens must also match canonical order. Existing stateful
native/session tests retain the CI seed rather than relaxing admission checks.
`StableSurfaces.lean` adds eight theorems on current-frame target fidelity,
arbitrary reordering histories, exact join endpoints and adjacency refusal,
plus counterexamples for stale ordinals and stale adjacency. Native mapping,
story preservation and UTF-8/rendered conversion are Rust verification boundaries.

### Plain-note run maintenance

The grown operational fixture accumulated 9,857 unformatted runs in one notes
body. Repeated native notes input scanned and retained that fragmentation,
slowing both live acknowledgments and replay of an existing journal. A regression
first showed 255 runs after 128 ordinary prepends. The collaboration materializer
now checks at the first text-changing notes input after open/restore, then every
eight successful changes. Slide-order edits invalidate this ordinal-keyed cadence.
No-op/refused inputs do not consume a check.

The optimization resolves one internal notes relationship and one active body
placeholder, matching the native fallback branch policy. A namespace-aware XML
scan joins physically adjacent runs only when they have no direct formatting or
unknown attributes/children and contain a single plain text element with the same
whitespace mode. Sixteen eligible runs trigger a join. Paragraph boundaries,
rich/foreign runs, comments, CDATA and unknown XML are barriers. Raw text spans
retain entity spelling; literal carriage returns and joins that would manufacture
an illegal `]]>` remain barriers. Bytes outside each eligible group stay exact;
other OPC parts are retained without modification.

Normalization is staged in a fresh native writer. The normal native notes input
still validates expected text, locking, capabilities and offsets. Only a successful
text-changing input publishes that writer; failed/no-op inputs leave the original
archive intact. An optimization failure uses the existing native path. The native
Undo cache may be rebased, while the collaboration log remains authoritative.
No journal format or engine-submodule change is needed, including old-journal replay.

Native stateful PBT checks insert/replace/delete, Unicode, paragraph splits,
export/restore/fork, refused edits, opaque positions, exact other parts and the
no-op cadence regression. `NotesRuns.lean` proves character/whitespace-stream
equivalence, opaque order/payload preservation through arbitrary rewrites, and
failed/no-op publication preservation. A separate invariant bounds one eligible
plain group at 31 runs when successful maintenance checks every eight changes
and native prepends add at most two runs. Parsing, namespace/placeholder fidelity,
native growth and Office rendering remain Rust/native verification boundaries;
the theorem is not a size bound for arbitrary rich-note XML.
