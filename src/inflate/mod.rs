//! DEFLATE decompression engine.
//!
//! This module contains the inflate (decompression) implementation ported from
//! C zlib's `inflate.c`, `inffast.c`, `inftrees.c`, `infback.c`, and related
//! headers.

pub mod tables;
