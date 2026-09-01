/**
 * The chunk codecs, in a real browser.
 *
 * Reading zstd and lz4 is the reason `ferroscope-mcap` takes any dependency at all, and the
 * reason it takes PURE-RUST ones: the C implementations cannot go to wasm, so a browser could
 * never open a `ros2 bag` file. That argument is only worth making if the decoders actually run
 * there, and "it is pure Rust so it will behave the same" is exactly the kind of assumption that
 * deserves a measurement rather than a sentence.
 *
 * The fixtures are written by the REFERENCE implementation (see
 * `examples/make_compressed_fixtures.rs`) at three codecs over identical messages. The claim is
 * that `open()` returns the same summary for all three: a decoder that silently produced zeros,
 * truncated a chunk, or mangled a byte would diverge, and one that never ran would throw.
 *
 * Usage: node ci/browser-codecs.mjs <dir-with-codec-*.mcap> [chrome]
 */
import { createServer } from 'node:http';
import { createReadStream, statSync } from 'node:fs';
import { extname, resolve, join } from 'node:path';
import { existsSync } from 'node:fs';
const puppeteer = (await import(process.env.PUPPETEER || 'puppeteer-core')).default;

const dir = process.argv[2];
if (!dir) { console.error('usage: node ci/browser-codecs.mjs <fixture-dir> [chrome]'); process.exit(2); }
const CHROME = process.argv[3] || process.env.CHROME
  || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

const CODECS = ['none', 'zstd', 'lz4'];
const TYPES = { '.html':'text/html', '.js':'text/javascript', '.wasm':'application/wasm',
                '.json':'application/json', '.mcap':'application/octet-stream',
                '.css':'text/css', '.svg':'image/svg+xml', '.png':'image/png' };

const root = resolve(new URL('../viewer', import.meta.url).pathname);
const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const m = url.pathname.match(/^\/codec-(none|zstd|lz4)\.mcap$/);
  const path = url.pathname === '/ros2-tf.mcap' ? resolve(join(dir, 'ros2-tf.mcap'))
             : url.pathname === '/ros2.mcap' ? resolve(join(dir, 'ros2.mcap'))
             : m ? resolve(join(dir, `codec-${m[1]}.mcap`))
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
for (const c of CODECS) {
  console.log(`  fixture codec-${c}.mcap ${statSync(join(dir, `codec-${c}.mcap`)).size} bytes`);
}

const browser = await puppeteer.launch({
  executablePath: CHROME, headless: true,
  args: ['--no-sandbox', '--disable-dev-shm-usage'],
});
let failed = 0;
const fail = (m) => { console.error(`FAIL ${m}`); failed = 1; };

try {
  const page = await browser.newPage();
  page.on('pageerror', e => fail(`page error: ${e.message}`));
  await page.goto(`${base}/index.html`, { waitUntil: 'load' });
  await page.addScriptTag({ type: 'module', content: `
    import init, { open } from './pkg/ferroscope_wasm.js';
    await init();
    window.__T = { open };
  `});
  await page.waitForFunction('window.__T', { timeout: 60000 });

  const out = await page.evaluate(async (codecs) => {
    const r = {};
    for (const c of codecs) {
      const bytes = new Uint8Array(await (await fetch(`/codec-${c}.mcap`)).arrayBuffer());
      try { r[c] = { ok: true, summary: window.__T.open(bytes), bytes: bytes.length }; }
      catch (e) { r[c] = { ok: false, err: String(e) }; }
    }
    return r;
  }, CODECS);

  for (const c of CODECS) {
    if (!out[c].ok) { fail(`codec ${c} did not open in the browser: ${out[c].err}`); continue; }
    console.log(`  ok  codec ${c}: opened ${out[c].bytes} bytes in the browser`);
  }
  if (!failed) {
    // The decisive assertion. Same messages, three codecs, one answer.
    for (const c of ['zstd', 'lz4']) {
      if (out[c].summary !== out.none.summary) {
        fail(`codec ${c} decoded to a DIFFERENT summary than the uncompressed original`);
        console.error(`  none: ${out.none.summary.slice(0, 300)}`);
        console.error(`  ${c}:   ${out[c].summary.slice(0, 300)}`);
      } else {
        console.log(`  ok  codec ${c} agrees with the uncompressed original, byte for byte`);
      }
    }
    // And the summary must be real: a decoder returning nothing would agree trivially.
    const s = out.none.summary;
    if (!s.includes('/sensor/temperature')) fail('the summary does not name the fixture topic');
    else if (!s.includes('2000')) fail('the summary does not carry the fixture message count');
    else console.log('  ok  the agreed summary names the topic and all 2000 messages');
    // Compression must actually have happened, or all three files are the same bytes and the
    // agreement above proves nothing about a codec.
    if (!(out.zstd.bytes < out.none.bytes * 0.9)) {
      fail(`codec-zstd.mcap (${out.zstd.bytes}) is not meaningfully smaller than codec-none.mcap (${out.none.bytes}): is it actually compressed?`);
    } else {
      console.log(`  ok  the zstd fixture really is compressed (${out.zstd.bytes} vs ${out.none.bytes} bytes)`);
    }
  }
  // ---- a real ROS 2 bag, in the tab ------------------------------------------------------
  // Reading the container is not reading the data, and the two bundle readers are separate
  // implementations: CDR decoding was added to the streaming one first, and the CLI plotted a
  // `ros2 bag` while the BROWSER — which uses the slice reader — showed the same file as a topic
  // list with nothing in it. A Rust test now pins the two readers equal; this pins the claim
  // that actually gets made, which is that a tab opens a robot log.
  if (existsSync(join(dir, 'ros2.mcap'))) {
    const out = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/ros2.mcap')).arrayBuffer());
      try {
        const b = JSON.parse(window.__T.open(bytes));
        return { ok: true, scalars: Object.keys(b.scalars || {}), messages: b.messages };
      } catch (e) { return { ok: false, err: String(e) }; }
    });
    if (!out.ok) fail(`the browser could not open a ROS 2 bag: ${out.err}`);
    else if (out.scalars.length !== 11) {
      fail(`expected 11 lanes from JointState in the browser, got ${out.scalars.length}: ${out.scalars}`);
    } else if (!out.scalars.includes('/joint_states:position[1]')) {
      fail(`lanes are not named from the definition: ${out.scalars}`);
    } else {
      console.log(`  ok  a ROS 2 bag opened in the browser: ${out.messages} messages, ${out.scalars.length} named lanes`);
    }
  }
  // ---- a transform tree draws, with no geometry at all ------------------------------------
  // ROS 2 publishes WHERE things are on /tf and leaves what they look like to a separate robot
  // description, so a real bag routinely carries a full transform tree and nothing to draw. The
  // data landing in `frames` is one claim; that the 3-D view actually puts something on screen
  // is another, and only the second is what a reader sees.
  if (existsSync(join(dir, 'ros2-tf.mcap'))) {
    const data = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/ros2-tf.mcap')).arrayBuffer());
      try {
        const b = JSON.parse(window.__T.open(bytes));
        return { ok: true, frames: Object.keys(b.frames || {}), geometry: (b.geometry || []).length };
      } catch (e) { return { ok: false, err: String(e) }; }
    });
    if (!data.ok) fail(`the browser could not open a /tf bag: ${data.err}`);
    else if (!data.frames.includes('base_link') || !data.frames.includes('lidar')) {
      fail(`transform tree did not become frames: ${JSON.stringify(data.frames)}`);
    } else if (data.geometry !== 0) {
      fail(`the /tf fixture is supposed to carry NO geometry, but has ${data.geometry}`);
    } else {
      console.log(`  ok  a /tf bag decodes to frames ${data.frames.join(', ')} with no geometry`);
    }

    // And now the render path: load it the way a person does and count what reached the scene.
    const drawn = await page.evaluate(async () => {
      const bytes = new Uint8Array(await (await fetch('/ros2-tf.mcap')).arrayBuffer());
      const dt = new DataTransfer();
      dt.items.add(new File([bytes], 'ros2-tf.mcap'));
      const input = document.querySelector('input[type=file]');
      if (!input) return 'no file input on the page';
      input.files = dt.files;
      input.dispatchEvent(new Event('change', { bubbles: true }));
      for (let i = 0; i < 100; i++) {
        await new Promise(r => setTimeout(r, 100));
        if (document.body.dataset.frames) return document.body.dataset.frames;
      }
      return '0';
    });
    if (drawn !== '2') fail(`the 3-D view drew ${drawn} frame axes for a two-frame /tf bag`);
    else console.log('  ok  the 3-D view drew both frames of a bag with no geometry in it');
  }
} finally {
  await browser.close();
  server.close();
}
process.exit(failed);
