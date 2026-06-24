//! Build script for the `zlib-rs` crate.
//!
//! This script generates the byte-wise IEEE 802.3 CRC-32 lookup table at build
//! time and writes it to `${OUT_DIR}/crc32_table.rs`. The scalar / `no_std`
//! CRC-32 fallback in `src/checksum/crc32.rs` consumes the generated table with:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/crc32_table.rs"));
//! ```
//!
//! The generated file exposes a single, stable item:
//!
//! ```ignore
//! pub static CRC32_TABLE: [u32; 256] = [ /* ... */ ];
//! ```
//!
//! ## Why generate instead of vendoring
//!
//! The upstream C `crc32.h` is itself a *generated* header (`/* Generated
//! automatically by crc32.c */`) whose 256-entry `crc_table[]` is produced by
//! `crc32.c`'s `make_crc_table()`. The full header weighs in at roughly 591 KB
//! because it also embeds the braided / slice-by-N tables used by the optimized
//! C code paths. The table is mathematically fixed by the reflected IEEE
//! polynomial `0xEDB88320`, so we reproduce the basic 256-entry table here from
//! first principles rather than vendoring the giant header.
//!
//! ## Runtime path selection
//!
//! At runtime the *preferred* CRC-32 implementation is the hardware-accelerated
//! `crc32fast` crate (gated by the `simd` feature). This generated table is the
//! correctness-guaranteeing fallback used when that path is unavailable — for
//! example `--no-default-features` `no_std` / `Z_SOLO`-style builds.
//!
//! ## Properties
//!
//! * Dependency-free: uses only the standard library. Cargo always runs build
//!   scripts on the host with `std` available, even when the target crate is
//!   compiled for a `no_std` target, so this is sound.
//! * Safe: contains no `unsafe` code. (Build scripts are not subject to the
//!   crate's `#![forbid(unsafe_code)]`, which lives in `src/lib.rs`; this script
//!   simply has no need for `unsafe`.)
//! * Deterministic: a given polynomial yields byte-identical output on every
//!   run, because the table contents and the textual formatting are both fixed.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The reflected IEEE 802.3 CRC-32 polynomial
/// (`x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 +
/// x^4 + x^2 + x + 1`), matching the C `#define POLY 0xedb88320` in `crc32.c`.
const POLY: u32 = 0xedb8_8320;

/// Number of entries in the byte-wise CRC-32 lookup table — one slot for every
/// possible 8-bit input value.
const TABLE_LEN: usize = 256;

/// Number of table entries emitted per line in the generated source. Purely
/// cosmetic; chosen for readability while keeping the output stable.
const ENTRIES_PER_LINE: usize = 8;

/// Name of the generated file written into `OUT_DIR`.
const OUTPUT_FILE: &str = "crc32_table.rs";

fn main() {
    // The generated table is a pure function of `POLY`, so the only input that
    // can change its output is this script itself. Emitting an explicit
    // `rerun-if-changed` directive both expresses that intent and disables
    // Cargo's default behavior of re-running the script whenever any file in
    // the package changes, avoiding needless rebuilds.
    println!("cargo:rerun-if-changed=build.rs");

    let table = make_crc_table();
    let rendered = render_table(&table);

    // `OUT_DIR` is guaranteed to be set by Cargo for build scripts. Use the
    // OS-string form so non-UTF-8 build paths are handled correctly.
    let out_dir =
        env::var_os("OUT_DIR").expect("Cargo always sets OUT_DIR when running a build script");
    let dest = Path::new(&out_dir).join(OUTPUT_FILE);

    fs::write(&dest, rendered).unwrap_or_else(|err| {
        panic!(
            "zlib-rs build script failed to write the CRC-32 table to {}: {err}",
            dest.display()
        )
    });
}

/// Build the standard 256-entry, byte-wise IEEE CRC-32 lookup table.
///
/// This is a faithful port of the inner loop of `make_crc_table()` in the
/// upstream `crc32.c`: for every possible byte value the CRC register is seeded
/// with that byte and reduced one bit at a time under the reflected polynomial
/// [`POLY`]. Entry `n` holds the CRC of the single byte `n` against a zero
/// initial register, which is exactly the value needed to advance a running
/// CRC one byte at a time.
fn make_crc_table() -> [u32; TABLE_LEN] {
    let mut table = [0u32; TABLE_LEN];

    // `iter_mut().enumerate()` rather than indexing by a range avoids the
    // `clippy::needless_range_loop` lint while keeping the index `n` available
    // as the seed value.
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            // Shift the register right by one bit; if the bit shifted out is
            // set, fold in the polynomial. This is the reflected (LSB-first)
            // CRC shift-register step.
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }

    table
}

/// Render `table` as a self-contained Rust source snippet defining
/// `CRC32_TABLE: [u32; 256]`.
///
/// The layout is fixed — a leading `@generated` banner, then
/// [`ENTRIES_PER_LINE`] lowercase, zero-padded 8-digit hex literals per line,
/// each followed by a comma — so the emitted bytes are identical on every run
/// for a given table.
fn render_table(table: &[u32; TABLE_LEN]) -> String {
    // Each entry renders as "0x12345678, " (12 bytes); add headroom for the
    // banner, indentation, and newlines so the buffer never needs to grow.
    let mut out = String::with_capacity(TABLE_LEN * 13 + 512);

    out.push_str("// @generated by zlib-rs/build.rs - DO NOT EDIT.\n");
    out.push_str("//\n");
    out.push_str("// Byte-wise IEEE 802.3 CRC-32 lookup table, reflected polynomial 0xEDB88320.\n");
    out.push_str("// Reproduces the 256-entry crc_table[] of the upstream C crc32.h and is\n");
    out.push_str("// included by the scalar CRC-32 path in src/checksum/crc32.rs.\n");
    out.push_str("pub static CRC32_TABLE: [u32; 256] = [\n");

    for (i, value) in table.iter().enumerate() {
        if i % ENTRIES_PER_LINE == 0 {
            out.push_str("    ");
        }

        // Writing to a `String` is infallible, so the `Result` is discarded via
        // `expect` purely to document that invariant. Inline format arguments
        // keep `clippy::uninlined_format_args` satisfied.
        write!(out, "0x{value:08x},").expect("writing to a String is infallible");

        if i % ENTRIES_PER_LINE == ENTRIES_PER_LINE - 1 {
            out.push('\n');
        } else {
            out.push(' ');
        }
    }

    out.push_str("];\n");
    out
}
