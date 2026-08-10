# Assert that the installed zlib.pc advertises the Rust static library's private link line.
#
# Run by the zlib_static_closure_pc_advertises_private_libs case of
# test/static_link_closure_test.cmake.in, as `cmake -DZLIB_PC_FILE=<path> -P <this file>`.
# A script rather than a chain of `cmake -E' invocations because the assertion is a pattern
# match with a diagnostic, and because this file is reviewable in a way a generated one-liner
# would not be.
#
# WHY THE FILE IS READ RATHER THAN pkg-config BEING ASKED.  zlib has never required
# pkg-config to build or to test, and adding a tool dependency for one assertion would be the
# wrong trade.  Reading the descriptor checks the same fact -- the key is present and
# non-empty -- and cannot be defeated by a pkg-config that silently reports nothing when a
# variable it wanted is missing.
#
# The C build does not register the case that runs this, because the C libz.a needs no
# companion libraries and a private link line there would be wrong.  See
# test/static_link_closure_test.cmake.in for the whole argument.

if(NOT DEFINED ZLIB_PC_FILE)
    message(FATAL_ERROR "ZLIB_PC_FILE was not passed; run this script with "
                        "-DZLIB_PC_FILE=<path to the installed zlib.pc>")
endif(NOT DEFINED ZLIB_PC_FILE)

if(NOT EXISTS "${ZLIB_PC_FILE}")
    message(
        FATAL_ERROR
            "the installed pkg-config descriptor is missing: ${ZLIB_PC_FILE}.  ZLIB_INSTALL "
            "publishes it into <libdir>/pkgconfig, so either the install did not run or the "
            "layout has changed.")
endif(NOT EXISTS "${ZLIB_PC_FILE}")

file(READ "${ZLIB_PC_FILE}" _zlib_pc)

# The keys the descriptor must keep advertising whatever implementation is installed.  A
# Libs.private: that arrived at the cost of one of these would be a regression, not a fix.
if(NOT _zlib_pc MATCHES "\n[ \t]*Libs:[^\n]*-lz")
    message(FATAL_ERROR "the installed ${ZLIB_PC_FILE} no longer advertises `Libs: ... -lz'. "
                        "That is part of the drop-in contract and must not change.")
endif(NOT _zlib_pc MATCHES "\n[ \t]*Libs:[^\n]*-lz")

# The private link line, and it has to carry something: an empty value would satisfy a
# presence test while telling a consumer nothing.
if(NOT _zlib_pc MATCHES "\n[ \t]*Libs\\.private:[ \t]*([^\n]*[^ \t\n][^\n]*)")
    message(
        FATAL_ERROR
            "the installed ${ZLIB_PC_FILE} carries no non-empty `Libs.private:' line, so a "
            "consumer that static-links the Rust libz.a is not told which native runtime "
            "libraries it needs.  CMakeLists.txt derives them with `rustc --crate-type "
            "staticlib --print native-static-libs' and substitutes the line into "
            "zlib.pc.cmakein; that did not happen.\n"
            "The descriptor as installed:\n${_zlib_pc}")
endif(NOT _zlib_pc MATCHES "\n[ \t]*Libs\\.private:[ \t]*([^\n]*[^ \t\n][^\n]*)")

message(STATUS "${ZLIB_PC_FILE} advertises Libs.private:${CMAKE_MATCH_1}")
