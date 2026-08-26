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
A  a.mcap
   3bfe5cc7d194f408  on aarch64-macos  VERIFIED (receipt recomputed from the file)
B  b.mcap
   3bfe5cc7d194f408  on x86_64-linux / vulkan  VERIFIED (receipt recomputed from the file)

  diverged at step 392, /robot/joints[5]: 1.11677862853056808e0 vs 1.11689030639342102e0
                                          (|Δ| 1.117e-4, rel 9.999e-5)
  that is       /robot/joints effort[knee]

  onset        step 392 on /energy/actuation/leg — where the bits first parted
  crossing     step 392 — where a value first exceeded abs 1.0e-9 / rel 1.0e-9
               (this step moves when the tolerance moves; the onset does not)
  shape        growing: the difference against the channel's scale ends 2.795e1x where it
               started, e-folding about every 783 steps — sensitivity, so no tolerance
               makes this reproducible
  extent       6 channel(s) differ, ranked by |Δ| against each channel's own scale — they are
               not independent, so this is a ranking and not a count of separate faults:
    /energy/actuation/leg  Δ/scale 1.661e-6 at step 2979   watts          0.020079697055 vs 0.020022165866
    /robot/contacts        Δ/scale 9.115e-7 at step 392    force_n        0.057866189648 vs 0.057700417129
    /robot/joints          Δ/scale 9.115e-7 at step 392    velocity[hip] -0.000039893826 vs -0.000039767935
    /control/height_error  Δ/scale 4.185e-7 at step 2917   value         -0.000000066768 vs -0.000000068747

  energy       A 109.700 J    B 109.700 J    Δ -0.000000 J
               total -3.587e-7 J (below 0.01 %)   compute identical   actuation -3.587e-7 J
```

That is the whole idea. Two machines ran the same declared experiment. One of them drifted.
The tool does not say "results differ". It says **which step, which channel, which quantity by
name, both numbers, how the difference behaved over time, and what each run cost in joules**.

The browser viewer shows the same profile, from the same Rust compiled to wasm — onset, shape,
the ranked channels, the structural differences and the joules delta, on a page that uploads
nothing. Both surfaces re-verify each file before comparing: the page had the identical hole
the CLI did, and told a reader that a file carrying another run's edited digest was "identical
at quantized", on a green strip.

One column in that table is the whole reason it is trustworthy. Channels are ranked by |Δ|
against **each channel's own scale**, not by the pointwise relative difference — because a
quantity passing through zero has a meaningless relative error, and ranking on it puts a
signal's own zero crossings above the real finding. Ranked the naive way, this very pair led
with `/control/height_error` at "rel 2.9e-2", which was the controller error crossing zero;
the perturbation the demo actually injects is on the contact force, and it now ranks where it
belongs. The same denominator fixes the `shape` line, which is summarised by each window's
envelope rather than its median: a leg's power is zero through every flight phase, and a median
read zero straight through a live divergence and called it a transient.

Four of those lines exist because a design review took the old output apart. It used to name
the first value in *recorder emit order* that crossed the threshold — a hip velocity at
rel 3.7e-8 — while the injected perturbation, four thousand times larger, sat two slots away in
the same message. It reported one step where there are two different ones worth knowing: the
**onset**, where the bits first parted, which is causal, and the **crossing**, which moves
whenever you move the tolerance flag. And it printed "the runs agreed for 39.2 % of the
trajectory", a number that measures run length rather than agreement — the same injected fault
reports 60.3 %, 2.6 % and 0.4 % at 200, 3000 and 20000 steps. That line is gone.

And look at the size of it. The perturbation enters at `1.1e-4` on one joint effort and is still
under `3e-2` relative three thousand steps later — a difference no plot shows and no eye catches.
Plotted against each other, the two runs are the same picture. That is the whole reason the digest
exists, and the `shape` line is what tells you whether to raise the tolerance and move on or stop
claiming the scenario is reproducible at all.

---

## Why this exists

Robotics tooling in 2026 is good and getting better. A full survey of the field, across languages
and regions, is in [docs/LANDSCAPE.md](docs/LANDSCAPE.md). The short version, five tools:

| | what it does well | where it stops |
|---|---|---|
| **[Foxglove](https://foxglove.dev)** | The best panel-and-layout viewer in the field. MCAP is theirs and it is genuinely open (Apache-2.0). The SDK core is Rust, MIT. Live WebSocket streaming, teleop, a real data platform. | The app itself is proprietary: Studio 1.x (MPL-2.0) was frozen in February 2024 and the current product is closed. Cloud storage, seats, and device counts are metered. Visualization only: no physics, no scenario execution, no notion of whether a run reproduced. |
| **[Rerun](https://rerun.io)** | Open-source core, Rust viewer that runs native *and* in a browser, an entity-component data model with real timelines, and a good embedding story. | Its own MCAP support is marked experimental. It is a logging and visualization layer, by design, not a place where a run is *executed*, gated, or certified. (The "bounded by RAM" complaint that used to sit here has been struck: this viewer holds the whole recording in memory too. What it is bounded *at* is measured below.) |
| **[NVIDIA Isaac Sim / Isaac Lab](https://developer.nvidia.com/isaac/sim)** | The strongest physics-and-rendering primitives available, GPU-parallel environments, OpenUSD throughout, an enormous asset ecosystem. | Apache-2.0 source that needs the Omniverse Kit SDK under NVIDIA's own license; redistributing it or offering it as a service to third parties pulls in NVIDIA AI Enterprise. Needs an RTX-class GPU. And Isaac Lab's own docs state the limitation plainly: GPU work scheduling reorders floating-point reductions, so *"experiments from the IsaacGym simulator are not perfectly reproducible on a different system."* |
| **[Lichtblick](https://github.com/lichtblick-suite/lichtblick)** | An actively maintained community fork of Foxglove Studio, browser and desktop, preserving the open-core model. **The honest correction to the row above:** licence is not the differentiator here. | Like Foxglove, it is a viewer. No physics, no scenario execution, no determinism receipt, no energy ledger. |
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

## How big a recording, exactly

Every viewer in this field is bounded by memory, this one included. The number is rarely stated,
so here is this one's, measured rather than estimated — `ferroscope demo --steps N` on an M4, and
the same files dropped on the page in headless Chrome:

| recording | messages | `ferroscope verify` | peak RSS | browser viewer |
|---|---|---|---|---|
| 8 MB | 43 k | 0.08 s | 25 MB | opens in 1.3 s |
| 136 MB | 681 k | 1.4 s | 300 MB | opens in 4.1 s |
| 552 MB | 2.7 M | 6.8 s | 1.2 GB | opens in 13.3 s |
| 1.2 GB | 6.0 M | 15.4 s | 2.6 GB | opens in 43 s |
| 2.6 GB | 12.8 M | 65 s | 1.7 GB† | **refused** |

Native throughput is about **80 MB/s** and memory about **2.1× the file**, both linear — up to
roughly a gigabyte. The parser holds the decoded log and nothing streams, so past that the
machine starts fighting: † at 2.6 GB on 48 GB of RAM the throughput halves to 40 MB/s and peak
RSS comes in *below* the 1.2 GB file's, because the memory is being compressed rather than held.
Those two numbers are the measurement bending, not the program improving, and they are printed
here as measured because the first draft of this table carried an extrapolation — 32 s and
5.5 GB — that was wrong in both directions.

In the browser the ceiling is Chrome's, not ours: past roughly 2 GB the File API will not hand a
page one `ArrayBuffer`, and the read fails before any Ferroscope code runs.

**So hand the browser a bundle instead.** `ferroscope export` strides every lane down to what a
screen can actually draw, and the viewer opens the result:

```sh
ferroscope export huge.mcap bundle.json   # 2.6 GB in, 1.4 MB out, 76 s
```

That 2.6 GB run — 12.8 million messages, the one the page refuses — opens **as a bundle in a
tenth of a second**, with its scene tree, its lanes, its receipt and its 54.8 kJ ledger. The
reduction is about 1800:1 because a lane is capped at 4,000 points; nothing a 1500-second run
shows on a 1080-pixel plot needs more.

What a bundle cannot do, it says rather than pretending. It carries no raw bytes, so it cannot
be compared (`diff` recomputes both receipts from the recordings, which is the point) and it
carries no mesh attachments, so the 3-D view draws primitives and notes why. And its receipt is
labelled **"recomputed at export … checked by the CLI, not this page"**: a bundle inherits a
verification that happened elsewhere, and printing a bare "VERIFIED" over it would claim a check
this machine never performed — the same misplaced confidence the comparator had when it trusted
a stored digest.

What matters is what happens *at* the ceiling. It used to be nothing at all: the rejection was
unhandled, so the page sat silent — measured, three minutes of a tab that looks broken. It now
refuses in a tenth of a second and says why, how big the file was, and that the CLI will do it.
Above 64 MB the page also says `reading…` and `parsing…` rather than freezing mutely for the
tens of seconds the work honestly takes.

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

#### And now it actually measures

`ferroscope power` reads the machine's own counters — Linux RAPL through `/sys/class/powercap`,
macOS `powermetrics` — and integrates them over a real command:

```sh
ferroscope power                             # what can this machine tell me?
ferroscope power --out run.mcap -- cargo build --release
```

```text
power source
  Linux RAPL, 1 top-level domain(s): package-0

  E_compute    37.984 J  (measured, 20 samples)
  mean power   19.216 W
  coverage     sound (20 samples, median interval 105.1 ms)
```

Two traps are handled, because both produce a plausible wrong number rather than an error:

- **Nested domains double-count.** `intel-rapl:0` is the package and `intel-rapl:0:0` is the core
  *inside* it. Summing every directory under `powercap` counts those joules twice — silently,
  by 30–60 %, always in the flattering direction. Only top-level domains are summed.
- **The counters wrap**, on some parts every minute or two under load. A naive subtraction goes
  hugely negative, and clamping that to zero drops a whole interval. Each domain's declared
  `max_energy_range_uj` is used to unwrap.

And when the machine will not say, it says *that*:

```text
no power interface: powermetrics is installed but this process cannot run it (powermetrics must
be invoked as the superuser). macOS has no unprivileged power interface, so run under sudo to
measure. Reporting no measurement rather than zero joules.
```

That case is the common one, not the exception: since the mitigation for CVE-2020-8694, RAPL's
`energy_uj` is root-only on most distributions, so a meter that shrugs reports **0 J for a machine
drawing 90 W**. The command still runs, its exit status still propagates, and no joules figure is
printed at all.

`FERROSCOPE_POWERCAP` overrides the sysfs root, which is how CI exercises the measuring path on
every runner against a synthetic counter with a known right answer — a 20 W counter must read back
as 20 W. A code path that only runs on hardware nobody in the loop owns is a code path nobody has
run.

#### Every recording now says what it cost to make

Two different quantities share the word "compute", and conflating them would be a category error.
The compute rail *inside* a described scene models the **robot** — an embedded SoC drawing ~8 W
during the task — and stays clearly labelled an estimate. What the machine *producing* the file
spent is a separate, measurable fact, and every recording now carries it in its own
`ferroscope.production` metadata block:

```text
  production
    joules                 0.616237
    duration_s             0.0293
    source                 Linux RAPL, 1 top-level domain(s): package-0
    basis                  cumulative energy counter
```

When the machine will not say — no root, no counters — the block names the reason instead, and
never writes `joules: 0`. A sweep sums per-case deltas off the cumulative counter, and refuses to
print a total if any case went unmeasured, because a sum with a hole in it reads as smaller than
the truth.

The block lives **outside both digests**, deliberately: production cost varies run to run by
nature — the same scene on a busier machine costs more — so it can never be part of the
determinism claim. The receipt says *"this is the same experiment"*; the production block says
*"and here is what making this copy of it cost"*. CI holds that as an invariant: a measured run
and an unmeasured run of the same scene must agree digest for digest.

This is also, concretely, the figure the platform column above lacks: a **per-run production cost**,
measured, in the file, recomputable by nobody because it is not a claim — it is a receipt of spend.


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

### It checks the description before it draws it

```sh
ferroscope urdf my_robot.urdf out.mcap --check
```

Every CAD pipeline in the field writes URDF. This survey found none that reads one back and asks
whether it is *physically usable*. So this does, and exits 1 when it is not:

```text
  CHECKS
    FAIL no-collision           no_collision   1 visual(s), 0 collision(s): the renderer can draw
                                               this link and the physics engine cannot touch it
    FAIL no-inertial            no_inertial    a movable link with no <inertial>: engines
                                               substitute a default, and the default is not your robot
    FAIL zero-inertia           zero_inertia   mass = 2 kg with an all-zero inertia tensor
    FAIL triangle-inequality    impossible     I1 + I2 = 2.000000e-3 < I3 = 5.000000e-1: no mass
                                               distribution produces these principal moments
    FAIL not-positive-definite  indefinite     smallest principal moment -4.600000e-2 <= 0
    FAIL bad-mass               negative_mass  mass = -1 kg, which is not positive
```

Each of those is a real bug that has shipped in real robot descriptions, each produces a policy
that works in simulation and not on hardware, and each is checkable from the file in milliseconds.
The inertia checks are eigenvalues of the tensor: positive definiteness, and the triangle
inequality on the principal moments that every physically realisable rigid body satisfies.

It found nine defects in this repository's own example URDF the first time it ran.
`examples/robots/broken.urdf` carries one of each class and CI asserts every one is caught.

### A real arm, not only a toy one

`examples/robots/so101.urdf` is the **LeRobot SO-101**, the 5-DOF open-hardware arm with a gripper.
Its kinematics and inertials are verbatim from the published calibrated description
([TheRobotStudio SO-ARM100](https://github.com/TheRobotStudio/SO-ARM100) / LeRobot, Apache-2.0):
every joint origin, `rpy`, axis, limit, effort and velocity, and every link mass, centre of mass
and inertia tensor. What is *not* verbatim is the geometry — upstream ships STL meshes of the
printed parts, and this file substitutes boxes sized to the real link extents. So the arm moves
exactly like an SO-101 and does not look like one, which the file says at the top rather than
leaving you to discover it.

```sh
ferroscope urdf examples/robots/so101.urdf so101.mcap --steps 1440 --rate 240 --sweep each
```

That is the recording behind the **SO-101** button in the viewer, and it is the one that matters
for how this behaves at size: 4.7 MB, 23,071 messages, 8 links across 6 joints. In the browser the
WebAssembly parser takes it from bytes to a scene you can orbit in **118 ms**, and it plays back at
the display's refresh rate. No upload, no server, no decode step in JavaScript.

It checks clean, and CI asserts that it stays clean: the SO-101 is the closest thing here to a
third-party description, so if the checker ever starts failing a shipped commercial arm, that is
a bug in the checker and it surfaces there.

`--sweep` chooses how the description is exercised. `all` (the default) drives every joint at once,
which moves the whole tree but is also a knot — a kinematic sweep has no collision check, so a real
arm folds through its own base and through the floor. `each` drives **one joint at a time** out of
the home pose and back, which is what you want when the question is "does this joint go where the
file says": everything else holds still, so what you see moving is the joint being asked about.

The recording also carries **collision geometry** and **inertial properties** as their own layers,
translucent over the visuals, with a centre-of-mass marker and an inertia ellipsoid whose semi-axes
come from the principal moments. Toggle them in the viewer: seeing what the physics engine sees
next to what the renderer draws is where sim-to-real gaps hide.

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

## Watch it live — the stream is the file

```sh
ferroscope-motion out.mcap --serve       # then press  live  in the viewer
```

`ferroscope-live` streams a recording as it is written: a zero-dependency WebSocket server whose
every frame is **one MCAP record, in file order, starting with the magic**. A client that appends
what it receives holds, at every instant, a valid prefix of the recording — the viewer opens the
prefix live, message counts climbing, with the receipt honestly reading `none` — and the moment
the producer seals, the tab holds the **byte-identical complete file** and verifies its receipt
with the same code that verifies one read from disk. Live viewing and archived evidence are not
two formats; they are one format at two moments, and CI holds the two moments equal over a real
socket: `cmp` says identical, `verify` says VERIFIED.

A viewer that connects mid-run is caught up first — schemas and channels live at the front of the
stream, and a client that missed them could draw nothing. A viewer that stalls is dropped without
ceremony: a browser tab must never back-pressure a simulation.

Two transports, one invariant. The default is WebSocket — ~300 lines of `std`, SHA-1 included,
checked against the RFC 6455 handshake vector every browser checks against, zero dependencies.
**WebTransport (HTTP/3 over QUIC) is the `webtransport` feature**: each viewer gets one
unidirectional QUIC stream carrying the raw recording bytes in file order — no framing layer at
all — FIN when the recording seals. TLS is mandatory even on localhost, and the designed answer
for local tooling is a fresh self-signed certificate the browser accepts through
`serverCertificateHashes`; the producer prints the whole connection story as one clickable link:

```text
ferroscope-motion out.mcap --serve-wt
  webtransport https://127.0.0.1:4433
               https://ferroscope.physicalai-bmi.org/viewer?wt=https://127.0.0.1:4433&hash=9f2c…
```

The QUIC stack (wtransport, quinn, rustls) is real weight, which is why it is opt-in and the
default build of `ferroscope-live` keeps the zero-dependency claim CI measures it by. Two
QUIC-specific lessons are pinned in the code: UDP has no lingering socket, so a session must
outlive its own FIN until the peer closes or unacked packets die with the process; and a
`Notify::notify_waiters` wakes only *current* waiters, so the seal signal is a flag checked after
creating the notified future — the first version delivered every byte and no FIN, and every
stream died at the QUIC idle timeout.

One permission stands between the hosted viewer and a local producer, and it is the browser's to
grant, not ours to route around: Chrome's **Local Network Access** policy makes a public site ask
before reaching `localhost` (the block surfaces as `ERR_BLOCKED_BY_LOCAL_NETWORK_ACCESS_CHECKS`).
Allow it when prompted and the live link works; mixed-content rules are not the obstacle —
`ws://localhost` is exempt from those — and the viewer's error message says all of this instead
of shrugging. A viewer served from `localhost` itself needs no permission at all.

### Any file replays as a live stream

```sh
ferroscope live run.mcap            # WebSocket; waits for the first viewer, then plays
ferroscope live run.mcap --wt       # WebTransport, with the one-click link printed
```

Until this verb, only a producer mid-run could stream; a finished recording could only be
opened. `ferroscope live` serves an existing file over the same two transports, **whole records
in file order, paced by the file's own log clock** (`--rate 2` plays it double speed). The
invariant is untouched: the bytes a viewer accumulates are the bytes on disk, so at the seal it
holds the byte-identical file, receipt and all. It waits for the first viewer before starting
the clock — a replay exists to be seen, and a short file could otherwise stream out and exit
inside the window a browser needs to connect (`--no-wait` starts immediately).

Replay is also the honest reason recordings are chunked at 64 KiB rather than the writer's
1 MiB default: a chunk is one record, a record is the unit a stream frames and a replay paces,
and a megabyte batches half a run into a single burst. Pacing is still bounded by that
granularity, and the verb says so rather than promising otherwise — a recording whose messages
all fit one chunk has a single pacing instant and streams in one burst, and that is what it
prints.

Three failure modes here are worth naming, because an adversarial audit found all three in
shipped code and each is now held by its own gate:

- **A lagging QUIC viewer used to be handed a truncated file that looked sealed.** On QUIC,
  *dropping* a send stream is a graceful FIN — and FIN is exactly how this protocol says "the
  recording sealed, what you hold is the file". A session that gave up on a viewer 1024 records
  behind therefore delivered a structurally valid prefix wearing the seal's own signal, while
  producer and viewer both reported success. A session that falls behind now re-reads what it
  missed straight from history (the server holds every byte it ever sent, so recovery is exact),
  and any session that must be abandoned **resets** its stream so a partial transfer can never
  be mistaken for the recording.
- **A stalled viewer used to freeze the producer.** "A browser tab must never back-pressure a
  simulation" was aspirational: a blocking write to a peer that stopped reading blocks forever,
  and the single accept thread does the catch-up write, so one paused tab wedged every later
  viewer too. Sockets now carry write and handshake timeouts, which is what makes the sentence
  true. CI runs a viewer that completes the handshake and then reads nothing, and requires a
  healthy viewer to still finish `cmp`-identical.
- **The producer used to claim the invariant over an audience of nobody.** Both transports now
  report what actually happened — how many viewers received the sealed file, how many were
  abandoned — and a replay whose viewers did not all receive it exits non-zero.

CI's adversarial case for the join paths is five clients joining at staggered instants
*mid-burst*: a record broadcast while a joiner snapshots history must reach it exactly once —
the join paths hold one lock across extend-and-send so nothing lands between a snapshot and a
subscription — and every joiner must end up `cmp`-identical.

## Real dynamics, same receipt

`ferroscope-motion` closes the last gap between "described" and "simulated": ferromotion's
recursive Newton-Euler dynamics drive the SO-101's calibrated inertials through a PD reach, and
every step lands in a recording with the same receipt as everything else here — **the run is
produced and certified by the same stack that renders it.**

```sh
cargo install ferroscope-motion
ferroscope-motion reach.mcap                # PD reach under real dynamics
ferroscope-motion drop.mcap --passive      # gravity only; exit 1 if energy drifts > 5 %
```

Two things are real here that are models everywhere else:

- the **actuation rail is computed**, per joint, as mechanical shaft power `|τ · ω|` from the
  torques the controller actually applied — stated as mechanical, because electrical would need
  a motor model this crate does not have;
- the **physics is gated**: `--passive` drops the arm under gravity alone and fails past 5 %
  total-energy drift, and CI runs the same experiment twice and requires identical digests —
  the determinism claim on an actual integrator, not a closed form.

The gate has already caught one real bug: the first build omitted the **armature** — a geared
servo's reflected rotor inertia, which dominates palm-sized links — and reported 12.31 % drift
and 597 J of "actuation" for a desk arm. With the armature on the mass-matrix diagonal the drift
is 0.31 % and the reach costs 1.875 J, which is what a small arm actually spends.

And the ledger then says something worth hearing: on this arm, an 8 W SoC model out-spends the
measured mechanics **17 to 1**. For palm-sized robots, thinking costs more than moving.

The **dynamics** button in the viewer is this recording.

## Describe the scene you want

Everything above reads a file some simulator produced. This goes the other way.

A scene is JSON — bodies, how each one moves, and optionally a robot from its own URDF — and it
records to the same plain MCAP, with the same determinism receipt and the same energy ledger:

```json
{
  "name": "a crate dropped beside a sweeping arm",
  "duration_s": 4.0, "rate_hz": 120,
  "bodies": [
    { "id": "crate", "shape": "box", "size": [0.3, 0.3, 0.3], "material": "6061-T6",
      "motion": { "kind": "fall", "from": [0.6, 0, 1.8], "restitution": 0.35 } },
    { "id": "beacon", "shape": "sphere", "size": [0.06, 0.06, 0.06],
      "motion": { "kind": "orbit", "center": [0, 0, 0.9], "radius": 0.8, "period_s": 3.0 } }
  ],
  "robots": [ { "id": "arm", "urdf": "examples/robots/so101.urdf", "sweep": "each" } ]
}
```

```sh
ferroscope scene examples/scenes/warehouse.json warehouse.mcap
ferroscope scene --schema          # the format, its defaults, and a worked example
```

Every motion has a **closed form**, so the pose at a timestamp costs the same whether you played to
it or scrubbed to it, and two runs of one scene agree bit for bit without anything having to be
careful about ordering. That is what lets a described scene carry the same receipt as a simulated
one. `fall` is the exception worth naming: it is a real ballistic arc that bounces and comes to
rest, and it still has a closed form because each bounce is a fixed fraction of the last.

### A scenario, not a single run

One recording answers "what happened". A scenario answers "does it *still* hold, across the range
I care about" — which is the question that decides whether something ships. Add `cases` and
`checks` to any scene:

```json
{
  "cases": { "drop_m": [0.5, 1.0, 2.0, 4.0], "bounce": [0.1, 0.6] },
  "bodies": [
    { "id": "crate", "shape": "box", "size": [0.3, 0.3, 0.3],
      "motion": { "kind": "fall", "from": [0.8, 0, {"$": "drop_m"}],
                  "restitution": {"$": "bounce"} } }
  ],
  "checks": [
    { "name": "settles within 1.5 s", "measure": "settled_s", "of": "crate", "at_most": 1.5 }
  ]
}
```

```text
$ ferroscope scene examples/scenes/drop-height.json out.mcap --sweep

  drop_m=1, bounce=0.1           300     41.86  pass
      ok   settles within 1.5 s: settled_s[crate] = 0.5000
  drop_m=1, bounce=0.6           300     41.86  FAIL
      FAIL settles within 1.5 s: settled_s[crate] = 1.6583, above 1.5

  8 case(s), 5 passed, 3 failed          # and exit 1
```

Each case is its own recording with its own receipt, so a failure is a file you can open. The
measured number prints on a pass as well as a failure — a column of "pass" with no numbers is a
table nobody can sanity-check.

**`of` is not optional detail.** Put a robot in the scene and the scene-wide minimum is the
robot's, so a check named "the crate stays on the floor" quietly becomes a statement about the
arm. Scoping it to a body is what makes it mean what it says, and a check naming a body that is
not there **fails and lists the ids that are**, rather than passing on a number nobody measured.

### Or just say it

You should not have to write JSON to see a crate fall.

```sh
ferroscope say "drop three red crates from 2 m beside an SO-101 arm for 6 seconds"
```

```text
  understood   3 boxes each of 0.3 m, falling from 2 m
  understood   arm: the so101 description, sweeping one joint at a time
  assumed      box size 0.3 m (not stated)
  assumed      120 Hz (say "at 60 Hz" to change it)
  NOT USED     "belt", "conveyor" — no meaning in the scene vocabulary
```

**That last line is the whole design.** Any phrase reader fails on language it was not built for;
what separates a useful one from an infuriating one is whether it tells you *which words it threw
away*. A sentence that silently loses "onto a conveyor belt" produces a scene with no conveyor and
no explanation, and you are left comparing your sentence against a picture, guessing which half
arrived.

So it always reports three things — what it **understood**, what it **assumed** because you did
not say, and what it **could not use** — and `--json` prints the scene it built, which is ordinary
scene JSON you can edit and re-run. It is a starting point you can correct, never a black box.

It is deterministic and offline: no key, no request, no model. That also means it is *small* —
shapes, five motions, counts, units, colours, materials, and a few worlds with different gravity.
For anything more open-ended, a model is the right tool, and the MCP server below is how you get
one. The viewer runs this parser in WebAssembly, so typing a sentence into the page and getting a
recording back never leaves the tab.

### An agent can drive all of it

`ferroscope-mcp` is an [MCP](https://modelcontextprotocol.io) server over stdio. Point a client at
the binary — no configuration, no network, no account:

| tool | what it does |
|---|---|
| `scene_schema` | the format, with defaults and a worked example |
| `scene_from_text` | an English phrase, with what it understood, assumed and ignored |
| `scene_sweep` | a grid of cases, judged, with the number that decided each |
| `scene_validate` | every problem at once, each with the JSON path that was wrong |
| `scene_record` | records it, and returns the receipt, the joules and the clearance |
| `robot_check` | is this URDF physically usable |
| `mesh_check` | what an STL is, and what it would weigh |
| `materials_search` | 437 materials, each with the source it is cited from |
| `run_inspect` · `run_verify` · `run_energy` · `run_diff` | the CLI's read verbs |

### On ACP

Ferroscope deliberately does **not** implement the [Agent Client Protocol](https://agentclientprotocol.com).
ACP connects an *editor* to a *coding agent*, and its `session/new` carries an `mcpServers` list
that the client hands to that agent. So an ACP editor already delivers this server: configure
`ferroscope-mcp` as an MCP server in Zed, JetBrains or Kiro and the agent gets all ten tools.
Implementing ACP here would mean pretending to be a coding agent, which Ferroscope is not.

The design rule for all of it is that **the caller is a model that has to fix its own mistakes**, so
a refusal that does not say how is a wasted round trip:

```text
4 problem(s) in this scene:
  bodies[0].shape: unknown shape "cube"; expected one of box, sphere, cylinder, plane
  bodies[0].size: expected 3 numbers, found 2
  bodies[0].motion.kind: unknown motion "drop"; expected one of static, linear, orbit, oscillate, fall
  bodies[0].color: "brown" is not a hex colour; expected "#rrggbb"
```

Every problem at once, not the first one — five mistakes should cost one pass, not five.

### Or over HTTP, with nothing installed

The same crate compiled to wasm runs at the edge, so a scene can be recorded by anything that can
make a request:

```sh
curl -X POST https://physicalai-bmi.org/api/scene/record \
     -d '{"duration_s":3,"rate_hz":100,
          "bodies":[{"id":"crate","shape":"box","size":[0.3,0.3,0.3],
                     "motion":{"kind":"fall","from":[0.6,0,1.8]}}],
          "robots":[{"id":"arm","urdf":"so101"}]}' -o scene.mcap
```

```text
x-ferroscope-trace-digest:  64adb806a715535f3dd6d16ce9bf0626…
x-ferroscope-joules:        50.232
x-ferroscope-lowest-point:  -0.2057 arm/moving_jaw_so101_v1_link
```

The receipt, the joules and the clearance come back in headers, so none of it costs a second
request or a parse of the file you were just handed. `GET /api/scene/schema` is the format,
`POST /api/scene/validate` checks without recording, CORS is open, and there is no key. A short
list of robots is built in, so `"urdf": "so101"` resolves without you shipping a description.

And the part worth saying plainly: **a scene recorded by wasm in a Cloudflare Worker verifies, byte
for byte, under the native CLI.** Two runtimes with nothing in common but the bytes, agreeing —
which is the entire reason the receipt is defined over the file rather than over the process.

From a page, with no build step:

```js
import { record } from 'https://physicalai-bmi.org/assets/ferroscope/ferroscope.js';

const run = await record({ duration_s: 3, bodies: [ /* … */ ] });
run.receipt.traceDigest;  // recomputable from run.bytes alone
run.joules;               // E_task, estimated
```

## What a mesh weighs

`ferroscope-mesh` reads STL (both dialects, deciding by arithmetic rather than by the leading word,
because a binary STL may legally begin with `solid`), writes glTF, and integrates **volume, centre
of mass and the full inertia tensor** straight off the triangles by the divergence theorem. Exact
for any closed mesh; no sampling, no voxels.

```sh
ferroscope urdf my_robot.urdf out.mcap --meshes ./meshes
```

That resolves the meshes a URDF names, reports each one's triangle count, volume and whether it
closes, converts it to glTF and carries it **inside** the recording as an attachment.

It matters because a robot description makes two claims about every link — a *shape* and a *mass
distribution* — and nothing in the usual toolchain checks that the second is consistent with the
first. `ferroscope-cad` closes that loop against [CadFuture]'s material tables:

```text
AS 6061-T6 (Lut tier, ASM Handbook Vol 2, MatWeb)
  density     2710 kg/m3
  mass        0.1301 kg
  inertia     ixx=2.1680e-5  iyy=4.3360e-5  izz=5.6368e-5

That is the <inertial> block this geometry implies, about its centre of mass.
```

So a declared inertial can be *compared* with the geometry it claims to describe: a link heavier
than solid stock of its own outline is impossible and is refused; a tensor that describes a
different shape at the right mass is caught too, because the tensor is normalised by mass before
comparison.

### The tier is part of the answer

[CadFuture] resolves every engineering query at the cheapest tier that can answer it — **LUT, then
closed-form formula, then solver, then a model**. Which tier answered is not an implementation
detail. It is the difference between a number that cost picojoules and one that cost joules, and
between a number with a citation and one with a residual. Every value that crosses this bridge
carries its tier and its source into the recording, because a quantity whose origin is not in the
file is a quantity nobody can audit.

[CadFuture]: https://github.com/dcharlot-physicalai-bmi/cad-future

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
ferroscope export  <run.mcap> <out.json> viewer bundle — how to see a run too big
                                         for a browser to open whole
ferroscope live    <run.mcap>            REPLAY it as a live stream, on its own clock
                   [--port <n>] [--wt] [--rate <x>] [--hold <s>] [--no-wait]
                   binds 8737, the port the viewer's live button dials
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

**Five crates carry zero runtime dependencies** — `std` and nothing else — and build for
`wasm32-unknown-unknown` unchanged. Everything above them is additive, and the two crates that
reach outside are the two that are not published, so nothing you install pulls in a tree.

```
ferroscope-mcap      MCAP v0 reader + writer.       0 deps.  wasm-clean.
ferroscope-ledger    E_task arithmetic + coverage.  0 deps.  wasm-clean.
ferroscope-receipt   SHA-256, digests, comparator.  0 deps.  wasm-clean.
ferroscope-mesh      STL in, glTF out, and what     0 deps.  wasm-clean.
                     a mesh weighs.
ferroscope-power     RAPL and powermetrics, or a    0 deps.  native only.
                     clear reason there is nothing.
ferroscope-schema    Recorder, schemas, verify().   depends only on the crates above.
ferroscope-urdf      URDF to scene, plus FK,        0 external deps, wasm-clean.
                     validation and clearance.
ferroscope-scene     Described scenes to MCAP.      0 external deps.
ferroscope-run       Scenarios, cases, suites,      native only: it reads clocks and
                     verdicts, local history.       writes files, and says so.
ferroscope-cli       The CLI (binary: `ferroscope`).
ferroscope-wasm      Browser bindings.              + wasm-bindgen.
ferroscope-cad       The LUT-first bridge.          + CadFuture. Not published.
ferroscope-mcp       The MCP server.                + the above. Not published.
viewer/              The WebGPU workspace.          + three.js, vendored, not a CDN.
```

`ferroscope-cad` and `ferroscope-mcp` are marked `publish = false` on purpose: they consume
CadFuture's `physical-*` crates from git, and those are not on crates.io yet. Everything you get
from `cargo install ferroscope-cli` is the dependency-free half.

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
the CLI's nine verbs, the in-browser WebGPU viewer with glTF meshes and a measure tool, the
scenario harness, URDF import with its physical-usability checks and ground-clearance report, the
LeRobot SO-101 as a demo device, STL-to-glTF with exact mass properties, the LUT-first material
bridge, described scenes, the MCP server, the HTTP API and browser SDK, and `ferroscope power` reading real counters. **204 tests, clean clippy, three platforms in CI plus
wasm32**, and jobs that gate the zero-dependency claim, the viewer bundle's export surface, the
scene format and the MCP protocol surface.

**The original "next" list is now empty.**

Nothing here is feature-gated, and nothing here will be.

---

## License

MIT **OR** Apache-2.0, at your option. Free to use, study, fork, and build on, which is the point.

Part of the open ecosystem from the [Institute for Physical AI @ BMI](https://physicalai-bmi.org)
alongside [Ferric](https://ferric.physicalai-bmi.org) (compute),
[Ferralloy](https://ferralloy.physicalai-bmi.org) (fleet), and Ferromotion (kinematics and dynamics).
