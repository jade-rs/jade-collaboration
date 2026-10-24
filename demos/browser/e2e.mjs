// Real WebSocket + compiled browser WASM against the demo host. Each run
// creates a separate document so it cannot alter the visible demo's contents.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

const [assets, origin = 'http://127.0.0.1:8787'] = process.argv.slice(2);
if (!assets) throw new Error('Usage: node demos/browser/e2e.mjs <generated-www> [origin]');
const { default:init, BrowserDocument } = await import(pathToFileURL(`${assets}/collab-pkg/jade_collab_browser.js`));
await init({module_or_path:await readFile(`${assets}/collab-pkg/jade_collab_browser_bg.wasm`)});
const {default:initOffice,PreviewFonts,OfficeEditor}=await import(pathToFileURL(`${assets}/office-pkg/jade_office_collab_preview.js`));
const {projectDocument}=await import(pathToFileURL(`${assets}/editing.mjs`));
await initOffice({module_or_path:await readFile(`${assets}/office-pkg/jade_office_collab_preview_bg.wasm`)});
const fonts=new PreviewFonts(...await Promise.all(['Regular','Bold'].map(weight=>readFile(`${assets}/fonts/NotoSansJP-${weight}.ttf`))));
const name = `e2e-${Date.now()}.docx`;
const seed = await fetch(`${origin}/documents/demo.docx`).then(r=>r.arrayBuffer());
assert.equal((await fetch(`${origin}/documents/${name}`,{method:'PUT',body:seed})).status,201);
const url = origin.replace(/^http/,'ws') + `/documents/${name}/ws`;
const sleep = ms => new Promise(resolve=>setTimeout(resolve,ms));
async function until(predicate) {
  for(let i=0;i<200;i++){if(predicate())return;await sleep(25);}
  throw new Error('Timed out waiting for convergence');
}
class Editor {
  constructor(){this.ready=false;this.deferred=[];this.delay=false;this.errors=[];this.peers=new Map();}
  async connect(){
    this.socket = new WebSocket(url);
    this.socket.onopen=()=>this.socket.send(this.doc?this.doc.resume_message():'{"type":"join"}');
    this.socket.onmessage=e=>{try{this.message(e.data);}catch(error){this.errors.push(error);}};
    await until(()=>this.ready||this.errors.length); assert.deepEqual(this.errors,[]);
  }
  message(wire){
    const m=JSON.parse(wire);const before=this.doc?this.state().paragraphs:null;
    if(m.type==='presence'){this.peers.set(this.doc.presence_id(wire),m.state);return;}
    if(m.type==='presence_left'){this.peers.delete(this.doc.presence_id(wire));return;}
    if(m.type==='presence_rejected')throw new Error(m.reason);
    if(this.delay){this.deferred.push(wire);return;}
    if(m.type==='welcome'){this.doc=new BrowserDocument(wire);this.native=new OfficeEditor(new Uint8Array(seed),fonts);projectDocument(this.native,this.state().paragraphs,this.state().paragraphs);this.ready=true;}
    else if(m.type==='resumed'){
      assert.equal(this.doc.receive(wire),'[]');
      for(const raw of JSON.parse(this.doc.pending()))this.socket.send(raw);
      this.ready=true;
    }else if(['committed','rejected'].includes(m.type))assert.equal(this.doc.receive(wire),'[]');
    else throw new Error(`Unexpected message ${m.type}`);
    if(before)projectDocument(this.native,before,this.state().paragraphs);
    assert.deepEqual(JSON.parse(this.native.state()).paragraphs,this.state().paragraphs.map(p=>p.text));
  }
  state(){return JSON.parse(this.doc.state());}
  edit(p,start,end,text){
    this.native.select(p,start,p,end);this.native.insert(text);
    for(const raw of JSON.parse(this.doc.replace(p,start,end,text)))if(this.ready)this.socket.send(raw);
    assert.deepEqual(JSON.parse(this.native.state()).paragraphs,this.state().paragraphs.map(p=>p.text));
  }
  flush(){this.delay=false;for(const wire of this.deferred.splice(0))this.message(wire);}
  async close(){this.ready=false;const done=new Promise(resolve=>this.socket.addEventListener('close',resolve,{once:true}));this.socket.close();await done;}
}
const a=new Editor(),b=new Editor();
try {
  await a.connect();await b.connect();
  // Both authors plan against the same view; hold incoming frames until both submit.
  a.delay=b.delay=true;
  a.edit(1,0,0,'ALICE😀 ');b.edit(1,0,0,'BOBあ ');
  a.flush();b.flush();
  await until(()=>!a.state().pending&&!b.state().pending&&a.state().seq===b.state().seq);
  const text=a.state().paragraphs[1].text;
  assert.ok(text.includes('ALICE😀 ')&&text.includes('BOBあ '));
  assert.deepEqual(a.state().paragraphs,b.state().paragraphs);
  const aid=a.state().replica;
  const seq=a.state().seq;
  a.socket.send(JSON.stringify({type:'presence',state:{name:'Alice',focused:true,selection:JSON.parse(a.doc.capture_selection(1,0,1,2))}}));
  await until(()=>b.peers.has(aid));
  const peerRange=JSON.parse(b.doc.resolve_selection(JSON.stringify(b.peers.get(aid).selection)));
  assert.deepEqual(peerRange,{anchor:[1,0],focus:[1,2]});
  const beforeMeasure=b.native.state();
  assert.ok(JSON.parse(b.native.selection_geometry(...peerRange.anchor,...peerRange.focus)).highlights.length);
  assert.equal(b.native.state(),beforeMeasure);assert.equal(a.state().seq,seq);
  // Offline optimistic typing and a concurrent edit from the connected peer.
  await a.close();await until(()=>!b.peers.has(aid));a.edit(2,0,0,'OFFLINE復帰 ');b.edit(2,0,0,'ONLINE ');
  await until(()=>!b.state().pending);
  await a.connect();
  await until(()=>!a.state().pending&&!b.state().pending&&a.state().seq===b.state().seq);
  assert.deepEqual(a.state().paragraphs,b.state().paragraphs);
  assert.ok(a.state().paragraphs[2].text.includes('OFFLINE復帰 '));
  // Two users edit the same real table-cell story.
  const cell=a.state().paragraphs.find(p=>p.story.startsWith('Cell'));
  assert.ok(cell,'fixture has an editable table cell');
  a.delay=b.delay=true;a.edit(cell.index,0,0,'TABLE-A ');b.edit(cell.index,0,0,'TABLE-B ');a.flush();b.flush();
  await until(()=>!a.state().pending&&!b.state().pending&&a.state().seq===b.state().seq);
  assert.deepEqual(a.state().paragraphs,b.state().paragraphs);
  const at=a.state().paragraphs[cell.index].text.length;
  a.native.select(cell.index,at,cell.index,at);
  b.edit(cell.index,0,0,'LINE-A\nLINE-B\t');
  await until(()=>!a.state().pending&&!b.state().pending&&a.state().seq===b.state().seq);
  assert.deepEqual(JSON.parse(a.native.state()).selection.focus,[cell.index,at+'LINE-A\nLINE-B\t'.length]);
  const response=await fetch(`${origin}/documents/${name}`);assert.equal(response.status,200);
  const archive=Buffer.from(await response.arrayBuffer());
  const xml=execFileSync('python3',['-c','import io,sys,zipfile; print(zipfile.ZipFile(io.BytesIO(sys.stdin.buffer.read())).read("word/document.xml").decode())'],{input:archive,encoding:'utf8'});
  for(const marker of ['ALICE😀','BOBあ','OFFLINE復帰','ONLINE','TABLE-A','TABLE-B'])assert.ok(xml.includes(marker),marker);
  // Reopen the native export through the real Office loader, not only ZIP/XML.
  const reopened = `${name.slice(0,-5)}-reopened.docx`;
  assert.equal((await fetch(`${origin}/documents/${reopened}`,{method:'PUT',body:archive})).status,201);
  const reopenedEditor=new OfficeEditor(archive,fonts);
  assert.deepEqual(JSON.parse(reopenedEditor.state()).paragraphs,a.state().paragraphs.map(p=>p.text));reopenedEditor.free();
  assert.deepEqual(a.errors,[]);assert.deepEqual(b.errors,[]);
  console.log(JSON.stringify({status:'passed',document:name,seq:a.state().seq,checks:['concurrent_unicode','offline_resume','same_table_cell','native_docx_export','native_export_reopen','native_canvas_model_projection','remote_caret_rebase','word_line_break_and_tab','peer_selection_transport_and_geometry','presence_left_without_document_edit']},null,2));
}finally{if(a.socket?.readyState===WebSocket.OPEN)await a.close();if(b.socket?.readyState===WebSocket.OPEN)await b.close();a.doc?.free();b.doc?.free();a.native?.free();b.native?.free();fonts.free();}
