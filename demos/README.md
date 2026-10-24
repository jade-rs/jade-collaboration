# jade-office simultaneous-editing demo

This is a local **DOCX text-editing demo**, using jade-office's actual document
layout/painter and jade-collaboration's production `Replica<DocxModel>` compiled
to WebAssembly. It does not implement a separate JavaScript CRDT or send whole
files as edits. Each tab owns its replica; the server remains the single document
owner and materializes the admitted operations into a downloadable DOCX.

## Start

Check out `jade-office` next to this repository, with its
`demos/collaboration` integration and both repositories' submodules initialized.
The integration is available on jade-office's
[`codex/collaboration-demo`](https://github.com/jade-rs/jade-office/tree/codex/collaboration-demo)
branch (pinned to `00b9148780e1c3dce62c9ec76fcf0fab8fbc509d` in this repository's demo CI).
Install Rust stable **1.99 or newer**, its `wasm32-unknown-unknown` target,
the pinned native Rust toolchain, Python 3, and **wasm-bindgen-cli 0.2.128**:

```sh
rustup target add --toolchain stable wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked
./demos/run-office.sh
```

Open <http://127.0.0.1:8787/?name=Alice>, then click **別の編集者を開く** for
Bob. Edit the same paragraph or table-cell text in both tabs. Their text
converges, and the right-hand preview repaints the committed DOCX through
jade-office. **DOCXを保存** downloads that actual server document.

Click **オフラインにする**, type in that tab and in the connected peer, then
**再接続する**. The retained replica resumes with the same submission tickets;
unanswered edits are reconciled and resent. A refused edit rolls back and is
shown as a notice. A required fresh join or protocol error stops editing while
retaining pending state instead of silently replacing it.

Stop the host with Ctrl-C. Re-running retains the document through the server's
durable data directory. Set `DEMO_DATA` to a **new directory** for a fresh demo.
Data defaults to `target-office-demo/office-demo/data`; do not delete an active
server's data. This host deliberately binds only to loopback and has no tokens.

Supported environment overrides:

| Variable | Purpose |
| --- | --- |
| `JADE_OFFICE_DIR` | jade-office checkout (default `../jade-office`) |
| `CARGO_TARGET_DIR` | build/output directory (default `target-office-demo`) |
| `WASM_BINDGEN` | path to the 0.2.128 executable |
| `DEMO_RUST_TOOLCHAIN` | browser compiler (default `stable`) |
| `DEMO_PORT` | listener port (default `8787`) |
| `DEMO_DATA` | durable storage directory |

The launcher builds committed Office/engine sources in a generated isolated
tree, then overlays only the demo integration. It does not build, stage or
modify unrelated unfinished changes in the Office checkout. Allow several GiB
for Rust/WASM outputs and a few minutes for the first build.

## Scope

The dedicated browser demo shell edits existing paragraph and table-cell text,
including Japanese IME and emoji. It shows peer names and active paragraphs.
The native GPUI ribbon, XLSX/PPTX editing, paragraph creation, formatting, table
structure controls, collaborative undo and graphical remote carets are not
connected in this first demo. The collaboration engine's broader API is
independent of these demo controls.

Offline edits live in **this tab's memory**; closing/reloading it loses that
unsubmitted state. The UI asks before leaving when operations remain pending.
Server acknowledgements are durable (`fsync every`, checkpoint every 101).
The rendered preview follows committed state, so offline text appears there
after resumption, not while disconnected. Preview rendering currently reloads
the small document, suitable for a demo rather than large-document latency.
The bundled Japanese preview fonts do not include every emoji glyph; the text
and exported DOCX retain those Unicode characters.

## Verification

```sh
cargo test --manifest-path demos/browser/Cargo.toml --locked
node --test ../jade-office/demos/collaboration/editing.test.mjs
# While the demo host is running (Node >=22):
node demos/browser/e2e.mjs target-office-demo/office-demo/www
```

The adapter tests cover Unicode boundary validation, all-or-none local planning,
rejection rollback, identifiers above JavaScript's safe integer range, lost-ack
resumption and stateful random delayed concurrent replacements. The real-socket
test creates a separate document and checks concurrent Unicode inserts, offline
resumption, same-table-cell edits, convergence and the actual exported DOCX XML.
The underlying replica/CRDT remains the implementation checked by the existing
Lean models; these adapter tests do not constitute a proof of DOM/IME behavior.
