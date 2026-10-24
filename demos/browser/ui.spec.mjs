import { test, expect } from '@playwright/test';

async function ready(page){await expect(page.locator('#editor')).toHaveAttribute('data-ready','true');await expect(page.locator('#status')).toHaveText('● 同期済み');}
const editor=page=>page.evaluate(()=>jadeDemo.editor());
const model=page=>page.evaluate(()=>jadeDemo.state());
async function clickPage(page){
  const state=await editor(page),box=await page.locator('canvas').first().boundingBox();
  const width=await page.locator('canvas').first().evaluate(c=>c.width),scale=box.width/width;
  await page.mouse.click(box.x+state.caret.x*scale,box.y+(state.caret.y+state.caret.height/2)*scale);
  await expect(page.locator('#ime')).toBeFocused();
}
async function paragraph(page,index){
  await page.locator('#ime').press('Control+Home');
  for(let n=0;n<100;n++){
    const current=(await editor(page)).selection.focus[0];
    if(current===index){await page.keyboard.press('Home');return;}
    expect(current).toBeLessThan(index);
    await page.keyboard.press('End');await page.keyboard.press('ArrowRight');
  }
  throw new Error('Could not navigate to paragraph');
}
async function converged(a,b){
  await expect.poll(async()=>{
    const [ma,mb,ea,eb]=await Promise.all([model(a),model(b),editor(a),editor(b)]);
    return ma.pending===0&&mb.pending===0&&ma.seq===mb.seq&&JSON.stringify(ea.paragraphs)===JSON.stringify(eb.paragraphs)&&JSON.stringify(ea.paragraphs)===JSON.stringify(ma.paragraphs.map(p=>p.text));
  }).toBe(true);
}

test('page clicks and native keys edit the Word canvas and the same real table cell',async({context,request})=>{
  const seed=await request.get('/documents/demo.docx');
  const name=`ui-direct-${Date.now()}.docx`;
  expect((await request.put(`/documents/${name}`,{data:await seed.body()})).status()).toBe(201);
  const a=await context.newPage(),b=await context.newPage();
  const errors=[];for(const page of [a,b])page.on('pageerror',error=>errors.push(String(error)));
  try{
    await Promise.all([a.goto(`/?name=Alice&document=${name}`),b.goto(`/?name=Bob&document=${name}`)]);
    await Promise.all([ready(a),ready(b)]);
    expect(await a.locator('textarea').count()).toBe(0);
    await clickPage(a);await clickPage(b);
    await paragraph(a,1);await paragraph(b,1);
    await Promise.all([a.keyboard.insertText('CANVAS-ALICE😀'),b.keyboard.insertText('CANVAS-BOBあ')]);
    await converged(a,b);
    const text=(await editor(a)).paragraphs[1];expect(text).toContain('CANVAS-ALICE😀');expect(text).toContain('CANVAS-BOBあ');
    // Real selection keys replace a range, including a full emoji character.
    await paragraph(a,1);await a.keyboard.press('ArrowRight');
    await a.keyboard.press('Shift+ArrowRight');await a.keyboard.press('Shift+ArrowRight');
    await a.keyboard.insertText('置換');await converged(a,b);
    // Keep Alice's caret at the same character boundary while Bob inserts before it.
    await paragraph(a,1);await a.keyboard.press('End');
    const caret=(await editor(a)).selection.focus;
    await paragraph(b,1);await b.keyboard.insertText('REMOTE ');await converged(a,b);
    expect((await editor(a)).selection.focus).toEqual([caret[0],caret[1]+7]);
    const cell=(await model(a)).paragraphs.find(p=>p.story.startsWith('Cell')&&p.text.includes('Alice / Bob'))??(await model(a)).paragraphs.find(p=>p.story.startsWith('Cell'));
    await paragraph(a,cell.index);await paragraph(b,cell.index);
    await Promise.all([a.keyboard.insertText('CELL-A '),b.keyboard.insertText('CELL-B ')]);await converged(a,b);
    expect((await editor(a)).paragraphs[cell.index]).toContain('CELL-A ');expect((await editor(a)).paragraphs[cell.index]).toContain('CELL-B ');
    await a.keyboard.press('Shift+ArrowLeft');await a.keyboard.press('Backspace');await converged(a,b);
    // Soft line breaks are real Word line breaks, not a browser paragraph node.
    await a.keyboard.press('Enter');await a.keyboard.insertText('次の行');await converged(a,b);
    expect((await editor(a)).paragraphs[cell.index]).toContain('\n次の行');
    // Offline text is painted immediately in the page, then reconciled.
    await a.getByRole('button',{name:'オフラインにする',exact:true}).click();
    await a.locator('#ime').focus();await a.keyboard.insertText('OFFLINE-CANVAS');
    expect((await editor(a)).paragraphs[cell.index]).toContain('OFFLINE-CANVAS');
    await b.keyboard.insertText('ONLINE-CANVAS');
    await a.getByRole('button',{name:'再接続する',exact:true}).click();await converged(a,b);
    expect((await editor(b)).paragraphs[cell.index]).toContain('OFFLINE-CANVAS');
    expect((await editor(a)).paragraphs[cell.index]).toContain('ONLINE-CANVAS');
    expect(await a.locator('#notice').textContent()).toBe('');expect(errors).toEqual([]);
  }finally{await context.close();}
});

test('canvas editors preserve an IME composition through peer edits and reconnect',async({page})=>{
  const errors=[];page.on('pageerror',error=>errors.push(String(error)));
  await page.goto('/browser-tests.html');await page.getByRole('button',{name:'テストを実行',exact:true}).click();
  await expect(page.locator('#result')).toHaveAttribute('data-status',/passed|failed/,{timeout:70_000});
  const result=await page.locator('#result').textContent();
  expect(result).toContain('"status": "passed"');expect(result).toContain('canvas-editor-without-textarea');expect(result).toContain('named-peer-caret-and-selection');expect(result).toContain('peer-left-and-resume');expect(result).toContain('active-peers-only');
  expect(result).toContain('cancel-selected-composition');expect(result).toContain('ime-focus-survives-repagination');
  expect(result).toContain('final-input-before-compositionend');expect(result).toContain('final-input-after-compositionend');expect(errors).toEqual([]);
});


test('named peer carets and selections track focus, table cells, edits, IME and reconnect',async({context,request})=>{
  const seed=await request.get('/documents/demo.docx'),name=`ui-presence-${Date.now()}.docx`;
  expect((await request.put(`/documents/${name}`,{data:await seed.body()})).status()).toBe(201);
  const a=await context.newPage(),b=await context.newPage(),errors=[];
  for(const page of [a,b])page.on('pageerror',error=>errors.push(String(error)));
  await Promise.all([a.goto(`/?name=Alice&document=${name}`),b.goto(`/?name=Bob&document=${name}`)]);
  await Promise.all([ready(a),ready(b)]);
  const bid=(await model(b)).replica,caret=a.locator(`.peer-caret[data-peer="${bid}"]`);
  const selected=a.locator(`.peer-selection[data-peer="${bid}"]`);
  await b.bringToFront();await clickPage(b);await paragraph(b,1);
  await b.keyboard.press('Shift+ArrowRight');await b.keyboard.press('Shift+ArrowRight');
  let range=(await editor(b)).selection;
  await expect(caret).toHaveAttribute('data-focus',JSON.stringify(range.focus));
  await expect(caret).toHaveAttribute('data-focused','true');
  await expect(caret.locator('.peer-name')).toHaveText('Bob');expect(await selected.count()).toBeGreaterThan(0);
  // Exercise editor blur independently of window-manager tab activation.
  await b.locator('#connection').focus();
  await a.bringToFront();await expect(caret).toHaveCount(0);
  await expect(selected).toHaveCount(0);await expect(a.locator(`.peer-chip[data-peer="${bid}"]`)).toHaveCount(0);
  await paragraph(a,1);await a.keyboard.insertText('PRE ');await converged(a,b);
  await expect(caret).toHaveCount(0);
  await b.bringToFront();await b.locator('#ime').focus();
  await expect(caret).toHaveAttribute('data-focus',JSON.stringify([range.focus[0],range.focus[1]+4]));
  const cell=(await model(b)).paragraphs.find(p=>p.story.startsWith('Cell')&&p.text.length>8);
  await b.bringToFront();await paragraph(b,cell.index);
  for(let i=0;i<5;i++)await b.keyboard.press('Shift+ArrowRight');
  range=(await editor(b)).selection;
  await expect(caret).toHaveAttribute('data-focus',JSON.stringify(range.focus));
  const rectangles=(await editor(b)).highlights;
  await expect.poll(()=>selected.count()).toBe(rectangles.length);
  const rendered=await selected.first().evaluate(el=>({x:parseFloat(el.style.left),y:parseFloat(el.style.top),width:parseFloat(el.style.width)}));
  expect(rendered.x).toBeCloseTo(rectangles[0].x);expect(rendered.y).toBeCloseTo(rectangles[0].y);expect(rendered.width).toBeCloseTo(rectangles[0].width);
  // A received presence frame must not cancel or move the author's IME draft.
  await a.bringToFront();await a.locator('#ime').focus();
  // Playwright's dispatchEvent falls back to Event for composition events,
  // which drops .data. Use the browser's real CompositionEvent constructor.
  await a.locator('#ime').evaluate(el=>el.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true,data:''})));
  await a.locator('#ime').evaluate(el=>el.dispatchEvent(new CompositionEvent('compositionupdate',{bubbles:true,data:'入力中😀'})));
  const draft=await editor(a);
  expect(draft.composing).toBe(true);expect(draft.paragraphs[draft.selection.focus[0]]).toContain('入力中😀');
  await b.bringToFront();await b.locator('#ime').focus();await b.keyboard.press('ArrowRight');await b.keyboard.press('ArrowRight');
  await expect(caret).toHaveAttribute('data-focus',JSON.stringify((await editor(b)).selection.focus));
  expect(await editor(a)).toEqual(draft);
  await a.locator('#ime').evaluate(el=>el.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true,data:'入力中😀'})));await converged(a,b);
  await b.getByRole('button',{name:'オフラインにする',exact:true}).click();await expect(caret).toHaveCount(0);
  await b.getByRole('button',{name:'再接続する',exact:true}).click();await ready(b);await expect(caret).toHaveCount(0);
  await b.locator('#ime').focus();await expect(caret).toHaveCount(1);
  const c=await context.newPage();c.on('pageerror',error=>errors.push(String(error)));
  await c.goto(`/?name=Charlie&document=${name}`);await ready(c);
  await a.locator('#connection').focus();await b.locator('#connection').focus();
  await expect(c.locator('.peer-caret')).toHaveCount(0);
  await b.bringToFront();await b.locator('#ime').focus();
  await expect(c.locator('.peer-caret')).toHaveCount(1);
  await expect(c.locator('.peer-name')).toHaveText('Bob');
  expect(await a.locator('#notice').textContent()).toBe('');expect(errors).toEqual([]);
});
