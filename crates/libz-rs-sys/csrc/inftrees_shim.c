/* ==========================================================================
 *  crates/libz-rs-sys/csrc/inftrees_shim.c -- the `codetype` ABI boundary
 * ==========================================================================
 *
 * One function lives here, and it lives here for one reason: `inflate_table`'s
 * first parameter is a C enum.
 *
 *     int ZLIB_INTERNAL inflate_table(codetype type, unsigned short FAR *lens,
 *                                     unsigned codes, code FAR * FAR *table,
 *                                     unsigned FAR *bits,
 *                                     unsigned short FAR *work);
 *                                                    -- inftrees.h L60-L62
 *
 * `codetype` is `typedef enum { CODES, LENS, DISTS } codetype;` (inftrees.h
 * L53-L58).  Two facts about it decide the shape of this file:
 *
 *   1. It is a DISTINCT TYPE for the purposes of declaration compatibility.  A
 *      Rust `#[no_mangle] pub extern "C" fn inflate_table(type_: c_int, ...)`
 *      is ABI-identical on every target in scope -- an enum of 0..2 is passed
 *      in a 32-bit register exactly as an `int` is -- but a C translation unit
 *      that redeclares that signature alongside `inftrees.h` does not compile:
 *      `conflicting types for 'inflate_table'`.  The contract this port must
 *      not drift from is a DECLARATION, not just a calling convention.
 *
 *   2. Its representation is implementation-defined.  GCC and Clang give an
 *      all-non-negative enum `unsigned int`; another compiler may choose `int`.
 *      So even naming the "right" fixed-width type in Rust would be guessing at
 *      something only the C compiler knows.  Letting the C compiler decide is
 *      the only way to be exactly right on every target.
 *
 * There is a third reason, and it is a safety one rather than an ABI one.  A
 * Rust `#[repr(C)] enum CodeType { Codes, Lens, Dists }` would render the
 * prototype correctly, but constructing one from a C caller's arbitrary `int`
 * is instant undefined behaviour -- and `test/infcover.c` passes the
 * enumerators only by convention, while nothing in the language stops a caller
 * from passing 7.  So the enum stops here, at the C boundary where it is legal,
 * and what crosses into Rust is a plain `int` that the Rust side VALIDATES.
 *
 * --------------------------------------------------------------------------
 *  Why this symbol exists at all
 * --------------------------------------------------------------------------
 *
 * `inflate_table` is private: `zlib.map`'s `ZLIB_1.2.0` node lists it in the
 * `local:` block, so the reference shared library does not export it.  And yet
 * the unmodified `test/infcover.c` will not LINK without it, because its
 * `cover_trees` (L617-L639) calls it directly, twice, to provoke the
 * `ENOUGH`-exceeded return that a well-behaved `inflate()` can never produce:
 *
 *     ret = inflate_table(DISTS, lens, 16, &next, &bits, work);
 *
 * Both requirements hold at once because they are about two different
 * artifacts.  `infcover` links the STATIC library (test/CMakeLists.txt L95-L96
 * links it to ZLIB::ZLIBSTATIC), where the name is an ordinary global; the
 * shared object is linked under `--version-script=zlib.map`, where it is
 * hidden.  Compiling this file with `-DHAVE_HIDDEN` -- which the build script
 * always passes, matching configure L962-L963 -- additionally gives the
 * definition `visibility("hidden")` through `ZLIB_INTERNAL`, so it stays out of
 * a shared object's dynamic table even before the version script is applied.
 * That is exactly the pair of properties the C build has.
 *
 * --------------------------------------------------------------------------
 *  What this file is NOT allowed to become
 * --------------------------------------------------------------------------
 *
 *   * It must not grow logic.  It performs no validation, no bounds
 *     arithmetic and no error mapping; every one of those lives in
 *     `crates/libz-rs-sys/src/inflate.rs` and, below it, in
 *     `zlib_rs::inflate::inftrees`.  The single statement below is the whole
 *     translation unit's behaviour on purpose.
 *   * It must not interpret `code`.  The struct is opaque here: only the Rust
 *     side knows how it copies entries out, and `zlib_rs`'s `Code` is
 *     deliberately not `#[repr(C)]`.  The pointer is passed through unread.
 *   * It must not be given a `zlib.map` entry.  That file is immutable
 *     (AAP §0.8.1 directive 2), and it already hides both names involved:
 *     `inflate_table` by name and `_zlib_rs_inflate_table` through its `_*`
 *     pattern.
 */

/* `zutil.h` for `ZLIB_INTERNAL`, and `inftrees.h` for `codetype`, `code` and
 * the prototype itself.  Including them rather than restating anything is the
 * whole point of this file: if the definition below ever disagreed with the
 * declaration `test/infcover.c` compiles against, this translation unit would
 * not build.  `inftrees.h` needs `zutil.h`'s `FAR` and `ZLIB_INTERNAL` first;
 * it has no include guard of its own for them, exactly as `inftrees.c` L7-L8
 * arranges.
 *
 * The include path is the repository root, which the build script passes as
 * `-I` and the C build already passes as `$(ZINCOUT)`.
 */
#include "zutil.h"
#include "inftrees.h"

/* The Rust side of the boundary.
 *
 * Implemented in `crates/libz-rs-sys/src/inflate.rs` over
 * `zlib_rs::inflate::inftrees::inflate_table_build`.  The leading underscore is
 * load-bearing: `zlib.map`'s `local:` block ends with the pattern `_*`, which is
 * how zlib itself keeps `_tr_init` and its five siblings out of the dynamic
 * table, so the name follows the reference implementation's own convention.
 *
 * `type` arrives as a plain `int` and is validated there: a value outside
 * `0..=2` answers -1, which already means "this code set cannot be built".
 * Everything else is passed through untouched, including the null-pointer cases,
 * which the Rust side diagnoses rather than dereferences.
 *
 * Returns C's tri-state unchanged: 0 on success, -1 for an over-subscribed or
 * incomplete code set, +1 when the table would need more than `ENOUGH_LENS` or
 * `ENOUGH_DISTS` entries.
 */
extern int _zlib_rs_inflate_table(int type,
                                  unsigned short FAR *lens,
                                  unsigned codes,
                                  code FAR * FAR *table,
                                  unsigned FAR *bits,
                                  unsigned short FAR *work);

/* ------------------------------------------------------------------------ */
/*  inflate_table -- inftrees.h L60-L62, inftrees.c L46-L311               */
/* ------------------------------------------------------------------------ */

int ZLIB_INTERNAL inflate_table(codetype type,
                               unsigned short FAR *lens,
                               unsigned codes,
                               code FAR * FAR *table,
                               unsigned FAR *bits,
                               unsigned short FAR *work) {
    /* The cast is the entire content of this function.  It is a widening of a
     * value the C standard guarantees is representable as an `int` -- the
     * enumerators are 0, 1 and 2 -- so nothing can be lost, and it is the
     * conversion the compiler would perform implicitly anyway.  Writing it out
     * says that the narrowing of the TYPE, not of the value, is deliberate.
     */
    return _zlib_rs_inflate_table((int)type, lens, codes, table, bits, work);
}
