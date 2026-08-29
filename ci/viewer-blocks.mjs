/**
 * The block reader, in a real browser.
 *
 * Two claims, and neither can be checked by grep:
 *
 *   1. A recording read in 8 MB blocks folds to the SAME bundle, byte for byte, as the same
 *      recording read as one `ArrayBuffer`. Two readers are two definitions of what a bundle is
 *      unless something holds them equal, and the browser is a separate implementation surface
 *      from the CLI — the wasm comparator has already regressed on its own schedule once.
 *   2. The viewer actually wires it up: a file opened in block mode populates the page, says so,
 *      and reports a receipt it recomputed HERE rather than inheriting.
 *
 * Usage: node ci/viewer-blocks.mjs <recording.mcap> [chrome-binary]
 */
import { createServer } from 'node:http';
import { createReadStream, statSync } from 'node:fs';
import { extname, basename, resolve } from 'node:path';
// ESM ignores NODE_PATH, so PUPPETEER may name an absolute module path when the driver is
// installed somewhere other than beside this repository.
const puppeteer = (await import(process.env.PUPPETEER || 'puppeteer-core')).default;

const file = process.argv[2];
const fileB = process.argv[3];
if (!file || !fileB) {
  console.error('usage: node ci/viewer-blocks.mjs <a.mcap> <b.mcap> [chrome]');
  process.exit(2);
}
const CHROME = process.argv[4] || process.env.CHROME
  || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

const TYPES = { '.html':'text/html', '.js':'text/javascript', '.wasm':'application/wasm',
                '.json':'application/json', '.mcap':'application/octet-stream',
                '.css':'text/css', '.svg':'image/svg+xml', '.png':'image/png' };

// Serve the viewer directory, plus the recording under test at /fixture.mcap.
const root = resolve(new URL('../viewer', import.meta.url).pathname);
const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const path = url.pathname === '/fixture.mcap' ? resolve(file)
             : url.pathname === '/fixture-b.mcap' ? resolve(fileB)
             : resolve(root, '.' + (url.pathname === '/' ? '/index.html' : url.pathname));
  try {
    const st = statSync(path);
    res.writeHead(200, { 'content-type': TYPES[extname(path)] || 'application/octet-stream',
                         'content-length': st.size });
    createReadStream(path).pipe(res);
  } catch { res.writeHead(404).end('no'); }
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;
const size = statSync(resolve(file)).size;
console.log(`serving ${root} at ${base}, fixture ${basename(file)} (${(size/1048576).toFixed(1)} MB)`);

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--no-sandbox', '--disable-dev-shm-usage', '--js-flags=--max-old-space-size=8192'],
});
let failed = 0;
const fail = (m) => { console.error(`FAIL ${m}`); failed = 1; };
const ok = (m) => console.log(`  ok  ${m}`);

try {
  // ---- 1. the two readers, in the browser, on the same bytes -------------------------------
  {
    const page = await browser.newPage();
    page.on('pageerror', e => fail(`page error: ${e.message}`));
    await page.goto(`${base}/index.html`, { waitUntil: 'load' });
    await page.addScriptTag({ type: 'module', content: `
      import init, { open, BundleStream } from './pkg/ferroscope_wasm.js';
      await init();
      window.__T = { open, BundleStream };
    `});
    await page.waitForFunction('window.__T', { timeout: 60000 });

    const r = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/fixture.mcap')).arrayBuffer());
      const whole = window.__T.open(bytes);

      const blob = new Blob([bytes]);
      const BLOCK = 8 * 1048576;
      const s = new window.__T.BundleStream();
      let peakBuffered = 0, keptPoints = 0;
      for (let pass = 1; pass <= 2; pass++) {
        let more = true;
        for (let at = 0; at < blob.size && more; at += BLOCK) {
          const b = new Uint8Array(await blob.slice(at, Math.min(at + BLOCK, blob.size)).arrayBuffer());
          more = s.push(b);
          peakBuffered = Math.max(peakBuffered, s.buffered());
        }
        if (pass === 1) s.rewind();
      }
      keptPoints = s.kept_points();
      const streamed = s.finish();
      return { same: whole === streamed, wholeLen: whole.length, streamedLen: streamed.length,
               peakBuffered, keptPoints,
               head: streamed.slice(0, 120), wheadxx: whole.slice(0, 120) };
    });

    if (!r.same) {
      fail(`block-read bundle differs from whole-file bundle (${r.streamedLen} vs ${r.wholeLen} chars)`);
      console.error(`   blocks: ${r.head}`);
      console.error(`   whole : ${r.wheadxx}`);
    } else ok(`block-read bundle is byte-identical to the whole-file bundle (${r.wholeLen} chars)`);

    // The reason the door exists: what is held is one record and one screenful, not the file.
    if (r.peakBuffered >= size / 4) fail(`the fold buffered ${r.peakBuffered} of ${size} bytes`);
    else ok(`framing held at most ${(r.peakBuffered/1048576).toFixed(2)} MB of a ${(size/1048576).toFixed(1)} MB recording`);
    if (!(r.keptPoints > 0)) fail('no lane points kept at all');
    else ok(`${r.keptPoints.toLocaleString()} lane points kept`);
    await page.close();
  }

  // ---- 2. the viewer, driven the way a reader drives it ------------------------------------
  // Both ways, because a check that only runs the new path cannot tell "block mode is broken"
  // from "this assertion was never true".
  const drive = async (query) => {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    // `accept` is async and is called from an event handler, so anything it throws becomes an
    // unhandled REJECTION, which `pageerror` does not see. That is the exact shape of the
    // failure this page already shipped once — minutes of silence at the size ceiling — so the
    // gate listens for it rather than trusting that a quiet page is a working one.
    await page.evaluateOnNewDocument(() => {
      window.__rejections = [];
      addEventListener('unhandledrejection', e =>
        window.__rejections.push(String(e.reason && e.reason.message || e.reason)));
    });
    await page.goto(`${base}/index.html${query}`, { waitUntil: 'load' });
    await page.waitForSelector('#fileA', { timeout: 60000 });
    // The file input is in the static HTML; its handler is not. A change event fired before the
    // module attaches is simply LOST, which reads exactly like a page that ignored the file —
    // and that is what a slower machine, or a network, produces. `#ver` is written on the line
    // after `await init()`, so it is the honest readiness signal.
    await page.waitForFunction(() => /^v\d/.test(document.getElementById('ver').textContent),
      { timeout: 120000, polling: 250 });
    const input = await page.$('#fileA');
    await input.uploadFile(resolve(file));
    // Wait for the page to be DRAWN, not merely labelled: a label set before the panels are
    // built would let a page that renders nothing pass.
    await page.waitForFunction(
      () => document.getElementById('tree').textContent.length > 20
         || document.getElementById('err').style.display === 'block'
         || (window.__rejections || []).length > 0,
      { timeout: 240000, polling: 500 })
      .catch(() => {});   // a page that never draws is a failure to REPORT, not to throw
    // Meshes arrive after the tree does; give them a bounded chance rather than racing them.
    await page.waitForFunction(
      () => (+document.body.dataset.meshes || 0) + (+document.body.dataset.meshesFailed || 0) > 0,
      { timeout: 60000, polling: 250 }).catch(() => {});
    const state = await page.evaluate(() => ({
      label: document.getElementById('nmA').textContent,
      tree: document.getElementById('tree').textContent.length,
      text: document.body.innerText,
      meshes: +document.body.dataset.meshes || 0,
      meshFailed: +document.body.dataset.meshesFailed || 0,
      rejections: window.__rejections || [],
    }));
    await page.close();
    return { ...state, errors: errors.concat(state.rejections) };
  };

  const whole = await drive('');
  const blocks = await drive('?blocks');
  for (const [name, r] of [['whole-file', whole], ['block', blocks]]) {
    for (const e of r.errors) fail(`${name} read: page error: ${e}`);
    // What the 3-D view ACTUALLY got. The first version of this checked that no complaint
    // appeared on the page, and passed a build with the mesh path disabled: the complaint was
    // real but a note already occupied the strip, so it was silently dropped. "The page did not
    // complain" is not evidence.
    if (r.meshFailed > 0) fail(`${name} read: ${r.meshFailed} mesh(es) failed to load`);
    else if (!(r.meshes > 0)) fail(`${name} read: the 3-D view loaded no mesh at all`);
    else ok(`${name} read: the 3-D view loaded ${r.meshes} mesh(es)`);
    if (r.tree < 20) fail(`${name} read: the topic tree is empty (${r.tree} chars)`);
    else ok(`${name} read: topic tree populated (${r.tree} chars)`);
    if (!/VERIFIED/.test(r.text)) fail(`${name} read: the receipt panel does not report VERIFIED`);
    else ok(`${name} read: the recording verified in the browser`);
    // The advantage a bundle does not have: the digest was recomputed in THIS browser.
    if (!/recomputed here/.test(r.text)) fail(`${name} read: the receipt panel does not say it recomputed here`);
    else ok(`${name} read: the receipt was recomputed here, not inherited`);
    if (/recomputed at export/.test(r.text)) fail(`${name} read: reported as a bundle`);
  }
  // ---- 3. comparing two recordings the page never held ------------------------------------
  // The last thing a block read could not do. `diff` takes bytes and a streamed recording has
  // none, so the comparison reads both files itself -- three passes each, the third with both
  // walked together. It must come out the SAME as the comparison made from the bytes.
  {
    const page = await browser.newPage();
    page.on('pageerror', e => fail(`compare: page error: ${e.message}`));
    await page.goto(`${base}/index.html`, { waitUntil: 'load' });
    await page.addScriptTag({ type: 'module', content: `
      import init, { diff, DiffStream } from './pkg/ferroscope_wasm.js';
      await init();
      window.__D = { diff, DiffStream };
    `});
    await page.waitForFunction('window.__D', { timeout: 60000 });
    const r = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/fixture.mcap')).arrayBuffer());
      const other = new Uint8Array(await (await fetch('/fixture-b.mcap')).arrayBuffer());
      const held = window.__D.diff(bytes, other, 0, 0);

      const fa = new Blob([bytes]), fb = new Blob([other]);
      const BLOCK = 8 * 1048576;
      const d = new window.__D.DiffStream(0, 0);
      const feed = async (blob, which) => {
        for (let at = 0; at < blob.size; at += BLOCK) {
          const b = new Uint8Array(await blob.slice(at, Math.min(at + BLOCK, blob.size)).arrayBuffer());
          if (!(which === 'a' ? d.push_a(b) : d.push_b(b))) break;
        }
      };
      for (let p = 0; p < 2; p++) { await feed(fa, 'a'); await feed(fb, 'b'); d.rewind(); }
      let ia = 0, ib = 0;
      while (!d.refused()) {
        let moved = false;
        if (d.wants_a()) {
          if (ia < fa.size) { const e = Math.min(ia + BLOCK, fa.size);
            d.push_a(new Uint8Array(await fa.slice(ia, e).arrayBuffer())); ia = e; moved = true; }
          else d.end_a();
        }
        if (d.wants_b()) {
          if (ib < fb.size) { const e = Math.min(ib + BLOCK, fb.size);
            d.push_b(new Uint8Array(await fb.slice(ib, e).arrayBuffer())); ib = e; moved = true; }
          else d.end_b();
        }
        if (!moved) break;
      }
      const streamed = d.finish();
      return { same: held === streamed, held: held.slice(0, 160), streamed: streamed.slice(0, 160) };
    });
    if (!r.same) {
      fail('the streamed comparison differs from the held one');
      console.error(`   held    : ${r.held}`);
      console.error(`   streamed: ${r.streamed}`);
    } else ok('the streamed comparison is the same comparison, character for character');
    await page.close();
  }

  // And the page must actually drive it: open a second recording in block mode and require a
  // verdict on the strip rather than a refusal.
  {
    const page = await browser.newPage();
    page.on('pageerror', e => fail(`compare page: ${e.message}`));
    await page.goto(`${base}/index.html?blocks`, { waitUntil: 'load' });
    await page.waitForSelector('#fileA', { timeout: 60000 });
    // The file input is in the static HTML; its handler is not. A change event fired before the
    // module attaches is simply LOST, which reads exactly like a page that ignored the file —
    // and that is what a slower machine, or a network, produces. `#ver` is written on the line
    // after `await init()`, so it is the honest readiness signal.
    await page.waitForFunction(() => /^v\d/.test(document.getElementById('ver').textContent),
      { timeout: 120000, polling: 250 });
    await (await page.$('#fileA')).uploadFile(resolve(file));
    await page.waitForFunction(() => document.getElementById('tree').textContent.length > 20,
      { timeout: 600000, polling: 500 }).catch(() => {});
    // Clear the strip first: opening A leaves a note in it, and a wait that merely required
    // "the strip says something" would pass on that note without B ever being compared.
    await page.evaluate(() => {
      const s = document.getElementById('strip');
      s.textContent = ''; s.style.display = 'none'; s.className = '';
    });
    await (await page.$('#fileB')).uploadFile(resolve(fileB));
    await page.waitForFunction(
      () => { const s = document.getElementById('strip');
              return s && s.textContent.length > 0 && !/^comparing /.test(s.textContent); },
      { timeout: 600000, polling: 250 }).catch(() => {});
    const strip = await page.evaluate(() => ({
      text: document.getElementById('strip').textContent,
      cls: document.getElementById('strip').className,
    }));
    if (/cannot be compared|no longer available/.test(strip.text))
      fail(`the page refused to compare two block-read recordings: ${strip.text}`);
    else if (!/vs/.test(strip.text))
      fail(`the page produced no verdict: ${strip.text.slice(0, 120)}`);
    else ok(`the page compared two block-read recordings: ${strip.text.slice(0, 90)}`);
    await page.close();
  }

  // ---- 4. a mesh, out of a recording the page never held -----------------------------------
  // A glTF has to be WHOLE to be a mesh, so the lanes a block read keeps are not enough. Going
  // back to the file for it is one more pass, and it must produce the same bytes the whole-file
  // reader produces.
  {
    const page = await browser.newPage();
    page.on('pageerror', e => fail(`mesh: page error: ${e.message}`));
    await page.goto(`${base}/index.html`, { waitUntil: 'load' });
    await page.addScriptTag({ type: 'module', content: `
      import init, { attachment, AttachmentStream, open } from './pkg/ferroscope_wasm.js';
      await init();
      window.__M = { attachment, AttachmentStream, open };
    `});
    await page.waitForFunction('window.__M', { timeout: 60000 });
    const r = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/fixture.mcap')).arrayBuffer());
      // Whatever mesh this recording declares — read it out of the bundle rather than guessing.
      const b = JSON.parse(window.__M.open(bytes));
      const name = (b.geometry || []).map(g => g.mesh).find(Boolean);
      if (!name) return { skipped: true };
      const held = window.__M.attachment(bytes, name);
      const blob = new Blob([bytes]);
      const BLOCK = 1 << 20;
      const s = new window.__M.AttachmentStream(name);
      let fed = 0;
      for (let at = 0; at < blob.size; at += BLOCK) {
        const blk = new Uint8Array(await blob.slice(at, Math.min(at + BLOCK, blob.size)).arrayBuffer());
        fed += blk.length;
        if (!s.push(blk)) break;
      }
      const got = s.take();
      let same = held.length === got.length;
      for (let i = 0; same && i < held.length; i++) if (held[i] !== got[i]) same = false;
      return { name, same, bytes: got.length, fed, total: blob.size };
    });
    if (r.skipped) console.log('  --  this recording declares no mesh; nothing to pull');
    else if (!r.same) fail(`the mesh pulled in blocks differs from the one read whole (${r.name})`);
    else {
      ok(`pulled ${r.name} (${r.bytes} bytes) out of a recording read in blocks`);
      if (r.fed >= r.total) fail('pulling the mesh read the whole file rather than stopping at it');
      else ok(`stopped after ${(100 * r.fed / r.total).toFixed(1)}% of the file`);
    }
    await page.close();
  }

  // ---- 5. with the network turned off -------------------------------------------------------
  // The README says "turn your network off and it still works" and the crate docs say the page
  // opens *your* recording with the network turned off. That is a claim someone with a
  // confidential run might rely on, and it had never been tested. So: load the page, go
  // OFFLINE, and then do the work.
  {
    const page = await browser.newPage();
    const errors = [];
    const attempted = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.goto(`${base}/index.html`, { waitUntil: 'load' });
    await page.waitForSelector('#fileA', { timeout: 60000 });
    await page.waitForFunction(() => /^v\d/.test(document.getElementById('ver').textContent),
      { timeout: 120000, polling: 250 });

    // Everything the page needs must already be here. Record anything it reaches for anyway —
    // a request that fails silently would leave the page working and the claim still broken.
    page.on('request', r => attempted.push(r.url()));
    await page.setOfflineMode(true);

    await (await page.$('#fileA')).uploadFile(resolve(file));
    await page.waitForFunction(
      () => document.getElementById('tree').textContent.length > 20
         || document.getElementById('err').style.display === 'block',
      { timeout: 300000, polling: 250 }).catch(() => {});
    await (await page.$('#fileB')).uploadFile(resolve(fileB));
    await page.waitForFunction(
      () => { const s = document.getElementById('strip');
              return s.textContent.length > 0 && !/^comparing /.test(s.textContent); },
      { timeout: 300000, polling: 250 }).catch(() => {});

    const off = await page.evaluate(() => ({
      tree: document.getElementById('tree').textContent.length,
      strip: document.getElementById('strip').textContent,
      err: document.getElementById('err').style.display === 'block'
        ? document.getElementById('err').textContent : '',
      meshes: +document.body.dataset.meshes || 0,
    }));
    await page.setOfflineMode(false);
    await page.close();

    if (off.err) fail(`offline: the page reported "${off.err.slice(0, 90)}"`);
    else if (off.tree < 20) fail('offline: the recording did not open');
    else if (!/vs/.test(off.strip)) fail(`offline: no verdict — "${off.strip.slice(0, 80)}"`);
    else ok(`offline: opened, compared and drew ${off.meshes} mesh(es) with the network off`);
    for (const e of errors) fail(`offline: page error: ${e}`);
    if (attempted.length) {
      // A FAILURE, not a note. "Nothing is uploaded" is a claim someone with a confidential run
      // relies on, and a page that reaches out and silently swallows the error still works
      // offline while breaking it. If a request ever becomes legitimate, this forces the
      // decision to be made deliberately rather than noticed later.
      fail(`offline: the page attempted ${attempted.length} request(s) — nothing should leave it`);
      for (const u of [...new Set(attempted)].slice(0, 5)) console.error(`      ${u}`);
    } else ok('offline: the page made no requests at all');
  }

  if (!/read in blocks/.test(blocks.label)) fail('the viewer did not say it read the file in blocks');
  else ok('the viewer said it read the file in blocks');
  if (/read in blocks/.test(whole.label)) fail('a whole-file read claimed to be a block read');

} finally {
  await browser.close();
  server.close();
}
process.exit(failed);
