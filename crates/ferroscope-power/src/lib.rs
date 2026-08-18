//! Read real power off the machine, or say clearly why you cannot.
//!
//! Ferroscope's energy ledger claims its joules come from **measured power integrated over the
//! run, never a datasheet TDP**. This crate is the half that makes that claim true: it reads what
//! the machine will actually tell you, and when the machine will not tell you anything, it says
//! so instead of returning a zero that integrates into a very confident nothing.
//!
//! ```no_run
//! use ferroscope_power::Meter;
//!
//! let mut m = Meter::open();
//! println!("{}", m.describe());
//! // The first sample of a cumulative counter has nothing to difference against, so a rate
//! // needs two reads. That is a property of the counter, not an error.
//! let _ = m.sample();
//! std::thread::sleep(std::time::Duration::from_millis(200));
//! if let Ok(Some(w)) = m.sample() {
//!     println!("{w:.2} W");
//! }
//! ```
//!
//! # What it reads
//!
//! - **Linux**: RAPL through `/sys/class/powercap`. Cumulative microjoules per domain.
//! - **macOS**: `powermetrics`, which reports instantaneous milliwatts and **requires
//!   superuser**. Without it there is no supported user-space power interface on this platform,
//!   and [`Source::Unavailable`] says exactly that rather than implying the machine has no
//!   sensor.
//!
//! # Two traps this handles, because both produce plausible wrong numbers
//!
//! **Nested domains double-count.** `intel-rapl:0` is the package; `intel-rapl:0:0` is the core
//! *inside* that package. Summing every directory under `powercap` counts the core's joules
//! twice — once on its own and once inside its parent — and the error is silent, roughly 30–60 %,
//! and always in the flattering direction. Only top-level domains are summed.
//!
//! **The counters wrap.** RAPL energy registers are finite and roll over, on some parts every
//! minute or two under load. A naive `now - before` then goes hugely negative, and code that
//! clamps at zero silently drops a whole interval's energy. Each domain's declared
//! `max_energy_range_uj` is used to unwrap.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where the numbers come from, or why there are none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Linux RAPL. Each entry is a top-level domain: its name and its sysfs directory.
    Rapl { domains: Vec<(String, PathBuf)> },
    /// macOS `powermetrics`, sampling instantaneous power.
    Powermetrics,
    /// Nothing readable. `why` is the part that matters: an unreadable meter and an absent one
    /// call for completely different actions from whoever is reading this.
    Unavailable { why: String },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Rapl { domains } => write!(
                f,
                "Linux RAPL, {} top-level domain(s): {}",
                domains.len(),
                domains
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Source::Powermetrics => write!(f, "macOS powermetrics (instantaneous, superuser)"),
            Source::Unavailable { why } => write!(f, "no power interface: {why}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The interface is not available at all; see [`Meter::source`].
    Unavailable(String),
    /// It was there a moment ago and is not now, or a read failed mid-run.
    Read(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unavailable(s) => write!(f, "no power interface: {s}"),
            Error::Read(s) => write!(f, "power read failed: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Keep only the domains whose energy is not already counted inside another one.
///
/// A top-level domain has exactly one colon: `intel-rapl:0`. A subdomain has two,
/// `intel-rapl:0:0`, and its joules are already inside its parent's — so summing every directory
/// under `powercap` counts them twice, silently, by 30–60 %, always in the flattering direction.
///
/// This is a pure function on names rather than a filter buried in the directory walk, because a
/// Windows filesystem cannot create a directory called `intel-rapl:0` at all, and an invariant
/// this load-bearing should not be reachable only through a filesystem that can spell it.
pub fn top_level_domains(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| n.matches(':').count() == 1)
        .cloned()
        .collect()
}

/// One domain's cumulative counter and its wrap point.
#[derive(Clone, Debug)]
struct Counter {
    name: String,
    path: PathBuf,
    max_uj: u64,
    last_uj: Option<u64>,
}

/// A power meter over whatever this machine exposes.
pub struct Meter {
    source: Source,
    counters: Vec<Counter>,
    last_at: Option<Instant>,
    /// Per-domain watts from the most recent successful [`Meter::sample`].
    last_by_domain: Vec<(String, f64)>,
}

fn read_u64(p: &Path) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

impl Meter {
    /// Probe the machine and open whatever it offers.
    ///
    /// `FERROSCOPE_POWERCAP` overrides the sysfs root. That exists for two real reasons: a
    /// powercap tree is not always mounted where the default expects, and it is the only way to
    /// exercise the measuring path on a machine that has no RAPL — which is every macOS and
    /// Windows runner, and most developer laptops. A code path that only ever runs on hardware
    /// nobody in the loop owns is a code path nobody has run.
    pub fn open() -> Meter {
        if let Some(root) = std::env::var_os("FERROSCOPE_POWERCAP") {
            return Meter::open_rapl_at(Path::new(&root));
        }
        if cfg!(target_os = "linux") {
            return Meter::open_rapl_at(Path::new("/sys/class/powercap"));
        }
        if cfg!(target_os = "macos") {
            return Meter::open_powermetrics();
        }
        Meter::unavailable(format!(
            "{} has no power interface this crate reads",
            std::env::consts::OS
        ))
    }

    fn unavailable(why: String) -> Meter {
        Meter {
            source: Source::Unavailable { why },
            counters: Vec::new(),
            last_at: None,
            last_by_domain: Vec::new(),
        }
    }

    /// Open RAPL against an explicit sysfs root.
    ///
    /// The root is a parameter so the parsing, the de-duplication and the wraparound can be
    /// tested against a laid-out directory on any machine — including one with no RAPL at all,
    /// which is most developer machines and every macOS CI runner.
    pub fn open_rapl_at(root: &Path) -> Meter {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Meter::unavailable(format!(
                "{} is not readable, so this kernel exposes no powercap interface",
                root.display()
            ));
        };

        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("intel-rapl") || n.starts_with("amd-rapl"))
            .collect();
        names.sort();

        let top = top_level_domains(&names);

        if top.is_empty() {
            return Meter::unavailable(format!(
                "{} exists but exposes no top-level RAPL domain",
                root.display()
            ));
        }

        let mut counters = Vec::new();
        let mut unreadable = Vec::new();
        for n in &top {
            let dir = root.join(n);
            let energy = dir.join("energy_uj");
            let label = std::fs::read_to_string(dir.join("name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| n.clone());
            match read_u64(&energy) {
                Some(_) => counters.push(Counter {
                    name: label,
                    path: energy,
                    // A domain that does not declare its range still wraps; u32 microjoules is
                    // the common register width, and assuming it is far better than assuming
                    // the counter is infinite.
                    max_uj: read_u64(&dir.join("max_energy_range_uj")).unwrap_or(u32::MAX as u64),
                    last_uj: None,
                }),
                None => unreadable.push(label),
            }
        }

        if counters.is_empty() {
            // The single most common outcome on a modern distribution, and the one worth
            // spelling out: the mitigation for CVE-2020-8694 made these files root-only, so a
            // process that shrugs here reports 0 J for a machine that is drawing 90 W.
            return Meter::unavailable(format!(
                "RAPL is present ({}) but energy_uj is not readable by this user: since \
                 CVE-2020-8694 these files are root-only on most distributions. Run as root, or \
                 grant CAP_DAC_READ_SEARCH. Reporting no measurement rather than zero joules.",
                unreadable.join(", ")
            ));
        }

        let domains = counters
            .iter()
            .map(|c| (c.name.clone(), c.path.clone()))
            .collect();
        Meter {
            source: Source::Rapl { domains },
            counters,
            last_at: None,
            last_by_domain: Vec::new(),
        }
    }

    fn open_powermetrics() -> Meter {
        if !Path::new("/usr/bin/powermetrics").exists() {
            return Meter::unavailable("powermetrics is not installed".into());
        }
        // Probe rather than assume: powermetrics exists on every Mac and runs on almost none of
        // them without privilege, so the useful question is whether *this process* can run it.
        match std::process::Command::new("/usr/bin/powermetrics")
            .args(["--samplers", "cpu_power", "-n", "1", "-i", "50"])
            .output()
        {
            Ok(out) if out.status.success() => Meter {
                source: Source::Powermetrics,
                counters: Vec::new(),
                last_at: None,
                last_by_domain: Vec::new(),
            },
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr);
                let msg = msg.trim();
                Meter::unavailable(format!(
                    "powermetrics is installed but this process cannot run it ({}). macOS has no \
                     unprivileged power interface, so run under sudo to measure. Reporting no \
                     measurement rather than zero joules.",
                    if msg.is_empty() { "no output" } else { msg }
                ))
            }
            Err(e) => Meter::unavailable(format!("powermetrics could not be started: {e}")),
        }
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Whether this meter can produce a number at all.
    pub fn is_measuring(&self) -> bool {
        !matches!(self.source, Source::Unavailable { .. })
    }

    /// One line naming the source, for a run spec or a log.
    pub fn describe(&self) -> String {
        self.source.to_string()
    }

    /// Watts since the previous call, or `None` when there is not yet an interval to divide by.
    ///
    /// The first call on a cumulative counter returns `Ok(None)`: there is nothing to difference
    /// against yet. That is the counter's nature, not a failure, and it is distinct from
    /// `Err(Unavailable)`, which means there will never be a number.
    pub fn sample(&mut self) -> Result<Option<f64>, Error> {
        match self.source {
            Source::Unavailable { ref why } => Err(Error::Unavailable(why.clone())),
            Source::Powermetrics => {
                let w = self.sample_powermetrics()?;
                self.last_by_domain = vec![("cpu".into(), w)];
                Ok(Some(w))
            }
            Source::Rapl { .. } => self.sample_rapl(),
        }
    }

    /// Per-domain watts from the most recent [`Meter::sample`], so a ledger can attribute rather
    /// than only total.
    pub fn by_domain(&self) -> &[(String, f64)] {
        &self.last_by_domain
    }

    fn sample_rapl(&mut self) -> Result<Option<f64>, Error> {
        let now = Instant::now();
        let mut readings = Vec::with_capacity(self.counters.len());
        for c in &self.counters {
            let uj = read_u64(&c.path)
                .ok_or_else(|| Error::Read(format!("{} became unreadable", c.path.display())))?;
            readings.push(uj);
        }

        let Some(prev_at) = self.last_at else {
            for (c, uj) in self.counters.iter_mut().zip(readings) {
                c.last_uj = Some(uj);
            }
            self.last_at = Some(now);
            return Ok(None);
        };

        let dt = now.duration_since(prev_at).as_secs_f64();
        if dt <= 0.0 {
            // Two reads inside the clock's resolution divide to infinity. Better to say "not
            // yet" than to emit a number no interval supports.
            return Ok(None);
        }

        let mut total_w = 0.0;
        let mut by_domain = Vec::with_capacity(self.counters.len());
        for (c, uj) in self.counters.iter_mut().zip(readings) {
            let prev = c.last_uj.unwrap_or(uj);
            // Unwrap the counter. A register that rolled over reads *lower* than last time; the
            // energy actually spent is the distance up to the wrap plus the distance since.
            let delta_uj = if uj >= prev {
                uj - prev
            } else {
                c.max_uj
                    .saturating_sub(prev)
                    .saturating_add(uj)
                    .saturating_add(1)
            };
            c.last_uj = Some(uj);
            let w = delta_uj as f64 * 1e-6 / dt;
            total_w += w;
            by_domain.push((c.name.clone(), w));
        }
        self.last_at = Some(now);
        self.last_by_domain = by_domain;
        Ok(Some(total_w))
    }

    fn sample_powermetrics(&self) -> Result<f64, Error> {
        let out = std::process::Command::new("/usr/bin/powermetrics")
            .args(["--samplers", "cpu_power", "-n", "1", "-i", "100"])
            .output()
            .map_err(|e| Error::Read(e.to_string()))?;
        if !out.status.success() {
            return Err(Error::Read(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        parse_powermetrics(&String::from_utf8_lossy(&out.stdout))
            .ok_or_else(|| Error::Read("no power line in powermetrics output".into()))
    }
}

/// Pull watts out of a `powermetrics` report.
///
/// The label differs across generations — `Combined Power`, `Package Power`, `CPU Power` — so the
/// most inclusive one present is preferred, and the unit is read rather than assumed.
pub fn parse_powermetrics(text: &str) -> Option<f64> {
    let mut best: Option<(u8, f64)> = None;
    for line in text.lines() {
        let l = line.trim();
        let rank = if l.starts_with("Combined Power") {
            3
        } else if l.starts_with("Package Power") {
            2
        } else if l.starts_with("CPU Power") {
            1
        } else {
            continue;
        };
        let Some((_, rhs)) = l.split_once(':') else {
            continue;
        };
        let rhs = rhs.trim();
        let num: String = rhs
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let v: f64 = num.parse().ok()?;
        let w = if rhs.contains("mW") { v / 1000.0 } else { v };
        if best.map(|(r, _)| rank > r).unwrap_or(true) {
            best = Some((rank, w));
        }
    }
    best.map(|(_, w)| w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_domains_are_dropped_by_name_alone() {
        // Runs on every platform, including the one whose filesystem cannot represent these
        // names: `intel-rapl:0:0` is the core inside package `intel-rapl:0`.
        let names: Vec<String> = [
            "intel-rapl:0",
            "intel-rapl:0:0",
            "intel-rapl:0:1",
            "intel-rapl:1",
            "intel-rapl:1:0",
            "amd-rapl:0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            top_level_domains(&names),
            vec!["intel-rapl:0", "intel-rapl:1", "amd-rapl:0"],
            "only domains that contain no other domain may be summed"
        );
        assert!(top_level_domains(&[]).is_empty());
    }

    /// Lay out a fake `/sys/class/powercap`, so the parsing is testable on a machine with no RAPL.
    fn fake_sysfs(dir: &Path, domains: &[(&str, &str, u64, u64)]) {
        std::fs::create_dir_all(dir).unwrap();
        for (d, name, energy, max) in domains {
            let p = dir.join(d);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(p.join("energy_uj"), format!("{energy}\n")).unwrap();
            std::fs::write(p.join("max_energy_range_uj"), format!("{max}\n")).unwrap();
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ferroscope-power-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    // Windows reserves the colon in a filename, so a real sysfs name cannot exist there.
    // The naming rule itself is covered on every platform by the test above.
    #[test]
    #[cfg_attr(windows, ignore = "a Windows path cannot contain a colon")]
    fn a_subdomain_is_never_summed_into_its_parent() {
        // The silent 30-60 % error: intel-rapl:0:0 is the core INSIDE package intel-rapl:0, so
        // summing every directory counts those joules twice.
        let d = tmp("nested");
        fake_sysfs(
            &d,
            &[
                ("intel-rapl:0", "package-0", 1_000_000, u32::MAX as u64),
                ("intel-rapl:0:0", "core", 600_000, u32::MAX as u64),
                ("intel-rapl:0:1", "uncore", 200_000, u32::MAX as u64),
                ("intel-rapl:1", "package-1", 900_000, u32::MAX as u64),
            ],
        );
        let m = Meter::open_rapl_at(&d);
        match m.source() {
            Source::Rapl { domains } => {
                let names: Vec<&str> = domains.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["package-0", "package-1"],
                    "subdomains must not be counted"
                );
            }
            other => panic!("expected RAPL, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    // Windows reserves the colon in a filename, so a real sysfs name cannot exist there.
    // The naming rule itself is covered on every platform by the test above.
    #[test]
    #[cfg_attr(windows, ignore = "a Windows path cannot contain a colon")]
    fn energy_between_two_reads_becomes_watts() {
        let d = tmp("rate");
        fake_sysfs(&d, &[("intel-rapl:0", "package-0", 0, u32::MAX as u64)]);
        let mut m = Meter::open_rapl_at(&d);
        assert_eq!(
            m.sample(),
            Ok(None),
            "the first read has nothing to difference"
        );
        std::thread::sleep(std::time::Duration::from_millis(120));
        // 3 J spent since the first read.
        std::fs::write(d.join("intel-rapl:0").join("energy_uj"), "3000000\n").unwrap();
        let w = m.sample().unwrap().expect("a rate after two reads");
        // ~0.12 s elapsed, so ~25 W. Generous bounds: the sleep is a floor, not a promise.
        assert!(
            w > 8.0 && w < 60.0,
            "3 J over ~0.12 s should be tens of watts, got {w}"
        );
        assert_eq!(m.by_domain().len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    // Windows reserves the colon in a filename, so a real sysfs name cannot exist there.
    // The naming rule itself is covered on every platform by the test above.
    #[test]
    #[cfg_attr(windows, ignore = "a Windows path cannot contain a colon")]
    fn a_wrapped_counter_does_not_read_as_negative_or_zero_energy() {
        // RAPL registers roll over, on some parts every minute or two under load. A naive
        // subtraction goes hugely negative; clamping it to zero silently drops the interval.
        let d = tmp("wrap");
        let max = 1_000_000u64; // a small range, so the wrap is easy to state
        fake_sysfs(&d, &[("intel-rapl:0", "package-0", 999_000, max)]);
        let mut m = Meter::open_rapl_at(&d);
        m.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Past the wrap by 2000 µJ: the real spend is (max - 999000) + 2000 + 1 = 3001 µJ.
        std::fs::write(d.join("intel-rapl:0").join("energy_uj"), "2000\n").unwrap();
        let w = m.sample().unwrap().expect("a rate");
        assert!(w > 0.0, "a wrapped counter must not read as zero, got {w}");
        // 3001 µJ over ~0.06 s is ~0.05 W. The point is the sign and the magnitude, not the exact
        // interval, so this checks it is small-but-positive rather than a huge bogus number.
        assert!(w < 1.0, "and must not read as a huge spike either, got {w}");
        let _ = std::fs::remove_dir_all(&d);
    }

    // Windows reserves the colon in a filename, so a real sysfs name cannot exist there.
    // The naming rule itself is covered on every platform by the test above.
    #[test]
    #[cfg_attr(windows, ignore = "a Windows path cannot contain a colon")]
    fn an_unreadable_counter_reports_why_instead_of_zero() {
        // The modern default on Linux: the directory is there, energy_uj is root-only. A meter
        // that shrugs here reports 0 J for a machine drawing 90 W.
        let d = tmp("perm");
        std::fs::create_dir_all(d.join("intel-rapl:0")).unwrap();
        std::fs::write(d.join("intel-rapl:0").join("name"), "package-0\n").unwrap();
        // energy_uj deliberately absent, which is what an unreadable file looks like to us.
        let m = Meter::open_rapl_at(&d);
        assert!(!m.is_measuring());
        match m.source() {
            Source::Unavailable { why } => {
                assert!(why.contains("package-0"), "{why}");
                assert!(why.contains("rather than zero"), "{why}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_powercap_tree_is_unavailable_not_a_panic() {
        let m = Meter::open_rapl_at(Path::new("/definitely/not/here"));
        assert!(!m.is_measuring());
        assert!(matches!(m.source(), Source::Unavailable { .. }));
    }

    #[test]
    fn sampling_an_unavailable_meter_is_an_error_not_a_zero() {
        // The distinction the whole crate turns on: `Ok(None)` means "ask again", `Err` means
        // "there will never be a number". Collapsing them is how a zero gets integrated.
        let mut m = Meter::open_rapl_at(Path::new("/definitely/not/here"));
        assert!(matches!(m.sample(), Err(Error::Unavailable(_))));
    }

    #[test]
    fn powermetrics_output_is_parsed_and_the_unit_is_read_not_assumed() {
        let apple = "\
*** Sampled system activity ***
CPU Power: 1234 mW
GPU Power: 56 mW
Combined Power (CPU + GPU + ANE): 1290 mW";
        assert!((parse_powermetrics(apple).unwrap() - 1.29).abs() < 1e-9);

        // Watts rather than milliwatts, and only a CPU line present.
        assert!((parse_powermetrics("CPU Power: 42.5 W").unwrap() - 42.5).abs() < 1e-9);
        // The most inclusive label wins regardless of the order it appears in.
        let ordered = "Combined Power: 10000 mW\nCPU Power: 9000 mW";
        assert!((parse_powermetrics(ordered).unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(parse_powermetrics("nothing to see here"), None);
    }

    #[test]
    fn this_machine_reports_what_it_can_and_says_why_when_it_cannot() {
        // Not an assertion about the platform — CI runs three of them — but a check that the
        // probe always yields something a reader can act on.
        let m = Meter::open();
        let d = m.describe();
        assert!(!d.is_empty());
        if !m.is_measuring() {
            assert!(
                d.contains("no power interface"),
                "an unavailable meter must say so: {d}"
            );
        }
    }
}
