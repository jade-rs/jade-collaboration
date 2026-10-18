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
jade-collab apply ws://127.0.0.1:8787/documents/contract.docx/ws \
  '[{"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":6,"change":{"kind":"bold","value":true}},
    {"type":"add_comment","start_paragraph":0,"end_paragraph":0,"text":"Remove before signing"}]'
jade-collab apply ws://127.0.0.1:8787/documents/budget.xlsx/ws \
  '{"type":"set_cell","sheet":"Sheet1","reference":"B2","value":{"kind":"number","value":42}}'
```

`GET /documents/{name}` downloads the current file, `PUT /documents/{name}`
uploads a new one. Programs use `jade_collab_server::Client<DocxModel>` /
`Client<XlsxModel>`, which keep an optimistic replica in step in the
background.

## How it is verified

Tests come first. Three layers:

* **Lean 4** ([lean/](lean/README.md)) proves strong eventual consistency of
  the RGA and LWW specifications, that the sequencer preserves any validity
  predicate, that the CRDT union of two valid merges breaks the table
  invariant, and that the client's fast path for convergent operations is
  sound.
* **Property-based tests** check the RGA implementation against its
  specification, random multi-client sessions over a simulated network for
  convergence, grid invariants under random structure edits, and the model
  against jade after random edits on real fixtures (every fixture in
  jade-core is loaded and compared).
* **End-to-end tests** run real WebSocket sessions and the CLI.

## License

PolyForm Noncommercial 1.0.0. Free for personal, research, educational, and
noncommercial use. Commercial use requires a separate license — contact the
author. The full text is in [LICENSE](LICENSE).

The jade-core submodule is MIT-licensed; third-party crates keep their own
licenses.
