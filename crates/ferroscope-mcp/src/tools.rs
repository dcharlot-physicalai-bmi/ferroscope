//! What the agent can actually do.
//!
//! Every tool returns text a model can act on rather than a blob it has to guess at, and every
//! refusal says which field was wrong and what would have been right. That is the whole design
//! rule here: the caller is a model that will rewrite the document and try again, so an error
//! that does not say how to fix it costs a round trip for nothing.

use ferroscope_scene::Scene;
use ferroscope_schema::json::{self, Value};

/// The tool table, as a JSON array body (no brackets).
pub fn list() -> String {
    let t: Vec<String> = TOOLS
        .iter()
        .map(|(name, desc, schema)| {
            format!(
                r#"{{"name":{},"description":{},"inputSchema":{schema}}}"#,
                q(name),
                q(desc)
            )
        })
        .collect();
    t.join(",")
}

type Tool = (&'static str, &'static str, &'static str);

const TOOLS: &[Tool] = &[
    (
        "scene_schema",
        "The JSON schema for a scene, with defaults and a worked example. Read this before \
         writing a scene: it is the authority on shapes, motions, units and axes.",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "scene_from_text",
        "Turn an English phrase into a scene, and say what it understood, what it assumed, and \
         which words it could not use. Deterministic and offline — for open-ended language, \
         write the JSON yourself against scene_schema. Returns the scene for you to check or \
         edit before scene_record.",
        r#"{"type":"object","properties":{"text":{"type":"string","description":"e.g. \"drop three red crates from 2 m beside an SO-101 arm for 6 seconds\""}},"required":["text"]}"#,
    ),
    (
        "scene_validate",
        "Check a scene without recording it. Returns every problem at once, each with the JSON \
         path that was wrong, so one pass fixes the whole document.",
        r#"{"type":"object","properties":{"scene":{"type":"string","description":"The scene as a JSON string."}},"required":["scene"]}"#,
    ),
    (
        "scene_record",
        "Record a described scene to an MCAP file. Returns the determinism receipt, the energy \
         ledger, the lowest point anything reached, and a viewer URL. This is the tool that \
         turns a description into something you can look at.",
        r#"{"type":"object","properties":{"scene":{"type":"string","description":"The scene as a JSON string."},"out":{"type":"string","description":"Path to write the .mcap to."}},"required":["scene","out"]}"#,
    ),
    (
        "scene_sweep",
        "Run a scene over a grid of cases and judge each one. Add \"cases\" (name -> list of \
         numbers), write {\"$\": \"name\"} anywhere a number belongs, and \"checks\" with a \
         measure and a bound. Returns a verdict per case with the number that decided it. This \
         is the scenario, not the single run.",
        r#"{"type":"object","properties":{"scene":{"type":"string","description":"The scene as a JSON string, with cases and checks."},"out":{"type":"string","description":"Optional path stem; each case is written to <stem>-<i>.mcap."}},"required":["scene"]}"#,
    ),
    (
        "robot_check",
        "Read a URDF and report whether it is physically usable: links the renderer draws that \
         the engine cannot touch, movable links with no inertial, impossible inertia tensors, \
         and meshes referenced but not present.",
        r#"{"type":"object","properties":{"urdf":{"type":"string","description":"Path to a .urdf file."}},"required":["urdf"]}"#,
    ),
    (
        "mesh_check",
        "Read an STL and report what it is: triangle count, bounds, whether it closes, and the \
         volume, centre of mass and inertia tensor computed from the geometry itself. Give it a \
         material to get the mass it would have.",
        r#"{"type":"object","properties":{"stl":{"type":"string","description":"Path to a .stl file, binary or ASCII."},"material":{"type":"string","description":"Optional material id, e.g. \"6061-T6\" or \"PLA\"."}},"required":["stl"]}"#,
    ),
    (
        "materials_search",
        "Search the material table. Every hit carries density, yield strength, modulus and the \
         source it is cited from. Use it to pick the material id for mesh_check or a scene body.",
        r#"{"type":"object","properties":{"query":{"type":"string","description":"Free text, e.g. \"aluminium\", \"PLA\", \"titanium\"."},"limit":{"type":"integer","default":12}},"required":["query"]}"#,
    ),
    (
        "run_inspect",
        "What is in a recording: topics, schemas, message counts, time span, and its receipt.",
        r#"{"type":"object","properties":{"run":{"type":"string","description":"Path to a .mcap file."}},"required":["run"]}"#,
    ),
    (
        "run_verify",
        "Recompute a recording's receipt from its own bytes and say whether it still stands \
         behind it. Needs nothing but the file.",
        r#"{"type":"object","properties":{"run":{"type":"string"}},"required":["run"]}"#,
    ),
    (
        "run_energy",
        "The energy ledger for a run: E_task = E_compute + E_actuation, with a coverage verdict \
         that refuses to quote a number it cannot stand behind.",
        r#"{"type":"object","properties":{"run":{"type":"string"}},"required":["run"]}"#,
    ),
    (
        "run_diff",
        "Compare two recordings and say whether the second reproduced the first, and if not, at \
         which step and on which topic they parted.",
        r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},"required":["a","b"]}"#,
    ),
];

pub fn call(name: &str, args: &Value) -> String {
    let r = match name {
        "scene_schema" => Ok(Scene::SCHEMA.to_string()),
        "scene_from_text" => scene_from_text(args),
        "scene_validate" => scene_validate(args),
        "scene_record" => scene_record(args),
        "scene_sweep" => scene_sweep(args),
        "robot_check" => robot_check(args),
        "mesh_check" => mesh_check(args),
        "materials_search" => materials_search(args),
        "run_inspect" => run_inspect(args),
        "run_verify" => run_verify(args),
        "run_energy" => run_energy(args),
        "run_diff" => run_diff(args),
        other => Err(format!(
            "no such tool: {other:?}. Available: {}",
            TOOLS.iter().map(|t| t.0).collect::<Vec<_>>().join(", ")
        )),
    };
    match r {
        Ok(text) => content(&text, false),
        // A tool failure is reported inside the result with isError, not as a JSON-RPC error:
        // the call succeeded, the work did not, and the model needs to read why.
        Err(e) => content(&e, true),
    }
}

fn content(text: &str, is_error: bool) -> String {
    format!(
        r#"{{"content":[{{"type":"text","text":{}}}],"isError":{is_error}}}"#,
        q(text)
    )
}

fn q(s: &str) -> String {
    let mut out = String::new();
    json::write_string(&mut out, s);
    out
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing required argument {key:?}"))
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))
}

// ---------------------------------------------------------------------------

fn parse_scene(args: &Value) -> Result<Scene, String> {
    let text = arg(args, "scene")?;
    Scene::parse(text).map_err(|problems| {
        let mut s = format!("{} problem(s) in this scene:\n", problems.len());
        for p in &problems {
            s.push_str(&format!("  {}: {}\n", p.path, p.message));
        }
        s.push_str("\nCall scene_schema for the full shape, defaults and a worked example.");
        s
    })
}

fn scene_from_text(args: &Value) -> Result<String, String> {
    let text = arg(args, "text")?;
    let r = ferroscope_phrase::read(text).map_err(|e| e.to_string())?;
    let mut out = format!("\"{text}\"\n");
    for u in &r.understood {
        out.push_str(&format!("  understood   {u}\n"));
    }
    for a in &r.assumed {
        out.push_str(&format!("  assumed      {a}\n"));
    }
    if !r.ignored.is_empty() {
        // Reported, never dropped in silence: a scene quietly stripped of half the sentence is
        // a scene the caller cannot tell apart from one that worked.
        out.push_str(&format!(
            "  NOT USED     {} — no meaning in the scene vocabulary\n",
            r.ignored
                .iter()
                .map(|w| format!("{w:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!(
        "\n{}\n\nCheck it, edit anything that is wrong, then pass it to scene_record.",
        r.scene_json
    ));
    Ok(out)
}

fn scene_validate(args: &Value) -> Result<String, String> {
    let s = parse_scene(args)?;
    Ok(format!(
        "This scene is valid.\n  name        {}\n  duration    {} s at {} Hz ({} steps)\n  \
         bodies      {}\n  robots      {}\n\nRecord it with scene_record.",
        s.name,
        s.duration_s,
        s.rate_hz,
        s.steps(),
        s.bodies
            .iter()
            .map(|b| format!("{} ({})", b.id, b.motion.describe()))
            .collect::<Vec<_>>()
            .join(", "),
        if s.robots.is_empty() {
            "none".into()
        } else {
            s.robots
                .iter()
                .map(|r| format!("{} from {}", r.id, r.urdf))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ))
}

fn scene_record(args: &Value) -> Result<String, String> {
    let s = parse_scene(args)?;
    let out_path = arg(args, "out")?;
    let mut meter = ferroscope_power::Meter::open();
    let _ = meter.sample_energy(); // prime, so the note covers the whole recording
    let mut note: Vec<(String, String)> = Vec::new();
    let rec = s.record_with(
        |p| std::fs::read_to_string(p).ok(),
        || {
            note = meter.production_note();
            note.clone()
        },
    )?;
    std::fs::write(out_path, &rec.bytes).map_err(|e| format!("cannot write {out_path}: {e}"))?;

    let mut r = format!(
        "Recorded {} to {out_path} ({} bytes, {} steps).\n\n\
         RECEIPT\n  spec digest   {}\n  trace digest  {}\n  precision     {}\n\n\
         ENERGY\n  E_task        {:.2} J estimated ({:.1} % compute)\n",
        s.name,
        rec.bytes.len(),
        rec.steps,
        rec.receipt.spec_digest,
        rec.receipt.trace_digest,
        rec.receipt.precision,
        rec.total_j,
        rec.compute_fraction * 100.0,
    );
    {
        let get = |k: &str| note.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        match (get("joules"), get("unavailable")) {
            (Some(j), _) => r.push_str(&format!(
                "\nPRODUCTION\n  this machine spent {j} J making the file ({})\n",
                get("source").unwrap_or("?")
            )),
            (_, Some(why)) => r.push_str(&format!("\nPRODUCTION\n  not measured: {why}\n")),
            _ => {}
        }
    }
    if let Some((z, who)) = &rec.lowest {
        r.push_str(&format!(
            "\nCLEARANCE\n  lowest point  {z:.4} m ({who}){}\n",
            if *z < -1e-6 {
                "  -- BELOW THE GROUND PLANE"
            } else {
                ""
            }
        ));
    }
    if !rec.notes.is_empty() {
        r.push_str("\nNOTES\n");
        for n in &rec.notes {
            r.push_str(&format!("  {n}\n"));
        }
    }
    r.push_str(
        "\nOpen it at https://ferroscope.physicalai-bmi.org/viewer and drop the file on slot A. \
         Nothing is uploaded; the parser runs in the tab.",
    );
    Ok(r)
}

fn scene_sweep(args: &Value) -> Result<String, String> {
    let text = arg(args, "scene")?;
    let suite = ferroscope_scene::Suite::parse(text).map_err(|problems| {
        let mut s = format!("{} problem(s) in this suite:\n", problems.len());
        for p in &problems {
            s.push_str(&format!("  {}: {}\n", p.path, p.message));
        }
        s.push_str("\nCall scene_schema for the scene format; cases and checks are described in scene_sweep's own schema.");
        s
    })?;
    let mut meter = ferroscope_power::Meter::open();
    let _ = meter.sample_energy(); // prime: each case's note is the delta since the previous one
    let results = suite.run_with(|p| std::fs::read_to_string(p).ok(), &mut || {
        meter.production_note()
    })?;
    let failed = results.iter().filter(|r| !r.passed()).count();

    let mut out = format!(
        "{}\n  {} case(s), {} check(s) each\n\n",
        suite.name,
        results.len(),
        suite.checks.len()
    );
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "  {:<26} {:>7} steps {:>9.2} J  {}\n",
            r.label,
            r.recorded.steps,
            r.recorded.total_j,
            if r.passed() { "pass" } else { "FAIL" }
        ));
        // The measured number on a pass as well as a failure: a column of "pass" with no numbers
        // is a result nobody can sanity-check.
        for (name, ok, why) in &r.checks {
            out.push_str(&format!(
                "      {} {name}: {why}\n",
                if *ok { "ok  " } else { "FAIL" }
            ));
        }
        if let Some(stem) = args.get("out").and_then(|x| x.as_str()) {
            let file = format!("{}-{i}.mcap", stem.strip_suffix(".mcap").unwrap_or(stem));
            std::fs::write(&file, &r.recorded.bytes)
                .map_err(|e| format!("cannot write {file}: {e}"))?;
        }
    }
    out.push_str(&format!(
        "\n{} passed, {failed} failed.",
        results.len() - failed
    ));
    if failed > 0 {
        out.push_str(" The number beside each FAIL is what decided it.");
    }
    Ok(out)
}

fn robot_check(args: &Value) -> Result<String, String> {
    let path = arg(args, "urdf")?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let robot = ferroscope_urdf::Robot::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let findings = robot.check();
    let failures = findings.iter().filter(|f| f.fails).count();
    let mut r = format!(
        "{path}\n  robot       {}\n  root link   {}\n  links       {}\n  joints      {} total, \
         {} movable\n\n",
        robot.name,
        robot.root_link().unwrap_or("?"),
        robot.links.len(),
        robot.joints.len(),
        robot.movable_joints().count()
    );
    if findings.is_empty() {
        r.push_str("CHECKS  all clear\n");
    } else {
        r.push_str("CHECKS\n");
        for f in &findings {
            r.push_str(&format!(
                "  {} {:<22} {:<26} {}\n",
                if f.fails { "FAIL" } else { "note" },
                f.kind,
                f.link,
                f.detail
            ));
        }
        r.push_str(&format!(
            "  {} finding(s), {failures} that fail\n",
            findings.len()
        ));
    }
    if failures > 0 {
        r.push_str("\nThis description is not physically usable as written.");
    }
    Ok(r)
}

fn mesh_check(args: &Value) -> Result<String, String> {
    let path = arg(args, "stl")?;
    let bytes = read(path)?;
    let mesh = ferroscope_mesh::stl::read(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let (tight, open) = mesh.is_watertight();
    let (lo, hi) = mesh.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    let p = mesh.mass_properties(1.0);
    let mut r = format!(
        "{path}\n  triangles   {}\n  vertices    {}\n  bounds      [{:.4} {:.4} {:.4}] to \
         [{:.4} {:.4} {:.4}] m\n  closed      {}\n  degenerate  {} face(s)\n  volume      \
         {:.3} cm3\n  centroid    [{:.4} {:.4} {:.4}] m\n",
        mesh.triangles(),
        mesh.positions.len(),
        lo[0],
        lo[1],
        lo[2],
        hi[0],
        hi[1],
        hi[2],
        if tight {
            "yes".to_string()
        } else {
            format!("NO: {open} unmatched edge(s), so the volume below is not well defined")
        },
        mesh.degenerate_faces(),
        p.volume * 1e6,
        p.centroid[0],
        p.centroid[1],
        p.centroid[2],
    );
    if let Some(id) = args.get("material").and_then(|m| m.as_str()) {
        match ferroscope_cad::mass_properties(&mesh, id) {
            Some(m) => {
                let i = m.value.urdf_inertia();
                r.push_str(&format!(
                    "\nAS {id} ({:?} tier, {})\n  density     {:.0} kg/m3\n  mass        \
                     {:.4} kg\n  inertia     ixx={:.4e} ixy={:.4e} ixz={:.4e}\n              \
                     iyy={:.4e} iyz={:.4e} izz={:.4e}\n\nThat is the <inertial> block this \
                     geometry implies, about its centre of mass.",
                    m.provenance.tier,
                    m.provenance.source,
                    m.value.mass / m.value.volume,
                    m.value.mass,
                    i[0],
                    i[1],
                    i[2],
                    i[3],
                    i[4],
                    i[5]
                ));
            }
            None => r.push_str(&format!(
                "\n{id:?} is not in the table of {} materials. Use materials_search to find one.",
                ferroscope_cad::material_count()
            )),
        }
    }
    Ok(r)
}

fn materials_search(args: &Value) -> Result<String, String> {
    let query = arg(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_f64())
        .unwrap_or(12.0)
        .clamp(1.0, 100.0) as usize;
    let hits: Vec<_> = ferroscope_cad::search(query).take(limit).collect();
    if hits.is_empty() {
        return Ok(format!(
            "Nothing matching {query:?} in the table of {} materials. Try a broader term \
             (\"steel\", \"aluminium\", \"nylon\") or a standard designation.",
            ferroscope_cad::material_count()
        ));
    }
    let mut r = format!(
        "{} of {} materials match {query:?}:\n\n{:<18} {:<30} {:>9} {:>10} {:>9}  SOURCE\n",
        hits.len(),
        ferroscope_cad::material_count(),
        "ID",
        "NAME",
        "kg/m3",
        "YIELD MPa",
        "E GPa"
    );
    for m in hits {
        r.push_str(&format!(
            "{:<18} {:<30} {:>9.0} {:>10.0} {:>9.0}  {}\n",
            m.id,
            m.name,
            m.density.value(),
            m.yield_strength.value() / 1e6,
            m.elastic_modulus.value() / 1e9,
            m.source
        ));
    }
    Ok(r)
}

fn run_inspect(args: &Value) -> Result<String, String> {
    let path = arg(args, "run")?;
    let bytes = read(path)?;
    let log = ferroscope_schema::mcap::read(&bytes).map_err(|e| e.to_string())?;
    let mut r = format!(
        "{path}\n  profile     {}\n  library     {}\n  messages    {}\n",
        log.profile,
        log.library,
        log.messages.len()
    );
    if let Some((t0, t1)) = log.time_span() {
        r.push_str(&format!("  sim span    {:.3} s\n", (t1 - t0) as f64 * 1e-9));
    }
    r.push_str(&format!("\n  {:<34} {:>8}  SCHEMA\n", "TOPIC", "MSGS"));
    let mut rows: Vec<(String, usize, String)> = log
        .channels
        .iter()
        .map(|c| {
            let n = log.messages.iter().filter(|m| m.channel_id == c.id).count();
            let s = log
                .schema(c.schema_id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "(none)".into());
            (c.topic.clone(), n, s)
        })
        .collect();
    rows.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (topic, n, schema) in rows.iter().take(40) {
        r.push_str(&format!("  {topic:<34} {n:>8}  {schema}\n"));
    }
    if rows.len() > 40 {
        r.push_str(&format!("  ... and {} more topics\n", rows.len() - 40));
    }
    if let Some(kv) = log.metadata_block(ferroscope_schema::PRODUCTION_BLOCK) {
        r.push_str("\n  production\n");
        for (k, v) in kv {
            r.push_str(&format!("    {k:<22} {v}\n"));
        }
    }
    Ok(r)
}

fn run_verify(args: &Value) -> Result<String, String> {
    let path = arg(args, "run")?;
    // Recomputing a receipt is a fold, so it reads the file rather than holding it.
    let v = ferroscope_schema::verify_streaming(|| std::fs::File::open(path))
        .ok_or_else(|| format!("{path} carries no Ferroscope receipt"))?;
    Ok(format!(
        "{path}\n  scenario      {}\n  precision     {}\n  platform      {}\n  messages      \
         {}\n\n  spec digest   {}  {}\n  trace digest  {}  {}\n\n{}",
        v.receipt.spec.scenario,
        v.receipt.precision,
        v.receipt.platform,
        v.messages,
        v.receipt.spec_digest,
        if v.spec_matches { "ok" } else { "MISMATCH" },
        v.receipt.trace_digest,
        if v.trace_matches { "ok" } else { "MISMATCH" },
        if v.spec_matches && v.trace_matches {
            "VERIFIED: this file still stands behind its own receipt."
        } else {
            "REFUSED: the file's contents no longer hash to the receipt it carries."
        }
    ))
}

fn run_energy(args: &Value) -> Result<String, String> {
    let path = arg(args, "run")?;
    // The ledger is a fold too: total each sample as it goes past.
    let v = ferroscope_schema::verify_streaming(|| std::fs::File::open(path))
        .ok_or_else(|| format!("{path} is not a Ferroscope recording"))?;
    let q = &v.quote;
    let mut r = format!(
        "{path}\n\n  E_task = E_compute + E_actuation\n  compute       {:>10.3} J   {:>5.1} %\n  \
         actuation     {:>10.3} J\n  overhead      {:>10.3} J\n  total         {:>10.3} J\n  \
         duration      {:>10.3} s\n  mean power    {:>10.3} W\n  peak          {:>10.3} W  ({})\n",
        q.compute_j,
        q.compute_fraction() * 100.0,
        q.actuation_j,
        q.overhead_j,
        q.total_j,
        q.duration_s,
        q.mean_power_w(),
        q.peak_w,
        q.peak_source,
    );
    if !q.by_source.is_empty() {
        r.push_str(&format!(
            "\n  {:<10} {:<18} {:>12}\n",
            "RAIL", "SOURCE", "JOULES"
        ));
        for (rail, name, j) in q.by_source.iter().take(12) {
            r.push_str(&format!("  {rail:?}{:<4} {name:<18} {j:>12.3}\n", ""));
        }
    }
    if let Some(kv) = std::fs::File::open(path)
        .ok()
        .and_then(|f| ferroscope_schema::metadata_streaming(f, ferroscope_schema::PRODUCTION_BLOCK))
    {
        r.push_str("\n  PRODUCTION (what the producing machine spent making this file)\n");
        for (k, v) in kv {
            r.push_str(&format!("    {k:<22} {v}\n"));
        }
    }
    r.push_str(&format!("\n  coverage      {}\n", q.coverage));
    if !q.quotable {
        r.push_str(
            "  DO NOT QUOTE: the sampling cannot support this number. It is reported so it is \
             not silently missing, but it must not be cited as a measurement.\n",
        );
    }
    Ok(r)
}

fn run_diff(args: &Value) -> Result<String, String> {
    let a = arg(args, "a")?;
    let b = arg(args, "b")?;
    // Read, do not hold. An agent may be pointed at recordings far larger than whatever it is
    // running inside, and comparing two runs is a fold over pairs in file order — so the floor
    // is what the report keeps, not the two files.
    let open_a = || std::fs::File::open(a);
    let open_b = || std::fs::File::open(b);
    let ra = ferroscope_schema::receipt_streaming(
        open_a().map_err(|e| format!("cannot read {a}: {e}"))?,
    );
    let rb = ferroscope_schema::receipt_streaming(
        open_b().map_err(|e| format!("cannot read {b}: {e}"))?,
    );

    // Recompute both receipts before answering. An agent asking "did this reproduce?" and
    // getting a yes has no way to tell that nothing checked whether either file still stands
    // behind its own digest, and this tool answers exactly that question.
    let va = ferroscope_schema::verify_streaming(open_a);
    let vb = ferroscope_schema::verify_streaming(open_b);
    let ok = |v: &Option<ferroscope_schema::Verification>| v.as_ref().is_some_and(|v| v.ok());
    let (a_ok, b_ok) = (ok(&va), ok(&vb));
    let trustworthy = a_ok && b_ok;

    let tol = ferroscope_receipt::Tolerance::default();
    // The lockstep walk refuses any pair whose samples do not line up rather than guessing at
    // which sample belongs with which; when it does, build both trajectories instead.
    let p = match ferroscope_schema::profile_streaming(open_a, open_b, tol) {
        Some(p) => p,
        None => {
            let read_trace = |path: &str, f: std::io::Result<std::fs::File>| {
                f.ok()
                    .and_then(ferroscope_schema::trace_from_streaming)
                    .map(|(_, t)| t)
                    .ok_or_else(|| format!("{path} is not a Ferroscope recording"))
            };
            let ta = read_trace(a, open_a())?;
            let tb = read_trace(b, open_b())?;
            ferroscope_receipt::profile(&ta, &tb, tol)
        }
    };
    let v = &p.verdict;

    let mut out = format!(
        "A  {a}  {}\nB  {b}  {}\n\n",
        if a_ok { "VERIFIED" } else { "DOES NOT VERIFY" },
        if b_ok { "VERIFIED" } else { "DOES NOT VERIFY" },
    );
    if !trustworthy {
        out.push_str(
            "  A file that fails its own receipt cannot support a reproduction verdict. What \
             follows compares bytes, not evidence.\n\n",
        );
    }
    if let (Some(x), Some(y)) = (&ra, &rb) {
        let diffs = ferroscope_receipt::spec_differences(&x.spec, &y.spec);
        if !diffs.is_empty() {
            out.push_str("  the specs differ, so these are not two runs of one experiment:\n");
            for d in diffs.iter().take(6) {
                out.push_str(&format!(
                    "    {} : {} -> {}  ({})\n",
                    d.field, d.a, d.b, d.means
                ));
            }
            out.push('\n');
        }
    }
    if p.structural.is_some() {
        out.push_str(&format!("  on what both runs recorded: {v}\n"));
    } else {
        out.push_str(&format!("  verdict     {v}\n"));
    }
    if let Some((ch, step)) = &p.onset {
        out.push_str(&format!(
            "  onset       step {step} on {ch} - where the bits first parted, which is the \
             causal step; the crossing below moves whenever the tolerance does\n"
        ));
    }
    if let Some(c) = p.crossing {
        out.push_str(&format!("  crossing    step {c}\n"));
    }
    if let Some(d) = p.dominant() {
        out.push_str(&format!("  shape       {}\n", d.shape));
    }
    if !p.channels.is_empty() {
        out.push_str(
            "  extent      channels ranked by |delta| against each channel's own scale, which a \
             zero crossing cannot inflate - they are not independent, so this is a ranking and \
             not a count of separate faults:\n",
        );
        for c in p.channels.iter().take(6) {
            out.push_str(&format!(
                "    {} delta/scale {:.3e} at step {} (onset {}, pointwise rel {:.3e})\n",
                c.channel, c.worst_scaled, c.worst_scaled_step, c.onset_step, c.worst_rel
            ));
        }
    }
    if let Some(st) = &p.structural {
        out.push_str(&format!(
            "  coverage    A ends at step {}, B at step {}; the verdict covers the {} sample(s) \
             both runs recorded and says nothing about the {} excluded\n",
            st.a_last_step, st.b_last_step, st.shared_samples, st.excluded_samples
        ));
    }
    if let (Some(x), Some(y)) = (&va, &vb)
        // Both quotes must be admissible: a gap in either ledger can hide the whole difference.
        && x.quote.quotable
        && y.quote.quotable
    {
        out.push_str(&format!(
            "  energy      A {:.3} J   B {:.3} J   delta {:+.6} J\n",
            x.quote.total_j,
            y.quote.total_j,
            y.quote.total_j - x.quote.total_j
        ));
    }
    out.push_str(&format!(
        "\n  {}\n",
        if !trustworthy {
            "No reproduction claim can be made: at least one file fails its own receipt."
        } else if v.reproduced() {
            "B reproduced A, and both files still stand behind their own receipts."
        } else {
            "B did not reproduce A. The lines above name where they parted and how it grew."
        }
    ));
    Ok(out)
}
