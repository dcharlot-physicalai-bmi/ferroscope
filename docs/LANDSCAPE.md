# The physical-AI tooling landscape, August 2026

A survey of what exists, across languages and regions, for the three things this repository
touches: **simulation and sim-to-real**, **control**, and **CAD and robot description**. Written to
find out what Ferroscope should *not* build, and what nobody has built.

## Method, and its limits

Read: the maintained simulator census, the primary repositories and docs of each tool named below,
two 2026 systematic reviews of energy measurement, the ROS 2 message and bag documentation, and
vendor documentation for the commercial platforms. Searched in English, Chinese, Japanese, German
and French.

Not done: no tool below was installed and benchmarked for this survey except the ones this
repository already depends on. Star counts are from the weekly-regenerated
[best-of-robot-simulators](https://github.com/knmcguire/best-of-robot-simulators) census and are a
proxy for attention, not for quality. Where a claim is a vendor's, it is marked as theirs. Where
this review could not find something, it says it did not locate it rather than that it does not
exist.

---

## 1. Simulators and physics engines

The field is not short of simulators. It is short of agreement.

| | Language | Licence | Attention |
|---|---|---|---|
| **Genesis** | Python/C++ | Apache-2.0 | 30k |
| **AirSim** | C++ | MIT | 18k |
| **Bullet** | C++ | zlib | 15k |
| **MuJoCo** | C++ | Apache-2.0 | 14k |
| **CARLA** | C++ | MIT | 14k |
| **Gymnasium** | Python | MIT | 12k |
| **O3DE for Robotics** | C++ | MIT/Apache-2.0 | 9.5k |
| **Isaac Lab** | Python | BSD-3.0 | 7.7k |
| **Newton** | Python/C++ | Apache-2.0 | 5.2k |
| **Webots** | C++ | Apache-2.0 | 4.5k |
| **Drake** | C++/Python | BSD-3.0 | 4.1k |
| **Isaac Sim** | Python | Apache-2.0 + NVIDIA EULA | 3.7k |
| **Habitat Sim** | Python/C++ | MIT | 3.7k |
| **Pinocchio** | C++/Python | BSD-2.0 | 3.6k |
| **Brax** | Python | Apache-2.0 | 3.2k |
| **ManiSkill** | Python | Apache-2.0 | 3.1k |
| **Project Chrono** | C++ | BSD-3.0 | 2.9k |
| **MuJoCo Warp** | Python/C++ | Apache-2.0 | 1.4k |
| **Gazebo** | C++ | Apache-2.0 | 1.4k |
| **DART** | C++ | BSD-2.0 | 1.2k |

Plus domain-specific families the census separates out: aerial (Project AirSim, Cosys-AirSim,
Aerial Gym, Pegasus, PyFlyt, Crazyflow, gym-pybullet-drones), maritime (Virtual RobotX, Stonefish,
HoloOcean, UNav-Sim, DAVE), automotive (CARLA, esmini, AWSIM), space (Basilisk, Astrobee, OmniLRS),
2-D (IR-SIM, pyrobosim, mvsim), and swarms (ARGoS).

**Two things stand out.** Gazebo, the ROS default for fifteen years, now draws less attention than
eight newer engines. And the fastest-growing entries are GPU-parallel Python (Genesis, Isaac Lab,
Brax, MuJoCo Warp, ManiSkill), which is a statement about where the field thinks the bottleneck is:
throughput for learning, not fidelity for engineering.

### Determinism, as the field actually offers it

Every serious engine offers *deterministic stepping*, and that is not the same as reproducibility:

- **MuJoCo** is chosen precisely for "deterministic stepping for reproducible control evaluation".
- **CoppeliaSim** offers "deterministic scene replay and time-aligned telemetry logging".
- **Isaac Sim** documents determinism through "exact PhysX parameter tuning, standalone step control
  scripts, and Omnigraph-based environment orchestration", and Isaac Lab documents the limit in the
  same breath: GPU work scheduling reorders floating-point reductions, so "experiments from the
  IsaacGym simulator are not perfectly reproducible on a different system".

So the field's answer to reproducibility is **pins**: fix the seed, pin the assets, lock the physics
config. Pins are necessary. What this review did not locate anywhere is the other half: **an
artefact that lets a third party check, from the recording alone, that a run actually reproduced,
and if it did not, where.** That is the gap `ferroscope-receipt` fills.

---

## 2. Robot description, and CAD into it

Three formats dominate in 2026: **URDF** (ROS), **MJCF** (MuJoCo), **USD** (Isaac/Omniverse). The
best practice the community converged on is worth quoting because it settled a design question here:
maintain **URDF as canonical** and regenerate the others.

Converters and pipelines, by entry point:

| From | Tool | Notes |
|---|---|---|
| Onshape | **onshape-to-robot** | to URDF, SDF and MJCF, over the Onshape API |
| Fusion 360 | **fusion2urdf**, **ExportURDF** | ExportURDF aims at Fusion/Onshape/SolidWorks in one library |
| SolidWorks | **sw2urdf** | the long-standing plugin |
| FreeCAD | robot workbench | built in |
| STEP | **robosimtools STEP-to-URDF** | computes inertia, browser-based |
| URDF | **newton-physics/urdf-usd-converter** | to OpenUSD with `UsdPhysics` |
| URDF | **DLR-RM/urdfmodelica** | to Modelica multibody; award, 16th Modelica & FMI Conference |

And a live attempt to replace the format itself: **"Beyond URDF: The Universal Robot Description
Directory"** (arXiv 2512.23135) argues for shared, extensible, standardised robot models. Worth
watching; not yet something to build on.

**The interesting absence.** Every one of those tools gets geometry *into* a description. This
review did not locate a tool that **checks a description for the errors that break sim-to-real**:
a link with a visual and no collision, a zero or negative mass, an inertia tensor that is not
positive definite, principal moments that violate the triangle inequality. Those are the cheap,
common causes of a policy that works in simulation and not on hardware, and they are checkable from
the file in milliseconds.

---

## 3. Control and dynamics

Mature, and mostly C++ with Python bindings:

- **Pinocchio** (BSD-2, 3.6k) is the centre of gravity: rigid-body dynamics with analytical
  derivatives, and the dependency under Crocoddyl, Stack-of-Tasks and the Humanoid Path Planner.
- **Crocoddyl** — DDP-based optimal control under contact sequences.
- **OCS2** (ETH legged robotics) — optimal control of switched systems, with URDF-to-model helpers
  via Pinocchio, centroidal models, self-collision through HPP-FCL, **and its own ROS visualisation
  and plotting**.
- **Drake** (TRI, BSD-3, 4.1k) — model-based design and verification, now integrable into MoveIt 2.
- **MoveIt 2** and **ros2_control** — the ROS 2 planning and hardware layer.
- **iDynTree** + **YARP** (IIT, Italy) — dynamics for control and estimation, with a URDF-loading
  visualiser reading state over YARP ports.
- **IHMC open robotics software** — **Java**, momentum-based whole-body control, running on
  humanoids and exoskeletons. The field's most successful non-C++ control stack.
- **RigidBodyDynamics.jl**, **RigidBodySim.jl**, **QPControl.jl** — **Julia**, with automatic
  differentiation through ForwardDiff and symbolic dynamics through SymPy.
- **MATLAB/Simulink + Simscape**, and an open floating-base simulator library for it. Open
  alternatives: Scilab/Xcos, OpenModelica.
- **RoMoCo** — reduced-order-model locomotion toolbox for bipeds and humanoids.

Note the pattern: **every control framework ships its own visualisation.** OCS2 has ROS plotting,
iDynTree has a visualiser, Drake has meshcat, MuJoCo has its viewer. Observability is repeatedly
rebuilt per stack rather than shared, which is the structural reason a neutral interface layer has
room to exist.

---

## 4. Middleware and dataflow, including Rust

The Rust robotics ecosystem is real and is no longer a curiosity:

- **Zenoh** — pub/sub/query with very low overhead, "becoming the protocol of choice for
  robot-to-anything"; now under the ROS 2 umbrella as an RMW.
- **dora-rs** — Rust dataflow with a Python API, claiming 10–17× ROS 2 via shared-memory IPC and
  Apache Arrow, with flat latency from 4 KB to 4 MB on a Zenoh SHM data plane.
- **copper-rs** — a Rust-first robotics runtime, "a game engine for robots", presented at FOSDEM
  2026.
- **openrr** — Rust robotics platform with `arci` as its hardware abstraction, usable without a ROS
  installation.
- **rapier** / **parry** — rigid-body physics and collision in Rust. **k** — kinematics.
- **Bevy** (47k) — Rust data-driven engine, used as a rendering substrate.
- **Rumoca** (arXiv 2606.14998) — a Rust-native Modelica compiler.
- **ros2_rust** — Rust client library for ROS 2.

Elsewhere: **Gobot** (Go, devices and IoT), **Unity/C#** with ROS-TCP and the industrial
**realvirtual** framework (deterministic kinematics for closed chains, 25+ industrial interfaces
including Siemens S7, TwinCAT, OPC UA and FMI/Simulink), **PowerJoular** in **Ada**.

**What this means for Ferroscope:** the neighbours to interoperate with are Zenoh, dora-rs and
copper-rs, not to compete with. A recording layer that speaks MCAP is downstream of all three.

---

## 5. Viewers and observability

This is the layer this repository plays in, and the survey corrected a claim made earlier in this
project's own documentation.

| | What it is |
|---|---|
| **Foxglove** | The commercial platform. App proprietary since 2.0; MCAP and the Rust-core SDK genuinely open. |
| **Lichtblick** | **An actively maintained community fork of Foxglove Studio**, browser and desktop, preserving the open-core licensing model. Releases through mid-2026. |
| **Rerun** | Open-core, Rust viewer native and in-browser, entity-component with timelines. MCAP support marked experimental. |
| **RViz** | The ROS default. Cannot play a bag by itself; needs `ros2 bag play` alongside. |
| **PlotJuggler** | The time-series workhorse. |
| **meshcat** | Browser + WebGL, the viewer Drake and Pinocchio reach for. |
| **viser** | Python web 3-D for vision and robotics, explicitly inspired by Pangolin, ImGui, rviz, meshcat and Gradio. |
| **Choreonoid** | AIST, Japan. C++ plugin architecture, GUI editing of robot state and motion, OpenRTM integration. |
| **iDynTree visualizer** | IIT, Italy. URDF in, YARP state, rendered. |

**The correction:** an earlier version of this project's landscape said Foxglove's app is
proprietary and left the impression that there is no maintained open alternative. **Lichtblick is
one**, and a viewer comparison that omits it is incomplete. Ferroscope's differentiators against
Lichtblick are the same two as against Foxglove — a recomputable determinism receipt and a joules
ledger — and they are differentiators of *substance*, not of licence.

**The format question is settled and it settles it in our favour.** MCAP is the ROS 2 default bag
format from Iron Irwini onward. Anything that writes MCAP is downstream of the whole ROS 2 world by
default.

---

## 6. Energy: the one place the gap is documented, not inferred

Two 2026 systematic studies, read in full, and they agree.

**"A Curated List of Open-source Software-only Energy Efficiency Measurement Tools: A GitHub Mining
Study"** (arXiv 2603.21772) censused 24 repositories:

| Tool | Language | Scope |
|---|---|---|
| Scaphandre | Rust | process |
| Kepler | Go | container/pod |
| pyRAPL | Python | sub-process |
| PowerJoular | Ada | process |
| Green Metrics Tool | Python | multi-level |
| CodeCarbon, CarbonTracker, Eco2AI | Python | energy + emissions |

The gaps the authors name: reliance on vendor-specific metering interfaces (RAPL, `nvidia-smi`)
limiting cross-platform comparability; Linux dominance; machine-level monitoring still dominant;
and — the finding that matters here — **no tool in the census targets robotics**, and the study does
not identify a logging or recording format for energy data.

Every one of those 24 tools measures **compute**. None measures **actuation**.

**"Energy Efficiency in Robotics Software: A Systematic Literature Review (2020–2024)"**
(arXiv 2508.12170) says the rest:

- **no unified standard metric** exists for robotics energy efficiency;
- actuation energy and computational energy are **"typically treated as separate concerns" rather
  than holistically**;
- open problems include **"limited tooling for joint analysis of mechanical and computational
  energy"** and "poor integration between hardware energy models and software profiling";
- the research "remains fragmented across mechanical engineering, embedded systems, and software
  communities".

And the message layer confirms it from the other direction: ROS 2 ships
`sensor_msgs/msg/BatteryState` — percentage, charge, current, voltage — and this review **did not
locate a standard convention for watts or joules**, nor an energy-accounting topic standard.

That is `E_task = E_compute + E_actuation` described as an open problem by an independent review, in
a field whose 24 energy tools do not include one for robots.

---

## 7. Regions

**China.** The most active hardware-plus-open-source pairing. Humanoid programmes releasing URDFs,
simulation code, RL training code and SDKs alongside the robots; whole-body control built on
Pinocchio with MPC+WBC; an 18-platform simulator census maintained in Chinese; a widely shared
practitioner rule that **URDF is the route for verifying kinematics and glTF/GLB the route for a
presentable digital twin** — which is exactly the pair Ferroscope now carries in one file.

**Japan.** Choreonoid (AIST, now Choreonoid Inc.) and OpenRTM-aist: a C++ plugin architecture for
simulation and GUI motion editing, integrating RT-components.

**Europe.** DLR heads the Modelica Association and ships `urdfmodelica`; **FMI** is the
tool-independent model-exchange standard the industrial world actually uses; INRIA/LAAS produce
Pinocchio and the Stack-of-Tasks; IIT produce iDynTree and YARP. The European centre of gravity is
**model exchange and co-simulation standards**, which is a different instinct from the American one
(platforms) and the Chinese one (hardware plus weights).

---

## The gap map

Six gaps, each with what it would take and whether this repository addresses it.

| Gap | Evidence | Ferroscope |
|---|---|---|
| **1. No third-party-checkable reproduction.** The field pins seeds and assets; nothing produces an artefact a stranger can verify from the file. | Isaac Lab documents non-reproducibility across systems; MuJoCo/CoppeliaSim offer deterministic stepping, not verification. | **Yes.** `ferroscope-receipt`: a spec digest excluding the platform, a trace digest at declared precision, a comparator that names the diverging step. |
| **2. No joint accounting of compute and actuation energy.** | SLR: "typically treated as separate concerns"; "limited tooling for joint analysis". 24-tool census: none for robotics. | **Yes.** `ferroscope-ledger`, with a coverage refusal. |
| **3. No per-run cost figure on the platforms.** | Antioch's own docs: machine time is assignment-scoped, "no per-run or per-scenario cost figure to report". | **Yes.** Every run record carries joules, wall time and joules per passing check. |
| **4. Nothing validates a robot description.** Every CAD pipeline writes URDF; none checks it for the errors that break sim-to-real. | This review did not locate one. Missing collisions, bad masses and non-physical inertia tensors are classic causes. | **Next.** See below. |
| **5. Energy measurement is compute-only and vendor-locked.** | Census: RAPL and `nvidia-smi` dependence; Linux dominance; machine-level. | **Partly.** The ledger accepts measured watts; it does not yet measure them itself. |
| **6. Observability is rebuilt per control stack.** | OCS2, iDynTree, Drake, MuJoCo each ship a viewer. | **Yes, structurally.** One MCAP recording, read by any of them, plus a viewer that needs no install. |

### What the survey changed

1. **A correction shipped.** Lichtblick belongs in the viewer comparison. Licence is not the
   differentiator; the receipt and the ledger are.
2. **The next feature changed.** Collision and inertial geometry was going to be a rendering
   feature. Gap 4 says it should be a **validator**: `ferroscope urdf --check` that reads a
   description and reports, as pass/fail criteria with measurements, whether every link has
   collision geometry, whether masses are positive, and whether each inertia tensor is physically
   realisable. That is a CI gate for robot descriptions, it costs milliseconds, and this review did
   not find one.
3. **Gap 5 is the feature after that.** Read real power from the machine (RAPL, `powermetrics`) and
   book it as the compute rail automatically, so the joules lane stops taking the caller's word for
   the compute half.
4. **Interoperate, do not compete, with Zenoh, dora-rs and copper-rs.** They are transport and
   runtime; this is recording and evidence.

---

*Compiled 2026-08-17 for the [Institute for Physical AI @ JBI](https://physicalai-bmi.org). Every
number and quotation above is from a source named in the row it appears in. Corrections welcome as
issues.*
