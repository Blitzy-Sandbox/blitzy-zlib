/* ==========================================================================
 *  crates/libz-rs-sys/csrc/gzprintf_shim.c -- the variadic ABI boundary
 * ==========================================================================
 *
 * This is the ONLY C translation unit in the port, and it exists for one
 * reason: `gzprintf` is variadic and `gzvprintf` takes a `va_list`, and
 * stable Rust can DEFINE neither.
 *
 *   * `extern "C" fn f(x: c_int, ...)` -- defining a C-variadic function --
 *     requires the unstable `c_variadic` feature.  AAP §0.7.1 (h) pins the
 *     toolchain to stable Rust 1.80 / edition 2021 and forbids
 *     `#![feature(...)]`, so this is not merely inconvenient, it is closed.
 *   * `core::ffi::VaList` is unstable for the same reason, and its `arg`
 *     method is `unsafe` besides.
 *   * Guessing a layout for `va_list` and forwarding it as an opaque pointer
 *     is ABI-INVALID and cannot be made portable.  `va_list` is
 *     `__va_list_tag[1]` on x86-64 SysV (an array, so it decays to a pointer
 *     when passed), a `struct __va_list` passed BY VALUE on AArch64 AAPCS64,
 *     and a plain `char *` on i386.  A single Rust declaration cannot be
 *     right on all three, and being wrong corrupts register and stack state.
 *
 * So the variadic edge is written in C, where `va_start`/`va_end` are the
 * compiler's own business and correct by construction on every target.  It is
 * deliberately the smallest such edge that can exist: it renders the caller's
 * format into a bounded region the safe Rust core hands it, and does no
 * accounting, no allocation, no error policy and no format parsing of its own.
 * Everything that could get zlib's semantics wrong lives in
 * `crates/zlib-rs/src/gz/printf.rs`, in safe Rust, under test.
 *
 * --------------------------------------------------------------------------
 *  What this file is NOT allowed to become
 * --------------------------------------------------------------------------
 *
 *   * It must not grow logic.  Any new behaviour belongs in `printf.rs`.
 *   * It must not parse the format string.  `vsnprintf` does that, and it is
 *     the platform's audited implementation.
 *   * It must not use an unbounded formatter.  `vsprintf`/`sprintf` are what
 *     `ZLIB_INSECURE` re-enables in the C reference (`gzwrite.c` L455-L469
 *     and the `gzguts.h` L59-L104 cascade); this port has no such build and
 *     never will.  Rust always has a bounded formatter, and so does C99.
 *   * It must not touch the `gz_state` behind the `gzFile`.  It is opaque
 *     here on purpose: only the Rust side knows the layout, and only the Rust
 *     side may move a cursor.
 *
 * --------------------------------------------------------------------------
 *  Who compiles it
 * --------------------------------------------------------------------------
 *
 * CARGO does, through `crates/libz-rs-sys/build.rs`, which drives the platform
 * C compiler directly -- no `cc` crate and no other `[build-dependencies]`, so
 * the dependency inventory AAP §0.5.1.3 freezes is untouched and §0.6.4.1's
 * rule that the reference C sources are the differential crate's business alone
 * still holds: nothing here compiles a line of the C library.
 *
 * That is deliberate and it is what gives the workspace ONE complete artifact.
 * The object is archived and rustc merges the archive into the `libz.a` it
 * produces, so `cargo build -p libz-rs-sys --features libz-compat` yields a
 * static library that defines every one of the 95 functions `zlib.h` declares.
 * The packaged shared object is then relinked from exactly that archive:
 *
 *     $(LDSHARED) $(SFLAGS) -o <staged libz.so.VER> \
 *         -Wl,--whole-archive <cargo libz.a> -Wl,--no-whole-archive
 *
 * where `$(LDSHARED)` carries `-Wl,-soname,libz.so.1,--version-script,
 * ${SRCDIR}zlib.map`, which is what produces the 16 symbol-version nodes.  That
 * relink cannot be folded into cargo: rustc attaches a version script of its own
 * to every cdylib link, `zlib.map` cannot be added alongside it, and a symbol
 * this file defines gets no dynamic entry there at all.  The artifact matrix in
 * `crates/libz-rs-sys/src/lib.rs` states that once, with the measurements.
 *
 * --------------------------------------------------------------------------
 *  Symbol visibility -- read before renaming anything
 * --------------------------------------------------------------------------
 *
 * `zlib.map` has NO `local: *;` catch-all.  Its `ZLIB_1.2.0` node lists nine
 * names plus the pattern `_*`, and a symbol matched by nothing at all is
 * assigned to the base version and stays EXPORTED.  So the two Rust-side
 * helpers below would land in the dynamic symbol table and break the
 * 111-symbol parity target -- unless their names begin with an underscore,
 * which `_*` then hides.
 *
 * That is exactly the mechanism zlib itself uses for `_tr_init`, `_tr_tally`,
 * `_tr_flush_block`, `_tr_flush_bits`, `_tr_align` and `_tr_stored_block`, so
 * the naming below follows the reference implementation's own convention
 * rather than inventing one.  DO NOT rename these helpers to something
 * without the leading underscore, and DO NOT add them to `zlib.map` -- that
 * file is immutable (AAP §0.8.1 directive 2).
 *
 * `gzprintf` and `gzvprintf` themselves are the opposite case and must stay
 * exported: `gzvprintf` is named in the `ZLIB_1.2.7.1` node, and `gzprintf`
 * predates the first node so it takes the base binding, matching the C
 * library exactly.
 */

/* zlib's own public header, unmodified.  Including it rather than restating
 * the prototypes is deliberate and load-bearing: `gzFile`, `ZEXTERN`,
 * `ZEXPORTVA`, the `Z_*` codes and the `#if defined(STDC) ||
 * defined(Z_HAVE_STDARG_H)` guard all come from the immutable contract, so
 * the definitions below CANNOT drift from the declarations a caller compiled
 * against.  If the signatures ever disagreed, this file would not compile.
 *
 * The include path is the repository root, which is where the C build already
 * points ($(ZINCOUT) is `-I.`).  `zconf.h` is picked up from the same place.
 */
#include "zlib.h"

#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>

/* Under `Z_SOLO` there is no `gzFile` layer at all: `gzguts.h` is what pulls in
 * the file I/O world, and the whole of `gzwrite.c` is unreachable in that
 * configuration.  Compiling to nothing is then exactly right -- the reference
 * library does not export `gzprintf` either, so silence here IS parity.
 */
#ifndef Z_SOLO

/* The stdarg form is the only one this file implements, and its absence is a
 * hard error rather than a quiet omission.
 *
 * It would be wrong to treat this the way `Z_SOLO` is treated above.  Without
 * `STDC`/`Z_HAVE_STDARG_H`, `zlib.h` still declares `gzprintf` -- as the
 * unprototyped K&R form (L1550-L1551) -- and `gzwrite.c` still DEFINES it, as
 * the twenty-`int` variant that reads its arguments positionally instead of
 * through `va_list`.  So the reference library exports `gzprintf` in that
 * configuration, and compiling to nothing would leave this port one documented
 * export short of it, with nothing but a link-time undefined symbol in some
 * downstream consumer to reveal it.  That silent-gap failure mode is the very
 * thing this file exists to close, so the build stops here instead.
 *
 * The condition is unreachable on every target in scope: `zconf.h` L212-L237
 * defines `STDC` for `__STDC_VERSION__`, `__STDC__`, `__cplusplus`, `__GNUC__`,
 * `__BORLANDC__`, MSDOS/WINDOWS/WIN32, OS2/AIX and OS400 -- which covers every
 * Tier-1 toolchain.  Reaching it means a pre-ANSI compiler, and porting the
 * twenty-`int` variant is out of scope with the rest of the non-Tier-1 build
 * surface.
 */
#if !defined(STDC) && !defined(Z_HAVE_STDARG_H)
#error "gzprintf_shim.c implements the stdarg gzprintf/gzvprintf only, but this compiler defines neither STDC nor Z_HAVE_STDARG_H, so zlib.h declares the pre-ANSI gzprintf() form instead. Porting gzwrite.c's twenty-int variant is out of scope for this port's Tier-1 targets; compile with an ANSI C or later compiler, or define Z_HAVE_STDARG_H."
#endif

/* A bounded formatter is not optional here, so a compiler that provably does
 * not declare one is refused with a message that names the fix.
 *
 * `vsnprintf` arrived in C99.  In a STRICT pre-C99 mode -- `-std=c89`,
 * `-std=c90` or `-std=iso9899:199409`, all of which define `__STRICT_ANSI__`
 * and leave `__STDC_VERSION__` below 199901L -- the standard headers hide it,
 * and using it anyway would be an implicit declaration.
 *
 * The reference implementation's two answers to that situation are both
 * unavailable to this port.  `ZLIB_INSECURE` re-enables the UNBOUNDED
 * `vsprintf` (`gzwrite.c` L455-L469; `.github/workflows/c-std.yml` passes
 * `-DZLIB_INSECURE` for exactly the c89/gnu89/c94 rows), which would
 * reintroduce the overflow class this port exists to remove.  Without it, C
 * compiles `gzprintf` down to a body that ignores its arguments and returns
 * `Z_STREAM_ERROR` (`gzwrite.c` L406-L412), which would silently fail
 * `test/example.c`'s `gzprintf(file, ", %s!", "hello") == 8` assertion --
 * a broken drop-in replacement that reports itself as working.
 *
 * So neither degradation is acceptable, and the build stops instead.  Note
 * that this file is compiled by `build.rs` with the platform compiler's
 * ordinary flags (gnu17 by default), never as part of the C89 conformance
 * sweep, so this is a guard against misconfiguration rather than a
 * restriction on any supported build.  `HAVE_VSNPRINTF` is honoured as the
 * escape hatch because it is zlib's own spelling of "this platform has it
 * regardless of what the standard version says" (`gzguts.h` L59-L104).
 */
#if defined(__STRICT_ANSI__) && \
    (!defined(__STDC_VERSION__) || __STDC_VERSION__ < 199901L)
#  ifdef HAVE_VSNPRINTF
/* The builder has asserted that the platform provides it even though the
 * standard headers, in this conformance mode, do not declare it.  Taking them
 * at their word means declaring it here, because an implicit declaration would
 * be a warning and this project builds warning-free.  The signature is fixed
 * by C99 7.19.6.12 and by POSIX, so there is nothing to guess.
 */
extern int vsnprintf(char *str, size_t size, const char *format, va_list ap);
#  else
#error "gzprintf_shim.c needs a bounded vsnprintf: compile it with -std=c99 or later (or define HAVE_VSNPRINTF). This port has no unbounded formatting path and will not substitute a Z_STREAM_ERROR stub for gzprintf."
#  endif
#endif

/* ------------------------------------------------------------------------ */
/*  The Rust side of the boundary                                           */
/* ------------------------------------------------------------------------ */

/* Prepare the stream and lend out the scratch region.
 *
 * Implemented in `crates/libz-rs-sys/src/gz.rs` over
 * `zlib_rs::gz::printf_begin`, which is the port of `gzvprintf`'s first half
 * (`gzwrite.c` L416-L453): the mode and error guards, `gz_init`, `gz_zero`,
 * `gz_vacate`, and the overflow sentinel.
 *
 * On success returns `Z_OK` (0) and sets `*scratch` to the first byte of a
 * writable region of exactly `*size` bytes whose LAST byte has been set to
 * zero.  On failure returns the negative zlib code `gzvprintf` must return
 * and leaves `*scratch` and `*size` untouched.
 *
 * `*size` is never zero on success, so `vsnprintf` always has room for at
 * least the terminator.
 */
extern int _zlib_rs_gzprintf_begin(gzFile file,
                                   unsigned char **scratch,
                                   size_t *size);

/* Account for the formatted result and produce `gzvprintf`'s return value.
 *
 * Implemented over `zlib_rs::gz::printf_commit`, the port of `gzvprintf`'s
 * second half (`gzwrite.c` L471-L483): the three-part rejection test, the
 * `avail_in`/`x.pos` accounting, and the closing `gz_vacate`.
 *
 * `reported` is what `vsnprintf` RETURNED, widened -- that is, the length it
 * WOULD have written, which may exceed the region.  A negative return is
 * passed as `(size_t)-1`, which reproduces C's own
 * `(unsigned)len >= state->size` comparison on a negative `int` and folds an
 * encoding error into "did not fit".
 *
 * Returns the byte count on success, 0 when the result did not fit (with
 * nothing written or counted), or a negative zlib code.
 */
extern int _zlib_rs_gzprintf_commit(gzFile file, size_t reported);

/* ------------------------------------------------------------------------ */
/*  gzvprintf -- zlib.h L2045-L2050, gzwrite.c L403-L485                    */
/* ------------------------------------------------------------------------ */

int ZEXPORTVA gzvprintf(gzFile file, const char *format, va_list va) {
    unsigned char *scratch;
    size_t size;
    size_t reported;
    int status;
    int len;

    /* `gzwrite.c` L418-L419: `if (file == NULL) return Z_STREAM_ERROR;`.  The
     * safe core cannot perform this test -- a `&mut GzState` cannot be null --
     * so it belongs on this side, and `printf_begin`'s documentation says so.
     *
     * The `format == NULL` test has no line in the reference, which passes the
     * pointer straight to `vsnprintf`; a null format there is undefined
     * behaviour.  Rejecting it with the same code costs nothing and removes a
     * crash a caller could trigger with one mistake.
     */
    if (file == NULL || format == NULL)
        return Z_STREAM_ERROR;

    scratch = NULL;
    size = 0;

    /* L421-L453, all of it, in the safe core. */
    status = _zlib_rs_gzprintf_begin(file, &scratch, &size);
    if (status != Z_OK)
        return status;

    /* Defensive, and cheap: a zero-width region would make `vsnprintf`'s size
     * argument zero, which is legal but means "write nothing, not even the
     * terminator", and the sentinel could then never be reached.  The core
     * guarantees `size >= 1` on success; this refuses to proceed rather than
     * trust the guarantee, and reports the same code the core uses when the
     * region cannot be addressed.
     */
    if (scratch == NULL || size == 0)
        return Z_BUF_ERROR;

    /* L455-L469, the ONE step that needs a `va_list`.
     *
     * `vsnprintf` writes at most `size` bytes INCLUDING the terminating NUL,
     * so it cannot leave the region, and it returns the length it WOULD have
     * written.  Both properties are exactly what the core's rejection test is
     * written against.  `va` is consumed here and nowhere else -- there is no
     * second formatting pass, so no `va_copy` is needed.
     */
    len = vsnprintf((char *)scratch, size, format, va);

    /* Widen for the core.  C's own code casts a negative `len` to `unsigned`
     * and lets the `>= state->size` test reject it (L471); `(size_t)-1` is the
     * same trick at 64 bits and is what `printf_commit` documents as the
     * "formatter failed" encoding.
     */
    reported = (len < 0) ? (size_t)-1 : (size_t)len;

    /* L471-L483, all of it, in the safe core. */
    return _zlib_rs_gzprintf_commit(file, reported);
}

/* ------------------------------------------------------------------------ */
/*  gzprintf -- zlib.h L1548-L1551, gzwrite.c L487-L495                     */
/* ------------------------------------------------------------------------ */

int ZEXPORTVA gzprintf(gzFile file, const char *format, ...) {
    va_list va;
    int ret;

    /* Byte for byte the reference implementation's body.  `va` is started,
     * handed to `gzvprintf` exactly once, and ended; it is never read again
     * afterwards, which is what makes forwarding it legal.
     */
    va_start(va, format);
    ret = gzvprintf(file, format, va);
    va_end(va);
    return ret;
}

#endif /* !Z_SOLO */
