//! One-call compression/decompression utilities and version information.
//!
//! This module provides convenience wrappers for single-buffer compression
//! and decompression operations, plus version and configuration query functions.
//!
//! # Version and Metadata
//!
//! - [`zlib_version`] — Get the library version string
//! - [`compile_flags`] — Get compile-time configuration flags
//! - [`error_message`] — Convert error code to human-readable message

pub mod version;

// Re-export version / metadata public items for flat access via `util::*`.
pub use self::version::{compile_flags, error_message, zlib_version, ZLIB_VERSION};
