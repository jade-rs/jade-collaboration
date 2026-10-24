# jade-office simultaneous-editing demo

This is a local **DOCX text-editing demo**, using jade-office's actual
Word editing engine and jade-collaboration's production `Replica<DocxModel>` compiled
to WebAssembly. It does not implement a separate JavaScript CRDT or send whole
files as edits. Each tab owns its replica; the server remains the single document
owner and materializes the admitted operations into a downloadable DOCX.

## Start

Check out `jade-office` next to this repository, with its
`demos/collaboration` integration and both repositories' submodules initialized.
The integration is available on jade-office's
[`codex/collaboration-demo`](https://github.com/jade-rs/jade-office/tree/codex/collaboration-demo)
branch (pinned to `acb54c6278894faaa096dddd4faf697fdd4247bc` in this repository's demo CI).
Install Rust stable **1.99 or newer**, its `wasm32-unknown-unknown` target,
the pinned native Rust toolchain, Python 3, and **wasm-bindgen-cli 0.2.128**:

```sh
rustup target add --toolchain stable wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked
./demos/run-office.sh
```

Open <http://127.0.0.1:8787/?name=Alice>, then click **別の編集者を開く** for
Bob. Click the Word page or a table cell and type directly in both tabs.
Selections, carets, IME drafts and page layout use the actual Office Word
editor. Both pages update optimistically and converge through the replica. **DOCXを保存** downloads that actual server document.

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

The dedicated canvas shell edits existing paragraph and table-cell text
directly on the rendered Word page, including Japanese IME and emoji.
It contains no paragraph textareas or separate preview. Hit testing, arrow
navigation, selections, editing, IME drafts and layout use
`jade-web::editing::EditableDocument`. Drag or use Shift + arrows to select;
copy/paste and selection deletion work in the page. Enter inserts a Word line
break inside the paragraph, and paste supports line breaks and tabs.
Cmd/Ctrl+A selects the current paragraph. It shows peer names and active paragraphs.
The native GPUI ribbon, XLSX/PPTX editing, paragraph creation, formatting, table
structure controls, collaborative undo and graphical remote carets are not
connected in this first demo. The collaboration engine's broader API is
independent of these demo controls.

Offline edits live in **this tab's memory**; closing/reloading it loses that
unsubmitted state. The UI asks before leaving when operations remain pending.
Server acknowledgements are durable (`fsync every`, checkpoint every 101).
Offline changes are painted immediately, including IME drafts. Incoming
commits patch changed ranges in the native editor and rebase selections using
stable character IDs. The original DOCX is fetched once when opening; typing
does not refetch/reopen a whole document.
The bundled Japanese fonts do not include every emoji glyph; the text
and exported DOCX retain those Unicode characters.

## Verification

```sh
cargo test --manifest-path demos/browser/Cargo.toml --locked
node --test ../jade-office/demos/collaboration/editing.test.mjs
# While the demo host is running (Node >=22):
node demos/browser/editor-e2e.mjs target-office-demo/office-demo/www
node demos/browser/e2e.mjs target-office-demo/office-demo/www
# Optional real-browser composition/reconnection regression (Chromium):
npm --prefix demos/browser ci
(cd demos/browser && npx playwright install chromium)
npm --prefix demos/browser run test:ui
```

The adapter tests cover Unicode boundary validation, all-or-none local planning,
rejection rollback, identifiers above JavaScript's safe integer range, lost-ack
resumption and stateful random delayed concurrent replacements. The real-socket
test creates a separate document and checks concurrent Unicode inserts, offline
resumption, same-table-cell edits, convergence, remote caret rebasing, Word
line breaks/tabs and the actual exported DOCX XML. Every local/remote delivery
also verifies that the real Office editor matches its replica; the exported
DOCX is reopened in that editor. The editor WASM test checks native hit testing,
UTF-16 range replacement, IME commit and rendering.
The browser regression opens `/browser-tests.html`, creates another separate
document and drives two actual editor frames. It verifies composition text and
caret retention during disconnect, queued peer commits followed by resume, and
both final-input/compositionend event orders and cancellation over selected
text. A separate Chromium test clicks the page, types through keyboard input,
replaces/deletes selections, edits the same table cell, inserts line breaks
and resumes offline typing. The selection tests include 128 seeded histories
of 100 insert/delete deliveries to check Unicode caret boundaries. Its runner is pinned in
`demos/browser/package-lock.json` and runs in the demo CI.
The underlying replica/CRDT remains the implementation checked by the existing
Lean models; these adapter tests do not constitute a proof of DOM/IME behavior.
