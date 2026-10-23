# jade-collaboration

Real-time co-editing of `.docx`, `.xlsx` and `.pptx` by any mix of people and AI
agents, built on the [jade](https://github.com/jade-rs/jade-core) engine.

Plain CRDTs converge, but not always to a valid document: two merges that
are each fine can overlap, a paragraph join can swallow a table, a formula
typed against an old grid points at the wrong row. jade-collaboration
combines a CRDT with a central sequencer:

* **Convergent lane** — text, paragraph splits, character formatting
  (Peritext-style marks) and cell values are CRDT operations addressed by
  stable ids. They apply optimistically; valid concurrent edits compose without
  conflict rollback. Invalid or unauthorized submissions can still be refused.
* **Sequenced lane** — table structure, paragraph joins, formulas and other
  operations that need coordination are validated by the server, in one
  order, against the authoritative state; a conflicting one is rejected and
  rolled back on its author only.
* **The server is the only component that touches OOXML.** It applies the
  committed log to a jade document, so Word/Excel-compatible editing and
  every part of the file the model does not describe are preserved.

The full design, and why, is in [docs/design.md](docs/design.md).
Deployment ownership/routing, cold backup recovery and the configurable
process-kill/load probe are described in [docs/operations.md](docs/operations.md).

## Layout

```
crates/
  jade-collab-core     ids, Lamport clocks, RGA (+ executable spec), LWW registers, table grid
  jade-collab-sync     lanes, wire protocol, Sequencer (server core), Replica (client core), network simulator
  jade-docx-collab     docx model, jade materializer, paragraph/offset intent API
  jade-xlsx-collab     xlsx model, jade materializer, A1 intent API
  jade-pptx-collab     pptx model, jade materializer, slide/shape intent API
  jade-collab-server   WebSocket server, client, and the `jade-collab` CLI
  jade-collab-mcp      MCP server through which AI agents join as replicas
lean/                  Lean 4 proofs of the specifications
jade-core/             the engine (git submodule)
```

## Build and test

```sh
git clone --recurse-submodules https://github.com/jade-rs/jade-collaboration.git
cd jade-collaboration
cargo test --workspace
(cd lean && lake build)
```

Use `git submodule update --init` if you cloned without
`--recurse-submodules`.

## Try it

```sh
cargo install --path crates/jade-collab-server
jade-collab serve --save contract.docx budget.xlsx deck.pptx
```

Then, from any number of shells (or agents):

```sh
jade-collab show ws://127.0.0.1:8787/documents/contract.docx/ws
jade-collab apply ws://127.0.0.1:8787/documents/contract.docx/ws \
  '{"type":"insert_text","paragraph":0,"offset":0,"text":"DRAFT: "}'
jade-collab apply ws://127.0.0.1:8787/documents/contract.docx/ws \
  '[{"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":6,"change":{"kind":"bold","value":true}},
    {"type":"add_comment","start_paragraph":0,"end_paragraph":0,"text":"Remove before signing"}]'
jade-collab apply ws://127.0.0.1:8787/documents/budget.xlsx/ws \
  '{"type":"set_cell","sheet":"Sheet1","reference":"B2","value":{"kind":"number","value":42}}'
jade-collab apply ws://127.0.0.1:8787/documents/deck.pptx/ws \
  '{"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"Speaker note"}'
```

The SDK, CLI and MCP support verified `wss://` connections. MCP discovery
accepts `https://host:port` and returns `wss://` URLs; `http://` returns `ws://`.
TLS uses the operating system's native trust roots and verifies the certificate
chain and hostname before sending HTTP or WebSocket authentication. For a
private service, configure a PEM trust bundle through `SSL_CERT_FILE` (or
`SSL_CERT_DIR`) in the process environment. These explicit certificate paths replace automatic platform root discovery.
Trust is loaded for each new connection;
there is no certificate-verification bypass or plaintext fallback.

`jade-collab serve` still listens on plain HTTP/WebSocket. Terminate TLS at a
reverse proxy and route its HTTPS/WSS connections to that listener. The existing
10-second opening/request deadline includes TCP and TLS negotiation. Discovery
does not follow redirects, and its connection driver is dropped on completion,
failure or cancellation. CLI/MCP URL-validation and join diagnostics omit the
original URL so query credentials are not echoed in those errors.

The SDK accepts server messages and individual frames up to 64 MiB, including
initial snapshots and inspections; fragmented messages share that same total
budget. Snapshot JSON includes CRDT history and can be much larger than the
compressed Office file. Above this budget, joining fails with a protocol size
error. A large snapshot within the budget can use the full configured connection
deadline; callers can adjust it with `connect_with_timeout`/`resume_with_timeout`.

`GET /documents/{name}` downloads the current file, `PUT /documents/{name}`
uploads a new one. Programs use `jade_collab_server::Client<DocxModel>` /
`Client<XlsxModel>`, which keep an optimistic replica in step in the
background.

HTTP uploads accept compressed files up to 64 MiB by default. Configure this
with `jade-collab serve --upload-limit BYTES` or
`Server::with_upload_limit(bytes)`. The bound includes streamed/chunked bodies
without Content-Length; exactly the limit is accepted, and an excess receives
413. Zero refuses nonempty uploads. Authentication and the document's editor
role are checked before reading its body. An oversized or interrupted upload
never opens a document or reserves durable storage, so it can be retried with
the same name. Body failures are audited with 400/413. Native open failures
and name conflicts are audited too, without payloads or decoder error text.
Local files opened at startup and later collaborative edits are independent of
this HTTP bound.

The engine separately checks Office ZIP packages: 128 MiB compressed archive,
10,000 entries, 64 MiB per inflated part and 256 MiB total inflated bytes by
default. Declared sizes and actual decompressor output are checked independently.
A package-limit or parsing refusal returns 400, releases its name, and creates
no document storage. These byte budgets do not cap parser heap use, the number
of open documents or later collaborative growth.

DOCX table structure requests create or enumerate at most 10,000 cell
positions per operation (`MAX_TABLE_CELLS_PER_OPERATION`). Exact fits are
accepted; larger requests are refused before changing the model or reserving
story IDs. Row/column insertion counts stored opposite-axis lines, including
tombstones. Existing files, incremental growth and trusted committed history
remain supported. Sparse merged tables are walked by stored cells, preserving
text editing without scanning every absent position.

Consecutive committed DOCX text insertions reuse a runtime position index.
After one initial projection, updating positions visits only the edited
paragraph, including UTF-8 byte offsets and zero-width story references.
Structural changes discard the index. It is omitted from checkpoints and
clones; optimistic local plans keep their existing walk instead of building
an index that their next clone would discard. Intent resolution and native
materialization still have separate document-size costs.

DOCX tracking mode follows the file's `w:trackRevisions` setting. Changing
it through `set_tracking` persists in downloaded files and across server
restarts; later typing and deletion use the mode each replica has observed.

DOCX text and metadata written directly to XML reject XML 1.0-forbidden
characters before model or native file changes; diagnostics identify the
codepoint and UTF-8 byte offset. Unicode, XML entities, tabs, LF and CR remain
supported in body text. Comments and newly created note/text-box text retain
their existing stricter round-trip rules. PPTX keeps its native text rules;
use its multiline intent to generate paragraph splits.

XLSX cell text and notes preserve control characters and literal `_xHHHH_`
strings through SpreadsheetML escaping. Formulas, sheet names, table headers
and string format attributes are checked for directly writable XML characters.
Normalized empty formulas and non-finite numbers are refused. Invalid input
leaves the model/file unchanged, with normal rejection receipts and retries.

XLSX reads include the file's editable cell, row, column and default styles.
Later partial format changes preserve the other attributes; source styles
stay sparse, including whole-column defaults. Conditional painting remains
in the original file and is not represented as an editable base style.

Applications can use `Client::try_reserve` with each format's
`intent::try_resolve` (`try_resolve_with` for DOCX revision context) to
report exhausted identifier space as an error. The CLI and MCP server use
these checked paths too. Identifier runs received from peers are checked
before changing text, notes, text boxes, merged-cell stories or sheet
structure; an overflowing range leaves the document unchanged.

`Client::edit` holds the replica lock while resolving, reserving and queueing
operations. `Client::try_edit` also stages the replica and outgoing messages:
returning an error discards the entire local plan, including allocated IDs and
client tickets. Both accept an optional `ClientSnapshot` precondition. Use the
context's methods inside their synchronous callbacks.

New submissions must mint IDs and write stamps in their own replica namespace.
DOCX character, paragraph-mark and story-anchor allocations must also be unused
in every retained story, including deleted notes, boxes, cells and tombstones.
Complete implicit ranges are checked before native writes or author registration;
unused older clock slots remain valid.
The SDK and sequencer check this before applying the model or touching the
native file; the socket also checks before registering a tracked-change author.
Other replicas' existing text, rows, shapes and revision IDs remain shared edit
targets. Refusals consume the server ticket without changing content, and a
changed payload cannot revive a processed ticket. A new tracked author and its
edit are logically checked together before author publication, with two log
positions reserved; a matching author needs one. Invalid parent/story/range
references or insufficient capacity cannot publish an author alone.
Trusted committed history
and server-origin derived operations retain their original meaning.

The CLI plans an `apply` array this way before sending any operation. A locally
invalid edit leaves the document unchanged, and later edits in a valid array
see the preceding edits' tentative results. Once submitted, the server judges
each operation; a server rejection does not roll back earlier committed edits.
DOCX CLI edits carry the replica's revision context, including Word's rule for
removing one's own tracked insertion when deleting it in the same session.

## Agents (MCP)

`jade-collab-mcp` is an MCP server on stdio. An agent that loads it joins
hosted documents as a replica, next to people and other agents, without a
shell:

```sh
cargo install --path crates/jade-collab-mcp
claude mcp add jade-collab -- jade-collab-mcp   # or any MCP client's config
```

| Tool | What it does |
| --- | --- |
| `list_documents` | `{server: "http://host:port"}` → names, formats and socket URLs |
| `join_document` | `{url: "ws://…/documents/<name>/ws"}` → `session_id`, format, replica id, seq |
| `read_document` | the session's view, paged: docx paragraphs (with their story) and tables; xlsx sparse cell entries (value, note and resolved format), row/column styles and merge ranges; pptx slides |
| `apply_edits` | the CLI's edit JSON, applied in order; each reported `applied`, `rejected` (reason), `invalid` (error) or `pending` |
| `get_edit_status` | look up an `apply_edits` receipt, optionally waiting for that batch's late verdicts |
| `wait_for_changes` | returns once someone's edit arrives (seq > `after_seq`) or on timeout |
| `leave_document` | disconnects the session |

One process holds any number of sessions. The full schemas are in
`tools/list`.

MCP request payloads are limited to 16 MiB of encoded JSON bytes
(`MAX_REQUEST_BYTES`), including JSON whitespace and excluding LF/CRLF framing.
Exactly the limit is accepted, including a final record terminated by EOF.
An oversized record is drained through its newline and refused with JSON-RPC
`-32600`, `id: null`, before parsing or running edits; a valid-looking prefix
cannot execute. Invalid UTF-8 receives `-32700`, `id: null`. The next record
remains usable in both cases. Embedding callers of `handle_message` use the same
payload bound. Picture edits retain their existing 8 MiB decoded-image bound;
split larger edit arrays into separate requests.

Document discovery accepts HTTP response bodies up to 8 MiB
(`MAX_DISCOVERY_BYTES`), across Content-Length and chunked transfer. A larger,
interrupted or invalid response fails as a tool error without publishing a
partial document list. Its existing request deadline and connection cleanup
still apply over HTTP and HTTPS.

Use the socket URLs returned by `list_documents` or printed by `jade-collab
serve` directly. Names are UTF-8 path segments: spaces, `#`, `?`, `+` and literal
percent signs are percent-encoded. For example `日本 #?.docx` has the segment
`%E6%97%A5%E6%9C%AC%20%23%3F.docx`, while `literal%2F.docx` has
`literal%252F.docx`. Decoding happens once; joined metadata keeps the original
name. Query authentication (`?token=…`) is separate from that name and its
extension. SDK callers can build paths with `document_socket_path(name)` or
`document_path(name)` for HTTP uploads/downloads.

Every accepted `apply_edits` call returns a `receipt`, `complete` and
`unresolved` alongside its per-edit results. After a timeout, call
`get_edit_status` with the same `session_id` and `receipt` to learn the final
outcomes without submitting again. Its default `timeout_ms` is zero; a positive
value waits for that batch's submitted operations, including refusals that do
not advance the document's `seq`. Later batches do not delay an answered one.
`complete` covers every submitted operation: an edit already reported rejected
can still have earlier submitted operations outstanding. There is no rollback
of the other committed operations.

Receipts retain known verdicts across reads, waits and later edit calls. A
connection failure after submission returns known outcomes and `connection_error`; unanswered
operations stay `pending` because they may have committed. Each session retains
up to 64 receipts, evicting the oldest completed one when space is needed.
A full cache of unresolved receipts refuses new calls before allocation or
submission. Tokens are scoped to the session and MCP instance, and disappear
on leave or process restart. An expired or unknown receipt does not establish
that its edits failed. Use the retained receipt while the session is live.
New calls on a connection already known to have failed are refused before
planning edits.

For XLSX, `start` and `next` index sparse entries across all sheets. A cell's
value, note and resolved format share one entry, including blank cells with
styles or notes; each row style, column style and merge range is another
entry. `limit` bounds their combined count. `cell_count` counts non-blank
values; `entry_count` counts all read entries. Follow `next` until it is
null, even when a page has no values. Dimensions and the default style are
repeated metadata.

For every format, the first `read_document` returns a `snapshot` token.
Pass that token with `start: next` on later pages to keep the same document,
`seq` and `pending` while other replicas edit. Reading without a token
captures the live view and replaces this session's previous snapshot.
One snapshot is retained per session and removed on leave. Session ids and
tokens include an MCP-instance namespace; expired or foreign-instance/session tokens fail
explicitly. Pass the token as `apply_edits.snapshot` to require that the local
replica is unchanged since that read. A stale token refuses the entire call
before reserving IDs or submitting edits; read again and reconsider the edits.
Checking the version, resolving positions and queueing edits hold one replica
lock. Omitting the token edits the current local view. Accepted calls retain
per-edit outcomes; this does not provide rollback or a server-wide transaction.
The retained token stays readable after editing. `wait_for_changes` uses the
live replica.

DOCX paragraphs include `formatting`: maximal UTF-8 byte spans with one
`change` each, in the same form that the `format` edit accepts. Different
attributes can overlap; explicit `false` values are retained. The root's
`formatting_scope: "collaborative_overrides"` means these are the changes
made through collaborative marks. The file's base formatting and paragraph
styles are not included, so an empty array does not mean unformatted text.
A clear removes those overrides except highlight. Formatting stays attached
to the retained read snapshot along with text and replication metadata.

For DOCX base styles, pass `include_base_formatting: true` on a new read.
This captures the server's committed content, native formatting and `seq`
together. It returns `view: "authoritative"` and `pending: 0`; earlier queued
edits are processed before the capture. Each paragraph adds
`resolved_formatting` (UTF-8 runs of complete character properties) and
`paragraph_format` (style id, alignment, indentation and spacing). Run
properties resolve document defaults, paragraph and character styles,
direct properties and collaborative marks. Unresolved font/colour values
remain null; these are editable properties rather than rendered theme effects.

Use the returned `snapshot` token on later pages, omitting the option or keeping
its original value. The retained data does not change when peers edit, and its
token works as an `apply_edits.snapshot` precondition. A subsequent local or
remote edit makes that precondition stale. Programs can use `Client::inspect`
for the same capture through their existing authenticated socket; viewers can
inspect. Canceled requests are removed and inspection does not advance the log.

PPTX agents can use `format_text` (bold, italic, underline, size, Latin/East
Asian font and colour), `align_paragraphs`, `insert_picture` (PNG/JPEG bytes
as base64) and `set_slide_hidden`, in addition to slide/text/geometry edits.
PPTX reads include `hidden`, each shape's `aspect_locked` and enclosing `group`
number, and per-paragraph UTF-8 `formatting` spans for collaborative overrides.
Hidden slides stay in read order and keep their numbers. The file's inherited
base text styles and paragraph alignment are preserved in downloads; the
optimistic PPTX read reports collaborative character overrides only. Picture
resizes keep their aspect ratio; x/y and rotation accept signed values.

PPTX also accepts `include_base_formatting: true` for the authoritative capture
described above. Text shapes (including group children) add `paragraphs`, each
with `index`, `text`, `resolved_formatting` and `paragraph_format`. Character
properties resolve presentation defaults, master/layout placeholders, theme
fonts and colours, direct properties and collaborative marks. Font sizes are
authoring points before display autofit. Paragraph properties include alignment,
bullets, indentation and spacing; a native soft break creates another text
segment with the same paragraph properties. Runs partition each segment's UTF-8
bytes. Fields retain their cached authoring text, matching the model's offsets.
Non-text shapes omit `paragraphs`, and speaker notes remain plain text. Snapshot
paging, edit preconditions and viewer authorization work as for DOCX.

### Durable documents

```sh
jade-collab serve --data ./collab-data contract.docx budget.xlsx
```

With `--data DIR` (`Server::with_storage(dir)`) every document lives in
`DIR/<name>/`: a snapshot of the collaborative model and of the file at one
log position, and a journal (`log-<n>.jsonl`) of everything after it. Kill
the server and start it again on the same directory and every document comes
back as it was; connected programs call `Client::resume` and carry on. A file
named on the command line is only read the first time — after that the
stored document wins. `--checkpoint-every N` (default 1000) bounds the
journal. `--keep-tail N` (default 1000) keeps the last N committed operations
of each checkpoint in its snapshot and in memory, so a replica at most N
operations behind can always resume, across checkpoints and restarts;
one further behind is told to join again (`ClientError::Rejoin`).

`Client::resume(url, &replica, token)` borrows the disconnected replica. Keep it
until resumption succeeds: authentication/transport errors, timeouts, malformed
catch-up and cancellation preserve its optimistic view, pending operations,
identifiers and tickets for retry. This changes the 0.1 API's second argument
from an owned replica to a reference. The opening future holds an immutable
borrow until it finishes or is dropped, so offline edits cannot race its staged
copy. Discard the old replica after success.
Starting a new replica with `connect` does not transfer those pending edits;
retain them for the application to reconcile if the server requires rejoining.

`connect` and `resume` use one 10-second network deadline covering connection,
HTTP upgrade, the initial sync response and pending resends. Use
`connect_with_timeout(url, token, duration)` or
`resume_with_timeout(url, &replica, token, duration)` to adjust it. Timeout is
`ClientError::Timeout`; an unfinished opening drops its socket and starts no
background tasks. TCP reachability or a successful upgrade alone is insufficient
for a successful connection.

The sync core validates that the server's processed ticket is at least the last
known answer and below the next unissued ticket. Catch-up is staged as a whole:
an invalid watermark, contradictory commit, gap or inapplicable operation
leaves the original replica unchanged. Repeating a resume after lost answers
uses the same tickets, so already committed submissions are not duplicated.
Viewer refusals consume and persist their tickets too, without decoding or
materializing the operation. A refusal remains refused across lost replies,
server restart and later promotion to editor.

An unexpected native writer error closes that document's connections before
publishing a verdict or checkpoint. Its potentially partial model/file cannot
be read or exported. Keep the disconnected replica and its pending edits;
restart a server using `--data DIR`, then resume against the last healthy
checkpoint/journal. The same ticket is retried exactly once. Other documents
continue serving. Without durable storage, recovery requires an independently
saved healthy state. Failure while replaying a journal aborts startup rather
than publishing an inconsistent document. Old checkpoints with recorded native
faults require repair before they can be served.

Errors on an independent staged cause/derived plan discard that plan and return
a normal refusal, so the live document remains usable. Ordinary typing and cell
writes keep their fast path without copying the native file on every edit.

Every journal record reaches the operating system before anyone hears of
it, so killing the process loses nothing. `--fsync every`
(`StorageOptions { sync: Fsync::EveryRecord, .. }`) also syncs each record to
the disk before it is broadcast, so an acknowledged edit survives a power
failure, at one `fsync` per batch; `--fsync checkpoint`, the default, only
syncs checkpoints.

An edit and the operations jade derives from it (such as a comment's assigned
number) are one journal batch. They are all written before any of them is
broadcast or checkpointed. Recovery discards a torn final batch in full, so
it cannot keep a comment without its number. Checkpoint thresholds count
records and are checked between batches. Existing journals remain readable;
new multi-record batches require this version or newer to restore.

Resuming a replica replaces its previous connection. Late edits and presence
from the old connection cannot change the document or consume the resumed
connection's submission numbers; an old connection that submits receives an
error telling it to resume again.

If a journal write or checkpoint fails, the document stops and closes its
connections so clients waiting for acknowledgement wake with an error.
Their pending edits stay in the replica; after storage is available again,
restart the server and resume to learn which edits were persisted and resend
the rest.

Only one server can own a storage directory. Starting a second one fails
immediately, including through a different path to the same directory.
Server clones share ownership; dropping the last clone stops and joins the
document threads before releasing it. Stop the listener and any token-watch
tasks first, since they hold clones. A killed process releases ownership
automatically. The `.lock` file stays in place and must not be removed while
a server is running.

Opening a document reserves its name while native loading and initial storage
run. Other documents remain available for joins, editing, export and uploads.
An opening name already conflicts with another upload, but appears in listings
and accepts connections only after initialization succeeds. Failure releases
the name for retry. Initial storage creation also excludes competing paths to
the same directory, preserving the winning document.
Its logical document name is saved with the snapshot, so filesystem spelling
or an aliased storage path cannot change the published name on restart. Older
snapshots without this field keep using their directory name.

### Access tokens

```sh
cat > tokens.json <<'JSON'
{
  "s3cret-alice": { "name": "alice", "role": "editor" },
  "s3cret-bot":   { "name": "review-bot", "role": "viewer" },
  "s3cret-carol": { "name": "carol", "role": "viewer",
                    "documents": ["deck-*.pptx",
                                  { "document": "contract.docx", "role": "editor" }] }
}
JSON
jade-collab serve --tokens tokens.json contract.docx
jade-collab apply --token s3cret-alice ws://127.0.0.1:8787/documents/contract.docx/ws '…'
```

With `--tokens FILE` (`Server::with_tokens`) every route and the WebSocket
answer 401 unless the request carries a listed token, as
`Authorization: Bearer <token>` or as `?token=<token>` (for browsers, which
cannot set headers on a WebSocket). Viewers can join, read and follow along;
their edits are rejected by the server and never applied, and they cannot
upload. `Client::connect(url, Some(token))` / `Client::resume(url, &replica,
Some(token))` send the header.

A grant's `name` is its stable principal identity and author label. Give
different users different names. Keys with the same name intentionally share
that principal, so credentials and roles can rotate while its replicas remain
resumable. Knowing another replica's public ID or document state does not let
another principal resume it. An unsuccessful attempt preserves its active
connection, presence and submission tickets. Ownership is journaled with the
join before the welcome and survives checkpoints and restarts; bearer tokens
are not stored in that metadata.

Names must contain non-whitespace text and XML 1.0 characters so they can be
stored as DOCX authors. This applies to viewers and document-scoped grants too.
Valid names remain exact, including leading/trailing whitespace and literal
entity-shaped text; they are never trimmed or normalized as principal IDs.
A token table containing an unusable name is refused in full. A failed reload
keeps the previous credentials, roles and existing connections active.
Tracked edits whose author registration is refused are durably refused as well;
they cannot commit without the configured author. DOCX export/reopen preserves
special characters and referenced attribute whitespace in author labels.

Legacy stored replicas without ownership metadata are anonymous. An
authenticated principal must join as a new replica and reconcile any retained
pending edits; it cannot claim an old ID. Replicas from an open server likewise
cannot be claimed after authentication is enabled. Configuring a token with a
different `name` revokes its existing sockets; retaining the name while
changing the key or role preserves the principal.

A grant without `documents` opens every document. With `documents`, it opens
only the names listed — exact names or globs over the whole name (`*` any
run of characters, `?` exactly one) — each with the token's role or, written
`{"document": …, "role": …}`, a role of its own; the first entry that matches
decides. `GET /documents` lists only what the token opens; the other routes
and the socket answer 403 to a known token without access, and uploading
needs the editor role on that name.

The tokens file can change while the server runs: it is read again when its
contents change (checked every `--tokens-poll SECONDS`, default 1; 0 turns
it off) and on SIGHUP, and a file that does not parse is reported and
changes nothing. The new tokens apply to the next request, and to every open
socket at once: one whose token was withdrawn, or no longer opens its
document, receives `{"type":"revoked","message":…}` and is closed
(`ClientError::Revoked`); a changed role applies to what it sends next. In
Rust: `Server::set_tokens`, `Server::reload_tokens`, `Server::watch_tokens`.
There is deliberately no HTTP endpoint for this: whoever can write the
tokens file already decides who gets in.

### Presence

Cursors, selections and the like travel as presence: a client sends
`{"type":"presence","state":<any JSON>}` and the other replicas of the
document receive `{"type":"presence","replica":3,"name":"alice","state":…}`
(`name` comes from the token, when there are tokens), and
`{"type":"presence_left","replica":3}` when its connection closes. Newcomers
are told the current presence of everyone. Presence is never logged or
stored and never reorders operations. In Rust: `client.set_presence(json)`,
`client.presence()`, `client.wait_for_presence(…)`.

Each connection may share a state of at most 16 KiB of JSON, 40 updates at
once and 20 per second after that (`--presence-max-bytes`,
`--presence-burst`, `--presence-per-second`; `Server::with_presence_limits`).
An update over a limit is dropped — nobody else sees it — and only its
sender receives `{"type":"presence_rejected","reason":…}`
(`client.take_presence_rejections()`). Edits do not use the presence rate quota.

Each document admits at most 128 queued/reserved commands and 32 MiB of
combined WebSocket JSON bytes, with byte leases retained through processing.
Capacity is reserved before JSON decoding. Waiting is asynchronous; token
revocation, role changes and actor shutdown remain live. After 10 seconds of
waiting the connection receives `overloaded` and closes with 1013; resume to
reconcile pending edits. One frame exceeding 32 MiB receives an error and
closes with 1009; split the edit before retrying. These are fixed per-document
defaults (`DEFAULT_INCOMING_LIMIT`, `DEFAULT_INCOMING_BYTES`), separate from
presence quotas and outgoing queue limits. One raw frame may also wait per
connection; this is not a process-wide memory or connection limit.

## How it is verified

Tests come first. Three layers:

* **Lean 4** ([lean/](lean/README.md)) proves strong eventual consistency of
  the RGA, mark (rich-text formatting) and LWW specifications and the
  marks' expand rules, that the sequencer preserves any validity predicate,
  that the CRDT union of two valid merges breaks the table invariant, and
  that the client's fast path for convergent operations is sound. The journal
  model proves that checkpointing preserves recovery and that an incomplete
  final batch has no effect; a Lean oracle is compared with the Rust file
  store under random batches, torn writes, checkpoints and reopenings.
* **Property-based tests** check the RGA implementation against its
  specification, random multi-client sessions over a simulated network for
  convergence, grid invariants under random structure edits, the model
  against jade after random edits on real fixtures (every fixture in
  jade-core is loaded and compared), a server restarted from its
  checkpoint, kept tail and journal against one that never stopped and one
  that never checkpointed, the journal's fsync policy, token tables with
  per-document roles against an independent reference, and the presence
  rate limiter against an exact GCRA.
* **End-to-end tests** run real WebSocket sessions and the CLI, including a
  server restarted from its storage directory, tokens (per document, and
  rotated while connections are open) and presence limits.

## License

PolyForm Noncommercial 1.0.0. Free for personal, research, educational, and
noncommercial use. Commercial use requires a separate license — contact the
author. The full text is in [LICENSE](LICENSE).

The jade-core submodule is MIT-licensed; third-party crates keep their own
licenses.
