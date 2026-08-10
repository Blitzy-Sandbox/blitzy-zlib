/* ==========================================================================
 *  crates/libz-rs-sys/csrc/gzvprintf_probe.c -- exercising `gzvprintf`
 * ==========================================================================
 *
 * One function lives here, it is hidden, and it exists so that `gzvprintf`
 * can be CALLED the way a real consumer calls it.
 *
 * --------------------------------------------------------------------------
 *  Why a C file is the only way to test this one entry point
 * --------------------------------------------------------------------------
 *
 * `gzvprintf` takes a `va_list` (`zlib.h` L2045-L2050).  Stable Rust cannot
 * CONSTRUCT one: `core::ffi::VaList` is unstable, `extern "C" fn f(..., ...)`
 * needs the unstable `c_variadic` feature, and inventing a layout for
 * `va_list` and passing it as an opaque pointer is ABI-invalid -- it is
 * `__va_list_tag[1]` on x86-64 SysV, a `struct __va_list` passed by value on
 * AArch64 AAPCS64 and a plain `char *` on i386, and a single Rust declaration
 * cannot be right on all three.  That is the same reasoning
 * `csrc/gzprintf_shim.c` records for why the variadic edge is written in C at
 * all, and it applies just as forcefully to a caller.
 *
 * The consequence, before this file existed, was a real gap: every other one
 * of the 95 exported functions had a test that ran it, and `gzvprintf` had
 * only a symbol-parity entry and a Rust function-pointer assignment -- which
 * prove the symbol is PRESENT and has the right TYPE, and prove nothing at
 * all about the platform-specific `va_list` forwarding inside it.  A
 * `gzvprintf` that mis-forwarded its `va_list`, or that reported the wrong
 * count, or that wrote the caller's bytes into the wrong half of the input
 * buffer, would have passed everything the suite had.
 *
 * Calling `gzprintf` instead is NOT the same test.  `gzprintf` builds the
 * `va_list` itself and forwards it once (`gzwrite.c` L487-L495), so it
 * exercises the callee's body but never the case a logging wrapper actually
 * produces: a `va_list` started in the CALLER's frame and handed across the
 * boundary.  That is the path this probe covers, and it is the reason the
 * probe is variadic itself rather than taking a pre-built list.
 *
 * --------------------------------------------------------------------------
 *  What it may and may not do
 * --------------------------------------------------------------------------
 *
 * It is `va_start`, one call, `va_end`, `return`.  It must never grow logic,
 * inspect the format, touch the `gz_state` behind the `gzFile`, or decide
 * anything: every assertion belongs to `crates/libz-rs-sys/tests/gz_printf.rs`,
 * whose whole purpose would be defeated by a probe that corrected or
 * second-guessed what it observed.  It deliberately does NOT null-check
 * either argument -- `gzvprintf`'s own rejection of a null `file` or `format`
 * is one of the behaviours under test, so both are forwarded unchanged.
 *
 * --------------------------------------------------------------------------
 *  Visibility, and why it changes no exported surface
 * --------------------------------------------------------------------------
 *
 * `ZLIB_INTERNAL` -- `__attribute__((visibility("hidden")))` under
 * `HAVE_HIDDEN`, which `build.rs` defines for every shim exactly as
 * `configure` L962-L963 does for the C library -- and the name begins with an
 * underscore, which `zlib.map`'s `local: _*;` entry hides a second time.  So
 * the packaged library's dynamic table is untouched: it still publishes
 * exactly the 95 functions plus 16 version nodes that
 * `crates/libz-rs-sys/tests/symbol_parity.rs` requires, and this symbol is
 * reachable only from the static archive, which is where a test binary links
 * it from.  `cbindgen.toml`'s `[export] exclude` keeps it out of the
 * generated header for the same reason the other `_zlib_rs_*` names are
 * there.
 *
 * Compiled by `crates/libz-rs-sys/build.rs` under `libz-compat` AND `gz`,
 * the same pair `csrc/gzprintf_shim.c` needs, because a build without the
 * `gzFile` layer has no `gzvprintf` for it to call.
 */

/* `zutil.h` for `ZLIB_INTERNAL`, and `zlib.h` -- which it includes -- for
 * `gzFile` and `ZEXPORTVA`.  Including the real headers rather than restating
 * anything is what makes it impossible for this file to disagree with the
 * contract: if `gzvprintf`'s declaration ever changed, this would not compile.
 *
 * The include path is the repository root, which `build.rs` passes as `-I`.
 */
#include "zutil.h"
#include "zlib.h"

#include <stdarg.h>

/* Under `Z_SOLO` there is no `gzFile` layer, so there is nothing to probe --
 * and `csrc/gzprintf_shim.c` compiles to nothing for the same reason.
 */
#ifndef Z_SOLO

/* Forward `...` to `gzvprintf` through a `va_list` this function owns.
 *
 * Returns exactly what `gzvprintf` returned: the byte count on success, 0
 * when the formatted result did not fit, or a negative zlib code.
 *
 * `va` is started, handed to `gzvprintf` exactly once, and ended; it is never
 * read again afterwards, which is what makes forwarding it legal -- the same
 * discipline `gzprintf` itself follows at `gzwrite.c` L491-L494.
 */
int ZLIB_INTERNAL _zlib_rs_gzvprintf_probe(gzFile file, const char *format, ...) {
    va_list va;
    int ret;

    va_start(va, format);
    ret = gzvprintf(file, format, va);
    va_end(va);
    return ret;
}

#endif /* !Z_SOLO */
