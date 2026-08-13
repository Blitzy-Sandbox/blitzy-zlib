# ============================================================================
# the CTest suite
# ============================================================================
#
# Every executable below links ZLIB::ZLIB or ZLIB::ZLIBSTATIC and nothing else,
# and the root CMakeLists.txt decides which implementation those two names
# denote: the C library by default, or -- with -DZLIB_BUILD_RUST=ON -- the
# memory-safe Rust library staged from crates/libz-rs-sys.  Re-pointing those two
# aliases is the mechanism by which this suite runs against either
# implementation: with the option ON, zlib_example, zlib_example64,
# zlib_static_example, zlib_static_example64 and zlib_coverage ARE the
# Rust-linked cases, as are the three find_package fixtures further down, which
# consume what was installed.  The two add_subdirectory fixtures are the one
# exception and say so at their own definitions.
#
# WHAT THAT DOES AND DOES NOT GIVE A READER, stated plainly because a case list
# that names no implementation is easy to over-read.  A configured build tree
# exercises exactly ONE implementation -- the one the option selected -- so this
# file registers no `_rust'-suffixed case beside its C counterpart and a single
# tree therefore never runs the two side by side.  Comparing them means
# configuring twice, into two build directories, and comparing the two ctest
# runs; or using the differential harness at crates/zlib-rs-differential, which
# is where side-by-side comparison actually lives, because it holds both
# implementations in one process and compares their output byte for byte.
#
# Two cases here ARE Rust-only and are registered conditionally rather than
# aliased: zlib_rust_loader_binding below, and zlib_rust_loader_binding_installed
# further down, which assert with ldd that the staged library is the one bound in
# the build tree and in the install tree respectively.  Neither has a C
# counterpart, because the failure they guard against -- a SONAME resolving to
# the system libz -- can only happen to the Rust build.  Diffing the generated
# CTestTestfile.cmake of a C configure against a ZLIB_BUILD_RUST=ON one shows
# exactly those two names as the whole difference in the case list.
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

# Registers one example.c driver with a private working directory and a private
# archive name.
#
# example.c writes exactly one file and never removes it: test_gzio() is handed
# `argc > 1 ? argv[1] : TESTFILE' (its L531), and TESTFILE is "foo.gz" on nearly
# every platform (its L21-L25).  Four executables are built from that one source
# -- zlib_example, zlib_example64, zlib_static_example and zlib_static_example64
# -- and until now all four ran with no argument in this directory's build tree,
# so all four wrote, read back and left behind the same test/foo.gz.  Serially
# that merely means each run overwrites the last; under `ctest -j' it is a race
# on a single path, and the driver's own gzopen-write / gzopen-read sequence is
# exactly the window in which another copy of the same program can truncate the
# file between them.  A failure of that kind names whichever driver lost, not the
# collision, which is the worst possible diagnostic for a suite whose job is to
# tell you which implementation broke.
#
# Both halves of the fix are deliberate rather than redundant.  The private
# directory stops the four from sharing a path at all, and it keeps the artifact
# beside the case that produced it instead of accumulating in the build root; the
# private archive name means that even if two cases were ever pointed at one
# directory again they still could not collide.  Passing the name through argv is
# what makes this possible without touching example.c, which must stay
# byte-identical -- the whole claim being that an existing consumer needs no
# edit.
#
# The target and the test share a name for all four cases, so `COMMAND
# ${testName}' resolves to that target's executable.  ZLIB_findTestEnv sets only
# ENVIRONMENT and is applied afterwards, so it neither conflicts with the working
# directory nor has to know about it.
function(ZLIB_addExampleTest testName)
    set(workDir ${CMAKE_CURRENT_BINARY_DIR}/${testName}_work)
    file(MAKE_DIRECTORY ${workDir})

    add_test(
        NAME ${testName}
        COMMAND ${testName} ${testName}.gz
        WORKING_DIRECTORY ${workDir})
endfunction()

# zlib_add_nested_test(<name> <working directory> <command> [<argument>...])
#
# add_test() for a case that runs a NESTED BUILD, with one difference that is the
# whole reason it exists: the command is given the null device on standard input,
# and so is everything it starts.
#
# WHY THAT MATTERS, measured rather than assumed.  The two add_subdirectory cases
# configure and build zlib from source, so in Rust mode their nested
# `cmake --build' runs cargo -- and before building anything cargo asks the
# compiler about the target with `rustc - --crate-name ___ --print=file-names',
# where `-' means rustc reads its program text from STANDARD INPUT.  ctest gives
# a test whatever standard input ctest itself has, every child inherits it, and
# under `ctest -j' that descriptor is shared: rustc then reads bytes another
# process wrote and fails with `unknown start of token' on `<anon>:1' whose text
# is somebody else's output.  It failed roughly one run in three.  RESOURCE_LOCK
# was tried first and did NOT fix it, because the problem is descriptor
# inheritance rather than concurrency -- one readable standard input anywhere
# above cargo is enough.  Closing it here fixes it at the cause.
#
# HOW.  A test property cannot redirect standard input, so the command is wrapped
# in a generated cmake script and run with `cmake -P': execute_process()'s
# INPUT_FILE gives the child the null device, and a child that inherits it can
# read nothing.  The script is written into the build tree at configure time, the
# same way the loader-binding assertion above is written, because it serves only
# the cases below and needs nothing from version control.  The command reaches it
# as a single semicolon-separated list argument, so a path containing a space
# survives the round trip; NUL and /dev/null are selected by platform, since
# Windows spells it differently.
function(zlib_add_nested_test testName workDir)
    set(runnerScript ${CMAKE_CURRENT_BINARY_DIR}/zlib_nested_test.cmake)

    if(WIN32)
        set(nullDevice NUL)
    else(WIN32)
        set(nullDevice /dev/null)
    endif(WIN32)

    # Rewritten on every configure rather than written once if absent: a script
    # generated by an earlier revision of this file would otherwise survive in an
    # existing build tree and keep running in preference to this one.
    file(
        WRITE ${runnerScript}
        "# Generated by test/CMakeLists.txt.  Runs one command with the null device on
# standard input, so that a nested cargo cannot inherit a readable one.
#
# Inputs, both as -D on the cmake -P command line:
#   ZLIB_NT_COMMAND  the command and its arguments, as a semicolon-separated list
#   ZLIB_NT_WORKDIR  the directory to run it in

if(NOT ZLIB_NT_COMMAND)
    message(FATAL_ERROR \"ZLIB_NT_COMMAND was not passed\")
endif(NOT ZLIB_NT_COMMAND)

# REQUIRED, and not boilerplate: `cmake -P' runs a script with policies unset, and
# with CMP0007 OLD every list command SILENTLY IGNORES empty elements -- which is
# precisely the element the walk below has to see.  Without this the length and the
# indices disagree with the list that was passed and the walk drops real arguments.
cmake_policy(SET CMP0007 NEW)

# `--config \$<CONFIG>' reaches this script as `--config' followed by an EMPTY
# element on a single-configuration generator, because this project sets
# CMAKE_BUILD_TYPE as an ordinary variable and the cache entry stays empty -- the
# sibling configure case shows the same thing as `-DCMAKE_BUILD_TYPE='.  An empty
# value is harmless to `cmake --install' and to `ctest -C', and is a USAGE ERROR
# for `cmake --build', which prints its help and exits 1.  So the flag is dropped
# together with its empty value, which is exactly what a build with no named
# configuration wants; other empty arguments are dropped as well, since no tool
# invoked here takes one meaningfully.  A non-empty configuration is passed
# through untouched.
set(zlib_nt_argv \"\")
list(LENGTH ZLIB_NT_COMMAND zlib_nt_count)
set(zlib_nt_index 0)

while(zlib_nt_index LESS zlib_nt_count)
    list(GET ZLIB_NT_COMMAND \${zlib_nt_index} zlib_nt_arg)
    math(EXPR zlib_nt_after \"\${zlib_nt_index} + 1\")
    set(zlib_nt_value \"\")

    if(zlib_nt_after LESS zlib_nt_count)
        list(GET ZLIB_NT_COMMAND \${zlib_nt_after} zlib_nt_value)
    endif(zlib_nt_after LESS zlib_nt_count)

    if((zlib_nt_arg STREQUAL \"--config\" OR zlib_nt_arg STREQUAL \"-C\")
       AND zlib_nt_value STREQUAL \"\"
       AND zlib_nt_after LESS zlib_nt_count)
        set(zlib_nt_index \${zlib_nt_after})
    elseif(NOT zlib_nt_arg STREQUAL \"\")
        list(APPEND zlib_nt_argv \"\${zlib_nt_arg}\")
    endif((zlib_nt_arg STREQUAL \"--config\" OR zlib_nt_arg STREQUAL \"-C\")
          AND zlib_nt_value STREQUAL \"\"
          AND zlib_nt_after LESS zlib_nt_count)

    math(EXPR zlib_nt_index \"\${zlib_nt_index} + 1\")
endwhile(zlib_nt_index LESS zlib_nt_count)

message(STATUS \"running \${zlib_nt_argv} in \${ZLIB_NT_WORKDIR}\")

execute_process(
    COMMAND \${zlib_nt_argv}
    WORKING_DIRECTORY \${ZLIB_NT_WORKDIR}
    INPUT_FILE ${nullDevice}
    RESULT_VARIABLE zlib_nt_result)

if(NOT zlib_nt_result EQUAL 0)
    message(FATAL_ERROR \"\${zlib_nt_argv} failed with \${zlib_nt_result}\")
endif(NOT zlib_nt_result EQUAL 0)
")

    add_test(
        NAME ${testName}
        COMMAND ${CMAKE_COMMAND} "-DZLIB_NT_COMMAND=${ARGN}"
                -DZLIB_NT_WORKDIR=${workDir} -P ${runnerScript})
endfunction()

if(ZLIB_BUILD_SHARED)
    add_executable(zlib_example example.c)
    target_link_libraries(zlib_example ZLIB::ZLIB)
    target_compile_definitions(zlib_example PRIVATE ZLIB_BUILD)
    zlib_addexampletest(zlib_example)

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
        zlib_addexampletest(zlib_example64)

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
    # Loader inspection is REQUIRED here, not probed.  The block is reached only
    # on the ELF platforms the Rust staging itself supports -- CMakeLists.txt
    # refuses to configure ZLIB_BUILD_RUST anywhere else -- and on every one of
    # them the loader can be asked what it would bind.  ldd is the convenient
    # spelling; setting LD_TRACE_LOADED_OBJECTS in the environment of the
    # executable is what ldd does internally and needs no tool at all, so it is
    # a mandatory fallback rather than an optional one.  A configuration without
    # ldd therefore still gets the check.  A C build never reaches this block,
    # so no C configuration acquires a requirement because of it.
    #
    # What is asserted is also stronger than a regular expression can express.
    # `libz.so.1 => /<anything>' passes for the system library under any prefix
    # that is not /lib or /usr/lib -- /lib64, /usr/lib64, an /opt install, a
    # conda environment -- so the check is a comparison of CANONICAL PATHS
    # against the exact artifact this build staged, performed by the script
    # below.  A test property cannot do that (no generator expressions, no
    # realpath), which is why the work moved into a script that a test property
    # can only invoke.
    if(ZLIB_BUILD_RUST
       AND UNIX
       AND NOT APPLE)
        find_program(LDD_EXECUTABLE NAMES ldd)

        # Written at configure time into the build tree rather than shipped as a
        # source file: it exists only to serve the two cases below, it needs
        # nothing from version control, and keeping it here keeps the whole
        # assertion readable in one place.
        set(ZLIB_LOADER_BINDING_SCRIPT
            ${CMAKE_CURRENT_BINARY_DIR}/zlib_loader_binding.cmake)
        file(
            WRITE ${ZLIB_LOADER_BINDING_SCRIPT}
            "# Generated by test/CMakeLists.txt.  Asserts that the dynamic loader binds a
# named executable to one exact shared object, by canonical path.
#
# Inputs, all as -D on the cmake -P command line:
#   ZLIB_LB_EXECUTABLE  the program to inspect, or empty to search a directory
#   ZLIB_LB_DIR         directory holding it, when the path is not known here
#   ZLIB_LB_NAME        its file name, used with ZLIB_LB_DIR
#   ZLIB_LB_CONFIG      build configuration, for a multi-config subdirectory
#   ZLIB_LB_SONAME      the DT_NEEDED name whose binding is asserted
#   ZLIB_LB_EXPECTED    the file that must be bound
#   ZLIB_LB_LDD         path to ldd, or empty to use LD_TRACE_LOADED_OBJECTS
#   ZLIB_LB_WHAT        a phrase naming the case, for the messages

if(ZLIB_LB_EXECUTABLE STREQUAL \"\")
    # A multi-config generator puts the executable in a per-configuration
    # subdirectory, a single-config one puts it in the directory itself.  Trying
    # both is what lets the installed case run under every generator instead of
    # being skipped on the ones this file cannot name a path for.
    set(_candidates \"\${ZLIB_LB_DIR}/\${ZLIB_LB_NAME}\")

    if(NOT ZLIB_LB_CONFIG STREQUAL \"\")
        list(APPEND _candidates \"\${ZLIB_LB_DIR}/\${ZLIB_LB_CONFIG}/\${ZLIB_LB_NAME}\")
    endif()

    foreach(_candidate IN LISTS _candidates)
        if(EXISTS \"\${_candidate}\")
            set(ZLIB_LB_EXECUTABLE \"\${_candidate}\")
            break()
        endif()
    endforeach()

    if(ZLIB_LB_EXECUTABLE STREQUAL \"\")
        message(FATAL_ERROR \"\${ZLIB_LB_WHAT}: none of \${_candidates} exists, so \"
                            \"the loader could not be asked what it binds\")
    endif()
endif()

if(NOT EXISTS \"\${ZLIB_LB_EXPECTED}\")
    message(FATAL_ERROR \"\${ZLIB_LB_WHAT}: the artifact that must be bound, \"
                        \"\${ZLIB_LB_EXPECTED}, does not exist\")
endif()

# Ask ldd first, then the loader itself.  LD_TRACE_LOADED_OBJECTS=1 makes the
# program's own interpreter print the map and exit without running main, which is
# precisely what ldd arranges, so the two produce the same lines.
set(_trace \"\")
set(_how \"\")

if(NOT ZLIB_LB_LDD STREQUAL \"\")
    execute_process(
        COMMAND \"\${ZLIB_LB_LDD}\" \"\${ZLIB_LB_EXECUTABLE}\"
        RESULT_VARIABLE _rc
        OUTPUT_VARIABLE _out
        ERROR_VARIABLE _err)

    if(_rc EQUAL 0)
        set(_trace \"\${_out}\")
        set(_how \"ldd\")
    else()
        set(_ldd_failure \"ldd exited \${_rc}: \${_err}\")
    endif()
endif()

if(_trace STREQUAL \"\")
    execute_process(
        COMMAND \${CMAKE_COMMAND} -E env LD_TRACE_LOADED_OBJECTS=1
                \"\${ZLIB_LB_EXECUTABLE}\"
        RESULT_VARIABLE _rc
        OUTPUT_VARIABLE _out
        ERROR_VARIABLE _err)

    if(_rc EQUAL 0)
        set(_trace \"\${_out}\")
        set(_how \"LD_TRACE_LOADED_OBJECTS\")
    else()
        message(FATAL_ERROR
                \"\${ZLIB_LB_WHAT}: could not read the loader map of \"
                \"\${ZLIB_LB_EXECUTABLE}.  \${_ldd_failure} \"
                \"LD_TRACE_LOADED_OBJECTS exited \${_rc}: \${_err}\")
    endif()
endif()

# `<soname> => <path> (0x...)' on glibc; the address is optional and musl omits
# it.  `not found' is matched by the same expression and rejected below, which is
# what keeps an unresolved dependency from reading as an absent line.
string(REGEX REPLACE \"\\r?\\n\" \";\" _lines \"\${_trace}\")
set(_resolved \"\")

foreach(_line IN LISTS _lines)
    if(_line MATCHES \"(^|[ \\t/])\${ZLIB_LB_SONAME} *=> *([^ \\t]+)\")
        set(_resolved \"\${CMAKE_MATCH_2}\")
        break()
    endif()
endforeach()

if(_resolved STREQUAL \"\")
    message(FATAL_ERROR
            \"\${ZLIB_LB_WHAT}: \${ZLIB_LB_EXECUTABLE} records no dependency on \"
            \"\${ZLIB_LB_SONAME} at all, so nothing was verified.  Loader map \"
            \"(via \${_how}):\\n\${_trace}\")
endif()

if(_resolved STREQUAL \"not\")
    message(FATAL_ERROR
            \"\${ZLIB_LB_WHAT}: \${ZLIB_LB_SONAME} is not found by the loader for \"
            \"\${ZLIB_LB_EXECUTABLE}.  Loader map (via \${_how}):\\n\${_trace}\")
endif()

# Canonical on both sides.  The staged and installed layout is one real file,
# libz.so.<version>, with libz.so.1 and libz.so as two symlinks pointing directly
# at it, so the loader reports whichever of those three names the executable
# recorded and this file may well name a different one.  Comparing the two paths
# as written would therefore fail on a correct install; canonicalising both and
# comparing the real file they share is what settles whether the right library
# was bound.
get_filename_component(_resolved_real \"\${_resolved}\" REALPATH)
get_filename_component(_expected_real \"\${ZLIB_LB_EXPECTED}\" REALPATH)

if(NOT _resolved_real STREQUAL _expected_real)
    message(FATAL_ERROR
            \"\${ZLIB_LB_WHAT}: \${ZLIB_LB_EXECUTABLE} binds \${ZLIB_LB_SONAME} to \"
            \"\${_resolved} (really \${_resolved_real}), not to the artifact this \"
            \"build produced, \${ZLIB_LB_EXPECTED} (really \${_expected_real}).  A \"
            \"drop-in test that ran against a different library would report \"
            \"success while proving nothing.  Loader map (via \${_how}):\\n\${_trace}\")
endif()

message(STATUS \"\${ZLIB_LB_WHAT}: \${ZLIB_LB_SONAME} binds to \${_expected_real} \"
               \"(via \${_how})\")
")

        # The build-tree case.  $<TARGET_FILE:> is available because the target
        # belongs to this project; the expected artifact is the versioned file
        # the staging rules produced, which the parent scope names.
        add_test(
            NAME zlib_rust_loader_binding
            COMMAND
                ${CMAKE_COMMAND}
                -DZLIB_LB_EXECUTABLE=$<TARGET_FILE:zlib_example> -DZLIB_LB_DIR=
                -DZLIB_LB_NAME= -DZLIB_LB_CONFIG=$<CONFIG>
                -DZLIB_LB_SONAME=${ZLIB_RUST_SONAME}
                -DZLIB_LB_EXPECTED=${ZLIB_RUST_LIBRARY}
                -DZLIB_LB_LDD=${LDD_EXECUTABLE}
                "-DZLIB_LB_WHAT=build-tree loader binding" -P
                ${ZLIB_LOADER_BINDING_SCRIPT})
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
    zlib_addexampletest(zlib_static_example)

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
        # zlib_coverage, with an underscore: the test registered directly
        # above is named zlib_coverage, and `zlib-coverage' with a hyphen is no
        # test at all.  CTest does not object to a DEPENDS naming something that
        # does not exist -- it simply has nothing to order against -- so the
        # dependency has been silently void, and this is the only DEPENDS in the
        # suite that failed to resolve.  It matters under `ctest -j', where
        # nothing then stops gcov from running before infcover has produced
        # infcover.c.gcda and reporting on a coverage file that is absent or
        # left over from an earlier run.
        set_tests_properties(zlib_coverage-summary PROPERTIES DEPENDS
                                                              zlib_coverage)
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
        zlib_addexampletest(zlib_static_example64)
    endif(HAVE_OFF64_T)
endif(ZLIB_BUILD_STATIC)

# minigzip and static_minigzip are built above.  Building a driver is not running
# it, and for a long time no add_test named either of them, so the one driver in
# this directory that
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

# The remaining cases come in two kinds, and the difference decides which of
# them a ZLIB_INSTALL=OFF configuration can still run.
#
# * The install consumers CONSUME AN INSTALLATION: they resolve a package, a
#   library or a descriptor out of a prefix this suite installs into.  With
#   ZLIB_INSTALL OFF this project installs nothing, so none of them can pass --
#   upstream they are registered all the same and fail outright, taking their
#   dependents with them, identically in C and in Rust mode -- and a case that
#   cannot pass measures nothing while making a legitimate configuration look
#   broken.  They are therefore registered only when there is an installation to
#   consume, and ONE if(ZLIB_INSTALL) below encloses ALL of them: the install
#   fixture itself, the three find_package cases, the installed loader-binding
#   assertion, and the three-step static-link-closure chain that reads the
#   installed libz.a and the installed zlib.pc.  A previous revision closed that
#   guard immediately after the install fixture and left a second, unmatched
#   `endif' at the end of the block, which made the whole file unparseable; when
#   it was made to parse by deleting the stray line instead, every consumer
#   escaped the guard.  Keep it as one block: if a case reads
#   ${CMAKE_CURRENT_BINARY_DIR}/test_install, it belongs inside.
# * The two add_subdirectory cases CONSUME THE SOURCE.  They re-configure zlib
#   from this very tree inside a nested project and link what that produces;
#   they never read the install prefix.  They used to sit behind the same guard
#   and to carry FIXTURES_REQUIRED zlib_install, which made a -DZLIB_INSTALL=OFF
#   build silently stop testing source consumption altogether -- the one form of
#   consumption that has nothing to do with installing.  They are now registered
#   unconditionally, and the install fixture is not among their prerequisites.
#
# ZLIB_INSTALL is ON by default, so every other configuration's set of cases is
# exactly what it was.
#
# ★ WHAT THE TWO KINDS SHARE HAS TO BE PRODUCED OUTSIDE THE GUARD, and this is the
# half that registering the cases unconditionally did not finish.  Deciding that the
# add_subdirectory cases do not need an installation is only half a fix if the files
# they read are still written inside `if(ZLIB_INSTALL)': with -DZLIB_INSTALL=OFF the
# two nested CMakeLists.txt were never configured, so `-S .../add_subdirectory_test'
# named a directory that did not exist and both cases failed at configure time,
# taking their build and test dependents with them.  MEASURED before this change:
# `cmake -DZLIB_INSTALL=OFF' configured cleanly and then
# `ctest -R zlib_add_subdirectory_.*configure' reported 0% of 2 passing.
#
# So everything the source-consumption cases need is produced here, before the guard,
# and the guard keeps only what genuinely depends on there being an installation.
# `PLATFORM' comes with them because every nested `cmake' invocation in this file
# interpolates it, including the two below the guard -- on a generator with
# CMAKE_GENERATOR_PLATFORM set, leaving it inside would have quietly dropped `-A' from
# the source-consumption cases in a ZLIB_INSTALL=OFF build and configured them for the
# wrong architecture.

# CMAKE_GENERATOR_PLATFORM doesn't work in the if
set(GENERATOR ${CMAKE_GENERATOR_PLATFORM})

if(GENERATOR)
    set(PLATFORM "-A ${GENERATOR}")
endif(GENERATOR)

# The two source-consumption fixtures.  Read by the `zlib_add_subdirectory_*' and
# `zlib_add_subdirectory_exclude_*' cases below, which are registered
# unconditionally, so these have to be written unconditionally too.  configure_file
# creates a missing parent itself, but the explicit MAKE_DIRECTORY is kept for the
# reason the note inside the guard gives: it is what makes a name mismatch between a
# directory and its destination visible instead of silent.
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test)
file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/add_subdirectory_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test/CMakeLists.txt @ONLY)

configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/add_subdirectory_exclude_test.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test/CMakeLists.txt @ONLY)

if(ZLIB_INSTALL)
    add_test(
        NAME zlib_install
        COMMAND ${CMAKE_COMMAND} --install ${zlib_BINARY_DIR} --prefix
                ${CMAKE_CURRENT_BINARY_DIR}/test_install --config $<CONFIG>
        WORKING_DIRECTORY ${zlib_BINARY_DIR})

    set_tests_properties(zlib_install PROPERTIES FIXTURES_SETUP zlib_install)

    # One directory per configured fixture, and every name here must match the
    # configure_file destination below: the two `find_package_*' spellings named
    # directories that nothing ever wrote to, while the fixtures themselves went to
    # `findpackage_*'.  configure_file creates a missing parent itself, so the
    # mismatch was harmless and therefore silent -- and it is exactly the sort of
    # near-miss that makes the F12 defect above easy to introduce.
    file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test)
    file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test)
    file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test)

    configure_file(
        ${CMAKE_CURRENT_SOURCE_DIR}/find_package_test.cmake.in
        ${CMAKE_CURRENT_BINARY_DIR}/findpackage_test/CMakeLists.txt @ONLY)

    configure_file(
        ${CMAKE_CURRENT_SOURCE_DIR}/find_package_no_components_test.cmake.in
        ${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test/CMakeLists.txt @ONLY)

    configure_file(
        ${CMAKE_CURRENT_SOURCE_DIR}/find_package_wrong_components_test.cmake.in
        ${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test/CMakeLists.txt @ONLY)

    # The two add_subdirectory fixtures and the PLATFORM derivation used to live here
    # and are now produced above the guard, because the cases that read them are
    # registered unconditionally.  Nothing else moved: every remaining producer in
    # this block writes a fixture that resolves an installation.

    # ★ Nested cmake invocations are run with a standard input that carries nothing.
    #
    # The two add_subdirectory fixtures below build zlib from source, so in Rust
    # mode their nested `cmake --build' runs cargo -- and before building anything
    # cargo asks the compiler about the target by running `rustc - --crate-name ___
    # --print=file-names ...', where that `-' means rustc reads its program text
    # from STANDARD INPUT.  When that probe had to run inside ctest, those two cases
    # failed roughly one run in three:
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
    # find_package(ZLIB), link, run.
    #
    # Two things that used to narrow this case are gone.  It is no longer
    # conditional on ldd, because the script it runs falls back to the loader's own
    # LD_TRACE_LOADED_OBJECTS and so needs no tool; and it is no longer restricted
    # to a single-configuration generator.  $<TARGET_FILE:> is still unavailable --
    # the executable belongs to a nested project whose targets this build does not
    # know -- but that only means the path is not known HERE, and the script is
    # given the directory, the file name and the configuration and finds it under
    # either layout.  Skipping the installed check on a multi-config generator meant
    # the one configuration most likely to get the install rules wrong was the one
    # that never checked them.
    #
    # The expected artifact is the versioned file inside the test prefix, which is
    # where the Rust install rules put the real library with libz.so.1 and libz.so
    # pointing at it.  Comparing canonical paths is what makes the case meaningful:
    # the installed prefix is under this build directory, but so would be a stray
    # copy, and only the exact file settles it.
    if(ZLIB_BUILD_RUST AND ZLIB_BUILD_SHARED)
        add_test(
            NAME zlib_rust_loader_binding_installed
            COMMAND
                ${CMAKE_COMMAND} -DZLIB_LB_EXECUTABLE=
                -DZLIB_LB_DIR=${CMAKE_CURRENT_BINARY_DIR}/findpackage_test_build
                -DZLIB_LB_NAME=test_example -DZLIB_LB_CONFIG=$<CONFIG>
                -DZLIB_LB_SONAME=${ZLIB_RUST_SONAME}
                -DZLIB_LB_EXPECTED=${CMAKE_CURRENT_BINARY_DIR}/test_install/${CMAKE_INSTALL_LIBDIR}/${ZLIB_RUST_SHARED_LIBV}
                -DZLIB_LB_LDD=${LDD_EXECUTABLE}
                "-DZLIB_LB_WHAT=installed loader binding" -P
                ${ZLIB_LOADER_BINDING_SCRIPT})
        set_tests_properties(zlib_rust_loader_binding_installed
                             PROPERTIES FIXTURES_REQUIRED fp_build)
    endif(ZLIB_BUILD_RUST AND ZLIB_BUILD_SHARED)

    #
    # static_link_closure_test
    #
    # Static consumption of the installed Rust libz.a with -nodefaultlibs, so that the
    # native runtime libraries the archive needs have to come from
    # ZLIB::ZLIBSTATIC's own interface rather than from whatever the compiler driver
    # and a modern libc happen to contribute.  zlib_test_example_static above is the
    # plain link and would stay green with the advertisement removed entirely, which
    # is precisely why this case exists beside it rather than instead of it.  The
    # nested project's own comment carries the full argument.
    #
    # Registered only for the Rust build: the C libz.a needs no companion libraries,
    # so the same link would fail there on printf rather than on anything zlib
    # advertises, and ZLIB_BUILD_STATIC is required because the case consumes the
    # static component.  Single-configuration generators only, for the reason the
    # installed loader assertion above gives -- a multi-config generator puts the
    # consumer's executable in a per-configuration subdirectory this file cannot
    # name, and CTest here runs the nested build's own test list rather than a path.
    if(ZLIB_BUILD_RUST AND ZLIB_BUILD_STATIC AND NOT IS_MULTI)
        file(MAKE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test)

        configure_file(
            ${CMAKE_CURRENT_SOURCE_DIR}/static_link_closure_test.cmake.in
            ${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test/CMakeLists.txt
            @ONLY)

        # The check script is copied beside the generated project rather than
        # referenced in the source tree, so the nested build reads it from the same
        # place it reads its own CMakeLists.txt and needs no path back into the
        # checkout.
        configure_file(
            ${CMAKE_CURRENT_SOURCE_DIR}/check_pc_private.cmake
            ${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test/check_pc_private.cmake
            COPYONLY)

        add_test(
            NAME zlib_static_link_closure_configure
            COMMAND
                ${CMAKE_COMMAND} ${PLATFORM}
                -B${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test_build
                -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
                -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
                -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install
                --fresh -G "${CMAKE_GENERATOR}"
                -S${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test)

        add_test(
            NAME zlib_static_link_closure_build
            COMMAND ${CMAKE_COMMAND} --build . --config $<CONFIG>
            WORKING_DIRECTORY
                ${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test_build)

        add_test(
            NAME zlib_static_link_closure_test
            COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
            WORKING_DIRECTORY
                ${CMAKE_CURRENT_BINARY_DIR}/static_link_closure_test_build)

        set_tests_properties(
            zlib_static_link_closure_configure
            PROPERTIES FIXTURES_REQUIRED zlib_install FIXTURES_SETUP slc_config)

        set_tests_properties(
            zlib_static_link_closure_build
            PROPERTIES FIXTURES_REQUIRED slc_config FIXTURES_SETUP slc_build)

        set_tests_properties(
            zlib_static_link_closure_test
            PROPERTIES FIXTURES_REQUIRED slc_build ENVIRONMENT
                       CTEST_OUTPUT_ON_FAILURE=1)
    endif(ZLIB_BUILD_RUST AND ZLIB_BUILD_STATIC AND NOT IS_MULTI)

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
                -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install
                --fresh -G "${CMAKE_GENERATOR}"
                -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_no_components_test)

        set_tests_properties(zlib_find_package_no_components_configure
                             PROPERTIES FIXTURES_REQUIRED zlib_install)

        if(NOT ZLIB_BUILD_SHARED OR NOT ZLIB_BUILD_STATIC)
            set_tests_properties(zlib_find_package_no_components_configure
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
                -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install
                --fresh -G "${CMAKE_GENERATOR}"
                -S${CMAKE_CURRENT_BINARY_DIR}/findpackage_wrong_components_test)

        set_tests_properties(
            zlib_find_package_wrong_components_configure
            PROPERTIES FIXTURES_REQUIRED zlib_install WILL_FAIL TRUE)
endif(ZLIB_INSTALL)

#
# add_subdirectory_test
#
# -S names add_subdirectory_test, which is what makes this case an
# add_subdirectory case at all.
#
# Both this fixture and the exclude one below used to point -S at
# findpackage_test.  The consequence was not a small one: the two CMakeLists.txt
# files configured from add_subdirectory_test.cmake.in and
# add_subdirectory_exclude_test.cmake.in were written into the build tree and
# then never read by anything, while these six cases silently re-ran the
# find_package fixture a further two times.  `add_subdirectory' consumption --
# the mode in which a project vendors this tree and builds it as part of its own
# build, with no install step and no package config -- had therefore never been
# exercised, in either implementation, and neither had EXCLUDE_FROM_ALL.  The
# defect was invisible because a duplicate of a passing case passes: the give-away
# was CMAKE_HOME_DIRECTORY in add_subdirectory_test_build/CMakeCache.txt naming
# findpackage_test.
#
# ZLIB_BUILD_RUST is forwarded, together with the cargo path and SIMD selection
# the root CMakeLists.txt needs to act on it.  These two cases build zlib from
# source rather than consuming an installation, so unless the option travels with
# them the nested build takes it at its default and compiles the C library --
# which would mean that in Rust mode the one consumption path that builds from
# source is the one path never testing the Rust library.  AAP 0.8.3 requires
# ZLIB::ZLIB and ZLIB::ZLIBSTATIC to resolve to the Rust artifacts whenever the
# Rust build is selected, and these fixtures are named there as the integration
# fixtures that must keep passing.  Forwarding costs nothing in C mode, where the
# variables are OFF and empty, and in Rust mode the outer build has already
# required the same toolchain, so no configuration acquires a new prerequisite.
add_test(
    NAME zlib_add_subdirectory_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install
        -DZLIB_BUILD_RUST=${ZLIB_BUILD_RUST}
        -DZLIB_CARGO_EXECUTABLE=${ZLIB_CARGO_EXECUTABLE}
        -DZLIB_RS_SIMD=${ZLIB_RS_SIMD} --fresh -G "${CMAKE_GENERATOR}"
        -S${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test)

zlib_add_nested_test(
    zlib_add_subdirectory_build
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build
    ${CMAKE_COMMAND}
    --build
    .
    --config
    $<CONFIG>)

add_test(
    NAME zlib_add_subdirectory_test
    COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
    WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_test_build)

# RESOURCE_LOCK on the configure and build steps of BOTH add_subdirectory
# fixtures, so that the two never run at the same time under `ctest -j'.
#
# This is a resource decision and nothing more.  These two cases now build zlib
# from source, so in Rust mode each one runs a full release cargo build of the
# workspace; two of those concurrently doubles the peak memory and compiler
# parallelism on the machine and makes both slower and their durations
# unpredictable, which matters because the build step is the longest test in
# this file.  Serialising them costs nothing that `ctest -j' can otherwise use,
# since cargo already saturates the machine on its own.
#
# It is NOT what makes these tests reliable.  The intermittent failures seen
# here under `ctest -j' -- rustc reporting `unknown start of token' on
# `<anon>:1' whose text was another process's output -- came from cargo's `rustc
# -' target probe reading an inherited standard input, and RESOURCE_LOCK was
# tried against them first and did not stop them: the problem is inheritance
# rather than concurrency, so one readable standard input anywhere above cargo
# is enough.  What fixes it is zlib_add_nested_test, which runs each nested
# cmake through a script that gives the whole subtree the null device on
# standard input; see the note on that function above.  The name is arbitrary
# and shared; nothing else in this file claims it.
set_tests_properties(zlib_add_subdirectory_configure
                     PROPERTIES FIXTURES_SETUP as_config RESOURCE_LOCK cargo)

set_tests_properties(
    zlib_add_subdirectory_build
    PROPERTIES FIXTURES_REQUIRED as_config FIXTURES_SETUP as_build
               RESOURCE_LOCK cargo)

set_tests_properties(
    zlib_add_subdirectory_test PROPERTIES FIXTURES_REQUIRED as_build
                                          ENVIRONMENT CTEST_OUTPUT_ON_FAILURE=1)

#
# add_subdirectory_exclude_test
#
# -S names add_subdirectory_exclude_test, for the reason recorded at the case
# above, and this is the only case anywhere in the suite that exercises
# EXCLUDE_FROM_ALL: a vendoring project that does not want zlib's own targets in
# its default build still has to be able to link them.  The same variables are
# forwarded, so in Rust mode the excluded subdirectory builds the Rust library.
add_test(
    NAME zlib_add_subdirectory_exclude_configure
    COMMAND
        ${CMAKE_COMMAND} ${PLATFORM}
        -B${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build
        -DCMAKE_BUILD_TYPE=$<CONFIG> -DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}
        -DCMAKE_C_FLAGS=${CMAKE_C_FLAGS}
        -DCMAKE_INSTALL_PREFIX=${CMAKE_CURRENT_BINARY_DIR}/test_install
        -DZLIB_BUILD_RUST=${ZLIB_BUILD_RUST}
        -DZLIB_CARGO_EXECUTABLE=${ZLIB_CARGO_EXECUTABLE}
        -DZLIB_RS_SIMD=${ZLIB_RS_SIMD} --fresh -G "${CMAKE_GENERATOR}"
        -S${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test)

zlib_add_nested_test(
    zlib_add_subdirectory_exclude_build
    ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build
    ${CMAKE_COMMAND}
    --build
    .
    --config
    $<CONFIG>)

add_test(
    NAME zlib_add_subdirectory_exclude_test
    COMMAND ${CMAKE_CTEST_COMMAND} -C $<CONFIG>
    WORKING_DIRECTORY
        ${CMAKE_CURRENT_BINARY_DIR}/add_subdirectory_exclude_test_build)

set_tests_properties(zlib_add_subdirectory_exclude_configure
                     PROPERTIES FIXTURES_SETUP asx_config RESOURCE_LOCK cargo)

# asx_config, not as_config: this build belongs to the exclude fixture set up
# directly above, and requiring the other fixture's configure step is what let
# `ctest -j' start this build before its own configure had run -- reproducible
# in both C and Rust mode, and visible even serially as `ctest -R
# ^zlib_add_subdirectory_exclude_build$' pulling in the wrong prerequisite and
# reporting Not Run.
set_tests_properties(
    zlib_add_subdirectory_exclude_build
    PROPERTIES FIXTURES_REQUIRED asx_config FIXTURES_SETUP asx_build
               RESOURCE_LOCK cargo)

set_tests_properties(
    zlib_add_subdirectory_exclude_test
    PROPERTIES FIXTURES_REQUIRED asx_build ENVIRONMENT
               CTEST_OUTPUT_ON_FAILURE=1)
