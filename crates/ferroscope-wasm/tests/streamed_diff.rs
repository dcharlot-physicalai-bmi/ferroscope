//! The streamed comparison must be the same comparison.
//!
//! `DiffStream` exists because a page that opened a recording in blocks no longer has its bytes,
//! and `diff` takes bytes. Two comparators is how this project has repeatedly ended up with two
//! answers to one question, so the only acceptable relationship between them is identity: the
//! same JSON, character for character, from the same pair of recordings.

use ferroscope_ledger::Rail;
use ferroscope_receipt::Precision;
use ferroscope_schema::{JointState, Recorder, Stamp};

fn recording(seed: u64, perturb_at: Option<u64>, steps: u64) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    let mut x = 0.2f64;
    let mut v = 0.0f64;
    for step in 0..steps {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        let mut f = -40.0 * x;
        if Some(step) == perturb_at {
            f *= 1.001;
        }
        v += f * 1e-3;
        x += v * 1e-3;
        rec.transform(
            "/body",
            t,
            "world",
            "body",
            [x, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
        rec.joints(
            "/joints",
            t,
            &JointState {
                names: vec!["slide".into(), "hip".into()],
                position: vec![x, -x],
                velocity: vec![v, -v],
                effort: vec![f, f * 0.5],
            },
        )
        .unwrap();
        rec.energy("/energy/motor", t, Rail::Actuation, "motor", f.abs() * 2.0)
            .unwrap();
        rec.energy("/energy/soc", t, Rail::Compute, "soc", 6.0)
            .unwrap();
        rec.scalar("/err", t, x, "m").unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", seed)
        .dt_ns(1_000_000)
        .steps(steps)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("wasm-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}

/// Drive a `DiffStream` the way a page does: two passes over each file on its own, then one
/// pass with both files fed together, block by block.
fn streamed(a: &[u8], b: &[u8], block: usize) -> Result<String, String> {
    let mut d = ferroscope_wasm::DiffStream::new(0.0, 0.0);
    for _ in 0..2 {
        for part in a.chunks(block) {
            if !d.push_a(part) {
                break;
            }
        }
        for part in b.chunks(block) {
            if !d.push_b(part) {
                break;
            }
        }
        d.rewind();
    }
    // Pass three: lockstep. Feed whichever side asks.
    let (mut ia, mut ib) = (0usize, 0usize);
    loop {
        let mut moved = false;
        if d.wants_a() && ia < a.len() {
            let end = (ia + block).min(a.len());
            d.push_a(&a[ia..end]);
            ia = end;
            moved = true;
        } else if d.wants_a() {
            d.end_a();
        }
        if d.wants_b() && ib < b.len() {
            let end = (ib + block).min(b.len());
            d.push_b(&b[ib..end]);
            ib = end;
            moved = true;
        } else if d.wants_b() {
            d.end_b();
        }
        if !moved || d.refused() {
            break;
        }
    }
    d.finish().map_err(|e| format!("{e:?}"))
}

#[test]
fn a_streamed_comparison_is_the_same_json_as_a_held_one() {
    for (name, a, b) in [
        (
            "identical",
            recording(11, None, 300),
            recording(11, None, 300),
        ),
        (
            "perturbed",
            recording(11, None, 300),
            recording(11, Some(90), 300),
        ),
        (
            "different seeds",
            recording(11, None, 300),
            recording(12, None, 300),
        ),
        (
            "one run stopped early",
            recording(11, None, 300),
            recording(11, None, 140),
        ),
    ] {
        let held = ferroscope_wasm::diff(&a, &b, 0.0, 0.0).expect("held diff");
        // Several block sizes, because nothing makes `File.slice()` land on a record boundary.
        for block in [4096usize, 65536, a.len().max(b.len())] {
            let s = streamed(&a, &b, block).unwrap_or_else(|e| panic!("{name} at {block}: {e}"));
            assert_eq!(
                held, s,
                "{name}: streamed diff differs at block size {block}"
            );
        }
    }
}

#[test]
fn a_streamed_comparison_handles_two_runs_that_share_no_channel() {
    // Not a refusal any more: samples are matched by STEP, so two runs that record entirely
    // different things pair up as nothing shared and everything a gap — which is what the held
    // comparison says too, and the only acceptable relationship between them is identity.
    let a = recording(11, None, 200);
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    for step in 0..200u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        rec.scalar("/something-else", t, step as f64, "m").unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", 11)
        .dt_ns(1_000_000)
        .steps(200)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("wasm-test");
    let b = rec.seal_with(spec, "test-platform", Vec::new).unwrap().0;
    let held = ferroscope_wasm::diff(&a, &b, 0.0, 0.0).expect("held diff");
    let s = streamed(&a, &b, 65536).expect("streamed diff");
    assert_eq!(held, s, "two runs sharing no channel compare differently");
}

#[test]
fn a_streamed_comparison_refuses_a_recording_whose_steps_run_backwards() {
    // What it still refuses. The walk matches a step's samples as a group and relies on both
    // files advancing; a file that goes backwards would have its samples skipped as unmatchable
    // while the held comparison, which matches on (channel, step) wherever they lie, would pair
    // them. Two answers to one question, so it declines to give one.
    let a = recording(11, None, 120);
    let mut rec = Recorder::new(Vec::new(), Precision::Quantized { drop_bits: 12 });
    for i in 0..120u64 {
        let step = 119 - i;
        let t = Stamp::at(step * 1_000_000, step * 1_010_000, step);
        rec.scalar("/err", t, step as f64, "m").unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", 11)
        .dt_ns(1_000_000)
        .steps(120)
        .integrator("semi-implicit-euler")
        .solver("none")
        .build("wasm-test");
    let b = rec.seal_with(spec, "test-platform", Vec::new).unwrap().0;
    let mut d = ferroscope_wasm::DiffStream::new(0.0, 0.0);
    for _ in 0..2 {
        d.push_a(&a);
        d.push_b(&b);
        d.rewind();
    }
    d.push_a(&a);
    d.push_b(&b);
    assert!(
        d.refused(),
        "the streamed comparison did not refuse a recording whose steps run backwards"
    );
}

#[test]
fn a_streamed_comparison_will_not_answer_after_too_few_passes() {
    // The receipt that says at what precision to hash is written at the END of the file, so a
    // comparison that read once has recomputed nothing and could only report a stored digest —
    // which is the exact defect this comparator was fixed for.
    let a = recording(11, None, 60);
    let b = recording(11, None, 60);
    let mut d = ferroscope_wasm::DiffStream::new(0.0, 0.0);
    d.push_a(&a);
    d.push_b(&b);
    assert_eq!(d.pass(), 1, "one push is not three passes");
    d.rewind();
    assert_eq!(d.pass(), 2, "rewind did not begin the second pass");
    d.push_a(&a);
    d.push_b(&b);
    d.rewind();
    assert_eq!(d.pass(), 3, "rewind did not begin the walk");
}

/// A recording carrying a mesh, the way `demo` and `urdf` do — mesh FIRST.
///
/// The order is the point, not decoration: in a recording this project writes, the glTF sits at
/// byte 1,543 of 1.8 MB, because geometry is declared before it moves. The first draft of this
/// fixture attached at the end and made the early-stop test fail for a reason that had nothing
/// to do with the code under test.
fn with_attachment(payload: &[u8]) -> Vec<u8> {
    let mut rec = Recorder::new(Vec::new(), Precision::Exact);
    rec.attach(
        "robot.glb",
        "model/gltf-binary",
        payload,
        Stamp::at(0, 0, 0),
    )
    .unwrap();
    for step in 0..4_000u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_000_000, step);
        rec.scalar("/err", t, step as f64, "m").unwrap();
    }
    let spec = ferroscope_receipt::RunSpec::new("spring", 1)
        .dt_ns(1_000_000)
        .steps(4_000)
        .integrator("none")
        .solver("none")
        .build("wasm-test");
    rec.seal_with(spec, "test-platform", Vec::new).unwrap().0
}

#[test]
fn a_mesh_can_be_pulled_from_a_recording_read_in_blocks() {
    // The last thing a block read could not do. A glTF mesh has to be WHOLE to be a mesh, so a
    // page that kept only the lanes has the geometry's declaration and not its bytes — and the
    // 3-D view fell back to primitives. Going back to the file for it is one more pass.
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let file = with_attachment(&payload);
    let held = ferroscope_wasm::attachment(&file, "robot.glb").expect("held attachment");
    assert_eq!(
        held, payload,
        "the fixture's own attachment does not round-trip"
    );

    // Every block size, because nothing makes `File.slice()` land on a record boundary and an
    // attachment is the one record big enough to span many blocks.
    for block in [1usize, 997, 65536, file.len()] {
        let mut s = ferroscope_wasm::AttachmentStream::new("robot.glb");
        for part in file.chunks(block) {
            if !s.push(part) {
                break;
            }
        }
        assert!(s.ready(), "the mesh was not found at block size {block}");
        assert_eq!(
            s.take().expect("streamed attachment"),
            payload,
            "the mesh differs when the bytes arrive {block} at a time"
        );
    }
}

#[test]
fn pulling_a_mesh_stops_at_the_attachment_rather_than_reading_on() {
    // Attachments are written near the front, so asking for one should not cost a whole pass
    // over a long recording. This is the difference between a mesh appearing and a page pausing.
    let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let file = with_attachment(&payload);
    let mut s = ferroscope_wasm::AttachmentStream::new("robot.glb");
    let mut fed = 0usize;
    for part in file.chunks(4096) {
        fed += part.len();
        if !s.push(part) {
            break;
        }
    }
    assert!(s.ready(), "the mesh was not found");
    assert!(
        fed * 2 < file.len(),
        "reading did not stop at the attachment: {fed} of {} bytes",
        file.len()
    );
}

#[test]
fn a_missing_mesh_names_what_the_recording_does_carry() {
    // "not found" sends a reader looking in the wrong place. What the file DOES carry is the
    // useful half of the answer, and the streamed path has to say it too.
    let file = with_attachment(b"glTF and then some");
    let mut s = ferroscope_wasm::AttachmentStream::new("nothing.glb");
    for part in file.chunks(4096) {
        if !s.push(part) {
            break;
        }
    }
    assert!(!s.ready(), "found an attachment that is not there");
}
