#!/usr/bin/env bash
# Start a two-tab DOCX demo using the sibling jade-office checkout.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
office="${JADE_OFFICE_DIR:-$root/../jade-office}"
toolchain="${DEMO_RUST_TOOLCHAIN:-stable}"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target-office-demo}"
mkdir -p "$CARGO_TARGET_DIR"
export CARGO_TARGET_DIR="$(cd "$CARGO_TARGET_DIR" && pwd)"
demo="$CARGO_TARGET_DIR/office-demo"
www="$demo/www"
if ! test -f "$office/demos/collaboration/renderer/Cargo.toml"; then
  echo 'jade-office needs the demos/collaboration integration. Set JADE_OFFICE_DIR to that checkout.' >&2
  exit 1
fi
if test "$("$bindgen" --version)" != 'wasm-bindgen 0.2.128'; then
  echo 'This demo needs wasm-bindgen-cli 0.2.128. Set WASM_BINDGEN to its executable.' >&2
  exit 1
fi
mkdir -p "$www/collab-pkg" "$www/office-pkg" "$www/fonts"
# Build the committed office/engine sources in an isolated generated tree.
# This also lets the demo run next to an unrelated, unfinished Office edit.
office_revision="$(git -C "$office" rev-parse HEAD)"
core_revision="$(git -C "$office" rev-parse HEAD:jade-core)"
source="$demo/sources/$office_revision-$core_revision"
if ! test -f "$source/.demo-source-complete"; then
  mkdir -p "$source/jade-core"
  git -C "$office" archive "$office_revision" | tar -xf - -C "$source"
  git -C "$office/jade-core" archive "$core_revision" | tar -xf - -C "$source/jade-core"
  touch "$source/.demo-source-complete"
fi
mkdir -p "$source/demos/collaboration/renderer/src"
cp "$office/demos/collaboration/renderer/"{Cargo.toml,Cargo.lock} "$source/demos/collaboration/renderer/"
cp "$office/demos/collaboration/renderer/src/lib.rs" "$source/demos/collaboration/renderer/src/"
cargo "+$toolchain" build --manifest-path "$root/demos/browser/Cargo.toml" --locked --release --target wasm32-unknown-unknown
"$bindgen" "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/jade_collab_browser.wasm" --target web --out-dir "$www/collab-pkg" --no-typescript
cargo "+$toolchain" build --manifest-path "$source/demos/collaboration/renderer/Cargo.toml" --locked --release --target wasm32-unknown-unknown
"$bindgen" "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/jade_office_collab_preview.wasm" --target web --out-dir "$www/office-pkg" --no-typescript
cp "$office/demos/collaboration/"{index.html,app.js,editing.mjs,canvas-editor.mjs,browser-tests.html,browser-tests.js} "$www/"
cp "$source/jade-core/fonts/Noto_Sans_JP/static/"{NotoSansJP-Regular.ttf,NotoSansJP-Bold.ttf} "$www/fonts/"
python3 "$root/demos/make-office-demo.py" "$demo/seed.docx"
cargo build --manifest-path "$root/Cargo.toml" --locked --release -p jade-collab-server --example office_demo
echo "Open http://127.0.0.1:${DEMO_PORT:-8787}/?name=Alice and use 別の編集者を開く. Ctrl-C stops the host."
exec "$CARGO_TARGET_DIR/release/examples/office_demo" --addr "127.0.0.1:${DEMO_PORT:-8787}" --assets "$www" --data "${DEMO_DATA:-$demo/data}" --document "$demo/seed.docx"
