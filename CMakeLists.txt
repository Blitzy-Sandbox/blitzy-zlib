cmake_minimum_required(VERSION 3.12...3.31)

project(
    zlib
    LANGUAGES C
    VERSION 1.3.2.1
    HOMEPAGE_URL "https://zlib.net/"
    DESCRIPTION "a general-purpose lossless data-compression library")

# ============================================================================
# CPack
# ============================================================================
set(CPACK_PACKAGE_VENDOR "zlib-Project")
set(CPACK_PACKAGE_DESCRIPTION_FILE ${zlib_SOURCE_DIR}/README)
set(CPACK_RESOURCE_FILE_LICENSE ${zlib_SOURCE_DIR}/LICENSE)
set(CPACK_RESOURCE_FILE_README ${zlib_SOURCE_DIR}/README)

# ============================================================================
# configuration
# ============================================================================

option(ZLIB_BUILD_TESTING "Enable Zlib Examples as tests" ON)
option(ZLIB_BUILD_SHARED "Enable building zlib shared library" ON)
option(ZLIB_BUILD_STATIC "Enable building zlib static library" ON)
option(ZLIB_BUILD_RUST
       "Use the memory-safe Rust libz for the exported ZLIB::* targets" OFF)
option(ZLIB_INSTALL "Enable installation of zlib" ON)
option(ZLIB_PREFIX "prefix for all types and library functions, see zconf.h.in"
       OFF)
mark_as_advanced(ZLIB_PREFIX)

# Whether the generator produces one build tree per configuration.  Queried
# here, ahead of everything that reads it, because the ZLIB_BUILD_RUST block
# below is the first consumer and the default-CMAKE_BUILD_TYPE rule further down
# is the second.
get_property(IS_MULTI GLOBAL PROPERTY GENERATOR_IS_MULTI_CONFIG)

# ZLIB_BUILD_RUST builds the memory-safe Rust implementation
# (crates/libz-rs-sys) instead of the C library for the exported ZLIB::*
# targets. It is entirely opt-in: with the option OFF -- the default -- nothing
# below runs, no tool is probed, and this project configures and builds exactly
# as it always has.  Everything the Rust path needs is therefore discovered
# here, inside the option, so that the C-only build never acquires a new
# requirement.
if(ZLIB_BUILD_RUST)
    # cargo drives the whole Rust build.  Which Rust version is required is
    # declared by rust-toolchain.toml and by rust-version in the crate
    # manifests, and is enforced by cargo itself -- exactly as configure L92-L94
    # documents for the autotools build -- so only the presence of cargo is
    # checked here.
    find_program(ZLIB_CARGO_EXECUTABLE NAMES cargo)
    mark_as_advanced(ZLIB_CARGO_EXECUTABLE)

    if(NOT ZLIB_CARGO_EXECUTABLE)
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST is ON but cargo was not found on PATH.\n"
                "Install a Rust toolchain (https://rustup.rs) and reconfigure, "
                "or point CMake at a particular cargo:\n"
                "    cmake -S . -B build -DZLIB_BUILD_RUST=ON "
                "-DZLIB_CARGO_EXECUTABLE=/opt/rust/bin/cargo\n"
                "Configuring with -DZLIB_BUILD_RUST=OFF (the default) builds "
                "the C library and needs no cargo.")
    endif(NOT ZLIB_CARGO_EXECUTABLE)

    # A multi-config generator is refused, and this is the one refusal that
    # looks like a limitation and is actually a correctness requirement.  Cargo
    # has two profiles where CMake has four configurations, and three of the
    # things this block produces cannot be made per-configuration at this
    # project's 3.12 floor: add_custom_command's OUTPUT, BYPRODUCTS and DEPENDS
    # accept generator expressions only from CMake 3.20, and IMPORTED_LOCATION
    # accepts none at any version.  So under Xcode or Visual Studio a Debug
    # build and a Release build would write cargo's output into one directory,
    # stage one drop-in, and point one IMPORTED_LOCATION at it -- whichever
    # configuration was built last wins, silently, and a consumer asking for
    # Debug gets an optimised library.  The configure-time strip decision has
    # the same shape: CMAKE_BUILD_TYPE is empty under a multi-config generator,
    # so every configuration including Debug would be stripped as though it were
    # optimised.  Refusing at configure time says so once, where the reason can
    # be given, instead of producing four configurations that are quietly the
    # same one.  Single-config generators -- Unix Makefiles and Ninja, which is
    # what every configuration in .github/workflows/ uses on this platform --
    # are unaffected, and the C build is untouched in either case.
    if(IS_MULTI)
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST does not support the multi-configuration "
                "generator \"${CMAKE_GENERATOR}\".  Cargo has one output "
                "directory per profile where this generator has one per "
                "configuration, and at this project's CMake 3.12 floor neither "
                "the staging rules nor IMPORTED_LOCATION can be made "
                "per-configuration, so Debug and Release would share one staged "
                "library and one imported location.\n"
                "Configure with a single-configuration generator -- for "
                "example -G \"Unix Makefiles\" or -G Ninja, with "
                "-DCMAKE_BUILD_TYPE=Debug or Release -- or with "
                "-DZLIB_BUILD_RUST=OFF to build the C library.")
    endif(IS_MULTI)

    # Cross-compilation is refused for the same reason: honestly, and at
    # configure time.  Nothing below passes cargo a --target, so cargo would
    # build for the HOST while CMake believed it had a target-architecture
    # library; the artifact path this file reads has no target-triple component
    # to look under either, and the two raw link commands drive
    # ${CMAKE_C_COMPILER} without the sysroot, the toolchain-file arguments or
    # the target flags a cross toolchain requires.  Producing a host library and
    # installing it as though it were a target one is the worst available
    # outcome -- it links, it installs, and it fails at run time on the device
    # -- so the combination is rejected rather than approximated.  Supporting it
    # means a CMake-system-to-Rust-target mapping plus target-specific artifact
    # paths and tools, which is a larger change than this option's contract.
    if(CMAKE_CROSSCOMPILING)
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST does not support cross-compilation.  "
                "CMAKE_CROSSCOMPILING is set (CMAKE_SYSTEM_NAME "
                "${CMAKE_SYSTEM_NAME}, CMAKE_SYSTEM_PROCESSOR "
                "${CMAKE_SYSTEM_PROCESSOR}), but cargo is invoked here without "
                "--target, so it would build for the host and this file would "
                "stage and install that host library under the target's name.\n"
                "Build the Rust library natively on the target, or configure "
                "with -DZLIB_BUILD_RUST=OFF to cross-compile the C library, "
                "which supports it fully.")
    endif(CMAKE_CROSSCOMPILING)

    # The SHARED library is relinked from cargo's static archive through
    # zlib.map, which needs an ELF linker that understands -soname and
    # --version-script.  That is precisely the platform set on which the C
    # shared target applies the same version script below, so the two agree by
    # construction.  Mach-O and MSVC spell every one of those concepts
    # differently -- an install name rather than a soname, an exported-symbol
    # list or a .def file rather than a version script -- and reproducing the
    # published surface through them is out of scope for this port; refusing
    # here is honest and actionable, whereas emitting a half-formed dylib or
    # import library would look like support that does not exist.
    #
    # ★ The refusal is scoped to shared linking, and that scope is the point.
    # The STATIC library needs none of it: cargo emits a complete archive on
    # every Tier-1 target, this file copies it under the platform's own archive
    # name (${CMAKE_STATIC_LIBRARY_PREFIX}z${CMAKE_STATIC_LIBRARY_SUFFIX}, which
    # is libz.a on Linux and macOS and z.lib under MSVC -- exactly what cargo
    # writes for the same target), and nothing in that path touches a soname or
    # a version script.  So -DZLIB_BUILD_RUST=ON -DZLIB_BUILD_SHARED=OFF is
    # supported wherever cargo runs, and .github/workflows/rust.yml's macOS and
    # Windows rows -- which exist because uLong is 8 bytes on LP64 and 4 on
    # LLP64 -- are not contradicted by a configure-time refusal on the very
    # platforms they cover.
    #
    # That claim reaches one more thing besides the copy, and it is the reason the
    # native-static-libraries probe below writes its throwaway archive into the
    # CMake binary directory: static-only mode also asks rustc what a Rust archive
    # must be linked against, and an output path of /dev/null -- which an earlier
    # revision passed -- does not exist on Windows, so the claim above would have
    # failed at exactly the place it promises support.  A packager who cannot run
    # rustc for the target at all supplies the answer with
    # -DZLIB_RUST_NATIVE_STATIC_LIBS="...", which is what a cross build does.
    if(ZLIB_BUILD_SHARED
       AND (NOT UNIX
            OR APPLE
            OR (CMAKE_SYSTEM_NAME STREQUAL AIX)
            OR (CMAKE_SYSTEM_NAME STREQUAL SunOS)
           ))
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST can only build a SHARED library on ELF "
                "platforms whose linker accepts -soname and --version-script, "
                "which is the same platform set that receives zlib.map in the C "
                "shared library below.  CMAKE_SYSTEM_NAME is "
                "${CMAKE_SYSTEM_NAME}.\n"
                "Configure with -DZLIB_BUILD_SHARED=OFF to stage the Rust "
                "static library, which is supported here, or with "
                "-DZLIB_BUILD_RUST=OFF to build the C library.")
    endif(
        ZLIB_BUILD_SHARED
        AND (NOT UNIX
             OR APPLE
             OR (CMAKE_SYSTEM_NAME STREQUAL AIX)
             OR (CMAKE_SYSTEM_NAME STREQUAL SunOS)
            ))

    # ZLIB_PREFIX rewrites every public name to z_* -- zconf.h.in L11-L21 does
    # it with the preprocessor for a caller, and the C library gets the prefixed
    # names because it is compiled through that same header.  The staged Rust
    # library cannot: its exported names come from the #[no_mangle] surface of
    # crates/libz-rs-sys and from the ZEXTERN declarations in the unmodified
    # zlib.h, so it publishes the unprefixed family and could never satisfy a
    # consumer compiled against a prefixed header.  Left to itself the
    # combination reaches the linker and fails there with undefined references
    # to z_zlibVersion and its neighbours, which says nothing about the cause,
    # so it is refused here where the reason can be given.  Note that
    # ZLIB_PREFIX does not currently build in the C configuration either --
    # minigzip.c fails the same way -- but that is upstream behaviour this port
    # neither introduced nor takes responsibility for, and it is why the refusal
    # is scoped to the combination with ZLIB_BUILD_RUST rather than to
    # ZLIB_PREFIX itself.
    if(ZLIB_PREFIX)
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST and ZLIB_PREFIX cannot both be ON.  "
                "ZLIB_PREFIX renames the whole public surface to z_* through "
                "zconf.h, and the staged Rust library exports the unprefixed "
                "names that the unmodified zlib.h declares, so no consumer "
                "compiled with the prefixed header could link against it.\n"
                "Configure with -DZLIB_PREFIX=OFF (the default) to use the "
                "Rust library, or with -DZLIB_BUILD_RUST=OFF to build the "
                "prefixed C library.")
    endif(ZLIB_PREFIX)

    # objcopy performs the one step of the relink below that no linker flag can
    # express, and it is only needed when a shared library is being staged.  It
    # comes from the same binutils CMake already located for this toolchain.
    if(ZLIB_BUILD_SHARED AND NOT CMAKE_OBJCOPY)
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST needs objcopy to stage the shared library "
                "and CMake found none for this toolchain.  Install binutils, "
                "pass -DCMAKE_OBJCOPY=/path/to/objcopy, or configure with "
                "-DZLIB_BUILD_SHARED=OFF to stage only the static library.")
    endif(ZLIB_BUILD_SHARED AND NOT CMAKE_OBJCOPY)

    # ZLIB_RS_SIMD=0|1 is the build-time toggle the port documents for the
    # alternative checksum backends -- portable integer code shaped so that LLVM
    # can autovectorise it, with no architecture intrinsic and no run-time CPU
    # probe deciding correctness, and output neutral either way (a checksum is
    # one scalar however it is computed).  crates/libz-rs-sys/build.rs declares
    # cargo::rerun-if-env-changed=ZLIB_RS_SIMD and VALIDATES the variable
    # against the resolved `simd` Cargo feature, refusing a request it cannot
    # satisfy -- a build script cannot switch a feature on.  Translating the
    # variable into the feature therefore belongs to whoever invokes cargo,
    # which here is this file (Makefile.in's `rust' target does the same).  The
    # value is accepted from -DZLIB_RS_SIMD=... or from the environment, so both
    # the CMake-native and the documented spellings work.
    if(NOT DEFINED ZLIB_RS_SIMD AND DEFINED ENV{ZLIB_RS_SIMD})
        set(ZLIB_RS_SIMD $ENV{ZLIB_RS_SIMD})
    endif(NOT DEFINED ZLIB_RS_SIMD AND DEFINED ENV{ZLIB_RS_SIMD})

    if(DEFINED ZLIB_RS_SIMD AND NOT ZLIB_RS_SIMD STREQUAL "")
        if(NOT ZLIB_RS_SIMD MATCHES "^[01]$")
            message(
                FATAL_ERROR
                    "ZLIB_RS_SIMD must be 0 or 1, got \"${ZLIB_RS_SIMD}\".")
        endif(NOT ZLIB_RS_SIMD MATCHES "^[01]$")
    endif(DEFINED ZLIB_RS_SIMD AND NOT ZLIB_RS_SIMD STREQUAL "")

    # ------------------------------------------------------------------------
    # The native libraries a Rust staticlib needs beside it
    # ------------------------------------------------------------------------
    #
    # A C libz.a needs no companion libraries.  A Rust staticlib does: it
    # carries the Rust standard library's own object code, whose undefined
    # references reach into libc and its neighbours, and nothing in the archive
    # records that.  A consumer that links the staged libz.a and nothing else
    # therefore fails at link time -- not always, which is what makes it
    # insidious.  A plain `cc' link happens to succeed on a glibc that folds
    # libpthread, libdl, librt and libutil into libc, and fails on one that
    # ships them separately, under -nodefaultlibs, when ld is driven directly,
    # or from a toolchain belonging to another language.
    #
    # The set is TARGET-SPECIFIC and must be measured, never copied: rustc is
    # the only thing that knows it.  `--print native-static-libs' is rustc's
    # answer to exactly this question, and it needs a real staticlib link to
    # produce one, so the probe compiles a two-line crate rather than asking an
    # abstract question.  The result is cached, so the probe runs once per build
    # tree and a packager can override it for a cross build with
    # -DZLIB_RUST_NATIVE_STATIC_LIBS="-lfoo -lbar".
    #
    # A probe that cannot answer is FATAL rather than empty.  Emitting an empty
    # answer would publish a static library whose link requirements are silently
    # understated, which is the failure this block exists to remove; and the
    # probe cannot fail on a host that has already been established to have a
    # working cargo and an ELF linker, so a failure here is a real toolchain
    # problem worth stopping for.  ZLIB_BUILD_STATIC=OFF stages no archive, so
    # the probe is scoped to the configuration that publishes one.
    if(ZLIB_BUILD_STATIC AND NOT DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)
        # rustc lives beside cargo in every rustup and distribution layout, so
        # the located cargo is the hint that keeps the two from coming from
        # different toolchains.
        get_filename_component(ZLIB_RUST_TOOLCHAIN_BIN ${ZLIB_CARGO_EXECUTABLE}
                               DIRECTORY)
        find_program(
            ZLIB_RUSTC_EXECUTABLE
            NAMES rustc
            HINTS ${ZLIB_RUST_TOOLCHAIN_BIN})
        mark_as_advanced(ZLIB_RUSTC_EXECUTABLE)

        if(NOT ZLIB_RUSTC_EXECUTABLE)
            message(
                FATAL_ERROR
                    "ZLIB_BUILD_RUST with ZLIB_BUILD_STATIC needs rustc to "
                    "determine the native libraries a Rust static library must "
                    "be linked against, and none was found beside "
                    "${ZLIB_CARGO_EXECUTABLE} or on PATH.\n"
                    "Point CMake at one with -DZLIB_RUSTC_EXECUTABLE=..., "
                    "state the answer directly with "
                    "-DZLIB_RUST_NATIVE_STATIC_LIBS=\"-lm -ldl -lc\" (as a "
                    "cross build must), or configure with "
                    "-DZLIB_BUILD_STATIC=OFF to stage only the shared library, "
                    "which records its dependencies in DT_NEEDED and needs "
                    "none of this.")
        endif(NOT ZLIB_RUSTC_EXECUTABLE)

        # In the CMake BINARY DIRECTORY, never in the source tree and never
        # /dev/null.  The output path has to be somewhere rustc can really write
        # an archive: an earlier revision passed `-o /dev/null', which works on
        # a Unix host and fails on Windows, where there is no such file -- and
        # static-only Rust mode is offered on every target cargo runs on, so
        # this path is reached there.  It is removed again below, so nothing but
        # the cached answer survives the configure.
        set(ZLIB_RUST_PROBE_DIR
            ${zlib_BINARY_DIR}/CMakeFiles/zlib-native-static-libs)
        file(MAKE_DIRECTORY ${ZLIB_RUST_PROBE_DIR})
        # `#[no_mangle] extern "C"' so the crate has an exported symbol and the
        # archive is a realistic one; the body is irrelevant, only the link is.
        file(
            WRITE ${ZLIB_RUST_PROBE_DIR}/probe.rs
            "#[no_mangle]\npub extern \"C\" fn zlib_rs_native_static_libs_probe() -> i32 { 0 }\n"
        )

        execute_process(
            COMMAND
                ${ZLIB_RUSTC_EXECUTABLE} --print native-static-libs --crate-type
                staticlib -o ${ZLIB_RUST_PROBE_DIR}/libzlib_rs_probe.a
                ${ZLIB_RUST_PROBE_DIR}/probe.rs
            WORKING_DIRECTORY ${ZLIB_RUST_PROBE_DIR}
            RESULT_VARIABLE ZLIB_RUST_PROBE_RESULT
            OUTPUT_VARIABLE ZLIB_RUST_PROBE_STDOUT
            ERROR_VARIABLE ZLIB_RUST_PROBE_STDERR)

        if(NOT ZLIB_RUST_PROBE_RESULT EQUAL 0)
            message(
                FATAL_ERROR
                    "${ZLIB_RUSTC_EXECUTABLE} --print native-static-libs "
                    "failed with ${ZLIB_RUST_PROBE_RESULT}, so the native "
                    "libraries the staged libz.a must be linked against are "
                    "unknown:\n${ZLIB_RUST_PROBE_STDERR}")
        endif(NOT ZLIB_RUST_PROBE_RESULT EQUAL 0)

        # rustc reports the answer as a `note:' on stderr.  Match the line and
        # keep what follows the marker; the flags are already in the order rustc
        # says is significant, so they are not sorted or de-duplicated.
        if(NOT ZLIB_RUST_PROBE_STDERR MATCHES
           "native-static-libs:[ \t]*([^\r\n]*)")
            message(
                FATAL_ERROR
                    "${ZLIB_RUSTC_EXECUTABLE} --print native-static-libs "
                    "printed no native-static-libs line, so the link "
                    "requirements of the staged libz.a cannot be published. "
                    "Its output was:\n${ZLIB_RUST_PROBE_STDOUT}"
                    "${ZLIB_RUST_PROBE_STDERR}")
        endif(NOT ZLIB_RUST_PROBE_STDERR MATCHES
              "native-static-libs:[ \t]*([^\r\n]*)")

        string(STRIP "${CMAKE_MATCH_1}" ZLIB_RUST_PROBE_LIBS)

        if(ZLIB_RUST_PROBE_LIBS STREQUAL "")
            message(
                FATAL_ERROR
                    "${ZLIB_RUSTC_EXECUTABLE} reported an empty "
                    "native-static-libs set.  That is not a usable answer for "
                    "a Rust staticlib on an ELF target; state the set "
                    "explicitly with -DZLIB_RUST_NATIVE_STATIC_LIBS=... if this "
                    "toolchain genuinely needs none.")
        endif(ZLIB_RUST_PROBE_LIBS STREQUAL "")

        set(ZLIB_RUST_NATIVE_STATIC_LIBS
            "${ZLIB_RUST_PROBE_LIBS}"
            CACHE
                STRING
                "Native libraries a Rust static library must be linked against, from rustc --print native-static-libs"
        )
        mark_as_advanced(ZLIB_RUST_NATIVE_STATIC_LIBS)
        message(
            STATUS "Rust static library needs: ${ZLIB_RUST_NATIVE_STATIC_LIBS}")

        # The answer is cached; the crate and the archive it was linked from are
        # not wanted again, and a stale probe archive beside the real one is only
        # something for a reader to wonder about later.
        file(REMOVE ${ZLIB_RUST_PROBE_DIR}/probe.rs
             ${ZLIB_RUST_PROBE_DIR}/libzlib_rs_probe.a)
    endif(ZLIB_BUILD_STATIC AND NOT DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)

    # CMake wants a list where rustc printed a single space-separated string.
    # Every element begins with `-', so CMake passes each through to the link
    # line verbatim rather than treating it as a target or a file path.
    if(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)
        string(REPLACE " " ";" ZLIB_RUST_NATIVE_STATIC_LIBS_LIST
                       "${ZLIB_RUST_NATIVE_STATIC_LIBS}")
        list(REMOVE_ITEM ZLIB_RUST_NATIVE_STATIC_LIBS_LIST "")
    endif(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)
endif(ZLIB_BUILD_RUST)

if(NOT DEFINED CMAKE_BUILD_TYPE AND NOT IS_MULTI)
    message(STATUS "No CMAKE_BUILD_TYPE set -- using Release")
    set(CMAKE_BUILD_TYPE Release)
endif(NOT DEFINED CMAKE_BUILD_TYPE AND NOT IS_MULTI)

include(CheckCSourceCompiles)
include(CheckFunctionExists)
include(CheckIncludeFile)
include(CMakePackageConfigHelpers)
include(CheckTypeSize)
include(CPack)
include(GNUInstallDirs)

set(CPACK_INCLUDED TRUE)

if(NOT ZLIB_CONF_WRITTEN)
    set(Z_PREFIX ${ZLIB_PREFIX})
    set(CONF_OUT_FILE ${zlib_BINARY_DIR}/zconf.h.cmakein)
    file(READ ${zlib_SOURCE_DIR}/zconf.h ZCONF_CONTENT LIMIT 245)
    file(WRITE ${CONF_OUT_FILE} ${ZCONF_CONTENT})
    file(APPEND ${CONF_OUT_FILE} "#cmakedefine Z_PREFIX 1\n")
    file(APPEND ${CONF_OUT_FILE} "#cmakedefine HAVE_STDARG_H 1\n")
    file(APPEND ${CONF_OUT_FILE} "#cmakedefine HAVE_UNISTD_H 1\n")
    file(READ ${zlib_SOURCE_DIR}/zconf.h ZCONF_CONTENT OFFSET 244)
    set(FIRST_ITEM TRUE)

    foreach(item IN LISTS ZCONF_CONTENT)
        if(FIRST_ITEM)
            string(APPEND OUT_CONTENT ${item})
            set(FIRST_ITEM FALSE)
        else(FIRST_ITEM)
            string(APPEND OUT_CONTENT "\;" ${item})
        endif(FIRST_ITEM)
    endforeach(item IN LISTS ${ZCONF_CONTENT})

    file(APPEND ${CONF_OUT_FILE} ${OUT_CONTENT})
    set(ZLIB_CONF_WRITTEN
        TRUE
        CACHE BOOL "zconf.h.cmakein was created")
    mark_as_advanced(ZLIB_CONF_WRITTEN)
endif(NOT ZLIB_CONF_WRITTEN)

#
# Check to see if we have large file support
#
set(CMAKE_REQUIRED_DEFINITIONS -D_LARGEFILE64_SOURCE=1)
check_type_size(off64_t OFF64_T)
unset(CMAKE_REQUIRED_DEFINITIONS) # clear variable

#
# Check for fseeko
#
check_function_exists(fseeko HAVE_FSEEKO)

#
# Check for stdarg.h
#
check_include_file(stdarg.h HAVE_STDARG_H)

#
# Check for unistd.h
#
check_include_file(unistd.h HAVE_UNISTD_H)

#
# Check visibility attribute is supported
#
if(MSVC)
    set(CMAKE_REQUIRED_FLAGS "-WX")
else(MSVC)
    set(CMAKE_REQUIRED_FLAGS "-Werror")
endif(MSVC)

check_c_source_compiles(
    "
        #include <stdlib.h>
        static void f(void) __attribute__ ((visibility(\"hidden\")));
        int main(void) {return 0;}
    "
    HAVE___ATTR__VIS_HIDDEN)

unset(CMAKE_COMPILE_FLAGS)

# The one implementation-specific key in zlib.pc, and the reason it is assembled
# here rather than written in the template.
#
# `Libs.private:' lists what a *static* consumer needs beyond the library
# itself.  A C libz.a needs nothing, and a .pc that claimed otherwise would be
# wrong for the C build; a Rust libz.a needs the set rustc printed above.  One
# template serves both implementations, so the difference has to be a
# substitution rather than a comment saying the difference exists.
#
# The newline is part of the value on purpose.  Making the whole line
# substitutable -- rather than leaving `Libs.private:' in the template with a
# possibly-empty value after it -- keeps the C-mode output byte-for-byte what it
# has always been: no such key at all, not an empty one.  zlib.pc.in, which the
# autotools build uses, has no equivalent because `configure' does not know
# whether `make rust' will be run afterwards; `make rust' prints the set
# instead.
if(ZLIB_BUILD_RUST AND DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)
    set(ZLIB_PC_LIBS_PRIVATE "\nLibs.private: ${ZLIB_RUST_NATIVE_STATIC_LIBS}")
else(ZLIB_BUILD_RUST AND DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)
    set(ZLIB_PC_LIBS_PRIVATE "")
endif(ZLIB_BUILD_RUST AND DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS)

set(ZLIB_PC ${zlib_BINARY_DIR}/zlib.pc)
configure_file(${zlib_SOURCE_DIR}/zlib.pc.cmakein ${ZLIB_PC} @ONLY)
configure_file(${zlib_BINARY_DIR}/zconf.h.cmakein ${zlib_BINARY_DIR}/zconf.h)

# ============================================================================
# zlib
# ============================================================================

set(ZLIB_PUBLIC_HDRS ${zlib_BINARY_DIR}/zconf.h zlib.h)

set(ZLIB_PRIVATE_HDRS
    crc32.h
    deflate.h
    gzguts.h
    inffast.h
    inffixed.h
    inflate.h
    inftrees.h
    trees.h
    zutil.h)

set(ZLIB_SRCS
    adler32.c
    compress.c
    crc32.c
    deflate.c
    gzclose.c
    gzlib.c
    gzread.c
    gzwrite.c
    inflate.c
    infback.c
    inftrees.c
    inffast.c
    trees.c
    uncompr.c
    zutil.c)

if(WIN32)
    set(zlib_static_suffix "s")
    set(CMAKE_DEBUG_POSTFIX "d")
endif(WIN32)

if(ZLIB_BUILD_SHARED)
    add_library(
        zlib SHARED ${ZLIB_SRCS} ${ZLIB_PUBLIC_HDRS} ${ZLIB_PRIVATE_HDRS}
                    $<$<OR:$<BOOL:${WIN32}>,$<BOOL:${CYGWIN}>>:win32/zlib1.rc>)
    target_include_directories(
        zlib
        PUBLIC $<BUILD_INTERFACE:${zlib_BINARY_DIR}>
               $<BUILD_INTERFACE:${zlib_SOURCE_DIR}>
               $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
    target_compile_definitions(
        zlib
        PRIVATE ZLIB_BUILD
                $<$<BOOL:NOT:${HAVE_FSEEKO}>:NO_FSEEKO>
                $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>
                $<$<BOOL:${MSVC}>:_CRT_SECURE_NO_DEPRECATE>
                $<$<BOOL:${MSVC}>:_CRT_NONSTDC_NO_DEPRECATE>
        PUBLIC $<$<BOOL:${HAVE_OFF64_T}>:_LARGEFILE64_SOURCE=1>)
    set(INSTALL_VERSION ${zlib_VERSION})

    if(NOT CYGWIN)
        set_target_properties(zlib PROPERTIES SOVERSION ${zlib_VERSION_MAJOR}
                                              VERSION ${INSTALL_VERSION})
    endif(NOT CYGWIN)

    set_target_properties(
        zlib
        PROPERTIES DEFINE_SYMBOL ZLIB_DLL
                   EXPORT_NAME ZLIB
                   OUTPUT_NAME z)
    if(UNIX
        AND NOT APPLE
        AND NOT (CMAKE_SYSTEM_NAME STREQUAL AIX)
        AND NOT (CMAKE_SYSTEM_NAME STREQUAL SunOS))
        # On unix-like platforms the library is almost always called libz
        set_target_properties(
            zlib
            PROPERTIES LINK_FLAGS
                       "-Wl,--version-script,\"${zlib_SOURCE_DIR}/zlib.map\"")
    endif(
        UNIX
        AND NOT APPLE
        AND NOT (CMAKE_SYSTEM_NAME STREQUAL AIX)
        AND NOT (CMAKE_SYSTEM_NAME STREQUAL SunOS))
endif(ZLIB_BUILD_SHARED)

if(ZLIB_BUILD_STATIC)
    add_library(zlibstatic STATIC ${ZLIB_SRCS} ${ZLIB_PUBLIC_HDRS}
                                  ${ZLIB_PRIVATE_HDRS})
    target_include_directories(
        zlibstatic
        PUBLIC $<BUILD_INTERFACE:${zlib_BINARY_DIR}>
               $<BUILD_INTERFACE:${zlib_SOURCE_DIR}>
               $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
    target_compile_definitions(
        zlibstatic
        PRIVATE ZLIB_BUILD
                $<$<BOOL:NOT:${HAVE_FSEEKO}>:NO_FSEEKO>
                $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>
                $<$<BOOL:${MSVC}>:_CRT_SECURE_NO_DEPRECATE>
                $<$<BOOL:${MSVC}>:_CRT_NONSTDC_NO_DEPRECATE>
        PUBLIC $<$<BOOL:${HAVE_OFF64_T}>:_LARGEFILE64_SOURCE=1>)
    set_target_properties(
        zlibstatic PROPERTIES EXPORT_NAME ZLIBSTATIC OUTPUT_NAME
                                                     z${zlib_static_suffix})
endif(ZLIB_BUILD_STATIC)

# ============================================================================
# zlib-rs -- the memory-safe Rust implementation
# ============================================================================
#
# ZLIB_BUILD_RUST redirects the exported ZLIB::ZLIB and ZLIB::ZLIBSTATIC targets
# at libraries built from crates/libz-rs-sys instead of the C sources.  Whichever
# C targets the ZLIB_BUILD_SHARED and ZLIB_BUILD_STATIC options enable above --
# `zlib' and `zlibstatic' -- are defined and built exactly as before, unaffected
# by this option: they are the differential oracle the port is verified against,
# and contrib/ consumes them by name.  This option adds a library rather than
# replacing one; turning a C target off is still the job of its own option.
# Nothing here touches zlib.h, zconf.h or zlib.map -- the public contract is read
# and, in zlib.map's case, handed to the linker, never edited -- so the exported
# API and ABI are unchanged and a consumer that dynamically links keeps working
# without recompilation.  Static consumption through the installed zlib.pc is the
# one case that is not equivalent; zlib.pc.cmakein states the measurement.
#
# The redirection is a SWITCH, not a fork.  Both C targets exist in either
# configuration, but only one implementation is behind the two exported aliases,
# so everything downstream of them -- test/, the find_package fixtures, an
# add_subdirectory consumer -- exercises whichever one the option selected and
# not both.  test/CMakeLists.txt registers no `_rust'-suffixed case beside its C
# counterpart, so a single build tree does not compare the two; that comparison
# lives in crates/zlib-rs-differential, which holds both in one process.  No CI
# job configures with this option on either -- .github/workflows/cmake.yml
# carries no Rust job and names ZLIB_BUILD_RUST nowhere -- so this path is
# exercised by hand.
#
# Why cargo's own libz.so is not the drop-in.  rustc attaches its own anonymous
# version script to every cdylib link, so zlib.map cannot be layered on top of
# it; the result carries none of the version nodes zlib.map declares, gives no
# dynamic entry to the two variadic exports that crates/libz-rs-sys/build.rs
# compiles from C, and leaves the crate's internal helpers visible.
# crates/libz-rs-sys/build.rs documents this and deliberately stages no
# versioned names of its own -- it even prunes any it finds, because a build
# script runs before the link and would leave them dangling.  The installable
# library is produced by one further step, which this block is the CMake
# implementation of and which Makefile.in's `rust' target mirrors for the
# autotools build: relink cargo's complete static archive through zlib.map with
# the right SONAME, drop the symbols that are not part of the published surface,
# and stage the versioned chain in a directory of its own.
if(ZLIB_BUILD_RUST)
    # Two things below are read out of zlib.h at configure time -- the versioned
    # library name and the set of symbols the staged library may export -- so
    # the header is a genuine configure input and is declared as one.  Without
    # this an edit to it would leave both derived values stale until someone
    # reconfigured by hand, and a stale export set is not a visible failure.
    set_property(
        DIRECTORY
        APPEND
        PROPERTY CMAKE_CONFIGURE_DEPENDS ${zlib_SOURCE_DIR}/zlib.h)

    # The versioned library name comes from ZLIB_VERSION in zlib.h, not from
    # ${zlib_VERSION}.  The header macro is the library's version identity --
    # deflateInit_ and inflateInit_ compare a caller's compile-time copy of it
    # against the library's -- and it is what configure L56 and Makefile.in's
    # `rust' target derive the file name from.  ${zlib_VERSION} comes from
    # project() above and carries no pre-release suffix, so the two can
    # disagree; when they do, both names are published below and both resolve to
    # the same library.
    file(STRINGS ${zlib_SOURCE_DIR}/zlib.h ZLIB_RUST_VERSION_LINE
         REGEX "^#define ZLIB_VERSION \"")
    string(REGEX REPLACE "^#define ZLIB_VERSION \"([^\"]*)\".*$" "\\1"
                         ZLIB_RUST_VERSION "${ZLIB_RUST_VERSION_LINE}")

    if(NOT ZLIB_RUST_VERSION MATCHES "^[0-9]")
        message(
            FATAL_ERROR
                "No usable ZLIB_VERSION in ${zlib_SOURCE_DIR}/zlib.h, so the "
                "versioned Rust library name cannot be derived.  That macro is "
                "the library's version identity and the header must not be "
                "edited; restore it from version control.")
    endif(NOT ZLIB_RUST_VERSION MATCHES "^[0-9]")

    string(REGEX MATCH "^[0-9]+" ZLIB_RUST_SOVERSION ${ZLIB_RUST_VERSION})

    # Makefile.in L47 (SHAREDLIB=libz.so) and L48-L49 spell the ELF names this
    # way, and the SONAME is what an already-linked binary recording `DT_NEEDED
    # libz.so.1' resolves against.
    set(ZLIB_RUST_SHARED_LIB libz.so)
    set(ZLIB_RUST_SONAME ${ZLIB_RUST_SHARED_LIB}.${ZLIB_RUST_SOVERSION})
    set(ZLIB_RUST_SHARED_LIBV ${ZLIB_RUST_SHARED_LIB}.${ZLIB_RUST_VERSION})

    # The archive name comes from the platform rather than from a literal, so
    # that a static-only Rust build works wherever cargo does.  Cargo spells a
    # staticlib's file name with the target's own conventions -- libz.a on Linux
    # and macOS, z.lib for an MSVC target -- and these two CMake variables carry
    # exactly those conventions, so one expression names cargo's output and the
    # file this project installs at the same time.  The shared names above stay
    # literal because the block that uses them is ELF-only by construction.
    set(ZLIB_RUST_STATIC_LIB
        ${CMAKE_STATIC_LIBRARY_PREFIX}z${CMAKE_STATIC_LIBRARY_SUFFIX})

    # Cargo writes into a directory of this build's own, never into the source
    # tree's target/, so an out-of-source build cannot collide with a
    # developer's plain `cargo build'.  The staged drop-in lives beside it and
    # is the only directory that ever holds the versioned chain.
    #
    # It is a cache entry so that a project embedding this one -- and this
    # project's own test/CMakeLists.txt, whose two add_subdirectory fixtures
    # configure a nested copy of this file -- can point several build trees at
    # one cargo directory.  Sharing it is safe: cargo locks the directory, keeps
    # each profile in a subdirectory of its own, and only ever reads and writes
    # its own bookkeeping there.  Nothing caller-visible is shared, because the
    # staged archive, the relinked shared library and the whole versioned
    # symlink chain are copied out into ${ZLIB_RUST_DROPIN_DIR}, which stays per
    # build tree.
    set(ZLIB_RUST_TARGET_DIR
        "${zlib_BINARY_DIR}/rust-target"
        CACHE PATH
              "Directory cargo builds the Rust zlib in (cargo --target-dir)")
    set(ZLIB_RUST_DROPIN_DIR ${zlib_BINARY_DIR}/rust-dropin)
    set(ZLIB_RUST_ARCHIVE ${ZLIB_RUST_DROPIN_DIR}/${ZLIB_RUST_STATIC_LIB})
    set(ZLIB_RUST_LIBRARY ${ZLIB_RUST_DROPIN_DIR}/${ZLIB_RUST_SHARED_LIBV})

    # A faithful, unmodified copy of what cargo produced, kept beside the staged
    # archive rather than being the staged archive.  It exists so that the
    # always-running cargo step has something whose CONTENT it can compare
    # against: the staged archive has its debug sections dropped, so it never
    # equals cargo's output and a copy_if_different straight onto it would
    # rewrite it -- and with it every downstream link -- on every single build.
    # This copy does equal cargo's output, so it is rewritten only on a real
    # change, and the strip below is what turns it into the staged archive.
    set(ZLIB_RUST_RAW_ARCHIVE ${ZLIB_RUST_DROPIN_DIR}/libz-cargo.a)

    # Cargo has two profiles where CMake has four configurations, so Debug maps
    # to cargo's `dev' -- whose output directory is spelled `debug' -- and every
    # optimised configuration maps to `release' (RelWithDebInfo and MinSizeRel
    # have no cargo counterpart).  The mapping is decided HERE, at configure
    # time, from CMAKE_BUILD_TYPE, and it can be: multi-configuration generators
    # are refused where the option is declared, so exactly one configuration is
    # in play and CMAKE_BUILD_TYPE names it -- the default-to-Release rule above
    # guarantees it is never empty.  A configure-time string is preferable to
    # the generator expression this used to be for two reasons that both bit:
    # the cargo profile, the artifact directory, the staged file names and
    # IMPORTED_LOCATION all have to agree, and IMPORTED_LOCATION accepts no
    # generator expression at any CMake version, so only one of the four could
    # ever have been per-configuration; and a VERBATIM custom command passes an
    # empty generator expression through as an empty argument, which cargo
    # rejects outright.
    if(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(ZLIB_RUST_CARGO_PROFILE dev)
        set(ZLIB_RUST_CARGO_PROFILE_DIR debug)
    else(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(ZLIB_RUST_CARGO_PROFILE release)
        set(ZLIB_RUST_CARGO_PROFILE_DIR release)
    endif(CMAKE_BUILD_TYPE STREQUAL "Debug")

    set(ZLIB_RUST_CARGO_ARTIFACT_DIR
        ${ZLIB_RUST_TARGET_DIR}/${ZLIB_RUST_CARGO_PROFILE_DIR})

    # ★ Cargo is invoked through a script, because CARGO IS GIVEN A STANDARD
    # INPUT THAT CARRIES NOTHING, AND A COMMAND CANNOT DO THAT.
    #
    # Before building anything cargo asks the compiler about the target with
    # `rustc - --crate-name ___ --print=file-names ...', and that `-' means
    # rustc reads its program text from standard input.  Neither cargo nor this
    # file controls what that standard input is: it is whatever the process that
    # started the build had, so a build driven from something that leaves data
    # there hands rustc that data as a Rust source file.  That is not
    # theoretical.  It is what made test/CMakeLists.txt's two add_subdirectory
    # fixtures fail intermittently under `ctest -j', with rustc reporting
    # `unknown start of token' on an `<anon>:1' whose text was another process's
    # output -- a compiler diagnostic in one run, another cargo's "Finished
    # `release` profile" line in another, NUL bytes in a third.
    #
    # add_custom_command cannot redirect standard input and `cmake -E env'
    # cannot either, but execute_process can: INPUT_FILE gives the child a
    # standard input of the caller's choosing.  So the command line is handed to
    # a generated script that runs it with the null device there, forwards the
    # environment, and turns a non-zero status into a fatal error naming the
    # command.  The script is written into the build tree, so nothing is added
    # to the source tree and the C configuration never sees it.
    #
    # Be precise about what this does and does not buy.  It removes the one
    # thing this file is responsible for -- cargo, as invoked from here, no
    # longer inherits a standard input from an unknown caller -- and it costs
    # one extra process per build.  It did NOT on its own stop the ctest
    # failures above: they continued with cargo measurably running on /dev/null,
    # so under ctest the bytes reach rustc by a path that starts below this
    # command.  What stopped them was removing the trigger, by not letting a
    # cold target-info probe run inside ctest at all; see the
    # ZLIB_RUST_TARGET_DIR note above and the note in test/CMakeLists.txt beside
    # zlib_add_nested_test.
    set(ZLIB_RUST_CARGO_SCRIPT ${zlib_BINARY_DIR}/zlib-rust-cargo.cmake)
    file(
        WRITE ${ZLIB_RUST_CARGO_SCRIPT}
        "# Generated by CMakeLists.txt.  Runs one cargo command with a standard input
# that carries nothing, because cargo probes the target with `rustc -' -- which
# reads its source from standard input -- and fails when anything is on it.
#
# Invoked as:
#   cmake -DCARGO=<path> \"-DCARGO_ARGS=<;-list>\" \"-DCARGO_ENV=<;-list>\"
#         -DWORK_DIR=<dir> -P zlib-rust-cargo.cmake
if(NOT CARGO)
    message(FATAL_ERROR \"CARGO was not given to this script\")
endif(NOT CARGO)

if(WIN32)
    set(zlib_rust_null_device NUL)
else(WIN32)
    set(zlib_rust_null_device /dev/null)
endif(WIN32)

execute_process(
    COMMAND \${CMAKE_COMMAND} -E env \${CARGO_ENV} \"\${CARGO}\" \${CARGO_ARGS}
    WORKING_DIRECTORY \"\${WORK_DIR}\"
    INPUT_FILE \${zlib_rust_null_device}
    RESULT_VARIABLE zlib_rust_cargo_status)

if(NOT zlib_rust_cargo_status EQUAL 0)
    message(FATAL_ERROR
        \"cargo failed with status \${zlib_rust_cargo_status}:\\n\"
        \"  \${CARGO} \${CARGO_ARGS}\")
endif(NOT zlib_rust_cargo_status EQUAL 0)
")

    # libz-compat exposes the extern "C" surface and gz gates the gzFile layer;
    # without either one the staged library would export nothing at all or carry
    # unresolved references from the C shim.  simd is added only on request and
    # is output-neutral: it changes checksum speed, never emitted bytes.
    set(ZLIB_RUST_FEATURES libz-compat,gz)
    set(ZLIB_RUST_CARGO_ENV CC=${CMAKE_C_COMPILER} AR=${CMAKE_AR})

    if(DEFINED ZLIB_RS_SIMD AND NOT ZLIB_RS_SIMD STREQUAL "")
        # The variable is forwarded as well as translated, so that build.rs's
        # own consistency assertion sees the same request this file acted on.
        list(APPEND ZLIB_RUST_CARGO_ENV ZLIB_RS_SIMD=${ZLIB_RS_SIMD})

        if(ZLIB_RS_SIMD)
            set(ZLIB_RUST_FEATURES ${ZLIB_RUST_FEATURES},simd)
        endif(ZLIB_RS_SIMD)
    endif(DEFINED ZLIB_RS_SIMD AND NOT ZLIB_RS_SIMD STREQUAL "")

    # Debug information, which the two staging steps below would otherwise ship
    # by accident.  Cargo's release profile defaults to strip = "debuginfo", so
    # cargo's own cdylib carries no .debug_* sections at all, and a Release
    # build of the C library above carries none either because Release supplies
    # no -g.  The static archive is the one artifact nothing strips: a staticlib
    # is assembled rather than linked, so the DWARF the prebuilt core and std
    # rlibs were compiled with survives into it, and `cc -r --whole-archive' then
    # re-imports every one of those sections into the relinked shared library,
    # where they are the majority of its size.  Debug information has no effect
    # on behaviour or on the exported surface, and it is invisible to
    # `cmake --install --strip',
    # which CMake implements for install(TARGETS) only and so cannot reach files
    # installed as files below.  So the same treatment cargo applies to its own
    # optimised output is applied here to both staged files, and the strip that
    # install(TARGETS) performs on request is reproduced at the bottom of this
    # file: a packager gets an artifact the size of the code in it, and the
    # documented install option is not silently ineffective.
    #
    # Debug keeps its debug information, because Debug is the configuration that
    # maps to cargo's `dev' profile and the C library keeps its own there too.
    # The decision is made here, at configure time, rather than as a generator
    # expression on the command lines below: a generator expression that
    # evaluates to nothing reaches the tool as an empty argument -- the hazard
    # --profile documents above -- whereas an empty CMake variable expands to no
    # argument at all.  It reads the same CMAKE_BUILD_TYPE the cargo profile
    # above does, and for the same reason it is unambiguous: multi-config
    # generators are refused, so this is a real configuration name rather than
    # the empty string every configuration would share.
    if(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(ZLIB_RUST_STRIP_DEBUG "")
    else(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(ZLIB_RUST_STRIP_DEBUG --strip-debug)
    endif(CMAKE_BUILD_TYPE STREQUAL "Debug")

    # The archive is stripped by a step of its own because cargo hands it over
    # complete and nothing else in this file rewrites it.  objcopy processes an
    # archive member by member and rebuilds its index, so the result is still a
    # linkable archive: every member survives, the internal globals the relink
    # needs are still defined, and the unmodified C drivers link against it and
    # pass -- which is what `make rust-test' and the dropin CI job check rather
    # than something to take on trust here.  It is tool-gated rather than
    # required: the shared path already refuses without objcopy above, and a
    # static-only build that has none simply stages the archive as it is instead
    # of acquiring a new prerequisite.
    set(ZLIB_RUST_STRIP_ARCHIVE_COMMAND "")

    if(CMAKE_OBJCOPY AND ZLIB_RUST_STRIP_DEBUG)
        set(ZLIB_RUST_STAGE_ARCHIVE_COMMAND
            COMMAND ${CMAKE_OBJCOPY} ${ZLIB_RUST_STRIP_DEBUG}
            ${ZLIB_RUST_RAW_ARCHIVE} ${ZLIB_RUST_ARCHIVE})
        set(ZLIB_RUST_STAGE_ARCHIVE_COMMENT
            "Staging ${ZLIB_RUST_STATIC_LIB} without debug sections")
    else(CMAKE_OBJCOPY AND ZLIB_RUST_STRIP_DEBUG)
        set(ZLIB_RUST_STAGE_ARCHIVE_COMMAND
            COMMAND ${CMAKE_COMMAND} -E copy ${ZLIB_RUST_RAW_ARCHIVE}
            ${ZLIB_RUST_ARCHIVE})
        set(ZLIB_RUST_STAGE_ARCHIVE_COMMENT "Staging ${ZLIB_RUST_STATIC_LIB}")
    endif(CMAKE_OBJCOPY AND ZLIB_RUST_STRIP_DEBUG)

    # The published surface, taken from the ZEXTERN declarations in zlib.h so
    # that the header stays the single source of truth.  Everything else the
    # archive defines -- the Rust runtime's own globals above all -- is made
    # local before the shared link, which is what zlib.map cannot do on its own:
    # it has no `local: *' wildcard, so a name it never mentions would otherwise
    # be exported.  Makefile.in reaches the same set with nm and comm at build
    # time; computing it here from the same declarations is equivalent and needs
    # no tool.  The list is copied through configure_file so that a reconfigure
    # that changes nothing does not force a relink.
    file(STRINGS ${zlib_SOURCE_DIR}/zlib.h ZLIB_RUST_ZEXTERN_LINES
         REGEX "^ *ZEXTERN")
    set(ZLIB_RUST_PUBLIC_SYMBOLS "")

    foreach(line IN LISTS ZLIB_RUST_ZEXTERN_LINES)
        if(line MATCHES "^ *ZEXTERN[^(]*[ *]([A-Za-z_][A-Za-z0-9_]*) *\\(")
            list(APPEND ZLIB_RUST_PUBLIC_SYMBOLS ${CMAKE_MATCH_1})
        endif(line MATCHES "^ *ZEXTERN[^(]*[ *]([A-Za-z_][A-Za-z0-9_]*) *\\(")
    endforeach(line IN LISTS ZLIB_RUST_ZEXTERN_LINES)

    if(NOT ZLIB_RUST_PUBLIC_SYMBOLS)
        message(
            FATAL_ERROR
                "No ZEXTERN declarations found in ${zlib_SOURCE_DIR}/zlib.h, "
                "so the exported symbol set of the Rust library cannot be "
                "determined.  Restore the header from version control.")
    endif(NOT ZLIB_RUST_PUBLIC_SYMBOLS)

    list(REMOVE_DUPLICATES ZLIB_RUST_PUBLIC_SYMBOLS)
    list(SORT ZLIB_RUST_PUBLIC_SYMBOLS)
    set(ZLIB_RUST_SYMBOL_FILE ${zlib_BINARY_DIR}/zlib-rust-public.syms)
    string(REPLACE ";" "\n" ZLIB_RUST_SYMBOL_TEXT "${ZLIB_RUST_PUBLIC_SYMBOLS}")
    file(WRITE ${ZLIB_RUST_SYMBOL_FILE}.in "${ZLIB_RUST_SYMBOL_TEXT}\n")
    configure_file(${ZLIB_RUST_SYMBOL_FILE}.in ${ZLIB_RUST_SYMBOL_FILE}
                   COPYONLY)

    # The cargo command line, as a list, so that it can be handed to the script
    # above as a single -D argument and reassembled there.
    set(ZLIB_RUST_CARGO_ARGS
        build
        --locked
        --profile
        ${ZLIB_RUST_CARGO_PROFILE}
        --manifest-path
        ${zlib_SOURCE_DIR}/Cargo.toml
        --target-dir
        ${ZLIB_RUST_TARGET_DIR}
        --package
        libz-rs-sys
        --features
        ${ZLIB_RUST_FEATURES})

    # ★ Step one: cargo, ALWAYS, and the archive as a byproduct.
    #
    # This used to be an add_custom_command(OUTPUT ${ZLIB_RUST_ARCHIVE} ...)
    # with a hand-written DEPENDS list, and that shape is not repairable by
    # extending the list.  The rule would be skipped whenever the archive was
    # newer than every declared input, and the inputs a cargo build actually has
    # are not enumerable from here: they include every file rustc reads, the
    # private headers crates/libz-rs-sys/csrc/*.c include from the repository
    # root, rust-toolchain.toml (which decides WHICH compiler runs, so an edit
    # changes the output without changing any source), clippy.toml,
    # .rustfmt.toml, every environment variable build.rs declares
    # rerun-if-env-changed for, and the contents of the registry cache.  Miss
    # one and CMake serves a stale archive -- silently, and with a library that
    # no longer corresponds to the sources.
    #
    # So cargo is invoked on every build and asked to decide the incremental
    # work, which is the one thing it is authoritative about.  The cost of that
    # decision is a few tens of milliseconds when nothing changed.  Everything
    # DOWNSTREAM stays incremental because what this target writes is a faithful
    # copy of cargo's output through `copy_if_different': identical content
    # means no write, which means the staged archive, the relink, the symlinks
    # and every consumer that links them are all skipped, exactly as before.
    # That is why ${ZLIB_RUST_RAW_ARCHIVE} exists and why the strip is a rule of
    # its own rather than a second command here -- stripping in place would make
    # the compared file differ from cargo's every time and rewrite the whole
    # chain on every build.
    #
    # BYPRODUCTS rather than OUTPUT is what lets a file produced by an
    # always-running target be a dependency of the OUTPUT rules below; it also
    # tells the generator to remove the file on `clean'.
    #
    # With ZLIB_BUILD_SHARED and ZLIB_BUILD_STATIC both OFF this project
    # deliberately produces and installs no library -- in Rust mode exactly as
    # in C mode -- so the target is not added to ALL and cargo is never paid
    # for.
    add_custom_target(
        zlibrust-cargo
        COMMAND ${CMAKE_COMMAND} -E make_directory ${ZLIB_RUST_DROPIN_DIR}
        COMMAND
            # Through the script written above, so that cargo's `rustc -' target
            # probe reads an empty standard input rather than whatever the
            # caller happened to leave on it.  --locked holds the build to the
            # committed Cargo.lock, which is what makes a system library's build
            # reproducible, and is the same flag Makefile.in L413 defaults to.
            # The archive is complete on its own: it defines every function
            # zlib.h declares, including the two variadic ones build.rs compiles
            # from C and merges in.
            ${CMAKE_COMMAND} -DCARGO=${ZLIB_CARGO_EXECUTABLE}
            "-DCARGO_ARGS=${ZLIB_RUST_CARGO_ARGS}"
            "-DCARGO_ENV=${ZLIB_RUST_CARGO_ENV}" -DWORK_DIR=${zlib_SOURCE_DIR}
            -P ${ZLIB_RUST_CARGO_SCRIPT}
        COMMAND
            ${CMAKE_COMMAND} -E copy_if_different
            ${ZLIB_RUST_CARGO_ARTIFACT_DIR}/${ZLIB_RUST_STATIC_LIB}
            ${ZLIB_RUST_RAW_ARCHIVE}
        BYPRODUCTS ${ZLIB_RUST_RAW_ARCHIVE}
        WORKING_DIRECTORY ${zlib_SOURCE_DIR}
        COMMENT "Building ${ZLIB_RUST_STATIC_LIB} with cargo"
        VERBATIM)

    # Step two: the staged archive, which is what this project installs and what
    # the relink below consumes.  Skipped when cargo produced the same bytes as
    # last time.
    add_custom_command(
        OUTPUT ${ZLIB_RUST_ARCHIVE} ${ZLIB_RUST_STAGE_ARCHIVE_COMMAND}
        DEPENDS ${ZLIB_RUST_RAW_ARCHIVE}
        COMMENT "${ZLIB_RUST_STAGE_ARCHIVE_COMMENT}"
        VERBATIM)

    # What the staging target below is asked to produce.  The archive is on the
    # list whenever either library is wanted: a static build installs it
    # directly and the shared relink consumes it, and naming it here is also
    # what makes the rule above run at all -- an add_custom_command OUTPUT is
    # built only when something depends on it.
    set(ZLIB_RUST_ARTIFACTS "")
    set(ZLIB_RUST_SYMLINKS "")

    if(ZLIB_BUILD_SHARED OR ZLIB_BUILD_STATIC)
        list(APPEND ZLIB_RUST_ARTIFACTS ${ZLIB_RUST_ARCHIVE})
    endif(ZLIB_BUILD_SHARED OR ZLIB_BUILD_STATIC)

    # The compiler and linker arguments this build was configured with,
    # forwarded to the two raw ${CMAKE_C_COMPILER} invocations below.
    #
    # Those two commands are the only place in this file that drives a compiler
    # by hand rather than through a CMake target, so they are also the only
    # place where the configured toolchain arguments have to be passed on
    # explicitly. Leaving them out is not cosmetic: -m32 changes the ABI of the
    # object being produced, -fsanitize=address changes what it must be linked
    # against, and -flto changes its representation entirely -- so a project
    # configured with any of them would get a shared library that does not match
    # the rest of the build, from commands that looked like they had honoured
    # the configuration. CMAKE_SHARED_LINKER_FLAGS goes to the shared link only,
    # which is the only command that produces a shared object.
    #
    # The per-configuration flags are folded in at configure time, which is
    # exact here because multi-configuration generators are refused:
    # CMAKE_BUILD_TYPE names the one configuration in play.  separate_arguments
    # is required because these are single strings and a VERBATIM custom command
    # would otherwise pass each whole string as one argument.
    string(TOUPPER "${CMAKE_BUILD_TYPE}" ZLIB_RUST_CONFIG_UPPER)
    separate_arguments(
        ZLIB_RUST_CC_FLAGS UNIX_COMMAND
        "${CMAKE_C_FLAGS} ${CMAKE_C_FLAGS_${ZLIB_RUST_CONFIG_UPPER}}")
    separate_arguments(ZLIB_RUST_LD_FLAGS UNIX_COMMAND
                       "${CMAKE_SHARED_LINKER_FLAGS}")

    if(ZLIB_BUILD_SHARED)
        # Step two: turn that archive into an installable shared library.
        # --whole-archive is required because a shared library pulls nothing out
        # of an archive unless something already references it, so without it
        # the result would be empty.  objcopy then localises everything outside
        # the published set -- measured: it is what keeps the Rust runtime's
        # rust_eh_personality out of the dynamic table -- and drops the debug
        # sections --whole-archive just pulled in, for the reasons set out where
        # ZLIB_RUST_STRIP_DEBUG is decided; both are done in the one pass over
        # the object.  The final link applies the same SONAME and the same
        # zlib.map the C shared target above receives.  The intermediate object
        # is large and of no further use, so it is removed rather than left
        # behind.
        add_custom_command(
            OUTPUT ${ZLIB_RUST_LIBRARY}
            COMMAND
                ${CMAKE_C_COMPILER} ${ZLIB_RUST_CC_FLAGS} -r -nostdlib -o
                ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o -Wl,--whole-archive
                ${ZLIB_RUST_ARCHIVE} -Wl,--no-whole-archive
            COMMAND
                ${CMAKE_OBJCOPY} --keep-global-symbols=${ZLIB_RUST_SYMBOL_FILE}
                ${ZLIB_RUST_STRIP_DEBUG} ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o
            COMMAND
                ${CMAKE_C_COMPILER} ${ZLIB_RUST_CC_FLAGS} ${ZLIB_RUST_LD_FLAGS}
                -shared -fPIC -Wl,-soname,${ZLIB_RUST_SONAME}
                -Wl,--version-script=${zlib_SOURCE_DIR}/zlib.map -o
                ${ZLIB_RUST_LIBRARY} ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o
            COMMAND ${CMAKE_COMMAND} -E remove -f
                    ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o
            DEPENDS ${ZLIB_RUST_ARCHIVE} ${zlib_SOURCE_DIR}/zlib.map
                    ${ZLIB_RUST_SYMBOL_FILE}
            COMMENT "Relinking ${ZLIB_RUST_SHARED_LIBV} through zlib.map"
            VERBATIM)
        list(APPEND ZLIB_RUST_ARTIFACTS ${ZLIB_RUST_LIBRARY})

        # Step three: the chain, which is a correctness requirement rather than
        # packaging polish.  The SONAME is ${ZLIB_RUST_SONAME}, so that is the
        # name the loader searches for; a directory holding only
        # ${ZLIB_RUST_SHARED_LIB} makes it keep searching and bind the system
        # libz instead, silently, which would let a drop-in test pass while
        # exercising the wrong library.  ${ZLIB_RUST_SHARED_LIB} is additionally
        # what -lz resolves.
        set(ZLIB_RUST_ALIASES ${ZLIB_RUST_SONAME} ${ZLIB_RUST_SHARED_LIB})

        # And the name the C shared target publishes, when project()'s version
        # spells it differently from the header, so that an installed
        # ZLIB-shared.cmake -- which names the C library's file -- resolves to
        # this library too.
        set(ZLIB_RUST_PROJECT_LIBV ${ZLIB_RUST_SHARED_LIB}.${INSTALL_VERSION})

        if(NOT ZLIB_RUST_SHARED_LIBV STREQUAL ZLIB_RUST_PROJECT_LIBV)
            list(APPEND ZLIB_RUST_ALIASES ${ZLIB_RUST_PROJECT_LIBV})
        endif(NOT ZLIB_RUST_SHARED_LIBV STREQUAL ZLIB_RUST_PROJECT_LIBV)

        foreach(alias IN LISTS ZLIB_RUST_ALIASES)
            add_custom_command(
                OUTPUT ${ZLIB_RUST_DROPIN_DIR}/${alias}
                COMMAND
                    ${CMAKE_COMMAND} -E create_symlink ${ZLIB_RUST_SHARED_LIBV}
                    ${ZLIB_RUST_DROPIN_DIR}/${alias}
                DEPENDS ${ZLIB_RUST_LIBRARY}
                COMMENT "Linking ${alias} -> ${ZLIB_RUST_SHARED_LIBV}"
                VERBATIM)
            list(APPEND ZLIB_RUST_SYMLINKS ${ZLIB_RUST_DROPIN_DIR}/${alias})
        endforeach(alias IN LISTS ZLIB_RUST_ALIASES)

        list(APPEND ZLIB_RUST_ARTIFACTS ${ZLIB_RUST_SYMLINKS})
    endif(ZLIB_BUILD_SHARED)

    add_custom_target(zlibrust ALL DEPENDS ${ZLIB_RUST_ARTIFACTS})

    # The ordering edge that makes the always-running cargo step run FIRST.  The
    # relink and symlink rules above take ${ZLIB_RUST_ARCHIVE} as a DEPENDS, and
    # it is a BYPRODUCT rather than an OUTPUT, so the file dependency alone does
    # not tell the generator which rule produces it -- this does.  It is
    # conditional so that a build wanting neither library never invokes cargo:
    # zlibrust-cargo is deliberately not in ALL, and this is the only edge that
    # ever reaches it.
    if(ZLIB_BUILD_SHARED OR ZLIB_BUILD_STATIC)
        add_dependencies(zlibrust zlibrust-cargo)
    endif(ZLIB_BUILD_SHARED OR ZLIB_BUILD_STATIC)

    # The staged files are described to CMake as imported libraries so that they
    # can stand in for the C targets everywhere ZLIB::ZLIB and ZLIB::ZLIBSTATIC
    # are consumed.  GLOBAL is required both for the alias below and because
    # test/ and contrib/ reference them from other directories.  The interface
    # they publish is the C targets' interface: the same include directories, so
    # consumers get the identical unmodified zlib.h and generated zconf.h, and
    # the same _LARGEFILE64_SOURCE, which decides whether a caller references
    # the *64 symbol family.  ZLIB_BUILD is deliberately not propagated -- it is
    # private to compiling the C library.  add_dependencies is what makes a
    # consumer wait for the staging above; without it the build stops at "No
    # rule to make target".
    if(ZLIB_BUILD_SHARED)
        add_library(zlibrs SHARED IMPORTED GLOBAL)
        set_target_properties(
            zlibrs PROPERTIES IMPORTED_LOCATION ${ZLIB_RUST_LIBRARY}
                              IMPORTED_SONAME ${ZLIB_RUST_SONAME})
        target_include_directories(
            zlibrs
            INTERFACE $<BUILD_INTERFACE:${zlib_BINARY_DIR}>
                      $<BUILD_INTERFACE:${zlib_SOURCE_DIR}>
                      $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
        target_compile_definitions(
            zlibrs INTERFACE $<$<BOOL:${HAVE_OFF64_T}>:_LARGEFILE64_SOURCE=1>)
        add_dependencies(zlibrs zlibrust)
    endif(ZLIB_BUILD_SHARED)

    if(ZLIB_BUILD_STATIC)
        add_library(zlibrsstatic STATIC IMPORTED GLOBAL)
        set_target_properties(zlibrsstatic PROPERTIES IMPORTED_LOCATION
                                                      ${ZLIB_RUST_ARCHIVE})
        target_include_directories(
            zlibrsstatic
            INTERFACE $<BUILD_INTERFACE:${zlib_BINARY_DIR}>
                      $<BUILD_INTERFACE:${zlib_SOURCE_DIR}>
                      $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
        target_compile_definitions(
            zlibrsstatic
            INTERFACE $<$<BOOL:${HAVE_OFF64_T}>:_LARGEFILE64_SOURCE=1>)

        # The one part of the interface that is NOT the C target's interface. A
        # Rust staticlib carries the Rust standard library's object code, so
        # linking it needs the libraries rustc named above; a C libz.a needs
        # none of them.  Publishing them on the target is what makes
        # `target_link_libraries(app ZLIB::ZLIBSTATIC)' produce a complete link
        # line -- without it a consumer of this build tree gets undefined
        # references to symbols it never mentioned, from an archive it was told
        # was self-contained.  Every element begins with `-', which CMake passes
        # through as a link flag.
        #
        # ★ THIS IS THE ONLY PLACE THE BUILD-TREE INTERFACE IS SET, and the
        # installed export has exactly one owner of its own further down.  An
        # earlier revision attached the same list from three places and from two
        # separate probes: `target_link_libraries' here, a `set_target_properties'
        # beside the descriptor below that overwrote rather than appended, and a
        # `set_property(APPEND)' in the install block.  Duplicated flags were the
        # visible symptom; the real cost was that no single place owned the answer,
        # so a change to the probe could reach one consumption channel and not the
        # other.  One owner per channel, and the result is read back and asserted.
        if(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS_LIST)
            target_link_libraries(
                zlibrsstatic INTERFACE ${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST})

            get_target_property(ZLIB_RUST_STATIC_INTERFACE zlibrsstatic
                               INTERFACE_LINK_LIBRARIES)

            if(NOT ZLIB_RUST_STATIC_INTERFACE STREQUAL
               "${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}")
                message(
                    FATAL_ERROR
                        "ZLIB::ZLIBSTATIC's interface link libraries are\n"
                        "    ${ZLIB_RUST_STATIC_INTERFACE}\n"
                        "but the probe for this target says they must be exactly\n"
                        "    ${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}\n"
                        "Something else in this file is also writing that "
                        "property.  There must be exactly one owner of it: a "
                        "second writer either duplicates the flags or discards "
                        "the probe's answer.")
            endif(NOT ZLIB_RUST_STATIC_INTERFACE STREQUAL
                  "${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}")
        endif(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS_LIST)

        add_dependencies(zlibrsstatic zlibrust)

        # ------------------------------------------------------------------
        # The pkg-config descriptor for the staged ARCHIVE.
        #
        # For dynamic linking one descriptor serves both implementations: -lz
        # resolves to either library, and a shared object records its own
        # dependencies in DT_NEEDED, so nothing has to be advertised.  Static
        # linking is where they part: the C libz.a needs no companion libraries,
        # while a Rust staticlib needs the Rust runtime's own native ones.
        #
        # THERE IS EXACTLY ONE PRODUCER OF THAT Libs.private KEY, AND IT IS
        # zlib.pc.cmakein.  The template carries a single @ZLIB_PC_LIBS_PRIVATE@
        # at the end of its Libs: line; the block above this one sets that
        # variable to the probed set in Rust mode and to nothing in C mode, and
        # configure_file() writes ${ZLIB_PC} from it.  So ${ZLIB_PC} ALREADY IS
        # the Rust-mode descriptor -- there is nothing left to add here, and
        # adding it here is precisely the defect this revision removes: a second
        # producer that read ${ZLIB_PC}, refused to continue if it already
        # carried the key, and then added the key itself could never succeed in
        # the one mode it existed for.
        #
        # What remains here is the staged COPY.  It is written into the staging
        # directory under the name pkg-config looks for, so that
        # PKG_CONFIG_PATH pointed at the staged tree answers correctly before
        # anything is installed, and it is a copy rather than a second
        # generation so that it cannot drift from the descriptor the install
        # rule publishes.  Which of the two files is installed is decided by the
        # single install() rule further down, not by declaration order, for the
        # reason recorded there.
        #
        # Makefile.in's `rust-pc' target is the autotools half of this, and it
        # additionally links and runs a C consumer built from nothing but the
        # flags this descriptor answers.  That gate belongs to a build system
        # that can run tools at build time against a staged tree; what is
        # asserted at configure time is what can be asserted here -- that the
        # descriptor really does carry one non-empty private link line and still
        # advertises -lz, which is what test/check_pc_private.cmake checks again
        # on the INSTALLED file.
        set(ZLIB_RUST_PC ${ZLIB_RUST_DROPIN_DIR}/zlib.pc)

        file(READ ${ZLIB_PC} ZLIB_RUST_PC_BODY)

        # One key, non-empty, and the public -lz line intact.  A gate on the
        # generated descriptor rather than on the variable that fed it, because
        # the template is what decides where the substitution lands: a stray
        # second occurrence of the name, or a Libs: line that stopped beginning
        # in column 0, is invisible in the variable and obvious here.
        string(REGEX MATCHALL "(^|\n)Libs\\.private:[ \t]*[^\n]*"
                     ZLIB_RUST_PC_PRIVATE_KEYS "${ZLIB_RUST_PC_BODY}")
        list(LENGTH ZLIB_RUST_PC_PRIVATE_KEYS ZLIB_RUST_PC_PRIVATE_COUNT)

        if(NOT ZLIB_RUST_PC_PRIVATE_COUNT EQUAL 1)
            message(
                FATAL_ERROR
                    "${ZLIB_PC} carries ${ZLIB_RUST_PC_PRIVATE_COUNT} "
                    "Libs.private keys; a Rust static build must publish "
                    "exactly one, carrying "
                    "${ZLIB_RUST_NATIVE_STATIC_LIBS}.  pkg-config reads the "
                    "first and ignores the rest, so a duplicate descriptor "
                    "would not say what it appears to say.  Check that "
                    "zlib.pc.cmakein still contains exactly one "
                    "@-delimited private-libraries name and one Libs: line "
                    "beginning in column 0.")
        endif(NOT ZLIB_RUST_PC_PRIVATE_COUNT EQUAL 1)

        if(NOT ZLIB_RUST_PC_BODY MATCHES "(^|\n)Libs\\.private:[ \t]*[^ \t\n]")
            message(
                FATAL_ERROR
                    "${ZLIB_PC} carries an empty Libs.private key.  An empty "
                    "value satisfies a presence test while telling a static "
                    "consumer nothing, which is the defect this key exists to "
                    "remove.")
        endif(NOT ZLIB_RUST_PC_BODY MATCHES "(^|\n)Libs\\.private:[ \t]*[^ \t\n]")

        if(NOT ZLIB_RUST_PC_BODY MATCHES "(^|\n)Libs:[^\n]*-lz")
            message(
                FATAL_ERROR
                    "${ZLIB_PC} no longer advertises `Libs: ... -lz'.  That is "
                    "part of the drop-in contract: a consumer's link command "
                    "must not have to change because the implementation did.")
        endif(NOT ZLIB_RUST_PC_BODY MATCHES "(^|\n)Libs:[^\n]*-lz")

        file(
            WRITE ${ZLIB_RUST_PC}
            "# GENERATED FILE -- do not edit.  CMake rewrites it on every"
            " configure.\n"
            "#\n"
            "# This is ${ZLIB_PC}, copied beside the STAGED RUST LIBRARY in"
            " this directory\n"
            "# so that PKG_CONFIG_PATH pointed here answers before anything is"
            " installed.\n"
            "# It is a copy and not a second generation, so the two cannot"
            " disagree.\n"
            "#\n"
            "# Its Libs.private line carries the native libraries a Rust"
            " staticlib needs on\n"
            "# this target, from `rustc --print native-static-libs', and is"
            " TARGET-SPECIFIC.\n"
            "# Do not copy it to another target: derive it there.\n"
            "#\n"
            "# `cmake --install' installs one descriptor at"
            " <libdir>/pkgconfig/zlib.pc, chosen\n"
            "# by the single rule declared with the Rust install rules, so the"
            " descriptor and\n"
            "# the library a consumer finds are the same implementation.\n")
        file(APPEND ${ZLIB_RUST_PC} "${ZLIB_RUST_PC_BODY}")
        # ------------------------------------------------------------------
    endif(ZLIB_BUILD_STATIC)
endif(ZLIB_BUILD_RUST)

# ============================================================================
# exported ZLIB::* targets
# ============================================================================
#
# One selection for both implementations, because an alias cannot be redefined.
# Everything downstream -- every executable in test/, the consumers in contrib/
# and the installed package -- reaches the library exclusively through these two
# names, so this is the single switch that decides which implementation the
# whole tree is built and tested against.  With ZLIB_BUILD_RUST OFF they are the
# C targets, exactly as before.
if(ZLIB_BUILD_SHARED)
    if(ZLIB_BUILD_RUST)
        add_library(ZLIB::ZLIB ALIAS zlibrs)
    else(ZLIB_BUILD_RUST)
        add_library(ZLIB::ZLIB ALIAS zlib)
    endif(ZLIB_BUILD_RUST)
endif(ZLIB_BUILD_SHARED)

if(ZLIB_BUILD_STATIC)
    if(ZLIB_BUILD_RUST)
        add_library(ZLIB::ZLIBSTATIC ALIAS zlibrsstatic)
    else(ZLIB_BUILD_RUST)
        add_library(ZLIB::ZLIBSTATIC ALIAS zlibstatic)
    endif(ZLIB_BUILD_RUST)
endif(ZLIB_BUILD_STATIC)

if(ZLIB_INSTALL)
    if(ZLIB_BUILD_SHARED)
        install(
            TARGETS zlib
            COMPONENT Runtime
            EXPORT zlibSharedExport
            RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
            ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}"
            LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}")
        install(
            EXPORT zlibSharedExport
            FILE ZLIB-shared.cmake
            NAMESPACE ZLIB::
            DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/zlib)

        if(MSVC)
            install(
                FILES $<TARGET_PDB_FILE:zlib>
                COMPONENT Runtime
                DESTINATION ${CMAKE_INSTALL_BINDIR}
                CONFIGURATIONS Debug OR RelWithDebInfo
                OPTIONAL)
        endif(MSVC)
    endif(ZLIB_BUILD_SHARED)

    if(ZLIB_BUILD_STATIC)
        install(
            TARGETS zlibstatic
            COMPONENT Development
            EXPORT zlibStaticExport
            RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
            ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}"
            LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}")
        install(
            EXPORT zlibStaticExport
            FILE ZLIB-static.cmake
            NAMESPACE ZLIB::
            DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/zlib)
    endif(ZLIB_BUILD_STATIC)

    configure_package_config_file(
        ${zlib_SOURCE_DIR}/zlibConfig.cmake.in
        ${zlib_BINARY_DIR}/ZLIBConfig.cmake
        INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/zlib)

    write_basic_package_version_file(
        "${zlib_BINARY_DIR}/ZLIBConfigVersion.cmake"
        VERSION "${zlib_VERSION}"
        COMPATIBILITY AnyNewerVersion)

    install(FILES ${zlib_BINARY_DIR}/ZLIBConfig.cmake
                  ${zlib_BINARY_DIR}/ZLIBConfigVersion.cmake
            DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/zlib)
    install(
        FILES ${ZLIB_PUBLIC_HDRS}
        COMPONENT Development
        DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}")
    install(
        FILES zlib.3
        COMPONENT Docs
        DESTINATION "${CMAKE_INSTALL_MANDIR}/man3")
    install(
        FILES LICENSE
              doc/algorithm.txt
              doc/crc-doc.1.0.pdf
              doc/rfc1950.txt
              doc/rfc1951.txt
              doc/rfc1952.txt
              doc/txtvsbin.txt
        COMPONENT Docs
        DESTINATION "${CMAKE_INSTALL_DOCDIR}/zlib")
    # The pkg-config descriptor, and there is exactly one of it.  With
    # ZLIB_BUILD_RUST the staged Rust library brings its own -- generated in the
    # ZLIB_BUILD_RUST block above, the C descriptor plus the one Libs.private
    # key a Rust static archive needs -- and both files are named zlib.pc, so
    # both would occupy <libdir>/pkgconfig/zlib.pc.
    #
    # THE CHOICE IS MADE HERE RATHER THAN BY DECLARATION ORDER, and that is a
    # correction rather than a preference.  Everything else the Rust path
    # publishes wins by being installed last, but a descriptor cannot: the
    # generated cmake_install.cmake calls file(INSTALL), which skips a copy
    # whose source and destination timestamps agree, and both descriptors are
    # written by the same configure run.  Measured with both rules declared --
    # the C descriptor was installed, and the Rust one reported "Up-to-date" and
    # never replaced it, leaving an installed libz.a that needs seven native
    # libraries described by a file naming none.  Two rules for one path is the
    # defect; one rule that picks is the fix.
    #
    # Only the static case takes the Rust descriptor, because static consumption
    # is the only case where the two differ, and it is also the only case in
    # which a Rust archive is published at all.
    if(ZLIB_BUILD_RUST AND ZLIB_BUILD_STATIC)
        install(
            FILES ${ZLIB_RUST_PC}
            COMPONENT Development
            DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")
    else(ZLIB_BUILD_RUST AND ZLIB_BUILD_STATIC)
        install(
            FILES ${ZLIB_PC}
            COMPONENT Development
            DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")
    endif(ZLIB_BUILD_RUST AND ZLIB_BUILD_STATIC)

    # The Rust artifacts go to the same destinations, under the same component
    # names, as the C libraries above, so that zlib.pc keeps advertising -lz and
    # the exported ZLIB-shared.cmake and ZLIB-static.cmake keep naming files
    # that exist.  Those two export files, their component names and the header,
    # zlib.3, LICENSE and doc/ rules are deliberately untouched: the installed
    # package's shape is part of the contract, and test/*.cmake.in asserts that
    # requesting a component outside {shared, static} fails.  The zlib.pc rule
    # above is the one exception, and it is not an addition to the shape: still
    # one descriptor at one path, chosen there rather than duplicated here.
    #
    # These rules are declared LAST on purpose.  install() runs in declaration
    # order, so where a name is published by both implementations the Rust file
    # is the one that ends up installed -- including the C library's own
    # versioned name, which becomes a link to it.  That is what makes
    # find_package(ZLIB CONFIG) hand a consumer the implementation this build
    # selected instead of whichever rule happened to run last.
    #
    # install(TARGETS) is not usable for them: it accepts IMPORTED targets only
    # from CMake 3.21, well above this project's floor, so the staged files are
    # installed as files.  PROGRAMS rather than FILES for the shared library
    # because a shared object is installed executable, matching what
    # install(TARGETS ... LIBRARY) does for the C target.  The one thing that
    # spelling loses is the strip on request, which CMake generates for
    # install(TARGETS) and for nothing else, so it is reproduced below.
    if(ZLIB_BUILD_RUST)
        if(ZLIB_BUILD_SHARED)
            install(
                PROGRAMS ${ZLIB_RUST_LIBRARY}
                COMPONENT Runtime
                DESTINATION "${CMAKE_INSTALL_LIBDIR}")
            install(
                FILES ${ZLIB_RUST_SYMLINKS}
                COMPONENT Runtime
                DESTINATION "${CMAKE_INSTALL_LIBDIR}")

            # `cmake --install --strip', and -DCMAKE_INSTALL_DO_STRIP=ON with
            # it, is a documented option that a packager reasonably expects to
            # apply to the library it is installing.  For the C target above
            # CMake emits exactly this block itself; for a file installed by
            # install(PROGRAMS) it emits nothing, so without this the option is
            # silently ineffective on the primary artifact of a Rust-mode
            # install.  What is reproduced is CMake's own behaviour, condition
            # for condition: the tool it located, the existence and
            # not-a-symlink guards -- the three published aliases are links and
            # stripping through one would replace it with a file -- and
            # CMAKE_INSTALL_DO_STRIP read at install time rather than here, so
            # that one build directory serves a stripped and an unstripped
            # install alike.  DESTDIR is honoured the same way the generated
            # script honours it.  The static archive is deliberately not
            # stripped: CMake refuses to strip static libraries because a full
            # strip removes the symbol table that makes them linkable, and the
            # debug sections it would otherwise carry are already dropped at
            # staging time.
            if(CMAKE_STRIP)
                if(IS_ABSOLUTE "${CMAKE_INSTALL_LIBDIR}")
                    set(ZLIB_RUST_INSTALLED_LIBDIR "${CMAKE_INSTALL_LIBDIR}")
                else(IS_ABSOLUTE "${CMAKE_INSTALL_LIBDIR}")
                    set(ZLIB_RUST_INSTALLED_LIBDIR
                        "\${CMAKE_INSTALL_PREFIX}/${CMAKE_INSTALL_LIBDIR}")
                endif(IS_ABSOLUTE "${CMAKE_INSTALL_LIBDIR}")

                install(
                    CODE "
set(_zlib_rust_installed_library
    \"\$ENV{DESTDIR}${ZLIB_RUST_INSTALLED_LIBDIR}/${ZLIB_RUST_SHARED_LIBV}\")

if(CMAKE_INSTALL_DO_STRIP
   AND EXISTS \"\${_zlib_rust_installed_library}\"
   AND NOT IS_SYMLINK \"\${_zlib_rust_installed_library}\")
    message(STATUS \"Stripping: \${_zlib_rust_installed_library}\")
    execute_process(COMMAND \"${CMAKE_STRIP}\"
                            \"\${_zlib_rust_installed_library}\")
endif(CMAKE_INSTALL_DO_STRIP
      AND EXISTS \"\${_zlib_rust_installed_library}\"
      AND NOT IS_SYMLINK \"\${_zlib_rust_installed_library}\")
"
                    COMPONENT Runtime)
            endif(CMAKE_STRIP)
        endif(ZLIB_BUILD_SHARED)

        if(ZLIB_BUILD_STATIC)
            install(
                FILES ${ZLIB_RUST_ARCHIVE}
                COMPONENT Development
                DESTINATION "${CMAKE_INSTALL_LIBDIR}")

            # The installed ZLIB-static.cmake is generated from the C target
            # `zlibstatic', because install(TARGETS) does not accept an IMPORTED
            # target below CMake 3.21 and the staged archive is therefore
            # installed as a file.  In Rust mode the FILE that lands in libdir
            # is the Rust archive -- the rule above runs after the C one and
            # wins the name -- while the exported interface describing it still
            # came from the C target, which needs no companion libraries.  An
            # installed consumer writing `find_package(ZLIB CONFIG);
            # target_link_libraries(app ZLIB::ZLIBSTATIC)' would get an
            # incomplete link line from a package that had every chance to state
            # the truth.
            #
            # Publishing the probed set on `zlibstatic' is what closes that: the
            # generated export file carries INTERFACE_LINK_LIBRARIES verbatim,
            # and every element is a `-l' flag, so it needs no target to exist
            # in the consumer's project and no path to survive relocation.  It
            # is appended only in Rust mode, so a C-mode package keeps
            # advertising an archive that stands alone, which is the truth
            # there.
            #
            # ★ THIS IS THE ONLY OWNER OF THE INSTALLED EXPORT'S COPY, exactly as
            # the build-tree target above owns its own.  The C target carries no
            # interface link libraries of its own, so the append lands on an empty
            # property and the result is asserted to be precisely the probed set --
            # a second writer anywhere would show up here as a duplicate.
            if(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS_LIST)
                set_property(
                    TARGET zlibstatic
                    APPEND
                    PROPERTY INTERFACE_LINK_LIBRARIES
                             ${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST})

                get_target_property(ZLIB_C_STATIC_INTERFACE zlibstatic
                                   INTERFACE_LINK_LIBRARIES)

                if(NOT ZLIB_C_STATIC_INTERFACE STREQUAL
                   "${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}")
                    message(
                        FATAL_ERROR
                            "the interface link libraries exported for the "
                            "installed static package are\n"
                            "    ${ZLIB_C_STATIC_INTERFACE}\n"
                            "but the probe for this target says they must be "
                            "exactly\n"
                            "    ${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}\n"
                            "ZLIB-static.cmake carries this property verbatim, "
                            "so a duplicated or foreign entry becomes a "
                            "consumer's link line.")
                endif(NOT ZLIB_C_STATIC_INTERFACE STREQUAL
                      "${ZLIB_RUST_NATIVE_STATIC_LIBS_LIST}")
            endif(DEFINED ZLIB_RUST_NATIVE_STATIC_LIBS_LIST)
        endif(ZLIB_BUILD_STATIC)
    endif(ZLIB_BUILD_RUST)
endif(ZLIB_INSTALL)

# ============================================================================
# Tests
# ============================================================================
if(ZLIB_BUILD_TESTING)
    enable_testing()
    add_subdirectory(test)
endif(ZLIB_BUILD_TESTING)

add_subdirectory(contrib)
