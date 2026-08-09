// THE FEATURE GATE. `libz-compat` is what turns the exported `extern "C"` surface on, so with
// the feature off there is no crate-root function surface to reach and this whole binary
// compiles away. `--no-default-features` is a supported configuration and must stay one, which
// is why the gate is here rather than expressed as a `compile_error!`.
//
// A gate that compiled away *silently* would be the one way this suite could fail without
// saying so: the binary would report "ok" while asserting nothing. It is therefore verified by
// counting, not by colour -- `cargo test -p libz-rs-sys --test rust_api_surface` must report a
// non-zero number of tests under the default feature set, and exactly zero under
// `--no-default-features`.
#![cfg(feature = "libz-compat")]
// The workspace denies the panic-prone family -- `unwrap_used`, `expect_used`,
// `indexing_slicing`, `panic` -- in `[workspace.lints.clippy]`, and every target of this package
// inherits it through `[lints] workspace = true`. That policy is right for `src/**`, where a
// panic would abort a C caller's process, and wrong here: a test asserts, and a failing
// assertion panics. `clippy.toml` already sets `allow-unwrap-in-tests`, `allow-expect-in-tests`
// and `allow-panic-in-tests`, but those keys key on `#[test]` context only, so the file-scope
// helpers below still need this -- and `indexing_slicing` has no in-tests key at all. Nothing
// else is relaxed.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! Integration tests for the crate-root Rust surface of `libz-rs-sys`.
//!
//! This suite exists because of a property no other test in the workspace can establish: that
//! an **external** crate can name this library's exported functions. The seven export modules
//! are private -- `#[no_mangle]` puts their symbols in the dynamic table without any help from
//! Rust visibility -- so the only Rust path to `deflate`, `inflate`, `gzopen` and the other
//! ninety of them is the curated `pub use` list at the crate root. If that list were missing or
//! incomplete, a consumer would get `E0603: module 'deflate' is private` and every downstream
//! Rust consumer would be blocked: `zlib-rs-differential`'s byte-identity and interoperability
//! suites, the five `fuzz/` targets, the three benches and this crate's own sibling test
//! binaries.
//!
//! # Why an integration test, and not a unit test
//!
//! A `#[cfg(test)] mod tests` block inside `src/` sees the crate's *private* interior and would
//! pass whether or not anything was re-exported. An integration test is compiled as a separate
//! crate that links this one as an ordinary `rlib`, so it sees exactly what `zlib-rs-differential`
//! sees -- and nothing else. **Every path below therefore goes through `z::`, the crate root.**
//! Not one line declares an `extern "C"` block of its own, deliberately: a hand-written
//! declaration compiles whether or not the definition behind it agrees, so it would prove
//! nothing about this library. Naming the real item is the only form that does.
//!
//! ★ The extern crate name is `z`, not `libz_rs_sys`. `Cargo.toml` declares `[lib] name = "z"`
//! so that the build emits `libz.so` and `libz.a`, and a library target's name is also its
//! extern crate name; a package's own test targets link it under that name with no opportunity
//! to rename. The two out-of-package Rust consumers rename the dependency and write
//! `libz_rs_sys::…` instead. Neither spelling reaches a C caller, which resolves the symbols
//! through the dynamic table where Rust crate names do not exist.
//!
//! # What is deliberately *not* here
//!
//! Three names this crate compiles are absent from the crate root on purpose, all three hidden
//! from the shared library's dynamic table by `zlib.map`: `inflate_table`, whose only caller is
//! the unmodified `test/infcover.c` linking `libz.a`, and the two `_zlib_rs_gzprintf_*` helpers,
//! whose only caller is `csrc/gzprintf_shim.c`. Their absence is asserted here arithmetically,
//! by the tally in [`the_families_reconcile_to_the_export_symbol_count`]; that they cannot be
//! *named* is asserted by the `compile_fail` examples in the crate documentation, which is the
//! one mechanism that can express "this must not compile" without a third-party crate.
//!
//! The boundary machinery is absent for the same reason and is checked the same way. The
//! allocator adapter that invokes a caller's `zalloc`, the raw-slice constructors, the tagged
//! state block and the install/check/take operations that round-trip the opaque `state`
//! pointer, and the panic guard with its per-family refusal values are all `pub(crate)` inside
//! private modules — so no test in this file can name them, which is the property those
//! `compile_fail` examples pin. What this file asserts is the positive half: that the C
//! contract *is* reachable, name by name, and that nothing else had to be exposed to get it
//! there. [`the_abi_mirror_types_are_nameable_through_the_crate_root`] is the type-level
//! counterpart — every `#[repr(C)]` mirror and alias a consumer needs in order to declare a
//! `z_stream` and call `deflate`, and not one item beyond them.
//!
//! The exhaustive per-argument behaviour of these functions is not this suite's business
//! either. It belongs to the safe core's own integration suites, to the differential harness
//! against the C oracle, and to the relinked C drivers. What is asserted here is reachability,
//! the documented per-family counts, and enough live calls to prove the `rlib` path reaches real
//! code rather than merely type-checking.

use core::ffi::{c_int, c_void, CStr};
use core::mem::{align_of, size_of};

/// Names a family of exported functions and pairs each name with the address the crate root
/// hands back for it.
///
/// The cast to `*const c_void` is what forces the compiler to *resolve* each path: an
/// unresolvable name is a compile error, which is the outcome this suite is built to detect.
/// `stringify!` carries the spelling along so a failure names the function rather than an index.
macro_rules! family {
    ($($name:ident),+ $(,)?) => {
        [$((stringify!($name), z::$name as *const c_void)),+]
    };
}

/// The eleven Adler-32 and CRC-32 entry points -- `zlib.h` L1809-L1901.
fn checksum_family() -> Vec<(&'static str, *const c_void)> {
    family![
        adler32,
        adler32_combine,
        adler32_combine64,
        adler32_z,
        crc32,
        crc32_combine,
        crc32_combine64,
        crc32_combine_gen,
        crc32_combine_gen64,
        crc32_combine_op,
        crc32_z,
    ]
    .to_vec()
}

/// The ten one-shot wrappers and their `size_t` forms -- `zlib.h` L1271-L1337.
fn compress_family() -> Vec<(&'static str, *const c_void)> {
    family![
        compress,
        compress2,
        compress2_z,
        compressBound,
        compressBound_z,
        compress_z,
        uncompress,
        uncompress2,
        uncompress2_z,
        uncompress_z,
    ]
    .to_vec()
}

/// The seventeen `deflate*` entry points -- `zlib.h` L254-L836.
///
/// `deflateInit` and `deflateInit2` are not among them and cannot be: `zlib.h` defines both as
/// macros over the `_`-suffixed functions, so neither is a symbol.
fn deflate_family() -> Vec<(&'static str, *const c_void)> {
    family![
        deflate,
        deflateBound,
        deflateBound_z,
        deflateCopy,
        deflateEnd,
        deflateGetDictionary,
        deflateInit2_,
        deflateInit_,
        deflateParams,
        deflatePending,
        deflatePrime,
        deflateReset,
        deflateResetKeep,
        deflateSetDictionary,
        deflateSetHeader,
        deflateTune,
        deflateUsed,
    ]
    .to_vec()
}

/// The eighteen `inflate*` entry points -- `zlib.h` L405-L1106.
///
/// `inflateInit` and `inflateInit2` are absent for the reason their deflate counterparts are,
/// and `inflate_table` because `zlib.map` hides it.
fn inflate_family() -> Vec<(&'static str, *const c_void)> {
    family![
        inflate,
        inflateCodesUsed,
        inflateCopy,
        inflateEnd,
        inflateGetDictionary,
        inflateGetHeader,
        inflateInit2_,
        inflateInit_,
        inflateMark,
        inflatePrime,
        inflateReset,
        inflateReset2,
        inflateResetKeep,
        inflateSetDictionary,
        inflateSync,
        inflateSyncPoint,
        inflateUndermine,
        inflateValidate,
    ]
    .to_vec()
}

/// The three `inflateBack*` entry points -- `zlib.h` L1138-L1208 and L1913.
fn infback_family() -> Vec<(&'static str, *const c_void)> {
    family![inflateBack, inflateBackEnd, inflateBackInit_].to_vec()
}

/// The four introspection entry points -- `zlib.h` L224, L2035, L2062 and L2064.
fn util_family() -> Vec<(&'static str, *const c_void)> {
    family![get_crc_table, zError, zlibCompileFlags, zlibVersion].to_vec()
}

/// The thirty `gz*` entry points -- `zlib.h` L1357-L1801 and L1966-L2013.
///
/// `gzprintf` and `gzvprintf` are absent because this crate does not define them: the packaging
/// link takes both from `csrc/gzprintf_shim.c`, since stable Rust at the declared MSRV can
/// neither define a C-variadic function nor name a `va_list`.
#[cfg(feature = "gz")]
fn gz_family() -> Vec<(&'static str, *const c_void)> {
    let mut family = family![
        gzbuffer,
        gzclearerr,
        gzclose,
        gzclose_r,
        gzclose_w,
        gzdirect,
        gzdopen,
        gzeof,
        gzerror,
        gzflush,
        gzfread,
        gzfwrite,
        gzgetc,
        gzgetc_,
        gzgets,
        gzoffset,
        gzoffset64,
        gzopen,
        gzopen64,
        gzputc,
        gzputs,
        gzread,
        gzrewind,
        gzseek,
        gzseek64,
        gzsetparams,
        gztell,
        gztell64,
        gzungetc,
        gzwrite,
    ]
    .to_vec();
    family.append(&mut gz_wide_path_family());
    family
}

/// The Windows-only wide-path entry point -- `zlib.h` L2042.
///
/// `zlib.h` declares `gzopen_w` under `#if defined(_WIN32)`, so on every other target a caller
/// has no declaration to link against and this crate compiles no definition. Written as a pair
/// of `cfg`-selected functions rather than as a `cfg` attribute on a statement so that the
/// caller's arithmetic is the same shape on both targets.
#[cfg(all(feature = "gz", windows))]
fn gz_wide_path_family() -> Vec<(&'static str, *const c_void)> {
    family![gzopen_w].to_vec()
}

/// Empty off Windows, where the wide-path entry point does not exist.
#[cfg(all(feature = "gz", not(windows)))]
fn gz_wide_path_family() -> Vec<(&'static str, *const c_void)> {
    Vec::new()
}

/// Every contract function, in module order.
fn every_contract_function() -> Vec<(&'static str, *const c_void)> {
    let mut all = checksum_family();
    all.append(&mut compress_family());
    all.append(&mut deflate_family());
    all.append(&mut inflate_family());
    all.append(&mut infback_family());
    all.append(&mut util_family());
    #[cfg(feature = "gz")]
    all.append(&mut gz_family());
    all
}

/// The number of `gz*` functions the crate root publishes in this configuration.
///
/// Thirty with the `gz` feature, thirty-one on Windows where `gzopen_w` joins them, none at all
/// when the feature is off -- in which case the `gzFile` layer is not compiled and there is
/// nothing to reach.
const GZ_COUNT: usize = if cfg!(feature = "gz") {
    if cfg!(windows) {
        31
    } else {
        30
    }
} else {
    0
};

/// Every family's documented size, which is what the crate root's own per-family tally records.
const CONTRACT_COUNT: usize = 11 + 10 + 17 + 18 + 3 + 4 + GZ_COUNT;

#[test]
fn each_family_carries_the_documented_number_of_functions() {
    assert_eq!(checksum_family().len(), 11, "checksum");
    assert_eq!(compress_family().len(), 10, "compress");
    assert_eq!(deflate_family().len(), 17, "deflate");
    assert_eq!(inflate_family().len(), 18, "inflate");
    assert_eq!(infback_family().len(), 3, "infback");
    assert_eq!(util_family().len(), 4, "util");
    #[cfg(feature = "gz")]
    assert_eq!(gz_family().len(), GZ_COUNT, "gz");
    assert_eq!(every_contract_function().len(), CONTRACT_COUNT);
}

#[test]
fn every_contract_function_resolves_to_a_live_address() {
    for (name, address) in every_contract_function() {
        assert!(
            !address.is_null(),
            "{name} resolved to a null address, which means it was not linked"
        );
    }
}

#[test]
fn no_function_is_listed_twice() {
    let mut names: Vec<&'static str> = every_contract_function()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let listed = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        listed,
        "a name appears twice, so the count above is not the count of distinct functions"
    );
}

#[test]
fn the_families_reconcile_to_the_export_symbol_count() {
    // The three names this crate compiles and deliberately does not re-export: `inflate_table`
    // plus the two `_zlib_rs_gzprintf_*` helpers, all three in `zlib.map`'s `local:` set, which
    // is what keeps them out of the shared library's dynamic table.
    const HIDDEN: usize = 3;
    // Every `#[no_mangle]` item the seven export modules compile on a non-Windows target: 17
    // deflate + 19 inflate (18 plus `inflate_table`) + 3 infback + 10 compress + 32 gz (30 plus
    // the two helpers) + 11 checksum + 4 util.
    const NO_MANGLE_ITEMS: usize = 96;
    // `gzprintf` and `gzvprintf`, which the packaging link takes from `csrc/gzprintf_shim.c`.
    const FROM_THE_C_SHIM: usize = 2;
    // What `nm -D --defined-only --extern-only` reports for the packaged shared library, less
    // its 16 symbol-version nodes: 111 - 16 = 95.
    const EXPORTED_FUNCTIONS: usize = 95;

    // Only the default feature set builds the whole surface, and only a non-Windows target has
    // exactly thirty `gz*` functions, so the arithmetic is asserted where it applies rather
    // than being made vague enough to hold everywhere.
    if cfg!(feature = "gz") && !cfg!(windows) {
        assert_eq!(
            CONTRACT_COUNT + HIDDEN,
            NO_MANGLE_ITEMS,
            "the crate-root list plus the hidden names must account for every compiled export"
        );
        assert_eq!(
            CONTRACT_COUNT + FROM_THE_C_SHIM,
            EXPORTED_FUNCTIONS,
            "the crate-root list plus the C shim must be the packaged library's export count"
        );
    }
}

#[test]
fn the_crate_root_path_reaches_live_code() {
    // Not merely nameable: called, through the same `rlib` path a differential test uses.
    // SAFETY: `zlibVersion` returns a pointer to the library's own `'static`
    // NUL-terminated version literal (`zlib.h` L224), so the scan terminates inside that
    // literal and the borrow outlives this test.
    let reported = unsafe { CStr::from_ptr(z::zlibVersion()) };
    assert_eq!(
        reported,
        z::ZLIB_VERSION,
        "the version the library reports must be the one the header declares"
    );
    assert_eq!(reported.to_str().unwrap(), "1.3.2.1-motley");

    // `zlib.h` L1307: an upper bound, never a status. 13 bytes of overhead for an empty input.
    assert_eq!(z::compressBound(0), 13);

    // `zlib.h` L1813-L1814: a null buffer yields the required initial value of each checksum.
    // SAFETY: the documented null-buffer form -- `zlib.h` L1813-L1814 defines a null `buf`
    // with a zero length as the request for the initial value, and neither entry point
    // dereferences the pointer on that path.
    let adler = unsafe { z::adler32(0, core::ptr::null(), 0) };
    assert_eq!(adler, 1);
    // SAFETY: as above, for the other checksum.
    let crc = unsafe { z::crc32(1, core::ptr::null(), 0) };
    assert_eq!(crc, 0);

    // `zError` answers with the `z_errmsg` slot the status selects -- `zutil.c` L13-L24, where
    // `Z_DATA_ERROR` is "data error" -- and `get_crc_table` with the table `crc32.h` generates,
    // whose first entry is 0 and second 0x77073096.
    // SAFETY: `zError` returns a slot of the library's `'static` `z_errmsg` table
    // (`zutil.c` L13-L24), so the pointer is non-null, NUL-terminated and outlives the scan.
    let message = unsafe { CStr::from_ptr(z::zError(z::Z_DATA_ERROR)) };
    assert_eq!(message.to_str().unwrap(), "data error");
    let table = z::get_crc_table();
    assert!(!table.is_null());
    // SAFETY: `get_crc_table` returns a pointer to the library's `'static` 256-entry
    // table (`zlib.h` L2035), asserted non-null just above, so the first two entries are
    // in bounds and initialised.
    let first = unsafe { *table };
    assert_eq!(first, 0);
    // SAFETY: as above -- index 1 of a 256-entry table.
    let second = unsafe { *table.add(1) };
    assert_eq!(second, 0x7707_3096);

    // `zlibCompileFlags` describes the build rather than the run, so the assertion is on the
    // type-size nibbles it is defined to carry: bits 0-1 `sizeof(uInt)`, 2-3 `sizeof(uLong)`,
    // 4-5 `sizeof(voidpf)`, 6-7 `sizeof(z_off_t)`, each encoded 2->0, 4->1, 8->2, else 3.
    let flags = z::zlibCompileFlags();
    assert_eq!(flags & 0x3, encoded_size(size_of::<z::uInt>()));
    assert_eq!((flags >> 2) & 0x3, encoded_size(size_of::<z::uLong>()));
    assert_eq!((flags >> 4) & 0x3, encoded_size(size_of::<z::voidpf>()));
    assert_eq!((flags >> 6) & 0x3, encoded_size(size_of::<z::z_off_t>()));
}

/// `zutil.c` L33-L44's size encoding: 2 -> 0, 4 -> 1, 8 -> 2, anything else -> 3.
fn encoded_size(size: usize) -> z::uLong {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

#[test]
fn a_one_shot_round_trip_runs_entirely_through_the_crate_root() {
    // `test/example.c`'s own payload, so the fixture is the acceptance suite's rather than an
    // invented one.
    let source: &[u8] = b"hello, hello!";
    let source_len = z::uLong::try_from(source.len()).unwrap();

    let bound = usize::try_from(z::compressBound(source_len)).unwrap();
    let mut compressed = vec![0_u8; bound];
    let mut compressed_len = z::uLong::try_from(compressed.len()).unwrap();
    // SAFETY: both buffers are live and their lengths are the ones passed; the source is
    // non-null with a matching length, which is `compress2`'s documented contract.
    //
    // `addr_of_mut!` rather than `&raw mut`, which is stable only from Rust 1.82 and would
    // break the declared 1.80 floor -- the same spelling the rest of the workspace uses.
    let status = unsafe {
        z::compress2(
            compressed.as_mut_ptr(),
            core::ptr::addr_of_mut!(compressed_len),
            source.as_ptr(),
            source_len,
            z::Z_BEST_COMPRESSION,
        )
    };
    assert_eq!(status, z::Z_OK);
    assert!(compressed_len > 0 && usize::try_from(compressed_len).unwrap() <= bound);

    let mut restored = vec![0_u8; source.len()];
    let mut restored_len = z::uLong::try_from(restored.len()).unwrap();
    // SAFETY: as above; the compressed slice is the one just written, truncated to the length
    // `compress2` reported.
    let status = unsafe {
        z::uncompress(
            restored.as_mut_ptr(),
            core::ptr::addr_of_mut!(restored_len),
            compressed.as_ptr(),
            compressed_len,
        )
    };
    assert_eq!(status, z::Z_OK);
    assert_eq!(usize::try_from(restored_len).unwrap(), source.len());
    assert_eq!(restored, source);
}

#[test]
fn the_abi_mirror_types_are_nameable_through_the_crate_root() {
    // The four `#[repr(C)]` mirrors. The exhaustive size-and-offset matrix belongs to the
    // layout suite; what is asserted here is that an external crate can *name* each one, which
    // it must be able to do before it can declare a `z_stream` to pass to `deflate`.
    assert_eq!(size_of::<z::z_stream>(), 112);
    assert_eq!(align_of::<z::z_stream>(), 8);
    assert_eq!(size_of::<z::gz_header>(), 80);
    assert_eq!(size_of::<z::gzFile_s>(), 24);
    assert_eq!(size_of::<z::code>(), 4);

    // The pointer aliases, each asserted against the type it points at rather than against a
    // number, because that is the relationship `zlib.h` fixes.
    assert_eq!(size_of::<z::z_streamp>(), size_of::<*mut z::z_stream>());
    assert_eq!(size_of::<z::gz_headerp>(), size_of::<*mut z::gz_header>());
    assert_eq!(size_of::<z::gzFile>(), size_of::<*mut z::gzFile_s>());
    assert_eq!(size_of::<z::voidpf>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<z::voidpc>(), size_of::<*const c_void>());
    assert_eq!(size_of::<z::voidp>(), size_of::<*mut c_void>());

    // The `zconf.h` primitive aliases. `uLong` is `unsigned long`, which is why it is asserted
    // against `c_ulong` and never against a fixed width: it is 8 bytes on this LP64 target and
    // 4 on LLP64 Windows, and a fixed-width spelling would be wrong on one of them.
    assert_eq!(size_of::<z::Byte>(), 1);
    assert_eq!(size_of::<z::Bytef>(), size_of::<z::Byte>());
    assert_eq!(size_of::<z::uInt>(), size_of::<core::ffi::c_uint>());
    assert_eq!(size_of::<z::uIntf>(), size_of::<z::uInt>());
    assert_eq!(size_of::<z::uLong>(), size_of::<core::ffi::c_ulong>());
    assert_eq!(size_of::<z::uLongf>(), size_of::<z::uLong>());
    assert_eq!(size_of::<z::charf>(), size_of::<core::ffi::c_char>());
    assert_eq!(size_of::<z::intf>(), size_of::<c_int>());
    assert_eq!(size_of::<z::z_crc_t>(), 4);
    assert_eq!(size_of::<z::z_size_t>(), size_of::<usize>());
    assert_eq!(size_of::<z::z_off64_t>(), 8);

    // The four nullable hooks. `Option<unsafe extern "C" fn(..)>` is the FFI-safe mirror of a C
    // function pointer in which `Z_NULL` is `None`, and the null-pointer optimisation is what
    // keeps it one pointer wide -- so a caller's `Z_NULL` crosses the boundary intact.
    assert_eq!(size_of::<z::alloc_func>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<z::free_func>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<z::in_func>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<z::out_func>(), size_of::<*mut c_void>());
}

#[test]
fn the_constant_surface_is_nameable_through_the_crate_root() {
    // A consumer that writes `Z_FINISH` rather than `4` needs these as items; `zlib.h`
    // publishes them as `#define`s, which a Rust crate cannot import.
    assert_eq!([z::Z_OK, z::Z_STREAM_END, z::Z_NEED_DICT], [0, 1, 2]);
    assert_eq!(
        [
            z::Z_ERRNO,
            z::Z_STREAM_ERROR,
            z::Z_DATA_ERROR,
            z::Z_MEM_ERROR,
            z::Z_BUF_ERROR,
            z::Z_VERSION_ERROR
        ],
        [-1, -2, -3, -4, -5, -6]
    );
    assert_eq!(
        [
            z::Z_NO_FLUSH,
            z::Z_PARTIAL_FLUSH,
            z::Z_SYNC_FLUSH,
            z::Z_FULL_FLUSH,
            z::Z_FINISH,
            z::Z_BLOCK,
            z::Z_TREES
        ],
        [0, 1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        [
            z::Z_FILTERED,
            z::Z_HUFFMAN_ONLY,
            z::Z_RLE,
            z::Z_FIXED,
            z::Z_DEFAULT_STRATEGY
        ],
        [1, 2, 3, 4, 0]
    );
    assert_eq!(
        [
            z::Z_NO_COMPRESSION,
            z::Z_BEST_SPEED,
            z::Z_BEST_COMPRESSION,
            z::Z_DEFAULT_COMPRESSION
        ],
        [0, 1, 9, -1]
    );
    assert_eq!(
        [z::Z_BINARY, z::Z_TEXT, z::Z_ASCII, z::Z_UNKNOWN],
        [0, 1, 1, 2]
    );
    assert_eq!([z::Z_DEFLATED, z::Z_NULL], [8, 0]);
    assert_eq!([z::MAX_WBITS, z::MAX_MEM_LEVEL], [15, 9]);

    // The version identity, which `deflateInit_` and `inflateInit_` compare a caller's
    // compile-time string against -- `zlib.h` L44-L45.
    assert_eq!(z::ZLIB_VERSION.to_str().unwrap(), "1.3.2.1-motley");
    assert_eq!(z::ZLIB_VERNUM, 0x1321);
    assert_eq!(
        [
            z::ZLIB_VER_MAJOR,
            z::ZLIB_VER_MINOR,
            z::ZLIB_VER_REVISION,
            z::ZLIB_VER_SUBREVISION
        ],
        [1, 3, 2, 1]
    );
}
