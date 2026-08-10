# ============================================================================
# the CTest suite
# ============================================================================
#
# Every executable below links ZLIB::ZLIB or ZLIB::ZLIBSTATIC and nothing else,
# and the root CMakeLists.txt decides which implementation those two names
# denote: the C library by default, or -- with -DZLIB_BUILD_RUST=ON -- the
# memory-safe Rust library staged from crates/libz-rs-sys.  Re-pointing those
# two aliases is the entire mechanism by which this suite runs against either
# implementation, and it is why no case here needs a Rust counterpart: with the
# option ON, zlib_example, zlib_example64, zlib_static_example,
# zlib_static_example64 and zlib_coverage ARE the Rust-linked cases, as are the
# three find_package fixtures further down, which consume what was installed.
# The absence of `_rust'-suffixed targets is deliberate rather than work left
# undone -- a second case linking the same target the first one already links
# would add no coverage, while a second example.c writing the same foo.gz would
# add a race under `ctest -j' -- so a reader looking for them should look at the
# alias instead.  The two add_subdirectory fixtures are the one exception and
# say so at their own definitions.
#
# example.c, minigzip.c and infcover.c are compiled from the same sources, with
# the same definitions, whichever implementation is behind the alias.  That
# invariance is the point rather than an accident of how this file is written: a
# consumer of zlib needs no edit and no recompilation, so a test driver that
# needed one would disprove the very thing it exists to demonstrate.
#
# Nothing here declares an include directory of its own.  `zlib.h', the
# generated `zconf.h' and, where the platform has off64_t,
# `_LARGEFILE64_SOURCE=1' all arrive as usage requirements of the target behind
# the alias, and both implementations publish them -- the C targets and the
# imported Rust ones alike.  So if a driver ever fails to find a header or stops
# seeing the *64 declarations, that interface is what is wrong. Compensating for
# it here with target_include_directories or an extra definition would hide a
# root CMakeLists.txt defect and could compile a driver against headers other
# than the ones the library was built from; it belongs fixed there.
function(ZLIB_findTestEnv testName)
    set(testEnv "PATH=")

    if(MSVC OR MINGW)
        set(separator "\\\;")
    else()
        set(separator ":")
    endif()

    string(APPEND testEnv "$<TARGET_FILE_DIR:ZLIB::ZLIB>${separator}")
    string(APPEND testEnv "$ENV{PATH}")

    set_tests_properties(${testName} PROPERTIES ENVIRONMENT "${testEnv}")
endfunction()

if(ZLIB_BUILD_SHARED)
    add_executable(zlib_example example.c)
    target_link_libraries(zlib_example ZLIB::ZLIB)
    target_compile_definitions(zlib_example PRIVATE ZLIB_BUILD)
    add_test(NAME zlib_example COMMAND zlib_example)

    add_executable(minigzip minigzip.c)
    target_compile_definitions(
        minigzip PRIVATE $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>)
    target_link_libraries(minigzip ZLIB::ZLIB)

    if(MSVC
       OR MSYS
       OR MINGW
       OR CYGWIN)
        zlib_findtestenv(zlib_example)
    endif(
        MSVC
        OR MSYS
        OR MINGW
        OR CYGWIN)

    # This case and zlib_static_example64 below take their large-file character
    # entirely from the `_LARGEFILE64_SOURCE=1' the library target propagates,
    # never from a flag of their own: zconf.h L504-L505 turns that into
    # Z_LARGE64, under which zlib.h L1977-L1986 declares gzopen64, gzseek64,
    # gztell64, gzoffset64, adler32_combine64, crc32_combine64 and
    # crc32_combine_gen64 in addition to the unsuffixed family, so a driver
    # compiled this way requires BOTH families to resolve.  That is the whole
    # value of these two cases and it is why -D_FILE_OFFSET_BITS=64 is
    # deliberately absent: it would select zconf.h L508-L509's Z_WANT64
    # redirect, where the unsuffixed names become macros for the *64 ones and
    # the unsuffixed symbols are no longer referenced at all.  That is a
    # different case, covered by Makefile.in's example64 and minigzip64.
    if(HAVE_OFF64_T)
        add_executable(zlib_example64 example.c)
        target_compile_definitions(
            zlib_example64
            PRIVATE ZLIB_BUILD
                    $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>)
        target_link_libraries(zlib_example64 ZLIB::ZLIB)
        add_test(NAME zlib_example64 COMMAND zlib_example64)

        if(MSVC
           OR MSYS
           OR MINGW
           OR CYGWIN)
            zlib_findtestenv(zlib_example64)
        endif(
            MSVC
            OR MSYS
            OR MINGW
            OR CYGWIN)
    endif(HAVE_OFF64_T)

    # Which library was actually loaded is not something the cases above can
    # check.  ZLIB_BUILD_RUST stages libz.so.<ZLIB_VERSION> together with the
    # libz.so.1 and libz.so links to it, and libz.so.1 -- the SONAME, recorded
    # in this executable -- is the name the loader searches for.  Should that
    # link ever be missing the loader does not fail: it keeps searching and
    # binds the system libz, silently.  Nothing inside the process can tell,
    # because the port returns the same ZLIB_VERSION the header declares, so
    # example.c's own version check passes either way and the suite would report
    # success while exercising a library this build did not produce.  Hence the
    # binding itself is asserted, which Makefile.in's `rust-test' target does
    # the same way for the autotools build.
    #
    # ldd is probed, never required: without it, or on a platform whose loader
    # is not ELF, the case is simply not registered.  A C build never reaches
    # this block at all, so no configuration acquires a new tool because of it.
    if(ZLIB_BUILD_RUST
       AND UNIX
       AND NOT APPLE)
        find_program(LDD_EXECUTABLE ldd)

        if(LDD_EXECUTABLE)
            add_test(NAME zlib_rust_loader_binding
                     COMMAND ${LDD_EXECUTABLE} $<TARGET_FILE:zlib_example>)
            # The pass expression is what keeps the case from being vacuous: an
            # unresolved `libz.so.1 => not found' matches nothing and fails,
            # rather than passing because the fail expression found nothing to
            # object to.  The fail expression is the failure that was actually
            # observed while porting -- the SONAME resolving into a system
            # prefix -- and it is anchored to the resolved path so that a build
            # directory of any name is accepted.  Both are plain strings: a test
            # property is not generator-expression territory, which is also why
            # neither can name the staged directory.
            set_tests_properties(
                zlib_rust_loader_binding
                PROPERTIES PASS_REGULAR_EXPRESSION "libz\\.so\\.1 => /"
                           FAIL_REGULAR_EXPRESSION
                           "libz\\.so\\.1 => /(usr/)?lib")
        endif(LDD_EXECUTABLE)
    endif(
        ZLIB_BUILD_RUST
        AND UNIX
        AND NOT APPLE)
endif(ZLIB_BUILD_SHARED)

if(ZLIB_BUILD_STATIC)
    add_executable(zlib_static_example example.c)
    target_link_libraries(zlib_static_example ZLIB::ZLIBSTATIC)
    target_compile_definitions(
        zlib_static_example
        PRIVATE ZLIB_BUILD
                $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>)
    add_test(NAME zlib_static_example COMMAND zlib_static_example)

    add_executable(static_minigzip minigzip.c)
    target_link_libraries(static_minigzip ZLIB::ZLIBSTATIC)

    if(${CMAKE_C_COMPILER_ID} STREQUAL "GNU" OR ${CMAKE_C_COMPILER_ID} STREQUAL
                                                "Clang")
        set(CFLAGS_OLD ${CMAKE_C_FLAGS})
        set(CMAKE_C_FLAGS
            ""
            CACHE STRING "" FORCE)

        if(${CMAKE_C_COMPILER_ID} STREQUAL "GNU")
            find_program(GCOV_EXECUTABLE gcov)
        endif(${CMAKE_C_COMPILER_ID} STREQUAL "GNU")

        if(${CMAKE_C_COMPILER_ID} STREQUAL "Clang")
            set(llvm_names llvm_cov)
            list(APPEND llvm_names llvm-cov)

            foreach(ver RANGE 11 99)
                list(APPEND llvm_names llvm-cov-${ver})
            endforeach(ver RANGE 11 99)

            find_program(GCOV_EXECUTABLE NAMES ${llvm_names})
            set(llvm_option "gcov")
        endif(${CMAKE_C_COMPILER_ID} STREQUAL "Clang")

        # infcover.c is not an ordinary consumer of the public API: it defines
        # ZLIB_INTERNAL and includes the private inftrees.h and inflate.h
        # (L16-L18), fills every allocation with 0xa5 instead of zeroes (L87) so
        # that anything assuming zeroed memory is caught, detects leaks,
        # non-LIFO frees and rogue frees through the zalloc/zfree hooks it
        # injects, forces Z_MEM_ERROR at chosen points with mem_limit(), writes
        # DICT straight into ((struct inflate_state *)strm.state)->mode (L330),
        # reads sizeof(struct inflate_state) (L428) and calls inflate_table()
        # itself (L632, L636).  Three requirements on whichever library is
        # behind the alias follow from that, and they hold for the Rust library
        # as much as for the C one: inflate_table has to be a linkable global in
        # the archive, struct inflate_state has to be layout-compatible, and the
        # whole inflate and inflateBack surface this driver reaches has to be
        # present.
        #
        # Which is why this case, alone among the drivers, needs the STATIC
        # alias.  zlib.map's local: block hides inflate_table, but a version
        # script governs shared linking only, so the name is absent from libz.so
        # and present in libz.a -- and the Rust build reproduces that split
        # deliberately, supplying the symbol from a C shim for this exact
        # caller.  If a Rust-linked infcover ever fails to build, link or pass,
        # that is a defect to fix in crates/libz-rs-sys: it must not be disabled
        # or dropped from CTest here, and infcover.c must not be edited, because
        # this driver is the only live check that the injected allocator
        # contract is honoured.
        #
        # Its checks are assert()s, so they are live only where NDEBUG is not
        # defined.  This block clears CMAKE_C_FLAGS but not
        # CMAKE_C_FLAGS_<CONFIG>, and Release supplies -DNDEBUG, so a Release
        # run establishes that all six cover_* paths run to completion without
        # crashing while a Debug run additionally checks every assertion. `make
        # cover' compiles with -O3 and no -DNDEBUG, so it is always the stricter
        # of the two.
        add_executable(infcover infcover.c)
        target_link_libraries(infcover ZLIB::ZLIBSTATIC)
        target_compile_options(infcover PRIVATE -coverage)
        target_link_options(infcover PRIVATE -coverage)
        target_compile_definitions(
            infcover PRIVATE $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>)
        add_test(NAME zlib_coverage COMMAND infcover)
        set(INFCOVER_DIR ${CMAKE_CURRENT_BINARY_DIR}/CMakeFiles/infcover.dir)
        add_test(
            NAME zlib_coverage-summary
            COMMAND
                ${GCOV_EXECUTABLE} ${llvm_option}
                ${CMAKE_CURRENT_SOURCE_DIR}/infcover.c -o
                ${INFCOVER_DIR}/infcover.c.gcda)
        set_tests_properties(zlib_coverage-summary PROPERTIES DEPENDS
                                                              zlib-coverage)
        set(CMAKE_C_FLAGS
            ${CFLAGS_OLD}
            CACHE STRING "" FORCE)
    endif(${CMAKE_C_COMPILER_ID} STREQUAL "GNU" OR ${CMAKE_C_COMPILER_ID}
                                                   STREQUAL "Clang")

    if(HAVE_OFF64_T)
        add_executable(zlib_static_example64 example.c)
        target_compile_definitions(
            zlib_static_example64
            PRIVATE ZLIB_BUILD
                    $<$<BOOL:${HAVE___ATTR__VIS_HIDDEN}>:HAVE_HIDDEN>)
        target_link_libraries(zlib_static_example64 ZLIB::ZLIBSTATIC)
        add_test(NAME zlib_static_example64 COMMAND zlib_static_example64)
    endif(HAVE_OFF64_T)
endif(ZLIB_BUILD_STATIC)

# minigzip and static_minigzip are built above and, until now, never run: no
# add_test named either of them, so the one driver in this directory that
# exercises the gzFile layer as a program -- gzopen, gzwrite, gzread, gzclose
# and gzerror over a real file, rather than the in-memory calls example.c makes
# -- went untested by ctest in both implementations.  A round trip through the
# file format is what closes that: whatever the library does to the bytes on the
# way out it has to undo on the way back in, and the gzip container carries its
# own CRC-32 and length check, so a defect anywhere in that path either aborts
# the driver or shows up as a difference at the end.
#
# Four cases per driver rather than one, because minigzip removes its input --
# file_compress() unlinks the file it compressed and file_uncompress() unlinks
# the archive -- so the input has to be re-made on every run for the suite to be
# repeatable, and no single command can both stage a file and compress it.  They
# are chained with fixtures so that ctest orders them under `-j' and pulls the
# whole chain in when only one of them is selected, and each driver gets a
# working directory of its own so the two chains never touch the same file: the
# case that fails is then the step that broke, not a collision.
set(ZLIB_MINIGZIP_DRIVERS "")

if(ZLIB_BUILD_SHARED)
    list(APPEND ZLIB_MINIGZIP_DRIVERS minigzip)
endif(ZLIB_BUILD_SHARED)

if(ZLIB_BUILD_STATIC)
    list(APPEND ZLIB_MINIGZIP_DRIVERS static_minigzip)
endif(ZLIB_BUILD_STATIC)

# The corpus is a file the tree already carries, so nothing has to be generated
# and the expected result needs no separate copy: the same path is both the
# input and what the restored file is compared against.  zlib.h is ~100 kB of
# text, which compresses to roughly a quarter of that and is large enough to
# cycle gzwrite's and gzread's internal buffers many times over, and it is
# immutable, so the case cannot drift.  It is only ever read: the copy below is
# what minigzip consumes and removes.
set(ZLIB_MINIGZIP_INPUT ${zlib_SOURCE_DIR}/zlib.h)

foreach(driver IN LISTS ZLIB_MINIGZIP_DRIVERS)
    set(ZLIB_MINIGZIP_WORKDIR ${CMAKE_CURRENT_BINARY_DIR}/${driver}_roundtrip)
    file(MAKE_DIRECTORY ${ZLIB_MINIGZIP_WORKDIR})

    add_test(NAME zlib_${driver}_roundtrip_prepare
             COMMAND ${CMAKE_COMMAND} -E copy ${ZLIB_MINIGZIP_INPUT}
                     ${ZLIB_MINIGZIP_WORKDIR}/roundtrip.dat)
    add_test(
        NAME zlib_${driver}_roundtrip_compress
        COMMAND ${driver} roundtrip.dat
        WORKING_DIRECTORY ${ZLIB_MINIGZIP_WORKDIR})
    add_test(
        NAME zlib_${driver}_roundtrip_decompress
        COMMAND ${driver} -d roundtrip.dat.gz
        WORKING_DIRECTORY ${ZLIB_MINIGZIP_WORKDIR})
    add_test(
        NAME zlib_${driver}_roundtrip_verify
        COMMAND ${CMAKE_COMMAND} -E compare_files
                ${ZLIB_MINIGZIP_WORKDIR}/roundtrip.dat ${ZLIB_MINIGZIP_INPUT})

    set_tests_properties(zlib_${driver}_roundtrip_prepare
                         PROPERTIES FIXTURES_SETUP ${driver}_input)
    set_tests_properties(
        zlib_${driver}_roundtrip_compress
        PROPERTIES FIXTURES_REQUIRED ${driver}_input FIXTURES_SETUP
                   ${driver}_gz)
    set_tests_properties(
        zlib_${driver}_roundtrip_decompress
        PROPERTIES FIXTURES_REQUIRED ${driver}_gz FIXTURES_SETUP ${driver}_out)
    set_tests_properties(zlib_${driver}_roundtrip_verify
                         PROPERTIES FIXTURES_REQUIRED ${driver}_out)

    # The shared driver needs the library's directory on PATH on the platforms
    # where that is how a dynamic library is found, for the same reason
    # zlib_example does above.  The static driver does not, and neither do the
    # other two cases in the chain, which run CMake rather than the driver.
    if(MSVC
       OR MSYS
       OR MINGW
       OR CYGWIN)
        if(NOT driver STREQUAL static_minigzip)
            zlib_findtestenv(zlib_${driver}_roundtrip_compress)
            zlib_findtestenv(zlib_${driver}_roundtrip_decompress)
        endif(NOT driver STREQUAL static_minigzip)
    endif(
        MSVC
        OR MSYS
        OR MINGW
        OR CYGWIN)
endforeach(driver IN LISTS ZLIB_MINIGZIP_DRIVERS)

# Everything from here on consumes an installation: the fixture that produces
# one, the three find_package cases that resolve a package out of it, and the
# two add_subdirectory cases that are handed it as their install prefix.  With
# ZLIB_INSTALL OFF this project installs nothing, so none of them can pass --
# upstream they are registered all the same and four of them fail outright,
# taking their dependents with them, identically in C and in Rust mode -- and a
# case that cannot pass measures nothing while making a legitimate configuration
# look broken.  Registering them only when there is an installation to consume
# leaves every other configuration's set of cases exactly as it was,
# ZLIB_INSTALL being ON by default.
if(NOT ZLIB_INSTALL)
    return()
endif(NOT ZLIB_INSTALL)

add_test(
    NAME zlib_install
    COMMAND ${CMAKE_COMMAND} --install ${zlib_BINARY_DIR} --prefix
            ${CMAKE_CURRENT_BINARY_DIR}/test_install --config $<CONFIG>
    WORKING_DIRECTORY ${zlib_BINARY_DIR})

set_tests_properties(zlib_install PROPERTIES FIXTURES_SETUP zlib_install)

file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test)
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/find_package_no_components_test)
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/find_package_wrong_components_test)
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test)
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/find_package_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test/CMakeLists.txt @ONLY)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/find_package_no_components_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test/CMakeLists.txt @ONLY)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/find_package_wrong_components_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test/CMakeLists.txt @ONLY)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/add_subdirectory_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test/CMakeLists.txt @ONLY)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/add_subdirectory_exclude_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test/CMakeLists.txt
    @ONLY)

# CMAKE_GENERATOR_PLATFORM doesn't work in the if
set(GENERATOR ${CMAKE_GENERATOR_PLATFORM})

if(GENERATOR)
    set(PLATFORM "-A ${GENERATOR}")
endif(GENERATOR)
#
# findpackage_test
#
add_test(
    NAME zlib_find_package_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/findpackage_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install --fresh
        -G "${CMAKE_GENERATOR}" -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_test)

add_test(
    NAME zlib_find_package_build
    COMMAND ${CMAKE_COMMAND} --build . --config $<CONFIG>
    WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test_build)

add_test(
    NAME zlib_find_package_test
    COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
    WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test_build)

set_tests_properties(
    zlib_find_package_configure PROPERTIES FIXTURES_REQUIRED zlib_install
                                           FIXTURES_SETUP fp_config)

set_tests_properties(
    zlib_find_package_build PROPERTIES FIXTURES_REQUIRED fp_config
                                       FIXTURES_SETUP fp_build)

set_tests_properties(
    zlib_find_package_test PROPERTIES FIXTURES_REQUIRED fp_build ENVIRONMENT
                                      CTEST_OUTPUT_ON_FAILURE=1)

# The loader assertion above covers the build tree; this one covers the
# installed prefix, which is a different chain of names produced by a different
# set of rules -- install() copies the versioned library and its three links
# separately -- so a build that staged the chain correctly and installed it
# incompletely would satisfy the first case and not this one.  It asserts
# against the consumer the fixture directly above has just built out of that
# installation, which is the same thing a real consumer does:
# find_package(ZLIB), link, run.  The expressions are the ones the build-tree
# case uses and are explained there; the installed prefix lives under this build
# directory, so a SONAME resolving into a system prefix still fails.
#
# The path cannot come from a generator expression because the executable
# belongs to a nested project this build does not know the targets of, which is
# also why the case is registered only for a single-configuration generator:
# there the consumer's build directory holds the executable directly, whereas a
# multi-config generator puts it in a per-configuration subdirectory this file
# cannot name.  That is the same boundary the staged drop-in already documents.
if(ZLIB_BUILD_RUST
   AND ZLIB_BUILD_SHARED
   AND LDD_EXECUTABLE
   AND NOT IS_MULTI)
    add_test(
        NAME zlib_rust_loader_binding_installed
        COMMAND ${LDD_EXECUTABLE}
                ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test_build/test_example)
    set_tests_properties(
        zlib_rust_loader_binding_installed
        PROPERTIES FIXTURES_REQUIRED fp_build PASS_REGULAR_EXPRESSION
                   "libz\\.so\\.1 => /" FAIL_REGULAR_EXPRESSION
                   "libz\\.so\\.1 => /(usr/)?lib")
endif(
    ZLIB_BUILD_RUST
    AND ZLIB_BUILD_SHARED
    AND LDD_EXECUTABLE
    AND NOT IS_MULTI)

#
# findpackage_no_components_test
#
add_test(
    NAME zlib_find_package_no_components_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install --fresh
        -G "${CMAKE_GENERATOR}"
        -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test)

set_tests_properties(
    zlib_find_package_no_components_configure
        PROPERTIES FIXTURES_REQUIRED zlib_install)

if(NOT ZLIB_BUILD_SHARED OR NOT ZLIB_BUILD_STATIC)
    set_tests_properties(
        zlib_find_package_no_components_configure
            PROPERTIES WILL_FAIL TRUE)
endif(NOT ZLIB_BUILD_SHARED OR NOT ZLIB_BUILD_STATIC)

#
# findpackage_no_component_test
#
add_test(
    NAME zlib_find_package_wrong_components_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install --fresh
        -G "${CMAKE_GENERATOR}"
        -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test)

set_tests_properties(
    zlib_find_package_wrong_components_configure
        PROPERTIES FIXTURES_REQUIRED zlib_install
                   WILL_FAIL TRUE)

#
# add_subdirectory_test
#
# This fixture and the exclude one below forward only CMAKE_BUILD_TYPE,
# CMAKE_C_COMPILER, CMAKE_C_FLAGS and CMAKE_INSTALL_PREFIX, so the nested build
# re-configures zlib from source with ZLIB_BUILD_RUST at its default and
# exercises the C library even when the outer tree was configured with the
# option ON.  The three find_package fixtures above behave differently because
# they consume what was installed, which is the implementation the outer build
# selected.  Forwarding the option would change what these two cases have always
# tested and would make a pure consumption fixture require a Rust toolchain, so
# the difference is recorded here rather than removed.
add_test(
    NAME zlib_add_subdirectory_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install --fresh
        -G "${CMAKE_GENERATOR}" -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_test)

add_test(
    NAME zlib_add_subdirectory_build
    COMMAND ${CMAKE_COMMAND} --build . --config $<CONFIG>
    WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build)

add_test(
    NAME zlib_add_subdirectory_test
    COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
    WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build)

set_tests_properties(
    zlib_add_subdirectory_configure PROPERTIES FIXTURES_REQUIRED zlib_install
                                               FIXTURES_SETUP as_config)

set_tests_properties(
    zlib_add_subdirectory_build PROPERTIES FIXTURES_REQUIRED as_config
                                           FIXTURES_SETUP as_build)

set_tests_properties(
    zlib_add_subdirectory_test PROPERTIES FIXTURES_REQUIRED as_build
                                          ENVIRONMENT CTEST_OUTPUT_ON_FAILURE=1)

#
# add_subdirectory_exclude_test
#
add_test(
    NAME zlib_add_subdirectory_exclude_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install --fresh
        -G "${CMAKE_GENERATOR}" -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_test)

add_test(
    NAME zlib_add_subdirectory_exclude_build
    COMMAND ${CMAKE_COMMAND} --build . --config $<CONFIG>
    WORKING_DIRECTORY
        ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build)

add_test(
    NAME zlib_add_subdirectory_exclude_test
    COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
    WORKING_DIRECTORY
        ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build)

set_tests_properties(
    zlib_add_subdirectory_exclude_configure
    PROPERTIES FIXTURES_REQUIRED zlib_install FIXTURES_SETUP asx_config)

# asx_config, not as_config: this build belongs to the exclude fixture set up
# directly above, and requiring the other fixture's configure step is what let
# `ctest -j' start this build before its own configure had run -- reproducible
# in both C and Rust mode, and visible even serially as `ctest -R
# ^zlib_add_subdirectory_exclude_build$' pulling in the wrong prerequisite and
# reporting Not Run.
set_tests_properties(
    zlib_add_subdirectory_exclude_build PROPERTIES FIXTURES_REQUIRED asx_config
                                                   FIXTURES_SETUP asx_build)

set_tests_properties(
    zlib_add_subdirectory_exclude_test
    PROPERTIES FIXTURES_REQUIRED asx_build ENVIRONMENT
               CTEST_OUTPUT_ON_FAILURE=1)
