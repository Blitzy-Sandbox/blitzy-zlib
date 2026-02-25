//! zlib-rs: A pure Rust implementation of the zlib compression library.

// Compile-time guard: the `no-std` and `gz-io` features are contradictory.
// `gz-io` implies `std` (for file I/O), so enabling both simultaneously is
// an invalid configuration.  Surface this as a clear compile error rather
// than silently letting `gz-io`'s `std` dependency override `no-std`.
#[cfg(all(feature = "no-std", feature = "gz-io"))]
compile_error!(
    "Feature conflict: `no-std` and `gz-io` cannot be enabled simultaneously. \
     `gz-io` requires `std` for file I/O, which is incompatible with `no-std` mode."
);

pub mod checksum;
pub mod constants;
pub mod error;
pub mod gz_header;
pub mod util;
