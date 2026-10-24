// Exercise the actual jade-office Word editor WASM, without a browser DOM.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const [assets, origin='http://127.0.0.1:8787'] = process.argv.slice(2);
const office = await import(pathToFileURL(`${assets}/office-pkg/jade_office_collab_preview.js`));
assert.equal(typeof office.OfficeEditor, 'function', 'The editable Word engine must be exposed');
await office.default({module_or_path:await readFile(`${assets}/office-pkg/jade_office_collab_preview_bg.wasm`)});
const fonts = new office.PreviewFonts(...await Promise.all(['Regular','Bold'].map(weight => readFile(`${assets}/fonts/NotoSansJP-${weight}.ttf`))));
const bytes = new Uint8Array(await fetch(`${origin}/documents/demo.docx`).then(r=>r.arrayBuffer()));
const doc = new office.OfficeEditor(bytes,fonts);
try {
  const initial = JSON.parse(doc.state());
  assert.ok(initial.paragraphs.length>=8, 'body and real table paragraphs are editable');
  doc.select(1,0,1,0);doc.insert('直接😀編集');
  let state=JSON.parse(doc.state());
  assert.ok(state.paragraphs[1].startsWith('直接😀編集'));
  assert.deepEqual(state.selection.focus,[1,6]);
  assert.ok(state.caret.height>0);
  const point=JSON.parse(doc.hit_test(state.caret.page,state.caret.x,state.caret.y+state.caret.height/2));
  assert.equal(point[0],1,'rendered page hit test reaches the edited paragraph');
  assert.ok(Math.abs(point[1]-6)<=1);
  doc.select(1,2,1,4);doc.insert('あ');
  assert.ok(JSON.parse(doc.state()).paragraphs[1].startsWith('直接あ編集'));
  assert.throws(()=>doc.select(1,99999,1,99999));
  doc.select(1,0,1,0);doc.compose('に');doc.compose('日本語😀');
  state=JSON.parse(doc.state());assert.ok(state.composing);
  doc.commit('日本語😀');state=JSON.parse(doc.state());
  assert.ok(state.paragraphs[1].startsWith('日本語😀直接あ編集'));
  assert.equal(state.composing,false);
  assert.throws(()=>doc.select(1,4,1,4),'must reject offsets inside a surrogate pair');
  const pixels=doc.render_page(0);
  assert.equal(pixels.length,doc.width()*doc.height()*4);
  assert.ok(doc.width()>600);
  console.log(JSON.stringify({status:'passed',checks:['actual_word_editing','utf16_selection','page_hit_test','range_replacement','ime_commit','render_after_edit']}));
} finally {doc.free();fonts.free();}
