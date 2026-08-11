# For building with cmake at least version 3.12 (minizip 3.12) is needed

In most cases the usual

    cmake -S . -B build -D CMAKE_BUILD_TYPE=Release

will create everything you need, however if you want something off default you can adjust several options fit your needs.
Every option is list below (excluding the cmake-standard options), they can be set via cmake-gui or on cmdline with

    -D<option>=ON/OFF

## ZLIB-options with defaults ##

    ZLIB_BUILD_TESTING=ON -- Enable Zlib Examples as tests

    ZLIB_BUILD_SHARED=ON -- Enable building zlib shared library

    ZLIB_BUILD_STATIC=ON -- Enable building zlib static library

    ZLIB_BUILD_RUST=OFF -- Use the memory-safe Rust libz for the exported ZLIB::* targets

With this option on, the exported ZLIB::ZLIB and ZLIB::ZLIBSTATIC targets are built by cargo from the
Rust workspace at the top of this tree (crates/libz-rs-sys) instead of from the C sources. The shared
libz.so it stages presents the same ABI as the C one -- the same SONAME libz.so.1 and the same 111
exported symbols at the same versions -- so a program that dynamically links zlib keeps working without
being recompiled. That is verified on x86_64-unknown-linux-gnu. Static consumption needs one thing the C
build does not, and this build path supplies it rather than leaving it to the consumer: a Rust libz.a
carries the Rust runtime's own companion libraries, so configuration derives that target's set and
publishes it in the installed zlib.pc as `Libs.private:` and on the exported ZLIB::ZLIBSTATIC target.
See "One thing the Rust build has to advertise" below for what that means for a static link command.
cargo has to be reachable on PATH and configuration stops
with a message telling you so when it is not, or -DZLIB_CARGO_EXECUTABLE=/path/to/cargo names a
particular one. This path needs Rust 1.80 or newer, which cargo enforces itself from
rust-toolchain.toml and the crate manifests: the build runs
`cargo build --package libz-rs-sys`, so it compiles only the two crates that ship. All three
workspace members declare the same 1.80 floor, including the dev-only
`crates/zlib-rs-differential` harness that hosts the C oracle, so no CMake configuration can reach
a higher one. The only higher floor in the repository is criterion's 1.86, and it belongs to the
excluded `benches/` package, which no CMake target builds and which is reached only by
`cargo bench --manifest-path benches/Cargo.toml`. The C targets zlib and zlibstatic are still defined and
still built, as they are the reference the Rust code is diffed against, so this option adds a library
rather than taking one away. It is off by default, and while it is off no Rust tool is probed and
everything here behaves exactly as it always has. The Rust library is supported on Rust's tier 1
targets, and this option additionally wants a linker that accepts the -soname and --version-script
flags, the same platform set that gets zlib.map for the C shared library.

With ZLIB_INSTALL on as well, installing publishes the same names, the same SONAME and the same symlink
topology the C build does: the real file libz.so.1.3.2.1-motley, named from ZLIB_VERSION in zlib.h, with
libz.so.1, libz.so and the versioned name the C library itself publishes all pointing at it, and the
static libz.a beside them. Those links are a correctness requirement rather than packaging polish, as
libz.so.1 is the SONAME the loader searches for and a directory holding libz.so alone leaves it free to
go on searching and bind the system libz instead, silently.

One thing the Rust build has to advertise that the C build does not: a Rust static library carries the
Rust standard library's own references, and a static archive cannot record a dependency the way a shared
object records DT_NEEDED. So with this option on, configuration derives the target's native runtime
libraries with `rustc --crate-type staticlib --print native-static-libs` and publishes that one answer
through the two channels a consumer can read: as the single `Libs.private:` key in the generated zlib.pc,
which `pkg-config --static --libs zlib` reports, and as the interface link libraries of the exported
ZLIB::ZLIBSTATIC target, which a `find_package(ZLIB CONFIG)` consumer gets without pkg-config at all. A
consumer that reads either one keeps writing the link command it always wrote; one that hardcodes `-lz`
and reads neither has to add the printed set itself.

The set is target-specific and is never hardcoded, and this path is FAIL-CLOSED: if rustc cannot be
reached, or answers with no set at all, configuration stops with a FATAL_ERROR naming the remedy --
either -DZLIB_RUSTC_EXECUTABLE=/path/to/rustc, or -DZLIB_RUST_NATIVE_STATIC_LIBS="<the set for your
target>" to state the answer yourself as a cross build must, or -DZLIB_BUILD_STATIC=OFF to publish no
archive. That second form takes the same `-l` list rustc prints, and there is deliberately no example
value here: on x86_64-linux-gnu it is currently seven libraries including -lgcc_s, and a plausible-looking
short list such as `-lm -ldl -lc` configures and builds and then fails at link time with an undefined
reference to `_Unwind_Resume`. Ask the compiler for the target you are building --
`rustc --print native-static-libs --crate-type staticlib [--target <triple>]` -- and pass what it answers.
Installing a static library whose link requirements are unknown would be worse than not installing one,
because the failure would surface in a consumer's build with nothing to connect it to this one. Shared
consumers are unaffected either way, and a C-mode build never reaches any of it. `ctest -R
static_link_closure` is the case that holds this to account, and it links with -nodefaultlibs on purpose --
a plain link succeeds on a modern glibc whether the advertisement is there or not, so it is also what
catches an incomplete set stated by hand.

README and rust/README.md cover the Rust build in full.

    ZLIB_BUILD_MINIZIP=ON -- Enable building libminizip contrib library

If this option is turned on, additional options are available from minizip (see below)

    ZLIB_INSTALL=ON -- Enable installation of zlib

    ZLIB_PREFIX=OFF -- prefix for all types and library functions, see zconf.h.in

This option is only on windows available and may/will be turned off and removed somewhen in the future.
If you rely cmake for finding and using zlib, this can be turned off, as `zlib1.dll` will never be used.

## minizip-options with defaults ##

    MINIZIP_BUILD_SHARED=ON -- Enable building minizip shared library

    MINIZIP_BUILD_STATIC=ON -- Enable building minizip static library

    MINIZIP_BUILD_TESTING=ON -- Enable testing of minizip

    MINIZIP_ENABLE_BZIP2=ON -- Build minizip withj bzip2 support

A usable installation of bzip2 is needed or config will fail. Turn this option of in this case.

    MINIZIP_INSTALL=ON -- Enable installation of minizip

This option is only available on mingw as they tend to name this lib different. Maybe this will also be
removed in the future as. If you rely cmake for finding and using zlib, this can be turned off, as
the other file will never be used.

## Using the libs ##

To pull in what you need it's enough to just write

    find_package(ZLIB CONFIG)

or

    find_package(minizip CONFIG)

in your CMakeLists.txt, however it is advised to specify what you really want via:

    find_package(ZLIB CONFIG COMPONENTS shared static REQUIRED)

or

    find_package(minizip CONFIG COMPONENTS shared static REQUIRED)

As it's possible to only build the shared or the static lib, you can make sure that everything you need
is found. If no COMPONENTS are requested, everything needs to be found to satisfy your request. If the
libraries are optional in you project, you can omit the REQUIRED and check yourself if the targets you
want to link against are created.

When you search for minizip, it will search zlib for you, so only one of both is needed.

## Imported targets ##

When found the following targets are created for you:

    ZLIB::ZLIB and ZLIB::ZLIBSTATIC -- for zlib
    MINIZIP::minizip and MINIZIP::minizipstatic -- for minizip

Those two zlib names are the whole consumption interface and they do not change with the implementation
behind them: with ZLIB_BUILD_RUST=ON they resolve to the cargo-built libraries, with it off to the C
ones. The find_package call above is written the same way either way, shared and static stay the only
components, and there is no rust component to ask for, because which implementation was built is settled
when zlib itself is configured and is not something a consumer selects or can see.

The CTest cases in test/ work the same way: example.c, minigzip.c and infcover.c link ZLIB::ZLIB or
ZLIB::ZLIBSTATIC and are compiled from exactly the same unmodified sources whichever library is behind
the name. Read that for what it is, though. A configured tree tests ONE implementation -- the one the
option selected -- and no `_rust`-suffixed case is registered beside its C counterpart, so a single tree
never runs the two side by side. Comparing them means configuring twice into two build directories, or
using crates/zlib-rs-differential, which holds both implementations in one process and compares their
output byte for byte. test/CMakeLists.txt states this at its own head.
