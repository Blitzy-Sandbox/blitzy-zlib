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
Rust workspace at the top of this tree (crates/libz-rs-sys) instead of from the C sources. The libz.so
and libz.a it stages present the same ABI as the C library, the shared one carrying the same SONAME
libz.so.1 and the same 111 exported symbols at the same versions, so programs already linked against
zlib keep working without being recompiled. cargo has to be reachable on PATH and configuration stops
with a message telling you so when it is not, or -DZLIB_CARGO_EXECUTABLE=/path/to/cargo names a
particular one. Building the workspace needs Rust 1.80 or newer, which cargo enforces itself from
rust-toolchain.toml and the crate manifests. The C targets zlib and zlibstatic are still defined and
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
go on searching and bind the system libz instead, silently. README and rust/README.md cover the Rust
build in full.

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
components, and there is deliberately no rust component to ask for, because which implementation was
built is settled when zlib itself is configured and is not something a consumer selects or can see.
That is also why the cases in test/ need no Rust-specific variants: example.c, minigzip.c and infcover.c
link ZLIB::ZLIB or ZLIB::ZLIBSTATIC and are compiled from exactly the same unmodified sources whichever
library is behind the name.
