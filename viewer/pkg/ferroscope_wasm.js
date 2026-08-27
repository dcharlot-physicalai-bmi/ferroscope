/**
 * Open a recording the browser cannot hold, a block at a time.
 *
 * [`open`] takes the whole file as one `Uint8Array`, which is what a browser hands over and is
 * fine up to a point. Measured in Chrome, that point is somewhere past 1.2 GB and short of
 * 2.6 GB: `file.arrayBuffer()` simply refuses, and no amount of care on this side changes it.
 *
 * So this is the other door. `File.slice(a, b).arrayBuffer()` returns one block at a time and
 * no single allocation is ever larger than a block, so the ceiling is the recording's own
 * lanes rather than its bytes. The fold underneath is the same one `ferroscope export` runs,
 * which is what keeps a bundle made in the browser and a bundle made by the CLI the same
 * bundle rather than two answers that resemble each other.
 *
 * Two passes over the file, because a lane's stride comes from its message count and the
 * receipt that says at what precision to recompute the digest is written at the END of the
 * recording. Push the file through, call [`rewind`](BundleStream::rewind), push it through
 * again, then [`finish`](BundleStream::finish).
 *
 * ```js
 * const s = new BundleStream();
 * for (let p = 0; p < 2; p++) {
 *   for (let at = 0; at < file.size; at += BLOCK) {
 *     const b = new Uint8Array(await file.slice(at, at + BLOCK).arrayBuffer());
 *     if (!s.push(b)) break;              // this pass has what it needs
 *   }
 *   if (p === 0) s.rewind();
 * }
 * const bundle = JSON.parse(s.finish());
 * ```
 *
 * What a streamed recording cannot do is what a bundle cannot do: comparison and attachment
 * extraction both need the bytes themselves, and a page that streamed the file no longer has
 * them. It costs a second read of the file, which for a recording this size is the cheaper
 * half of the bargain.
 */
export class BundleStream {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        BundleStreamFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_bundlestream_free(ptr, 0);
    }
    /**
     * How many pushed bytes are held because they are not yet a whole record. One record's
     * worth, however long the recording is.
     * @returns {number}
     */
    buffered() {
        const ret = wasm.bundlestream_buffered(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * How many bytes this pass has taken, so a page can draw a progress bar that is measuring
     * the work rather than guessing at it.
     * @returns {number}
     */
    fed() {
        const ret = wasm.bundlestream_fed(this.__wbg_ptr);
        return ret;
    }
    /**
     * Emit the viewer bundle as JSON. Consumes the stream.
     * @returns {string}
     */
    finish() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ptr = this.__destroy_into_raw();
            const ret = wasm.bundlestream_finish(ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * How many lane points are being held — what the page will actually draw. Bounded by the
     * screen, not by the recording, which is the whole reason this door exists.
     * @returns {number}
     */
    kept_points() {
        const ret = wasm.bundlestream_kept_points(this.__wbg_ptr);
        return ret >>> 0;
    }
    constructor() {
        const ret = wasm.bundlestream_new();
        this.__wbg_ptr = ret;
        BundleStreamFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Which pass is being fed: 1 before [`rewind`](BundleStream::rewind), 2 after.
     * @returns {number}
     */
    pass() {
        const ret = wasm.bundlestream_pass(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Add the next block of the recording, in file order. Blocks may be any size and need not
     * fall on record boundaries.
     *
     * Returns `false` once this pass has everything it needs — the footer has been reached, or
     * the bytes did not parse. Stop reading when it does; pushing after that does nothing.
     * @param {Uint8Array} block
     * @returns {boolean}
     */
    push(block) {
        const ptr0 = passArray8ToWasm0(block, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.bundlestream_push(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * End pass one and begin pass two. Push the same bytes again from the start.
     */
    rewind() {
        wasm.bundlestream_rewind(this.__wbg_ptr);
    }
}
if (Symbol.dispose) BundleStream.prototype[Symbol.dispose] = BundleStream.prototype.free;

/**
 * The raw bytes of one attachment, by name.
 *
 * A glTF has no business being base64'd into a JSON document, so the viewer asks for the blob
 * directly and hands it to a loader.
 * @param {Uint8Array} bytes
 * @param {string} name
 * @returns {Uint8Array}
 */
export function attachment(bytes, name) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.attachment(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * Every channel name in a recording, for a viewer that wants to offer a choice.
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function channels(bytes) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.channels(ptr0, len0);
        var ptr2 = ret[0];
        var len2 = ret[1];
        if (ret[3]) {
            ptr2 = 0; len2 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Compare two recordings. This is the function that has no equivalent anywhere: drop two
 * `.mcap` files onto a page with the network off and find out whether the second run
 * reproduced the first, and if it did not, at which step it stopped.
 *
 * `abs` and `rel` are the tolerances; pass `0` for either to use the default `1e-9`.
 * @param {Uint8Array} a
 * @param {Uint8Array} b
 * @param {number} abs
 * @param {number} rel
 * @returns {string}
 */
export function diff(a, b, abs, rel) {
    let deferred4_0;
    let deferred4_1;
    try {
        const ptr0 = passArray8ToWasm0(a, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(b, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.diff(ptr0, len0, ptr1, len1, abs, rel);
        var ptr3 = ret[0];
        var len3 = ret[1];
        if (ret[3]) {
            ptr3 = 0; len3 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred4_0 = ptr3;
        deferred4_1 = len3;
        return getStringFromWasm0(ptr3, len3);
    } finally {
        wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
    }
}

/**
 * Compute the per-step divergence curve between two recordings on one channel, so a viewer
 * can draw the lane that makes the point: flat zero, then never zero again.
 *
 * Returns a JSON array of `[step, |Δ|]`, taking the largest absolute difference across the
 * channel's values at each step.
 * @param {Uint8Array} a
 * @param {Uint8Array} b
 * @param {string} channel
 * @returns {string}
 */
export function divergence_curve(a, b, channel) {
    let deferred5_0;
    let deferred5_1;
    try {
        const ptr0 = passArray8ToWasm0(a, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(b, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(channel, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.divergence_curve(ptr0, len0, ptr1, len1, ptr2, len2);
        var ptr4 = ret[0];
        var len4 = ret[1];
        if (ret[3]) {
            ptr4 = 0; len4 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred5_0 = ptr4;
        deferred5_1 = len4;
        return getStringFromWasm0(ptr4, len4);
    } finally {
        wasm.__wbindgen_free(deferred5_0, deferred5_1, 1);
    }
}

/**
 * Open a recording. Returns the viewer bundle as a JSON string: topics, pose and scalar
 * lanes, power by rail, contacts, the clock drift lane, the closed energy ledger, and the
 * receipt **with the trace digest recomputed from these very bytes**.
 *
 * Throws a JS error naming what went wrong rather than returning a half-parsed run.
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function open(bytes) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.open(ptr0, len0);
        var ptr2 = ret[0];
        var len2 = ret[1];
        if (ret[3]) {
            ptr2 = 0; len2 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Open a growing live prefix of a recording — the bytes a WebSocket has delivered so far.
 *
 * Same bundle as [`open`], with no closing magic required and no receipt expected yet: the
 * moment the producer seals, the same buffer is a complete file and [`open`] takes over,
 * receipt and all.
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function open_prefix(bytes) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.open_prefix(ptr0, len0);
        var ptr2 = ret[0];
        var len2 = ret[1];
        if (ret[3]) {
            ptr2 = 0; len2 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Record one case of a scene's grid and hand back its MCAP bytes.
 * @param {string} scene_json
 * @param {number} index
 * @param {string} robot_name
 * @param {string} urdf
 * @returns {Uint8Array}
 */
export function record_case(scene_json, index, robot_name, urdf) {
    const ptr0 = passStringToWasm0(scene_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(robot_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(urdf, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.record_case(ptr0, len0, index, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * Record a scene, in the tab, and hand back the MCAP bytes.
 *
 * The same crate the CLI and the edge endpoint run, so a scene authored here produces the same
 * bytes and the same receipt as one authored anywhere else.
 * @param {string} scene_json
 * @returns {Uint8Array}
 */
export function record_scene(scene_json) {
    const ptr0 = passStringToWasm0(scene_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.record_scene(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * Record a scene with one robot description supplied by the caller.
 *
 * The page fetches the URDF (it knows how to make requests; this does not) and passes the text
 * in. Any robot whose name does not match is skipped and noted in the recording.
 * @param {string} scene_json
 * @param {string} robot_name
 * @param {string} urdf
 * @returns {Uint8Array}
 */
export function record_scene_with(scene_json, robot_name, urdf) {
    const ptr0 = passStringToWasm0(scene_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(robot_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(urdf, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.record_scene_with(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * Read an English phrase into a scene, in the tab.
 *
 * Returns `{ scene, understood[], assumed[], ignored[] }`, or an object with `error` and `hint`
 * when the phrase names nothing recordable. Deterministic and offline: no request leaves the
 * page, which is the same promise the viewer already makes about the files you open in it.
 * @param {string} text
 * @returns {string}
 */
export function scene_from_text(text) {
    let deferred2_0;
    let deferred2_1;
    try {
        const ptr0 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.scene_from_text(ptr0, len0);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Run a scene's cases and return the verdict table, without the recordings.
 *
 * Returns `{ name, cases: [{ label, steps, joules, passed, checks: [{name, ok, why}] }] }`, or
 * `{ error, problems }`. The bytes are left out on purpose: a grid of 256 cases is a lot of
 * memory to hand a page that will look at one of them, so the caller records the case it wants
 * with [`record_case`].
 * @param {string} scene_json
 * @param {string} robot_name
 * @param {string} urdf
 * @returns {string}
 */
export function sweep_scene(scene_json, robot_name, urdf) {
    let deferred4_0;
    let deferred4_1;
    try {
        const ptr0 = passStringToWasm0(scene_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(robot_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(urdf, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.sweep_scene(ptr0, len0, ptr1, len1, ptr2, len2);
        deferred4_0 = ret[0];
        deferred4_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
    }
}

/**
 * Verify one recording against its own receipt. Returns JSON:
 * `{ verified, spec_matches, trace_matches, stored, recomputed, messages, non_finite }`.
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function verify_receipt(bytes) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.verify_receipt(ptr0, len0);
        var ptr2 = ret[0];
        var len2 = ret[1];
        if (ret[3]) {
            ptr2 = 0; len2 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * The crate version, so a page can report which build produced what it is showing.
 * @returns {string}
 */
export function version() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.version();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./ferroscope_wasm_bg.js": import0,
    };
}

const BundleStreamFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_bundlestream_free(ptr, 1));

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('ferroscope_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
