//! The inflate state tag: the Rust port of the C `inflate_mode` enum.
//!
//! `inflate()` is a *resumable* state machine. A caller may hand it a single
//! byte of input at a time and offer a single byte of output space at a time,
//! so every point at which the decoder can run out of either has to be a
//! named, saveable state. The reference implementation names those states in
//! the `inflate_mode` enum at `inflate.h` L20-L53 and records the legal
//! transitions between them in the diagram at `inflate.h` L55-L78. [`Mode`]
//! reproduces both: variant for variant, in order, and value for value.
//!
//! # Position in the crate
//!
//! This is the foundational module of the `inflate` tree. It has no
//! intra-crate dependencies and names nothing outside `core`, which is why it
//! can be read and reviewed on its own. Everything else in the subsystem
//! depends on it:
//!
//! * the driver (`inflate/mod.rs`) matches exhaustively over [`Mode`] and
//!   relies on its ordering and on its discriminant range,
//! * the state (`inflate/state.rs`) stores one [`Mode`] and resets it to
//!   [`Mode::Head`], mirroring `inflateResetKeep` at `inflate.c` L110,
//! * the header parser (`inflate/header.rs`) walks [`Mode::Head`] through
//!   [`Mode::Dict`],
//! * the fast decode loop (`inflate/inffast.rs`) sets [`Mode::Type`] at
//!   end-of-block and [`Mode::Bad`] on a malformed stream,
//! * `infback.rs` reuses a subset of these same states instead of declaring
//!   its own, exactly as `infback.c` reuses the same C enum.
//!
//! # What must not change, and why
//!
//! Three properties of this type are load-bearing. Each is asserted by the
//! tests at the bottom of this file, because breaking any of them silently
//! breaks decompression rather than failing to compile.
//!
//! 1. **The variant set.** Exactly 32 states, no more and no fewer. Rust's
//!    exhaustiveness checking is the whole reason this is an `enum`: a state
//!    added without a matching arm in the driver is a compile error here,
//!    where in C it was a silent fall-through. There is therefore no catch-all
//!    variant, no `Unknown(..)`, and no `#[non_exhaustive]`.
//! 2. **The declaration order.** `inflate()`'s epilogue compares modes
//!    *ordinally*, not by equality: `inflate.c` L1133-L1134 tests
//!    `state->mode < BAD` for "no error yet" and `state->mode < CHECK` for
//!    "not in the trailer yet". [`Ord`] is derived so that consumers can write
//!    the same comparison, which makes the declaration order part of this
//!    module's contract. Reordering the variants for tidiness would change
//!    which streams get a window update on the way out.
//! 3. **The discriminant values.** They run over the dense block
//!    16180..=16211. The implausible base is not decoration: `inflateStateCheck`
//!    (`inflate.c` L88-L98) validates a caller-supplied stream by testing
//!    `state->mode < HEAD || state->mode > SYNC`, which is zlib's cheap way of
//!    noticing that `z_stream.state` points at foreign, stale, or already-freed
//!    memory rather than at an inflate state. [`Mode::from_raw`] and
//!    [`Mode::is_valid_tag`] are that test, and they keep the base value so the
//!    port fails on exactly the inputs the reference fails on.
//!
//! Note what is *not* claimed: none of this is ABI-visible. `inflate_state`
//! lives behind the opaque `z_stream.state` pointer, so no caller can observe
//! these discriminants and nothing here is `#[repr(C)]`, `#[no_mangle]` or
//! `extern "C"`. Preserving 16180 is a fidelity and debuggability decision, not
//! a layout requirement — which is also why the *in-memory width* of the tag is
//! deliberately left to the compiler instead of being pinned with a `#[repr]`
//! attribute: `Mode as i32` yields the documented value for a fieldless enum
//! whatever tag width the compiler picks, and the reference struct's use of a
//! C `int` here is an artefact of C enums, not a contract.

use core::fmt;

/// One of the 32 states `inflate()` can be suspended in.
///
/// Ported from the `inflate_mode` enum at `inflate.h` L20-L53. Variant order
/// and discriminant values are those of the reference implementation; see the
/// module documentation for why both are contractual.
///
/// # Naming
///
/// C spells these states in `SCREAMING_SNAKE_CASE`; Rust spells them in
/// `UpperCamelCase`. The mapping is mechanical (`HEAD` becomes `Head`, `EXLEN`
/// becomes `ExLen`, `CODELENS` becomes `CodeLens`, and so on) with exactly two
/// deliberate exceptions, both of them C names ending in an underscore that
/// would read as a typo in Rust:
///
/// | C name  | Rust name              |
/// |---------|------------------------|
/// | `COPY_` | [`Mode::CopyBlock`]    |
/// | `LEN_`  | [`Mode::LenFirst`]     |
///
/// [`Mode::c_name`] returns the exact C spelling for every variant, so a
/// maintainer diffing against the reference — or reading a trace next to
/// `test/infcover.c` output — never has to guess.
///
/// # Reading the variant documentation
///
/// Each variant carries the reference comment for its state. The `i:` / `o:` /
/// `i/o:` prefixes are the reference's own annotations and mean: `i:` the state
/// is waiting for *input* bits, `o:` it is waiting for *output* space, `i/o:`
/// either can be the reason it suspended. Five states carry no prefix in the
/// reference — [`Mode::Dict`], [`Mode::Done`], [`Mode::Bad`], [`Mode::Mem`] and
/// [`Mode::Sync`], none of which is waiting on the stream — and none is
/// invented for them here.
///
/// # Legal transitions
///
/// Transcribed from `inflate.h` L55-L78. The names are the C ones, so that the
/// diagram can be diffed against the header directly:
///
/// ```text
///     State transitions between above modes -
///
///     (most modes can go to BAD or MEM on error -- not shown for clarity)
///
///     Process header:
///         HEAD -> (gzip) or (zlib) or (raw)
///         (gzip) -> FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT ->
///                   HCRC -> TYPE
///         (zlib) -> DICTID or TYPE
///         DICTID -> DICT -> TYPE
///         (raw) -> TYPEDO
///     Read deflate blocks:
///             TYPE -> TYPEDO -> STORED or TABLE or LEN_ or CHECK
///             STORED -> COPY_ -> COPY -> TYPE
///             TABLE -> LENLENS -> CODELENS -> LEN_
///             LEN_ -> LEN
///     Read deflate codes in fixed or dynamic block:
///                 LEN -> LENEXT or LIT or TYPE
///                 LENEXT -> DIST -> DISTEXT -> MATCH -> LEN
///                 LIT -> LEN
///     Process trailer:
///         CHECK -> LENGTH -> DONE
/// ```
///
/// # Examples
///
/// The ordinal comparisons the driver's epilogue is built on:
///
/// ```
/// use zlib_rs::Mode;
///
/// // `inflate.c` L1133: `state->mode < BAD` means "no error has been latched".
/// assert!(Mode::Len.is_error_free());
/// assert!(!Mode::Bad.is_error_free());
///
/// // `inflate.c` L1134: `state->mode < CHECK` means "not into the trailer yet".
/// assert!(Mode::Match.is_before_trailer());
/// assert!(!Mode::Length.is_before_trailer());
///
/// // The derived ordering is what lets the driver spell the test the way C does.
/// assert!(Mode::Len < Mode::Bad);
/// ```
///
/// Validating an untrusted tag the way `inflateStateCheck` does:
///
/// ```
/// use zlib_rs::Mode;
///
/// assert_eq!(Mode::from_raw(16180), Some(Mode::Head));
/// assert_eq!(Mode::from_raw(0), None);
/// assert!(!Mode::is_valid_tag(i32::MIN));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    /// `i:` waiting for magic header. (C `HEAD`, `inflate.h` L21.)
    ///
    /// The reset state: `inflateResetKeep` assigns it at `inflate.c` L110 and
    /// `inflateInit2_` pre-seeds it at `inflate.c` L205 so that the freshly
    /// allocated state passes the tag check. It is the low end of the tag range
    /// `inflateStateCheck` accepts.
    Head = 16180,
    /// `i:` waiting for method and flags (gzip). (C `FLAGS`, `inflate.h` L22.)
    Flags,
    /// `i:` waiting for modification time (gzip). (C `TIME`, `inflate.h` L23.)
    Time,
    /// `i:` waiting for extra flags and operating system (gzip).
    /// (C `OS`, `inflate.h` L24.)
    Os,
    /// `i:` waiting for extra length (gzip). (C `EXLEN`, `inflate.h` L25.)
    ExLen,
    /// `i:` waiting for extra bytes (gzip). (C `EXTRA`, `inflate.h` L26.)
    Extra,
    /// `i:` waiting for end of file name (gzip). (C `NAME`, `inflate.h` L27.)
    Name,
    /// `i:` waiting for end of comment (gzip). (C `COMMENT`, `inflate.h` L28.)
    Comment,
    /// `i:` waiting for header crc (gzip). (C `HCRC`, `inflate.h` L29.)
    HCrc,
    /// `i:` waiting for dictionary check value. (C `DICTID`, `inflate.h` L30.)
    DictId,
    /// Waiting for `inflateSetDictionary()` call. (C `DICT`, `inflate.h` L31.)
    ///
    /// Carries no `i:` / `o:` prefix in the reference because it is waiting on
    /// the *caller*, not on the stream: `inflate()` returns `Z_NEED_DICT` and
    /// stays here until the dictionary arrives.
    Dict,
    /// `i:` waiting for type bits, including last-flag bit.
    /// (C `TYPE`, `inflate.h` L32.)
    ///
    /// Also reported to the caller: `inflate.c` L1148 adds 128 to
    /// `strm.data_type` when the stream suspends in this state, and
    /// `inffast.rs` returns to it at end-of-block.
    Type,
    /// `i:` same, but skip check to exit inflate on new block.
    /// (C `TYPEDO`, `inflate.h` L33.)
    TypeDo,
    /// `i:` waiting for stored size (length and complement).
    /// (C `STORED`, `inflate.h` L34.)
    Stored,
    /// `i/o:` same as [`Mode::Copy`] below, but only first time in.
    /// (C `COPY_`, `inflate.h` L35.)
    ///
    /// **Renamed.** The reference name ends in an underscore, which is not
    /// idiomatic in Rust and reads as a typo, so `COPY_` is spelled
    /// `CopyBlock` here; [`Mode::c_name`] still reports `"COPY_"`.
    /// `inflate.c` L1149 adds 256 to `strm.data_type` for this state.
    CopyBlock,
    /// `i/o:` waiting for input or output to copy stored block.
    /// (C `COPY`, `inflate.h` L36.)
    Copy,
    /// `i:` waiting for dynamic block table lengths.
    /// (C `TABLE`, `inflate.h` L37.)
    Table,
    /// `i:` waiting for code length code lengths.
    /// (C `LENLENS`, `inflate.h` L38.)
    LenLens,
    /// `i:` waiting for length/lit and distance code lengths.
    /// (C `CODELENS`, `inflate.h` L39.)
    CodeLens,
    /// `i:` same as [`Mode::Len`] below, but only first time in.
    /// (C `LEN_`, `inflate.h` L40.)
    ///
    /// **Renamed.** As with [`Mode::CopyBlock`], the trailing underscore is
    /// dropped: `LEN_` is spelled `LenFirst` here and [`Mode::c_name`] still
    /// reports `"LEN_"`. `inflate.c` L1149 adds 256 to `strm.data_type` for
    /// this state.
    LenFirst,
    /// `i:` waiting for length/lit/eob code. (C `LEN`, `inflate.h` L41.)
    Len,
    /// `i:` waiting for length extra bits. (C `LENEXT`, `inflate.h` L42.)
    LenExt,
    /// `i:` waiting for distance code. (C `DIST`, `inflate.h` L43.)
    Dist,
    /// `i:` waiting for distance extra bits. (C `DISTEXT`, `inflate.h` L44.)
    DistExt,
    /// `o:` waiting for output space to copy string.
    /// (C `MATCH`, `inflate.h` L45.)
    Match,
    /// `o:` waiting for output space to write literal.
    /// (C `LIT`, `inflate.h` L46.)
    Lit,
    /// `i:` waiting for 32-bit check value. (C `CHECK`, `inflate.h` L47.)
    ///
    /// The first trailer state, and therefore the threshold
    /// [`Mode::is_before_trailer`] compares against.
    Check,
    /// `i:` waiting for 32-bit length (gzip). (C `LENGTH`, `inflate.h` L48.)
    Length,
    /// Finished check, done -- remain here until reset.
    /// (C `DONE`, `inflate.h` L49.)
    Done,
    /// Got a data error -- remain here until reset.
    /// (C `BAD`, `inflate.h` L50.)
    ///
    /// The first error state, and therefore the threshold
    /// [`Mode::is_error_free`] compares against. It is the single most-assigned
    /// state in the reference: 21 assignments in `inflate.c`, 12 in `infback.c`
    /// and 4 in `inffast.c`.
    Bad,
    /// Got an `inflate()` memory error -- remain here until reset.
    /// (C `MEM`, `inflate.h` L51.)
    ///
    /// Non-recoverable: the reference notes at `inflate.c` L1129 that a memory
    /// error from `inflate()` cannot be resumed from.
    Mem,
    /// Looking for synchronization bytes to restart `inflate()`.
    /// (C `SYNC`, `inflate.h` L52.)
    ///
    /// Entered only by `inflateSync` (`inflate.c` L1277-L1278, which enters it at
    /// most once per synchronisation attempt), and the high end of the tag range
    /// `inflateStateCheck` accepts.
    Sync,
}

impl Mode {
    /// Every state, once, in declaration order — which is also ascending
    /// discriminant order and the order of `inflate.h` L21-L52.
    ///
    /// Exposed so that the differential harness and the tests can walk the
    /// whole state space without restating it, and so that the "dense block of
    /// 32 tags" invariant can be checked mechanically rather than by eye. The
    /// length is part of the type, so adding a state and appending it here
    /// without widening the array is a compile error.
    pub const ALL: [Self; 32] = [
        Self::Head,
        Self::Flags,
        Self::Time,
        Self::Os,
        Self::ExLen,
        Self::Extra,
        Self::Name,
        Self::Comment,
        Self::HCrc,
        Self::DictId,
        Self::Dict,
        Self::Type,
        Self::TypeDo,
        Self::Stored,
        Self::CopyBlock,
        Self::Copy,
        Self::Table,
        Self::LenLens,
        Self::CodeLens,
        Self::LenFirst,
        Self::Len,
        Self::LenExt,
        Self::Dist,
        Self::DistExt,
        Self::Match,
        Self::Lit,
        Self::Check,
        Self::Length,
        Self::Done,
        Self::Bad,
        Self::Mem,
        Self::Sync,
    ];

    /// The number of distinct inflate states: 32.
    ///
    /// Derived from [`Mode::ALL`] rather than written down, so the two cannot
    /// disagree.
    pub const COUNT: usize = Self::ALL.len();

    /// The lowest tag value a live inflate state can hold: [`Mode::Head`], 16180.
    ///
    /// Together with [`Mode::MAX_DISCRIMINANT`] this is the range that
    /// `inflateStateCheck` accepts (`inflate.c` L95, which rejects a stream when
    /// `state->mode < HEAD || state->mode > SYNC`). `inflate/mod.rs` reaches the
    /// pair through [`Mode::is_valid_tag`] when it validates a caller's stream.
    pub const MIN_DISCRIMINANT: i32 = Self::Head.as_raw();

    /// The highest tag value a live inflate state can hold: [`Mode::Sync`], 16211.
    ///
    /// See [`Mode::MIN_DISCRIMINANT`] for what the pair is used for.
    pub const MAX_DISCRIMINANT: i32 = Self::Sync.as_raw();

    /// This state's tag value, in the numbering the reference implementation
    /// uses: 16180 for [`Mode::Head`] through 16211 for [`Mode::Sync`].
    ///
    /// The value is the enum discriminant, so the ordering of the returned
    /// integers is the same as the [`Ord`] ordering of the states themselves.
    /// That equivalence is what lets the `const` predicates below compare tags
    /// while callers compare states, and it is what the C code relies on when it
    /// compares `state->mode` as an `int`.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self as i32
    }

    /// The state a tag value names, or `None` if the value does not name one.
    ///
    /// Written as an explicit, exhaustive match on purpose. Reinterpreting an
    /// integer as an enum with a transmute would be undefined behaviour for any
    /// value outside 16180..=16211, and this crate is `#![forbid(unsafe_code)]`
    /// precisely so that such a shortcut is not available. The literals below
    /// are pinned to the variants by the round-trip test at the bottom of this
    /// file, which walks [`Mode::ALL`] and asserts that every state converts
    /// back to itself.
    ///
    /// This is the conversion `inflate/mod.rs` needs to reproduce
    /// `inflateStateCheck` (`inflate.c` L88-L98): a tag read back out of a
    /// caller-supplied stream is untrusted, and rejecting it is how the port
    /// notices foreign, stale, or already-freed memory. See
    /// [`Mode::is_valid_tag`] for the predicate form and [`TryFrom`] for the
    /// fallible-conversion form.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            16180 => Some(Self::Head),
            16181 => Some(Self::Flags),
            16182 => Some(Self::Time),
            16183 => Some(Self::Os),
            16184 => Some(Self::ExLen),
            16185 => Some(Self::Extra),
            16186 => Some(Self::Name),
            16187 => Some(Self::Comment),
            16188 => Some(Self::HCrc),
            16189 => Some(Self::DictId),
            16190 => Some(Self::Dict),
            16191 => Some(Self::Type),
            16192 => Some(Self::TypeDo),
            16193 => Some(Self::Stored),
            16194 => Some(Self::CopyBlock),
            16195 => Some(Self::Copy),
            16196 => Some(Self::Table),
            16197 => Some(Self::LenLens),
            16198 => Some(Self::CodeLens),
            16199 => Some(Self::LenFirst),
            16200 => Some(Self::Len),
            16201 => Some(Self::LenExt),
            16202 => Some(Self::Dist),
            16203 => Some(Self::DistExt),
            16204 => Some(Self::Match),
            16205 => Some(Self::Lit),
            16206 => Some(Self::Check),
            16207 => Some(Self::Length),
            16208 => Some(Self::Done),
            16209 => Some(Self::Bad),
            16210 => Some(Self::Mem),
            16211 => Some(Self::Sync),
            _ => None,
        }
    }

    /// Whether `raw` is a tag value a live inflate state can hold.
    ///
    /// This is the range test in `inflateStateCheck` (`inflate.c` L95). Because
    /// the 32 discriminants form a dense block with no gaps, "inside
    /// [`Mode::MIN_DISCRIMINANT`]..=[`Mode::MAX_DISCRIMINANT`]" and "names a
    /// state" are the same predicate; it is spelled in terms of
    /// [`Mode::from_raw`] so that the two can never drift apart.
    #[must_use]
    pub const fn is_valid_tag(raw: i32) -> bool {
        Self::from_raw(raw).is_some()
    }

    /// Whether no error has been latched yet, i.e. `self < Mode::Bad`.
    ///
    /// The named form of the `state->mode < BAD` test in `inflate()`'s epilogue
    /// (`inflate.c` L1133), where it decides — together with
    /// [`Mode::is_before_trailer`] — whether the sliding window still needs
    /// updating on the way out. [`Mode::Done`] counts as error-free: a finished
    /// stream is not a failed one, and the reference relies on that.
    ///
    /// The comparison is written on the raw tags because `<` on the enum goes
    /// through [`PartialOrd`], which cannot be called from a `const fn`. The
    /// derived ordering and the tag ordering agree by construction (see
    /// [`Mode::as_raw`]), and C likewise compares the enum as an `int`.
    #[must_use]
    pub const fn is_error_free(self) -> bool {
        self.as_raw() < Self::Bad.as_raw()
    }

    /// Whether the stream has not reached the trailer yet, i.e.
    /// `self < Mode::Check`.
    ///
    /// The named form of the `state->mode < CHECK` test in `inflate()`'s
    /// epilogue (`inflate.c` L1134). The trailer states are [`Mode::Check`],
    /// [`Mode::Length`] and [`Mode::Done`]; everything before them is still
    /// producing output. See [`Mode::is_error_free`] for the note on why the
    /// comparison is spelled on the raw tags.
    #[must_use]
    pub const fn is_before_trailer(self) -> bool {
        self.as_raw() < Self::Check.as_raw()
    }

    /// The reference implementation's spelling of this state, exactly as
    /// `inflate.h` L21-L52 writes it.
    ///
    /// [`Debug`] prints the Rust name; this prints the C one. Both are wanted:
    /// a maintainer comparing a trace from this port against a trace from the C
    /// oracle — or against `test/infcover.c`, which includes `inflate.h`
    /// directly and drives the state machine hard — should not have to
    /// translate names by hand. It is also where the two renamed states
    /// ([`Mode::CopyBlock`] for `COPY_` and [`Mode::LenFirst`] for `LEN_`) are
    /// reconciled with the header.
    ///
    /// The exhaustive match is deliberate: it is what makes adding a state
    /// without revisiting this module a compile error rather than a silent
    /// omission.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Head => "HEAD",
            Self::Flags => "FLAGS",
            Self::Time => "TIME",
            Self::Os => "OS",
            Self::ExLen => "EXLEN",
            Self::Extra => "EXTRA",
            Self::Name => "NAME",
            Self::Comment => "COMMENT",
            Self::HCrc => "HCRC",
            Self::DictId => "DICTID",
            Self::Dict => "DICT",
            Self::Type => "TYPE",
            Self::TypeDo => "TYPEDO",
            Self::Stored => "STORED",
            Self::CopyBlock => "COPY_",
            Self::Copy => "COPY",
            Self::Table => "TABLE",
            Self::LenLens => "LENLENS",
            Self::CodeLens => "CODELENS",
            Self::LenFirst => "LEN_",
            Self::Len => "LEN",
            Self::LenExt => "LENEXT",
            Self::Dist => "DIST",
            Self::DistExt => "DISTEXT",
            Self::Match => "MATCH",
            Self::Lit => "LIT",
            Self::Check => "CHECK",
            Self::Length => "LENGTH",
            Self::Done => "DONE",
            Self::Bad => "BAD",
            Self::Mem => "MEM",
            Self::Sync => "SYNC",
        }
    }
}

/// The error [`Mode`]'s [`TryFrom`] implementation returns for an integer that
/// is not an inflate state tag.
///
/// Carries the rejected value so that a caller can report it. There is
/// deliberately no `core::error::Error` implementation: that trait only reached
/// `core` in Rust 1.81, one release after this workspace's declared MSRV of
/// 1.80, and the core crate is `no_std` so `std::error::Error` is not available
/// either. [`fmt::Display`] plus [`Debug`] cover the diagnostic need without
/// breaking the MSRV contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidModeTag {
    /// The integer that did not name a state.
    raw: i32,
}

impl InvalidModeTag {
    /// The integer that was rejected.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.raw
    }
}

impl fmt::Display for InvalidModeTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not a valid inflate mode tag; expected {}..={}",
            self.raw,
            Mode::MIN_DISCRIMINANT,
            Mode::MAX_DISCRIMINANT
        )
    }
}

impl TryFrom<i32> for Mode {
    type Error = InvalidModeTag;

    /// Converts a raw tag value into a state, rejecting anything that is not
    /// one.
    ///
    /// The fallible-conversion form of [`Mode::from_raw`], for callers that
    /// want to propagate the offending value. [`Mode::from_raw`] is the one to
    /// use in a `const` or hot path, because a trait method cannot be `const`.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidModeTag`] when `raw` lies outside
    /// [`Mode::MIN_DISCRIMINANT`]..=[`Mode::MAX_DISCRIMINANT`], which is the
    /// condition `inflateStateCheck` treats as "this is not one of our streams"
    /// (`inflate.c` L95).
    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::from_raw(raw).ok_or(InvalidModeTag { raw })
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidModeTag, Mode};
    use core::fmt::{self, Write};

    /// The reference spellings, transcribed from `inflate.h` L21-L52 in source
    /// order. Kept separate from [`Mode::c_name`] so that the two are an
    /// independent check on each other rather than the same list read twice.
    const C_NAMES: [&str; 32] = [
        "HEAD", "FLAGS", "TIME", "OS", "EXLEN", "EXTRA", "NAME", "COMMENT", "HCRC", "DICTID",
        "DICT", "TYPE", "TYPEDO", "STORED", "COPY_", "COPY", "TABLE", "LENLENS", "CODELENS",
        "LEN_", "LEN", "LENEXT", "DIST", "DISTEXT", "MATCH", "LIT", "CHECK", "LENGTH", "DONE",
        "BAD", "MEM", "SYNC",
    ];

    /// A `core`-only sink, so the [`fmt::Display`] output can be inspected in a
    /// crate that does not link `alloc`.
    struct Sink {
        buf: [u8; 128],
        len: usize,
    }

    impl Sink {
        const fn new() -> Self {
            Self {
                buf: [0; 128],
                len: 0,
            }
        }

        fn as_str(&self) -> Option<&str> {
            self.buf
                .get(..self.len)
                .and_then(|written| core::str::from_utf8(written).ok())
        }
    }

    impl Write for Sink {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            match self.buf.get_mut(self.len..end) {
                Some(destination) => {
                    destination.copy_from_slice(s.as_bytes());
                    self.len = end;
                    Ok(())
                }
                None => Err(fmt::Error),
            }
        }
    }

    /// The reference enum has 32 members (`inflate.h` L21-L52). It is an easy
    /// list to miscount by hand -- secondary descriptions of it have -- so the
    /// count is pinned here rather than trusted.
    #[test]
    fn there_are_exactly_thirty_two_states() {
        assert_eq!(Mode::COUNT, 32);
        assert_eq!(Mode::ALL.len(), 32);
        assert_eq!(C_NAMES.len(), Mode::COUNT);
    }

    #[test]
    fn discriminants_are_the_dense_block_the_reference_uses() {
        assert_eq!(Mode::Head.as_raw(), 16180);
        assert_eq!(Mode::Sync.as_raw(), 16211);
        assert_eq!(Mode::MIN_DISCRIMINANT, 16180);
        assert_eq!(Mode::MAX_DISCRIMINANT, 16211);

        // No gaps and no duplicates: the tags are exactly 16180..=16211, in
        // declaration order. Comparing the counts first means `zip` cannot hide
        // a short `ALL`.
        let expected = Mode::MIN_DISCRIMINANT..=Mode::MAX_DISCRIMINANT;
        assert_eq!(expected.clone().count(), Mode::COUNT);
        for (state, raw) in Mode::ALL.iter().zip(expected) {
            assert_eq!(state.as_raw(), raw, "{} moved off its tag", state.c_name());
        }
    }

    #[test]
    fn all_is_strictly_ascending() {
        for (earlier, later) in Mode::ALL.iter().zip(Mode::ALL.iter().skip(1)) {
            assert!(
                earlier < later,
                "{} must precede {}",
                earlier.c_name(),
                later.c_name()
            );
            assert!(earlier.as_raw() < later.as_raw());
        }
    }

    /// The ordinal facts `inflate()`'s epilogue depends on (`inflate.c`
    /// L1133-L1134). A maintainer who reorders the variants for tidiness breaks
    /// decompression, and this is where that shows up.
    #[test]
    fn declaration_order_matches_the_reference_ordinal_tests() {
        assert!(Mode::Head < Mode::Bad);
        assert!(Mode::Len < Mode::Check);
        assert!(Mode::Check < Mode::Bad);
        assert!(Mode::Done < Mode::Bad);
        assert!(Mode::Bad < Mode::Mem);
        assert!(Mode::Mem < Mode::Sync);

        // The header states precede the block states, which precede the code
        // states, which precede the trailer -- the four sections of the
        // transition diagram at `inflate.h` L55-L78.
        assert!(Mode::Head < Mode::Dict);
        assert!(Mode::Dict < Mode::Type);
        assert!(Mode::Type < Mode::CopyBlock);
        assert!(Mode::CopyBlock < Mode::LenFirst);
        assert!(Mode::LenFirst < Mode::Len);
        assert!(Mode::Len < Mode::Lit);
        assert!(Mode::Lit < Mode::Check);
        assert!(Mode::Check < Mode::Length);
        assert!(Mode::Length < Mode::Done);
    }

    #[test]
    fn is_error_free_partitions_the_states_at_bad() {
        for state in Mode::ALL {
            assert_eq!(state.is_error_free(), state < Mode::Bad);
        }
        assert!(Mode::Head.is_error_free());
        assert!(Mode::Len.is_error_free());
        // A finished stream is not a failed one: DONE precedes BAD.
        assert!(Mode::Done.is_error_free());
        assert!(!Mode::Bad.is_error_free());
        assert!(!Mode::Mem.is_error_free());
        assert!(!Mode::Sync.is_error_free());
    }

    #[test]
    fn is_before_trailer_partitions_the_states_at_check() {
        for state in Mode::ALL {
            assert_eq!(state.is_before_trailer(), state < Mode::Check);
        }
        assert!(Mode::Head.is_before_trailer());
        assert!(Mode::Match.is_before_trailer());
        assert!(Mode::Lit.is_before_trailer());
        assert!(!Mode::Check.is_before_trailer());
        assert!(!Mode::Length.is_before_trailer());
        assert!(!Mode::Done.is_before_trailer());
    }

    #[test]
    fn every_state_round_trips_through_its_tag() {
        for state in Mode::ALL {
            assert_eq!(Mode::from_raw(state.as_raw()), Some(state));
            assert!(Mode::is_valid_tag(state.as_raw()));
            assert_eq!(Mode::try_from(state.as_raw()), Ok(state));
        }
    }

    #[test]
    fn tags_outside_the_range_are_rejected_without_panicking() {
        let rejected = [
            Mode::MIN_DISCRIMINANT - 1,
            Mode::MAX_DISCRIMINANT + 1,
            0,
            -1,
            i32::MIN,
            i32::MAX,
        ];
        for raw in rejected {
            assert_eq!(Mode::from_raw(raw), None, "{raw} must not name a state");
            assert!(!Mode::is_valid_tag(raw));
            assert_eq!(Mode::try_from(raw), Err(InvalidModeTag { raw }));
        }
        // The two boundary values, spelled out, so the range is pinned from both
        // sides even if the constants above are ever mis-edited.
        assert_eq!(Mode::from_raw(16179), None);
        assert_eq!(Mode::from_raw(16212), None);
    }

    #[test]
    fn the_error_carries_the_rejected_value() {
        let failure = Mode::try_from(1234).err();
        assert_eq!(failure, Some(InvalidModeTag { raw: 1234 }));
        assert_eq!(failure.map(InvalidModeTag::raw), Some(1234));
    }

    #[test]
    fn the_error_message_names_the_value_and_the_range() {
        let mut sink = Sink::new();
        let failure = InvalidModeTag { raw: 42 };
        assert!(write!(sink, "{failure}").is_ok());
        assert_eq!(
            sink.as_str(),
            Some("42 is not a valid inflate mode tag; expected 16180..=16211")
        );
    }

    #[test]
    fn c_names_are_the_reference_spellings() {
        for (state, name) in Mode::ALL.iter().zip(C_NAMES) {
            assert_eq!(state.c_name(), name);
        }
        // The two states renamed on the way into Rust.
        assert_eq!(Mode::CopyBlock.c_name(), "COPY_");
        assert_eq!(Mode::LenFirst.c_name(), "LEN_");
    }

    #[test]
    fn states_and_their_c_names_are_all_distinct() {
        for (position, state) in Mode::ALL.iter().enumerate() {
            for other in Mode::ALL.iter().skip(position + 1) {
                assert_ne!(state, other);
                assert_ne!(state.c_name(), other.c_name());
            }
        }
    }

    /// The driver keeps a state in a local and compares it against the stored
    /// one, so the tag has to be [`Copy`] rather than a move-only value.
    #[test]
    fn states_are_copy_and_comparable() {
        let state = Mode::Len;
        let taken = state;
        assert_eq!(state, taken);
        assert_ne!(state, Mode::Bad);
    }

    /// The three states `inflate()` tests by equality when it publishes
    /// `strm.data_type` (`inflate.c` L1147-L1149: +128 for `TYPE`, +256 for
    /// `LEN_` or `COPY_`). All that has to hold here is that the three are
    /// distinct and reachable by name -- the arithmetic belongs to the driver.
    #[test]
    fn the_data_type_states_are_distinguishable() {
        assert_ne!(Mode::Type, Mode::LenFirst);
        assert_ne!(Mode::Type, Mode::CopyBlock);
        assert_ne!(Mode::LenFirst, Mode::CopyBlock);
        assert_eq!(Mode::Type.c_name(), "TYPE");
        assert_eq!(Mode::LenFirst.c_name(), "LEN_");
        assert_eq!(Mode::CopyBlock.c_name(), "COPY_");
    }
}
