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
  constructor(){this.ready=false;this.deferred=[];this.delay=false;this.errors=[];}
  async connect(){
    this.socket = new WebSocket(url);
    this.socket.onopen=()=>this.socket.send(this.doc?this.doc.resume_message():'{"type":"join"}');
    this.socket.onmessage=e=>{try{this.message(e.data);}catch(error){this.errors.push(error);}};
    await until(()=>this.ready||this.errors.length); assert.deepEqual(this.errors,[]);
  }
  message(wire){
    const m=JSON.parse(wire);
    if(['presence','presence_left','presence_rejected'].includes(m.type))return;
    if(this.delay){this.deferred.push(wire);return;}
    if(m.type==='welcome'){this.doc=new BrowserDocument(wire);this.ready=true;}
    else if(m.type==='resumed'){
      assert.equal(this.doc.receive(wire),'[]');
      for(const raw of JSON.parse(this.doc.pending()))this.socket.send(raw);
      this.ready=true;
    }else if(['committed','rejected'].includes(m.type))assert.equal(this.doc.receive(wire),'[]');
    else throw new Error(`Unexpected message ${m.type}`);
  }
  state(){return JSON.parse(this.doc.state());}
  edit(p,start,end,text){for(const raw of JSON.parse(this.doc.replace(p,start,end,text)))if(this.ready)this.socket.send(raw);}
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
  // Offline optimistic typing and a concurrent edit from the connected peer.
  await a.close();a.edit(2,0,0,'OFFLINE復帰 ');b.edit(2,0,0,'ONLINE ');
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
  const response=await fetch(`${origin}/documents/${name}`);assert.equal(response.status,200);
  const archive=Buffer.from(await response.arrayBuffer());
  const xml=execFileSync('python3',['-c','import io,sys,zipfile; print(zipfile.ZipFile(io.BytesIO(sys.stdin.buffer.read())).read("word/document.xml").decode())'],{input:archive,encoding:'utf8'});
  for(const marker of ['ALICE😀','BOBあ','OFFLINE復帰','ONLINE','TABLE-A','TABLE-B'])assert.ok(xml.includes(marker),marker);
  // Reopen the native export through the real Office loader, not only ZIP/XML.
  const reopened = `${name.slice(0,-5)}-reopened.docx`;
  assert.equal((await fetch(`${origin}/documents/${reopened}`,{method:'PUT',body:archive})).status,201);
  assert.deepEqual(a.errors,[]);assert.deepEqual(b.errors,[]);
  console.log(JSON.stringify({status:'passed',document:name,seq:a.state().seq,checks:['concurrent_unicode','offline_resume','same_table_cell','native_docx_export','native_export_reopen']},null,2));
}finally{if(a.socket?.readyState===WebSocket.OPEN)await a.close();if(b.socket?.readyState===WebSocket.OPEN)await b.close();a.doc?.free();b.doc?.free();}
