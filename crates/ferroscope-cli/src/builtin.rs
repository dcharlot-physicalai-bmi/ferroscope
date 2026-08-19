//! Robot descriptions that travel with the binary.
//!
//! A phrase like "an SO-101 arm" names a robot, not a path. Resolving that against the
//! filesystem works only inside this repository, so the two descriptions people name by word are
//! embedded: `cargo install ferroscope-cli` then `ferroscope say "an SO-101"` works with no
//! files at all. Anything else is still a path, resolved by the caller.

/// The descriptions live inside the crate rather than in `examples/`, because a published
/// tarball contains only the crate directory: an `include_str!` reaching outside it compiles
/// here and fails at `cargo publish`, which is exactly where this was caught. `examples/robots/`
/// keeps the copies a reader browses, and CI asserts the two are byte-identical so they cannot
/// drift into two different robots with one name.
pub const BUILTIN: &[(&str, &str)] = &[
    ("so101", include_str!("../robots/so101.urdf")),
    ("arm", include_str!("../robots/arm.urdf")),
];

/// Resolve a robot reference: a built-in name first, then a path on disk.
///
/// The name is matched on the file stem, so `so101`, `so101.urdf` and `robots/so101.urdf` all
/// reach the built-in — which is what a person means by any of them.
pub fn load(name: &str) -> Option<String> {
    let stem = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_end_matches(".urdf");
    if let Some((_, text)) = BUILTIN.iter().find(|(n, _)| *n == stem) {
        return Some(text.to_string());
    }
    std::fs::read_to_string(name).ok()
}
