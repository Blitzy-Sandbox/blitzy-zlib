// THE FEATURE GATE. `libz-compat` is what turns the exported `extern "C"` surface on, and every
// single call this suite makes is one of those exports. With the feature off there is no
// `deflateInit_`, no `inflate`, no `zlibVersion` -- nothing to port `test/example.c` against -- so
// the whole binary compiles away. `--no-default-features` is a supported configuration for this
// crate and must stay one, which is why the gate is expressed here rather than as a
// `compile_error!`.
//
// A gate that compiled away *silently* would be the one way this suite could fail without saying
// so, so it is verified by counting rather than by colour: `cargo test -p libz-rs-sys --test
// c_api_parity` must report a non-zero number of tests under the default feature set, and exactly
// zero under `--no-default-features`. The `gz` half of the surface is gated separately, at
// [`test_gzio`], so that `--features libz-compat` alone also compiles.
#![cfg(feature = "libz-compat")]
// The workspace denies the panic-prone family -- `unwrap_used`, `expect_used`, `indexing_slicing`,
// `panic` -- in `[workspace.lints.clippy]`, and every target of this package inherits it through
// `[lints] workspace = true`. That policy is right for `src/**`, where a panic would abort a C
// caller's process, and wrong here: the C driver's `CHECK_ERR` macro *is* an abort, and the Rust
// spelling of an abort in a test is a panic. `clippy.toml` already sets `allow-unwrap-in-tests`,
// `allow-expect-in-tests` and `allow-panic-in-tests`, but those keys key on `#[test]` context only,
// so the file-scope helpers below still need this -- and `indexing_slicing` has no in-tests key at
// all, while `compr[3]++` (`test/example.c` L357) is load-bearing rather than incidental. Nothing
// else is relaxed: the cast family stays denied and is answered with checked conversions, not with
// an allow.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! `test/example.c`, ported to Rust and run against this crate's exported C ABI.
//!
//! AAP 0.4.1.3 specifies this file as a "Rust port of the 10 ordered test functions, following the
//! `CHECK_ERR` structure", sourced from `test/example.c`. That C driver is `REFERENCE` mode: it is
//! read and never edited, and it continues to be compiled **unmodified** and linked against this
//! library's artifacts, which is the operational definition of drop-in compatibility (AAP 0.6.4.3).
//! This suite **complements** that relink; it does not replace it, and no assertion here is weakened
//! on the grounds that the C driver also covers it.
//!
//! # The structure is deliberate, and it is not the idiomatic one
//!
//! Every literal, buffer size, loop shape and call ordering below is `test/example.c`'s, reproduced
//! rather than improved, and every helper names the C line range it mirrors. AAP 0.7.1(c) makes
//! behavioural fidelity binding, and line-by-line comparability against the oracle *is* this file's
//! reason to exist: a cleaner-but-different port is a worse artifact, because it can no longer be
//! diffed against the thing it is supposed to agree with.
//!
//! ★ **The ten helpers run inside ONE `#[test]`, in `main`'s exact order.** They are not ten tests;
//! they are ten stages of one, and they are coupled through shared state in four separate ways:
//!
//! * `compr` and `uncompr` are one pair of buffers threaded through all ten (`test/example.c`
//!   L515-L516).
//! * [`test_flush`] *rewrites* `compr_len` to the actual output size (L367), [`test_sync`] consumes
//!   that value, and `main` resets it to `3 * uncompr_len` at L543 before the dictionary stages.
//!   Omitting that reset produces a `Z_BUF_ERROR` deep inside [`test_dict_deflate`].
//! * [`test_flush`] corrupts the first compressed block with `compr[3]++` (L357) *specifically* so
//!   that [`test_sync`] has something for `inflateSync` to recover from. It is not a bug.
//! * [`test_large_inflate`] decodes exactly what [`test_large_deflate`] wrote, and
//!   [`test_dict_inflate`] needs the Adler-32 `dict_id` that [`test_dict_deflate`] captured.
//!
//! Cargo runs `#[test]` functions on parallel threads with independent state, so splitting these
//! into ten tests would break all four dependencies and make the results non-deterministic. The
//! three *stateless* checks -- the startup gate, the `inflateBack` argument ladder and the `zError`
//! table -- are separate `#[test]`s precisely because they share nothing.
//!
//! # The unsafe boundary
//!
//! Every exported entry point is an `extern "C"` function over raw pointers, so every call is an
//! `unsafe` operation. Rather than repeat one invariant across some sixty call sites, this file
//! follows the pattern the workspace lint policy prescribes for exactly this case: each entry point
//! gets a **named call gate** that owns a single documented `unsafe` block and states the invariant
//! once. The gates are module-private, so the obligation they carry cannot escape this file, and
//! every caller in it discharges that obligation by construction -- streams come from
//! [`new_stream`], and the pointer/length pairs installed in them always describe a live [`Vec`] or
//! a `'static` array that outlives the call.
//!
//! # What the C driver's own allocator choice means here
//!
//! `test/example.c` L60-L61 sets `zalloc` and `zfree` to `Z_NULL`, which selects the library's
//! **internal default** allocation path, and [`new_stream`] reproduces that exactly with `None`.
//! That path is plain `malloc` in C (`zutil.c` L299-L308) -- *uninitialised*, not zeroed -- so
//! nothing here may assume the library hands back zero-filled state. The stricter form of that
//! check lives in `test/infcover.c`, whose tracking allocator fills every block with `0xa5`
//! (L87) and whose `mem_done` catches leaks, non-LIFO frees and rogue frees; this suite exercises
//! the default path that a caller who supplies no hooks actually gets.

// `c_char` is imported UNCONDITIONALLY, and that is not a tidiness choice -- it is what makes
// `--no-default-features --features libz-compat` compile. It once carried `#[cfg(feature = "gz")]`
// on the theory that only the `gzFile` stages reach it, and that theory was wrong: the version
// string handed to `deflateInit2_` and `inflateInit2_` is a `*const c_char`, and three of those
// calls live in [`inflate_reset_keep_retains_the_window`], which is gated on nothing. With
// `libz-compat` on and `gz` off the import configured out while its uses stayed, and the test
// target failed to build with three `error[E0425]: cannot find type c_char in this scope`.
//
// There is no `unused_imports` risk to trade against, in any configuration: this whole file is
// `#![cfg(feature = "libz-compat")]`, so it either compiles with those three ungated uses present
// or it does not compile at all. A gate here can therefore only ever subtract.
use core::ffi::{c_char, c_int, c_uint, CStr};
use core::mem::size_of;
use core::ptr::{self, addr_of_mut};

use z::{
    uInt, uLong, z_stream, ZLIB_VERSION, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_BUF_ERROR,
    Z_DATA_ERROR, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_ERRNO, Z_FILTERED, Z_FINISH,
    Z_FULL_FLUSH, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_OK, Z_STREAM_END,
    Z_STREAM_ERROR, Z_SYNC_FLUSH, Z_VERSION_ERROR,
};

// ---------------------------------------------------------------------------------------------
//  Fixtures -- `test/example.c` L35-L61 and L499-L516
// ---------------------------------------------------------------------------------------------

/// `static z_const char hello[] = "hello, hello!";` -- `test/example.c` L35.
///
/// Carried as a NUL-terminated byte literal so that `HELLO.len()` *is* C's
/// `(uLong) strlen(hello) + 1`, with no `strlen` call and no chance of an off-by-one. The comment
/// at L36-L38 records why this string and not "hello world": the repeated "hello" stresses the
/// match finder harder.
const HELLO: &[u8] = b"hello, hello!\0";

/// C's `len` in five of the ten helpers: `strlen(hello) + 1`, i.e. **14**, the trailing NUL
/// included.
///
/// Asserted rather than merely computed, because 13 would silently change what is compressed.
const HELLO_LEN: usize = HELLO.len();
const _: () = assert!(HELLO_LEN == 14);

/// `static const char dictionary[] = "hello";` -- `test/example.c` L40.
const DICTIONARY: &[u8] = b"hello\0";

/// C's `(int) sizeof(dictionary)`, i.e. **6** -- the NUL is part of the dictionary.
///
/// Asserted deliberately. Passing 5 would set a *different* dictionary and therefore produce a
/// different Adler-32 id, and the mismatch would surface only as an unexplained failure inside
/// [`test_dict_inflate`].
const DICTIONARY_LEN: usize = DICTIONARY.len();
const _: () = assert!(DICTIONARY_LEN == 6);

/// `uLong uncomprLen = 20000;` -- `test/example.c` L499.
const UNCOMPR_LEN: usize = 20_000;

/// `uLong comprLen = 3 * uncomprLen;` -- `test/example.c` L500.
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;
const _: () = assert!(COMPR_LEN == 60_000);

/// `strcpy((char *) uncompr, "garbage")` -- the pre-fill five helpers apply to the output buffer
/// before decompressing into it (`test/example.c` L74, L212, L304, L378, L453).
///
/// Eight bytes including the terminator, exactly as C's `strcpy` writes. Its purpose is to make a
/// silent failure loud: if the library wrote nothing, the C-string comparison would find "garbage"
/// rather than a stale copy of the expected answer.
const GARBAGE: &[u8] = b"garbage\0";

// ---------------------------------------------------------------------------------------------
//  `CHECK_ERR`, and the small helpers the C driver spells with libc
// ---------------------------------------------------------------------------------------------

/// `CHECK_ERR(err, msg)` -- `test/example.c` L28-L33.
///
/// The C macro prints `"<msg> error: <err>"` to stderr and calls `exit(1)`. The Rust equivalent of
/// that abort is a panic, and the message shape is reproduced character for character so that a
/// failure here and a failure of the relinked C driver read the same way.
fn check_err(err: c_int, msg: &str) {
    assert!(err == Z_OK, "{msg} error: {err}");
}

/// A `z_stream` with every field zeroed and the allocator hooks left null -- `test/example.c`
/// L177-L179, L214-L216, L251-L253, L306-L308, L343-L345, L380-L382, L418-L420 and L455-L457.
///
/// The C driver sets only `zalloc`, `zfree` and `opaque` before its `deflateInit`/`inflateInit`
/// call and leaves the rest of the struct uninitialised; the init functions then write what they
/// need. Building the whole struct explicitly here is both stricter and cheaper than reproducing
/// that: it needs no `unsafe`, no `MaybeUninit::zeroed`, and it makes the three fields the C code
/// *does* set visible as the deliberate choices they are.
///
/// `None` for the two hooks is C's `Z_NULL`, which selects the library's internal default
/// allocator -- see the note on that at the head of this file. `z_stream` deliberately does not
/// derive `Default`, because a zeroed stream is not yet a valid one.
fn new_stream() -> z_stream {
    z_stream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null(),
        state: ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// `(int) sizeof(z_stream)` -- the second half of the handshake the `deflateInit`/`inflateInit`
/// macros arrange (`zlib.h` L232, L543 and L1113).
///
/// Checked rather than cast: a `size_of` that did not fit a `c_int` would mean the ABI mirror had
/// grown past anything `zlib.h` could describe, which should fail loudly rather than truncate.
fn stream_size() -> c_int {
    c_int::try_from(size_of::<z_stream>()).expect("sizeof(z_stream) fits in a C int")
}

/// `strcpy((char *) buf, "garbage")`.
fn strcpy_garbage(buf: &mut [u8]) {
    buf[..GARBAGE.len()].copy_from_slice(GARBAGE);
}

/// C's view of a byte buffer as a string: everything up to the first NUL.
///
/// This is what makes the comparisons below match `strcmp` semantics exactly. Comparing whole
/// buffers instead would let the [`GARBAGE`] pre-fill, or any trailing byte the library happened
/// not to touch, mask a real mismatch in either direction.
fn as_cstr(buf: &[u8]) -> &CStr {
    CStr::from_bytes_until_nul(buf).expect("the library writes a NUL-terminated result here")
}

/// `strcmp((char *) buf, hello) == 0` -- `test/example.c` L79, L127, L235 and L485.
fn assert_equals_hello(buf: &[u8], what: &str) {
    let got = as_cstr(buf);
    let want = CStr::from_bytes_with_nul(HELLO).expect("HELLO is NUL-terminated");
    assert!(got == want, "bad {what}: {got:?}");
}

// ---------------------------------------------------------------------------------------------
//  Call gates -- the audited unsafe boundary
// ---------------------------------------------------------------------------------------------
//
// One gate per exported entry point, each owning a single documented `unsafe` block. Two properties
// make this the right shape rather than merely the shorter one:
//
//   * The invariant is stated ONCE per entry point instead of some sixty times, which is what the
//     workspace lint policy prescribes for a repeated call ("a named call gate that owns the single
//     unsafe block, rather than copied onto every call site"). Sixty near-identical comments are not
//     a control; six distinct ones are.
//   * The ported helper bodies below then read as `err = deflate(&mut c_stream, Z_NO_FLUSH);` --
//     the C statement with a borrow where C has an address-of -- which is the line-by-line
//     comparability this file exists to provide.
//
// The obligation every gate carries is the same, and it is discharged by construction throughout
// this file:
//
//   (a) `strm` is a live `&mut z_stream`, so it is non-null, aligned and uniquely borrowed for the
//       duration of the call -- a reference cannot be otherwise.
//   (b) Where `strm.next_in` / `strm.next_out` are non-null, `avail_in` / `avail_out` bytes really
//       are readable / writable there, because every such pair is installed from a live `Vec` or a
//       `'static` array that outlives the call and is never shortened while the stream holds it.
//   (c) A null `next_in` is only ever paired with `avail_in == 0` -- [`new_stream`] starts both at
//       zero and nothing here sets a length without also setting its pointer. `zlib.h` L91-L92
//       permits that pair explicitly, and the facade branches on the length before forming a slice.
//   (d) `strm.state` is either null or a block this same library allocated through the init entry
//       point, so the facade's tag check accepts or rejects it as the C `deflateStateCheck` /
//       `inflateStateCheck` would.
//
// The gates are module-private, so that obligation cannot escape this file.

/// `deflateInit(strm, level)` -- the `zlib.h` L232 macro, expanded.
///
/// The macro is not a symbol, so the real export is called with the two arguments the macro
/// supplies: the caller's compile-time [`ZLIB_VERSION`] and `sizeof(z_stream)`. Getting either
/// wrong yields `Z_VERSION_ERROR` (`zlib.h` L248) rather than a working stream.
fn deflate_init(strm: &mut z_stream, level: c_int) -> c_int {
    // SAFETY: obligations (a) and (d) at the head of this section. `deflateInit_` reads the
    // caller's three allocator members and writes `state`; it does not touch `next_in`/`next_out`,
    // which is why the C driver may leave them unset until after the call.
    unsafe { z::deflateInit_(strm, level, ZLIB_VERSION.as_ptr(), stream_size()) }
}

/// `deflate(strm, flush)` -- `zlib.h` L254.
fn deflate(strm: &mut z_stream, flush: c_int) -> c_int {
    // SAFETY: obligations (a) through (d) at the head of this section -- this is the one gate that
    // relies on all four, because it is the one that reads through `next_in` and writes through
    // `next_out`.
    unsafe { z::deflate(strm, flush) }
}

/// `deflateEnd(strm)` -- `zlib.h` L305.
fn deflate_end(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (d). The state block is released through the same allocator it
    // was taken from, which is the pairing this call exists to perform.
    unsafe { z::deflateEnd(strm) }
}

/// `deflateParams(strm, level, strategy)` -- `zlib.h` L679.
fn deflate_params(strm: &mut z_stream, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: obligations (a) through (d). A level change may flush pending output through
    // `next_out`, so the output pair must be valid here exactly as it is for `deflate`.
    unsafe { z::deflateParams(strm, level, strategy) }
}

/// `deflateSetDictionary(strm, dictionary, dictLength)` -- `zlib.h` L618.
///
/// Takes the dictionary as a slice so that the pointer and the length cannot disagree; C passes
/// them separately and the driver spells the length `sizeof(dictionary)`.
fn deflate_set_dictionary(strm: &mut z_stream, dictionary: &[u8]) -> c_int {
    let len = c_uint::try_from(dictionary.len()).expect("the dictionary length fits a C unsigned");
    // SAFETY: obligations (a) and (d), plus the pointer/length pair formed here: `dictionary` is a
    // live slice, so `len` bytes are readable at its start for the duration of the call, and the
    // library only reads from it.
    unsafe { z::deflateSetDictionary(strm, dictionary.as_ptr(), len) }
}

/// `inflateInit(strm)` -- the `zlib.h` L543 macro, expanded.
fn inflate_init(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (d), as for `deflate_init`.
    unsafe { z::inflateInit_(strm, ZLIB_VERSION.as_ptr(), stream_size()) }
}

/// `inflate(strm, flush)` -- `zlib.h` L405.
fn inflate(strm: &mut z_stream, flush: c_int) -> c_int {
    // SAFETY: obligations (a) through (d), as for `deflate` -- the direction of travel differs but
    // the pointer/length contract is identical.
    unsafe { z::inflate(strm, flush) }
}

/// `inflateEnd(strm)` -- `zlib.h` L500.
fn inflate_end(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (d), as for `deflate_end`.
    unsafe { z::inflateEnd(strm) }
}

/// `inflateSync(strm)` -- `zlib.h` L951.
fn inflate_sync(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) through (c). The scan runs over the input pair only, which
    // obligation (b) covers.
    unsafe { z::inflateSync(strm) }
}

/// `inflateSetDictionary(strm, dictionary, dictLength)` -- `zlib.h` L913.
fn inflate_set_dictionary(strm: &mut z_stream, dictionary: &[u8]) -> c_int {
    let len = uInt::try_from(dictionary.len()).expect("the dictionary length fits a uInt");
    // SAFETY: obligations (a) and (d), plus the pointer/length pair formed here, exactly as for
    // `deflate_set_dictionary`: a live slice, read only.
    unsafe { z::inflateSetDictionary(strm, dictionary.as_ptr(), len) }
}

/// `compress(dest, destLen, source, sourceLen)` -- `zlib.h` L1271.
///
/// `dest_len` is in/out: the caller sets it to the space available and the library overwrites it
/// with the space used, which is why it is a `&mut`.
fn compress(dest: &mut [u8], dest_len: &mut uLong, source: &[u8]) -> c_int {
    let source_len = uLong::try_from(source.len()).expect("the source length fits a uLong");
    assert_capacity(dest, *dest_len);
    // SAFETY: three pointer/length pairs, all formed from live storage that outlives the call:
    // `*dest_len` bytes are writable at `dest`, which `assert_capacity` has just established;
    // `source_len` bytes are readable at `source`, which is a live slice; and `dest_len` is a
    // unique borrow of a `uLong`, so the in/out write the library performs on it cannot alias.
    // `addr_of_mut!` rather than `&raw mut`, which is stable only from Rust 1.82 and would break
    // the declared 1.80 floor.
    unsafe {
        z::compress(
            dest.as_mut_ptr(),
            addr_of_mut!(*dest_len),
            source.as_ptr(),
            source_len,
        )
    }
}

/// `uncompress(dest, destLen, source, sourceLen)` -- `zlib.h` L1315.
fn uncompress(dest: &mut [u8], dest_len: &mut uLong, source: &[u8]) -> c_int {
    let source_len = uLong::try_from(source.len()).expect("the source length fits a uLong");
    assert_capacity(dest, *dest_len);
    // SAFETY: as for `compress` -- three pairs, all from live storage, with the writable extent
    // established by `assert_capacity` and the library writing only within the `*dest_len` bytes it
    // was told it had.
    unsafe {
        z::uncompress(
            dest.as_mut_ptr(),
            addr_of_mut!(*dest_len),
            source.as_ptr(),
            source_len,
        )
    }
}

/// Establishes the writable extent the two one-shot gates promise the library.
///
/// C has no equivalent because C has no slice to check against: the driver simply passes `comprLen`
/// alongside `compr` and relies on having allocated that much. Here the buffer knows its own
/// length, so the agreement can be checked instead of assumed -- which is what lets the `unsafe`
/// blocks above state it as a fact.
fn assert_capacity(buf: &[u8], claimed: uLong) {
    let available = uLong::try_from(buf.len()).expect("the buffer length fits a uLong");
    assert!(
        available >= claimed,
        "the claimed output space ({claimed}) exceeds the buffer ({available})"
    );
}

// ---------------------------------------------------------------------------------------------
//  The startup gate -- `test/example.c` L501-L513
// ---------------------------------------------------------------------------------------------

/// The two-bit code `zlibCompileFlags` uses for a type width -- `zutil.c` L35-L58.
///
/// The same ladder appears four times in the C function, once per type: `2 => 0`, `4 => 1`,
/// `8 => 2`, and anything else `=> 3`. Reproduced here rather than imported, because the point of
/// the check below is to derive the expected answer independently of the implementation that
/// produces it.
const fn size_code(size: usize) -> uLong {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// The value `zlibCompileFlags()` must return on *this* build of *this* target.
///
/// **Derived, never hardcoded.** `zutil.c` L31-L121 packs four two-bit type-width codes into bits
/// 0-7 and a set of build-configuration flags into bits 8-27, so the answer is target-dependent:
/// it is `0xa9` on an LP64 target and something else on LLP64 Windows, where `uLong` is four bytes
/// and bits 2-3 change accordingly. Asserting a literal would therefore be asserting the wrong
/// thing everywhere except here.
///
/// Bits 0-7 come from `size_of`. Every optional bit is clear in the shipped configuration --
/// 8 `ZLIB_DEBUG`, 10 `ZLIB_WINAPI`, 12 `BUILDFIXED`, **13 `DYNAMIC_CRC_TABLE`**, 17 `NO_GZIP`,
/// 20 `PKZIP_BUG_WORKAROUND`, 21 `FASTEST`, and 24-26 of the `vsnprintf` family -- and two are
/// derived from the resolved feature set rather than fixed:
///
/// * **bit 16, `NO_GZCOMPRESS`.** `zlib.h` L1237-L1238 defines it as "`gz*` functions cannot
///   compress". With the `gz` feature off the entire `gzFile` layer is compiled out, so the bit is
///   set; with it on, clear.
/// * **bit 27, the `vsnprintf`-unavailable flag.** `gzprintf` and `gzvprintf` come from
///   `csrc/gzprintf_shim.c`, which the build script compiles only when both `libz-compat` and `gz`
///   are on. `cfg(zlib_rs_gzprintf)` is what it publishes to say so, and the flags word reports the
///   artifact that was actually built.
///
/// Bit 13 deserves its own note, because it is the one place where this port's design could
/// plausibly have produced a *different* answer from the reference and does not: the CRC tables are
/// `const`-evaluated rather than generated on first use, so `DYNAMIC_CRC_TABLE` is honestly clear --
/// and the reference build has it clear too, because it is not a `DYNAMIC_CRC_TABLE` build either.
/// The two agree at `0xa9`, which the assertion below records.
fn expected_compile_flags() -> uLong {
    let size_bits = size_code(size_of::<uInt>())
        | (size_code(size_of::<uLong>()) << 2)
        | (size_code(size_of::<z::voidpf>()) << 4)
        | (size_code(size_of::<z::z_off_t>()) << 6);

    let no_gzcompress: uLong = if cfg!(feature = "gz") { 0 } else { 1 << 16 };
    let gzprintf_unavailable: uLong = if cfg!(zlib_rs_gzprintf) { 0 } else { 1 << 27 };

    size_bits | no_gzcompress | gzprintf_unavailable
}

/// `main`'s opening version handshake and the line it prints -- `test/example.c` L501-L513.
///
/// Stateless, so it is its own `#[test]` rather than a stage of [`example_c_parity`].
///
/// # The C semantics, and why this is stricter
///
/// The C driver splits the check in two: a mismatch in the **first character** of the version
/// string is fatal (L503-L505, `exit(1)`), while a mismatch anywhere else is only a warning
/// (L507-L510). That leniency exists so a program built against 1.2.x still *runs* against 1.3.x.
/// It is not licence to accept a wrong string here: this port must return `zlib.h` L44 verbatim, so
/// neither C branch should ever fire, and the way to prove that is to assert the whole string. AAP
/// 0.7.1(b) forbids relaxing this to a first-character comparison to make a build pass -- if
/// `zlibVersion()` disagrees, the facade is wrong.
///
/// The string matters beyond introspection: `deflateInit_` and `inflateInit_` compare the caller's
/// compile-time `ZLIB_VERSION` against the library's and return `Z_VERSION_ERROR` on a major
/// mismatch (`zlib.h` L248), which is exactly the handshake every stage of [`example_c_parity`]
/// performs.
#[test]
fn version_and_compile_flags_match_the_contract() {
    // SAFETY: `zlibVersion` returns a pointer to the library's own `'static` NUL-terminated
    // version literal (`zlib.h` L224 and L44), so the scan terminates inside that literal and the
    // borrow outlives this test.
    let reported = unsafe { CStr::from_ptr(z::zlibVersion()) };

    // The first-character test the C driver treats as fatal, reproduced so that a failure reports
    // the same distinction the C driver would -- then the full-string test it treats as a warning,
    // which here is an assertion.
    assert_eq!(
        reported.to_bytes().first(),
        ZLIB_VERSION.to_bytes().first(),
        "incompatible zlib version: {reported:?}"
    );
    assert_eq!(
        reported, ZLIB_VERSION,
        "different zlib version linked: {reported:?}"
    );
    assert_eq!(reported.to_bytes(), b"1.3.2.1-motley");

    // `zlib.h` L45. Printed by the C driver at L512-L513 alongside the version and the flags.
    assert_eq!(z::ZLIB_VERNUM, 0x1321);

    let flags = z::zlibCompileFlags();
    assert_eq!(
        flags,
        expected_compile_flags(),
        "zlibCompileFlags() disagrees with the ladder derived from this target's type widths"
    );

    // The measured reference answer on `x86_64-unknown-linux-gnu`, checked only where the type
    // widths that produce it actually hold. `1 + (2 << 2) + (2 << 4) + (2 << 6)` == 169 == 0xa9,
    // with every configuration bit clear. This is a cross-check on the *derivation*, not a
    // substitute for it: on LLP64 Windows the guard is false and the assertion above still applies.
    if size_of::<uInt>() == 4
        && size_of::<uLong>() == 8
        && size_of::<z::voidpf>() == 8
        && size_of::<z::z_off_t>() == 8
        && cfg!(feature = "gz")
        && cfg!(zlib_rs_gzprintf)
    {
        assert_eq!(flags, 0xa9);
    }

    // `test/example.c` L512-L513 prints these rather than asserting on them; printing them too
    // keeps the two harnesses' output comparable by eye.
    println!(
        "zlib version {reported:?} = 0x{:04x}, compile flags = 0x{flags:x}",
        z::ZLIB_VERNUM
    );
}

// ---------------------------------------------------------------------------------------------
//  1. `test_compress` -- `test/example.c` L63-L85
// ---------------------------------------------------------------------------------------------

/// Round-trips `hello` through the one-shot `compress` / `uncompress` wrappers.
///
/// The two lengths are taken **by value**, exactly as the C signature does (`uLong comprLen`,
/// `uLong uncomprLen` at L66-L67). That is not incidental: `compress` and `uncompress` both write
/// back through their length pointer, so C's caller never sees the update, and reproducing the
/// by-value parameter is what keeps `main`'s `comprLen` intact across this stage.
fn test_compress(compr: &mut [u8], compr_len: uLong, uncompr: &mut [u8], uncompr_len: uLong) {
    // L69: `uLong len = (uLong) strlen(hello) + 1;` -- the whole array, NUL included.
    let len = HELLO_LEN;
    assert_eq!(len, 14);

    // L71-L72.
    let mut compr_len = compr_len;
    let err = compress(compr, &mut compr_len, HELLO);
    check_err(err, "compress");

    // L74.
    strcpy_garbage(uncompr);

    // L76-L77. The source is the compressed prefix the call above actually produced.
    let mut uncompr_len = uncompr_len;
    let produced = usize::try_from(compr_len).expect("the compressed length fits a usize");
    let err = uncompress(uncompr, &mut uncompr_len, &compr[..produced]);
    check_err(err, "uncompress");

    // L79-L84.
    assert_equals_hello(uncompr, "uncompress");
    println!("uncompress(): {:?}", as_cstr(uncompr));
}

// ---------------------------------------------------------------------------------------------
//  2. `test_gzio` -- `test/example.c` L87-L165
// ---------------------------------------------------------------------------------------------
//
// Gated on `gz` as well as `libz-compat`, because the whole `gzFile` layer is (`src/gz.rs` is
// `#[cfg(all(feature = "libz-compat", feature = "gz"))]`). With `gz` off there is no `gzopen` to
// link against, so this stage and its call site in `example_c_parity` both compile away and the
// other nine stages still run.

/// `SEEK_CUR` -- `zconf.h` L513-L517.
///
/// Defined locally rather than imported. `zconf.h` supplies the three `SEEK_*` values itself when
/// the platform headers have not already, and the value is 1 on every platform this library
/// targets; `libc` would also supply it, but AAP 0.7.1(i) confines this file to `std` and adding a
/// dev-dependency for one integer would be the wrong trade. The facade exports no `SEEK_*`
/// constant of its own, so there is nothing to re-export.
#[cfg(feature = "gz")]
const SEEK_CUR: c_int = 1;

// `gzprintf` is the one entry point in the C contract that this crate does not define in Rust:
// stable Rust at the declared 1.80 MSRV can neither define a C-variadic function nor name a
// `va_list`, so `crates/libz-rs-sys/build.rs` compiles `csrc/gzprintf_shim.c` and publishes the
// archive with `cargo::rustc-link-lib=static=zlib_rs_cabi`. That is why the symbol is absent from
// the crate root and has to be named here instead.
//
// ★ Declaring it is enough, and that is by design rather than by luck: the build script's own
// comment records that "an integration test that names `gzprintf` still links, because naming it is
// what makes the linker take the member". *Calling* a C-variadic function through such a
// declaration is stable Rust and always has been -- only *defining* one requires a nightly feature.
// So the C driver's `gzprintf(file, ", %s!", "hello") != 8` check (L109-L112) ports **verbatim**,
// with no fallback and no weakened assertion.
//
// The declaration sits behind the same two features the shim is compiled under, so a build that
// does not contain the shim does not reference it either.
#[cfg(feature = "gz")]
extern "C" {
    fn gzprintf(file: z::gzFile, format: *const c_char, ...) -> c_int;
}

/// A private directory for this stage's `.gz` file, and the file's path inside it.
///
/// The C driver writes `TESTFILE` -- "foo.gz" -- into the current working directory (L25). Three
/// reasons not to copy that, and the third is the one that shapes this type:
///
/// 1. The relinked C driver runs in the same tree and would race for the very same name.
/// 2. `cargo test` may run this binary concurrently with another invocation of itself.
/// 3. **A predictable name in a shared, world-writable directory is an attack surface, not merely
///    a collision risk.** Any other user on the machine can plant a symlink, a hard link or a
///    directory at a path a test is about to create, and `gzopen` -- which calls `open()` with
///    `O_CREAT` and no `O_EXCL` for a `"wb"` mode -- then follows it and writes wherever it points.
///    A process id is neither secret nor unpredictable: it is visible in `/proc`, drawn from a small
///    space, and reused.
///
/// So the *directory*, not the file, carries the security property. It is created:
///
/// * **Unguessable** -- 128 bits from [`RandomState`], whose seed the operating system provides.
/// * **Owner-only in the same syscall that creates it** -- `mode(0o700)` on the [`DirBuilder`],
///   rather than a create followed by a `chmod` that leaves a window open.
/// * **Exclusively** -- `create` is the non-recursive form, so an existing path is refused with
///   `AlreadyExists` rather than adopted. Nothing here ever removes a path it did not itself create,
///   so a planted entry is refused, never deleted.
///
/// That is also what keeps the ported `gzopen(fname, "wb")` call at L99 *verbatim*: nothing can
/// pre-exist inside a directory this process just created empty, so `"wb"`'s truncating open has
/// nothing to be tricked by, and the stage still exercises the same `gz_open` branch the C driver
/// does. (`"wbx"` would be the answer if the file had to live in a shared directory.)
///
/// [`Drop`] removes the tree, so cleanup is panic-safe: a failing assertion anywhere in the stage
/// unwinds through it, where the previous trailing `remove_file` call was simply never reached.
///
/// No `tempfile` dev-dependency: AAP 0.7.1(i) rules one out and `std` makes it unnecessary.
#[cfg(feature = "gz")]
struct GzScratch {
    /// The private directory. Removed, with its contents, when this value is dropped.
    dir: std::path::PathBuf,
    /// The `.gz` file inside it.
    file: std::path::PathBuf,
}

#[cfg(feature = "gz")]
impl Drop for GzScratch {
    fn drop(&mut self) {
        // Best effort, and only ever on a directory this process created: a missing tree must never
        // itself become the failure, and nothing else can have a path into this one.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(feature = "gz")]
impl GzScratch {
    /// How many unguessable names to try before giving up.
    ///
    /// A collision means another process holds that exact 128-bit name, which is vanishingly
    /// unlikely; the retries keep one unlucky draw from failing the suite, and the bound keeps a
    /// directory that refuses *every* create from spinning.
    const ATTEMPTS: usize = 16;

    /// Creates the private directory, or panics saying which step refused.
    ///
    /// Panicking is right here and only here: this is a test, the failure is an environment failure
    /// rather than a library one, and continuing would test nothing. The message names the base
    /// directory so the cause is actionable.
    fn create() -> Self {
        use std::hash::{BuildHasher, RandomState};
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;

        let base = std::env::temp_dir();
        for _ in 0..Self::ATTEMPTS {
            // Two independent OS-seeded draws, so the name carries 128 bits rather than 64.
            let high = u128::from(RandomState::new().hash_one(0_u64));
            let low = u128::from(RandomState::new().hash_one(u64::MAX));
            let dir = base.join(format!("zlib_rs_c_api_parity_{:032x}", (high << 64) | low));

            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            builder.mode(0o700);

            match builder.create(&dir) {
                Ok(()) => {
                    let file = dir.join("example.gz");
                    return Self { dir, file };
                }
                // Someone holds that name. Draw another; nothing is removed.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!(
                    "cannot create a private scratch directory under {}: {error}",
                    base.display()
                ),
            }
        }
        panic!(
            "{} unguessable names under {} were all taken",
            Self::ATTEMPTS,
            base.display()
        )
    }

    /// The `.gz` file's path.
    fn path(&self) -> &std::path::Path {
        &self.file
    }
}

/// `gzerror(file, &err)` -- `zlib.h` L1775. Every failure message in this stage carries it, exactly
/// as the C driver's do, so a failure says *why* and not merely *where*.
#[cfg(feature = "gz")]
fn gz_error_text(file: z::gzFile) -> String {
    let mut errnum: c_int = 0;
    // SAFETY: `file` is a non-null handle this stage obtained from `gzopen` and has not yet closed,
    // and `errnum` is a live local, so the `*mut c_int` out-parameter is valid for the write.
    let message = unsafe { z::gzerror(file, addr_of_mut!(errnum)) };
    if message.is_null() {
        return format!("errnum {errnum}, no message");
    }
    // SAFETY: `gzerror` returns either null -- handled above -- or a NUL-terminated string owned by
    // the stream or by the library's `'static` message table, which outlives this borrow.
    let text = unsafe { CStr::from_ptr(message) };
    format!("errnum {errnum}, {}", text.to_string_lossy())
}

/// Exercises the whole `gzFile` read/write surface the C driver reaches -- `test/example.c`
/// L90-L165.
///
/// The `NO_GZCOMPRESS` branch at L91-L92 has no counterpart here: that macro is a C build option,
/// and the equivalent configuration in this crate is `gz` being off, which the `#[cfg]` above
/// handles by removing the stage entirely rather than by turning it into a message.
#[cfg(feature = "gz")]
fn test_gzio(uncompr: &mut [u8], uncompr_len: uLong) {
    // Held for the whole stage: its `Drop` is the cleanup, so it must outlive every assertion.
    let scratch = GzScratch::create();
    let path = scratch.path();
    let fname = std::ffi::CString::new(path.as_os_str().to_string_lossy().as_bytes())
        .expect("a temporary path contains no interior NUL");

    // L95: `int len = (int) strlen(hello) + 1;`
    let len = c_int::try_from(HELLO_LEN).expect("14 fits a C int");

    // ---- write phase: L99-L114 ----

    // SAFETY: both arguments are NUL-terminated C strings owned by live locals that outlive the
    // call -- `fname` is a `CString` and the mode is a `c"…"` literal -- and `gzopen` only reads
    // them.
    let file = unsafe { z::gzopen(fname.as_ptr(), c"wb".as_ptr()) };
    assert!(!file.is_null(), "gzopen error: {}", path.display());

    // L104. The C driver discards this return value; checking it is a *strengthening*, never a
    // relaxation -- `gzputc` answers with the character written or -1, and swallowing a -1 here
    // would surface much later as a confusing short read.
    // SAFETY: `file` is the non-null handle just asserted, and `gzputc` takes it by value.
    let written = unsafe { z::gzputc(file, c_int::from(b'h')) };
    assert_eq!(written, c_int::from(b'h'), "gzputc error");

    // L105-L108.
    // SAFETY: `file` is non-null and open for writing, and the string is a `c"…"` literal, so it
    // is NUL-terminated, `'static` and read-only for the library.
    let put = unsafe { z::gzputs(file, c"ello".as_ptr()) };
    assert_eq!(put, 4, "gzputs err: {}", gz_error_text(file));

    // L109-L112. The variadic call, ported verbatim -- see the note on the declaration above.
    // SAFETY: `file` is non-null and open for writing; the format string is a `c"…"` literal whose
    // single `%s` directive is matched by exactly one further argument, itself a `c"…"` literal, so
    // the callee's `va_arg` sequence agrees with what was pushed. Both strings are `'static`.
    let printed = unsafe { gzprintf(file, c", %s!".as_ptr(), c"hello".as_ptr()) };
    assert_eq!(printed, 8, "gzprintf err: {}", gz_error_text(file));

    // L113: seeking forward on a write stream appends one zero byte, which is what makes the file
    // 14 bytes long and the later `gzread` return 14 rather than 13. The C driver discards this
    // return value too; asserting the resulting position is again a strengthening, and it pins the
    // exact reason the read phase expects 14.
    // SAFETY: `file` is non-null and open; the offset and whence are plain integers.
    let sought = unsafe { z::gzseek(file, 1, SEEK_CUR) };
    assert_eq!(sought, 14, "gzseek error on the write stream");

    // L114.
    // SAFETY: `file` is non-null and has not been closed; this call consumes it, and it is not
    // used again -- the binding is shadowed by a fresh `gzopen` below.
    let err = unsafe { z::gzclose(file) };
    check_err(err, "gzclose after writing");

    // ---- read phase: L116-L163 ----

    // SAFETY: as for the write-phase `gzopen`.
    let file = unsafe { z::gzopen(fname.as_ptr(), c"rb".as_ptr()) };
    assert!(!file.is_null(), "gzopen error: {}", path.display());

    // L121.
    strcpy_garbage(uncompr);

    // L123-L126.
    let capacity = c_uint::try_from(uncompr_len).expect("uncomprLen fits a C unsigned");
    assert_capacity(uncompr, uncompr_len);
    // SAFETY: `file` is non-null and open for reading, and `capacity` bytes are writable at
    // `uncompr`'s start -- it is a live slice whose length `assert_capacity` has just checked
    // against `uncompr_len`, and it outlives the call.
    let read = unsafe { z::gzread(file, uncompr.as_mut_ptr().cast(), capacity) };
    assert_eq!(read, len, "gzread err: {}", gz_error_text(file));

    // L127-L132.
    assert_equals_hello(uncompr, "gzread");
    println!("gzread(): {:?}", as_cstr(uncompr));

    // L134-L139: eight bytes back from position 14 is position 6, and `gztell` must agree.
    // SAFETY: `file` is non-null and open; the offset and whence are plain integers.
    let pos = unsafe { z::gzseek(file, -8, SEEK_CUR) };
    // SAFETY: as above -- `gztell` only reads the handle's position.
    let tell = unsafe { z::gztell(file) };
    assert!(
        pos == 6 && tell == pos,
        "gzseek error, pos={pos}, gztell={tell}"
    );

    // L141-L144: the byte at position 6 is the space in "hello, hello!".
    //
    // `gzgetc` is a *macro* in `zlib.h` L1966-L1968 whose fast path decrements `have`, increments
    // `pos` and post-increments `next` directly in the caller's own object code. Rust cannot expand
    // a C macro, so this calls the exported **function** of the same name -- which is the macro's
    // own fallback branch, and is the correct and complete call. The macro's fast path is exercised
    // by the relinked C drivers, which is where it belongs.
    // SAFETY: `file` is non-null and open for reading.
    let ch = unsafe { z::gzgetc(file) };
    assert_eq!(ch, c_int::from(b' '), "gzgetc error");

    // L146-L149.
    // SAFETY: `file` is non-null and open for reading; the pushed-back character is a plain
    // integer, and `gzungetc` stores it in the stream's own buffer.
    let pushed = unsafe { z::gzungetc(c_int::from(b' '), file) };
    assert_eq!(pushed, c_int::from(b' '), "gzungetc error");

    // L151-L161: reads " hello!" -- the space just pushed back plus the rest of the line.
    let room = c_int::try_from(uncompr_len).expect("uncomprLen fits a C int");
    // SAFETY: `file` is non-null and open for reading, and `room` bytes are writable at
    // `uncompr`'s start for the same reason the `gzread` call above is sound. `gzgets` writes at
    // most `room` bytes including its terminator.
    let got = unsafe { z::gzgets(file, uncompr.as_mut_ptr().cast::<c_char>(), room) };
    assert!(
        !got.is_null(),
        "gzgets err after gzseek: {}",
        gz_error_text(file)
    );

    let line = as_cstr(uncompr);
    assert_eq!(
        line.to_bytes().len(),
        7,
        "gzgets err after gzseek: {}",
        gz_error_text(file)
    );
    // L156: `strcmp((char *) uncompr, hello + 6)`. `HELLO[6..]` is b" hello!\0", so the same
    // pointer arithmetic becomes a slice of the same fixture.
    let want = CStr::from_bytes_with_nul(&HELLO[6..]).expect("HELLO[6..] is NUL-terminated");
    assert!(line == want, "bad gzgets after gzseek: {line:?}");
    println!("gzgets() after gzseek: {line:?}");

    // L163.
    // SAFETY: `file` is non-null and has not been closed; this call consumes it and it is not used
    // afterwards.
    let err = unsafe { z::gzclose(file) };
    check_err(err, "gzclose after reading");

    // Cleanup is not spelled out here on purpose. The C driver leaves "foo.gz" behind for the build
    // tree to clean up; a temporary directory is this port's own creation, so removing it is this
    // port's own responsibility -- and a trailing `remove_file` call discharges that responsibility
    // only on the path where nothing went wrong. `GzScratch`'s `Drop` runs on *every* path out of
    // this function, including an unwind from any assertion above, which is the whole reason the
    // scratch is a value with a destructor rather than a path plus a tidy-up statement.
    drop(scratch);
}

// ---------------------------------------------------------------------------------------------
//  3. `test_deflate` -- `test/example.c` L169-L202
// ---------------------------------------------------------------------------------------------

/// Drives `deflate` one byte in and one byte out at a time.
///
/// The starvation is the whole point of the stage (L188: "force small buffers"): it proves the
/// engine suspends and resumes correctly at every internal boundary rather than only when handed
/// the whole input at once.
fn test_deflate(compr: &mut [u8], compr_len: uLong) {
    // L175.
    let len = uLong::try_from(HELLO_LEN).expect("14 fits a uLong");

    // L177-L182.
    let mut c_stream = new_stream();
    let err = deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION);
    check_err(err, "deflateInit");

    // L184-L185. Set once; the library advances both pointers itself, so -- unlike
    // `test_large_inflate` -- neither is re-pointed inside the loop.
    c_stream.next_in = HELLO.as_ptr();
    c_stream.next_out = compr.as_mut_ptr();

    // L187-L191.
    while c_stream.total_in != len && c_stream.total_out < compr_len {
        c_stream.avail_in = 1;
        c_stream.avail_out = 1;
        let err = deflate(&mut c_stream, Z_NO_FLUSH);
        check_err(err, "deflate");
    }

    // L192-L198: finish the stream, still forcing small buffers.
    loop {
        c_stream.avail_out = 1;
        let err = deflate(&mut c_stream, Z_FINISH);
        if err == Z_STREAM_END {
            break;
        }
        check_err(err, "deflate");
    }

    // L200-L201.
    let err = deflate_end(&mut c_stream);
    check_err(err, "deflateEnd");
}

// ---------------------------------------------------------------------------------------------
//  4. `test_inflate` -- `test/example.c` L204-L241
// ---------------------------------------------------------------------------------------------

/// Drives `inflate` one byte in and one byte out at a time, over what [`test_deflate`] wrote.
fn test_inflate(compr: &mut [u8], compr_len: uLong, uncompr: &mut [u8], uncompr_len: uLong) {
    // L212.
    strcpy_garbage(uncompr);

    // L214-L220. `avail_in` starts at **zero** while `next_in` is already set: the first `inflate`
    // call is therefore handed a valid pointer and nothing to read, and the loop supplies the first
    // byte. That ordering is C's and is reproduced rather than tidied.
    let mut d_stream = new_stream();
    d_stream.next_in = compr.as_ptr();
    d_stream.avail_in = 0;
    d_stream.next_out = uncompr.as_mut_ptr();

    // L222-L223.
    let err = inflate_init(&mut d_stream);
    check_err(err, "inflateInit");

    // L225-L230.
    while d_stream.total_out < uncompr_len && d_stream.total_in < compr_len {
        d_stream.avail_in = 1;
        d_stream.avail_out = 1;
        let err = inflate(&mut d_stream, Z_NO_FLUSH);
        if err == Z_STREAM_END {
            break;
        }
        check_err(err, "inflate");
    }

    // L232-L233.
    let err = inflate_end(&mut d_stream);
    check_err(err, "inflateEnd");

    // L235-L240.
    assert_equals_hello(uncompr, "inflate");
    println!("inflate(): {:?}", as_cstr(uncompr));
}

// ---------------------------------------------------------------------------------------------
//  5. `test_large_deflate` -- `test/example.c` L243-L294
// ---------------------------------------------------------------------------------------------

/// Compresses 50 000 bytes in three passes, changing level and strategy mid-stream.
///
/// This is the stage that depends on `uncompr` still being mostly zeroes (L261-L263), which is why
/// `main` clears both buffers and why this port allocates them with `vec![0u8; n]` rather than with
/// uninitialised capacity.
fn test_large_deflate(compr: &mut [u8], compr_len: uLong, uncompr: &mut [u8], uncompr_len: uLong) {
    // L251-L256.
    let mut c_stream = new_stream();
    let err = deflate_init(&mut c_stream, Z_BEST_SPEED);
    check_err(err, "deflateInit");

    // L258-L259.
    c_stream.next_out = compr.as_mut_ptr();
    c_stream.avail_out = uInt::try_from(compr_len).expect("comprLen fits a uInt");

    // L264-L267: feed the whole zero-filled buffer, which compresses very well.
    c_stream.next_in = uncompr.as_ptr();
    c_stream.avail_in = uInt::try_from(uncompr_len).expect("uncomprLen fits a uInt");
    let err = deflate(&mut c_stream, Z_NO_FLUSH);
    check_err(err, "deflate");

    // L268-L271: with output space for the whole thing, `deflate` must have consumed all input.
    assert_eq!(c_stream.avail_in, 0, "deflate not greedy");

    // L273-L278: feed in already compressed data and switch to no compression. The C driver
    // deliberately ignores `deflateParams`' return value here, and so does this port -- adding a
    // `CHECK_ERR` would be a behavioural change, not an improvement.
    deflate_params(&mut c_stream, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY);
    c_stream.next_in = compr.as_ptr();
    c_stream.avail_in = uInt::try_from(uncompr_len / 2).expect("uncomprLen / 2 fits a uInt");
    let err = deflate(&mut c_stream, Z_NO_FLUSH);
    check_err(err, "deflate");

    // L280-L285: switch back to compressing mode, with a different strategy this time.
    deflate_params(&mut c_stream, Z_BEST_COMPRESSION, Z_FILTERED);
    c_stream.next_in = uncompr.as_ptr();
    c_stream.avail_in = uInt::try_from(uncompr_len).expect("uncomprLen fits a uInt");
    let err = deflate(&mut c_stream, Z_NO_FLUSH);
    check_err(err, "deflate");

    // L287-L291.
    let err = deflate(&mut c_stream, Z_FINISH);
    assert_eq!(
        err, Z_STREAM_END,
        "deflate should report Z_STREAM_END, got {err}"
    );

    // L292-L293.
    let err = deflate_end(&mut c_stream);
    check_err(err, "deflateEnd");
}

// ---------------------------------------------------------------------------------------------
//  6. `test_large_inflate` -- `test/example.c` L296-L333
// ---------------------------------------------------------------------------------------------

/// Decodes what [`test_large_deflate`] wrote, discarding the output and checking only its size.
fn test_large_inflate(compr: &mut [u8], compr_len: uLong, uncompr: &mut [u8], uncompr_len: uLong) {
    // L304.
    strcpy_garbage(uncompr);

    // L306-L311.
    let mut d_stream = new_stream();
    d_stream.next_in = compr.as_ptr();
    d_stream.avail_in = uInt::try_from(compr_len).expect("comprLen fits a uInt");

    // L313-L314.
    let err = inflate_init(&mut d_stream);
    check_err(err, "inflateInit");

    // L316-L322. Here the output pair *is* re-pointed every iteration, because the 50 000 bytes
    // produced do not fit the 20 000-byte buffer and are deliberately thrown away.
    loop {
        d_stream.next_out = uncompr.as_mut_ptr();
        d_stream.avail_out = uInt::try_from(uncompr_len).expect("uncomprLen fits a uInt");
        let err = inflate(&mut d_stream, Z_NO_FLUSH);
        if err == Z_STREAM_END {
            break;
        }
        check_err(err, "large inflate");
    }

    // L324-L325.
    let err = inflate_end(&mut d_stream);
    check_err(err, "inflateEnd");

    // L327-L332: the three passes of `test_large_deflate` fed `uncomprLen`, `uncomprLen / 2` and
    // `uncomprLen` bytes, so 50 000 came out. Getting a different total means one of those passes
    // did not consume what it was given.
    let expected = 2 * uncompr_len + uncompr_len / 2;
    assert_eq!(expected, 50_000);
    assert_eq!(
        d_stream.total_out, expected,
        "bad large inflate: {}",
        d_stream.total_out
    );
    println!("large_inflate(): OK");
}

// ---------------------------------------------------------------------------------------------
//  7. `test_flush` -- `test/example.c` L335-L368
// ---------------------------------------------------------------------------------------------

/// Emits a `Z_FULL_FLUSH` boundary, then **corrupts the first block on purpose**.
///
/// `compr_len` is `&mut` because the C signature is `uLong *comprLen` (L338) and L367 writes the
/// actual output size back through it -- the one length in the whole driver that is an out-parameter.
/// [`test_sync`] then consumes that value, and `main` restores the full buffer size afterwards.
fn test_flush(compr: &mut [u8], compr_len: &mut uLong) {
    // L341.
    let len = uInt::try_from(HELLO_LEN).expect("14 fits a uInt");

    // L343-L348.
    let mut c_stream = new_stream();
    let err = deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION);
    check_err(err, "deflateInit");

    // L350-L355: three bytes in, a full flush out.
    c_stream.next_in = HELLO.as_ptr();
    c_stream.next_out = compr.as_mut_ptr();
    c_stream.avail_in = 3;
    c_stream.avail_out = uInt::try_from(*compr_len).expect("comprLen fits a uInt");
    let err = deflate(&mut c_stream, Z_FULL_FLUSH);
    check_err(err, "deflate");

    // L357-L358: `compr[3]++` forces an error in the first compressed block. This is not a defect
    // and must not be "fixed": it is precisely how `test_sync` acquires damage for `inflateSync` to
    // skip past. `wrapping_add` because C's `++` on an `unsigned char` wraps, and byte 3 could in
    // principle be 0xff.
    compr[3] = compr[3].wrapping_add(1);
    c_stream.avail_in = len - 3;

    // L360-L363: `Z_STREAM_END` is the expected answer; anything else goes through `CHECK_ERR`.
    let err = deflate(&mut c_stream, Z_FINISH);
    if err != Z_STREAM_END {
        check_err(err, "deflate");
    }

    // L364-L365.
    let err = deflate_end(&mut c_stream);
    check_err(err, "deflateEnd");

    // L367.
    *compr_len = c_stream.total_out;
}

// ---------------------------------------------------------------------------------------------
//  8. `test_sync` -- `test/example.c` L370-L409
// ---------------------------------------------------------------------------------------------

/// Recovers from the damage [`test_flush`] introduced, using `inflateSync`.
fn test_sync(compr: &mut [u8], compr_len: uLong, uncompr: &mut [u8], uncompr_len: uLong) {
    // L378.
    strcpy_garbage(uncompr);

    // L380-L385: just two bytes of input, which is exactly the zlib header.
    let mut d_stream = new_stream();
    d_stream.next_in = compr.as_ptr();
    d_stream.avail_in = 2;

    // L387-L388.
    let err = inflate_init(&mut d_stream);
    check_err(err, "inflateInit");

    // L390-L394.
    d_stream.next_out = uncompr.as_mut_ptr();
    d_stream.avail_out = uInt::try_from(uncompr_len).expect("uncomprLen fits a uInt");
    let err = inflate(&mut d_stream, Z_NO_FLUSH);
    check_err(err, "inflate");

    // L396-L398: hand over all the remaining compressed data, then skip the damaged part.
    d_stream.avail_in = uInt::try_from(compr_len - 2).expect("comprLen - 2 fits a uInt");
    let err = inflate_sync(&mut d_stream);
    check_err(err, "inflateSync");

    // L400-L404.
    let err = inflate(&mut d_stream, Z_FINISH);
    assert_eq!(
        err, Z_STREAM_END,
        "inflate should report Z_STREAM_END, got {err}"
    );

    // L405-L406.
    let err = inflate_end(&mut d_stream);
    check_err(err, "inflateEnd");

    // L408. The first three bytes were lost with the damaged block, so the C driver prints the
    // literal "hel" in front of whatever survived -- which is the tail of `hello, hello!`.
    println!(
        "after inflateSync(): hel{}",
        as_cstr(uncompr).to_string_lossy()
    );
}

// ---------------------------------------------------------------------------------------------
//  10. `test_dict_deflate` -- `test/example.c` L411-L443
// ---------------------------------------------------------------------------------------------

/// Compresses with a preset dictionary, capturing its Adler-32 id for [`test_dict_inflate`].
///
/// `dict_id` is C's file-scope `static uLong dictId` (L41), threaded here as a `&mut` local instead:
/// the coupling between the two dictionary stages is real and belongs in the signature, where the
/// compiler can see it, rather than in mutable global state.
fn test_dict_deflate(compr: &mut [u8], compr_len: uLong, dict_id: &mut uLong) {
    // L418-L423.
    let mut c_stream = new_stream();
    let err = deflate_init(&mut c_stream, Z_BEST_COMPRESSION);
    check_err(err, "deflateInit");

    // L425-L427: the length C passes is `sizeof(dictionary)` -- six bytes, the NUL included.
    assert_eq!(DICTIONARY_LEN, 6);
    let err = deflate_set_dictionary(&mut c_stream, DICTIONARY);
    check_err(err, "deflateSetDictionary");

    // L429-L431: `adler` now holds the Adler-32 of the dictionary, which the decoder will demand.
    *dict_id = c_stream.adler;
    c_stream.next_out = compr.as_mut_ptr();
    c_stream.avail_out = uInt::try_from(compr_len).expect("comprLen fits a uInt");

    // L433-L434.
    c_stream.next_in = HELLO.as_ptr();
    c_stream.avail_in = uInt::try_from(HELLO_LEN).expect("14 fits a uInt");

    // L436-L440: one shot, whole input, so `Z_FINISH` must complete the stream immediately.
    let err = deflate(&mut c_stream, Z_FINISH);
    assert_eq!(
        err, Z_STREAM_END,
        "deflate should report Z_STREAM_END, got {err}"
    );

    // L441-L442.
    let err = deflate_end(&mut c_stream);
    check_err(err, "deflateEnd");
}

// ---------------------------------------------------------------------------------------------
//  11. `test_dict_inflate` -- `test/example.c` L445-L491
// ---------------------------------------------------------------------------------------------

/// Decompresses the dictionary-compressed stream, completing the `Z_NEED_DICT` handshake.
fn test_dict_inflate(
    compr: &mut [u8],
    compr_len: uLong,
    uncompr: &mut [u8],
    uncompr_len: uLong,
    dict_id: uLong,
) {
    // L453.
    strcpy_garbage(uncompr);

    // L455-L460.
    let mut d_stream = new_stream();
    d_stream.next_in = compr.as_ptr();
    d_stream.avail_in = uInt::try_from(compr_len).expect("comprLen fits a uInt");

    // L462-L463.
    let err = inflate_init(&mut d_stream);
    check_err(err, "inflateInit");

    // L465-L466.
    d_stream.next_out = uncompr.as_mut_ptr();
    d_stream.avail_out = uInt::try_from(uncompr_len).expect("uncomprLen fits a uInt");

    // L468-L480. The `Z_NEED_DICT` arm checks the id *before* supplying the dictionary, so a
    // decoder that asked for the wrong one is caught rather than silently satisfied; then the
    // result of `inflateSetDictionary` falls through to the same `CHECK_ERR` the loop body ends on.
    loop {
        let mut err = inflate(&mut d_stream, Z_NO_FLUSH);
        if err == Z_STREAM_END {
            break;
        }
        if err == Z_NEED_DICT {
            assert_eq!(
                d_stream.adler, dict_id,
                "unexpected dictionary: 0x{:x}",
                d_stream.adler
            );
            err = inflate_set_dictionary(&mut d_stream, DICTIONARY);
        }
        check_err(err, "inflate with dict");
    }

    // L482-L483.
    let err = inflate_end(&mut d_stream);
    check_err(err, "inflateEnd");

    // L485-L490.
    assert_equals_hello(uncompr, "inflate with dict");
    println!("inflate with dictionary: {:?}", as_cstr(uncompr));
}

// ---------------------------------------------------------------------------------------------
//  `main` -- `test/example.c` L493-L552
// ---------------------------------------------------------------------------------------------

/// The whole of `test/example.c`'s `main`, in order, as one test.
///
/// ★ **One `#[test]`, not ten.** The reasons are set out at the head of this file: the ten stages
/// share `compr` and `uncompr`, `test_flush` rewrites `compr_len` for `test_sync` to consume and
/// corrupts a byte for it to recover from, `test_large_inflate` decodes exactly what
/// `test_large_deflate` wrote, and `test_dict_inflate` needs the id `test_dict_deflate` captured.
/// Cargo runs `#[test]` functions on parallel threads with independent state, so ten tests would
/// break every one of those couplings and turn a deterministic sequence into a race.
#[test]
fn example_c_parity() {
    // The `gz` stage opens, writes, reads and unlinks a real file, and Miri does not model that
    // freely. Miri is scoped to `-p zlib-rs` in CI, so this guard should never fire; it is one line
    // and it means the suite degrades to "skipped" rather than "failed" if that scope ever widens.
    if cfg!(miri) {
        println!("skipped under Miri: this suite performs real file I/O");
        return;
    }

    // L499-L500.
    let uncompr_len = uLong::try_from(UNCOMPR_LEN).expect("20000 fits a uLong");
    let mut compr_len = uLong::try_from(COMPR_LEN).expect("60000 fits a uLong");

    // L515-L519: `calloc`, not `malloc`. The comment there records both reasons -- it avoids
    // reading uninitialised data, and it is what makes `uncompr` compress well enough for
    // `test_large_deflate`'s "deflate not greedy" assertion to hold.
    let mut compr = vec![0_u8; COMPR_LEN];
    let mut uncompr = vec![0_u8; UNCOMPR_LEN];

    // C's file-scope `static uLong dictId` (L41), as a local threaded between the two dictionary
    // stages.
    let mut dict_id: uLong = 0;

    // L529.
    test_compress(&mut compr, compr_len, &mut uncompr, uncompr_len);

    // L531-L532. The C driver takes the path from `argv[1]` or falls back to `TESTFILE`
    // ("foo.gz" in the current directory); this port always uses a unique temporary path, for the
    // reasons given at `test_gzio`.
    #[cfg(feature = "gz")]
    test_gzio(&mut uncompr, uncompr_len);

    // L535-L536.
    test_deflate(&mut compr, compr_len);
    test_inflate(&mut compr, compr_len, &mut uncompr, uncompr_len);

    // L538-L539.
    test_large_deflate(&mut compr, compr_len, &mut uncompr, uncompr_len);
    test_large_inflate(&mut compr, compr_len, &mut uncompr, uncompr_len);

    // L541-L542. `test_flush` shrinks `compr_len` to the size it actually wrote, and `test_sync`
    // reads exactly that many bytes.
    test_flush(&mut compr, &mut compr_len);
    test_sync(&mut compr, compr_len, &mut uncompr, uncompr_len);

    // ★ L543: `comprLen = 3 * uncomprLen;`. Easy to miss and expensive to omit -- the dictionary
    // stages need the whole buffer again, and without this reset `test_dict_deflate` fails with a
    // `Z_BUF_ERROR` that gives no hint where it came from.
    compr_len = 3 * uncompr_len;
    assert_eq!(compr_len, 60_000);

    // L545-L546.
    test_dict_deflate(&mut compr, compr_len, &mut dict_id);
    test_dict_inflate(&mut compr, compr_len, &mut uncompr, uncompr_len, dict_id);
}

// ---------------------------------------------------------------------------------------------
//  Stateless additions from `test/infcover.c`
// ---------------------------------------------------------------------------------------------
//
// `test/example.c` never touches `inflateBack` and never calls `zError`, so these two checks add
// coverage rather than duplicating it -- which is what AAP 0.8.1 directive 12 asks for: the existing
// suite is relinked unmodified *and* ported, so coverage strictly increases. Both are stateless, so
// each is its own `#[test]` and neither can perturb `example_c_parity`.

/// The `inflateBack` argument-validation ladder -- `test/infcover.c` L470-L478 (`cover_back`).
///
/// ★ **The ordering is the assertion.** The first call passes a null stream *and* a null version
/// *and* a zero `stream_size`, and the required answer is `Z_VERSION_ERROR` rather than
/// `Z_STREAM_ERROR`: the version-and-layout handshake is checked *before* the stream pointer is
/// looked at. An implementation that tested the pointer first would return `Z_STREAM_ERROR` here,
/// pass a naive "rejects bad input" test, and still be wrong -- so the two calls are written
/// adjacently, exactly as `cover_back` writes them, with the only difference between them being the
/// version arguments.
#[test]
fn inflate_back_rejects_bad_parameters() {
    // `unsigned char win[32768]` -- `test/infcover.c` L469. A full 32 KiB window, the size
    // `inflateBackInit_` requires for `windowBits == 15`; heap-allocated here rather than placed on
    // the stack, which a test thread's default stack would not comfortably hold.
    let mut win = vec![0_u8; 32_768];

    // L474-L475: `inflateBackInit_(Z_NULL, 0, win, 0, 0)` == `Z_VERSION_ERROR`.
    // SAFETY: a null `z_streamp` is part of the published contract -- `zlib.h` documents a null
    // stream as a `Z_STREAM_ERROR` case, so the callee must test it rather than dereference it, and
    // the facade's entry guard does exactly that. `win` is a live 32 KiB slice that outlives the
    // call, and a null version pointer is likewise a contract case the version check handles.
    let err = unsafe { z::inflateBackInit_(ptr::null_mut(), 0, win.as_mut_ptr(), ptr::null(), 0) };
    assert_eq!(
        err, Z_VERSION_ERROR,
        "the version check must precede the null-stream check"
    );

    // L476: `inflateBackInit(Z_NULL, 0, win)` == `Z_STREAM_ERROR` -- the same call with the version
    // handshake satisfied, so the null stream is now what fails.
    // SAFETY: as above, with a valid `'static` version string and the true `sizeof(z_stream)`.
    let err = unsafe {
        z::inflateBackInit_(
            ptr::null_mut(),
            0,
            win.as_mut_ptr(),
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    };
    assert_eq!(err, Z_STREAM_ERROR);

    // L477-L478: `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL)` == `Z_STREAM_ERROR`.
    // SAFETY: every argument is the null the contract defines. The stream pointer is tested before
    // use, and neither callback is invoked on a stream that failed that test, so no null function
    // pointer is ever called.
    let err = unsafe {
        z::inflateBack(
            ptr::null_mut(),
            None,
            ptr::null_mut(),
            None,
            ptr::null_mut(),
        )
    };
    assert_eq!(err, Z_STREAM_ERROR);

    // L479: `inflateBackEnd(Z_NULL)` == `Z_STREAM_ERROR`.
    // SAFETY: a null `z_streamp`, which this entry point is contracted to reject rather than
    // dereference.
    let err = unsafe { z::inflateBackEnd(ptr::null_mut()) };
    assert_eq!(err, Z_STREAM_ERROR);

    // Matching `cover_back`'s progress line at L480.
    println!("inflateBack bad parameters");
}

/// `zError(err)` for every slot of the status table -- `zutil.c` L13-L24 and L139-L141, indexed by
/// the `ERR_MSG` formula at `zutil.h` L65.
///
/// That formula is `z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]`, so the table is walked from
/// `Z_NEED_DICT` (+2) down to `Z_VERSION_ERROR` (-6) and everything outside that range lands on slot
/// 9. Two slots hold the empty string on purpose -- slot 2 is `Z_OK` and slot 9 is the out-of-range
/// sentinel -- and the wording below is transcribed from the table the facade actually holds
/// (`src/util.rs` `Z_ERRMSG_C`), which is itself transcribed character for character from C.
#[test]
fn z_error_reports_the_status_table() {
    /// `ERR_MSG`'s index arithmetic, reproduced so the expectation is derived the way C derives it
    /// rather than tabulated by hand.
    ///
    /// C spells the guard `(err) < -6 || (err) > 2`; `!(-6..=2).contains(&err)` is the identical
    /// predicate in the form `clippy::manual_range_contains` requires, so the correspondence is
    /// preserved in this comment rather than by suppressing the lint.
    fn slot(err: c_int) -> usize {
        if (-6..=2).contains(&err) {
            usize::try_from(2 - err).expect("2 - err is in 0..=8 on this branch")
        } else {
            9
        }
    }

    /// `z_errmsg[10]` -- `zutil.c` L13-L24, in table order.
    const MESSAGES: [&str; 10] = [
        "need dictionary",      // Z_NEED_DICT       2
        "stream end",           // Z_STREAM_END      1
        "",                     // Z_OK              0
        "file error",           // Z_ERRNO         (-1)
        "stream error",         // Z_STREAM_ERROR  (-2)
        "data error",           // Z_DATA_ERROR    (-3)
        "insufficient memory",  // Z_MEM_ERROR     (-4)
        "buffer error",         // Z_BUF_ERROR     (-5)
        "incompatible version", // Z_VERSION_ERROR (-6)
        "",                     // out-of-range sentinel
    ];

    // The nine documented codes, plus four values outside the table's range in both directions.
    // `Z_ERRNO` is included even though nothing in this suite produces it, because it occupies a
    // real slot and a table shifted by one would still satisfy its neighbours.
    let codes = [
        Z_NEED_DICT,
        Z_STREAM_END,
        Z_OK,
        Z_ERRNO,
        Z_STREAM_ERROR,
        Z_DATA_ERROR,
        Z_MEM_ERROR,
        Z_BUF_ERROR,
        Z_VERSION_ERROR,
        3,
        -7,
        c_int::MAX,
        c_int::MIN,
    ];

    for err in codes {
        // SAFETY: `zError` takes a plain integer and returns a pointer into the library's `'static`
        // status table (`zutil.c` L13-L24), which is non-null, NUL-terminated and outlives this
        // borrow for every input -- an out-of-range code selects the empty slot 9 rather than
        // reading off the end.
        let message = unsafe { CStr::from_ptr(z::zError(err)) };
        assert_eq!(
            message.to_str().expect("the status messages are ASCII"),
            MESSAGES[slot(err)],
            "zError({err}) selected the wrong slot"
        );
    }

    // The two properties the formula's shape depends on, stated outright so that a table which
    // happened to match slot by slot but lost this structure would still be caught.
    // SAFETY: as above -- a `'static`, NUL-terminated table entry.
    let ok = unsafe { CStr::from_ptr(z::zError(Z_OK)) };
    assert!(ok.to_bytes().is_empty(), "zError(Z_OK) must be empty");
    // SAFETY: as above.
    let out_of_range = unsafe { CStr::from_ptr(z::zError(-7)) };
    assert!(
        out_of_range.to_bytes().is_empty(),
        "an out-of-range code must select the empty sentinel"
    );
}

// ---------------------------------------------------------------------------------------------
//  `gzvprintf`, driven through a C-built `va_list`
// ---------------------------------------------------------------------------------------------
//
// `gzvprintf` is one of the 95 functions `zlib.h` declares, and until this section existed it was
// the only one reachable in this process that no test called directly. `gzprintf` reaches it -- the
// shim's `gzprintf` is literally `va_start`, one forward, `va_end` -- so its body did execute; what
// had no coverage was `gzvprintf` AS AN ENTRY POINT, with its own argument guards, its own return
// contract and its own error paths. One call shape from one caller is not that.
//
// It cannot be called from Rust. `core::ffi::VaList` is unstable at the declared 1.80 MSRV, and
// `va_list` is a different type on every ABI -- `__va_list_tag[1]` on x86-64 SysV, a by-value
// struct on AArch64, a `char *` on i386 -- so a hand-written Rust declaration would be ABI-invalid
// rather than merely awkward. `crates/libz-rs-sys/csrc/gzvprintf_probe.c` is therefore the caller,
// and `build.rs` links it with `cargo::rustc-link-arg-tests`, which cargo applies to test targets
// and to nothing else. The shipped `libz.a` and `libz.so` are unchanged, and
// `tests/symbol_parity.rs` is what proves it rather than this comment.

// The test-only C helper that owns the `va_list`. See the section comment above.
//
// Declared with the same variadic form the probe defines, so the compiler marshals arguments the way
// the C side reads them. Calling a C-variadic function through a declaration is stable Rust; only
// *defining* one is not, which is precisely why the definition is in C and this is not. A `//`
// comment rather than a doc comment, because an `extern` block is not an item a doc comment attaches
// to -- the same shape the `gzprintf` declaration above uses.
#[cfg(feature = "gz")]
extern "C" {
    fn _zlib_rs_gzvprintf_probe(file: z::gzFile, format: *const c_char, ...) -> c_int;
}

/// `gzvprintf` writes through a caller-owned `va_list`, and answers its documented codes.
///
/// Six cases, and each one is a distinct branch of `gzwrite.c` L403-L485 rather than a repetition:
///
/// 1. **A format with no directives.** The `va_list` is started and never read. Returns the literal
///    length.
/// 2. **A format with three directives of three different types** -- `%s`, `%d`, `%c`. This is the
///    case that can only be written in C, and the one that would fail if the calling convention were
///    wrong: an argument sequence walked at the wrong widths produces a wrong string rather than an
///    error code, so the bytes are read back and compared.
/// 3. **A read-mode stream.** `printf_begin` step 2 (`gzwrite.c` L421-L422) requires
///    `Z_STREAM_ERROR`.
/// 4. **A null `gzFile`.** `gzwrite.c` L418-L419 requires `Z_STREAM_ERROR`, and the shim performs
///    that test because a `&mut GzState` cannot be null.
/// 5. **A null format.** The reference has no such test and passes the pointer to `vsnprintf`, which
///    is undefined behaviour; this port rejects it with `Z_STREAM_ERROR`, and that divergence is
///    documented at `csrc/gzprintf_shim.c`. Asserted so the added guard cannot regress unnoticed.
/// 6. **An expansion longer than the pending buffer.** `gzwrite.c` L471 answers 0 -- not an error
///    code, and not a truncated count -- when `len >= state->size`, and the overflow sentinel is what
///    detects it. Driven with a 4 KiB argument against a deliberately small `gzbuffer`.
///
/// The whole file is then read back and compared byte for byte, because a return value alone cannot
/// distinguish "formatted and written" from "formatted and dropped".
#[cfg(feature = "gz")]
#[test]
fn gzvprintf_formats_through_a_c_va_list() {
    // Real file I/O, as at `example_c_parity`.
    if cfg!(miri) {
        println!("skipped under Miri: this suite performs real file I/O");
        return;
    }

    // One private, unguessable, owner-only directory holds both files this test opens, and its
    // `Drop` removes the tree on every path out -- including a panicking one. See `GzScratch`.
    let scratch = GzScratch::create();
    let path = scratch.path().to_path_buf();
    let fname = std::ffi::CString::new(path.as_os_str().to_string_lossy().as_bytes())
        .expect("a temporary path contains no interior NUL");

    // ---- case 4: a null handle, before anything is opened ----
    // SAFETY: a null `gzFile` is a published contract case -- `gzwrite.c` L418-L419 tests it rather
    // than dereferencing it, and the shim performs that test before any Rust code sees the handle.
    // The format is a `c"…"` literal with no directives, so no variadic argument is read.
    let err = unsafe { _zlib_rs_gzvprintf_probe(ptr::null_mut(), c"unused".as_ptr()) };
    assert_eq!(
        err, Z_STREAM_ERROR,
        "gzvprintf(NULL, ...) must be Z_STREAM_ERROR (gzwrite.c L418-L419)"
    );

    // ---- the write stream ----
    // SAFETY: both arguments are NUL-terminated C strings that outlive the call -- `fname` is a live
    // `CString` and the mode is a `c"…"` literal -- and `gzopen` only reads them.
    let file = unsafe { z::gzopen(fname.as_ptr(), c"wb".as_ptr()) };
    assert!(!file.is_null(), "gzopen error: {}", path.display());

    // ---- case 5: a null format on a valid, open, writable stream ----
    // SAFETY: `file` is the non-null handle just asserted. A null format is the case under test and
    // the shim rejects it before `vsnprintf` could see it, so nothing dereferences it.
    let err = unsafe { _zlib_rs_gzvprintf_probe(file, ptr::null()) };
    assert_eq!(
        err, Z_STREAM_ERROR,
        "a null format must be refused rather than passed to vsnprintf"
    );

    // ---- case 1: no directives ----
    // SAFETY: `file` is non-null and open for writing; the format is a `c"…"` literal with no
    // directives, so the callee reads no variadic argument at all.
    let plain = unsafe { _zlib_rs_gzvprintf_probe(file, c"plain".as_ptr()) };
    assert_eq!(
        plain,
        5,
        "gzvprintf returns the number of characters written: {}",
        gz_error_text(file)
    );

    // ---- case 2: three directives, three argument types ----
    // SAFETY: `file` is non-null and open for writing. The format's three directives -- `%s`, `%d`,
    // `%c` -- are matched, in order and in type, by exactly three further arguments: a
    // NUL-terminated `c"…"` literal, a `c_int` and a `c_int` holding a character value (C promotes
    // the `%c` argument to `int`, which is what is passed). So the callee's `va_arg` sequence agrees
    // with what was pushed.
    let mixed = unsafe {
        _zlib_rs_gzvprintf_probe(
            file,
            c"|%s=%d%c".as_ptr(),
            c"k".as_ptr(),
            42_i32,
            c_int::from(b'!'),
        )
    };
    assert_eq!(
        mixed,
        6,
        "the three-directive expansion is the six characters |k=42! : {}",
        gz_error_text(file)
    );

    // SAFETY: `file` is non-null and has not been closed; this call consumes it and the binding is
    // not used again.
    let err = unsafe { z::gzclose(file) };
    check_err(err, "gzclose after gzvprintf");

    // ---- read the file back: the return values above cannot prove the bytes landed ----
    // SAFETY: as for the write-phase `gzopen`.
    let file = unsafe { z::gzopen(fname.as_ptr(), c"rb".as_ptr()) };
    assert!(!file.is_null(), "gzopen for reading: {}", path.display());

    let mut got = [0_u8; 64];
    let capacity = c_uint::try_from(got.len()).expect("64 fits a C unsigned");
    // SAFETY: `file` is non-null and open for reading, and `capacity` bytes are writable at `got`'s
    // start -- it is a live 64-byte array that outlives the call and `capacity` is its own length.
    let read = unsafe { z::gzread(file, got.as_mut_ptr().cast(), capacity) };
    assert_eq!(
        read,
        11,
        "the two successful calls wrote 5 + 6 bytes: {}",
        gz_error_text(file)
    );
    assert_eq!(
        &got[..11],
        b"plain|k=42!",
        "gzvprintf wrote the wrong bytes, which is what a mis-marshalled va_list looks like"
    );

    // ---- case 3: a read-mode stream ----
    // SAFETY: `file` is non-null and open, but for READING, which is the case under test; the format
    // is a `c"…"` literal with no directives.
    let err = unsafe { _zlib_rs_gzvprintf_probe(file, c"nope".as_ptr()) };
    assert_eq!(
        err, Z_STREAM_ERROR,
        "gzvprintf on a read stream must be Z_STREAM_ERROR (gzwrite.c L421-L422)"
    );

    // SAFETY: `file` is non-null and has not been closed; this call consumes it.
    let err = unsafe { z::gzclose(file) };
    check_err(err, "gzclose after the read-mode case");

    // ---- case 6: an expansion the pending buffer cannot hold ----
    // A sibling inside the same private directory, so it is covered by the same `Drop`.
    let over = path.with_file_name("gzvprintf_overflow.gz");
    let overname = std::ffi::CString::new(over.as_os_str().to_string_lossy().as_bytes())
        .expect("a temporary path contains no interior NUL");
    // SAFETY: as for the write-phase `gzopen`.
    let file = unsafe { z::gzopen(overname.as_ptr(), c"wb".as_ptr()) };
    assert!(!file.is_null(), "gzopen error: {}", over.display());

    // The smallest buffer `gzbuffer` accepts is what makes the overflow reachable with a payload
    // this test can hold in a literal. `gzbuffer` must be called before the first write, which is
    // why this case has a stream of its own rather than reusing the one above.
    // SAFETY: `file` is non-null and nothing has been written through it yet, which is `gzbuffer`'s
    // documented precondition (`zlib.h` L1429-L1443).
    let err = unsafe { z::gzbuffer(file, 8) };
    check_err(err, "gzbuffer(8)");

    // 4096 characters of expansion against a pending buffer of at most a few dozen bytes.
    let long = vec![b'x'; 4096];
    let long = std::ffi::CString::new(long).expect("a run of 'x' contains no interior NUL");
    // SAFETY: `file` is non-null and open for writing. The one `%s` directive is matched by one
    // further argument, a live `CString` that outlives the call. The expansion cannot leave the
    // scratch region however long it is: the shim passes `vsnprintf` the region's own size, so the
    // overflow is detected by the sentinel rather than by writing past the end -- which is the
    // property this case exists to exercise.
    let dropped = unsafe { _zlib_rs_gzvprintf_probe(file, c"%s".as_ptr(), long.as_ptr()) };
    assert_eq!(
        dropped, 0,
        "an expansion at or beyond the pending buffer answers 0, not an error and not a truncated \
         count (gzwrite.c L471)"
    );

    // SAFETY: `file` is non-null and has not been closed; this call consumes it.
    let err = unsafe { z::gzclose(file) };
    check_err(err, "gzclose after the overflow case");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&over);

    println!("gzvprintf through a C va_list: 6 cases");
}

// ---------------------------------------------------------------------------------------------
//  `inflateResetKeep`, through the facade's raw-pointer adapter
// ---------------------------------------------------------------------------------------------

/// `inflateResetKeep` restarts decoding without discarding the window or the set dictionary.
///
/// The safe core's own suite already covers the *behaviour*; what had no coverage was the facade
/// adapter -- the `#[no_mangle] pub extern "C" fn` that validates a raw `z_streamp` and hands the
/// call inward. `tests/symbol_parity.rs` proves the symbol exists and is bound to the right version
/// node, which is a different claim: a symbol can be present, correctly versioned, and still be an
/// adapter nobody ever ran.
///
/// The difference from `inflateReset` is the whole point of the entry point and is what this asserts:
/// both rewind the stream, but `inflateResetKeep` retains the sliding window and the dictionary state
/// (`zlib.h` L2039 declares it; `inflate.c`'s `inflateResetKeep` is the shared tail `inflateReset`
/// calls after clearing `wsize`, `whave`, `wnext` and `head`). So a stream reset with *Keep* can
/// still decode a raw block that back-references data from before the reset, and one reset without it
/// cannot. Both directions are checked, because only the pair distinguishes the two functions.
#[test]
fn inflate_reset_keep_retains_the_window() {
    /// Compresses the two parts as one raw deflate stream, split at a `Z_SYNC_FLUSH`.
    ///
    /// `Z_SYNC_FLUSH` is the load-bearing choice, and `Z_FULL_FLUSH` would be the wrong one.
    /// A sync flush aligns the output to a byte boundary -- so the stream can be cut there -- while
    /// *keeping* the encoder's history, which is what lets the second part's matches reference bytes
    /// from the first. A full flush would reset that history and make the two parts independent, and
    /// a decoder with no window would then decode the second part perfectly well: the test would pass
    /// against an `inflateResetKeep` that kept nothing.
    fn halves(first: &[u8], second: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut strm = new_stream();
        // SAFETY: `strm` is a live, zeroed `z_stream` that outlives the call, and `ZLIB_VERSION`
        // with `size_of::<z_stream>()` is the handshake the entry point requires. Raw `windowBits`
        // of -15 selects the headerless form.
        //
        // `addr_of_mut!` throughout this test and the next, for the reason given on `compress`
        // above: `&raw mut` is Rust 1.82 syntax and these crates declare a 1.80 floor, so the
        // shorter spelling compiles here and breaks the `msrv` CI job.
        let err = unsafe {
            z::deflateInit2_(
                addr_of_mut!(strm),
                Z_BEST_SPEED,
                8,
                -15,
                8,
                Z_DEFAULT_STRATEGY,
                ZLIB_VERSION.as_ptr().cast::<c_char>(),
                stream_size(),
            )
        };
        check_err(err, "deflateInit2_ raw");

        let mut out = vec![0_u8; 4096];
        let mut produced: Vec<Vec<u8>> = Vec::with_capacity(2);
        for (index, part) in [first, second].into_iter().enumerate() {
            strm.next_in = part.as_ptr().cast_mut();
            strm.avail_in = uInt::try_from(part.len()).expect("the halves are short");
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = uInt::try_from(out.len()).expect("4096 fits a uInt");
            let flush = if index == 0 { Z_SYNC_FLUSH } else { Z_FINISH };
            // SAFETY: `strm` is the live stream just initialised; `avail_in` bytes are readable at
            // `next_in` (a live borrow of `part`) and `avail_out` are writable at `next_out` (a live
            // borrow of `out`), and both outlive the call.
            let err = unsafe { z::deflate(addr_of_mut!(strm), flush) };
            let want = if index == 0 { Z_OK } else { Z_STREAM_END };
            assert_eq!(err, want, "deflate half {index}");
            let written = out.len() - usize::try_from(strm.avail_out).expect("a uInt fits a usize");
            produced.push(out[..written].to_vec());
        }

        // SAFETY: `strm` is the live stream; this call releases its state and it is not used again.
        let err = unsafe { z::deflateEnd(addr_of_mut!(strm)) };
        check_err(err, "deflateEnd raw");

        let mut parts = produced.into_iter();
        let first = parts.next().expect("two halves were produced");
        let second = parts.next().expect("two halves were produced");
        (first, second)
    }

    /// Decodes `data` into `out` on an already-initialised raw stream.
    fn inflate_part(strm: &mut z_stream, data: &[u8], out: &mut [u8]) -> (c_int, usize) {
        strm.next_in = data.as_ptr().cast_mut();
        strm.avail_in = uInt::try_from(data.len()).expect("the halves are short");
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).expect("the buffer is short");
        // SAFETY: `strm` is a live, initialised inflate stream; `avail_in` bytes are readable at
        // `next_in` and `avail_out` writable at `next_out`, both borrowed from live slices that
        // outlive the call.
        let err = unsafe { z::inflate(strm, Z_NO_FLUSH) };
        let written = out.len() - usize::try_from(strm.avail_out).expect("a uInt fits a usize");
        (err, written)
    }

    // A payload whose second half is a long back-reference into the first. With the window kept, the
    // second half decodes; without it, the distance points before the start of the stream.
    let first = b"the quick brown fox jumps over the lazy dog. ".repeat(4);
    let second = b"the quick brown fox jumps over the lazy dog. ".repeat(4);
    let (head, tail) = halves(&first, &second);

    // ---- with the window kept ----
    let mut strm = new_stream();
    // SAFETY: `strm` is a live, zeroed `z_stream` that outlives the call, and the version and
    // `stream_size` arguments are the handshake `inflateInit2_` requires. -15 selects raw inflate.
    let err = unsafe {
        z::inflateInit2_(
            addr_of_mut!(strm),
            -15,
            ZLIB_VERSION.as_ptr().cast::<c_char>(),
            stream_size(),
        )
    };
    check_err(err, "inflateInit2_ raw");

    let mut out = vec![0_u8; first.len() + second.len() + 64];
    let (err, written) = inflate_part(&mut strm, &head, &mut out);
    check_err(err, "inflate of the first half");
    assert_eq!(&out[..written], first.as_slice(), "first half round-trip");

    // ★ THE CALL UNDER TEST. The facade adapter, on a real `z_stream` behind a raw pointer.
    // SAFETY: `strm` is the live stream initialised above and not yet ended, which is exactly the
    // state `inflateResetKeep` documents; the pointer is derived from a live local that outlives the
    // call.
    let err = unsafe { z::inflateResetKeep(addr_of_mut!(strm)) };
    check_err(err, "inflateResetKeep");

    // The documented reset half: the counters are cleared like any reset.
    assert_eq!(strm.total_in, 0, "inflateResetKeep clears total_in");
    assert_eq!(strm.total_out, 0, "inflateResetKeep clears total_out");
    assert!(strm.msg.is_null(), "inflateResetKeep clears msg");

    // The *keep* half: the window survived, so the second half's back-references resolve.
    let (err, written) = inflate_part(&mut strm, &tail, &mut out);
    assert_eq!(
        err, Z_STREAM_END,
        "the second half was closed with Z_FINISH, so inflate reports the end of the stream"
    );
    assert_eq!(
        &out[..written],
        second.as_slice(),
        "inflateResetKeep must retain the sliding window, so a back-reference across the reset \
         still resolves"
    );

    // SAFETY: `strm` is the live stream; this releases its state and it is not used again.
    let err = unsafe { z::inflateEnd(addr_of_mut!(strm)) };
    check_err(err, "inflateEnd after inflateResetKeep");

    // ---- and the contrast: a full reset discards it ----
    // Without this half the test would pass against an `inflateResetKeep` that was a synonym for
    // `inflateReset`, which is the one way the entry point could be wrong and still look right.
    let mut strm = new_stream();
    // SAFETY: as for the first `inflateInit2_`.
    let err = unsafe {
        z::inflateInit2_(
            addr_of_mut!(strm),
            -15,
            ZLIB_VERSION.as_ptr().cast::<c_char>(),
            stream_size(),
        )
    };
    check_err(err, "inflateInit2_ raw, second stream");

    let (err, _) = inflate_part(&mut strm, &head, &mut out);
    check_err(err, "inflate of the first half, second stream");

    // SAFETY: `strm` is the live stream initialised above and not yet ended.
    let err = unsafe { z::inflateReset(addr_of_mut!(strm)) };
    check_err(err, "inflateReset");

    let (err, _) = inflate_part(&mut strm, &tail, &mut out);
    assert_eq!(
        err, Z_DATA_ERROR,
        "inflateReset discards the window, so the same back-reference must now be rejected -- if \
         this succeeds, inflateResetKeep and inflateReset are doing the same thing"
    );

    // SAFETY: `strm` is the live stream; this releases its state and it is not used again.
    let err = unsafe { z::inflateEnd(addr_of_mut!(strm)) };
    check_err(err, "inflateEnd after inflateReset");

    println!("inflateResetKeep retains the window; inflateReset does not");
}
