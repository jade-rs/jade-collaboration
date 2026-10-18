# jade-collaboration

Real-time co-editing of `.docx` and `.xlsx` by any mix of people and AI
agents, built on the [jade](https://github.com/jade-rs/jade-core) engine.

Plain CRDTs converge, but not always to a valid document: two merges that
are each fine can overlap, a paragraph join can swallow a table, a formula
typed against an old grid points at the wrong row. jade-collaboration
combines a CRDT with a central sequencer:

* **Convergent lane** — text, paragraph splits and cell values are CRDT
  operations addressed by stable ids. They apply immediately everywhere and
  are never rolled back.
* **Sequenced lane** — table structure, paragraph joins, formulas and other
  operations that need coordination are validated by the server, in one
  order, against the authoritative state; a conflicting one is rejected and
  rolled back on its author only.
* **The server is the only component that touches OOXML.** It applies the
  committed log to a jade document, so Word/Excel-compatible editing and
  every part of the file the model does not describe are preserved.

The full design, and why, is in [docs/design.md](docs/design.md).

## Layout

```
crates/
  jade-collab-core     ids, Lamport clocks, RGA (+ executable spec), LWW registers, table grid
  jade-collab-sync     lanes, wire protocol, Sequencer (server core), Replica (client core), network simulator
  jade-docx-collab     docx model, jade materializer, paragraph/offset intent API
  jade-xlsx-collab     xlsx model, jade materializer, A1 intent API
  jade-collab-server   WebSocket server, client, and the `jade-collab` CLI
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
jade-collab serve --save contract.docx budget.xlsx
```

Then, from any number of shells (or agents):

```sh
jade-collab show ws://127.0.0.1:8787/documents/contract.docx/ws
jade-collab apply ws://127.0.0.1:8787/documents/contract.docx/ws \
  '{"type":"insert_text","paragraph":0,"offset":0,"text":"DRAFT: "}'
jade-collab apply ws://127.0.0.1:8787/documents/budget.xlsx/ws \
  '{"type":"set_cell","sheet":"Sheet1","reference":"B2","value":{"kind":"number","value":42}}'
```

`GET /documents/{name}` downloads the current file, `PUT /documents/{name}`
uploads a new one. Programs use `jade_collab_server::Client<DocxModel>` /
`Client<XlsxModel>`, which keep an optimistic replica in step in the
background.

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
journal; a replica that was offline for longer than a checkpoint is told to
join again (`ClientError::Rejoin`).

### Access tokens

```sh
cat > tokens.json <<'JSON'
{
  "s3cret-alice": { "name": "alice", "role": "editor" },
  "s3cret-bot":   { "name": "review-bot", "role": "viewer" }
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
upload. `Client::connect(url, Some(token))` / `Client::resume(url, replica,
Some(token))` send the header.

### Presence

Cursors, selections and the like travel as presence: a client sends
`{"type":"presence","state":<any JSON>}` and the other replicas of the
document receive `{"type":"presence","replica":3,"name":"alice","state":…}`
(`name` comes from the token, when there are tokens), and
`{"type":"presence_left","replica":3}` when its connection closes. Newcomers
are told the current presence of everyone. Presence is never logged or
stored and never reorders operations. In Rust: `client.set_presence(json)`,
`client.presence()`, `client.wait_for_presence(…)`.

## How it is verified

Tests come first. Three layers:

* **Lean 4** ([lean/](lean/README.md)) proves strong eventual consistency of
  the RGA and LWW specifications, that the sequencer preserves any validity
  predicate, that the CRDT union of two valid merges breaks the table
  invariant, and that the client's fast path for convergent operations is
  sound.
* **Property-based tests** check the RGA implementation against its
  specification, random multi-client sessions over a simulated network for
  convergence, grid invariants under random structure edits, the model
  against jade after random edits on real fixtures (every fixture in
  jade-core is loaded and compared), and a server restarted from its
  checkpoint and journal against one that never stopped.
* **End-to-end tests** run real WebSocket sessions and the CLI, including a
  server restarted from its storage directory, tokens and presence.

## License

PolyForm Noncommercial 1.0.0. Free for personal, research, educational, and
noncommercial use. Commercial use requires a separate license — contact the
author. The full text is in [LICENSE](LICENSE).

The jade-core submodule is MIT-licensed; third-party crates keep their own
licenses.
