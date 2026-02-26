//! Build script for the `libz-rs-sys` FFI crate.
//!
//! This build script performs three primary tasks derived from the original C zlib
//! build system (`CMakeLists.txt`, `zlib.pc.in`, `zlib.map`):
//!
//! 1. **Version script generation** — Copies the GNU symbol version script (`zlib.map`)
//!    to `OUT_DIR` and configures the linker to use it on supported platforms. This enables
//!    binary-compatible symbol versioning with the original C zlib across 14 version
//!    milestones (`ZLIB_1.2.0` through `ZLIB_1.3.2`).
//!
//! 2. **pkg-config file generation** — Produces a `zlib.pc` file in `OUT_DIR` for
//!    downstream C/C++ consumers that link against this Rust zlib implementation via
//!    `pkg-config`. The template is derived from the original `zlib.pc.in`.
//!
//! 3. **Platform detection and `cfg` flags** — Emits `cargo:rustc-cfg` directives for
//!    platform-specific conditional compilation, replacing the C preprocessor `#ifdef`
//!    checks from `zconf.h` and `CMakeLists.txt`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Entry point for the Cargo build script.
///
/// Executed automatically by Cargo before compiling the `libz-rs-sys` crate.
/// Reads environment variables set by Cargo (`OUT_DIR`, `CARGO_PKG_VERSION`,
/// `CARGO_CFG_TARGET_*`, `CARGO_FEATURE_*`) and emits `cargo:` directives
/// that control linking, conditional compilation, and rebuild triggers.
fn main() {
    let out_dir = PathBuf::from(
        env::var("OUT_DIR").expect("OUT_DIR environment variable must be set by Cargo"),
    );

    generate_version_script(&out_dir);
    generate_pkg_config(&out_dir);
    emit_platform_cfg_flags();
    emit_rerun_directives();
}

/// Copies the GNU symbol version script (`zlib.map`) to `OUT_DIR` and, on supported
/// platforms, instructs the linker to apply it when producing the cdylib shared library.
///
/// The version script defines 14 symbol version milestones (`ZLIB_1.2.0` through
/// `ZLIB_1.3.2`) that partition the 105+ exported symbols into versioned sets. This is
/// required for binary compatibility when the Rust-built `libz.so` is used as a
/// drop-in replacement for the C-built `libz.so`.
///
/// # Platform behavior
///
/// Per the original `CMakeLists.txt`, the `--version-script` linker flag is applied
/// on GNU/Linux and compatible ELF platforms, but **not** on macOS (which uses a
/// different export mechanism), AIX, or `SunOS`.
///
/// The version script linker argument is only emitted when the `export-symbols`
/// Cargo feature is enabled, which gates whether C-compatible symbols are exported
/// from the shared library.
fn generate_version_script(out_dir: &Path) {
    let version_script_src = PathBuf::from("zlib.map");
    let version_script_dst = out_dir.join("zlib.map");

    // Copy the version script to OUT_DIR so the linker can reference it by
    // absolute path. The source file lives alongside the crate's Cargo.toml.
    if version_script_src.exists() {
        fs::copy(&version_script_src, &version_script_dst)
            .expect("Failed to copy zlib.map version script to OUT_DIR");
    }

    // Communicate the version script path to dependents (notably libz-rs-sys-cdylib)
    // and to the linker when appropriate.
    //
    // The `cargo:rustc-cdylib-link-arg` directive is only effective when emitted by a
    // crate that has a cdylib target in its `[lib]` section. Since `libz-rs-sys` is a
    // regular `lib` crate (not a cdylib), we use `cargo:rustc-link-arg-cdylib` which
    // is the correct directive for passing linker arguments when this crate's code is
    // linked into a downstream cdylib target.
    //
    // The export-symbols feature gates whether C-compatible symbols are exported.
    // When disabled, no version script is applied and symbols are not externally visible.
    let export_symbols_enabled = env::var("CARGO_FEATURE_EXPORT_SYMBOLS").is_ok();

    if export_symbols_enabled && version_script_dst.exists() {
        // Retrieve the target operating system from Cargo's build environment.
        // CARGO_CFG_TARGET_OS is always set during a build script invocation.
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

        // GNU/Linux and BSD-family platforms support the --version-script linker flag.
        // This matches the CMakeLists.txt condition: UNIX AND NOT APPLE AND NOT AIX
        // AND NOT SunOS.
        let supports_version_script = matches!(
            target_os.as_str(),
            "linux"
                | "freebsd"
                | "openbsd"
                | "netbsd"
                | "dragonfly"
                | "android"
                | "illumos"
                | "haiku"
                | "hurd"
        );

        if supports_version_script {
            // Emit the linker argument for the cdylib link step.
            // This takes effect when libz-rs-sys is linked into the
            // libz-rs-sys-cdylib shared library output.
            println!(
                "cargo:rustc-link-arg=-Wl,--version-script={}",
                version_script_dst.display()
            );
        }

        // macOS uses `-exported_symbols_list` or `-Wl,-exported_symbol` instead.
        // Windows uses `.def` files handled by the Cargo/MSVC toolchain automatically.
        // AIX and SunOS have their own export mechanisms not handled here.
    }
}

/// Generates a `zlib.pc` pkg-config file in `OUT_DIR`.
///
/// The generated file follows the structure of the original `zlib.pc.in` template,
/// with placeholder variables (`${prefix}`, `${libdir}`, etc.) that are resolved
/// by the system's `pkg-config` installation when consumers query for zlib.
///
/// The `Version` field is populated from the `CARGO_PKG_VERSION` environment
/// variable, which Cargo sets from the `[package] version` field in `Cargo.toml`.
///
/// The `License` field reflects the dual-licensing of this Rust implementation:
/// the original zlib license plus MIT or Apache-2.0 for the Rust code.
fn generate_pkg_config(out_dir: &Path) {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());

    // The pkg-config template preserves the same variable layout as the original
    // zlib.pc.in, including `sharedlibdir` for separate shared library paths on
    // platforms that distinguish between static and shared library directories.
    let pkg_config_content = format!(
        "\
prefix=${{prefix}}
exec_prefix=${{exec_prefix}}
libdir=${{libdir}}
sharedlibdir=${{sharedlibdir}}
includedir=${{includedir}}

Name: zlib
Description: zlib compression library (Rust implementation)
Version: {version}
License: Zlib AND (MIT OR Apache-2.0)

Requires:
Libs: -L${{libdir}} -L${{sharedlibdir}} -lz
Cflags: -I${{includedir}}
"
    );

    fs::write(out_dir.join("zlib.pc"), pkg_config_content)
        .expect("Failed to write zlib.pc pkg-config file to OUT_DIR");
}

/// Emits `cargo:rustc-cfg` directives for platform-specific conditional compilation.
///
/// These flags replace the C preprocessor `#ifdef` checks that the original zlib
/// performed via `zconf.h`, `zutil.h`, and `CMakeLists.txt`. Downstream Rust code
/// in the FFI crate can use `#[cfg(flag_name)]` attributes to conditionally compile
/// platform-specific code paths.
///
/// # Emitted flags
///
/// | Flag | Condition | Purpose |
/// |------|-----------|---------|
/// | `has_64bit_off_t` | 64-bit pointer width | Large file support (replaces `_LARGEFILE64_SOURCE` / `off64_t` check) |
/// | `windows_platform` | Windows target | Windows-specific APIs like `gzopen_w` |
/// | `unix_platform` | Unix target family | POSIX APIs like `fseeko` |
/// | `target_arch_x86` | x86/x86_64 arch | Potential SSE/AVX optimizations |
/// | `target_arch_arm` | ARM/AArch64 arch | Potential NEON/CRC32 hardware instructions |
/// | `target_arch_s390x` | s390x arch | Potential vector CRC optimizations |
fn emit_platform_cfg_flags() {
    // 64-bit off_t detection — replaces the CMakeLists.txt `check_type_size(off64_t)`
    // and the `_LARGEFILE64_SOURCE` compile definition. On 64-bit platforms, `off_t`
    // is natively 64 bits, so the `*64` suffix variants of gz functions are aliases.
    let target_pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default();
    if target_pointer_width == "64" {
        println!("cargo:rustc-cfg=has_64bit_off_t");
    }

    // Windows platform detection — enables `gzopen_w` (wide-character path variant)
    // and other Windows-specific code paths.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        println!("cargo:rustc-cfg=windows_platform");
    }

    // Unix platform detection — enables POSIX-specific APIs. The original C zlib
    // checks for `HAVE_UNISTD_H` and `fseeko` availability via CMake; in Rust,
    // the `unix` target family covers all POSIX-compatible platforms.
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if target_family == "unix" {
        println!("cargo:rustc-cfg=unix_platform");
    }

    // Architecture detection for potential SIMD and hardware checksum optimizations.
    // While the core Rust implementation does not use assembly, these flags allow
    // future use of architecture-specific intrinsics (e.g., ARM CRC32 instructions,
    // x86 PCLMULQDQ for CRC, or S390X vector extensions).
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    match target_arch.as_str() {
        "x86_64" | "x86" => {
            println!("cargo:rustc-cfg=target_arch_x86");
        }
        "aarch64" | "arm" => {
            println!("cargo:rustc-cfg=target_arch_arm");
        }
        "s390x" => {
            println!("cargo:rustc-cfg=target_arch_s390x");
        }
        _ => {}
    }
}

/// Emits `cargo:rerun-if-changed` directives for all input files that this build
/// script depends on, ensuring Cargo re-runs the script when any input is modified.
///
/// Per Cargo documentation, if any `rerun-if-changed` directive is emitted, Cargo
/// will **only** re-run the build script when one of the listed files changes (plus
/// `build.rs` itself, which is always tracked).
fn emit_rerun_directives() {
    // The version script is read and copied during the build.
    println!("cargo:rerun-if-changed=zlib.map");
    // The build script itself — Cargo tracks this automatically, but being explicit
    // ensures clarity and prevents surprises if Cargo's behavior changes.
    println!("cargo:rerun-if-changed=build.rs");
    // The crate manifest may affect feature flags and version information.
    println!("cargo:rerun-if-changed=Cargo.toml");
}
