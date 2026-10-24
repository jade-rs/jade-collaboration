# jade-collab-proofs

Lean 4 proofs of the properties `docs/design.md` §7 lists for the
*specifications* of jade-collaboration. Core Lean only (no Mathlib), no
`sorry`, no extra axioms (`#print axioms` shows at most `propext`,
`Classical.choice`, `Quot.sound`).

## Build

```sh
cd lean
lake build            # proofs, axiom audit/self-tests, seven differential oracles
```

The toolchain is pinned in `lean-toolchain` (`leanprover/lean4:v4.34.0`).
Rust/oracle commands below run from the repository root.

## Proof trust audit

The default build runs `AxiomAudit.lean` over every declaration in the
`JadeCollab` namespace **or originating from a `JadeCollab` module**, including
private definitions, generated helpers and opaque/transitive dependencies.
Only `propext`, `Classical.choice` and `Quot.sound` are permitted. A missing
namespace/module prefix fails instead of silently checking nothing. This uses
Lean's compiled environment and axiom dependency collector, rather than trusting
a text search. CI retains its separate source placeholder check too.

`AuditTests.lean` checks rejection of public, private/unused and opaque
transitive foreign postulates, rejection of an empty prefix, acceptance of
standard axioms and coverage of definitions outside their module's namespace.
Deliberately false fixtures are isolated from production proofs. The audit
checks the compiled dependencies in the current pinned Lean toolchain; Rust
implementation, native IO and the toolchain's trusted kernel remain boundaries.

## What is proved

### `JadeCollab/TextCursor.lean`: ephemeral DOCX carets and selection endpoints

Twenty-six theorems cover deferred unknown endpoints, refusal of reversed or
equal neighbour IDs, endpoint affinity, bounded raw cuts, deletion retaining the
identifier spine, idempotent/commuting deletions, zero-width anchors, paragraph
mark resets, UTF-8 byte accumulation/bounds, prefix composition, suffix isolation
and preservation of selection direction for ordered raw cuts. A counterexample
shows why filtering tombstones before lookup loses a valid cursor even when
both neighbouring characters are deleted.

The model takes one story's canonical RGA walk and byte widths as inputs. RGA
ordering/identifier uniqueness are verified separately; this module does not
prove browser focus events, native Word geometry, Rust execution or that every
pair of arbitrary forged endpoints represents a valid selection. A story that
is no longer rendered has no visible cursor. With no right endpoint the cut is
one atom after the left; with neither endpoint it is the story end.

`cursor-oracle` executes the same proved prefix-fold specification. Two stateful
Rust PBTs each run 96 generated histories over three replicas, retained cursors
and ranges. Every transition compares exact paragraph/UTF-8 coordinates against
an independent raw-prefix scan; the oracle variant also compares with Lean.
Histories include edits, paragraph splits/joins, table merges/unmerges/deletion,
row deletion, note/text-box deletion, delayed messages, reconnects and serialized
checkpoint/rejoin. They include empty paragraphs, supplementary/combining Unicode,
large 64-bit identities, unknown/reversed endpoints and zero-width references.
Each query preserves the durable model; convergence and range direction are
checked separately. The real mixed-story fixture supplies cells, notes and text
boxes. These tests verify the document model; existing native-session and browser
suites cover realization and editor integration.

```sh
(cd lean && lake build)
JADE_LEAN_CURSOR_ORACLE="$PWD/lean/.lake/build/bin/cursor-oracle" \
  cargo test -p jade-docx-collab --lib model::cursor::tests::stateful_cursors_match_lean --locked -- --ignored --exact
```

The regular cursor PBT and deterministic regressions run in ordinary `cargo test`;
CI explicitly executes the Lean comparison as well.

### `JadeCollab/NotesRuns.lean`: bounded plain-note run maintenance

Twelve theorems cover joining adjacent equivalent plain runs, preserving their
character/whitespace-mode stream and the exact order and payloads of opaque
anchors through arbitrary rewrite histories. Every interpreter of that stream
therefore agrees. A separate count model proves that checking before each
text-changing input keeps an eligible group at most 17 runs, assuming each
native prepend adds at most two; the first check also bounds an arbitrarily
large restored group. Failed and no-op staged inputs preserve the original
archive exactly. An archive-determined transition composes across any checkpoint;
a counterexample shows how an unpersisted maintenance counter changes replayed
run layout. Native transition determinism remains a Rust verification boundary.

This does not prove the XML parser, namespace resolution, native run-growth
assumption or Office rendering. Native Rust PBT checks decoded text, edit
capabilities, Unicode, opaque XML positions, exact uninterrupted-versus-restored
archives through edit/restore/fork histories and
unchanged other package parts. Rich, foreign, nonadjacent and otherwise unknown
runs remain barriers. The count bound concerns one eligible plain group under
the stated edit pattern, not arbitrary rich-note documents. An ignored
`notes_runs` native-path comparison accepts `JADE_NOTES_GROWN_PPTX` and optionally
`JADE_NOTES_COMPARISON_REPORT`; it checks equivalence and records timings without
turning a machine-dependent speed ratio into a test assertion.

### `JadeCollab/StableSurfaces.lean`: native indices across helper steps

Eight theorems prove that resolving a stable address in each faithful frame
preserves its target and logical caret, prevents aliasing, and survives arbitrary
surface reorderings. Join plans resolve both endpoints in the current frame
and refuse a newly nonadjacent pair. Two counterexamples show why a cached
ordinal or old adjacency result can select another story after a reorder.
Native map fidelity, story preservation and offset conversion remain explicit
assumptions. Rust replays the CI split/footnote counterexample and checks six
anchor-aware edit kinds before/after restore, clone, native preflight and export.

### `JadeCollab/AuthoredAdmission.lean`: author and edit preflight

Sixteen theorems cover all-or-none logical author/edit admission, exact one/two
log-position costs, invalid edits, missing author prerequisites, exact capacity,
refused processed tickets, duplicate non-revival, monotonic watermarks and
bounded stateful histories. Healthy snapshot restoration preserves those
watermarks. Unexpected native errors retain the separate `NativeFailure`
contract, including a healthy author prefix if that already committed.

`authored-oracle` runs these exact definitions. Rust drives authenticated native
DOCX sockets through valid/malformed tracked edits, altered duplicate payloads,
checkpoints/journal-only restarts and log exhaustion. Observed inspections,
exports and resume watermarks are checked against an independent reference;
an additional differential PBT compares the resulting trace to Lean.

```sh
JADE_LEAN_AUTHORED_ORACLE="$PWD/lean/.lake/build/bin/authored-oracle" \
  cargo test -p jade-collab-server --test authored_admission --locked -- --ignored
```

Successful native application, Rust model/field/context fidelity and durable IO
are assumptions, verified by the native tests rather than proved in Lean.

### `JadeCollab/AtomIdentity.lean`: globally addressed DOCX flow atoms

Sixteen theorems cover complete implicit allocation ranges, overlap refusal,
exact owner/clock identity, acceptance of unused older clock slots, preservation
of global uniqueness through arbitrary stateful histories, hidden atom
reservations, processed refusals, duplicate non-revival and internal uniqueness
of contiguous runs. Native writes, Rust operation field extraction,
bounded arithmetic and snapshot IO remain assumptions. Rust tests check every
flow-allocating operation and native histories with deletion, checkpoints,
restore, SDK refusal and exported/reopened file agreement. Socket regressions
check cross-story collisions before author registration and durable refusal
through both checkpoint and journal-only restarts.

### `JadeCollab/TextPositions.lean`: incremental DOCX text geometry

Twenty-two theorems cover unaffected paragraphs/prefixes, UTF-8 byte repair,
paragraph ends, equal-offset anchor ordering, stable widths/anchor counts,
inserted run prefix widths, structural invalidation and canonical lookup.
The native join specification also proves positional placeholder removal,
anchor projection preservation and retention of matching user characters.
Refused insertion attempts preserve the document/index and need no projection.
The stateful scan counter proves at most one canonical projection for an
eligible consecutive committed typing segment. Canonical walk fidelity, actual RGA
predecessors, UTF-8 widths, maps, mutexes and native editing are assumptions.
Rust compares accelerated effects with a forced canonical path across generated
structure/revision histories, typing bursts, clone and restore; a 5,000-paragraph
regression checks scan counts and paragraph-local repair without wall-clock
thresholds. Native existing histories check integration.

### `JadeCollab/AuthorIdentity.lean`: configured and persisted author labels

Eighteen theorems prove nonblank XML name admission independent of roles,
whole-table refusal and old-table preservation, exact accepted names, durable
logical author refusal, viewer refusal without author mutation, duplicate
non-revival, author-before-edit ordering, and a lossless attribute token codec
that decodes references once. Native fatal errors use `NativeFailure` instead.
Unicode blankness, lexical XML parsing/CRLF normalization, native writes, IO and
socket delivery are assumptions. Rust stateful native socket histories cover
bad reloads, role changes, rekeying, tracked edits, checkpoint/journal restarts
and exact exported/reopened author labels; file tests cover aliases, nesting,
literal entity-shaped names and whitespace-preserving cycles.

### `JadeCollab/NativeFailure.lean`: unexpected writer failure

Seventeen theorems cover no verdict/log/ticket advancement on live error,
quarantined snapshots, immunity to arbitrary further histories, independent
staged refusal/non-revival, recovery discarding arbitrary partial writes,
restored agreement, and exactly-once successful retry. Origin failures preserve
all client tickets and the committed prefix. Recovery denotes loading an
independent healthy checkpoint/journal, never exporting quarantined state.
Successful native writes, fork isolation and healthy journal IO are assumptions.
Rust stateful PBT and native DOCX/XLSX/PPTX fault injection check transport
closure, unknown pending edits, checkpoint/journal recovery, byte-identical
healthy exports, origin-author boundaries and aborting failed replay.

### `JadeCollab/TextBoundary.lean`: materialized text admission

Sixteen theorems cover XML scalar ranges (including whitespace, BMP exclusions,
surrogates and supplementary noncharacters), concatenation, whole-string
admission, SpreadsheetML escape policy, normalized empty formulas and finite
numbers. Refused payloads preserve model/native state; duplicates have no effect,
valid histories remain valid, and successful lossless materialization preserves
agreement. Native serialization, xstring encoding, formula normalization and
successful application are explicit assumptions. Rust scalar PBT, field matrices,
native stateful export/reopen histories and CLI/MCP tests check these boundaries.

### `JadeCollab/AllocationOwner.lean`: minted identifier namespaces

Twelve theorems prove that every admitted fresh ID/write stamp belongs to its
origin, one forged owner causes refusal, shared targets do not affect admission,
empty allocation sets and owned runs pass, and non-origin clients cannot mint
origin IDs. Namespace refusal preserves content, log position and tracked author
state while processing a new ticket. Duplicate tickets have no side effects and
a corrected payload cannot revive a refused ticket. Trusted facts preserve their
original content independently of current admission. The model abstracts successful
logical application, per-format field extraction, connection auth and native IO;
Rust field matrices, stateful native-model histories and durable socket tests
check those boundaries, including unused DOCX merge sentinels.

### `JadeCollab/Inbox.lean`: bounded document input

Twelve theorems cover bounded admission, cancellation returning slots/bytes,
publication preserving resources and FIFO order, processing retaining byte
leases while releasing slots, completion returning bytes, and closure preserving
accepted input while refusing further publication. Draining an empty queue does
not depend on outstanding uncommitted reservations. Natural weights abstract
wire lengths; Rust stateful PBT checks the actual semaphore/channel, and gated
socket tests check token changes, deadlines, cancellation and shutdown. This is
not a proof of global process memory or native parser behavior.

### `JadeCollab/TableWork.lean`: per-request DOCX table work

Checked cell products fit both machine and operation bounds; excessive or
overflowing products refuse. Refusal leaves the model/clock unchanged, while
small incremental steps may exceed the one-operation budget in aggregate.
Currently admitted and trusted recorded operations share their logical meaning;
recorded facts retain their result and logical errors remain errors. Zero new
stories need no identifier headroom. Sparse visible output is bounded by stored
cells, and deleted axes cannot appear. Parser behavior and grid ordering are
abstracted; Rust stateful/native tests check those implementations and recovery.

### `JadeCollab/PackageBudget.lean`: existing engine package budgets

Admitted parts fit both per-part and remaining total capacity; failed/oversized
reads leave accounting unchanged. Exact caps are accepted, cap-plus-one is
refused, and accounting remains monotonic/bounded through histories. Declared
ZIP sizes cannot authorize excessive actual output. This specifies the existing
engine read contract, with ZIP/XML parsing, IO scheduling and total parser heap
abstracted. Rust tests call that engine helper directly and exercise native
DOCX/XLSX/PPTX service refusals, auth, retries, storage and server restarts.

### `JadeCollab/DerivedBatch.lean`: complete native submission plans

Publishing a failed or oversized plan preserves its document, file and log;
a successful plan publishes the entire cause and derived suffix. Exact fits
are accepted, full logs refuse, and failed tickets remain processed so retries
cannot revive them. Origin identifier allocation accepts zero/exact counts and
refuses overflow. Publication composes with the atomic journal batch model.
Fork isolation and native editor behavior are abstract assumptions, checked by
Rust fault injection and DOCX/XLSX byte comparisons across refusal and restarts.
Stateful Rust tests compare mixed histories with independent counter/list models.

### `JadeCollab/RecoveryBounds.lean`: bounded recovery

Fresh replica ids are nonzero, have a bounded successor and never repeat.
Replayed tickets strictly advance their watermark, while repeated/older tickets
are refused without rewinding it. Retained tail positions fit their checkpoint
and the numeric limit; an empty tail at the final position needs no successor.
A full log has no next position. This specification abstracts parsing, IO and
materialization; `DerivedBatch.lean` specifies complete cause/suffix publication
at the live write boundary. Rust stateful histories use u128 reference counters;
native DOCX/XLSX/PPTX tests check boundary restoration, refused joins/edits,
existing replica resumption and unchanged exports across server restarts.

### `JadeCollab/ClientPump.lean`: SDK outbound ordering

Ticket selection is FIFO, an exclusive inspection barrier waits for earlier
edits and precedes later ones, and continuous presence updates cannot starve
ready edits/inspections. Presence coalesces to the latest value without changing
document order; sending, cancellation and terminal failure retain the pending
retry source. Failure prevents further transmission. The policy abstracts
encoding, socket IO and locking. Rust stateful PBT checks mixed histories
against a FIFO reference; gated-sink tests check backpressure, frame counts,
inspection placement, waiter failure and retained pending operations.

### `JadeCollab/InputFrame.lean`: bounded MCP records

The bounded-buffer proofs from Upload are reused with one extra byte reserved
for CRLF recognition. Buffered lengths and accepted payloads are bounded;
exact-limit LF/EOF and CRLF records are accepted; a non-delimiter extra byte
is refused. A refused record cannot recover before its boundary, and that
boundary resets the next record independently of failure. This is a byte-count
specification, with delimiter flags abstracted; it does not prove UTF-8/JSON
parsing, allocator capacity or IO scheduling. Rust stateful framing PBT compares
arbitrary bytes, fragmented delimiters, Unicode/invalid UTF-8 and EOF with an
unbounded reference while checking retained length. Cancellation tests retain
partial input and drain state. Actual stdio processes verify that oversized
valid edit prefixes never run, malformed UTF-8 does not end the process, exact
limits work for LF/CRLF/EOF and subsequent requests still execute. HTTP tests
cover exact bounds, chunking, incomplete streams and typed-list publication.

### `JadeCollab/Transport.lean`: secure endpoint policy

HTTPS discovery maps to WSS and preserves encryption. Secure application bytes
require successful certificate/hostname verification in the policy model;
transitions cannot change the scheme, and verification failures are terminal.
Verification booleans abstract the TLS library: this does not prove cryptography,
URI parsing, native certificate loading or IO cancellation. Real TLS proxies and
isolated CLI/MCP child processes test native-root overrides, wrong trust anchors,
hostname mismatch, authenticated discovery, encoded names, all three formats,
base-format inspection and native edits. Stateful histories mix session/process
restarts, reads, edits and trust failures. Rust also checks scheme/authority/IPv6
mapping, cancellation cleanup, redirect refusal and URL credential diagnostics.

### `JadeCollab/Ownership.lean`: authenticated replica ownership

A replica is resumed only by its stored principal. Unknown replicas and other
principals are refused, and refusal preserves the entire room, including
presence and socket ownership. Successful resumption changes only that
replica's socket and presence, retaining the document, tickets and immutable
owner binding. Anonymous/legacy replicas cannot be claimed by named callers.
This specifies trusted principal checks, not bearer-token validation or disk IO.
Rust stateful tests cover joins, refused attempts, access refusals, credential
aliases, checkpoint/JSON/journal restore, rebinding and legacy metadata. Actor
histories also exercise socket replacement and crash/restart. Socket tests run
against all three formats, preserve the original connection and cursor, and
restore both journals and checkpoints. Byte-cut and injected append/fsync tests
ensure join and ownership are durable together before a welcome is published.

### `JadeCollab/Reconnect.lean`: recoverable catch-up

An accepted watermark never forgets known answers and its successor is within
the next unissued ticket and the integer bound. Exactly the pending tickets
above the watermark remain. Replaying a batch composes across concatenation;
if an operation fails after a valid prefix, the atomic transaction retains its
entire source, including when more operations follow. This specifies validation
and staging, not HTTP/WebSocket behavior or native document guards. Rust stateful
PBT checks remote commits, server refusals, invalid metadata/prefixes, allocation
and retry against a pending-history model. Real socket histories for all three
formats cover authentication failure, deadline expiry, canceled opens, malformed
resume responses, disconnects during resends and deliberately lost answers;
retry preserves local edits and the server commits each stable ticket once.
The per-replica ticket model also proves that access refusal preserves the
document, consumes a fresh ticket, never lowers the watermark, and prevents
that ticket from committing after promotion. Permission refusals use the same
durable rejected-ticket record as guard refusals; actor model histories check
their watermarks through replacement,
checkpoint and restart. Native socket tests restore lost refusals after role
promotion and ensure they never become commits.

### `JadeCollab/Address.lean`: opaque document path segments

Percent encoding a byte sequence and decoding once returns the original bytes;
encoding is injective, and its output contains no raw slash, question mark or
hash. The unreserved ASCII set and uppercase hexadecimal digits match Rust's
canonical codec. This proves a byte specification, not URI parsing or Unicode
validation. Rust PBT checks UTF-8 names, query separation, malformed escapes
and literal percent sequences. Stateful upload/MCP histories check independent
names, authorization, retries, edits, leave/rejoin and durable restarts for
DOCX/XLSX/PPTX; a CLI process test follows the actual printed URLs before and
after restart. `address-oracle` executes the proved encoder on decimal UTF-8
bytes (one sequence per line); CI compares it with actual Rust document paths:

```sh
JADE_LEAN_ADDRESS_ORACLE="$PWD/lean/.lake/build/bin/address-oracle" \
  cargo test -p jade-collab-server --test address_oracle -- --ignored
```

### `JadeCollab/Receipts.lean`: late edit verdicts

Observing a verdict preserves every known answer; another ticket's answer is
unchanged. Completed receipts stay complete and an observed refusal stays
refused. Reporting applied requires every answer, reporting rejected requires
a refusal, and unanswered nonempty batches remain pending. An empty batch has
no outstanding operations. For increasing FIFO tickets, checking the oldest
pending one suffices to wait through a batch without waiting for later batches.
These are specification properties, not a proof of transport or bounded cache
code. Rust stateful PBT checks event routing, duplicates, unknown tickets,
partial rejection, completion, eviction and disconnects. Socket tests exercise
timeouts followed by late commits/rejections, later pending batches, lost
connections, all document formats, token scope and capacity refusal before
ID allocation/submission. Separate histories cover the replica's prefix test
through acknowledgements/rejections/remote changes near ticket exhaustion.

### `JadeCollab/Registry.lean`: document initialization reservations

An occupied name refuses another reservation; an opening room is invisible.
Only its owner can publish or cancel it. Failure permits retry, publication
survives the completed guard's cleanup, and a ready room cannot be replaced by
another initializer. Reserving, publishing and canceling a name leave other
names unchanged. Opaque Rust ownership identities are abstract natural numbers
in this specification; it does not prove mutex scheduling or OS file locks.
Stateful Rust PBT compares interleaved reservations/publications/aborts with a
separate lifecycle model. Controlled blocking-I/O tests verify independent
document startup, real socket edits, exports, name conflicts and durable restore.
Other tests cover initializer unwinding and per-directory initialization locks,
including simultaneous aliases with the winner's content preserved.
Snapshot-name persistence and validation are checked in Rust, including legacy
headers, checkpoint updates and filesystem spelling changes across upload histories.

### `JadeCollab/Upload.lean`: bounded upload admission

An unauthorized upload cannot begin buffering. Refusal absorbs subsequent
frames, and a complete body's admission depends only on its total byte count,
independent of frame fragmentation. Exactly the bound is accepted, every
accepted total is bounded, and incomplete bodies never publish. This is a
specification of buffering and publication, not a proof of axum or HTTP parsing.
Rust tests cover actual extractor ordering with an unpolled forbidden body,
inclusive streaming bounds, mid-stream errors, audit statuses, and CLI requests
with chunked transfer encoding. Stateful histories compare credentials, body
fragments, malformed/oversized files, conflicts and durable restarts against
an admission model with per-file content markers. Native exports preserve
large unrelated parts in DOCX, XLSX and PPTX.

### `JadeCollab/NativeInspection.lean`: one authoritative formatting capture

The actor read preserves authoritative state and captures its model, projected
native formatting and log position together, with zero optimistic pending
operations. The returned edit guard keeps the request's connection and client
ticket; a later local submission or a later committed log position refuses it.
This specifies the actor transition and guard semantics, not OOXML style
resolution or socket scheduling. Rust tests cover style cascades, Unicode
partition validation, export/reimport, checkpoint/reconnect histories, viewer
access, stale sockets, cancellation, late answers and terminal connections.
The capture specification applies to both DOCX and PPTX. PPTX tests resolve
master/layout/theme styles before autofit, explicit false marks and paragraph
properties, and check UTF-8 segments containing soft breaks, empty paragraphs
and cached fields. Stateful histories cover duplication, delivery/reconnection,
checkpoint restores and native export/reimport; MCP cache histories also run
against PPTX authoritative captures.

### `JadeCollab/FormatRead.lean`: reading partial character formatting

After the mark winner has been selected, unset values are omitted and explicit
false values survive. Reported spans retain their original boundaries, and
attaching formatting to paragraphs commutes with pagination. An omitted
override inherits the base, including an unknown base; omission cannot establish
that a property is false. This specifies the read projection, not OOXML styles
or UTF-8 decoding. Rust stateful PBT decodes compressed JSON spans and compares
each character/attribute with the mark specification through Unicode edits,
splits, joins, clears and checkpoint roundtrips. Socket tests check peer edits,
pinned snapshots and actual Word output.

The same projection rules apply to PPTX character overrides. Their read-side
grouping keys include both slide and shape identity: a shape shared by a
duplicated slide cannot receive another slide's annotations. Appending runs
keeps their order. Rust stateful PBT checks decoded per-character formats,
visibility and pagination through duplication, moves, deletion, message
delivery, reconnects and checkpoint restoration. Socket tests also inspect
the written PPTX for paragraph alignment, explicit false and East Asian fonts.

For native formatting, clipping a global run to a paragraph and translating
its offsets preserves exactly the original covered bytes within that paragraph
and retains its style. The proof uses natural-number ranges; Rust tests check
actual UTF-8/UTF-16 conversion, clipping at soft breaks and maximal coalescing.

### `JadeCollab/EditGuard.lean`: conditional local editing

A mismatched version preserves the document, allocation clock and outgoing
queue; a matching version executes the action. An action that advances the
version prevents a competing old guard from executing. Different connection
owners cannot share guards, and an advanced ticket prevents version reuse when
the document, log position and pending count return to their previous values.
The bounded ticket allocator advances strictly or refuses exhaustion. These
proofs describe an atomic transition specification. Rust tests check the actual
mutex boundary, submission order, rejected optimistic operations, and guarded
DOCX/XLSX/PPTX edits over sockets.

The fallible `tryRun` variant stages an action and publishes its state only on
success. Planning failure preserves the original document, clock, version and
queue, permitting a retry with the same guard. Successful publication appends
the plan's contiguous frame group in lock order. Rust stateful PBT checks
rollback and retry through pending acknowledgements/rejections and ID
reservations; CLI process tests exercise locally invalid arrays and dependent
successful edits in all three formats. Server acceptance remains per operation.

### `JadeCollab/Snapshot.lean`: immutable session read snapshots

Retaining a payload makes it readable by its session and token. Capturing
another session leaves that lookup unchanged; replacing the token rejects
the old one, and removing a session makes its snapshots unreadable. Editing
the live replica preserves the retained payload, including capture metadata.
This is a specification of the cache, not a proof of Rust mutex internals.
Rust stateful tests check actual payloads, one-entry-per-session retention,
cleanup and counter exhaustion; socket tests cover DOCX/XLSX/PPTX and atomic
capture of the document, sequence and pending count.

### `JadeCollab/Paging.lean`: sparse read pagination

A positive page limit bounds the item count, and every continuation
advances within an unchanged finite stream. A page followed by its unread
tail reconstructs the stream in order. Saturating the addition at the
machine-word cap before clipping to the total has the same boundary as
natural-number addition. This proves slicing of an already constructed
item stream; Rust stateful tests check how values, formats, notes, axis
styles and merge geometry become those items. Passing the retained read
snapshot on subsequent pages keeps that stream unchanged during live edits.

### `JadeCollab/Style.lean`: imported spreadsheet styles and later edits

Source style selection uses cell > row > column > default before looking
up attributes. An unresolved cell attribute does not inherit a row's value.
Per-attribute writes to cell/row/column registers have the same effective
result as writing one merged register, so cross-layer concurrent updates
commute. A Lamport tick above all four import priorities overrides the
source, including unresolved attributes. Rust fixture comparisons check
the OOXML-to-style projection; stateful tests cover delivery schedules,
checkpoints, reconnects and file reimports. The proof does not verify XML
parsing or the native writer.

### `JadeCollab/Rga.lean`: RGA specification (`rga::spec`)

The state is a set of inserts `Node { id, parent, value }` and a set of deleted
ids. The document is a depth-first walk of the parent tree, with siblings in
descending `(lamport, replica, value)` order and tombstones filtered out.

- `children_perm`, `walk_perm`: sibling order and the walk do not depend on
  the order in which inserts arrived.
- `state_nodup`, `state_mem`, `state_perm`: delivering inserts is idempotent
  set insertion. Replicas that received the same inserts hold permutations of
  the same list.
- `render_congr`: `render` depends only on the insert set and the deletion
  set.
- `converges`: replicas that delivered the same inserts and the same
  deletions, in any order and with any duplication, render the same document
  (strong eventual consistency).

### `JadeCollab/RgaWalk.lean`: the shape of the walk

`Valid` is what `Rga::insert` accepts, as a property of the insert set: ids
are unique, and every atom's parent is present and has a smaller Lamport time
(`MissingParent`, `NotNewerThanParent`). `valid_state`: a replica's state is
valid exactly when the inserts it received are.

- `walk_nodup`: the walk visits each atom at most once.
- `walk_fuel`: fuel `s.length` is enough (a chain from the story start has
  increasing Lamport times, so `path_length_le` bounds its length).
- `walk_filter`: the walk of a larger valid set, restricted to the atoms of a
  smaller valid set it contains, is the smaller set's walk.
- `walk_insert`: an atom that sorts before its siblings and has no children is
  visited right after its parent (`insertAfter`).

### `JadeCollab/Lww.lean`: last-writer-wins registers (`LwwRegister`)

- `merge_comm`, `merge_assoc`, `merge_idem`: `merge` is a join semilattice.
- `write_comm`, `write_idem`: concurrent writes commute, and re-delivering a
  write changes nothing.
- `converges`: the same writes, in any order and with any duplication, give
  the same register.

### `JadeCollab/IdAllocation.lean`: bounded identifier allocation

`runEnd` validates a non-empty run and `reserve` allocates above the observed
clock, both against an explicit bound (`u64Max` in the executable oracle).
`accepted_run_no_wrap` proves every offset is between the first and last id
and within the bound. `reserved_ids_are_fresh` proves every reserved id is
newer than the observed clock and within the bound;
`rejected_reservation_unchanged` leaves the clock unchanged on failure.

`id-oracle` executes those definitions on `o <time>` (observe), `r <count>`
(reserve) and `i <first> <count>` (validate a run). A stateful Rust differential
test covers zero-sized reservations, arbitrary observations, exhaustion and
retries, biased toward both ends of `u64`:

```sh
JADE_LEAN_ID_ORACLE="$PWD/lean/.lake/build/bin/id-oracle" \
  cargo test -p jade-collab-core --test id_oracle -- --ignored
```

Run from the repository root after `lake build`. CI runs this test too.
Independent `u128` reference tests check the Rust clock and RGA runs, including
rejected operations and serialized/restored states. Format model and real
WebSocket tests require overflowing operations to leave the document
unchanged; CLI and MCP tests check that clock exhaustion is reported without
losing the session's ability to read or perform edits that allocate no ids.

### `JadeCollab/Marks.lean`: marks over the RGA (`jade_collab_core::marks`)

A mark gives attribute `key` a value between two boundaries — a side of an
atom (`before a`, `after a`) or an end of the story (`start`, `stop`). An
atom is covered when it lies strictly between them in the story's order
(tombstones included, so anchors never vanish); per attribute it shows the
covering mark with the greatest identifier, as a `Lww` write.

- `effective_congr`, `converges`, `deliver_comm`: an atom's formatting is a
  function of the *set* of mark operations (any order, any duplication).
- `format_converges`, `richRender_converges`: with `Rga.converges`, replicas
  that received the same inserts, deletions and marks show the same text with
  the same formatting; deletions do not enter the formatting.
- `covers_preserved`, `effective_preserved`: atoms inserted later (any
  number, anywhere, as long as old atoms keep their relative order:
  `Extends`) never change the formatting of the atoms already there.
- `covers_in_gap`, `effective_in_gap`, `covers_at_start`: an atom that lands
  in the gap after `l` (`InGap`, `AtStart`) gets exactly the formatting of
  the marks spanning that gap — whoever inserted it and whenever it arrived.
- `markOver_covers_iff`, `typed_inside`, `typed_after_end`,
  `typed_before_start`, `typed_at_story_start`: the expand rules of the marks
  the intent API makes (`before first` … `before` the next atom or `stop`
  when the attribute expands, `after last` when it does not): an atom typed
  inside the range is covered, one typed right after the last atom is
  covered exactly when the attribute expands, one typed right before the
  first atom never is.

These are stated over sequences related by `Extends` / `InGap` / `AtStart`;
`MarksRga.lean` proves those relations for the RGA.

### `JadeCollab/MarksRga.lean`: the mark theorems for RGA histories

`Later s s'`: both states valid and duplicate-free, every insert of `s` in
`s'` (`later_state` for replicas' states). `Typed s s' x p`: `x` is in `s'`,
not in `s`, was typed after `p` (`none`: the story start), and is newer than
every atom of `s` typed after `p` — as it is when its typist had seen them.

- `extends_order`: a later state keeps every atom of an earlier one in the
  same relative order (`Extends`), whatever was inserted, concurrently or not.
- `inGap_order`, `atStart_order`: a typed atom lands in the gap right after
  its parent (`InGap`), or before every old atom at the story start
  (`AtStart`), whatever else was inserted into that gap.
- `concurrent_typing_lands_after_unseen_sibling`: the "newer" proviso is
  needed. Replica 1 holds `a₁ a₂ c` (`c` typed after `a₂`) and bolds
  `a₁ a₂`, expanding, so the mark ends before `c`; replica 2, which had not
  seen `c`, types `x` after `a₂` with an older id. The merged story is
  `a₁ a₂ c x`: `x` is not in the gap after `a₂` and not bold, although it was
  typed at the end of the bold text on replica 2.
- `effective_preserved_rga`, `format_preserved`, `effective_in_gap_rga`,
  `typed_inside_rga`, `typed_after_end_rga`, `typed_before_start_rga`,
  `typed_at_story_start_rga`: the mark theorems above with `Extends`,
  `InGap` and `AtStart` discharged — they hold for every RGA history.

### `JadeCollab/Sequencer.lean`: why tables are sequenced

- `sequenced_preserves`: a sequencer that rejects the operations it cannot
  apply, leaving the state unchanged, keeps any invariant that every
  successful operation keeps.
- `replicas_agree`: catching up on a log in chunks gives the same state as the
  server replaying the whole log. `same_log_same_state` is the trivial form.
- `crdt_union_breaks_invariant`: merge A (rows 0–1, cols 0–1) and merge B
  (rows 1–2, cols 1–2) are each valid on an empty table. Their CRDT union
  overlaps, so it is not valid (design.md §1, first row).
- `merge_sequenced_valid`: merges that go through the sequencer always leave a
  valid set of merge regions.

### `JadeCollab/Reconcile.lean`: the client fast path

- `fast_path_sound`: suppose a remote operation commutes with every
  operation. Applying it straight to the optimistic view
  `pending.foldl step confirmed` then gives the same result as rebuilding the
  view on the new confirmed state. `fast_path_sound_many` extends this to a
  batch of such operations.
- `fast_path_sound_pending`, `guarded_reconciliation_is_replay`: it suffices
  to commute with the actual pending queue. The guarded shortcut and replay
  produce the same optimistic view. Rust uses the shortcut for an applied,
  convergent-only queue; pending sequenced guards or inapplicable operations
  force replay. Stateful core and native DOCX regressions cover a tentative
  comment invalidated by a peer deleting its entire text range.
- `quiescent_view_eq_server`, `quiescent_converges`: with nothing pending and
  the server's log fully folded into the confirmed state, the client's view
  equals the server's state.

### `JadeCollab/Journal.lean`: atomic submission recovery

A store holds a snapshot, complete operation batches and an incomplete final
batch. `recover` folds only complete batches. `recover_appendBatch` proves
that a cause and its derived suffix replay once, in order;
`recover_interrupt` ignores every incomplete suffix; `recover_checkpoint`
and `checkpoint_batch` preserve the recovered state when complete batches
move into a snapshot. `recover_flatten` relates batches to the flat log.
`split_checkpoint_duplicates_derived` demonstrates the former bug: including
a derived effect in the snapshot and journaling it again applies it twice.
These are statements about the recovery model, not filesystem guarantees.

`journal-oracle` executes that model. Input is `a <numbers>` (commit a batch),
`t <numbers>` (interrupt a write), `c` (checkpoint), or `r` (recover); it prints
the recovered list after every command. The Rust differential stateful test
drives the actual file store through the same transitions, including writes
cut at random byte positions and reopening after each transition:

```sh
JADE_LEAN_JOURNAL_ORACLE="$PWD/lean/.lake/build/bin/journal-oracle" \
  cargo test -p jade-collab-server --lib journal_matches_lean -- --ignored
```

Run that command from the repository root after `lake build`. CI runs it
alongside the grid and marks oracle tests. The actor-level stateful test
also exercises derived edits, rejections, retries, checkpoints, restart and
resume against an independent list model; injected append/fsync failures
check that no part of a failed batch is broadcast or checkpointed.

### `JadeCollab/Grid.lean`: the table grid (`jade_collab_core::grid::Grid`)

A line-for-line model of `Grid` with `Nat` identifiers. `apply : Grid → Op →
Option Grid` takes the same accept/reject decisions as `Grid::apply` and leaves
the same state: an inserted row goes to `position(after) + 1` (or `0`), a new
merge is pushed at the end, `Unmerge` removes the first merge with that
top-left, and a deletion tombstones the line and then normalizes every merge
(corners moved to the first/last alive line between them, merges with fewer
than two alive cells dropped). `Valid` is exactly what `check_invariants`
checks: identifiers unique per axis, an alive row and column, every merge
passing `check_merge`, and no merge overlapping an earlier one (on raw
positions).

- `apply_preserves_valid : Valid g → apply g op = some g' → Valid g'`: every
  accepted operation keeps the invariant. Insertions shift positions
  monotonically (`pos_insertAt`, `sh_le_sh`), so `overlaps` is unchanged and
  alive-cell counts only grow (`cnt_insertAt`). Normalization after a deletion
  only shrinks a merge inside its old rectangle onto alive corners
  (`normalizeOne_spec`), and sub-rectangles of disjoint rectangles stay
  disjoint (`overlaps_normalizeOne`). Columns are handled as the rows of the
  transposed grid (`apply_insertCol`, `apply_deleteCol`, `Valid.tr`).
- `apply_rejects_unchanged`: a rejected operation leaves the sequencer's state
  as it was (`apply_accepts` is the other case).
- `grid_sequenced_valid`, `grid_replicas_valid`: any log of grid operations
  folded by the sequencer (`Sequencer.step apply`), in any number of chunks,
  leaves a valid grid.
- `check_iff_valid`: the executable `check` (the loops of `check_invariants`)
  decides `Valid`.
- Rows and columns through merged regions, as in Word and Excel, stated on
  the cells a merge covers (`between`, `covers`, `Holds`):
  `insertRow_covers` / `insertCol_covers` — merges keep their corners, every
  other line is inside or outside each merge as before, and the new line is
  inside a merge exactly when inserted after one of its lines other than the
  last (strictly inside: the merge grows; at its edge: it does not);
  `deleteRow_covers` / `deleteCol_covers` — a surviving merge covers exactly
  its old alive cells minus the deleted line (an edge moves inward;
  `aliveSpan_bounds`, `normalizeOne_holds`), and a merge is dropped exactly
  when fewer than two cells are left between its corners
  (`normalizeOne_none`, via `cnt_aliveSpan`).
- `mergeAt_spec`: `mergeAt` (`Grid::merge_at`) returns the merge covering an
  alive cell; in a valid grid there is at most one.
- `sanity_*`: the unit tests of `crates/jade-collab-core/tests/grid.rs`,
  evaluated by `decide` (insert after an anchor, deleted rows still anchor
  inserts, last row/column kept, overlapping and malformed merges rejected,
  deleting an anchor row shrinks a merge and a one-cell merge disappears,
  unmerge, the naive union of two merges is invalid, rows and columns
  inserted inside or at the edge of a merge, edges moving inward).

### `JadeCollab/CellCommute.lean`: cell contents are convergent

A table is a grid plus contents for every `(row, column)` pair; a cell edit
(any function of the old contents) is accepted iff the row and the column exist,
alive or deleted.

- `apply_ids`: grid operations never remove identifiers; the only ones they
  add are the row or column an insertion creates.
- `cell_edit_comm_grid`: a cell edit valid where it was made commutes with
  every grid operation — applying it before or after gives the same result,
  both rejected or both the same table.
- `cell_edit_after_grid_iff`: the edit is accepted after a grid operation iff
  before, unless that operation created the edited row or column
  (`cell_edit_needs_existing_row` shows the side condition is needed).
  `grid_after_cell_edit`: a grid operation's outcome does not depend on cell
  edits.
- `step_comm_edit_grid`: the sequencer steps of a valid cell edit and any grid
  operation commute. `cell_edit_in_deleted_row`: an edit to a cell of a
  deleted row still lands.

### `JadeCollab/Anchor.lean`: tables stay anchored to visible marks

A story of characters and paragraph marks (with tombstones) and tables
anchored after a mark or at the story start. Convergent: insert a character,
insert a mark (split), delete a character (never a mark). Sequenced: join
(delete a mark, refused while a live table hangs off it), insert a table after
a visible mark, delete a table.

- `apply_preserves_anchored`, `anchored_sequenced`: every live table's anchor
  mark exists and is visible, after any accepted operation and any sequenced
  log.
- `convergent_preserves_sequenced_validity`: a convergent operation never
  changes whether a join, a table insertion or a table deletion is accepted,
  as long as the sequenced operation does not name the mark the convergent one
  creates (`join_needs_existing_mark` shows why that side condition is
  needed). `join_blocked_by_table` is a concrete run of the guard.

## `grid-oracle`

`lake build` also builds `.lake/build/bin/grid-oracle` (`GridOracle.lean`),
which runs the same `apply`, `check` and `mergeAt` as `Grid.lean` on a line protocol, for
differential testing against the Rust `Grid`:

```
rows <id> <id> ...                 initial rows, all alive, in order
cols <id> <id> ...                 initial columns
ir <id> <after|->                  InsertRow (`-`: at the top)
dr <id>                            DeleteRow
ic <id> <after|->                  InsertColumn (`-`: at the left)
dc <id>                            DeleteColumn
m <top> <left> <bottom> <right>    Merge
u <top> <left>                     Unmerge
```

It prints `ok` or `err` per operation, then `rows <id>:<1|0> ...` and
`cols <id>:<1|0> ...` (every line ever inserted, in order, with its alive
flag), `merges <top>,<left>,<bottom>,<right> ...` (in list order; the bare word
when there are none), `cover <row>,<col>=<top>,<left> ...` (`mergeAt` of every
alive cell a merge covers, row by row) and `valid` or `invalid`. A malformed line is reported on
stderr with exit code 2.

```sh
$ printf 'rows 1 2 3\ncols 101\nm 1 101 3 101\ndr 1\ndr 3\n' | lake exe grid-oracle
ok
ok
ok
rows 1:0 2:1 3:0
cols 101:1
merges
cover
valid
```

## `marks-oracle`

`lake build` also builds `.lake/build/bin/marks-oracle` (`MarksOracle.lean`):
the RGA walk and `Marks.effective` on a line protocol, for differential
testing against the Rust `Rga` and `MarkSet`
(`crates/jade-collab-core/tests/marks_oracle.rs`):

```
i <lamport> <replica> <parent-lamport> <parent-replica>   insert after an atom
i <lamport> <replica> -                                   insert at the start
m <lamport> <replica> <key> <value> <start> <end>         mark; boundaries s | b:<l>:<r> | a:<l>:<r> | e
k <key> <key> ...                                         attributes to report
```

It prints one line per atom in story order: `<lamport>:<replica>`, then per
attribute `-` or the winning mark as `<lamport>:<replica>:<value>`.

```sh
$ printf 'i 1 1 -\ni 2 1 1 1\ni 3 1 2 1\ni 4 2 2 1\nm 10 1 0 1 b:1:1 b:3:1\nk 0\n' | lake exe marks-oracle
1:1 10:1:1
2:1 10:1:1
4:2 10:1:1
3:1 -
```
