# What determinism costs, measured

A receipt declares that a run reproduces *to a stated precision*. This document is where that
number comes from: every determinism measurement this project has made, what machine made it, and
the command that makes it again.

It exists because the interesting claims in this area are usually asserted. "GPUs are
non-deterministic", "floating point is not associative so all bets are off", "declare a tolerance
and move on" — each is true enough to repeat and vague enough to be useless when you are deciding
whether last week's run and today's are the same run. Every heading below is a number instead.

## Method, and its limits

**Machines.** Three GitHub-hosted runners — Linux x86-64, macOS arm64, Windows x86-64 — for
everything cross-platform, re-run on every push to `main`. One Apple M5 Max (40 GPU cores,
Metal 4) for the GPU work and the local timings.

**What is measured.** Trace digests recomputed from files; trajectories compared value by value;
reduction orderings performed exactly; wall and CPU time; peak RSS; browser heap.

**What this work did not measure**, and would need other hardware or another study to say
anything about:

- **A GPU with `f64`.** WGSL has no such type and Metal on Apple silicon exposes none, so the
  fabric measurements here are `f32`. An NVIDIA or AMD part with double precision would separate
  reordering from narrowing in a way this machine cannot.
- **Other libm implementations.** Three operating systems is three libms, not a survey. glibc,
  Apple's, and Microsoft's are the three below; musl, and the various vectorised math libraries,
  are not.
- **Other simulators.** Every trajectory here is produced by this repository. Whether a physics
  engine with a contact solver behaves the same way is a question this work does not answer.
- **Runs longer than ~12.8 million messages**, or wall-clock durations beyond about 25 minutes of
  simulated time.

Numbers stated as ranges are the observed range over the runs named, not error bars.

---

## 1. Two runs, one machine, one binary

Bit-identical, by construction, and not interesting except as the control: if this ever failed,
nothing below would mean anything. It is gated in CI on every push.

```sh
ferroscope demo a.mcap --steps 4000 --seed 7
ferroscope demo b.mcap --steps 4000 --seed 7
ferroscope diff a.mcap b.mcap        # identical at bit-exact, exit 0
```

## 2. Three operating systems, one spec

The spec digest deliberately excludes the platform, so two machines running one experiment share
it and the question becomes whether their **traces** agree. CI records the same spec on all three
runners at a ladder of declared precisions and reads off the answer.

|  | `demo` — only `+ − × ÷` and `abs` | `scene` — sweeps joints with `sin`, `cos` |
|---|---|---|
| linux vs macos | **identical at bit-exact** | worst \|Δ\| **4.441e-16** · worst rel 8.861e-12 |
| linux vs windows | **identical at bit-exact** | worst \|Δ\| **4.441e-16** · worst rel 8.861e-12 |
| macos vs windows | **identical at bit-exact** | worst \|Δ\| **4.441e-16** · worst rel 5.179e-12 |
| digests agree at | **exact** | **drop 20 of 52 mantissa bits** |
| at 19 bits | — | linux = macos, windows differs |

**Cross-machine bit-exactness is achievable, and it is not luck.** Every operation in the demo's
integrator is exactly specified by IEEE-754, so two conforming machines *must* agree. That they do
says the pipeline between the arithmetic and the digest adds nothing of its own — no reordering,
no contraction, no width surprises — which is worth establishing before trusting any receipt.

**What costs determinism is specific and nameable.** A libm is not the same function on three
operating systems. The price is 4.441e-16, which is **2.00 units in the last place** at magnitude
~1: two machines disagreeing about the last bit of a sine, twice. The two Unix libms converge one
bit before the third.

The relative figure is four orders larger than the absolute one because it lands on a transform
component passing near zero, where relative error is meaningless. It is printed, not ranked on.

### The same fact, said in units

```sh
ferroscope scene examples/scenes/warehouse.json out.mcap --precision exact --resolution 1e-9
```

Declare what the run claims — a nanometre, a nanoradian — and the three machines produce **one
digest at bit-exact precision**. `drop_bits 20` and `1e-9 of each unit` are the same underlying
fact, but only one of them is a sentence about a robot.

That works here because the scene is 400 steps and the differences sum to about 2e-13, far inside
a nanometre cell. See §5: a longer run needs a coarser claim.

*Reproduce:* the `platforms` and `platform-agreement` jobs in `.github/workflows/ci.yml`, on every
push.

## 3. A fabric that reduces in parallel

The expectation going in was that a GPU reorders its reductions, `+` is not associative, and so
bit-exactness dies and a declared precision is all that survives. Half right, and the wrong half
is the interesting one.

One shader, one set of values, dispatched at 16, 64, 256 and 1024 workgroups on an Apple GPU
through WebGPU — the only thing changed is the partition:

| shape | terms | reordering | narrowing f64→f32 | reordering ÷ narrowing |
|---|---|---|---|---|
| uniform | 65,536 | 2.383e-7 | 8.427e-6 | **2.8 %** |
| uniform | 1,048,576 | 2.382e-7 | 5.439e-6 | **4.4 %** |
| mixed magnitudes | 65,536 | 6.753e-7 | 3.784e-5 | **1.8 %** |
| mixed magnitudes | 1,048,576 | 5.600e-7 | 6.563e-4 | **0.1 %** |

**Reordering is the small term.** It is 2–6 `f32` ULP and near-constant in the number of terms,
because a GPU reduction is a *tree* — usually more accurate than the sequential loop it replaces,
not less. What costs you is that the fabric has no `f64` to reorder, so values are narrowed before
they arrive. On the worst row the narrowing is **560×** the reordering. Calling both "GPU
non-determinism" hides which one you are paying for.

**The GPU is not random.** Every figure reproduces exactly, run to run. It is deterministic *per
configuration* and differs *between* configurations — so bit-exactness is lost not to noise but to
the dispatch shape not being part of the spec. Put it in `RunSpec::config` and two runs of that
configuration are comparable bit for bit again.

In receipt units: two dispatch shapes agree at **drop_bits ≥ 32** (keeping 20 of 52); a GPU run
agrees with the CPU run it came from at **≥ 42** (keeping 10).

```sh
PUPPETEER=<path to puppeteer-core> node ci/gpu-reduction.mjs
```

Not in CI: GitHub's runners have no GPU. WebGPU also needs a **secure context** — `about:blank`
reports no `navigator.gpu` at all, which is indistinguishable from a machine without one.

## 4. The same reordering in `f64`, exactly

A GPU's reorderings can be performed on a CPU, where `f64` survives, which separates "different
order" from "narrower type".

```sh
cargo run --release --example reduction_order -p ferroscope-receipt
```

- **The spread grows like √n ULP** — measured at 0.1–0.6 ULP per √term across five decades of *n*,
  for both benign shapes. It is a property of the arithmetic, not of the data.
- **Reordering is not merely disagreement, it is error.** The worst ordering is about as far from
  the compensated (Kahan) sum as the orderings are from each other.
- **The digest needs 1–5 bits more than the spread implies**, because it *masks* low bits rather
  than rounding them: every ordering must land in the *same* bucket, not merely be close.
- **A near-cancelling sum cannot be pinned by a relative precision at all.** Where the terms are
  large and the answer sits near zero, the same absolute spread costs 14 dropped bits instead of 5.

## 5. What a receipt can actually declare

The trace digest quantizes by **masking** low mantissa bits, which is a *pointwise relative*
operation: each value is bucketed against its own magnitude. Right for a quantity that stays away
from zero, wrong for one that crosses it — and measurably so.

Two runs of the demo, agreeing to about 1e-6 of every channel's own scale, could declare no better
than **`drop_bits` 51 of 52**. One bit of mantissa.

```sh
ferroscope demo a.mcap --steps 3000 --seed 7 --precision exact
ferroscope demo b.mcap --steps 3000 --seed 7 --drift 1500 --precision exact
cargo run --release --example declarable_precision -p ferroscope-schema -- a.mcap b.mcap
```

```
 bits   channel                    scale   worst |Δ|    smallest
   51   /control/height_error   7.786e-2    2.048e-8    6.677e-8   ← binds the whole trace
   47   /robot/joints            1.225e2    1.396e-4    3.253e-6
   46   /robot/contacts          3.829e2    4.362e-4    1.084e-6
```

Verified end to end rather than modelled: re-recording that pair at each precision and comparing
the real trace digests agrees at 51 and differs at 50.

`/control/height_error` is a control error, so it lives near zero *by construction* — that is what
being a control error means. One channel doing its job set the precision for the entire recording.

**The fix is to declare the scale**, because a digest computed while the recording is being
written cannot normalise by a scale it has not seen yet:

```rust
rec.resolution("/control/height_error", 1e-6)?;   // or default_resolution for all channels
```

That channel is then hashed on an **absolute grid** whose cells stay the same size through zero. A
recording that declares nothing produces the digest this crate has always produced.

**What declaring does not fix.** Agreement is still per-sample: every value must land in the same
cell, so the grid must clear the **sum** of the differences, `grid ≫ Σ|Δ|` — a factor of *N* on top
of the physics. And **no multiplier is reliable**. Over six pairs the smallest resolution that
actually worked ran from **1.0× to 109×** the summed difference; a 10× rule fails twice, a 100×
rule once. So `ferroscope diff --declare` *measures* it, sieving the whole ladder of resolutions as
it walks, and reports the answer per channel rather than a rule of thumb.

```sh
ferroscope diff a.mcap b.mcap --declare
```



---

## What follows, if you are sealing a run

1. **Keep the arithmetic exactly specified where you can.** `+ − × ÷`, `sqrt` and comparisons are
   portable to the bit. Transcendentals are not, and that is where your precision budget goes.
2. **Declare in the units of the thing**, not in mantissa bits. "This run reproduces to a
   nanoradian" is checkable against the physics; "drop_bits 20" is checkable only against IEEE-754.
3. **Choose the number for the run's length as well as its units** — the *N* factor in §5 is real
   and grows.
4. **If a fabric is involved, put its configuration in the spec.** The dispatch shape is not noise;
   it is an input you have not written down.
5. **A digest match is proof; a mismatch is a question.** Quantization boundaries alone can split
   two values a hair apart, so a mismatch escalates to the comparator, which walks both traces and
   names what moved.

## Where this is weakest

The single most load-bearing untested assumption is §2's generality: **one demo and one scene, on
three runners.** The claim that pure IEEE-754 arithmetic is portable to the bit is a theorem and
the measurement agrees with it; the claim that 20 mantissa bits is what `sin` and `cos` cost is one
scene's worth of evidence, and a different trajectory through those functions could cost more. A
second and third scene family would turn a data point into a range.
