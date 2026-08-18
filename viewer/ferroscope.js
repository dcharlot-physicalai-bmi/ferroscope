/**
 * The Ferroscope browser SDK.
 *
 * Describe a scene, get a recording. The endpoint runs the same Rust the CLI and the MCP server
 * run, compiled to wasm at the edge, so a scene authored here and one authored on a laptop
 * produce the same bytes and the same receipt.
 *
 *   import { record, validate, schema } from 'https://physicalai-bmi.org/assets/ferroscope/ferroscope.js';
 *
 *   const run = await record({
 *     name: 'a crate beside a sweeping arm',
 *     duration_s: 3, rate_hz: 100,
 *     bodies: [{ id: 'crate', shape: 'box', size: [0.3, 0.3, 0.3],
 *                motion: { kind: 'fall', from: [0.6, 0, 1.8] } }],
 *     robots: [{ id: 'arm', urdf: 'so101' }],
 *   });
 *   run.receipt.traceDigest;  // recomputable from run.bytes alone
 *   run.joules;               // E_task, estimated
 *
 * No key, no account, and CORS is open, so this works from any page.
 */

const DEFAULT_BASE = 'https://physicalai-bmi.org';

/** A scene was refused. `problems` names the JSON path of each mistake and what would be right. */
export class SceneError extends Error {
  constructor(message, problems = [], status = 0) {
    super(message);
    this.name = 'SceneError';
    this.problems = problems;
    this.status = status;
  }
  /** Every problem on its own line, ready to hand back to whatever wrote the scene. */
  toString() {
    if (!this.problems.length) return `${this.name}: ${this.message}`;
    return `${this.name}: ${this.message}\n` +
      this.problems.map((p) => `  ${p.path}: ${p.message}`).join('\n');
  }
}

async function refuse(res) {
  let body = {};
  try { body = await res.json(); } catch { /* a non-JSON error body is still an error */ }
  throw new SceneError(
    body.error || `HTTP ${res.status}`,
    body.problems || (body.detail ? [{ path: '$', message: body.detail }] : []),
    res.status,
  );
}

/** The scene format: every field, its default, and a worked example. */
export async function schema({ base = DEFAULT_BASE } = {}) {
  const res = await fetch(`${base}/api/scene/schema`);
  if (!res.ok) return refuse(res);
  return res.json();
}

/** Check a scene without recording it. Throws {@link SceneError} listing every problem at once. */
export async function validate(scene, { base = DEFAULT_BASE } = {}) {
  const res = await fetch(`${base}/api/scene/validate`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: typeof scene === 'string' ? scene : JSON.stringify(scene),
  });
  if (!res.ok) return refuse(res);
  return res.json();
}

/**
 * Record a scene.
 *
 * Returns the MCAP bytes together with the receipt, the joules and the lowest point anything
 * reached — all of which the endpoint puts in response headers, so none of it costs a second
 * request or a parse of the file you were just handed.
 */
export async function record(scene, { base = DEFAULT_BASE } = {}) {
  const res = await fetch(`${base}/api/scene/record`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: typeof scene === 'string' ? scene : JSON.stringify(scene),
  });
  if (!res.ok) return refuse(res);
  const h = res.headers;
  const lowest = h.get('x-ferroscope-lowest-point');
  const notes = h.get('x-ferroscope-notes');
  return {
    bytes: new Uint8Array(await res.arrayBuffer()),
    steps: Number(h.get('x-ferroscope-steps')),
    joules: Number(h.get('x-ferroscope-joules')),
    computeFraction: Number(h.get('x-ferroscope-compute-fraction')),
    receipt: {
      specDigest: h.get('x-ferroscope-spec-digest'),
      traceDigest: h.get('x-ferroscope-trace-digest'),
    },
    // "-0.2057 arm/moving_jaw" -> { z, what }. Below zero means it went through the ground.
    lowest: lowest ? { z: Number(lowest.split(' ')[0]), what: lowest.split(' ').slice(1).join(' ') } : null,
    notes: notes ? notes.split(' | ') : [],
  };
}

/** Hand the recording to the browser as a download. */
export function save(run, filename = 'scene.mcap') {
  const url = URL.createObjectURL(new Blob([run.bytes], { type: 'application/octet-stream' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  // Revoke on the next turn: revoking synchronously races the click on some browsers.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
