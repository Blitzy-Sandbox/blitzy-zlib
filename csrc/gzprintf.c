/*
 * csrc/gzprintf.c — variadic `gzprintf` / `gzvprintf` implementations for
 * `zlib-rs` (the formatting half of the C-ABI shim).
 * ===========================================================================
 *
 * WHY THIS FILE EXISTS
 *   zlib's `gzprintf(gzFile, const char *format, ...)` and its `va_list` form
 *   `gzvprintf` are variadic. Stable Rust cannot read a C `va_list` (the
 *   `c_variadic` feature is unstable and would break the crate's edition-2024
 *   stable MSRV), so the formatting cannot be implemented in Rust. This tiny,
 *   self-authored shim performs the `vsnprintf` formatting into a bounded
 *   buffer and then calls back into the safe-Rust gzip engine
 *   (`zlibrs_gzprintf_write`, defined in `src/ffi.rs`) to do the actual
 *   compressed write. This is exactly the "format into a bounded buffer before
 *   calling the Rust safe core" strategy.
 *
 * SYMBOL OWNERSHIP — note the `_impl` suffix
 *   The two functions below are NOT the public zlib symbols. They are the
 *   internal *implementations*, named `zlibrs_gzprintf_impl` /
 *   `zlibrs_gzvprintf_impl`. The public, canonical `gzprintf` / `gzvprintf`
 *   symbols are exported from `src/ffi.rs` as `#[unsafe(naked)]` Rust
 *   trampolines that tail-`jmp` here, preserving every argument register (so
 *   the variadic arguments pass through untouched). That indirection is what
 *   lets the canonical symbols be EXPORTED from the `cdylib`: rustc only places
 *   Rust `#[no_mangle]` items into the cdylib's linker version-script `global:`
 *   list, so a C-defined `gzprintf` would otherwise be localized out of the
 *   shared object's dynamic symbol table. Routing through a Rust naked symbol
 *   makes `gzprintf` a first-class exported symbol of both the `cdylib` and the
 *   `staticlib`, while the actual variadic capture still happens here in C.
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
 * `zlibrs_gzvprintf_impl` — the implementation behind the public `gzvprintf`.
 * Format `format` with `va` into a bounded buffer, then hand the finished bytes
 * to the Rust safe core. Matches zlib's `gzvprintf` signature and return
 * contract (number of uncompressed bytes written, `0` if empty/too large for
 * the stream buffer, or a negative `Z_*` code).
 */
int zlibrs_gzvprintf_impl(zlibrs_gzFile file, const char *format, va_list va) {
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
 * `zlibrs_gzprintf_impl` — the implementation behind the public `gzprintf`.
 * The variadic arguments arrive here (forwarded register-for-register by the
 * Rust naked trampoline `gzprintf`), are packaged into a `va_list`, and handed
 * to `zlibrs_gzvprintf_impl`, exactly as zlib's `gzprintf` forwards to
 * `gzvprintf`.
 */
int zlibrs_gzprintf_impl(zlibrs_gzFile file, const char *format, ...) {
    va_list va;
    int ret;

    va_start(va, format);
    ret = zlibrs_gzvprintf_impl(file, format, va);
    va_end(va);
    return ret;
}
