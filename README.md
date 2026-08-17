# Ferroscope

**The open interface layer for physical AI.** Pure Rust. Reads and writes plain
[MCAP](https://mcap.dev). Adds the two axes every robotics viewer is missing: **a joules lane**
and **a determinism receipt**.

[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

```
cargo install ferroscope-cli

ferroscope demo   a.mcap
ferroscope demo   b.mcap --platform "x86_64-linux / vulkan" --drift 320
ferroscope diff   a.mcap b.mcap
```

```
  digests differ, comparing step by step
  diverged at step 392, /robot/joints[2]: 3.88603112635621584e-1 vs 3.88603098094233157e-1
                                          (|Δ| 1.454e-8, rel 3.742e-8)
  → the runs agreed for 39.2 % of the trajectory, then did not.
```

That is the whole idea. Two machines ran the same declared experiment. One of them drifted.
The tool does not say "results differ". It says **which step, which channel, which index, and
both numbers**.

And look at the size of it: `1.454e-8`. In this demo the perturbation never explodes. It settles
into a persistent offset around 10⁻⁹ m that no plot can show and no eye can catch. Plotted against
each other, the two runs are the same picture. That is the whole reason the digest exists.

---

## Why this exists

Robotics tooling in 2026 is good and getting better. Four things are shipping:

| | what it does well | where it stops |
|---|---|---|
| **[Foxglove](https://foxglove.dev)** | The best panel-and-layout viewer in the field. MCAP is theirs and it is genuinely open (Apache-2.0). The SDK core is Rust, MIT. Live WebSocket streaming, teleop, a real data platform. | The app itself is proprietary: Studio 1.x (MPL-2.0) was frozen in February 2024 and the current product is closed. Cloud storage, seats, and device counts are metered. Visualization only: no physics, no scenario execution, no notion of whether a run reproduced. |
| **[Rerun](https://rerun.io)** | Open-source core, Rust viewer that runs native *and* in a browser, an entity-component data model with real timelines, and a good embedding story. | Its own MCAP support is marked experimental; the viewer is bounded by RAM. It is a logging and visualization layer, by design, not a place where a run is *executed*, gated, or certified. |
| **[NVIDIA Isaac Sim / Isaac Lab](https://developer.nvidia.com/isaac/sim)** | The strongest physics-and-rendering primitives available, GPU-parallel environments, OpenUSD throughout, an enormous asset ecosystem. | Apache-2.0 source that needs the Omniverse Kit SDK under NVIDIA's own license; redistributing it or offering it as a service to third parties pulls in NVIDIA AI Enterprise. Needs an RTX-class GPU. And Isaac Lab's own docs state the limitation plainly: GPU work scheduling reorders floating-point reductions, so *"experiments from the IsaacGym simulator are not perfectly reproducible on a different system."* |
| **[Antioch](https://www.antioch.com/)** | The newest and sharpest framing of the problem: define the whole robot stack as software, containerize it as one version-controlled artifact (Ark), spin up thousands of twins in the cloud, wire it into CI/CD, and lean on Omniverse/Cosmos for physics and Foxglove for observability. | A commercial cloud product on a subscription. This review did not locate public documentation of its wire format, its determinism guarantees, or a self-hostable path, so a team cannot verify a claim about a run without being a customer at the moment the claim is made. |

Put the columns side by side and the gap has a shape:

**Nothing in that list answers, from a file alone, either of the two questions that decide whether
a robot ships.**

1. *Did this run reproduce*, and if not, **where**?
2. *What did the task cost in joules*, compute **and** actuation, in one ledger?

Ferroscope answers both, offline, from the recording, with no account and no daemon.

---

## The three ideas

### 1. A physical-AI run has three clocks, not one

MCAP gives a message a `log_time` and a `publish_time`. Enough for a log; not enough for a robot.
A run has **simulated time**, **wall time**, and a **control step index**, and the interesting bugs
live in the drift between them. A controller that holds 1 kHz in simulation and 780 Hz on hardware
is not a controller that works, and on a single-clock timeline that failure is invisible.

Every Ferroscope message carries all three. Real-time factor and control-loop jitter read straight
off the recording:

```
  clocks      worst wall−sim drift 61.745 ms at sim t=0.999 s
```

### 2. Energy is a lane, not an afterthought

```
E_task = E_compute + E_actuation
```

Both terms come from **measured power integrated over the run**, never from a datasheet TDP.
`ferroscope energy` prints the split, and the split is the design decision:

```
  E_task = E_compute + E_actuation
  ---------------------------------------------
  compute             8.357 J    23.9 %
  actuation          25.182 J    72.1 %
  overhead            1.399 J     4.0 %
  ---------------------------------------------
  total              34.938 J
  peak               87.253 W   actuation/leg

  coverage     sound (3000 samples, median interval 1.0 ms)
```

Note the last line. Integrating power over time is trivial; integrating it *honestly* is not. A
series with a four-second hole in it under-reports by exactly whatever happened in the hole. So the
ledger carries a coverage verdict, and when the sampling cannot support the number it says so:

```
  coverage     DO NOT QUOTE: actuation/hip has a 4900 ms gap against a 10.0 ms median
               interval, so a transient in that hole is invisible to the integral
  → this number is reported but must not be cited as a measurement.
```

The number is still printed. A flagged measurement beats a missing one, as long as it is flagged.

### 3. The receipt is recomputable from the file

Every run is sealed with a receipt stored in the recording's own metadata. It has two halves:

- a **spec digest** over everything that must match for two runs to be *comparable at all*:
  scenario, seed, timestep, integrator, solver, asset digests, physics config, build. The
  **platform is deliberately excluded**, because comparing across platforms is the entire point.
- a **trace digest** over the trajectory at a **declared precision**. Bit-exact if you can afford
  it; otherwise mantissa-quantized, so a receipt says *"identical to 2⁻⁴⁰ relative"* instead of
  pretending to a bit-exactness no GPU fabric delivers.

Because the trace digest is defined over *what is in the file*, anyone holding the file can
recompute it, with no simulator, no source tree, and no access to the machine that produced it:

```
$ ferroscope verify a.mcap
  spec digest     4d46e750af663f1c684f275038d420c8a2cf19a26c6aa7dd473742954e7cb9cb  ok
  trace digest    8d5b26d74bf395c8d1f74a87cccbb0e12d2f53d6241743acb7212939294d1681  ok

  VERIFIED: this file still stands behind its own receipt.
```

And the rule that keeps it sound:

> **A digest match is proof. A digest mismatch is a question.**

Quantization boundaries alone can split two values a nanometre apart, so a mismatch never reports
as a divergence. It escalates to the comparator, which walks both traces and returns one of:
`BitExact`, `IdenticalAtPrecision`, `WithinTolerance` (with the worst case named), `Diverged` (with
the step named), `NonFinite`, or `Incomparable`.

A NaN outranks every tolerance question. A run that produced one is *reported*, never hashed into a
match.

---

## The viewer runs in your tab

```sh
./viewer/build.sh                              # rebuild the wasm (already committed)
python3 -m http.server 8080 --directory viewer  # a module import of .wasm needs http, not file
open http://localhost:8080
```

Live: **[ferroscope.physicalai-bmi.org/viewer](https://ferroscope.physicalai-bmi.org/viewer)**

Drop a recording into slot A to read it: topics with schemas and counts, an isometric pose trail
with contacts, a stacked power lane by rail, every scalar as its own lane, the wall-minus-sim drift,
and the receipt **with the trace digest recomputed from the bytes you just dropped**. Drop a second
recording into slot B and it tells you whether the replay reproduced the first, and if not, at which
step it stopped, plus the log-scale divergence lane.

183 KB of WebAssembly, one HTML file, no bundler, no worker, no upload, no account. **Turn your
network off and it still works.** That is the difference between an open interface layer and a
client for somebody's cloud, and it is only possible because the four libraries underneath are
`std`-only.

The same functions are callable from any page:

```js
import init, { open, diff, divergence_curve, verify_receipt, version }
  from './pkg/ferroscope_wasm.js';
await init();
const bundle  = JSON.parse(open(bytesA));            // lanes, energy, receipt, verify
const verdict = JSON.parse(diff(bytesA, bytesB, 0, 0));  // 0 = default 1e-9 tolerance
const curve   = JSON.parse(divergence_curve(bytesA, bytesB, '/robot/joints'));
```

---

## Install

```sh
cargo install ferroscope-cli      # installs the `ferroscope` binary
```

> The bare name `ferroscope` on crates.io belongs to an unrelated Rust debugger, published in
> July 2025. The libraries keep the namespace; the CLI crate carries the `-cli` suffix and the
> binary it installs is still called `ferroscope`.

```toml
# or the libraries
[dependencies]
ferroscope-schema = "0.1"         # recorder + well-known schemas (pulls the three below)
ferroscope-mcap    = "0.1"        # MCAP reader/writer, zero dependencies
ferroscope-ledger  = "0.1"        # the joules arithmetic
ferroscope-receipt = "0.1"        # digests and the comparator
```

## Record a run

```rust
use ferroscope_schema::{Recorder, Stamp, JointState};
use ferroscope_ledger::Rail;
use ferroscope_receipt::{RunSpec, Precision};

let mut rec = Recorder::new(std::fs::File::create("run.mcap")?, Precision::Quantized { drop_bits: 12 });

for step in 0..1000 {
    let t = Stamp::at(sim_ns, wall_ns, step);       // three clocks, always
    rec.transform("/robot/base", t, "world", "base", position, orientation)?;
    rec.joints("/robot/joints", t, &JointState { names, position, velocity, effort })?;
    rec.energy("/energy/leg", t, Rail::Actuation, "leg", watts)?;
    rec.energy("/energy/soc", t, Rail::Compute,  "soc", soc_watts)?;
    rec.scalar("/control/error", t, err, "m")?;
}

let spec = RunSpec::new("pick-and-place", 42)
    .dt_ns(1_000_000).steps(1000)
    .integrator("semi-implicit-euler").solver("pgs-30")
    .asset("panda.urdf", sha_of_the_urdf)
    .config("gravity_z", "-9.80665")
    .build(env!("CARGO_PKG_VERSION"));

let (file, receipt, energy) = rec.seal(spec, "aarch64-apple-darwin / Metal")?;
```

That is it. The receipt is written into the file. The energy ledger is closed. The recording is
plain MCAP, and it opens in Foxglove, in Rerun, or in `mcap cat` with no plugin, because every
channel carries a published JSON Schema. **Being readable by the incumbent is the price of asking
anyone to try a new one.**

## The CLI

```
ferroscope inspect <run.mcap>            topics, schemas, clocks, receipt
ferroscope verify  <run.mcap>            recompute the receipt from the file itself
ferroscope energy  <run.mcap>            E_task = E_compute + E_actuation
ferroscope diff    <a.mcap> <b.mcap>     did the replay reproduce the run
                   [--abs <f>] [--rel <f>]
ferroscope export  <run.mcap> <out.json> viewer bundle for the browser
ferroscope demo    <out.mcap>            write a synthetic run
                   [--seed <n>] [--steps <n>] [--drift <step>] [--platform <s>]
```

Exit codes are the point of the CLI existing:

| code | meaning |
|---|---|
| `0` | the answer is yes |
| `1` | the answer is **no**: verification failed, or the runs diverged |
| `2` | the tool could not answer: bad file, bad arguments |

A CI gate is one line:

```yaml
- run: ferroscope diff baseline.mcap $(pwd)/run.mcap --rel 1e-9
```

---

## Architecture

Five crates. **The four libraries have zero runtime dependencies**, `std` and nothing else, and all four
build for `wasm32-unknown-unknown` unchanged.

```
ferroscope-mcap      MCAP v0 reader + writer.       0 deps.  wasm-clean.
ferroscope-ledger    E_task arithmetic + coverage.  0 deps.  wasm-clean.
ferroscope-receipt   SHA-256, digests, comparator.  0 deps.  wasm-clean.
ferroscope-schema    Recorder, schemas, verify().   depends only on the three above.
ferroscope-cli       The CLI (binary: `ferroscope`).
ferroscope-wasm      Browser bindings.              + wasm-bindgen, the project's one dep.
```

### Why reimplement MCAP?

The reference `mcap` crate is good, and it is Foxglove's. It also links `zstd` and `lz4`, both of
them C libraries, for compression it carries whether or not a given file is compressed. That is
precisely what keeps robotics log tooling off the browser.

`ferroscope-mcap` writes **uncompressed chunks**, which is a conforming MCAP profile every reader
must accept, and stays `std`-only. Compression is a codec decision, not a format decision; if you
want the bytes smaller, the transport already compresses them. What you get in exchange is a
recording layer that runs in a tab.

And an implementation that only round-trips against itself proves nothing: two matching bugs read
as a pass. So the reference crate is a **dev-dependency**, and the test suite is an oracle:

- every file this writer produces is parsed by Foxglove's `mcap` crate: messages, chunk CRCs,
  summary, statistics, metadata;
- every file that crate produces is read back by this one;
- a byte flipped inside a chunk must surface as a CRC mismatch, not a short read;
- a truncated file must be named as truncated, not returned as a shorter run.

---

## Verification

Everything above is a test, and the tests are the negative cases:

| test | what would otherwise go unnoticed |
|---|---|
| `reference_reader_accepts_our_files` | a writer that is only self-consistent |
| `a_flipped_byte_inside_a_chunk_is_caught` | silent corruption |
| `truncation_is_named_not_guessed` | half a run read as a whole one |
| `re_encoding_the_file_with_one_number_changed_fails_the_trace_digest` | a **structurally perfect** file, correct CRCs, one value nudged by 1 part in 10⁶ |
| `editing_the_receipt_metadata_is_caught` | a receipt edited after the run |
| `platform_is_not_part_of_the_spec_digest` | cross-platform reproduction becoming unstateable |
| `a_field_cannot_be_smuggled_across_a_boundary` | `"ab"+"c"` hashing like `"a"+"bc"` |
| `nan_never_hashes_into_a_match_silently` | a diverged run passing because both sides went NaN |
| `a_hole_in_the_telemetry_is_refused_not_smoothed` | an energy number quoted from gappy sampling |
| `viability_cost_is_undefined_without_a_success` | a policy that never succeeds looking merely expensive |
| `a_recording_opens_in_a_viewer_that_never_heard_of_ferroscope` | schema drift breaking third-party viewers |

```sh
cargo test                                        # all of it
cargo build --target wasm32-unknown-unknown       # the four libraries, unchanged
```

---

## Status

**Real, tested, and shipping in 0.1:** the MCAP reader and writer with the reference-oracle suite,
the three-clock recording model, the well-known schemas, the energy ledger with its coverage
refusal, the determinism receipt and comparator, `verify` recomputing a receipt from bytes alone,
the five CLI verbs, and the in-browser viewer.

**Next, in the open, on the same repository:** live streaming over WebTransport, a scenario runner
that executes a spec rather than only describing one, and coupling to
[Ferromotion](https://crates.io/crates/ferromotion) so a run can be produced and certified by the
same stack that renders it.

Nothing here is feature-gated, and nothing here will be.

---

## License

MIT **OR** Apache-2.0, at your option. Free to use, study, fork, and build on, which is the point.

Part of the open ecosystem from the [Institute for Physical AI @ BMI](https://physicalai-bmi.org)
alongside [Ferric](https://ferric.physicalai-bmi.org) (compute),
[Ferralloy](https://ferralloy.physicalai-bmi.org) (fleet), and Ferromotion (kinematics and dynamics).
