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

    # The staged drop-in library is relinked from cargo's static archive through
    # zlib.map, which needs an ELF linker that understands -soname and
    # --version-script.  That is precisely the platform set on which the C
    # shared target applies the same version script below, so the two agree by
    # construction.  Mach-O and MSVC spell every one of those concepts
    # differently and are out of scope for this port; refusing here is honest
    # and actionable, whereas emitting a half-formed dylib or import library
    # would look like support that does not exist.
    if(NOT UNIX
       OR APPLE
       OR (CMAKE_SYSTEM_NAME STREQUAL AIX)
       OR (CMAKE_SYSTEM_NAME STREQUAL SunOS))
        message(
            FATAL_ERROR
                "ZLIB_BUILD_RUST is only supported on ELF platforms whose "
                "linker accepts -soname and --version-script, which is the "
                "same platform set that receives zlib.map in the C shared "
                "library below.  CMAKE_SYSTEM_NAME is ${CMAKE_SYSTEM_NAME}.\n"
                "Reconfigure with -DZLIB_BUILD_RUST=OFF to build the C library."
        )
    endif(
        NOT UNIX
        OR APPLE
        OR (CMAKE_SYSTEM_NAME STREQUAL AIX)
        OR (CMAKE_SYSTEM_NAME STREQUAL SunOS))

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
    # vectorised checksum backends.  crates/libz-rs-sys/build.rs declares
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
endif(ZLIB_BUILD_RUST)

get_property(IS_MULTI GLOBAL PROPERTY GENERATOR_IS_MULTI_CONFIG)

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
# at libraries built from crates/libz-rs-sys instead of the C sources.  The C
# targets `zlib' and `zlibstatic' above are ALWAYS defined and built exactly as
# before: they are the differential oracle the port is verified against, and
# contrib/ consumes them by name.  Nothing here touches zlib.h, zconf.h or
# zlib.map -- the public contract is read and, in zlib.map's case, handed to the
# linker, never edited -- so the exported API and ABI are unchanged and callers
# link without recompilation.
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
    set(ZLIB_RUST_STATIC_LIB libz.a)

    # Cargo writes into a directory of this build's own, never into the source
    # tree's target/, so an out-of-source build cannot collide with a
    # developer's plain `cargo build'.  The staged drop-in lives beside it and
    # is the only directory that ever holds the versioned chain.
    set(ZLIB_RUST_TARGET_DIR ${zlib_BINARY_DIR}/rust-target)
    set(ZLIB_RUST_DROPIN_DIR ${zlib_BINARY_DIR}/rust-dropin)
    set(ZLIB_RUST_ARCHIVE ${ZLIB_RUST_DROPIN_DIR}/${ZLIB_RUST_STATIC_LIB})
    set(ZLIB_RUST_LIBRARY ${ZLIB_RUST_DROPIN_DIR}/${ZLIB_RUST_SHARED_LIBV})

    # Cargo has two profiles where CMake has four configurations, so Debug maps
    # to cargo's `dev' -- whose output directory is spelled `debug' -- and every
    # optimised configuration maps to `release' (RelWithDebInfo and MinSizeRel
    # have no cargo counterpart).  The mapping is a generator expression because
    # a multi-config generator only knows the configuration at build time, and
    # it is spelled as an argument to `--profile' rather than as a conditional
    # `--release' so that it is never empty: a VERBATIM custom command passes an
    # empty generator expression through as an empty argument, which cargo
    # rejects outright.  The staged drop-in directory, by contrast, is NOT
    # per-configuration: add_custom_command's OUTPUT, BYPRODUCTS and DEPENDS
    # accept generator expressions only from CMake 3.20, well above the 3.12
    # floor this project supports, and IMPORTED_LOCATION accepts none at all. So
    # under a multi-config generator the staged library is the one belonging to
    # the configuration most recently built; single-config generators -- which
    # is what every configuration in .github/workflows/cmake.yml uses on this
    # platform -- are unaffected.
    set(ZLIB_RUST_CARGO_ARTIFACT_DIR
        ${ZLIB_RUST_TARGET_DIR}/$<IF:$<CONFIG:Debug>,debug,release>)

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

    # Cargo owns incremental rebuilds of its own crates, but it has to be
    # reached first, so the crate sources are what this rule depends on.
    # CONFIGURE_DEPENDS re-runs the glob at build time, so adding or removing a
    # source file is noticed without a manual reconfigure.
    file(
        GLOB_RECURSE
        ZLIB_RUST_SOURCES
        CONFIGURE_DEPENDS
        ${zlib_SOURCE_DIR}/crates/*.rs
        ${zlib_SOURCE_DIR}/crates/*.c
        ${zlib_SOURCE_DIR}/crates/*.h
        ${zlib_SOURCE_DIR}/crates/*.toml)

    # Step one: cargo, then stage the archive.  --locked holds the build to the
    # committed Cargo.lock, which is what makes a system library's build
    # reproducible, and is the same flag Makefile.in L413 defaults to.  The
    # archive is complete on its own: it defines every function zlib.h declares,
    # including the two variadic ones build.rs compiles from C and merges in.
    add_custom_command(
        OUTPUT ${ZLIB_RUST_ARCHIVE}
        COMMAND ${CMAKE_COMMAND} -E make_directory ${ZLIB_RUST_DROPIN_DIR}
        COMMAND
            ${CMAKE_COMMAND} -E env ${ZLIB_RUST_CARGO_ENV}
            ${ZLIB_CARGO_EXECUTABLE} build --locked --profile
            $<IF:$<CONFIG:Debug>,dev,release> --manifest-path
            ${zlib_SOURCE_DIR}/Cargo.toml --target-dir ${ZLIB_RUST_TARGET_DIR}
            --package libz-rs-sys --features ${ZLIB_RUST_FEATURES}
        COMMAND
            ${CMAKE_COMMAND} -E copy_if_different
            ${ZLIB_RUST_CARGO_ARTIFACT_DIR}/${ZLIB_RUST_STATIC_LIB}
            ${ZLIB_RUST_ARCHIVE}
        DEPENDS ${ZLIB_RUST_SOURCES} ${zlib_SOURCE_DIR}/Cargo.toml
                ${zlib_SOURCE_DIR}/Cargo.lock ${zlib_SOURCE_DIR}/zlib.h
        WORKING_DIRECTORY ${zlib_SOURCE_DIR}
        COMMENT "Building ${ZLIB_RUST_STATIC_LIB} with cargo"
        VERBATIM)
    set(ZLIB_RUST_ARTIFACTS ${ZLIB_RUST_ARCHIVE})
    set(ZLIB_RUST_SYMLINKS "")

    if(ZLIB_BUILD_SHARED)
        # Step two: turn that archive into an installable shared library.
        # --whole-archive is required because a shared library pulls nothing out
        # of an archive unless something already references it, so without it
        # the result would be empty.  objcopy then localises everything outside
        # the published set -- measured: it is what keeps the Rust runtime's
        # rust_eh_personality out of the dynamic table -- and the final link
        # applies the same SONAME and the same zlib.map the C shared target
        # above receives.  The intermediate object is large and of no further
        # use, so it is removed rather than left behind.
        add_custom_command(
            OUTPUT ${ZLIB_RUST_LIBRARY}
            COMMAND
                ${CMAKE_C_COMPILER} -r -nostdlib -o
                ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o -Wl,--whole-archive
                ${ZLIB_RUST_ARCHIVE} -Wl,--no-whole-archive
            COMMAND
                ${CMAKE_OBJCOPY} --keep-global-symbols=${ZLIB_RUST_SYMBOL_FILE}
                ${ZLIB_RUST_DROPIN_DIR}/libz-whole.o
            COMMAND
                ${CMAKE_C_COMPILER} -shared -fPIC
                -Wl,-soname,${ZLIB_RUST_SONAME}
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
        add_dependencies(zlibrsstatic zlibrust)
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
    install(
        FILES ${ZLIB_PC}
        COMPONENT Development
        DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")

    # The Rust artifacts go to the same destinations, under the same component
    # names, as the C libraries above, so that zlib.pc keeps advertising -lz and
    # the exported ZLIB-shared.cmake and ZLIB-static.cmake keep naming files
    # that exist.  Those two export files, their component names and the header,
    # zlib.3, LICENSE, doc/ and zlib.pc rules are deliberately untouched: the
    # installed package's shape is part of the contract, and test/*.cmake.in
    # asserts that requesting a component outside {shared, static} fails.
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
    # install(TARGETS ... LIBRARY) does for the C target.
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
        endif(ZLIB_BUILD_SHARED)

        if(ZLIB_BUILD_STATIC)
            install(
                FILES ${ZLIB_RUST_ARCHIVE}
                COMPONENT Development
                DESTINATION "${CMAKE_INSTALL_LIBDIR}")
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
