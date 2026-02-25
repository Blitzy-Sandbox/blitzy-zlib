//! enough — determine maximum size of inflate's Huffman code tables
//!
//! Port of enough.c (Version 1.6, 29 July 2024, Mark Adler)
//! Copyright (C) 2007, 2008, 2012, 2018, 2024 Mark Adler
//!
//! Examines all possible valid and complete prefix codes for a given number
//! of symbols and maximum code length in bits to determine the maximum table
//! size for zlib's inflate. Only complete prefix codes are counted.
//!
//! Two codes are considered distinct if the vectors of the number of codes per
//! length are not identical. Permutations of symbol assignments and bit values
//! are not counted separately (only canonical codes are counted).
//!
//! The inflate Huffman decoding algorithm uses two-level lookup tables. There
//! is a single first-level table to decode codes up to `root` bits, and
//! second-level tables for longer codes. This program computes the maximum
//! total table entries across all valid prefix codes.
//!
//! # Usage
//!
//! ```text
//! enough [syms [root [max]]]
//! ```
//!
//! - Default: `enough 286 9 15` (deflate literal/length code)
//! - For distance code: `enough 30 6`
//!
//! # Known Result
//!
//! For the default arguments (286 symbols, root=9, max=15), the maximum
//! table size is **1444** entries (the ENOUGH constant in `inftrees.h`).

use std::env;
use std::fmt::Write;
use std::process;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Type for code counting (`big_t` / `unsigned long long` in C).
type BigT = u64;

/// Overflow sentinel value, equivalent to `(big_t)-1` in C.
/// Returned by [`State::count`] when a code count overflows `u64`.
const OVERFLOW: BigT = u64::MAX;

/// Been-here check with dynamic bit vector (`struct tab` in C).
///
/// Each element tracks visited `(mem, rem)` states for a particular
/// `(syms, left, len)` triplet via a variable-length bit vector. The vector
/// is indexed using a skewed triangular array formula and grows dynamically
/// as new states are visited.
struct Tab {
    /// Bit vector for tracking visited states, grows as needed via
    /// [`Vec::resize`]. Each bit represents one `(mem, rem)` state.
    vec: Vec<u8>,
}

impl Tab {
    /// Create a new `Tab` with an empty bit vector.
    fn new() -> Self {
        Self { vec: Vec::new() }
    }
}

// ---------------------------------------------------------------------------
// State — replaces C global struct `g`
// ---------------------------------------------------------------------------

/// Computation state for the enough algorithm.
///
/// Replaces the C global `g` struct. All fields that were global in the C
/// version are owned here, and all recursive functions receive `&mut self`
/// instead of accessing globals.
struct State {
    /// Maximum allowed bit length for the codes.
    max: usize,
    /// Size of the base code table in bits.
    root: usize,
    /// Largest code table found so far (in entries).
    large: usize,
    /// Display of sub-codes for maximum table size (replaces C `string_t`).
    out: String,
    /// Number of symbols assigned to each bit length.
    /// Indexed by bit length `0..=max`.
    code: Vec<usize>,
    /// Saved results array for code counting (memoization).
    /// Indexed by `map(syms, left, len)`.
    num: Vec<BigT>,
    /// States already evaluated array (been-here tracking).
    /// Indexed by `map(syms, left, len)`, each containing a bit vector
    /// indexed by `(mem, rem)` for fine-grained state deduplication.
    done: Vec<Tab>,
}

impl State {
    /// Index function for `num[]` and `done[]` arrays.
    ///
    /// Maps the `(syms, left, len)` triplet to a flat array index using the
    /// formula from the C version (line 237):
    ///
    /// ```text
    /// ((syms-1)/2 * (syms-2)/2 + left/2 - 1) * (max - 1) + len - 1
    /// ```
    ///
    /// # Constraints
    ///
    /// - `syms >= 3`
    /// - `left >= 2` (even)
    /// - `1 <= len <= max - 1`
    fn map(&self, syms: usize, left: usize, len: usize) -> usize {
        ((syms - 1) / 2 * ((syms - 2) / 2) + left / 2 - 1) * (self.max - 1) + len - 1
    }

    /// Return the number of possible prefix codes using bit patterns of lengths
    /// `len` through `max` inclusive, coding `syms` symbols, with `left` bit
    /// patterns of length `len` unused.
    ///
    /// Returns [`OVERFLOW`] if there is an overflow in the counting. Keeps a
    /// record of previous results in `self.num` to prevent repeating the same
    /// calculation.
    ///
    /// Port of `count()` (line 261 of `enough.c`).
    fn count(&mut self, syms: usize, left: usize, len: usize) -> BigT {
        // See if only one possible code
        if syms == left {
            return 1;
        }

        // Note and verify the expected state
        debug_assert!(syms > left && left > 0 && len < self.max);

        // See if we've done this one already
        let index = self.map(syms, left, len);
        let got = self.num[index];
        if got != 0 {
            return got; // we have — return the saved result
        }

        // We need to use at least this many bit patterns so that the code
        // won't be incomplete at the next length (more bit patterns than
        // symbols remaining).
        let least = (left * 2).saturating_sub(syms);

        // We can use at most this many bit patterns, lest there not be enough
        // available for the remaining symbols at the maximum length. (If there
        // were no limit to the code length, this would become: most = left - 1.)
        let shift = self.max - len;
        let most = (((left as u64) << shift) - syms as u64) / ((1u64 << shift) - 1);
        let most = most as usize;

        // Count all possible codes from this juncture and add them up
        let mut sum: BigT = 0;
        for used in least..=most {
            let got = self.count(syms - used, (left - used) << 1, len + 1);
            sum = sum.wrapping_add(got);
            if got == OVERFLOW || sum < got {
                // Overflow detected
                return OVERFLOW;
            }
        }

        // Verify that all recursive calls are productive
        debug_assert!(sum != 0);

        // Save the result and return it
        self.num[index] = sum;
        sum
    }

    /// Return `true` if we've been here before, `false` if not (and mark as
    /// visited).
    ///
    /// Sets a bit in a bit vector to indicate visiting this state. Each
    /// `(syms, left, len)` triplet has a variable-size bit vector indexed by
    /// `(mem, rem)`. The bit vector is lengthened as needed.
    ///
    /// Port of `been_here()` (line 308 of `enough.c`).
    fn been_here(
        &mut self,
        syms: usize,
        left: usize,
        len: usize,
        mem: usize,
        rem: usize,
    ) -> bool {
        // Point to vector for (syms,left,len), bit in vector for (mem,rem)
        let index = self.map(syms, left, len);
        let adj_mem = (mem - (1 << self.root)) >> 1; // mem always includes root table; always even
        let adj_rem = rem >> 1; // rem is always even

        // Skewed triangular array formula — 8x range for mem vs rem
        let offset_base = (adj_mem >> 3) + adj_rem;
        let offset = (offset_base * (offset_base + 1)) / 2 + adj_rem;
        let bit: u8 = 1 << (adj_mem & 7);

        // See if we've been here
        let length = self.done[index].vec.len();
        if offset < length && (self.done[index].vec[offset] & bit) != 0 {
            return true; // done this!
        }

        // We haven't been here before — set the bit to show we have now.
        // See if we need to lengthen the vector in order to set the bit.
        if length <= offset {
            let new_length = if length > 0 {
                // We have one already — enlarge it, zero out the appended space
                let mut l = length;
                while l <= offset {
                    l <<= 1;
                }
                l
            } else {
                // We need to make a new vector
                let mut l = 16;
                while l <= offset {
                    l <<= 1;
                }
                l
            };
            // Vec::resize zero-fills the new space automatically
            self.done[index].vec.resize(new_length, 0);
        }

        // Set the bit
        self.done[index].vec[offset] |= bit;
        false
    }

    /// Examine all possible codes from the given node `(syms, left, len)`.
    ///
    /// Computes the amount of memory required to build inflate's decoding
    /// tables, where `mem` is the number of code structures used so far,
    /// and `rem` is the number remaining in the current sub-table.
    ///
    /// Port of `examine()` (line 361 of `enough.c`).
    #[allow(clippy::too_many_lines)]
    fn examine(
        &mut self,
        mut syms: usize,
        mut left: usize,
        len: usize,
        mut mem: usize,
        mut rem: usize,
    ) {
        let root = self.root;
        let max = self.max;

        // See if we have a complete code
        if syms == left {
            // Set the last code entry
            self.code[len] = left;

            // Complete computation of memory used by this code
            while rem < left {
                left -= rem;
                rem = 1 << (len - root);
                mem += rem;
            }
            debug_assert!(rem == left);

            // If this is at the maximum, show the sub-code
            if mem >= self.large {
                // If this is a new maximum, update the maximum and clear out
                // the printed sub-codes from the previous maximum
                if mem > self.large {
                    self.large = mem;
                    self.out.clear();
                }

                // Compute the starting state for this sub-code
                syms = 0;
                left = 1 << max;
                for bits in ((root + 1)..=max).rev() {
                    syms += self.code[bits];
                    left -= self.code[bits];
                    debug_assert!(left & 1 == 0);
                    left >>= 1;
                }

                // Print the starting state and the resulting sub-code to out
                let _ = write!(
                    self.out,
                    "<{}, {}, {}>:",
                    syms,
                    root + 1,
                    ((1 << root) - left) << 1
                );
                for bits in (root + 1)..=max {
                    if self.code[bits] != 0 {
                        let _ = write!(self.out, " {}[{}]", self.code[bits], bits);
                    }
                }
                let _ = writeln!(self.out);
            }

            // Remove entries as we drop back down in the recursion
            self.code[len] = 0;
            return;
        }

        // Prune the tree if we can
        if self.been_here(syms, left, len, mem, rem) {
            return;
        }

        // We need to use at least this many bit patterns so that the code
        // won't be incomplete at the next length
        let least = (left * 2).saturating_sub(syms);

        // We can use at most this many bit patterns
        let shift = max - len;
        let most = (((left as u64) << shift) - syms as u64) / ((1u64 << shift) - 1);
        let most = most as usize;

        // Occupy least table spaces, creating new sub-tables as needed
        let mut used_init = least;
        while rem < used_init {
            used_init -= rem;
            rem = 1 << (len - root);
            mem += rem;
        }
        rem -= used_init;

        // Examine codes from here, updating table space as we go
        for used in least..=most {
            self.code[len] = used;
            let sub_table = if rem != 0 { 1 << (len - root) } else { 0 };
            self.examine(syms - used, (left - used) << 1, len + 1, mem + sub_table, rem << 1);
            if rem == 0 {
                rem = 1 << (len - root);
                mem += rem;
            }
            rem -= 1;
        }

        // Remove entries as we drop back down in the recursion
        self.code[len] = 0;
    }

    /// Look at all sub-codes starting with `root + 1` bits.
    ///
    /// For each completed code, calculates the amount of memory required by
    /// inflate to build the decoding tables. Finds the maximum amount and
    /// shows the codes that require that maximum.
    ///
    /// Port of `enough()` (line 454 of `enough.c`).
    fn enough(&mut self, syms: usize) {
        // Clear code
        for n in 0..=self.max {
            self.code[n] = 0;
        }

        // Look at all (root + 1) bit and longer codes
        self.out.clear();
        self.large = 1 << self.root;

        let root = self.root;
        let max = self.max;

        if root < max {
            // otherwise, there's only a base table
            for n in 3..=syms {
                let mut left_val = 2;
                while left_val < n {
                    // Look at all reachable (root + 1) bit nodes, and the
                    // resulting codes (complete at root + 2 or more)
                    let index = self.map(n, left_val, root + 1);
                    if root + 1 < max && self.num[index] != 0 {
                        // reachable node
                        self.examine(n, left_val, root + 1, 1 << root, 0);
                    }

                    // Also look at root bit codes with completions at root + 1
                    // bits (not saved in num, since complete), just in case
                    if self.num[index - 1] != 0 && n <= left_val << 1 {
                        let diff = (n - left_val) << 1;
                        self.examine(diff, diff, root + 1, 1 << root, 0);
                    }

                    left_val += 2;
                }
            }
        }

        // Done — print results
        println!(
            "maximum of {} table entries for root = {}",
            self.large, self.root
        );
        print!("{}", self.out);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Main entry point — parses CLI arguments and runs the enough computation.
///
/// # Arguments
///
/// ```text
/// enough [syms [root [max]]]
/// ```
///
/// - `syms`: number of symbols (default 286, minimum 2)
/// - `root`: base code table size in bits (default 9, minimum 1)
/// - `max`:  maximum code length in bits (default 15, minimum 1)
///
/// The default arguments correspond to the deflate literal/length code.
/// For the deflate distance code, use `enough 30 6`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Get arguments — default to the deflate literal/length code
    let mut syms: usize = 286;
    let mut root: usize = 9;
    let mut max: usize = 15;

    if args.len() > 1 {
        syms = match args[1].parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "invalid arguments, need: [sym >= 2 [root >= 1 [max >= 1]]]"
                );
                process::exit(1);
            }
        };
        if args.len() > 2 {
            root = match args[2].parse() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!(
                        "invalid arguments, need: [sym >= 2 [root >= 1 [max >= 1]]]"
                    );
                    process::exit(1);
                }
            };
            if args.len() > 3 {
                max = match args[3].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!(
                            "invalid arguments, need: [sym >= 2 [root >= 1 [max >= 1]]]"
                        );
                        process::exit(1);
                    }
                };
            }
        }
    }

    if args.len() > 4 || syms < 2 || root < 1 || max < 1 {
        eprintln!("invalid arguments, need: [sym >= 2 [root >= 1 [max >= 1]]]");
        process::exit(1);
    }

    // If not restricting the code length, the longest is syms - 1
    if max > syms - 1 {
        max = syms - 1;
    }

    // Determine the number of bits in a BigT (code_t in C)
    let bits: usize = u64::BITS as usize;

    // Make sure that the calculation of most will not overflow
    if max > bits || (syms as u64 - 2) >= (u64::MAX >> (max - 1)) {
        eprintln!("abort: code length too long for internal types");
        process::exit(1);
    }

    // Reject impossible code requests — more symbols than can be encoded
    if max < bits && (syms as u64 - 1) > ((1u64 << max) - 1) {
        eprintln!("{syms} symbols cannot be coded in {max} bits");
        process::exit(1);
    }

    // Allocate code vector (indexed by bit length 0..=max)
    let code_vec = vec![0usize; max + 1];

    // Determine size of saved results array, checking for overflows.
    // The array stores memoized code counts indexed by (syms, left, len).
    let (array_size, num_vec) = if syms == 2 {
        // Only max == 1 when syms == 2 — no results to save
        (0, Vec::new())
    } else {
        let mut sz: usize = syms / 2;
        let n1 = (syms - 1) / 2;
        sz = match sz.checked_mul(n1) {
            Some(v) => v,
            None => {
                eprintln!("abort: overflow computing array size");
                process::exit(1);
            }
        };
        let n2 = max - 1;
        sz = match sz.checked_mul(n2) {
            Some(v) => v,
            None => {
                eprintln!("abort: overflow computing array size");
                process::exit(1);
            }
        };
        (sz, vec![0u64; sz])
    };

    // Create computation state
    let mut state = State {
        max,
        root,
        large: 0,
        out: String::new(),
        code: code_vec,
        num: num_vec,
        done: Vec::new(),
    };

    // Count possible codes for all numbers of symbols, add up counts
    let mut sum: BigT = 0;
    for n in 2..=syms {
        let got = state.count(n, 2, 1);
        sum = sum.wrapping_add(got);
        if got == OVERFLOW || sum < got {
            eprintln!("abort: overflow in code counting");
            process::exit(1);
        }
    }

    // Print total codes summary
    if max < syms - 1 {
        println!(
            "{sum} total codes for 2 to {syms} symbols ({max}-bit length limit)"
        );
    } else {
        println!("{sum} total codes for 2 to {syms} symbols (no length limit)");
    }

    // Allocate done array for been_here() — each entry is a Tab with an
    // empty bit vector that grows on demand
    if syms != 2 {
        state.done = (0..array_size).map(|_| Tab::new()).collect();
    }

    // Reduce root to max length if needed
    if state.root > max {
        state.root = max;
    }

    // Find and show maximum inflate table usage
    if (syms as u64) < (1u64 << (state.root + 1)) {
        state.enough(syms);
    } else {
        eprint!("cannot handle minimum code lengths > root");
    }

    Ok(())
}
