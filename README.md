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
| **[Antioch](https://www.antioch.com/)** | The best-designed scenario model in the field, and the reason this repository has one. A scenario is a parameterized 3-D integration test; cases and grids turn it into many comparable runs; a verdict is named checks with measured details; suites are unions of selector clauses; history is queryable with `key:op:value` predicates; telemetry lands in Rerun. Every one of those ideas is worth porting, and this repository ports them. | The delivery, not the design. Its own documentation is the source: a run needs an ephemeral GPU VM, *"allocation is the slow step"*, and when none is warm the CLI *"polls up to 600 s"*. Simulator imports are banned at module scope because discovery must happen *"before requesting a machine"*. Cost is assignment-scoped, *"idle time included … there is no per-run or per-scenario cost figure to report"*. Reproduction means re-queueing saved images, and *"multi-machine interactive runs are not currently rerunnable"*: there is no digest and no divergence step. And *"the CLI has no `compare` command"*. |

Put the columns side by side and the gap has a shape:

**Nothing in that list answers, from a file alone, either of the two questions that decide whether
a robot ships.**

1. *Did this run reproduce*, and if not, **where**?
2. *What did the task cost in joules*, compute **and** actuation, in one ledger?

Ferroscope answers both, offline, from the recording, with no account and no daemon. The second one
is not a guess about a competitor: Antioch's documentation says in as many words that the platform
has no per-run cost figure to report.

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

## Scenarios, cases, suites, verdicts

`ferroscope-run` is the harness. It keeps Antioch's model and throws away everything between the
engineer and the answer: no manifest, no `services` map, no engine image, no container, no machine
to wait for. A scenario is a function in your binary.

```rust
use ferroscope_run::prelude::*;
use ferroscope_run::Case;

fn main() -> std::process::ExitCode {
    Harness::new()
        .scenario(
            Scenario::new("hop")
                .describe("One leg, one hop. Did it leave the ground and land on its feet?")
                .tags(["locomotion", "smoke"])
                .param("stiffness", 8000.0)
                .param("restitution", 0.4)
                .steps(1_000)
                .dt(1.0 / 1000.0)
                .cases(Case::sweep("stiffness", [4000.0, 8000.0, 16000.0]))
                .body(hop),
        )
        .suite(Suite::new("smoke").tags(["smoke"]))
        .main()
}

fn hop(run: &mut Run) -> Result<(), Halt> {
    let k = run.param("stiffness");
    let (mut z, mut vz) = (0.32, 1.5);
    let mut peak = z;
    while run.running() {
        let t = run.tick();                       // <- the only bookkeeping call in the loop
        let f = (k * (0.32 - z).max(0.0)).max(0.0);
        vz += (-9.80665 + f / 12.0) * run.dt();
        z += vz * run.dt();
        peak = peak.max(z);
        run.position("/robot/base", t, [0.0, 0.0, z]);
        run.energy("/energy/leg", t, Rail::Actuation, "leg", (f * vz).abs() * 0.45);
        run.energy("/energy/soc", t, Rail::Compute, "soc", 7.8);
    }
    run.result("peak_height_m", peak);
    run.result("gate_peak_m", 0.36);              // record what "passed" meant
    run.check("left the ground", peak > 0.36, format!("peak {peak:.4} m > 0.360 m"));
    Ok(())
}
```

`run.tick()` is the whole trick. It advances simulated time by the declared `dt`, reads the wall
clock, and carries the control-step index, so the **three clocks**, the **energy ledger**, the
**trace digest** and the **verdict** all advance together. You write the physics; the plumbing is
not yours to write.

Your binary now has the surface:

```text
collect  [--scenario S] [--tag T] [--exclude-tag T] [--case C] [--json]
run      [--suite NAME] [--scenario S] [--tag T] [--case C] [--set k=v] [--json]
suites   [--json]
list     [--outcome O] [-q TEXT] [--param k:op:v] [--result k:op:v] [--limit N] [--json]
show     RUN_ID [--json]
compare  RUN_A RUN_B [--abs F] [--rel F]
```

```text
$ cargo run --release --example hopper -- run --suite acceptance
  hop[stiffness=4000]                      failed   3 check(s), 1 failed     5.0 ms    28.70 J
      x leg did not bottom out: worst penetration 0.0886 m <= 0.0600 m
  hop[stiffness=8000]                      passed   3 check(s)               4.6 ms    26.28 J
  hop[stiffness=16000]                     passed   3 check(s)               4.7 ms    24.29 J
  …
13 run(s), 8 passed, 5 not, 338.32 J total, in 197 ms
```

A softer leg bottoms out; a bouncier one costs more joules. That is a design trade-off the harness
surfaced in a fifth of a second, with a receipt per run.

### Measured

On an M-series laptop, release build, one process, no GPU, no container, no network:

| | median |
|---|---|
| process start, discover 13 cases, print (`collect`) | **1.7 ms** |
| `run --suite smoke`: 4 runs, 4,000 physics steps, 4 sealed and re-verified recordings | **39 ms** |
| `run --suite acceptance`: 13 runs, 13,000 steps, 13 recordings | **132 ms** |
| `list` over a 105-run history | **3.7 ms** |

That is about 10 ms per run, and each run writes a ~740 KB MCAP file, seals a SHA-256 receipt into
it, and then **recomputes that receipt from the bytes it just wrote** before recording the verdict.

The honest framing: this is a local harness with no GPU and no Isaac, so it is not doing the same
work as a cloud platform booting Kit on an RTX machine, and these numbers are not a benchmark
against one. What they are is the overhead *around* the physics (declaration, dispatch, recording, sealing,
verification, history) and that overhead is milliseconds rather than a machine allocation. Antioch's documented allocation ceiling alone, before any build or boot, is 600
seconds.

### What is deliberately not here

No GPU orchestration, no queue, no fan-out across machines, no Isaac, no renderer, no asset
catalog, no organization. If the job needs a photorealistic RTX sensor sim on twenty machines,
Antioch and Isaac are the right tools and this is not trying to be them. What this is: the layer
that says what a run *was*, what it *decided*, what it *cost*, and whether it *reproduced*, in one
file, on your machine, in milliseconds.

---

## Point it at your robot

```sh
ferroscope urdf my_robot.urdf run.mcap --steps 400
```

That reads *your* description, declares one drawable per `<visual>`, sweeps every movable joint
through its declared limits, runs forward kinematics per step, and writes a recording with a
receipt. Then open it in the viewer and your robot is there, because the file says what your robot
is.

```text
$ ferroscope urdf examples/robots/arm.urdf arm.mcap --steps 400
  robot        bench_arm
  root link    base
  links        5 (7 visuals)
  joints       4 total, 4 movable

wrote arm.mcap (854445 bytes)
  spec digest  f9ecf5286aabbd…
  energy       14.48 J estimated (21.5 % compute)
```

`ferroscope-urdf` is its own crate and has **no dependencies beyond Ferroscope**, including no XML
library: the dialect URDF uses is elements, attributes and comments, and a robot description is not
worth an XML stack. Boxes, cylinders, spheres and meshes; fixed-axis `rpy` origins; fixed, revolute,
continuous and prismatic joints; `<material><color>` for colour, with a palette per link when a
material is absent.

It is strict where being loose would cost you later. **Joint limits are enforced, not advisory.**
A broken kinematic tree is named rather than half-drawn: a joint pointing at an undeclared link
says which link, two roots say which links, and a cycle says so. A joint type it does not model
(`floating`, `planar`) is held fixed and **reported in `notes`** rather than dropped. A `<mesh>`
keeps its `filename` so an attachment under that name draws it, and the CLI tells you when a mesh
is referenced and not attached, instead of leaving a hole in the scene for you to find.

```rust
let robot = Robot::parse(&std::fs::read_to_string("arm.urdf")?)?;
robot.declare(&mut rec, t0, "/scene")?;                 // visuals, once
for step in 0..steps {
    let t = Stamp::sim(step * dt, step);
    robot.log_pose(&mut rec, t, &joint_positions, "/scene")?;   // link transforms, per step
}
```

---

## The viewer: WebGPU 3D in a tab

Live: **[ferroscope.physicalai-bmi.org/viewer](https://ferroscope.physicalai-bmi.org/viewer)**

```sh
./viewer/build.sh                               # rebuild the wasm (already committed)
python3 -m http.server 8080 --directory viewer  # a module import of .wasm needs http, not file
open http://localhost:8080
```

A dockable workspace with a **three.js WebGPU** viewport, and it says which backend is live rather
than leaving you to guess (WebGPU where the browser has it, WebGL2 where it does not):

- **3D**: orbit, pan and zoom with OrbitControls; a z-up ground grid and world axes, because
  robotics is z-up and a viewer that disagrees with the data fights you; PBR materials, one
  shadow-casting sun and an environment map, because metalness with nothing to reflect renders
  black and real robot glTFs are frequently metallic; **ghosts**, the same machine at earlier
  instants, drawn rather than scrubbed for; the **path travelled** as a Line2 ribbon; contact
  markers sized by force, instanced so a thousand of them cost one draw call; and a **measure
  tool**.
- **Scene tree**: every declared part with its shape, whether it moves, its colour swatch, and a
  checkbox. Every channel with its schema and message count.
- **Inspector**: the comparison verdict, the receipt with the trace digest **recomputed from the
  bytes you just dropped**, the joule ledger, and every pose and scalar at the current instant.
- **Plots**: power stacked by rail, each scalar as a lane, wall-minus-sim drift, and the
  log-scale divergence lane when a second run is loaded. All on one playhead.
- **Timeline**: scrub, play, and a speed selector.

The 3-D panel draws **what the recording declares**, through the `ferroscope.Geometry` schema:
boxes, spheres, cylinders, planes, polylines and **glTF meshes**, attached to frames, with size and
colour on the per-step track. So a leg whose length *is* its compression, and a part that turns red
on contact, are things the file says rather than things the viewer guesses. A recording with no
geometry still opens; it just has nothing to draw.

**Meshes travel inside the recording.** `Shape::Mesh` names an MCAP **attachment**, and the viewer
pulls the glTF straight out of the file it already opened:

```rust
rec.attach("arm.glb", "model/gltf-binary", &glb_bytes, t)?;
rec.geometry("/scene/arm", t, &Geometry::mesh("/robot/link3", "arm", "arm.glb", [1.0; 3]))?;
```

Nothing outside the file is referenced, which is the point: a viewer that fetches a robot's meshes
from somewhere else stops working the moment somewhere else moves, and a recording whose geometry
lives in a sibling directory is not evidence. Attachments are checked against their own CRC on
read, and Foxglove's reference reader finds them through the summary index like any other MCAP.

**Measure** (`m`, or the toolbar button) raycasts against the drawn geometry: click two points and
get the distance and the per-axis deltas. It measures what you can see, including a glTF the viewer
has never been told anything about.

Drop a file on **A** to read it, a second on **B** to compare. No bundler, no worker, no upload,
no account. **Turn your network off and it still works**: three.js is vendored into the repository rather
than pulled from a CDN, for exactly that reason.

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
cargo install ferroscope-cli      # installs a binary called `ferroscope`
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
ferroscope-run     = "0.1"        # scenarios, cases, suites, verdicts, local history
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
ferroscope urdf    <robot.urdf> <out.mcap>   record YOUR robot from its description
                   [--steps <n>] [--rate <hz>]
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

Five crates. **The four core libraries have zero runtime dependencies**, `std` and nothing else, and all four
build for `wasm32-unknown-unknown` unchanged. `ferroscope-run` depends only on those four and is
native by design: a harness that writes run history to a directory is not a browser component.

```
ferroscope-mcap      MCAP v0 reader + writer.       0 deps.  wasm-clean.
ferroscope-ledger    E_task arithmetic + coverage.  0 deps.  wasm-clean.
ferroscope-receipt   SHA-256, digests, comparator.  0 deps.  wasm-clean.
ferroscope-schema    Recorder, schemas, verify().   depends only on the three above.
ferroscope-run       Scenarios, cases, suites,      native only: it reads clocks and
                     verdicts, local history.       writes files, and says so.
ferroscope-urdf      URDF to scene, plus FK.        0 external deps, wasm-clean.
ferroscope-cli       The CLI (binary: `ferroscope`).
ferroscope-wasm      Browser bindings.              + wasm-bindgen.
viewer/              The WebGPU workspace.          + three.js, vendored, not a CDN.
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
| `one_topic_cannot_carry_two_schemas` | a channel carrying payloads a reader decodes with the wrong shape. The URDF exporter did exactly this, writing transforms onto geometry channels, and nothing stopped it |
| `a_broken_tree_is_named_rather_than_half_drawn` | a URDF whose joints point at links that do not exist, or that has two roots |
| `joint_limits_are_enforced_rather_than_advisory` | a commanded angle past a limit quietly being obeyed |
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
the CLI's seven verbs, the in-browser viewer, the scenario harness, and URDF import.

**Next, in the open, on the same repository:** live streaming over WebTransport, collision geometry and inertial frames from URDF, a scenario runner
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
