//! Write the SAME recording three ways — uncompressed, zstd, lz4 — using the reference
//! implementation.
//!
//! These are the fixtures for the browser codec gate. They have to come from the reference
//! writer rather than from us, because the thing under test is whether we read what OTHER tools
//! produce; a file we compressed ourselves would only prove our decoder matches our encoder.
//!
//! Payloads are `ferroscope.Scalar` so the viewer renders them as series, and carry a `step`, so
//! the trace has distinct steps rather than collapsing onto zero.
//!
//! ```sh
//! cargo run --example make_compressed_fixtures -p ferroscope-mcap -- ./out
//! ```

fn write(path: &std::path::Path, compression: Option<mcap::Compression>) {
    let f = std::fs::File::create(path).expect("create");
    let mut w = mcap::WriteOptions::new()
        .compression(compression)
        .create(std::io::BufWriter::new(f))
        .expect("writer");
    let s = w
        .add_schema("ferroscope.Scalar", "jsonschema", br#"{"type":"object"}"#)
        .expect("schema");
    let c = w
        .add_channel(s, "/sensor/temperature", "json", &Default::default())
        .expect("channel");
    for i in 0..2_000u32 {
        // A value that moves, so a decoder that returned zeros would not pass by luck.
        let v = (f64::from(i) * 0.017).sin() * 40.0 + 20.0;
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: c,
                sequence: i,
                log_time: 1_000_000 * u64::from(i),
                publish_time: 1_000_000 * u64::from(i),
            },
            format!(r#"{{"step":{i},"value":{v}}}"#).as_bytes(),
        )
        .expect("message");
    }
    w.finish().expect("finish");
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).expect("mkdir");
    for (name, comp) in [
        ("codec-none.mcap", None),
        ("codec-zstd.mcap", Some(mcap::Compression::Zstd)),
        ("codec-lz4.mcap", Some(mcap::Compression::Lz4)),
    ] {
        let p = dir.join(name);
        write(&p, comp);
        let n = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        println!("{} {n} bytes", p.display());
    }
}
