//! Shared test utility library for zlib-rs integration tests.
//!
//! Provides common helpers, fixture loading, hex decoding, memory tracking,
//! and assertion utilities used across all integration test files.
//!
//! # Re-exports
//!
//! This crate re-exports core types from `zlib_rs` for convenience. Test files
//! can do `use zlib_rs_tests::*;` to access all public constants, the
//! [`ZStream`] type, [`ReturnCode`] enum, and compression/decompression
//! functions without individual imports.
//!
//! # Source Derivation
//!
//! Utility code is ported from two C test files:
//! - `test/example.c` — `CHECK_ERR` macro, `hello`/`dictionary` constants,
//!   buffer allocation patterns
//! - `test/infcover.c` — `mem_item`/`mem_zone` memory tracking,
//!   `h2b()` hex decoder

#![deny(clippy::all)]
#![deny(clippy::pedantic)]

// ─── Re-exports from zlib-rs core crate ──────────────────────────────────────
//
// These make all zlib types and functions available to test files via a single
// `use zlib_rs_tests::*;` import.

pub use zlib_rs::compress::{compress, compress_bound, compress2, uncompress, uncompress2};
#[allow(clippy::wildcard_imports)]
pub use zlib_rs::constants::*;
pub use zlib_rs::error::ReturnCode;
pub use zlib_rs::stream::ZStream;

// ─── Standard library imports ────────────────────────────────────────────────

use std::fs;
use std::path::PathBuf;

// =============================================================================
// Common Test Constants
// Ported from test/example.c lines 35–41, 497–500
// =============================================================================

/// Standard test string used throughout the test suite.
///
/// Uses `"hello, hello!"` because the repeated `"hello"` stresses
/// the compression code better than `"hello world"`.
/// (Matches C zlib `test/example.c` line 35.)
///
/// The trailing null byte `\0` is included to match C's `strlen(hello)+1`
/// usage throughout `example.c`, where the null terminator is part of the
/// data being compressed and compared after decompression.
pub const HELLO: &[u8] = b"hello, hello!\0";

/// Dictionary used for preset-dictionary compression tests.
///
/// (Matches C zlib `test/example.c` line 40.)
///
/// The trailing null byte `\0` is included to match C's
/// `sizeof(dictionary)` = 6 bytes.
pub const DICTIONARY: &[u8] = b"hello\0";

/// Default decompression buffer size for tests (20 000 bytes).
///
/// Matches C `test/example.c` line 499: `uLong uncomprLen = 20000`.
pub const UNCOMPR_LEN: usize = 20_000;

/// Default compression buffer size for tests (3 × `UNCOMPR_LEN` = 60 000 bytes).
///
/// Matches C `test/example.c` line 500: `uLong comprLen = 3 * uncomprLen`.
pub const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

/// Default test file name for gzip I/O tests.
///
/// Matches C `test/example.c` line 25: `#define TESTFILE "foo.gz"`.
pub const TESTFILE: &str = "foo.gz";

// =============================================================================
// Error Checking
// Ported from test/example.c lines 28–33 (CHECK_ERR macro)
// =============================================================================

/// Check a zlib return code, panicking with a descriptive message if it is not
/// [`ReturnCode::Ok`].
///
/// This is the Rust equivalent of the C `CHECK_ERR(err, msg)` macro from
/// `test/example.c` line 28. Unlike the C version which calls `exit(1)`, this
/// panics with a useful message, integrating with Rust's test harness for
/// proper test failure reporting.
///
/// # Panics
///
/// Panics if `code` is anything other than [`ReturnCode::Ok`].
pub fn check_err(code: ReturnCode, msg: &str) {
    assert!(
        code == ReturnCode::Ok,
        "{msg} error: {code:?} ({})",
        code as i32
    );
}

/// Macro version of [`check_err()`] for concise error checking in tests.
///
/// # Examples
///
/// ```ignore
/// check_err!(return_code, "deflateInit");
/// ```
#[macro_export]
macro_rules! check_err {
    ($err:expr, $msg:expr) => {
        $crate::check_err($err, $msg)
    };
}

// =============================================================================
// Hex Decoding
// Ported from test/infcover.c lines 238–273 (h2b function)
// =============================================================================

/// Decode a hexadecimal string into a byte vector.
///
/// Port of `h2b()` from `test/infcover.c` lines 245–272. Decodes liberally:
///
/// - Hex digits can be adjacent, in which case two in a row writes a byte.
/// - Digits can be delimited by any non-hex character; delimiters are ignored.
/// - A single hex digit followed by a delimiter writes that single digit as a
///   byte (using the internal `val += 240` trick to trigger emission).
/// - The null terminator is part of the decoding loop — in Rust, an extra `0u8`
///   byte is chained to simulate the C `do { ... } while (*hex++)` pattern.
///
/// # Implementation Notes
///
/// The internal sentinel value `val = 1` (not zero) distinguishes "no pending
/// digit" from "pending digit is 0". The `val += 240` adjustment for a single
/// digit followed by a delimiter pushes `val` above 255, triggering byte
/// emission on the next check.
///
/// # Examples
///
/// ```
/// use zlib_rs_tests::h2b;
///
/// let bytes = h2b("48 65 6c 6c 6f");
/// assert_eq!(bytes, vec![0x48, 0x65, 0x6c, 0x6c, 0x6f]);
///
/// // Adjacent hex digits without delimiters
/// let bytes = h2b("48656c6c6f");
/// assert_eq!(bytes, vec![0x48, 0x65, 0x6c, 0x6c, 0x6f]);
///
/// // Single digit followed by delimiter
/// let bytes = h2b("0 1 2");
/// assert_eq!(bytes, vec![0x00, 0x01, 0x02]);
/// ```
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn h2b(hex: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity((hex.len() + 1) / 2);
    let mut val: u32 = 1;

    for &ch in hex.as_bytes().iter().chain(std::iter::once(&0u8)) {
        match ch {
            b'0'..=b'9' => val = (val << 4) + u32::from(ch - b'0'),
            b'A'..=b'F' => val = (val << 4) + u32::from(ch - b'A') + 10,
            b'a'..=b'f' => val = (val << 4) + u32::from(ch - b'a') + 10,
            _ if val != 1 && val < 32 => {
                // One digit followed by a delimiter — make it look like two
                // digits so the emission check below triggers.
                val += 240;
            }
            _ => {}
        }
        if val > 255 {
            // Have two digits — save the decoded byte and reset.
            result.push((val & 0xff) as u8);
            val = 1;
        }
    }

    result
}

// =============================================================================
// Memory Tracking Utilities
// Ported from test/infcover.c lines 56–234
// =============================================================================

/// Tracks a single memory allocation for monitoring purposes.
///
/// Port of `struct mem_item` from `test/infcover.c` line 56.
#[derive(Debug)]
struct MemItem {
    /// Identifier for the allocation (replaces C's `void *ptr`).
    id: usize,
    /// Size of the allocation in bytes.
    size: usize,
}

/// Memory allocation zone for tracking statistics.
///
/// Port of `struct mem_zone` from `test/infcover.c` lines 63–68. Tracks total
/// allocations, high water marks, allocation limits, non-LIFO frees, and
/// unrecognized (rogue) frees.
///
/// In Rust this is simpler than the C version because Rust's ownership model
/// prevents most bugs the C tracker detects. These utilities remain useful for:
/// - Tracking total allocation counts and sizes
/// - Simulating allocation failure paths via [`set_limit`](MemZone::set_limit)
/// - Verifying no leaked resources in complex test scenarios
#[derive(Debug)]
pub struct MemZone {
    /// List of tracked allocations.
    items: Vec<MemItem>,
    /// Total bytes currently allocated.
    pub total: usize,
    /// Highest total allocation seen (high water mark).
    pub highwater: usize,
    /// Allocation limit in bytes (0 = no limit).
    pub limit: usize,
    /// Counter of non-LIFO free operations.
    pub notlifo: usize,
    /// Counter of unrecognized free operations (frees of unknown IDs).
    pub rogue: usize,
    /// Next allocation identifier.
    next_id: usize,
}

impl MemZone {
    /// Create a new memory tracking zone with all counters zeroed.
    ///
    /// Port of `mem_setup()` from `test/infcover.c` lines 158–173.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            highwater: 0,
            limit: 0,
            notlifo: 0,
            rogue: 0,
            next_id: 0,
        }
    }

    /// Set an allocation limit in bytes. Zero means no limit.
    ///
    /// Any subsequent [`track_alloc`](MemZone::track_alloc) call that would
    /// push `total` above `limit` returns `None`.
    ///
    /// Port of `mem_limit()` from `test/infcover.c` lines 176–181.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    /// Track a simulated allocation of `size` bytes.
    ///
    /// Returns `Some(id)` with a unique allocation identifier on success, or
    /// `None` if the allocation would exceed the configured
    /// [`limit`](MemZone::limit).
    ///
    /// Updates [`total`](MemZone::total) and
    /// [`highwater`](MemZone::highwater) on success.
    ///
    /// Port of `mem_alloc()` from `test/infcover.c` lines 71–109.
    pub fn track_alloc(&mut self, size: usize) -> Option<usize> {
        if self.limit > 0 && self.total + size > self.limit {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(MemItem { id, size });
        self.total += size;
        if self.total > self.highwater {
            self.highwater = self.total;
        }
        Some(id)
    }

    /// Track a simulated free operation for the allocation identified by `id`.
    ///
    /// If the allocation is found, its size is subtracted from
    /// [`total`](MemZone::total) and the entry is removed. If the freed entry
    /// is not the most recent allocation, [`notlifo`](MemZone::notlifo) is
    /// incremented (matching C zlib's non-LIFO tracking). If `id` is not found,
    /// [`rogue`](MemZone::rogue) is incremented.
    ///
    /// Port of `mem_free()` from `test/infcover.c` lines 112–154.
    pub fn track_free(&mut self, id: usize) {
        if let Some(pos) = self.items.iter().position(|item| item.id == id) {
            let size = self.items[pos].size;
            self.total -= size;
            if pos != self.items.len() - 1 {
                self.notlifo += 1;
            }
            self.items.remove(pos);
        } else {
            self.rogue += 1;
        }
    }

    /// Check for leaks and report any issues to stderr.
    ///
    /// Prints diagnostic messages if:
    /// - Any allocations remain (memory leak)
    /// - Any non-LIFO frees occurred
    /// - Any rogue (unrecognized) frees occurred
    ///
    /// If everything is normal, nothing is printed — matching the C behavior.
    ///
    /// Port of `mem_done()` from `test/infcover.c` lines 200–234.
    pub fn done(&self, prefix: &str) {
        if !self.items.is_empty() || self.total > 0 {
            eprintln!(
                "** {prefix}: {} bytes in {} blocks not freed",
                self.total,
                self.items.len()
            );
        }
        if self.notlifo > 0 {
            eprintln!("** {prefix}: {} frees not LIFO", self.notlifo);
        }
        if self.rogue > 0 {
            eprintln!("** {prefix}: {} frees not recognized", self.rogue);
        }
    }

    /// Get the current total allocated bytes.
    ///
    /// Port of `mem_used()` from `test/infcover.c` lines 184–189.
    #[must_use]
    pub fn used(&self) -> usize {
        self.total
    }

    /// Get the high water mark of allocations.
    ///
    /// This is the maximum value that [`total`](MemZone::total) ever reached
    /// during the tracking session.
    ///
    /// Port of `mem_high()` from `test/infcover.c` lines 192–197.
    #[must_use]
    pub fn high(&self) -> usize {
        self.highwater
    }
}

impl Default for MemZone {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Buffer Management Helpers
// Ported from test/example.c lines 515–523
// =============================================================================

/// Allocate a pair of compression and decompression buffers for testing.
///
/// Returns `(compression_buffer, decompression_buffer)` initialized to zeros.
/// The compression buffer is 3× the decompression buffer size, matching
/// the C test's `comprLen = 3 * uncomprLen` ratio.
///
/// Port of buffer allocation in `test/example.c` lines 515–523.
#[must_use]
pub fn alloc_test_buffers() -> (Vec<u8>, Vec<u8>) {
    let uncompr = vec![0u8; UNCOMPR_LEN];
    let compr = vec![0u8; COMPR_LEN];
    (compr, uncompr)
}

/// Allocate a compression buffer of the specified size, initialized to zeros.
#[must_use]
pub fn alloc_compr_buffer(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

/// Allocate a decompression buffer of the specified size, initialized to zeros.
#[must_use]
pub fn alloc_uncompr_buffer(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

// =============================================================================
// Assertion Helpers
// =============================================================================

/// Assert that a return code is [`ReturnCode::Ok`], panicking with a
/// descriptive context message otherwise.
///
/// # Panics
///
/// Panics if `code` is not [`ReturnCode::Ok`].
pub fn assert_ok(code: ReturnCode, context: &str) {
    assert_eq!(
        code,
        ReturnCode::Ok,
        "{context}: expected Z_OK, got {code:?}"
    );
}

/// Assert that a return code matches an expected value.
///
/// # Panics
///
/// Panics if `actual` does not equal `expected`.
pub fn assert_return_code(actual: ReturnCode, expected: ReturnCode, context: &str) {
    assert_eq!(
        actual, expected,
        "{context}: expected {expected:?}, got {actual:?}"
    );
}

/// Assert that two byte slices are identical.
///
/// On failure, provides a detailed message showing the first differing byte
/// position, both lengths, and the differing byte values. This is much more
/// informative than the default `assert_eq!` output for large byte buffers.
///
/// # Panics
///
/// Panics if `actual` and `expected` differ in length or content.
pub fn assert_bytes_equal(actual: &[u8], expected: &[u8], context: &str) {
    if actual != expected {
        let first_diff = actual
            .iter()
            .zip(expected.iter())
            .position(|(a, e)| a != e)
            .unwrap_or(actual.len().min(expected.len()));
        panic!(
            "{context}: byte mismatch at position {first_diff}\n  \
             actual length: {}, expected length: {}\n  \
             actual[{first_diff}]: {:#04x}, expected[{first_diff}]: {:#04x}",
            actual.len(),
            expected.len(),
            actual.get(first_diff).copied().unwrap_or(0),
            expected.get(first_diff).copied().unwrap_or(0),
        );
    }
}

/// Perform a compression round-trip test: compress data and verify it
/// decompresses back to the original.
///
/// This is the most common test pattern, used across multiple test files.
/// Internally calls [`compress`] followed by [`uncompress`], then compares
/// the decompressed output against the original data byte-for-byte.
///
/// # Panics
///
/// Panics if compression fails, decompression fails, or the decompressed
/// output does not match the original data.
pub fn assert_round_trip(data: &[u8]) {
    let mut compressed = Vec::new();
    compress(&mut compressed, data).expect("compression failed in round-trip test");

    // Pre-allocate the decompression buffer to the known original size.
    // uncompress() uses dest.len() as the output buffer capacity.
    let mut decompressed = vec![0u8; data.len()];
    uncompress(&mut decompressed, &compressed).expect("decompression failed in round-trip test");

    assert_bytes_equal(&decompressed, data, "round-trip");
}

// =============================================================================
// Fixture Loading Utilities
// =============================================================================

/// Get the path to the test fixtures directory.
///
/// Returns `<CARGO_MANIFEST_DIR>/fixtures`, where `CARGO_MANIFEST_DIR` is the
/// directory containing the `tests/Cargo.toml` manifest. This ensures correct
/// path resolution regardless of where Cargo runs the tests from.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("fixtures");
    path
}

/// Get the path to the test vectors directory.
///
/// Returns `<CARGO_MANIFEST_DIR>/fixtures/test_vectors`.
#[must_use]
pub fn test_vectors_dir() -> PathBuf {
    let mut path = fixtures_dir();
    path.push("test_vectors");
    path
}

/// Load a test fixture file as bytes.
///
/// Reads the file at `<fixtures_dir>/<name>` and returns its contents.
///
/// # Panics
///
/// Panics if the file cannot be read (e.g., file not found, permission error).
/// This is acceptable in test utility code where missing fixtures should
/// immediately fail the test.
#[must_use]
pub fn load_fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("failed to load fixture {}: {e}", path.display()))
}

/// Load a test vector file from the `test_vectors` subdirectory.
///
/// Reads the file at `<test_vectors_dir>/<name>` and returns its contents.
///
/// # Panics
///
/// Panics if the file cannot be read (e.g., file not found, permission error).
#[must_use]
pub fn load_test_vector(name: &str) -> Vec<u8> {
    let path = test_vectors_dir().join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("failed to load test vector {}: {e}", path.display()))
}
