/**
 * A reordered reduction on REAL silicon.
 *
 * NOT run in CI: GitHub's runners have no GPU. Run it on a machine that does —
 *
 *   PUPPETEER=<path to puppeteer-core> node ci/gpu-reduction.mjs
 *
 * It drives Chrome against the deployed viewer origin because WebGPU needs a SECURE CONTEXT:
 * `about:blank` reports no `navigator.gpu` at all, which reads exactly like a machine without a
 * GPU and cost an hour of chasing the wrong thing.
 *
 * The CPU experiment models what a GPU does to a sum. This does it: same values, same shader,
 * different workgroup counts — which is the one thing a fabric picks for you — on an Apple GPU
 * through WebGPU. It also measures the other half, which is not reordering at all: the fabric
 * has no f64, so the values must be narrowed before they ever reach it.
 */
const puppeteer = (await import(process.env.PUPPETEER)).default;
const browser = await puppeteer.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true, args: ['--no-sandbox', '--enable-unsafe-webgpu'],
});
const page = await browser.newPage();
page.on('pageerror', e => console.log('  page error:', e.message));
await page.goto('https://ferroscope.physicalai-bmi.org/assets/ferroscope/', { waitUntil: 'load', timeout: 120000 });

const out = await page.evaluate(async () => {
  const adapter = await navigator.gpu.requestAdapter();
  const device = await adapter.requestDevice();
  const info = adapter.info || {};

  // The same deterministic generator the CPU experiment uses (SplitMix64), so the two
  // measurements are of the same numbers rather than of two different samples.
  function makeValues(n, shape) {
    let s0 = 0x1234n, s1 = 0x5EEDABCDn;   // 64-bit split as two 32-bit halves
    let state = (0x5EED1234n << 32n) | 0xABCD0001n;
    const M = (1n << 64n) - 1n;
    const next = () => {
      state = (state + 0x9E3779B97F4A7C15n) & M;
      let z = state;
      z = ((z ^ (z >> 30n)) * 0xBF58476D1CE4E5B9n) & M;
      z = ((z ^ (z >> 27n)) * 0x94D049BB133111EBn) & M;
      return z ^ (z >> 31n);
    };
    const unit = () => Number(next() >> 11n) / 2 ** 53;
    const v = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      v[i] = shape === 'uniform' ? unit() : 10 ** (unit() * 10 - 5);
    }
    return v;
  }

  const shader = device.createShaderModule({ code: `
    @group(0) @binding(0) var<storage, read> src: array<f32>;
    @group(0) @binding(1) var<storage, read_write> partial: array<f32>;
    var<workgroup> scratch: array<f32, 256>;
    @compute @workgroup_size(256)
    fn main(@builtin(global_invocation_id) gid: vec3<u32>,
            @builtin(local_invocation_id) lid: vec3<u32>,
            @builtin(workgroup_id) wid: vec3<u32>,
            @builtin(num_workgroups) nwg: vec3<u32>) {
      let n = arrayLength(&src);
      let stride = nwg.x * 256u;
      var acc = 0.0;
      var i = gid.x;
      // A grid-stride loop: which elements land in which workgroup is decided by how many
      // workgroups the dispatch happens to have, and nothing else.
      loop {
        if (i >= n) { break; }
        acc = acc + src[i];
        i = i + stride;
      }
      scratch[lid.x] = acc;
      workgroupBarrier();
      var s = 128u;
      loop {
        if (s == 0u) { break; }
        if (lid.x < s) { scratch[lid.x] = scratch[lid.x] + scratch[lid.x + s]; }
        workgroupBarrier();
        s = s / 2u;
      }
      if (lid.x == 0u) { partial[wid.x] = scratch[0]; }
    }
  `});
  const pipeline = device.createComputePipeline({ layout: 'auto', compute: { module: shader, entryPoint: 'main' } });

  async function gpuSum(f32vals, groups) {
    const srcBuf = device.createBuffer({ size: f32vals.byteLength, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    device.queue.writeBuffer(srcBuf, 0, f32vals);
    const partBuf = device.createBuffer({ size: groups * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const readBuf = device.createBuffer({ size: groups * 4, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const bind = device.createBindGroup({ layout: pipeline.getBindGroupLayout(0), entries: [
      { binding: 0, resource: { buffer: srcBuf } }, { binding: 1, resource: { buffer: partBuf } }]});
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(pipeline); pass.setBindGroup(0, bind); pass.dispatchWorkgroups(groups); pass.end();
    enc.copyBufferToBuffer(partBuf, 0, readBuf, 0, groups * 4);
    device.queue.submit([enc.finish()]);
    await readBuf.mapAsync(GPUMapMode.READ);
    const partials = new Float32Array(readBuf.getMappedRange().slice(0));
    readBuf.unmap();
    srcBuf.destroy(); partBuf.destroy(); readBuf.destroy();
    // The final combine, in f32, in index order — the shape a host does it.
    let total = Math.fround(0);
    for (let i = 0; i < partials.length; i++) total = Math.fround(total + partials[i]);
    return total;
  }

  const rows = [];
  for (const shape of ['uniform', 'mixed']) {
    for (const n of [65536, 1048576]) {
      const f64 = makeValues(n, shape);
      const f32 = Float32Array.from(f64);                     // the narrowing, before any GPU
      // f64 reference, compensated.
      let sum = 0, c = 0;
      for (const x of f64) { const y = x - c, t = sum + y; c = (t - sum) - y; sum = t; }
      const truth = sum;
      // f32 on the CPU, sequential — isolates the narrowing from the reordering.
      let cpu32 = Math.fround(0);
      for (let i = 0; i < n; i++) cpu32 = Math.fround(cpu32 + f32[i]);

      const results = [];
      for (const g of [16, 64, 256, 1024]) results.push({ g, v: await gpuSum(f32, g) });
      const vals = results.map(r => r.v);
      const lo = Math.min(...vals), hi = Math.max(...vals);
      rows.push({ shape, n, truth, cpu32, results, spread: hi - lo,
                  spreadRel: (hi - lo) / Math.abs(truth),
                  narrowRel: Math.abs(cpu32 - truth) / Math.abs(truth) });
    }
  }
  return { info: { vendor: info.vendor, arch: info.architecture }, hasF64: false, rows };
});

console.log(`adapter: ${out.info.vendor} ${out.info.arch}   (WGSL has no f64 type; this is f32)`);
console.log();
console.log('shape      terms     GPU spread   spread/|sum|   narrowing f64->f32   ratio');
for (const r of out.rows) {
  const ratio = r.spreadRel / r.narrowRel;
  console.log(
    `${r.shape.padEnd(9)} ${String(r.n).padStart(8)}   ${r.spread.toExponential(3).padStart(10)}` +
    `   ${r.spreadRel.toExponential(3).padStart(12)}   ${r.narrowRel.toExponential(3).padStart(18)}` +
    `   ${ratio.toFixed(3).padStart(6)}`);
  console.log('           workgroups: ' + r.results.map(x => `${x.g}:${x.v.toPrecision(9)}`).join('  '));
}
await browser.close();
