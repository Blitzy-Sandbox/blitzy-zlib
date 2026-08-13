// A fuzz target reports a finding BY panicking, so the panic family that the rest of the
// workspace denies is allowed for these binaries by `fuzz/Cargo.toml`'s `[lints.clippy]`.
// Nothing in THIS file panics: it is reached on every execution of every target, including
// the instrumented ones, so it is written to be inert.
#![forbid(unsafe_code)]

//! What the five fuzz targets share: one line per execution saying what the run reached.
//!
//! # Why this exists
//!
//! Every target here takes a **typed** record rather than raw bytes -- `InflateInput`,
//! `BackInput`, `FuzzInput`, `GzRoundTrip`, `ChecksumInput` -- which `arbitrary` decodes
//! from the input file. That is what lets a target sweep the configuration space instead of
//! only the payload space, and it has one consequence that is easy to miss: **an input file
//! that is not a valid encoding of that record decodes to something useless.** Not to an
//! error -- `arbitrary` is infallible by design, filling missing bytes with zeros and
//! taking what it can -- but to a record whose `windowBits` is out of range, or whose
//! payload is empty, so the execution stops in the first refusal and exercises nothing.
//!
//! Seeding all six CI rows from the same directory of *uncompressed* fixtures did exactly
//! that. The fixtures are excellent DEFLATE inputs and meaningless typed records, so the
//! corpus every row started from was a corpus of immediate error paths. libFuzzer recovers
//! from that on its own eventually, but "eventually" is not what a 300-second budget buys,
//! and nothing in the workflow could tell the difference between a row that was decoding
//! real streams and a row that was not.
//!
//! # What it does
//!
//! When `ZLIB_RS_FUZZ_REPORT` is set to anything other than `0`, each target prints one
//! line per execution:
//!
//! ```text
//! FUZZ-REACHED target=fuzz_inflate reached=stream_end windowBits=47 payload=31 produced=13
//! ```
//!
//! `reached` is the field that matters and it is a closed vocabulary per target, declared by
//! that target. `.github/workflows/rust.yml`'s `fuzz` job runs every committed seed under
//! `fuzz/seeds/<target>/` through its target once with the variable set and requires at
//! least one seed to report the productive value. So "these seeds reach real code" is a
//! measurement in the log rather than a claim in a comment, and a change to a record's field
//! order -- which silently invalidates every seed for that target -- turns the job red.
//!
//! The variable is also how a seed is *authored*: the same line names what the record
//! decoded to, so a candidate file can be adjusted until it decodes to the record that was
//! wanted. That the authoring tool and the CI gate are the same mechanism is deliberate;
//! a generator that encoded the record independently would be a second implementation of
//! `arbitrary`'s decoding rules, and it would drift.
//!
//! # Cost when it is off
//!
//! One relaxed load of an already-resolved [`OnceLock`] and a branch. The environment is
//! read once per process, not once per execution, and nothing is formatted unless the line
//! is going to be printed -- [`reached`] takes the fields as a closure so an execution with
//! reporting off does not build the string.

use core::fmt::Write as _;
use std::sync::OnceLock;

/// The variable that turns reporting on. Any value but `0` enables it.
pub const REPORT_ENV: &str = "ZLIB_RS_FUZZ_REPORT";

/// The prefix every report line carries, which is what the CI gate greps for.
pub const REPORT_PREFIX: &str = "FUZZ-REACHED";

/// Whether reporting is on, resolved once per process.
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Whether this process should print report lines.
///
/// Reads [`REPORT_ENV`] exactly once. A missing variable, an empty value and `0` all mean
/// off; anything else means on.
#[must_use]
pub fn reporting() -> bool {
    *ENABLED.get_or_init(|| match std::env::var(REPORT_ENV) {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    })
}

/// A field accumulator a target fills in to describe one execution.
///
/// Deliberately not a map: the order the fields are added is the order they are printed, so
/// a log line reads the way the target's author wrote it.
#[derive(Debug, Default)]
pub struct Fields {
    /// `key=value` pairs, already rendered, joined by a space when printed.
    rendered: String,
}

impl Fields {
    /// An empty set of fields.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one `key=value` pair.
    ///
    /// `value` is taken as `impl Display` so a target can pass an integer, a status code or
    /// a `&str` without allocating first.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl core::fmt::Display) -> Self {
        if !self.rendered.is_empty() {
            self.rendered.push(' ');
        }
        // Writing into a `String` cannot fail, and this file must not panic: a
        // `write!` that returned `Err` here would drop the field rather than abort a
        // fuzzing run over a log line.
        let _ = write!(self.rendered, "{key}={value}");
        self
    }
}

/// Prints one report line, if and only if reporting is on.
///
/// `target` is the fuzz target's name as `cargo fuzz` knows it -- the CI gate matches on it,
/// so it must be spelled exactly as the `[[bin]]` entry in `fuzz/Cargo.toml`. `reached` is
/// the closed-vocabulary word for what this execution got to. `fields` is called only when
/// the line will actually be printed, so an ordinary fuzzing run builds no strings.
///
/// Written to **stderr**, which is where libFuzzer writes its own output and therefore where
/// a `cargo fuzz run` transcript ends up.
pub fn reached(target: &str, reached: &str, fields: impl FnOnce(Fields) -> Fields) {
    if !reporting() {
        return;
    }
    let rendered = fields(Fields::new()).rendered;
    if rendered.is_empty() {
        eprintln!("{REPORT_PREFIX} target={target} reached={reached}");
    } else {
        eprintln!("{REPORT_PREFIX} target={target} reached={reached} {rendered}");
    }
}
