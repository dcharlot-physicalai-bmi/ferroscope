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
if (!file) { console.error('usage: node ci/viewer-blocks.mjs <recording.mcap> [chrome]'); process.exit(2); }
const CHROME = process.argv[3] || process.env.CHROME
  || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

const TYPES = { '.html':'text/html', '.js':'text/javascript', '.wasm':'application/wasm',
                '.json':'application/json', '.mcap':'application/octet-stream',
                '.css':'text/css', '.svg':'image/svg+xml', '.png':'image/png' };

// Serve the viewer directory, plus the recording under test at /fixture.mcap.
const root = resolve(new URL('../viewer', import.meta.url).pathname);
const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const path = url.pathname === '/fixture.mcap' ? resolve(file)
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
    const state = await page.evaluate(() => ({
      label: document.getElementById('nmA').textContent,
      tree: document.getElementById('tree').textContent.length,
      text: document.body.innerText,
      rejections: window.__rejections || [],
    }));
    await page.close();
    return { ...state, errors: errors.concat(state.rejections) };
  };

  const whole = await drive('');
  const blocks = await drive('?blocks');
  for (const [name, r] of [['whole-file', whole], ['block', blocks]]) {
    for (const e of r.errors) fail(`${name} read: page error: ${e}`);
    if (r.tree < 20) fail(`${name} read: the topic tree is empty (${r.tree} chars)`);
    else ok(`${name} read: topic tree populated (${r.tree} chars)`);
    if (!/VERIFIED/.test(r.text)) fail(`${name} read: the receipt panel does not report VERIFIED`);
    else ok(`${name} read: the recording verified in the browser`);
    // The advantage a bundle does not have: the digest was recomputed in THIS browser.
    if (!/recomputed here/.test(r.text)) fail(`${name} read: the receipt panel does not say it recomputed here`);
    else ok(`${name} read: the receipt was recomputed here, not inherited`);
    if (/recomputed at export/.test(r.text)) fail(`${name} read: reported as a bundle`);
  }
  if (!/read in blocks/.test(blocks.label)) fail('the viewer did not say it read the file in blocks');
  else ok('the viewer said it read the file in blocks');
  if (/read in blocks/.test(whole.label)) fail('a whole-file read claimed to be a block read');

} finally {
  await browser.close();
  server.close();
}
process.exit(failed);
