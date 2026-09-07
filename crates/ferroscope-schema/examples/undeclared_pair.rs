//! Two sealed recordings that DECLARE NOTHING, differing only in content.
//!
//! Every producer in this repository names the experiment it recorded, so this shape cannot be
//! made with the CLI — which is exactly why it needs a fixture. A `RunSpec::default()` has an
//! empty scenario, and two of them digest identically, so a comparator that only asks "do the
//! spec digests match" concludes these are two runs of one experiment. They are not: a matching
//! digest of nothing is not evidence of a shared experiment.
//!
//! ```sh
//! cargo run --example undeclared_pair -p ferroscope-schema -- ./out
//! ```

use ferroscope_receipt::{Precision, RunSpec};
use ferroscope_schema::{Recorder, Stamp};

fn write(path: &std::path::Path, slope: f64) {
    let mut rec = Recorder::new(Vec::new(), Precision::Exact);
    for step in 0..50u64 {
        let t = Stamp::at(step * 1_000_000, step * 1_000_000, step);
        rec.scalar("/x", t, slope * step as f64, "m").unwrap();
    }
    let bytes = rec.seal(RunSpec::default(), "fixture").expect("seal").0;
    std::fs::write(path, bytes).expect("write");
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).expect("mkdir");
    write(&dir.join("undeclared-a.mcap"), 1.0);
    write(&dir.join("undeclared-b.mcap"), 2.0);
    println!("wrote two recordings that name no experiment and differ in content");
}
