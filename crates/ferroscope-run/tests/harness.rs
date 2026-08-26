//! What the harness must get right, stated as the failure each test prevents.

use std::path::PathBuf;

use ferroscope_run::prelude::*;
use ferroscope_run::{CASE_CAP, Case, Clause, Outcome, Query, Selection, Store};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fsrun-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A tiny deterministic body: integrate a constant and burn a constant wattage.
fn body(run: &mut Run) -> Result<(), Halt> {
    let gain = run.param("gain");
    let mut x = 0.0;
    while run.running() {
        let t = run.tick();
        x += gain * run.dt();
        run.position("/x", t, [x, 0.0, 0.0]);
        run.energy("/e", t, Rail::Compute, "soc", 10.0);
    }
    run.result("final_x", x);
    run.check("advanced", x > 0.0, format!("x = {x:.6}"));
    Ok(())
}

fn harness(root: &PathBuf) -> Harness {
    Harness::new().store(root).scenario(
        Scenario::new("ramp")
            .tags(["smoke"])
            .param("gain", 1.0)
            .steps(100)
            .dt(0.001)
            .cases(Case::sweep("gain", [1.0, 2.0]))
            .body(body),
    )
}

#[test]
fn a_run_records_a_verdict_a_receipt_and_a_cost() {
    let root = tmp("basic");
    let recs = harness(&root).execute(&Selection::default()).unwrap();
    assert_eq!(recs.len(), 2, "two swept cases");
    for r in &recs {
        assert_eq!(r.outcome, Outcome::Passed);
        assert_eq!(r.checks.len(), 1);
        assert!(r.verified, "the stored recording must verify at write time");
        assert!(!r.spec_digest.is_empty() && !r.trace_digest.is_empty());
        // 10 W over 99 ms of intervals.
        assert!((r.energy_j - 0.99).abs() < 1e-6, "got {}", r.energy_j);
        assert!(r.quotable);
        assert!(r.wall_s > 0.0);
        assert_eq!(r.steps, 100);
        assert!((r.sim_s - 0.1).abs() < 1e-9);
    }
    // The cost figure Antioch's own docs say the platform cannot produce.
    assert!(recs[0].joules_per_pass().unwrap() > 0.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failing_check_fails_the_run_without_stopping_it() {
    let root = tmp("failcheck");
    let recs = Harness::new()
        .store(&root)
        .scenario(Scenario::new("four_gates").steps(1).body(|run: &mut Run| {
            run.tick();
            run.check("a", true, "1");
            run.check("b", false, "2 > 1");
            run.check("c", false, "3 > 1");
            run.check("d", true, "4");
            Ok(())
        }))
        .execute(&Selection::default())
        .unwrap();
    let r = &recs[0];
    assert_eq!(r.outcome, Outcome::Failed);
    assert_eq!(
        r.checks.len(),
        4,
        "a chained assert would have hidden the other three measurements"
    );
    assert_eq!(r.failed_checks().count(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn re_checking_a_criterion_replaces_it_in_place() {
    let root = tmp("recheck");
    let recs = Harness::new()
        .store(&root)
        .scenario(Scenario::new("loop").steps(5).body(|run: &mut Run| {
            while run.running() {
                let s = run.step();
                run.tick();
                run.check("settled", s >= 3, format!("step {s}"));
            }
            Ok(())
        }))
        .execute(&Selection::default())
        .unwrap();
    let r = &recs[0];
    assert_eq!(r.checks.len(), 1, "a per-step check costs one row");
    assert!(
        r.checks[0].passed,
        "the last verdict is the one that stands"
    );
    assert_eq!(r.checks[0].detail, "step 4");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn skip_is_not_failure_and_a_panic_is_not_a_verdict() {
    let root = tmp("halts");
    let recs = Harness::new()
        .store(&root)
        .scenario(
            Scenario::new("skipped").body(|run: &mut Run| Err(run.skip("no hardware attached"))),
        )
        .scenario(Scenario::new("panicked").body(|_run: &mut Run| {
            panic!("the rig exploded");
        }))
        .scenario(
            Scenario::new("halted").body(|run: &mut Run| Err(run.fail("policy file missing"))),
        )
        .execute(&Selection::default())
        .unwrap();

    let by = |name: &str| recs.iter().find(|r| r.scenario == name).unwrap();
    assert_eq!(by("skipped").outcome, Outcome::Skipped);
    assert_eq!(by("skipped").reason, "no hardware attached");
    // A defect in the run is reported as ERRORED, distinct from a verdict on the task, and it
    // does not take the rest of the selection down with it.
    assert_eq!(by("panicked").outcome, Outcome::Errored);
    assert!(by("panicked").reason.contains("the rig exploded"));
    assert_eq!(by("halted").outcome, Outcome::Failed);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_run_with_no_checks_says_so_rather_than_claiming_success() {
    let root = tmp("nochecks");
    let recs = Harness::new()
        .store(&root)
        .scenario(Scenario::new("silent").steps(1).body(|run: &mut Run| {
            run.tick();
            Ok(())
        }))
        .execute(&Selection::default())
        .unwrap();
    // It passes, faithfully to Antioch's rule, but the record carries zero checks so a reader
    // can tell the difference between "the task succeeded" and "the code did not crash".
    assert_eq!(recs[0].outcome, Outcome::Passed);
    assert!(recs[0].checks.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_same_case_run_twice_produces_the_same_digests() {
    let root = tmp("determinism");
    let a = harness(&root).execute(&Selection::default()).unwrap();
    let b = harness(&root).execute(&Selection::default()).unwrap();
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.spec_digest, y.spec_digest);
        assert_eq!(
            x.trace_digest, y.trace_digest,
            "a deterministic body must hash the same twice"
        );
        assert_ne!(x.id, y.id, "but the runs are distinct records");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_changed_input_moves_the_spec_digest() {
    let root = tmp("specmove");
    let recs = harness(&root).execute(&Selection::default()).unwrap();
    assert_ne!(
        recs[0].spec_digest, recs[1].spec_digest,
        "gain=1 and gain=2 are not the same experiment"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn selection_narrows_and_suites_union_in_authored_order() {
    let root = tmp("select");
    let h = Harness::new()
        .store(&root)
        .scenario(
            Scenario::new("alpha")
                .tags(["fast"])
                .steps(1)
                .body(|r: &mut Run| {
                    r.tick();
                    Ok(())
                }),
        )
        .scenario(
            Scenario::new("beta")
                .tags(["slow"])
                .steps(1)
                .cases([Case::new("one"), Case::new("two").tag("fast")])
                .body(|r: &mut Run| {
                    r.tick();
                    Ok(())
                }),
        )
        .suite(Suite::new("fast").tags(["fast"]))
        .suite(Suite::new("mixed").tags(["fast"]).clause(Clause {
            scenarios: vec!["beta".into()],
            cases: vec!["one".into()],
            ..Default::default()
        }));

    assert_eq!(h.collect(&Selection::default()).len(), 3);
    let only_beta = Selection {
        scenarios: vec!["beta".into()],
        ..Default::default()
    };
    assert_eq!(h.collect(&only_beta).len(), 2);
    let no_slow = Selection {
        exclude_tags: vec!["slow".into()],
        ..Default::default()
    };
    assert_eq!(h.collect(&no_slow).len(), 1, "exclude-tag wins over tag");

    // A case tag selects, not only a scenario tag.
    assert_eq!(h.collect_suite("fast").unwrap().len(), 2);
    // Clauses union without duplicating the overlap.
    assert_eq!(h.collect_suite("mixed").unwrap().len(), 3);
    match h.collect_suite("nope") {
        Err(e) => assert!(e.contains("declared"), "got {e}"),
        Ok(_) => panic!("an undeclared suite must be refused, with the declared names listed"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_ad_hoc_override_reaches_the_body() {
    let root = tmp("set");
    let sel = Selection {
        scenarios: vec!["ramp".into()],
        cases: vec!["gain=1".into()],
        set: vec![("gain".into(), 7.0)],
        ..Default::default()
    };
    let recs = harness(&root).execute(&sel).unwrap();
    assert_eq!(recs.len(), 1);
    let x = recs[0]
        .results
        .iter()
        .find(|(k, _)| k == "final_x")
        .unwrap()
        .1
        .as_f64()
        .unwrap();
    // 7.0 * 100 steps * 1 ms.
    assert!((x - 0.7).abs() < 1e-9, "got {x}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_case_cap_refuses_a_grid_and_names_the_count() {
    let ok = Case::grid(&[
        ("a", (0..40).map(|i| i as f64).collect()),
        ("b", (0..40).map(|i| i as f64).collect()),
    ]);
    assert_eq!(ok.unwrap().len(), 1600);
    let too_big = Case::grid(&[
        ("a", (0..50).map(|i| i as f64).collect()),
        ("b", (0..50).map(|i| i as f64).collect()),
    ]);
    let e = too_big.unwrap_err();
    assert!(
        e.contains("2500") && e.contains(&CASE_CAP.to_string()),
        "got {e}"
    );
}

#[test]
fn history_is_a_directory_that_survives_the_process() {
    let root = tmp("history");
    let written = harness(&root).execute(&Selection::default()).unwrap();

    // A fresh Store, as a later process or a different tool would open it.
    let store = Store::open(&root).unwrap();
    let mut q = Query::new();
    q.results = vec![ferroscope_run::Predicate::parse("final_x:>:0.15").unwrap()];
    let hits = store.list(&q);
    assert_eq!(hits.len(), 1, "gain=2 reaches 0.2, gain=1 only 0.1");
    assert_eq!(hits[0].case, "gain=2");

    // The recording is beside the record and still verifies from its own bytes.
    let bytes = store.recording(&written[0].id).unwrap();
    let v = ferroscope_schema::verify(&bytes).expect("receipt present");
    assert!(v.ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_artifact_is_copied_in_rather_than_referenced() {
    let root = tmp("artifact");
    let scratch = std::env::temp_dir().join(format!("fsrun-scratch-{}.txt", std::process::id()));
    std::fs::write(&scratch, b"episode,reward\n0,1.0\n").unwrap();
    let path = scratch.clone();
    let recs = Harness::new()
        .store(&root)
        .scenario(
            Scenario::new("with_artifact")
                .steps(1)
                .body(move |run: &mut Run| {
                    run.tick();
                    run.artifact("episodes.csv", path.clone());
                    Ok(())
                }),
        )
        .execute(&Selection::default())
        .unwrap();

    assert_eq!(recs[0].artifacts, vec!["episodes.csv".to_string()]);
    // Delete the original: the artifact must still be there, which is the whole point.
    std::fs::remove_file(&scratch).unwrap();
    let copied = root.join(&recs[0].id).join("episodes.csv");
    assert!(
        copied.is_file(),
        "an artifact that lives elsewhere goes missing"
    );
    assert!(std::fs::read_to_string(copied).unwrap().contains("reward"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_three_clocks_are_all_distinct_and_recorded() {
    let root = tmp("clocks");
    let recs = harness(&root).execute(&Selection::default()).unwrap();
    let r = &recs[0];
    assert!((r.sim_s - 0.1).abs() < 1e-9, "simulated time is exact");
    assert!(r.wall_s > 0.0, "wall time is measured");
    assert_eq!(r.steps, 100, "the control step index is counted");
    assert!(
        r.real_time_factor() > 1.0,
        "a 100-step ramp beats real time; got {}",
        r.real_time_factor()
    );
    let _ = std::fs::remove_dir_all(&root);
}
