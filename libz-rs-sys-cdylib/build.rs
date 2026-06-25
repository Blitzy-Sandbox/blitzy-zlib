//! Build script for `libz-rs-sys-cdylib` — optional GNU ld version-script wiring.
//!
//! This crate emits the drop-in `libz.so` (cdylib) / `libz.a` (staticlib). The C
//! build (CMakeLists.txt) links the *shared* library with a symbol
//! `--version-script` (`-Wl,--version-script,zlib.map`) on GNU-ld UNIX targets so
//! the produced `.so` carries versioned symbol nodes (`ZLIB_1.2.0` …
//! `ZLIB_1.3.2`) for a *true versioned* drop-in. This script reproduces that
//! behavior, gated behind the opt-in `version-script` Cargo feature.
//!
//! Design constraints (AAP §0.3.1 "optional ld version-script wired from
//! zlib.map"; the crate's `Cargo.toml` notes):
//!
//! * **`std`-only.** No external build dependency is declared or used.
//! * **Opt-in.** Wiring happens ONLY when the `version-script` feature is
//!   enabled (Cargo then sets `CARGO_FEATURE_VERSION_SCRIPT`). With the feature
//!   off — the default — this script is a no-op and the build is unaffected.
//! * **Best-effort and never fatal.** `../zlib.map` lists symbols that the
//!   73-symbol Rust shim does not export (the `*64` 64-bit variants, internal
//!   `local:` symbols, and the `*_z` aliases). Modern GNU ld defaults to
//!   `--no-undefined-version`, which would *error* on those. We therefore also
//!   pass `--undefined-version` so the link tolerates the gaps. Any problem
//!   (missing map, unsupported platform) downgrades to a `cargo:warning` and a
//!   skip — the build stays green.
//! * **cdylib only.** The version script affects the *linked* shared object, so
//!   the flags are emitted via `rustc-cdylib-link-arg`; the `staticlib` (an
//!   archive) has no link step and is unaffected.

use std::env;
use std::path::PathBuf;

fn main() {
    // Re-run when the opt-in toggle or the version script itself changes.
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_VERSION_SCRIPT");
    println!("cargo:rerun-if-changed=build.rs");

    // The `version-script` feature is off by default; do nothing unless asked.
    // Cargo exposes an enabled feature `version-script` as this env var.
    if env::var_os("CARGO_FEATURE_VERSION_SCRIPT").is_none() {
        return;
    }

    // Resolve `../zlib.map` (repo root) relative to this crate's manifest dir.
    let manifest_dir = match env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // Practically unreachable (Cargo always sets it); never fail the build.
            println!(
                "cargo:warning=version-script: CARGO_MANIFEST_DIR unset; skipping version script"
            );
            return;
        }
    };
    let map_path = manifest_dir.join("..").join("zlib.map");

    // Canonicalize to an absolute path AND confirm the file exists in one step.
    let map_path = match map_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            println!(
                "cargo:warning=version-script: cannot locate {} ({e}); skipping version script",
                map_path.display()
            );
            return;
        }
    };

    // Re-link if the map changes.
    println!("cargo:rerun-if-changed={}", map_path.display());

    // The version script is a GNU ld / lld feature. The C build (CMakeLists.txt)
    // wires it on UNIX AND NOT APPLE AND NOT AIX AND NOT SunOS. We restrict to
    // `linux`, the platform whose GNU ld behavior is validated here, and warn +
    // skip on every other target so the build stays green everywhere.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        println!(
            "cargo:warning=version-script: requested but unsupported on target_os=\"{target_os}\"; \
             skipping (the unversioned libz.so / libz.a are still produced)"
        );
        return;
    }

    let map = map_path.display();

    // `--undefined-version` MUST precede the script so ld tolerates version
    // assignments for symbols the Rust shim does not export (`*64`, locals,
    // `*_z`); without it, modern ld's default `--no-undefined-version` errors.
    println!("cargo:rustc-cdylib-link-arg=-Wl,--undefined-version");
    println!("cargo:rustc-cdylib-link-arg=-Wl,--version-script={map}");
}
