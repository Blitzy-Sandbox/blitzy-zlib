/*
 * csrc/gzprintf.c — variadic `gzprintf` / `gzvprintf` implementations for
 * `zlib-rs` (the formatting half of the C-ABI shim).
 * ===========================================================================
 *
 * WHY THIS FILE EXISTS
 *   zlib's `gzprintf(gzFile, const char *format, ...)` and its `va_list` form
 *   `gzvprintf` are variadic. Stable Rust cannot DEFINE a C variadic function
 *   and cannot read a C `va_list` (the `c_variadic` feature is unstable and
 *   would break the crate's edition-2024 stable MSRV of Rust 1.85), so these
 *   two entry points cannot be implemented in Rust. This tiny, self-authored
 *   shim performs the `vsnprintf` formatting into a bounded buffer and then
 *   calls back into the safe-Rust gzip engine (`zlibrs_gzprintf_write`, defined
 *   in `src/ffi.rs`) to do the actual compressed write. This is exactly the
 *   "format into a bounded buffer before calling the Rust safe core" strategy.
 *
 * SYMBOL OWNERSHIP — these ARE the public zlib symbols
 *   The two functions below (`gzvprintf` and `gzprintf`) are the canonical,
 *   public zlib C-API symbols themselves — NOT internal `_impl` helpers behind
 *   a Rust trampoline. Defining them directly in C keeps the C variadic calling
 *   convention exact (the arguments are captured by genuine C `va_start` /
 *   `vsnprintf`) WITHOUT relying on a `#[unsafe(naked)]` Rust trampoline, whose
 *   `naked_asm!` was only stabilized in Rust 1.88. Removing that dependency is
 *   what lets the optional `capi` drop-in build on the crate's declared
 *   Rust 1.85 MSRV (AAP §0.3.3 / §0.7.2).
 *
 *   EXPORT NOTE (cdylib): a `staticlib` (`.a`) archives every object file, so
 *   these C symbols are present in the static drop-in automatically. A `cdylib`
 *   (`.so`/`.dylib`/`.dll`), however, only exports the symbols rustc places in
 *   its generated export list — i.e. the crate's own `#[no_mangle]` Rust items —
 *   so a C-defined symbol would be localized out of the shared object's dynamic
 *   symbol table by default. `build.rs` therefore force-exports `gzprintf` /
 *   `gzvprintf` from the cdylib (an `--undefined` root + a `--version-script`
 *   on ELF targets, with the equivalent additive linker flags on mach-o / MSVC),
 *   so both canonical symbols are first-class exports of the `cdylib` and the
 *   `staticlib` drop-in alike.
 *
 * SCOPE / DEPENDENCIES
 *   This is the crate's OWN source, not an external C library: it is compiled
 *   by `build.rs` (via the `cc` build-dependency) ONLY when the `capi` and
 *   `gz-io` features are enabled, and linked into the `cdylib`/`staticlib`
 *   drop-in artifacts. The default, pure-Rust library never compiles or links
 *   it, so the shipped Rust crate remains free of any C dependency (AAP
 *   §0.7.2); the C shim exists purely because the *C-ABI drop-in* must honour
 *   C's variadic calling convention exactly.
 */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

/* zlib return codes used on this shim's local error paths (values from
 * zlib.h). All other return values come straight from the Rust core. */
#define ZLIBRS_Z_STREAM_ERROR (-2)
#define ZLIBRS_Z_MEM_ERROR (-4)

/* Opaque gzip handle; forwarded verbatim to the Rust core. */
typedef void *zlibrs_gzFile;

/*
 * Implemented in Rust (`src/ffi.rs` :: `zlibrs_gzprintf_write`): write `len`
 * already-formatted bytes from `buf` to the gzip stream `file` through the safe
 * gz engine, returning the number of uncompressed bytes written, `0` when the
 * input is empty or exceeds the stream buffer's `want` size, or a negative
 * `Z_*` error code (e.g. for a null/invalid handle).
 */
extern int zlibrs_gzprintf_write(zlibrs_gzFile file, const unsigned char *buf,
                                 int len);

/*
 * `gzvprintf` — the canonical zlib `va_list` write entry point. Format `format`
 * with `va` into a bounded buffer, then hand the finished bytes to the Rust
 * safe core. Matches zlib's `gzvprintf` signature and return contract (number
 * of uncompressed bytes written, `0` if empty/too large for the stream buffer,
 * or a negative `Z_*` code).
 */
int gzvprintf(zlibrs_gzFile file, const char *format, va_list va) {
    char stackbuf[1024];
    va_list cp;
    int len;

    if (format == NULL) {
        /* Nothing to format; let the Rust core validate the handle and return
         * the appropriate code (Z_STREAM_ERROR for a bad handle, else 0). */
        return zlibrs_gzprintf_write(file, (const unsigned char *)"", 0);
    }

    /* Measure the formatted length WITHOUT writing: `vsnprintf` with a NULL
     * buffer and size 0 returns the number of bytes that would be produced.
     * `va` itself must not be consumed by the measurement, so use a copy. */
    va_copy(cp, va);
    len = vsnprintf(NULL, 0, format, cp);
    va_end(cp);

    if (len < 0) {
        /* Genuine encoding error from the C library. */
        return ZLIBRS_Z_STREAM_ERROR;
    }
    if (len == 0) {
        return zlibrs_gzprintf_write(file, (const unsigned char *)"", 0);
    }

    if ((size_t)len < sizeof(stackbuf)) {
        /* Fast path: render into the stack buffer (no heap traffic). */
        vsnprintf(stackbuf, sizeof(stackbuf), format, va);
        return zlibrs_gzprintf_write(file, (const unsigned char *)stackbuf,
                                     len);
    } else {
        /* Large output: render into a transient heap buffer. The Rust core
         * enforces the stream's `want`-size limit (returning 0 if the result
         * would not fit), so the observable behaviour — bytes written and
         * return value — matches zlib regardless of this buffer's size. */
        char *buf = (char *)malloc((size_t)len + 1);
        int ret;
        if (buf == NULL) {
            return ZLIBRS_Z_MEM_ERROR;
        }
        vsnprintf(buf, (size_t)len + 1, format, va);
        ret = zlibrs_gzprintf_write(file, (const unsigned char *)buf, len);
        free(buf);
        return ret;
    }
}

/*
 * `gzprintf` — the canonical zlib variadic write entry point. The variadic
 * arguments are captured here with genuine C `va_start`, packaged into a
 * `va_list`, and handed to `gzvprintf`, exactly as zlib's `gzprintf` forwards
 * to `gzvprintf`.
 */
int gzprintf(zlibrs_gzFile file, const char *format, ...) {
    va_list va;
    int ret;

    va_start(va, format);
    ret = gzvprintf(file, format, va);
    va_end(va);
    return ret;
}
