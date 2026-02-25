// build.rs — Build script for CRC-32 table generation
//
// This build script generates CRC-32 lookup tables at build time, replacing
// the pre-generated `crc32.h` header (9,446 lines of lookup table data) from
// the original C zlib source. The generated tables are written to
// `$OUT_DIR/crc32_tables.rs` and can be included via:
//
//   include!(concat!(env!("OUT_DIR"), "/crc32_tables.rs"));
//
// The generation algorithm is a faithful Rust translation of the C
// `make_crc_table()` function from `crc32.c`, producing byte-identical table
// values. Two tables are generated:
//
// 1. **CRC_TABLE** — 256-entry byte-wise CRC-32 lookup table. Each entry
//    `CRC_TABLE[i]` is the CRC-32 of the single byte value `i`, used for the
//    software-fallback CRC computation path.
//
// 2. **X2N_TABLE** — 32-entry table of powers of x modulo the CRC polynomial.
//    `X2N_TABLE[k]` = x^(2^k) mod p(x), where p(x) is the CRC-32 polynomial
//    (reflected). This table is used by `crc32_combine` and related functions
//    to efficiently combine CRC-32 checksums of concatenated data segments via
//    GF(2) polynomial multiplication.
//
// # Zero C Dependency
//
// This build script is pure Rust and does not invoke any C compiler, linker,
// or external tool. It uses only the Rust standard library.
//
// # Compatibility
//
// The generated tables exactly match the values in the original C `crc32.h`:
// - `crc_table[]` on lines 5–57
// - `x2n_table[]` on lines 9439–9446
//
// # References
//
// - RFC 3720 §12.1 for the CRC-32C polynomial context
// - IEEE 802.3 for the standard CRC-32 polynomial
// - zlib source `crc32.c` for the `make_crc_table`, `multmodp`, and
//   `x2nmodp` reference implementations

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::Path;

// ---------------------------------------------------------------------------
// CRC-32 polynomial constant (IEEE 802.3, reflected representation)
// ---------------------------------------------------------------------------

/// CRC-32 polynomial in reflected (bit-reversed) form with x^32 implied.
///
/// The standard polynomial is:
///   x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10
///        + x^8 + x^7 + x^5 + x^4 + x^2 + x + 1
///
/// Normal form:  0x04C11DB7
/// Reflected:    0xEDB88320
///
/// This matches `crc32.c` line 157: `#define POLY 0xedb88320`
const POLY: u32 = 0xedb88320;

// ---------------------------------------------------------------------------
// GF(2) polynomial arithmetic — translated from crc32.c lines 163–195
// ---------------------------------------------------------------------------

/// Multiply `a` by `b` modulo p(x) in GF(2), where p(x) is the CRC-32
/// polynomial in reflected form.
///
/// This is a direct translation of the C `multmodp()` function from
/// `crc32.c` lines 163–178. The algorithm scans the bits of `a` from the
/// most significant bit downward, accumulating the product in `p` via XOR
/// (polynomial addition in GF(2)) and reducing `b` modulo the polynomial
/// after each step.
///
/// # Precondition
///
/// `a` must not be zero. If `a` is zero, the function enters an infinite
/// loop in the C original (and would do the same here without the early
/// return guard).
fn multmodp(a: u32, b: u32) -> u32 {
    if a == 0 {
        return 0;
    }

    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    let mut b = b;

    loop {
        if a & m != 0 {
            p ^= b;
            if a & (m.wrapping_sub(1)) == 0 {
                break;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }

    p
}

/// Compute x^(n × 2^k) modulo p(x) using the pre-computed x2n_table.
///
/// This is a direct translation of the C `x2nmodp()` function from
/// `crc32.c` lines 184–195. It performs exponentiation by squaring in
/// GF(2) using the table of squared powers.
///
/// # Parameters
///
/// - `n`: The multiplicative factor (non-negative).
/// - `k`: The initial power-of-two exponent index into `x2n_table`.
/// - `x2n_table`: Pre-computed table where `x2n_table[k]` = x^(2^k) mod p(x).
fn x2nmodp(n: i64, k: u32, x2n_table: &[u32; 32]) -> u32 {
    let mut p: u32 = 1 << 31; // x^0 == 1 in reflected representation
    let mut n = n;
    let mut k = k;

    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(x2n_table[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }

    p
}

// ---------------------------------------------------------------------------
// Table generation — translated from crc32.c make_crc_table() lines 243–402
// ---------------------------------------------------------------------------

/// Generate the byte-wise CRC-32 lookup table (256 entries).
///
/// For each byte value `i` (0–255), computes the CRC-32 by processing all
/// 8 bits through the reflected polynomial. This exactly replicates the
/// inner loop of C `make_crc_table()` at lines 248–252:
///
/// ```c
/// for (i = 0; i < 256; i++) {
///     p = i;
///     for (j = 0; j < 8; j++)
///         p = p & 1 ? (p >> 1) ^ POLY : p >> 1;
///     crc_table[i] = p;
/// }
/// ```
fn generate_byte_table() -> [u32; 256] {
    let mut table = [0u32; 256];

    for i in 0..256u32 {
        let mut crc = i;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
        }
        table[i as usize] = crc;
    }

    table
}

/// Generate the powers-of-x table for CRC combine operations (32 entries).
///
/// Computes `x2n_table[k]` = x^(2^k) mod p(x) by iterative squaring in
/// GF(2). This replicates C `make_crc_table()` lines 258–262:
///
/// ```c
/// p = (z_crc_t)1 << 30;         /* x^1 */
/// x2n_table[0] = p;
/// for (n = 1; n < 32; n++)
///     x2n_table[n] = p = multmodp(p, p);
/// ```
///
/// The initial value `1 << 30` represents x^1 in the reflected polynomial
/// representation used by the CRC algorithm.
fn generate_x2n_table() -> [u32; 32] {
    let mut table = [0u32; 32];

    // x^1 in reflected representation
    let mut p: u32 = 1 << 30;
    table[0] = p;

    for entry in table.iter_mut().skip(1) {
        p = multmodp(p, p);
        *entry = p;
    }

    table
}

// ---------------------------------------------------------------------------
// Output formatting helpers
// ---------------------------------------------------------------------------

/// Write a `[u32; N]` array as a Rust `const` declaration to the output file.
///
/// The values are formatted as `0xHHHHHHHH` hex literals, arranged in rows
/// of `cols` values for readability.
///
/// # Parameters
///
/// - `f`: Output file handle.
/// - `name`: The constant name (e.g., `"CRC_TABLE"`).
/// - `table`: Slice of u32 values to write.
/// - `cols`: Number of values per output line (typically 5 or 8).
/// - `doc`: Documentation comment lines to prepend.
fn write_u32_table(
    f: &mut File,
    name: &str,
    table: &[u32],
    cols: usize,
    doc: &[&str],
) -> std::io::Result<()> {
    // Write doc comments
    for line in doc {
        writeln!(f, "/// {line}")?;
    }

    writeln!(f, "#[rustfmt::skip]")?;
    writeln!(f, "pub(crate) const {name}: [u32; {}] = [", table.len())?;

    for (i, &val) in table.iter().enumerate() {
        if i % cols == 0 {
            write!(f, "    ")?;
        }
        write!(f, "0x{val:08x}")?;

        if i < table.len() - 1 {
            write!(f, ",")?;
            if i % cols == cols - 1 {
                writeln!(f)?;
            } else {
                write!(f, " ")?;
            }
        }
    }

    writeln!(f)?;
    writeln!(f, "];")?;
    writeln!(f)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry points (exported per schema)
// ---------------------------------------------------------------------------

/// Generate all CRC-32 lookup tables and write them to the given file.
///
/// This function produces two tables:
///
/// 1. `CRC_TABLE: [u32; 256]` — byte-wise CRC-32 lookup table.
/// 2. `X2N_TABLE: [u32; 32]` — powers-of-x table for combine operations.
///
/// Both tables are written as `pub(crate) const` declarations so they can
/// be included by `src/checksum/crc32.rs` via `include!` if desired.
///
/// The generated values are byte-identical to those in the original C
/// `crc32.h` header file.
///
/// # Errors
///
/// Returns `std::io::Error` if writing to the file fails.
pub fn generate_crc_tables(f: &mut File) -> std::io::Result<()> {
    // Write file header
    writeln!(f, "// crc32_tables.rs — Generated by build.rs")?;
    writeln!(f, "//")?;
    writeln!(
        f,
        "// CRC-32 lookup tables for the zlib-rs crate, generated at build time."
    )?;
    writeln!(
        f,
        "// These tables are equivalent to the C `crc32.h` generated by `crc32.c`."
    )?;
    writeln!(
        f,
        "// Do not edit manually — regenerate by running `cargo build`."
    )?;
    writeln!(f)?;

    // Generate and write the byte-wise CRC table (256 entries)
    let crc_table = generate_byte_table();
    write_u32_table(
        f,
        "CRC_TABLE",
        &crc_table,
        5,
        &[
            "Byte-wise CRC-32 lookup table (256 entries).",
            "",
            "Each entry `CRC_TABLE[i]` is the CRC-32 of the single byte value `i`,",
            "computed using the IEEE 802.3 polynomial 0xEDB88320 (reflected form).",
            "",
            "Equivalent to `crc_table[]` in the original C `crc32.h`.",
        ],
    )?;

    // Generate and write the x2n table (32 entries)
    let x2n_table = generate_x2n_table();
    write_u32_table(
        f,
        "X2N_TABLE",
        &x2n_table,
        5,
        &[
            "Powers-of-x table for CRC-32 combine operations (32 entries).",
            "",
            "`X2N_TABLE[k]` = x^(2^k) mod p(x), where p(x) is the CRC-32 polynomial.",
            "Used by `crc32_combine` and related functions for efficient checksum",
            "combination of concatenated data segments via GF(2) polynomial multiplication.",
            "",
            "Equivalent to `x2n_table[]` in the original C `crc32.h`.",
        ],
    )?;

    Ok(())
}

/// Build script entry point.
///
/// Generates CRC-32 lookup tables and writes them to `$OUT_DIR/crc32_tables.rs`.
/// Sets appropriate Cargo rebuild directives so the tables are only regenerated
/// when this build script itself changes.
///
/// # Panics
///
/// Panics if:
/// - The `OUT_DIR` environment variable is not set (should never happen under Cargo).
/// - The output file cannot be created or written to.
fn main() {
    // Tell Cargo to re-run this build script only when build.rs changes.
    // This avoids unnecessary rebuilds when only source files change.
    println!("cargo:rerun-if-changed=build.rs");

    // Resolve the output directory provided by Cargo.
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set");
    let dest_path = Path::new(&out_dir).join("crc32_tables.rs");

    // Create the output file and generate all tables.
    let mut file = File::create(&dest_path).expect("Failed to create crc32_tables.rs in OUT_DIR");

    generate_crc_tables(&mut file).expect("Failed to write CRC-32 tables to output file");

    // Verify the generated tables by spot-checking known values from crc32.h.
    // This ensures the generation algorithm is correct at build time.
    verify_generated_tables();
}

// ---------------------------------------------------------------------------
// Build-time verification
// ---------------------------------------------------------------------------

/// Verify that the generated CRC-32 tables match known reference values
/// from the original C `crc32.h`.
///
/// This function checks a selection of entries in both the byte-wise CRC
/// table and the x2n_table against values copied verbatim from the C header.
/// If any mismatch is detected, the build fails with a descriptive error.
///
/// Checked reference values from `crc32.h`:
/// - `crc_table[0]`   = 0x00000000
/// - `crc_table[1]`   = 0x77073096
/// - `crc_table[128]` = 0xedb88320 (the polynomial itself)
/// - `crc_table[255]` = 0x2d02ef8d
/// - `x2n_table[0]`   = 0x40000000
/// - `x2n_table[5]`   = 0xedb88320 (x^32 mod p = POLY)
/// - `x2n_table[31]`  = 0xc4e22c3c
fn verify_generated_tables() {
    let crc_table = generate_byte_table();
    let x2n_table = generate_x2n_table();

    // Verify byte-wise CRC table against known reference values from crc32.h
    assert!(
        crc_table[0] == 0x00000000,
        "CRC table mismatch at index 0: expected 0x00000000, got 0x{:08x}",
        crc_table[0]
    );
    assert!(
        crc_table[1] == 0x77073096,
        "CRC table mismatch at index 1: expected 0x77073096, got 0x{:08x}",
        crc_table[1]
    );
    assert!(
        crc_table[2] == 0xee0e612c,
        "CRC table mismatch at index 2: expected 0xee0e612c, got 0x{:08x}",
        crc_table[2]
    );
    assert!(
        crc_table[128] == 0xedb88320,
        "CRC table mismatch at index 128: expected 0xedb88320, got 0x{:08x}",
        crc_table[128]
    );
    assert!(
        crc_table[255] == 0x2d02ef8d,
        "CRC table mismatch at index 255: expected 0x2d02ef8d, got 0x{:08x}",
        crc_table[255]
    );

    // Additional spot checks across the table for thorough verification
    assert!(
        crc_table[3] == 0x990951ba,
        "CRC table mismatch at index 3: expected 0x990951ba, got 0x{:08x}",
        crc_table[3]
    );
    assert!(
        crc_table[100] == 0x4adfa541,
        "CRC table mismatch at index 100: expected 0x4adfa541, got 0x{:08x}",
        crc_table[100]
    );
    assert!(
        crc_table[200] == 0x95bf4a82,
        "CRC table mismatch at index 200: expected 0x95bf4a82, got 0x{:08x}",
        crc_table[200]
    );

    // Verify x2n_table against known reference values from crc32.h
    assert!(
        x2n_table[0] == 0x40000000,
        "X2N table mismatch at index 0: expected 0x40000000, got 0x{:08x}",
        x2n_table[0]
    );
    assert!(
        x2n_table[1] == 0x20000000,
        "X2N table mismatch at index 1: expected 0x20000000, got 0x{:08x}",
        x2n_table[1]
    );
    assert!(
        x2n_table[2] == 0x08000000,
        "X2N table mismatch at index 2: expected 0x08000000, got 0x{:08x}",
        x2n_table[2]
    );
    assert!(
        x2n_table[5] == 0xedb88320,
        "X2N table mismatch at index 5: expected 0xedb88320, got 0x{:08x}",
        x2n_table[5]
    );
    assert!(
        x2n_table[31] == 0xc4e22c3c,
        "X2N table mismatch at index 31: expected 0xc4e22c3c, got 0x{:08x}",
        x2n_table[31]
    );

    // Verify additional x2n entries for thorough coverage
    assert!(
        x2n_table[3] == 0x00800000,
        "X2N table mismatch at index 3: expected 0x00800000, got 0x{:08x}",
        x2n_table[3]
    );
    assert!(
        x2n_table[4] == 0x00008000,
        "X2N table mismatch at index 4: expected 0x00008000, got 0x{:08x}",
        x2n_table[4]
    );
    assert!(
        x2n_table[10] == 0xd7bbfe6a,
        "X2N table mismatch at index 10: expected 0xd7bbfe6a, got 0x{:08x}",
        x2n_table[10]
    );
    assert!(
        x2n_table[20] == 0x9fec022a,
        "X2N table mismatch at index 20: expected 0x9fec022a, got 0x{:08x}",
        x2n_table[20]
    );
    assert!(
        x2n_table[30] == 0xc40ba6d0,
        "X2N table mismatch at index 30: expected 0xc40ba6d0, got 0x{:08x}",
        x2n_table[30]
    );

    // Verify x2nmodp round-trip: x^(1 * 2^0) should equal x2n_table[0]
    let x1 = x2nmodp(1, 0, &x2n_table);
    assert!(
        x1 == 0x40000000,
        "x2nmodp(1, 0) mismatch: expected 0x40000000, got 0x{:08x}",
        x1
    );

    // Verify x2nmodp: x^(1 * 2^5) should equal x2n_table[5] = POLY
    let x32 = x2nmodp(1, 5, &x2n_table);
    assert!(
        x32 == 0xedb88320,
        "x2nmodp(1, 5) mismatch: expected 0xedb88320, got 0x{:08x}",
        x32
    );
}

// ---------------------------------------------------------------------------
// Tests (compile-time verification within the build script)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the CRC-32 of the byte 0x00 is 0x00000000.
    #[test]
    fn test_crc_table_zero() {
        let table = generate_byte_table();
        assert_eq!(table[0], 0x00000000);
    }

    /// Verify the CRC-32 of byte 0x80 (128) equals the polynomial.
    /// This is a well-known property: CRC of byte whose MSB of the
    /// reflected value is 1 equals the polynomial XOR'd appropriately.
    #[test]
    fn test_crc_table_polynomial_entry() {
        let table = generate_byte_table();
        // crc_table[128] should equal POLY (0xedb88320)
        assert_eq!(table[128], POLY);
    }

    /// Verify the last entry of the CRC table.
    #[test]
    fn test_crc_table_last_entry() {
        let table = generate_byte_table();
        assert_eq!(table[255], 0x2d02ef8d);
    }

    /// Verify the first several entries of the CRC table against crc32.h.
    #[test]
    fn test_crc_table_first_entries() {
        let table = generate_byte_table();
        #[rustfmt::skip]
        let expected: [u32; 10] = [
            0x00000000, 0x77073096, 0xee0e612c, 0x990951ba, 0x076dc419,
            0x706af48f, 0xe963a535, 0x9e6495a3, 0x0edb8832, 0x79dcb8a4,
        ];
        assert_eq!(&table[..10], &expected);
    }

    /// Verify the entire x2n_table against values from crc32.h.
    #[test]
    fn test_x2n_table_complete() {
        let table = generate_x2n_table();
        #[rustfmt::skip]
        let expected: [u32; 32] = [
            0x40000000, 0x20000000, 0x08000000, 0x00800000, 0x00008000,
            0xedb88320, 0xb1e6b092, 0xa06a2517, 0xed627dae, 0x88d14467,
            0xd7bbfe6a, 0xec447f11, 0x8e7ea170, 0x6427800e, 0x4d47bae0,
            0x09fe548f, 0x83852d0f, 0x30362f1a, 0x7b5a9cc3, 0x31fec169,
            0x9fec022a, 0x6c8dedc4, 0x15d6874d, 0x5fde7a4e, 0xbad90e37,
            0x2e4e5eef, 0x4eaba214, 0xa8a472c0, 0x429a969e, 0x148d302a,
            0xc40ba6d0, 0xc4e22c3c,
        ];
        assert_eq!(table, expected);
    }

    /// Verify multmodp identity: multiplying by x^0 (1 << 31) returns
    /// the operand unchanged.
    #[test]
    fn test_multmodp_identity() {
        let identity = 1u32 << 31; // x^0 in reflected representation
        assert_eq!(multmodp(identity, 0x12345678), 0x12345678);
    }

    /// Verify multmodp with zero input returns zero.
    #[test]
    fn test_multmodp_zero() {
        assert_eq!(multmodp(0, 0x12345678), 0);
    }

    /// Verify x2nmodp computes x^0 correctly (should return 1 << 31).
    #[test]
    fn test_x2nmodp_zero_exponent() {
        let x2n = generate_x2n_table();
        // x^(0 * 2^0) = x^0 = 1 in reflected representation
        let result = x2nmodp(0, 0, &x2n);
        assert_eq!(result, 1u32 << 31);
    }

    /// Verify x2nmodp for n=1, k=0 returns x2n_table[0] = x^1.
    #[test]
    fn test_x2nmodp_x1() {
        let x2n = generate_x2n_table();
        let result = x2nmodp(1, 0, &x2n);
        // x^(1 * 2^0) = x^1 = x2n_table[0]
        assert_eq!(result, 0x40000000);
    }
}
