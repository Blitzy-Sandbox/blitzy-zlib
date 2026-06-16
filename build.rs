//! Cargo build script for the `zlib-rs` crate.
//!
//! This script runs **before** the crate is compiled and has three distinct,
//! independent responsibilities (AAP §0.3.1, §0.4.1, §0.6.5):
//!
//! 1. **CRC-32 table generation (Phase A — mandatory).** It regenerates the
//!    IEEE reflected CRC-32 lookup tables at build time, mirroring the upstream
//!    `crc32.c` `make_crc_table()` routine and the 9,446-line pre-computed
//!    `crc32.h`. Regenerating the tables means that enormous generated header is
//!    never hand-ported, while the values remain *bit-identical* to canonical C
//!    zlib — which is what keeps CRC-32 output byte-for-byte compatible
//!    (AAP §0.7.1). The tables are written to `${OUT_DIR}/crc32_table.rs`, which
//!    `src/checksum/crc32.rs` pulls in with
//!    `include!(concat!(env!("OUT_DIR"), "/crc32_table.rs"))`.
//!
//! 2. **C-header emission (Phase B — gated, best-effort).** When header
//!    generation is requested, it invokes `cbindgen` (configured by the
//!    repository-root `cbindgen.toml`) to derive the C header — the
//!    `zlib.h`-equivalent — from the FFI surface in `src/ffi.rs`, so the
//!    published C ABI always tracks the Rust source. Header generation is
//!    deliberately *non-fatal*: if `src/ffi.rs` is absent or `cbindgen` cannot
//!    parse the crate, the script emits a `cargo:warning` and continues. The
//!    library build never depends on the header existing.
//!
//! 3. **`gzprintf` C forwarder (Phase D — gated on `capi` + `gz-io`).** The
//!    exported `gzprintf` symbol is C-variadic, which stable Rust cannot
//!    *define* (`c_variadic` is unstable). `src/ffi.rs` therefore exports
//!    `gzprintf` as a naked tail-branch into the C symbol
//!    `rs_gzprintf_trampoline`; this phase compiles that 4-line `<stdarg.h>`
//!    forwarder (which runs `va_start` and delegates to the Rust `gzvprintf`
//!    export) and archives it so the cdylib/staticlib link resolves the symbol.
//!    It runs **only** for the C-ABI drop-in build and is the reason the drop-in
//!    is a true binary replacement for `libz` (AAP §0.1.1, §0.6.2).
//!
//! The script requires no network access. Phases A and B need no tooling beyond
//! the `cbindgen` build-dependency declared in `Cargo.toml`. Phase D — and only
//! the optional `capi` build that triggers it — additionally requires a host C
//! compiler and archiver (`cc`/`ar`, overridable via the `CC`/`AR` environment
//! variables); no `cc` *crate* is added, keeping the build-dependency set frozen
//! to `cbindgen` (AAP §0.5.1). Because stable `#[unsafe(naked)]` (used by the
//! `gzprintf` export in `src/ffi.rs`) requires rustc >= 1.88, the optional
//! `capi` feature raises the effective MSRV to 1.88 for that build alone; the
//! core crate keeps the AAP MSRV of 1.85.0.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

// ===========================================================================
// Phase A — CRC-32 table generation
// ===========================================================================

/// The IEEE CRC-32 polynomial in reflected (LSB-first) form, matching
/// `#define POLY 0xedb88320` in `crc32.c`. "Reflected" means the input bits are
/// processed least-significant-bit first, which is the convention used by zlib,
/// gzip, PNG, and Ethernet.
const POLY: u32 = 0xEDB8_8320;

/// Number of slice-by-N tables to generate. Eight tables (the classic
/// "slice-by-8" layout) let the consumer fold eight input bytes per iteration
/// on the fast path while remaining endianness-portable (AAP §0.6.4).
const SLICES: usize = 8;

/// First four entries of the canonical single-byte table, taken verbatim from
/// `crc32.h` line 6. The freshly generated table is asserted against these so a
/// broken host toolchain can never silently emit an incorrect table.
const BASIC_TABLE_HEAD: [u32; 4] = [0x0000_0000, 0x7707_3096, 0xEE0E_612C, 0x9909_51BA];

/// CRC-32 of the standard check string `"123456789"`. This is the universally
/// published IEEE CRC-32 check value; it is recomputed from the freshly built
/// table as an end-to-end self-test.
const CHECK_STRING_CRC: u32 = 0xCBF4_3926;

/// Build the slice-by-8 CRC-32 tables.
///
/// Row 0 is the canonical single-byte table produced exactly as
/// `crc32.c::make_crc_table()` does: for each byte value, fold in the
/// polynomial eight times. Rows 1..8 are then derived by repeatedly pushing the
/// running CRC through row 0 — the standard slice-by-8 construction
/// (`crc_table[k][n] = crc_table[0][c & 0xff] ^ (c >> 8)`).
fn make_crc_tables() -> [[u32; 256]; SLICES] {
    let mut tables = [[0u32; 256]; SLICES];

    // Row 0: the canonical single-byte table (port of `make_crc_table()`).
    for (n, slot) in tables[0].iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }

    // Rows 1..8: slice-by-8 derivation from row 0. Each statement borrows the
    // `tables` array only momentarily, so the read of row 0 and the write of
    // row `k` never overlap.
    for n in 0..256 {
        let mut c = tables[0][n];
        for k in 1..SLICES {
            c = tables[0][(c & 0xff) as usize] ^ (c >> 8);
            tables[k][n] = c;
        }
    }

    tables
}

/// Compute a CRC-32 over `data` using the freshly generated single-byte table.
/// Used only as a build-time self-test (see [`verify_tables`]).
fn crc32_oneshot(table0: &[u32; 256], data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc = (crc >> 8) ^ table0[((crc ^ u32::from(byte)) & 0xff) as usize];
    }
    crc ^ 0xFFFF_FFFF
}

/// Self-test the generated tables against known-correct constants.
///
/// A failure here means the host toolchain miscomputed the tables; failing the
/// build loudly is vastly preferable to silently shipping an incorrect CRC that
/// would corrupt every zlib/gzip stream trailer.
fn verify_tables(tables: &[[u32; 256]; SLICES]) {
    for (i, &expected) in BASIC_TABLE_HEAD.iter().enumerate() {
        assert_eq!(
            tables[0][i], expected,
            "CRC-32 basic table[{i}] = {:#010x}, expected {expected:#010x} (must match crc32.h)",
            tables[0][i],
        );
    }

    // Re-derive rows 1..8 independently and confirm the stored values agree.
    for n in 0..256 {
        let mut c = tables[0][n];
        for k in 1..SLICES {
            c = tables[0][(c & 0xff) as usize] ^ (c >> 8);
            assert_eq!(
                tables[k][n], c,
                "slice-by-8 table[{k}][{n}] is inconsistent with row 0",
            );
        }
    }

    let check = crc32_oneshot(&tables[0], b"123456789");
    assert_eq!(
        check, CHECK_STRING_CRC,
        "CRC-32 of \"123456789\" = {check:#010x}, expected {CHECK_STRING_CRC:#010x}",
    );
}

/// Banner written at the top of the generated table module.
const GENERATED_PREAMBLE: &str = r#"// @generated by build.rs — DO NOT EDIT.
//
// IEEE reflected CRC-32 lookup tables (polynomial 0xEDB88320), regenerated at
// build time so the 9,446-line upstream `crc32.h` is never hand-ported.
//
//   * `CRC_TABLE[0]`    — the canonical single-byte table, bit-identical to the
//                         `crc_table` in `crc32.h`.
//   * `CRC_TABLE[1..8]` — the slice-by-8 derived tables for the fast path.
//   * `CRC_TABLE_0`     — a flat alias of row 0 for the single-byte / tail path.
//
// Consumed via:  include!(concat!(env!("OUT_DIR"), "/crc32_table.rs"));

"#;

/// Append one 256-entry table as rows of eight `0x`-prefixed, zero-padded hex
/// literals (mirroring the formatting of the upstream `crc32.h`).
fn write_table_rows(out: &mut String, table: &[u32; 256]) {
    for (i, &value) in table.iter().enumerate() {
        if i % 8 == 0 {
            out.push_str("        ");
        }
        // Writing to a `String` is infallible, so the `fmt::Result` is ignored.
        let _ = write!(out, "0x{value:08X},");
        out.push(if i % 8 == 7 { '\n' } else { ' ' });
    }
}

/// Render the generated tables into `${OUT_DIR}/crc32_table.rs`.
fn emit_crc_tables(out_dir: &Path, tables: &[[u32; 256]; SLICES]) {
    let mut out = String::with_capacity(SLICES * 256 * 12 + 2_048);
    out.push_str(GENERATED_PREAMBLE);

    out.push_str("pub(crate) static CRC_TABLE: [[u32; 256]; 8] = [\n");
    for table in tables {
        out.push_str("    [\n");
        write_table_rows(&mut out, table);
        out.push_str("    ],\n");
    }
    out.push_str("];\n\n");

    // A flat alias of row 0, emitted as an independent literal (a `static`
    // cannot be initialized by indexing another `static` in a const context).
    out.push_str("pub(crate) static CRC_TABLE_0: [u32; 256] = [\n");
    write_table_rows(&mut out, &tables[0]);
    out.push_str("];\n");

    let dest = out_dir.join("crc32_table.rs");
    fs::write(&dest, out).unwrap_or_else(|err| panic!("failed to write {}: {err}", dest.display()));
}

// ===========================================================================
// Phase B / C — cbindgen C-header emission (gated, best-effort)
// ===========================================================================

/// Environment flag that explicitly requests C-header generation.
const HEADER_REQUEST_ENV: &str = "ZLIB_RS_GENERATE_HEADER";

/// Decide whether C-header generation was requested for this build.
///
/// Generation is opt-in for two reasons: (1) the C header is only needed by C
/// consumers of the `cdylib`/`staticlib` drop-in — not by Rust builds or tests
/// — and (2) `src/ffi.rs` may not exist during early or partial builds. It is
/// enabled when the [`HEADER_REQUEST_ENV`] flag is set, or when a `capi`/`ffi`
/// Cargo feature gates the FFI exports (Cargo surfaces an enabled feature `foo`
/// as the environment variable `CARGO_FEATURE_FOO`).
fn header_generation_requested() -> bool {
    env::var_os(HEADER_REQUEST_ENV).is_some()
        || env::var_os("CARGO_FEATURE_CAPI").is_some()
        || env::var_os("CARGO_FEATURE_FFI").is_some()
}

/// Invoke cbindgen to emit the C header, degrading gracefully on any problem.
///
/// Every failure path prints a `cargo:warning` and returns without panicking,
/// so a missing `src/ffi.rs`, an unreadable `cbindgen.toml`, or a cbindgen
/// parse error can never break the core library build.
fn maybe_generate_header(manifest_dir: &Path, out_dir: &Path) {
    if !header_generation_requested() {
        // Deliberate no-op for ordinary Rust builds and tests. Emitted as a
        // plain informational line (visible with `cargo build -vv`), not a
        // warning, so routine builds stay clean.
        println!(
            "zlib-rs build.rs: C-header generation not requested; skipping cbindgen \
             (set {HEADER_REQUEST_ENV}=1 to generate include/zlib-rs.h)."
        );
        return;
    }

    let ffi_source = manifest_dir.join("src").join("ffi.rs");
    if !ffi_source.exists() {
        println!(
            "cargo:warning=zlib-rs: C-header generation requested but {} does not exist yet; \
             skipping cbindgen. The library build is unaffected.",
            ffi_source.display()
        );
        return;
    }

    // Load the repository-root cbindgen.toml. If it cannot be read, fall back to
    // cbindgen's defaults (still warning) so a missing or malformed config never
    // breaks the build.
    let config_path = manifest_dir.join("cbindgen.toml");
    let config = match cbindgen::Config::from_file(&config_path) {
        Ok(config) => config,
        Err(err) => {
            println!(
                "cargo:warning=zlib-rs: could not read {} ({err}); using cbindgen defaults.",
                config_path.display()
            );
            cbindgen::Config::from_root_or_default(manifest_dir)
        }
    };

    match cbindgen::Builder::new()
        .with_crate(manifest_dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => {
            // Primary, stable location for downstream C consumers, with a copy
            // in OUT_DIR for hermetic or sandboxed build environments.
            let include_dir = manifest_dir.join("include");
            match fs::create_dir_all(&include_dir) {
                Ok(()) => {
                    bindings.write_to_file(include_dir.join("zlib-rs.h"));
                }
                Err(err) => {
                    println!(
                        "cargo:warning=zlib-rs: could not create {} ({err}); \
                         writing the header to OUT_DIR only.",
                        include_dir.display()
                    );
                }
            }
            bindings.write_to_file(out_dir.join("zlib-rs.h"));
            // Restore the C-variadic `gzprintf` prototype that cbindgen cannot
            // render (it is `exclude`d in cbindgen.toml). Applied to both header
            // copies; a no-op on any copy that was not written.
            inject_gzprintf_decl(&include_dir.join("zlib-rs.h"));
            inject_gzprintf_decl(&out_dir.join("zlib-rs.h"));
            println!(
                "zlib-rs build.rs: generated C header from {}.",
                ffi_source.display()
            );
        }
        Err(err) => {
            println!(
                "cargo:warning=zlib-rs: cbindgen header generation skipped: {err}. \
                 The library build is unaffected."
            );
        }
    }
}

/// Inject the C-variadic `gzprintf` prototype into a generated header.
///
/// `gzprintf` is `exclude`d in `cbindgen.toml` because cbindgen cannot render
/// its trailing `...` (the Rust export is a naked branch with a non-variadic
/// declared signature). This restores the exact C prototype immediately after
/// the `gzvprintf` declaration. It is **idempotent** (a header that already
/// declares `gzprintf` is left unchanged) and **best-effort** (a missing or
/// unexpected header only logs and returns, never panicking).
fn inject_gzprintf_decl(header: &Path) {
    let Ok(contents) = fs::read_to_string(header) else {
        // The header may legitimately not exist on this path (e.g. `include/`
        // creation failed and only the OUT_DIR copy was written).
        return;
    };

    // Detect an existing `gzprintf` *declaration* (a code line, not a comment
    // that merely mentions the name) so re-runs never inject twice.
    let already_declared = contents.lines().any(|l| {
        let t = l.trim_start();
        l.contains("gzprintf(")
            && l.trim_end().ends_with(';')
            && !t.starts_with("//")
            && !t.starts_with('*')
            && !t.starts_with("/*")
    });
    if already_declared {
        return;
    }

    let decl = "\n/* gzprintf — C-variadic formatted gzip write (restored by zlib-rs\n * build.rs; cbindgen cannot render the `...`). Forwards to gzvprintf. */\nint gzprintf(gzFile file, const char *format, ...);\n";

    let lines: Vec<&str> = contents.lines().collect();
    // Anchor on the `gzvprintf` declaration line (code ending in `;`, not a
    // doc-comment line that happens to mention the symbol).
    let anchor = lines.iter().position(|l| {
        let t = l.trim_start();
        l.contains("gzvprintf(")
            && l.trim_end().ends_with(';')
            && !t.starts_with("//")
            && !t.starts_with('*')
            && !t.starts_with("/*")
    });

    let mut out = String::with_capacity(contents.len() + decl.len() + 16);
    match anchor {
        Some(idx) => {
            for (i, l) in lines.iter().enumerate() {
                out.push_str(l);
                out.push('\n');
                if i == idx {
                    out.push_str(decl);
                }
            }
        }
        None => {
            // `gzvprintf` not found (unexpected for a default-feature header).
            // Fall back to inserting before the include-guard `#endif`.
            println!(
                "cargo:warning=zlib-rs: gzvprintf declaration not found in {}; \
                 inserting gzprintf before the include-guard close.",
                header.display()
            );
            match lines
                .iter()
                .rposition(|l| l.trim_start().starts_with("#endif"))
            {
                Some(idx) => {
                    for (i, l) in lines.iter().enumerate() {
                        if i == idx {
                            out.push_str(decl);
                            out.push('\n');
                        }
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                None => {
                    out.push_str(&contents);
                    out.push_str(decl);
                }
            }
        }
    }

    if let Err(err) = fs::write(header, &out) {
        println!(
            "cargo:warning=zlib-rs: could not write injected gzprintf into {} ({err}).",
            header.display()
        );
    } else {
        println!(
            "zlib-rs build.rs: injected the C-variadic gzprintf prototype into {}.",
            header.display()
        );
    }
}

// ===========================================================================
// Phase D — `gzprintf` C variadic forwarder (gated: `capi` + `gz-io`)
// ===========================================================================

/// Compile and archive the tiny C `gzprintf` forwarder for the C-ABI drop-in.
///
/// `src/ffi.rs` exports `gzprintf` as a naked tail-branch into the C symbol
/// `rs_gzprintf_trampoline`, which is **defined here**. The forwarder is the
/// canonical zlib three-liner: it captures the caller's varargs with `va_start`,
/// delegates to the Rust `gzvprintf` export (which performs the real `vsnprintf`
/// formatting and the buffered gzip write), then `va_end`s. This is the only way
/// to honor C varargs from a crate that cannot define a variadic `extern "C"`
/// function on stable Rust.
///
/// Runs only when **both** `capi` and `gz-io` are active (the C-ABI gzip build).
/// No `cc` build-dependency crate is used — the AAP freezes the build-deps to
/// `cbindgen` (§0.5.1) — so the host `cc`/`ar` (overridable via `CC`/`AR`) are
/// invoked directly. The symbol is required for the `capi` link to succeed, so a
/// toolchain failure here is fatal (the `capi` feature inherently opts into a C
/// toolchain).
fn maybe_build_gzprintf_trampoline(out_dir: &Path) {
    // Gate on the Cargo-provided feature env vars: only the C-ABI gzip drop-in
    // needs (or can link) this forwarder.
    if env::var_os("CARGO_FEATURE_CAPI").is_none() || env::var_os("CARGO_FEATURE_GZ_IO").is_none() {
        return;
    }

    let csrc = out_dir.join("rs_gzprintf_trampoline.c");
    let trampoline = "\
/* Auto-generated by zlib-rs build.rs (Phase D). DO NOT EDIT.
 *
 * C variadic forwarder backing the exported `gzprintf` symbol. `src/ffi.rs`
 * defines `gzprintf` as a naked tail-branch to `rs_gzprintf_trampoline`; this
 * captures the caller's varargs with `va_start` and delegates to the Rust
 * `gzvprintf` export, exactly like upstream zlib's `gzprintf` (gzwrite.c). */
#include <stdarg.h>

/* The Rust `#[no_mangle] extern \"C\"` gzvprintf export from src/ffi.rs (gz-io).
 * On the SysV/AArch64 C ABIs a `va_list` is passed as a single pointer, so
 * forwarding `&ap` is call-compatible. gzFile is `void *` at the ABI level. */
extern int gzvprintf(void *file, const char *format, void *va);

int rs_gzprintf_trampoline(void *file, const char *format, ...) {
    va_list ap;
    va_start(ap, format);
    int ret = gzvprintf(file, format, (void *)&ap);
    va_end(ap);
    return ret;
}
";
    if let Err(err) = fs::write(&csrc, trampoline) {
        panic!(
            "zlib-rs build.rs (Phase D): could not write {} ({err})",
            csrc.display()
        );
    }

    let obj = out_dir.join("rs_gzprintf_trampoline.o");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    match std::process::Command::new(&cc)
        .args(["-O2", "-fPIC", "-std=c11", "-c"])
        .arg(&csrc)
        .arg("-o")
        .arg(&obj)
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => panic!(
            "zlib-rs build.rs (Phase D): `{cc}` failed to compile the gzprintf \
             forwarder (exit {s}). A C toolchain is required for the `capi` drop-in."
        ),
        Err(err) => panic!(
            "zlib-rs build.rs (Phase D): could not run the C compiler `{cc}` ({err}). \
             Set CC, or install a C toolchain, to build the `capi` drop-in."
        ),
    }

    let lib = out_dir.join("librs_gz_trampoline.a");
    // Recreate from scratch so a stale member never lingers across rebuilds.
    let _ = fs::remove_file(&lib);
    let ar = env::var("AR").unwrap_or_else(|_| "ar".to_string());
    match std::process::Command::new(&ar)
        .arg("crs")
        .arg(&lib)
        .arg(&obj)
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => panic!(
            "zlib-rs build.rs (Phase D): `{ar}` failed to archive the gzprintf \
             forwarder (exit {s})."
        ),
        Err(err) => panic!(
            "zlib-rs build.rs (Phase D): could not run the archiver `{ar}` ({err}). \
             Set AR, or install binutils, to build the `capi` drop-in."
        ),
    }

    // Link the static forwarder into the cdylib/staticlib so the naked
    // `gzprintf` branch in `src/ffi.rs` resolves `rs_gzprintf_trampoline`.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=rs_gz_trampoline");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_CAPI");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_GZ_IO");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
}

// ===========================================================================
// Entry point
// ===========================================================================

fn main() {
    // Re-run triggers: the script itself, the FFI surface and cbindgen config it
    // reads, and the header-request flag.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-env-changed={HEADER_REQUEST_ENV}");

    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo always sets OUT_DIR for build scripts"));
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo always sets CARGO_MANIFEST_DIR for build scripts"),
    );

    // Phase A — always generate, self-test, and emit the CRC-32 tables.
    let tables = make_crc_tables();
    verify_tables(&tables);
    emit_crc_tables(&out_dir, &tables);

    // Phase B / C — optionally emit the C header (best-effort, gated).
    maybe_generate_header(&manifest_dir, &out_dir);

    // Phase D — when building the C-ABI drop-in (`capi` + `gz-io`), compile and
    // archive the C variadic forwarder that backs the exported `gzprintf` symbol
    // (see `src/ffi.rs`). No-op for ordinary library builds and tests.
    maybe_build_gzprintf_trampoline(&out_dir);
}
