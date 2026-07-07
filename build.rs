//! Cargo build script for the `zlib-rs` crate: regenerates the CRC-32 lookup
//! tables at build time.
//!
//! # Why this exists
//!
//! In the C zlib baseline the header `crc32.h` (~9,400 lines) is a checked-in,
//! machine-generated set of CRC-32 lookup tables produced by the
//! `make_crc_table()` routine in `crc32.c`. Rather than ship a giant literal
//! table file, this build script reimplements `make_crc_table()` in safe,
//! host-independent Rust and emits the equivalent Rust source into Cargo's
//! `OUT_DIR`. The checksum module then pulls the tables in with:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/crc32_tables.rs"));
//! ```
//!
//! The regenerated tables are **bit-identical** to the values in `crc32.h`, so
//! the pure-Rust scalar CRC-32 path and the `crc32_combine` math produce output
//! that matches reference zlib exactly. (The SIMD hot path is delegated to the
//! `crc32fast` crate behind the `simd` feature; these tables back the scalar
//! fallback and the combine operations.)
//!
//! # Emitted contract (consumed by `src/checksum/crc32.rs`)
//!
//! The generated `${OUT_DIR}/crc32_tables.rs` defines exactly these items — the
//! names and shapes are a stable contract; do not rename without updating the
//! consuming module:
//!
//! | Symbol                 | Type                 | Meaning                                                   |
//! |------------------------|----------------------|-----------------------------------------------------------|
//! | `CRC_BRAID_N`          | `usize` (= 5)        | Number of interleaved braids used by the braided path.    |
//! | `CRC_BRAID_W`          | `usize` (= 8)        | Bytes per CRC word for the braided path (64-bit words).    |
//! | `CRC_TABLE`            | `[u32; 256]`         | Byte-wise CRC-32 table (`crc_table` in `crc32.h`).         |
//! | `X2N_TABLE`            | `[u32; 32]`          | Powers of x (`x^(2^n) mod p`) for `crc32_combine`.         |
//! | `CRC_BIG_TABLE`        | `[u64; 256]`         | Byte-swapped table for big-endian word processing.         |
//! | `CRC_BRAID_TABLE`      | `[[u32; 256]; 8]`    | Little-endian braid table (`crc_braid_table`, W=8).        |
//! | `CRC_BRAID_BIG_TABLE`  | `[[u64; 256]; 8]`    | Big-endian braid table (`crc_braid_big_table`, W=8).       |
//!
//! # Determinism
//!
//! The tables are pure mathematical constants derived from the reflected
//! CRC-32/IEEE polynomial `0xEDB88320`. This script performs **no** host or
//! target detection: it always emits the fixed `N = 5`, `W = 8` configuration
//! (the zlib default for 64-bit targets) and generates **both** the
//! little-endian and big-endian braid tables so the runtime can select the
//! correct one via `cfg!(target_endian = ...)`. Output is therefore byte-for-byte
//! identical on every build and every platform. The byte-wise `CRC_TABLE` is
//! endianness- and word-size-independent and always provides a correct fallback.
//!
//! # Constraints
//!
//! Pure `std` only — no external crates, no build-dependencies, and zero
//! `unsafe`. All generation runs on the host at build time using plain integer
//! arithmetic, mirroring `crc32.c`'s `make_crc_table()`, `multmodp()`,
//! `x2nmodp()`, `byte_swap()`, and `braid()`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The CRC-32 polynomial, reflected, with the `x^32` term implied.
///
/// This is `0xEDB88320`, the reflected form of the IEEE 802.3 CRC-32 polynomial
/// (`POLY` in `crc32.c`).
const POLY: u32 = 0xedb8_8320;

/// Number of interleaved braids (`N` in `crc32.c`). The zlib default is 5.
const BRAID_N: usize = 5;

/// Number of bytes in a CRC "word" (`W` in `crc32.c`). We emit the 64-bit
/// configuration (`W = 8`), matching the `#if W == 8` branch of `crc32.h`.
const BRAID_W: usize = 8;

// ---------------------------------------------------------------------------
// Core polynomial arithmetic (faithful port of crc32.c helpers)
// ---------------------------------------------------------------------------

/// Return `a(x)` multiplied by `b(x)` modulo `p(x)`, where `p(x)` is the
/// reflected CRC polynomial.
///
/// This is a carry-less (GF(2)) multiply-and-reduce, ported directly from
/// `multmodp()` in `crc32.c`. For speed the C routine requires that `a` is not
/// zero; every call site in this script upholds that invariant, so the loop is
/// guaranteed to terminate at the lowest set bit of `a`.
///
/// All intermediate values fit in 32 bits, so `u32` arithmetic reproduces the
/// C `uLong` computation exactly.
fn multmodp(a: u32, mut b: u32) -> u32 {
    // `m` walks a single set bit from bit 31 down to the lowest set bit of `a`.
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    loop {
        if (a & m) != 0 {
            p ^= b;
            // Once we have consumed the lowest set bit of `a`, we are done.
            if (a & (m - 1)) == 0 {
                break;
            }
        }
        m >>= 1;
        // Multiply `b` by x modulo p(x): a right shift, reducing on carry-out.
        b = if (b & 1) != 0 {
            (b >> 1) ^ POLY
        } else {
            b >> 1
        };
    }
    p
}

/// Return `x^(n * 2^k) modulo p(x)`.
///
/// Port of `x2nmodp()` in `crc32.c`. Requires that `x2n_table` has been
/// initialized. `n` is non-negative in every use here.
fn x2nmodp(mut n: u64, mut k: u32, x2n_table: &[u32; 32]) -> u32 {
    let mut p: u32 = 1 << 31; // x^0 == 1
    while n != 0 {
        if (n & 1) != 0 {
            p = multmodp(x2n_table[(k & 31) as usize], p);
        }
        n >>= 1;
        k = k.wrapping_add(1);
    }
    p
}

/// Reverse the eight bytes of a 64-bit word.
///
/// Port of the `W == 8` branch of `byte_swap()` in `crc32.c`, used to convert
/// between little- and big-endian representations of a CRC word. A 32-bit CRC
/// value that is zero-extended to 64 bits and passed through this function ends
/// up byte-swapped in the **high** 32 bits (the low 32 bits become zero), which
/// is exactly how `crc32.c` builds the big-endian tables.
fn byte_swap64(word: u64) -> u64 {
    ((word & 0xff00_0000_0000_0000) >> 56)
        | ((word & 0x00ff_0000_0000_0000) >> 40)
        | ((word & 0x0000_ff00_0000_0000) >> 24)
        | ((word & 0x0000_00ff_0000_0000) >> 8)
        | ((word & 0x0000_0000_ff00_0000) << 8)
        | ((word & 0x0000_0000_00ff_0000) << 24)
        | ((word & 0x0000_0000_0000_ff00) << 40)
        | ((word & 0x0000_0000_0000_00ff) << 56)
}

// ---------------------------------------------------------------------------
// Table construction (faithful port of crc32.c make_crc_table / braid)
// ---------------------------------------------------------------------------

/// Build the byte-wise CRC-32 table and its byte-swapped (big-endian) companion.
///
/// For each byte value `i`, the CRC of that byte is computed with the standard
/// shift-register method, exactly as in `make_crc_table()`.
fn make_crc_tables() -> ([u32; 256], [u64; 256]) {
    let mut crc_table = [0u32; 256];
    let mut crc_big_table = [0u64; 256];
    for i in 0..256u32 {
        let mut p = i;
        for _ in 0..8 {
            p = if (p & 1) != 0 {
                (p >> 1) ^ POLY
            } else {
                p >> 1
            };
        }
        crc_table[i as usize] = p;
        crc_big_table[i as usize] = byte_swap64(u64::from(p));
    }
    (crc_table, crc_big_table)
}

/// Build the table of powers of x used to combine CRC-32 values.
///
/// `x2n_table[n]` holds `x^(2^n) mod p(x)`; entry 0 is `x^1` and each subsequent
/// entry is the previous one squared modulo `p(x)`. Port of the `x2n_table`
/// initialization in `make_crc_table()`.
fn make_x2n_table() -> [u32; 32] {
    let mut x2n_table = [0u32; 32];
    let mut p: u32 = 1 << 30; // x^1
    x2n_table[0] = p;
    for slot in x2n_table.iter_mut().skip(1) {
        p = multmodp(p, p);
        *slot = p;
    }
    x2n_table
}

/// Build the little- and big-endian braid tables for the given `n` and word
/// size `w`.
///
/// Port of `braid()` in `crc32.c`. Each of the `w` sub-tables holds, for every
/// possible byte value, the sparse CRC contribution of that byte at the braid's
/// position. The big-endian sub-tables are stored in reverse position order and
/// byte-swapped, matching the C layout so the runtime big-endian path indexes
/// them identically.
fn braid(n: usize, w: usize, x2n_table: &[u32; 32]) -> (Vec<[u32; 256]>, Vec<[u64; 256]>) {
    let mut ltl = vec![[0u32; 256]; w];
    let mut big = vec![[0u64; 256]; w];
    for k in 0..w {
        // Exponent for this braid position, in bits: (n*w + 3 - k) * 8.
        let exponent = ((n * w + 3 - k) << 3) as u64;
        let p = x2nmodp(exponent, 0, x2n_table);
        ltl[k][0] = 0;
        big[w - 1 - k][0] = 0;
        for i in 1..256u32 {
            let q = multmodp(i << 24, p);
            ltl[k][i as usize] = q;
            big[w - 1 - k][i as usize] = byte_swap64(u64::from(q));
        }
    }
    (ltl, big)
}

// ---------------------------------------------------------------------------
// Rust source emission
// ---------------------------------------------------------------------------

/// Header written at the top of the generated `crc32_tables.rs`.
///
/// Uses plain `//` comments (not `//!`) because the file is pulled in with
/// `include!` and inner doc comments would attach to the including module.
const GENERATED_HEADER: &str = "\
// crc32_tables.rs -- CRC-32 lookup tables for the zlib-rs crate.
//
// GENERATED FILE - DO NOT EDIT.
// Produced at build time by build.rs (a faithful Rust port of the C
// make_crc_table() routine in crc32.c). The values are bit-identical to the
// checked-in C header crc32.h and must remain so for wire-format compatibility.
//
// Consumed by src/checksum/crc32.rs via:
//     include!(concat!(env!(\"OUT_DIR\"), \"/crc32_tables.rs\"));
//
// Emitted symbols:
//   CRC_BRAID_N:         usize            number of braids (5)
//   CRC_BRAID_W:         usize            bytes per CRC word (8)
//   CRC_TABLE:           [u32; 256]       byte-wise CRC-32 table
//   X2N_TABLE:           [u32; 32]        powers of x for crc32_combine
//   CRC_BIG_TABLE:       [u64; 256]       byte-swapped table (big-endian words)
//   CRC_BRAID_TABLE:     [[u32; 256]; 8]  little-endian braid table
//   CRC_BRAID_BIG_TABLE: [[u64; 256]; 8]  big-endian braid table

";

/// Append a `[u32; N]` static array literal to `out`, eight values per line.
fn write_u32_array(out: &mut String, name: &str, vals: &[u32]) {
    let _ = writeln!(out, "#[allow(dead_code)]");
    let _ = writeln!(out, "pub(crate) static {}: [u32; {}] = [", name, vals.len());
    for chunk in vals.chunks(8) {
        out.push_str("    ");
        for (i, v) in chunk.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "0x{v:08x},");
        }
        out.push('\n');
    }
    out.push_str("];\n\n");
}

/// Append a `[u64; N]` static array literal to `out`, four values per line.
fn write_u64_array(out: &mut String, name: &str, vals: &[u64]) {
    let _ = writeln!(out, "#[allow(dead_code)]");
    let _ = writeln!(out, "pub(crate) static {}: [u64; {}] = [", name, vals.len());
    for chunk in vals.chunks(4) {
        out.push_str("    ");
        for (i, v) in chunk.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "0x{v:016x},");
        }
        out.push('\n');
    }
    out.push_str("];\n\n");
}

/// Append a `[[u32; 256]; W]` static braid table literal to `out`.
fn write_braid_u32(out: &mut String, name: &str, tbl: &[[u32; 256]]) {
    let _ = writeln!(out, "#[allow(dead_code)]");
    let _ = writeln!(
        out,
        "pub(crate) static {}: [[u32; 256]; {}] = [",
        name,
        tbl.len()
    );
    for sub in tbl {
        out.push_str("    [\n");
        for chunk in sub.chunks(8) {
            out.push_str("        ");
            for (i, v) in chunk.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "0x{v:08x},");
            }
            out.push('\n');
        }
        out.push_str("    ],\n");
    }
    out.push_str("];\n\n");
}

/// Append a `[[u64; 256]; W]` static braid table literal to `out`.
fn write_braid_u64(out: &mut String, name: &str, tbl: &[[u64; 256]]) {
    let _ = writeln!(out, "#[allow(dead_code)]");
    let _ = writeln!(
        out,
        "pub(crate) static {}: [[u64; 256]; {}] = [",
        name,
        tbl.len()
    );
    for sub in tbl {
        out.push_str("    [\n");
        for chunk in sub.chunks(4) {
            out.push_str("        ");
            for (i, v) in chunk.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "0x{v:016x},");
            }
            out.push('\n');
        }
        out.push_str("    ],\n");
    }
    out.push_str("];\n\n");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // The tables are pure constants, so we only need to regenerate them when
    // this script itself changes.
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set by Cargo");
    let dest = Path::new(&out_dir).join("crc32_tables.rs");

    // Build every table in memory.
    let (crc_table, crc_big_table) = make_crc_tables();
    let x2n_table = make_x2n_table();
    let (braid_ltl, braid_big) = braid(BRAID_N, BRAID_W, &x2n_table);

    // Emit the Rust source.
    let mut out = String::with_capacity(384 * 1024);
    out.push_str(GENERATED_HEADER);
    let _ = writeln!(out, "pub(crate) const CRC_BRAID_N: usize = {BRAID_N};");
    let _ = writeln!(out, "pub(crate) const CRC_BRAID_W: usize = {BRAID_W};");
    out.push('\n');

    write_u32_array(&mut out, "CRC_TABLE", &crc_table);
    write_u32_array(&mut out, "X2N_TABLE", &x2n_table);
    write_u64_array(&mut out, "CRC_BIG_TABLE", &crc_big_table);
    write_braid_u32(&mut out, "CRC_BRAID_TABLE", &braid_ltl);
    write_braid_u64(&mut out, "CRC_BRAID_BIG_TABLE", &braid_big);

    fs::write(&dest, out).unwrap_or_else(|e| panic!("failed to write {}: {e}", dest.display()));
}
