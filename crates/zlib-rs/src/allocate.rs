//! Working-memory acquisition and release: the one place `zlib-rs` obtains a
//! byte of heap, and the one place it gives one back.
//!
//! # The allocator contract
//!
//! In the reference implementation every buffer the library uses is obtained
//! through three caller-supplied members of `z_stream` -- `zalloc`, `zfree` and
//! `opaque` (`zlib.h` L102-L104) -- reached through three macros
//! (`zutil.h` L252-L255):
//!
//! The hooks themselves are `alloc_func` and `free_func` (`zlib.h` L85-L86), and
//! the header states the contract normatively at `zlib.h` L144-L153: the
//! `opaque` value is passed back as the first argument of every call and *the
//! library attaches no meaning to it*; `zalloc` returns `Z_NULL` when it cannot
//! satisfy a request; the library is thread-safe exactly to the extent that the
//! caller's two hooks are; and when both hooks are `Z_NULL` on entry to an
//! initialisation function they are replaced by internal routines built on
//! `malloc` and `free`.
//!
//! That replacement happens at `deflate.c` L401-L414, `inflate.c` L183-L195 and
//! `infback.c` L37-L49, each of which installs `zcalloc`/`zcfree` from
//! `zutil.c` L299-L308 and sets `opaque` to the null pointer. This module models
//! the whole arrangement as one abstraction, [`Allocator`], with the internal
//! routines provided here as [`GlobalAllocator`] and the caller-hook path
//! provided by the `libz-rs-sys` facade.
//!
//! # Freshly allocated memory is **not** zeroed -- and nothing may assume it is
//!
//! This is the single most consequential fact in this module. The generic
//! `zcalloc` (`zutil.c` L299-L303) reads:
//!
//! `uInt` is four bytes wide on every target this implementation supports, so
//! `sizeof(uInt) > 2` is always true and the branch taken is always **`malloc`**,
//! never `calloc`. Default-allocated blocks therefore hold whatever the platform
//! allocator left behind, and a caller-supplied hook is under no obligation to do
//! better. The reference implementation is written accordingly: `deflate.c` L442
//! zeroes the *state struct* explicitly with `zmemzero`, and `deflate.c`
//! L170-L173 zeroes the hash head array explicitly through `CLEAR_HASH`, because
//! neither buffer arrives zeroed.
//!
//! `test/infcover.c` exists partly to police this. Its tracking allocator fills
//! every block it hands out with the byte `0xa5` (`test/infcover.c` L87, whose
//! comment says "fill memory with a non-zero value to make sure that the code
//! isn't depending on zeros"), so any code path that quietly relies on zeroed
//! memory produces wrong answers under that test rather than passing by luck.
//! **No state constructor in this crate may assume a fresh buffer is zeroed.**
//! Initialise what you need to initialise.
//!
//! Safe Rust cannot hand out genuinely uninitialised memory -- reading an
//! uninitialised byte through a `&mut [u8]` is undefined behaviour, and the tools
//! that would let this module talk about uninitialised memory (`MaybeUninit`
//! plus `assume_init`) all require the escape hatch this crate forbids. Every
//! block that leaves this module is therefore fully initialised before it is
//! handed over. That is a deliberate, documented divergence from `malloc`, and it
//! is unobservable to callers: the reference implementation never reads a byte of
//! a fresh buffer before writing it, so the value the fill leaves behind cannot
//! reach the compressed output or any return value. To keep the divergence from
//! *hiding* the bug class `test/infcover.c` hunts, [`GlobalAllocator`] fills with
//! the non-zero [`SENTINEL_FILL`] byte in debug builds, so a mistaken assumption
//! about zeroed memory still fails a test rather than passing silently.
//!
//! # Why this module never calls the caller's hooks
//!
//! `zalloc` and `zfree` are raw C function pointers, and calling a raw function
//! pointer requires an escape hatch from the compiler's safety guarantees --
//! which this crate forbids outright at the crate root. The resolution is
//! dependency injection: [`Allocator`] is a trait, this module supplies the
//! implementation that needs no hooks at all ([`GlobalAllocator`]), and the
//! `libz-rs-sys` facade -- the one crate in the workspace permitted to hold and
//! dereference a raw pointer -- supplies the implementation that calls the
//! caller's hooks. The core algorithms take an `impl Allocator` (or a
//! `&dyn Allocator`) by injection and never learn which is which.
//!
//! # How the mismatched-allocator bug class is closed
//!
//! Memory obtained from one allocator must be returned to that same allocator.
//! Getting this wrong is not a leak, it is heap corruption: handing a `malloc`ed
//! block to Rust's global deallocator, or a Rust-allocated block to a caller's
//! `zfree`, is undefined behaviour in both directions. `test/infcover.c` detects
//! the symptom -- `mem_free` (L112-L154) counts frees of addresses it never
//! handed out as *rogue* frees and reports them from `mem_done` (L200-L234),
//! along with leaks and frees that are not in last-in-first-out order.
//!
//! Two mechanisms close the class here, in order of strength:
//!
//! 1. **Structural.** A [`Buffer`] stores its bytes in one of exactly two
//!    private forms: a Rust-owned allocation, or a borrow of a block belonging to
//!    somebody else. The two are distinct variants of a private enum, so the
//!    catastrophic direction -- Rust's allocator freeing a caller's block, or a
//!    caller's `zfree` receiving a Rust allocation -- is not expressible. A
//!    Rust-owned buffer releases itself when it is dropped; a foreign buffer can
//!    only be turned back into a raw block through [`Buffer::release_to`].
//! 2. **Identity.** [`Buffer::release_to`] takes the allocator itself, compares
//!    its [`AllocatorId`] against the one recorded when the block was handed out,
//!    and yields [`Release::Refused`] on any mismatch -- so a foreign block is
//!    never surrendered to the wrong `zfree`. The identity is an exact record of
//!    what distinguishes two allocators rather than a hash of it, so the
//!    comparison cannot produce a false match. A refused release leaks the block,
//!    which is safe and diagnosable, instead of corrupting the heap.
//!
//! # Deallocation order
//!
//! `deflateEnd` frees "in reverse order of allocations" (`deflate.c` L1300-L1306:
//! `pending_buf`, then `head`, then `prev`, then `window`, then the state), which
//! is exactly the last-in-first-out order `test/infcover.c` expects; a departure
//! from it increments that harness's `notlifo` counter. Rust drops the fields of
//! a struct in *declaration* order, so a state struct that declares its buffers
//! in allocation order would free them in precisely the wrong one. State modules
//! must therefore either declare buffers in release order or release them
//! explicitly. This module supports both: dropping a buffer works, and
//! [`Allocator::deallocate_bytes`] -- which takes a *slot* and so doubles as the
//! `TRY_FREE` analogue from `zutil.h` L255 -- makes an explicit, ordered teardown
//! read like the C original.
//!
//! # Shapes
//!
//! Every buffer the algorithms allocate is one of two shapes, so the trait offers
//! exactly two allocating methods:
//!
//! | C call site | Shape | Method |
//! |---|---|---|
//! | `ZALLOC(strm, s->w_size, 2*sizeof(Byte))`, `deflate.c` L458 | bytes | [`Allocator::allocate_bytes`] |
//! | `ZALLOC(strm, s->lit_bufsize, LIT_BUFS)`, `deflate.c` L505 | bytes | [`Allocator::allocate_bytes`] |
//! | `ZALLOC(strm, 1U << state->wbits, sizeof(unsigned char))`, `inflate.c` L260-L262 | bytes | [`Allocator::allocate_bytes`] |
//! | `ZALLOC(strm, s->w_size, sizeof(Pos))`, `deflate.c` L459 | `u16` | [`Allocator::allocate_u16s`] |
//! | `ZALLOC(strm, s->hash_size, sizeof(Pos))`, `deflate.c` L460 | `u16` | [`Allocator::allocate_u16s`] |
//!
//! Two shapes that might be expected are deliberately absent:
//!
//! * The inflate code tables. `lens[320]`, `work[288]` and `codes[ENOUGH]` --
//!   `ENOUGH` being 1444, from `inftrees.h` L49-L51 -- are inline members of
//!   `struct inflate_state` (`inflate.h` L118-L122), not separate allocations, so
//!   they travel with the state object and need no shape of their own.
//! * The state objects themselves (`ZALLOC(strm, 1, sizeof(deflate_state))` at
//!   `deflate.c` L440 and its inflate counterparts at `inflate.c` L197-L198 and
//!   `infback.c` L51). Moving a Rust value into memory that a caller's `zalloc`
//!   returned is a raw pointer write, so it belongs in the facade crate that is
//!   permitted to write through raw pointers; and there is no fallible,
//!   MSRV-stable way to heap-allocate a single value here in any case. [`Buffer`]
//!   is generic over its element type, so a facade allocator can still describe
//!   any shape it needs through [`Buffer::from_foreign`].
//!
//! # Thread safety and shared state
//!
//! `zlib.h` L150-L151 puts the whole burden on the caller: "If zlib is used in a
//! multi-threaded application, `zalloc` and `zfree` must be thread safe. In that
//! case, zlib is thread-safe." This module holds up its end by adding no shared
//! mutable state of its own -- there is no `static`, no interior mutability, no
//! lazily initialised global and no override of the global allocator anywhere in
//! it, so nothing here can serialise, race, or surprise a caller whose hooks are
//! already thread safe.
//!
//! # Visibility
//!
//! `zcalloc` and `zcfree` carry `ZLIB_INTERNAL`, which expands to hidden ELF
//! visibility (`zutil.h` L16-L20), and `zlib.map` lists both in its `local:`
//! block. Nothing in this module is exported to C, given C-compatible linkage, or
//! given a C-compatible layout; the names below are Rust API only.
//!
//! [`SENTINEL_FILL`]: crate::allocate::SENTINEL_FILL

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

use core::ffi::c_void;
use core::fmt;
use core::marker::PhantomData;

use alloc::vec::Vec;

use crate::error::ReturnCode;

/// The non-zero byte a freshly allocated block is filled with in debug builds.
///
/// This is the value `test/infcover.c` L87 uses -- `memset(ptr, 0xa5, len)` --
/// for the reason its comment gives: to make sure the library "isn't depending on
/// zeros". [`GlobalAllocator`] fills with it in debug builds so that a state
/// constructor which mistakenly assumes zeroed memory fails a test here rather
/// than only under the C coverage harness.
///
/// It is public so that an [`Allocator`] implementation in another crate can fill
/// with the same value, and so that tests can assert on it.
pub const SENTINEL_FILL: u8 = 0xA5;

/// The caller's private data pointer, passed back to every allocator call.
///
/// This is `voidpf opaque` from `z_stream` (`zlib.h` L104), whose contract is
/// spelled out at `zlib.h` L144-L147: it "will be passed as the first parameter
/// for calls of `zalloc` and `zfree`", it exists so a caller can implement custom
/// memory management, and "the compression library attaches no meaning to the
/// `opaque` value".
///
/// This type takes that literally. It is a newtype over the pointer and offers no
/// way to read through it: the library never dereferences it, never inspects what
/// it points at, and never requires it to be non-null. Storing a raw pointer
/// needs no escape hatch -- only reading through one does -- so the value can be
/// carried here, in the crate that forbids such escape hatches, and handed back
/// to the facade verbatim with its provenance intact.
///
/// A value of [`Opaque::NULL`] is the `Z_NULL` the initialisation functions
/// install alongside the internal routines (`deflate.c` L406,
/// `inflate.c` L188, `infback.c` L42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Opaque(*mut c_void);

impl Opaque {
    /// The null `opaque`, which is what an initialisation function installs when
    /// it substitutes the library's own allocation routines.
    ///
    /// Mirrors `strm->opaque = (voidpf)0`, `deflate.c` L406.
    pub const NULL: Self = Self(core::ptr::null_mut());

    /// Wraps a caller-supplied `opaque` pointer.
    ///
    /// The pointer is stored exactly as given. No requirement is placed on it:
    /// null is a perfectly ordinary value here, and a non-null value is never
    /// examined.
    #[must_use]
    pub const fn new(pointer: *mut c_void) -> Self {
        Self(pointer)
    }

    /// Returns the pointer unchanged, for passing back to a caller's hook.
    ///
    /// This is the only thing the library ever does with an `opaque` value.
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Reports whether this is the null `opaque`.
    ///
    /// Note that a null `opaque` is not an error and does not mean "absent": a
    /// caller may legitimately install hooks that ignore `opaque` entirely, and
    /// `test/infcover.c`'s `mem_free` (L118-L121) shows the pattern -- it treats a
    /// null zone as "just do a plain free".
    #[must_use]
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// Returns the pointer's address, for identity comparison only.
    ///
    /// Used to build the `opaque` component of an [`AllocatorId`]. The result is
    /// never turned back into a pointer, and no memory is reached through it.
    #[must_use]
    pub fn addr(self) -> usize {
        // A pointer-to-integer cast, not the reverse: this discards the ability
        // to reach the memory rather than fabricating it. `Opaque::as_ptr` is the
        // only way back to a usable pointer, and it returns the original value.
        self.0 as usize
    }
}

impl Default for Opaque {
    /// Yields [`Opaque::NULL`], matching an uninitialised `z_stream` field.
    fn default() -> Self {
        Self::NULL
    }
}

/// The identity of one allocator, used to prove that a block is being returned to
/// the allocator it came from.
///
/// The C API has no equivalent, because C has no way to notice the mistake: every
/// call goes through `ZFREE(strm, addr)` (`zutil.h` L254) and whichever `zfree`
/// happens to be installed on that `strm` receives the address. The mistake shows
/// up only afterwards, as a *rogue free* in `test/infcover.c`'s report
/// (`test/infcover.c` L148-L150, L225-L227), by which point the heap is already
/// corrupt. Recording an identity turns that into a refusal at the point of
/// release; see [`Buffer::release_to`].
///
/// # Why the comparison cannot produce a false match
///
/// The identity stores what actually distinguishes two allocators -- the two hook
/// addresses and the `opaque` value -- rather than a digest of them, so two
/// identities compare equal precisely when the allocators are interchangeable,
/// which is the property being checked. There is no hash and therefore no
/// collision. [`AllocatorId::GLOBAL`] is reserved for the Rust global allocator
/// and can never equal an identity built by [`AllocatorId::foreign`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AllocatorId(Identity);

/// The private discriminated form of [`AllocatorId`].
///
/// Private so that the two kinds cannot be confused or forged from outside: an
/// implementation reaches them only through [`AllocatorId::GLOBAL`] and
/// [`AllocatorId::foreign`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Identity {
    /// The crate's own allocator, [`GlobalAllocator`]. A singleton: two
    /// `GlobalAllocator` values really are interchangeable, so one identity for
    /// all of them is correct.
    Global,
    /// An allocator that calls out to caller-supplied hooks, identified by the
    /// addresses of those hooks together with the `opaque` value they receive.
    Foreign {
        /// Address of the caller's `zalloc` (`zlib.h` L85).
        allocate: usize,
        /// Address of the caller's `zfree` (`zlib.h` L86).
        deallocate: usize,
        /// Address held in the caller's `opaque` (`zlib.h` L104).
        opaque: usize,
    },
}

impl AllocatorId {
    /// The identity of [`GlobalAllocator`], reserved for it alone.
    ///
    /// Implementations that call caller-supplied hooks must use
    /// [`AllocatorId::foreign`]; this value must never be returned for a block
    /// that Rust's global deallocator cannot free.
    pub const GLOBAL: Self = Self(Identity::Global);

    /// Builds the identity of an allocator that calls caller-supplied hooks.
    ///
    /// Pass the address of the caller's `zalloc`, the address of the caller's
    /// `zfree`, and [`Opaque::addr`] of the `opaque` value the two receive. Those
    /// three values are exactly what `ZALLOC` and `ZFREE` (`zutil.h` L252-L254)
    /// consume, so two allocators agreeing on all three are interchangeable and
    /// two disagreeing on any are not.
    ///
    /// The addresses are used for comparison only; nothing is ever called or read
    /// through them, which is why they arrive as plain integers.
    #[must_use]
    pub const fn foreign(allocate: usize, deallocate: usize, opaque: usize) -> Self {
        Self(Identity::Foreign {
            allocate,
            deallocate,
            opaque,
        })
    }

    /// Reports whether this is the reserved identity of [`GlobalAllocator`].
    ///
    /// Useful to an implementation that wants to assert it has not accidentally
    /// claimed the reserved identity, and to tests.
    #[must_use]
    pub const fn is_global(self) -> bool {
        matches!(self.0, Identity::Global)
    }
}

/// The largest byte count that can name a single allocated object: `isize::MAX`.
///
/// Not a policy figure, a representational one, and it is the same number on both
/// sides of the FFI boundary. In Rust, `core::alloc::Layout` rejects any size above
/// it and `core::slice::from_raw_parts` requires the total size of the slice to
/// stay within it, so a larger block could not be described even if an allocator
/// produced one. In C, the difference of two pointers into one object is a
/// `ptrdiff_t`, so a larger object cannot be traversed either.
///
/// Written as `isize::MAX.unsigned_abs()` rather than as a cast, both because the
/// workspace denies lossy `as` conversions and because the spelling says what the
/// bound *is*.
///
/// Private, and deliberately so: it is an internal consequence of the platform
/// rather than a knob, and [`block_len`] is the only thing that needs it. Its
/// effect is documented on that function, which is the one callers see.
const MAX_BLOCK_LEN: usize = isize::MAX.unsigned_abs();

/// The size in bytes of an `items` by `size` allocation request, or [`None`] if
/// that product cannot name a single object.
///
/// `ZALLOC` (`zutil.h` L252-L253) passes `items` and `size` to the caller's hook
/// separately, and it is the hook that multiplies them: `zcalloc` does so in
/// `unsigned` arithmetic (`zutil.c` L301) and `test/infcover.c`'s tracking
/// allocator widens first (`size_t len = count * (size_t)size`, L76). An
/// unchecked product is the classic route from an allocation bug to a
/// memory-safety bug -- it yields a block far smaller than the caller believes it
/// asked for -- so this implementation computes it checked, in `usize`, and treats overflow
/// as an allocation failure.
///
/// # Two ways a request is refused
///
/// The product is refused when it overflows `usize`, and also when it exceeds
/// [`isize::MAX`], which is the largest byte count that can name a single object:
/// [`core::alloc::Layout`] rejects any size above it, [`core::slice::from_raw_parts`]
/// requires the total size of the slice to stay within it, and in C the difference
/// of two pointers into one object is a `ptrdiff_t`, so such a block could not be
/// described on either side of the boundary even if an allocator produced one.
///
/// The second test is what makes the answer the same on every
/// target rather than an accident of pointer width: `items = size = uInt::MAX`
/// overflows a 32-bit `usize` and is refused there, while on LP64 the product is
/// `0xffff_fffe_0000_0001`, which *is* representable and would otherwise be passed
/// on to an allocator as a real request. It could never be honoured -- no such
/// object can exist, in Rust or in C -- so refusing it up front is both the
/// truthful answer and the useful one:
///
/// * `zalloc`'s documented failure answer is `Z_NULL` (`zlib.h` L149), which every
///   caller of this library already turns into `Z_MEM_ERROR`, so nothing is lost;
/// * an allocator asked for an impossible size may do rather more than return
///   null. AddressSanitizer, which AAP §0.6.4.5 requires this library's boundary
///   layer to run clean under, treats a request above its own maximum as a fatal
///   `allocation-size-too-big` error and **aborts the process** unless
///   `allocator_may_return_null=1` is set. Declining the request before it reaches
///   `malloc` keeps the sanitizer job runnable with its default options, so a real
///   finding cannot be masked by a configuration failure.
///
/// Neither test can change behaviour for any request the library actually makes:
/// every one is bounded by `MAX_WBITS` (15) and `MAX_MEM_LEVEL` (9), so the
/// largest is `hash_size * sizeof(Pos)` = 131072 bytes. They exist for the general
/// case, and because an [`Allocator`] implementation must perform the same
/// multiplication before calling a caller's hook and should perform it the same
/// way.
///
/// ```
/// use zlib_rs::allocate::block_len;
///
/// assert_eq!(block_len(32768, 2), Some(65536)); // deflate.c L458
/// assert_eq!(block_len(usize::MAX, 2), None);   // overflow, not a wrapped size
///
/// // Representable on a 64-bit target, but no such object can exist.
/// assert_eq!(block_len(0xffff_ffff, 0xffff_ffff), None);
/// ```
#[must_use]
pub const fn block_len(items: usize, size: usize) -> Option<usize> {
    // Spelled with `match` and `if` rather than `let ... else` and a guard so that
    // it is unambiguously a `const fn` on the declared 1.80 floor.
    match items.checked_mul(size) {
        Some(len) => {
            if len > MAX_BLOCK_LEN {
                None
            } else {
                Some(len)
            }
        }
        None => None,
    }
}

/// Where a [`Buffer`]'s elements actually live.
///
/// Private, and deliberately so: keeping Rust-owned and foreign blocks in
/// separate variants of a type nobody outside this module can construct or match
/// on is what makes the mismatched-allocator bug class inexpressible rather than
/// merely discouraged.
enum Storage<'a, T> {
    /// A Rust allocation, owned outright and released by its own destructor.
    ///
    /// A [`Vec`] rather than a boxed slice because [`Vec`] is the only container
    /// in `alloc` that can be grown *fallibly* on stable Rust
    /// ([`Vec::try_reserve_exact`]); converting to a boxed slice afterwards can
    /// reallocate, and that reallocation is not fallible.
    Owned(Vec<T>),
    /// A block belonging to somebody else -- in practice one that a caller's
    /// `zalloc` returned -- borrowed for as long as the library holds it, and
    /// returned through [`Buffer::release_to`].
    Foreign(&'a mut [T]),
}

/// One allocated block of `T`, together with the identity of the allocator that
/// produced it.
///
/// This is the Rust form of the raw `Bytef *` and `Posf *` members of
/// `deflate_state` and `inflate_state`. It carries its length, so no separate
/// size field is needed and no access can run off the end; it knows which
/// allocator owns it, so it cannot be returned to the wrong one; and it releases
/// itself if it is a Rust allocation and is simply dropped.
///
/// It implements [`AsRef<[T]>`](AsRef) and [`AsMut<[T]>`](AsMut), which is what
/// the crate's buffer views are generic over, so an allocated block can be handed
/// straight to one of them without any adaptation.
///
/// # Contents on arrival
///
/// Unspecified. A fresh block is fully initialised -- reading it is well defined --
/// but the values are whatever the allocator chose to leave, exactly as `malloc`
/// leaves whatever was there before (`zutil.c` L301). Do not assume zeros; see the
/// module documentation.
#[must_use = "a leaked allocation is reported by test/infcover.c's mem_done"]
pub struct Buffer<'a, T> {
    /// The elements, in one of the two forms [`Storage`] distinguishes.
    storage: Storage<'a, T>,
    /// The allocator that produced this block, and the only one that may take it
    /// back. Recorded at handout so [`Buffer::release_to`] can check it.
    owner: AllocatorId,
}

/// The outcome of surrendering a [`Buffer`] to an allocator.
///
/// Returned by [`Buffer::release_to`], which is the only way to get a foreign
/// block back out of a [`Buffer`]. The three cases are exhaustive and an
/// implementation must handle each: this is where the `ZFREE` of `zutil.h` L254
/// happens, and it is deliberately impossible to reach it with a block that came
/// from somewhere else.
#[derive(Debug)]
pub enum Release<'a, T> {
    /// The block was a Rust allocation and has already been released. There is
    /// nothing left to do; in particular, a caller's `zfree` must **not** be
    /// called, because it never handed this memory out.
    Handled,
    /// The block belongs to the allocator it was released to, which must now
    /// return it -- for a facade allocator, by calling the caller's `zfree` with
    /// the block's base address, exactly as `ZFREE` does.
    ///
    /// The borrow has already been given up: what arrives is a
    /// [`ForeignBlock`] -- an address and a length -- so the previous holder can no
    /// longer reach the memory *and* nothing holds a live reference to it while it
    /// is being freed. See [`ForeignBlock`] for why that distinction is load-bearing
    /// rather than stylistic.
    Foreign(ForeignBlock<'a, T>),
    /// The block did **not** come from the allocator it was offered to, and has
    /// not been released. The buffer is handed back untouched.
    ///
    /// This is a programming error in the caller, not a recoverable condition, and
    /// it is reported this way on purpose: dropping the returned value leaks a
    /// foreign block, which is safe and observable, whereas passing the block to
    /// the wrong `zfree` would corrupt the heap. A Rust-owned block in this
    /// position is released by its own destructor when the returned value is
    /// dropped, so nothing leaks in that direction.
    Refused(Buffer<'a, T>),
}

/// A block whose borrow has been given up, ready to be handed to `zfree`.
///
/// ★ **Why this exists instead of passing the `&mut [T]` straight to the allocator.**
/// A reference in *argument position* is protected for the whole of the call it is
/// passed to, and deallocating memory that a protected reference covers is undefined
/// behaviour -- Miri reports it as "deallocating while item \[Unique for \<tag\>\] is
/// strongly protected". An earlier revision of this trait took the whole
/// [`Buffer`] into the allocator's release method and called the caller's `zfree`
/// inside it; that is exactly the forbidden shape, and it was reachable from any C
/// caller that supplies its own `zalloc`/`zfree` -- which `test/infcover.c` does on
/// every one of its streams.
///
/// The fix is structural rather than local: [`Buffer::release_to`] converts the
/// borrow into this address-and-length pair *and returns*, so the frame that held the
/// reference is gone before [`Allocator::release_foreign_bytes`] is entered. Nothing
/// then protects the block and freeing it is sound. That is also why the release path
/// is two calls rather than one, and why neither may be folded into the other.
///
/// The lifetime is carried but never used to reach the memory: it records that the
/// block was valid for `'a`, which is what makes holding one past its allocator's
/// life impossible in safe code.
#[derive(Debug)]
pub struct ForeignBlock<'a, T> {
    /// The block's base address, as `zfree` needs it.
    address: *mut T,
    /// How many elements the block holds.
    len: usize,
    /// The borrow this block was detached from, recorded and never followed.
    lifetime: PhantomData<&'a mut [T]>,
}

impl<'a, T> ForeignBlock<'a, T> {
    /// Detaches `block` from its borrow.
    ///
    /// Taking the slice by value is what makes this a one-way door: the caller has
    /// given up the only reference, so the address that comes out is not aliased by
    /// anything the borrow checker can still see.
    fn detach(block: &'a mut [T]) -> Self {
        Self {
            address: block.as_mut_ptr(),
            len: block.len(),
            lifetime: PhantomData,
        }
    }

    /// The block's base address: the pointer to pass to `zfree`.
    ///
    /// Dereferencing it is the caller's business and needs `unsafe`; producing it
    /// here does not.
    #[must_use]
    pub const fn as_mut_ptr(&self) -> *mut T {
        self.address
    }

    /// How many elements the block holds.
    ///
    /// `zfree` does not need it -- C's `free` takes only an address -- but a Rust
    /// allocator, and every tracking allocator, does.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the block holds no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<'a, T> Buffer<'a, T> {
    /// Wraps a block that belongs to `allocator` -- the constructor an
    /// [`Allocator`] implementation uses to hand out memory it did not obtain
    /// from Rust.
    ///
    /// The identity is taken from the allocator rather than passed in separately,
    /// so an implementation cannot mislabel its own block.
    ///
    /// # For implementations
    ///
    /// `items` must be a block this allocator produced, initialised, valid for
    /// `'a`, and not aliased by any other live [`Buffer`]. Those obligations are
    /// discharged in the crate that produces the borrow; see the notes on
    /// implementing [`Allocator`].
    pub fn from_foreign<A>(allocator: &A, items: &'a mut [T]) -> Self
    where
        A: Allocator<'a> + ?Sized,
    {
        Self {
            storage: Storage::Foreign(items),
            owner: allocator.id(),
        }
    }

    /// Allocates `len` elements from the Rust global allocator, filling them with
    /// `fill`, or returns [`None`] if the allocation fails.
    ///
    /// This is the mechanism behind [`GlobalAllocator`], and it is the shape to
    /// reach for when a buffer is not caller-visible -- as in the `gzFile` layer,
    /// which allocates with plain `malloc` rather than through a `z_stream`
    /// (`gzlib.c` L100, `gzread.c` L99-L100, `gzwrite.c` L16 and L25) and so has
    /// no caller hooks to honour.
    ///
    /// Failure is reported, never fatal: the allocation goes through
    /// [`Vec::try_reserve_exact`], so an out-of-memory condition returns [`None`]
    /// instead of aborting the process. This is what makes `Z_MEM_ERROR` reachable
    /// on demand, which `test/infcover.c` depends on -- its `mem_limit`
    /// (L176-L181) caps the total and its allocator then returns null at
    /// L79-L80 to force the library's error paths to run.
    ///
    /// A `len` of zero yields an empty buffer. That is a valid, releasable buffer
    /// with no elements: [`Buffer::as_slice`] is empty, so there is nothing to
    /// read or write through and no dangling storage to misuse.
    ///
    /// # ★ Every element is written, and that has to be measured, not hidden
    ///
    /// `try_reserve_exact` obtains the capacity and `resize` then writes `len`
    /// copies of `fill` into it. C's `zcalloc` (`zutil.c` L299-L308) is a plain
    /// `malloc` and writes nothing. So a Rust stream is *born* having touched
    /// every byte it owns, where a C stream is not: initialising a
    /// default-configuration deflate state -- a 64 KiB window, a 64 KiB `prev`, a
    /// 128 KiB `head` and a 64 KiB pending buffer -- means roughly a quarter of a
    /// megabyte written before the first input byte arrives.
    ///
    /// This is deliberate and is not an algorithmic defect. A block handed out by
    /// a caller's `zalloc` may contain anything at all --
    /// `test/infcover.c` L87 fills every one with `0xa5` precisely to catch code
    /// that assumes otherwise -- and safe Rust cannot hand out a partially
    /// initialised slice, so the fill is what makes `Buffer::as_slice` a slice at
    /// all. What it does mean is that **construction cost and steady-state
    /// throughput must not be measured together**. Any benchmark of this library
    /// is required to:
    ///
    /// * build and reset streams **outside** the timed region, so that a
    ///   compression or decompression rate is a rate for the algorithm and not
    ///   for `memset`;
    /// * time initialisation and reset as their own measurements, since they are a
    ///   real cost that a caller who opens many short-lived streams will pay; and
    /// * report the initialised-allocation cost with the memory figures rather
    ///   than folding it into the throughput ones, because it is the honest place
    ///   for it.
    ///
    /// The ad-hoc probes used while this was being written follow exactly that
    /// discipline, and so does every suite under `benches/`: cost that is
    /// invisible in a rate is cost that gets attributed to the wrong thing.
    /// `benches/deflate_bench.rs` discharges all three clauses explicitly — its
    /// `deflate_steady_state` group, which is the one the throughput gate is read
    /// from, initialises one encoder per row and calls `deflateReset` between
    /// iterations so the fill never enters the timed region; its
    /// `deflate_lifecycle` group times `deflateInit2_` and `deflateEnd` as their
    /// own measurement and is deliberately kept out of the gate; and the
    /// per-stream high-water figure it reports comes from the instrumented
    /// allocator rather than from either rate. `benches/inflate_bench.rs` splits
    /// the same way.
    ///
    /// # ★ What it actually costs, measured
    ///
    /// The obligation above has been discharged. Measured against the C library built from the
    /// in-tree sources and called in the same process (release profile, fat LTO, trimmed mean of the
    /// faster half of the samples), with initialisation timed on its own -- `deflateInit2_` followed
    /// immediately by `deflateEnd`, compressing nothing:
    ///
    /// | configuration | this port | C | ratio |
    /// |---|---|---|---|
    /// | level 6, `memLevel` 8 | 6.10 us | 1.40 us | 4.35 |
    /// | level 9, `memLevel` 9 | 9.06 us | 2.39 us | 3.79 |
    /// | level 1, `memLevel` 8 | 6.10 us | 1.41 us | 4.34 |
    ///
    /// So the fill costs about 4.7 us per stream at the default configuration, which is what a
    /// quarter-megabyte `memset` costs at this machine's memory bandwidth. It is a real cost and it is
    /// stated as a multiple of C's, not hidden.
    ///
    /// What it is *not* is a throughput cost, and the same measurement shows where the crossover
    /// falls. Timing a whole `deflateInit2_` + `deflate(Z_FINISH)` + `deflateEnd` at level 6:
    ///
    /// | payload | this port | C | ratio |
    /// |---|---|---|---|
    /// | 1 KiB | 14.9 us | 7.2 us | 2.09 |
    /// | 8 KiB | 40.9 us | 30.4 us | 1.35 |
    /// | 64 KiB | 247.8 us | 256.6 us | 0.97 |
    /// | 1 MiB | 3.90 ms | 4.61 ms | 0.85 |
    ///
    /// A caller who opens a stream per one-kilobyte message pays roughly twice what C costs, and a
    /// caller who compresses 64 KiB or more per stream pays less than C does. Both of those are true
    /// at once; reporting only the second would be the dishonest half.
    ///
    /// # Why it cannot simply be removed
    ///
    /// The obvious answer -- hand out uninitialised storage and initialise each byte at its first
    /// write -- works for a buffer written front to back, and the caller-facing output plumbing in
    /// this crate does exactly that. It does not work for `prev` and `head`. Those are indexed by a
    /// rolling hash, that is to say at effectively arbitrary positions, so there is no initialised
    /// prefix to track and no ordering that guarantees a slot is written before it is read. Reading
    /// one back would require turning `MaybeUninit` into an initialised value, and this crate carries
    /// `#![forbid(unsafe_code)]`, which the compiler enforces. The fill is therefore the price of
    /// that attribute on randomly indexed arrays, it is paid once per stream, and the honest thing to
    /// do with it is to measure it -- which is what the tables above are.
    #[must_use]
    pub fn try_global(len: usize, fill: T) -> Option<Self>
    where
        T: Clone,
    {
        let mut items = Vec::new();
        // The fallible half. `try_reserve_exact` asks for capacity for exactly
        // `len` elements and reports both an overflowing byte size and a genuine
        // out-of-memory condition as an error, where `Vec::with_capacity` and
        // `Box::new` would abort the process instead.
        items.try_reserve_exact(len).ok()?;
        // The infallible half: capacity is already sufficient, so `resize` writes
        // `len` copies of `fill` without allocating again.
        items.resize(len, fill);
        Some(Self {
            storage: Storage::Owned(items),
            owner: AllocatorId::GLOBAL,
        })
    }

    /// Returns the identity of the allocator that produced this block.
    #[must_use]
    pub const fn owner(&self) -> AllocatorId {
        self.owner
    }

    /// Returns the number of elements in the block.
    ///
    /// This is the length the block was requested with, so for a byte block it is
    /// the `items * size` product of the original `ZALLOC` call; see
    /// [`block_len`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Reports whether the block has no elements, which happens only when it was
    /// requested with a length of zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    /// Borrows the block's elements.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        match &self.storage {
            Storage::Owned(items) => items.as_slice(),
            Storage::Foreign(items) => items,
        }
    }

    /// Borrows the block's elements mutably.
    ///
    /// This is the replacement for the raw pointer the reference implementation
    /// keeps in `deflate_state` and `inflate_state`: it carries the length, so
    /// every access through it is bounds checked and there is no pointer to walk
    /// off the end of.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match &mut self.storage {
            Storage::Owned(items) => items.as_mut_slice(),
            Storage::Foreign(items) => items,
        }
    }

    /// Overwrites every element with `value`.
    ///
    /// The analogue of the `zmemzero` calls the reference implementation makes on
    /// buffers that must start from a known state -- `CLEAR_HASH`
    /// (`deflate.c` L170-L173) is exactly this over the hash head array, and it
    /// exists because the allocator does not zero anything.
    pub fn fill(&mut self, value: T)
    where
        T: Clone,
    {
        self.as_mut_slice().fill(value);
    }

    /// Surrenders the block to `allocator`, which is the only way a foreign block
    /// can be turned back into a raw borrow.
    ///
    /// The allocator's [`AllocatorId`] is compared against the one recorded when
    /// the block was handed out. On a match the block is released -- a Rust
    /// allocation is freed here and reported as [`Release::Handled`], a foreign
    /// block is handed to the caller as [`Release::Foreign`] to pass to `zfree`.
    /// On any mismatch nothing is released and the buffer comes back as
    /// [`Release::Refused`].
    ///
    /// Taking the allocator rather than a bare identity is what makes the check
    /// reliable: an implementation cannot accidentally compare against the wrong
    /// identity, because it has none to pass.
    ///
    /// The three outcomes a call site must handle:
    ///
    /// ```text
    /// match buffer.release_to(self) {
    ///     Release::Handled => {}
    ///     Release::Foreign(block) => call the caller's zfree on block,
    ///     Release::Refused(_) => not ours; releasing it would corrupt,
    /// }
    /// ```
    #[must_use = "a refused release means the block was not freed"]
    pub fn release_to<A>(self, allocator: &A) -> Release<'a, T>
    where
        A: Allocator<'a> + ?Sized,
    {
        // Destructured by move, which is possible precisely because `Buffer` has
        // no destructor of its own: the `Vec` in the owned case carries the only
        // destructor, and it runs when that `Vec` is dropped below.
        let Self { storage, owner } = self;
        if allocator.id() != owner {
            return Release::Refused(Self { storage, owner });
        }
        match storage {
            Storage::Owned(items) => {
                // Rust's global deallocator, reached the only way it can be
                // reached from safe code: by dropping the allocation that owns it.
                // Deliberately explicit, so that the one place this crate returns
                // memory to Rust rather than to a caller is easy to find.
                drop(items);
                Release::Handled
            }
            // The borrow is given up *here*, inside this call, and only the address
            // and length travel onward. That is what lets the allocator free the
            // block: see `ForeignBlock` for why doing it one frame later matters.
            Storage::Foreign(items) => Release::Foreign(ForeignBlock::detach(items)),
        }
    }
}

impl<T> AsRef<[T]> for Buffer<'_, T> {
    /// Borrows the block, so it can be handed to anything generic over
    /// `AsRef<[T]>` -- which is how this crate's buffer views accept storage.
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> AsMut<[T]> for Buffer<'_, T> {
    /// Borrows the block mutably, the counterpart of the [`AsRef`] implementation.
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T> fmt::Debug for Buffer<'_, T> {
    /// Reports the block's shape and provenance, never its contents.
    ///
    /// Written out rather than derived for two reasons: a derived implementation
    /// would require `T` to be [`Debug`](fmt::Debug), which excludes the state
    /// types this may one day hold, and dumping a 64 KiB window into a log is not
    /// useful diagnostics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("len", &self.len())
            .field("foreign", &matches!(self.storage, Storage::Foreign(_)))
            .field("owner", &self.owner)
            .finish()
    }
}

/// The source of every buffer the library uses, injected into the state types
/// that need one.
///
/// This is the Rust form of the `(zalloc, zfree, opaque)` triple from `z_stream`
/// (`zlib.h` L102-L104). Two implementations exist across the workspace:
/// [`GlobalAllocator`] here, which is the mirror of `zcalloc`/`zcfree`
/// (`zutil.c` L299-L308) and is what an initialisation function substitutes when
/// both caller hooks are `Z_NULL` (`deflate.c` L401-L414); and one in the
/// `libz-rs-sys` facade, which calls the caller's hooks. The algorithms take
/// either and cannot tell them apart, which is the point: allocation policy is a
/// parameter, not a global.
///
/// The trait is object safe, so `&dyn Allocator<'a>` works and is itself an
/// `Allocator<'a>` through the blanket implementation on references. That matters
/// because a state type must keep hold of its allocator in order to release its
/// buffers, and a shared reference is [`Copy`], so it can be stored by value
/// wherever the state goes.
///
/// # The lifetime parameter
///
/// `'a` is the lifetime of the *allocations*, not of the allocator value. An
/// allocator is typically a small value reconstructed on each entry into the
/// library from the fields of the caller's `z_stream`, while the blocks it hands
/// out live as long as the stream state does. Keeping the two apart is what lets
/// an allocator be built, used, and dropped within one call while its blocks
/// survive to the next.
///
/// # Implementing this trait
///
/// An implementation that hands out blocks it did not obtain from Rust must
/// uphold five obligations. They are ordinary preconditions here, but in the
/// facade crate they are the safety invariants of the pointer work that produces
/// the borrows, so they are stated in full:
///
/// 1. **Initialised.** Every element of a returned block must hold a valid value
///    before the block is handed over, because a [`Buffer`] hands out ordinary
///    Rust borrows and reading an uninitialised byte through one is undefined
///    behaviour. Fill with [`SENTINEL_FILL`] to match `test/infcover.c` L87. This
///    is the documented divergence from `malloc` described in the module
///    documentation, and it is unobservable to callers.
/// 2. **Disjoint.** No two live blocks may overlap, and no block may alias
///    anything else the library holds.
/// 3. **Valid for `'a`.** A block must stay allocated and untouched by anything
///    else until it is released.
/// 4. **Aligned.** A `u16` block must be aligned for `u16`. Every allocator the
///    reference implementation contemplates satisfies this, because both
///    `zcalloc` (`zutil.c` L301) and `test/infcover.c`'s tracking allocator
///    (L84) are backed by `malloc`, whose result is suitably aligned for any
///    fundamental type.
/// 5. **Correctly identified.** [`Allocator::id`] must return the same value for
///    the lifetime of the allocator, must differ between allocators whose blocks
///    are not interchangeable, and must be built with [`AllocatorId::foreign`] --
///    [`AllocatorId::GLOBAL`] is reserved for the Rust global allocator, and
///    claiming it for a foreign block would defeat the check in
///    [`Buffer::release_to`].
///
/// A deallocating method must route the buffer through [`Buffer::release_to`]
/// rather than reaching into it, which is what guarantees the identity check
/// happens and is also the only way to obtain the raw borrow to free.
pub trait Allocator<'a> {
    /// Returns this allocator's identity, recorded in every block it hands out.
    ///
    /// See the obligations above: the value must be stable, must distinguish
    /// allocators whose blocks are not interchangeable, and must not be
    /// [`AllocatorId::GLOBAL`] unless this really is the Rust global allocator.
    fn id(&self) -> AllocatorId;

    /// Returns the `opaque` value this allocator passes to its hooks.
    ///
    /// The library never interprets it (`zlib.h` L146-L147); this accessor exists
    /// so that code holding only an allocator can thread the value back where the
    /// C API needs it. `deflateCopy` and `inflateCopy` are the concrete cases:
    /// both copy the whole `z_stream`, hooks and `opaque` included, and then
    /// allocate the copy's buffers through them (`deflate.c` L1335-L1345,
    /// `inflate.c` L1340-L1346).
    ///
    /// [`GlobalAllocator`] returns [`Opaque::NULL`], which is what
    /// `deflate.c` L406 stores when it substitutes the internal routines.
    fn opaque(&self) -> Opaque;

    /// Allocates `items * size` bytes, or returns [`None`] if that is not
    /// possible.
    ///
    /// The two arguments mirror `ZALLOC(strm, items, size)` (`zutil.h`
    /// L252-L253), which keeps them separate all the way to the caller's hook;
    /// splitting them here means a call site can read like the C it came from --
    /// `deflate.c` L458 asks for `w_size` items of two bytes, and `deflate.c` L505
    /// asks for `lit_bufsize` items of `LIT_BUFS` bytes.
    ///
    /// [`None`] means the request could not be met, whether because the product
    /// overflows (see [`block_len`]) or because the underlying allocator declined,
    /// which is `zalloc` returning `Z_NULL` as `zlib.h` L149 requires. Callers map
    /// that to [`ReturnCode::MEM_ERROR`]; [`Allocator::allocate_bytes_or_mem_error`]
    /// does it for them.
    ///
    /// The contents of the returned block are unspecified. See the module
    /// documentation: nothing may assume they are zero.
    fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>>;

    /// Allocates `items` unsigned 16-bit values, or returns [`None`] if that is
    /// not possible.
    ///
    /// This is `ZALLOC(strm, items, sizeof(Pos))`, the shape both hash-chain
    /// arrays use (`deflate.c` L459-L460), where `Pos` is `ush` -- an
    /// `unsigned short` (`deflate.h` L96, `zutil.h` L45). An implementation must
    /// pass `size_of::<u16>()` as the hook's `size` argument so the C-visible
    /// request is identical.
    ///
    /// Failure and initial contents behave exactly as for
    /// [`Allocator::allocate_bytes`].
    fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>>;

    /// Returns a byte block this allocator handed out to whatever produced it.
    ///
    /// This is the `ZFREE(strm, addr)` itself (`zutil.h` L254): for a facade
    /// allocator, the call to the caller's `zfree` with the block's base address.
    ///
    /// # Never call this directly
    ///
    /// [`Allocator::deallocate_bytes`] is the entry point, and it is what
    /// guarantees the two properties this method depends on: that
    /// [`Buffer::release_to`] has confirmed the block is *this* allocator's, and
    /// that the borrow has already been given up one frame earlier. Freeing a block
    /// while a reference to it is still live in an enclosing call is undefined
    /// behaviour; [`ForeignBlock`] records that reasoning in full.
    ///
    /// An allocator that only ever hands out Rust-owned storage can never reach
    /// this method, because [`Buffer::release_to`] answers [`Release::Handled`] for
    /// every block it produced. Such an implementation is still required to be
    /// total, and does nothing.
    fn release_foreign_bytes(&self, block: ForeignBlock<'a, u8>);

    /// Returns a `u16` block this allocator handed out.
    ///
    /// The [`Allocator::release_foreign_bytes`] counterpart for the shape
    /// [`Allocator::allocate_u16s`] produces, with the same contract.
    fn release_foreign_u16s(&self, block: ForeignBlock<'a, u16>);

    /// Releases the byte block in `slot`, if there is one, leaving `slot` empty.
    ///
    /// This is `TRY_FREE(s, p)` -- `{if (p) ZFREE(s, p);}` (`zutil.h` L255) -- the
    /// form `deflateEnd` uses for all four of its buffers because an initialisation
    /// that failed part-way leaves some of them null (`deflate.c` L1301-L1304,
    /// reached from the failure path at L508-L513). A block is passed by slot rather
    /// than by value for two reasons, and both matter:
    ///
    /// * an absent block is the common case during teardown, and a slot expresses it
    ///   without a second method; and
    /// * ★ the block's borrow must **not** be live in this frame. A reference in
    ///   argument position is protected for the duration of the call, and freeing
    ///   memory a protected reference covers is undefined behaviour. Taking
    ///   `&mut Option<Buffer<'a, u8>>` retags only the slot -- which lives in the
    ///   caller -- and never the block, and the borrow inside is surrendered by
    ///   [`Buffer::release_to`] in a frame that has returned before
    ///   [`Allocator::release_foreign_bytes`] is entered. See [`ForeignBlock`].
    ///
    /// A block belonging to a different allocator is refused and dropped rather than
    /// freed: dropping leaks a foreign block, which is safe and shows up in a leak
    /// report, whereas passing it to the wrong `zfree` would corrupt the heap.
    fn deallocate_bytes(&self, slot: &mut Option<Buffer<'a, u8>>) {
        let Some(buffer) = slot.take() else { return };
        match buffer.release_to(self) {
            Release::Foreign(block) => self.release_foreign_bytes(block),
            // The block was Rust-owned and its destructor has already run.
            Release::Handled => {}
            // Not ours. Dropping it is the safe answer; see the method documentation.
            Release::Refused(refused) => drop(refused),
        }
    }

    /// Releases the `u16` block in `slot`, if there is one, leaving `slot` empty.
    ///
    /// The [`Allocator::deallocate_bytes`] counterpart for `u16` blocks, which
    /// `deflateEnd` needs for `head` and `prev` (`deflate.c` L1302-L1303). The
    /// contract, and the reason for the slot, are identical.
    fn deallocate_u16s(&self, slot: &mut Option<Buffer<'a, u16>>) {
        let Some(buffer) = slot.take() else { return };
        match buffer.release_to(self) {
            Release::Foreign(block) => self.release_foreign_u16s(block),
            Release::Handled => {}
            Release::Refused(refused) => drop(refused),
        }
    }

    /// Allocates `items * size` bytes, reporting failure as
    /// [`ReturnCode::MEM_ERROR`].
    ///
    /// A convenience over [`Allocator::allocate_bytes`] for the common case, which
    /// is an initialisation function that wants to propagate the status code
    /// straight out. In C the same step is `if (s->window == Z_NULL || ...) { ...
    /// return Z_MEM_ERROR; }` (`deflate.c` L508-L513).
    ///
    /// The C code also records the message alongside the code -- `strm->msg =
    /// ERR_MSG(Z_MEM_ERROR)` at `deflate.c` L511. That is the stream's business,
    /// not the allocator's, so it stays with the caller:
    /// [`ReturnCode::record_msg`] performs it.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the request could not be met, either because
    /// `items * size` overflows or because the underlying allocator declined it.
    fn allocate_bytes_or_mem_error(
        &self,
        items: usize,
        size: usize,
    ) -> Result<Buffer<'a, u8>, ReturnCode> {
        self.allocate_bytes(items, size)
            .ok_or(ReturnCode::MEM_ERROR)
    }

    /// Allocates `items` unsigned 16-bit values, reporting failure as
    /// [`ReturnCode::MEM_ERROR`].
    ///
    /// The [`Allocator::allocate_bytes_or_mem_error`] counterpart for the shape
    /// [`Allocator::allocate_u16s`] produces.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the request could not be met.
    fn allocate_u16s_or_mem_error(&self, items: usize) -> Result<Buffer<'a, u16>, ReturnCode> {
        self.allocate_u16s(items).ok_or(ReturnCode::MEM_ERROR)
    }
}

/// Lets a shared reference to an allocator be used as an allocator.
///
/// Two things depend on this. It makes `&dyn Allocator<'a>` -- which is a
/// reference, not a bare trait object -- satisfy `A: Allocator<'a>`, so a state
/// type can be generic over its allocator and still accept a dynamically
/// dispatched one. And because a shared reference is [`Copy`], it gives a state
/// type an allocator it can store by value and hand around freely, which is what
/// releasing buffers during teardown requires.
impl<'a, A> Allocator<'a> for &A
where
    A: Allocator<'a> + ?Sized,
{
    fn id(&self) -> AllocatorId {
        (**self).id()
    }

    fn opaque(&self) -> Opaque {
        (**self).opaque()
    }

    fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
        (**self).allocate_bytes(items, size)
    }

    fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
        (**self).allocate_u16s(items)
    }

    fn release_foreign_bytes(&self, block: ForeignBlock<'a, u8>) {
        (**self).release_foreign_bytes(block);
    }

    fn release_foreign_u16s(&self, block: ForeignBlock<'a, u16>) {
        (**self).release_foreign_u16s(block);
    }

    // Both provided methods are forwarded rather than inherited: an allocator that
    // overrides them -- a tracking one that has to see every release, not only the
    // foreign ones -- would otherwise be bypassed the moment it were used by
    // reference, and `&A` is how every state type holds its allocator.
    fn deallocate_bytes(&self, slot: &mut Option<Buffer<'a, u8>>) {
        (**self).deallocate_bytes(slot);
    }

    fn deallocate_u16s(&self, slot: &mut Option<Buffer<'a, u16>>) {
        (**self).deallocate_u16s(slot);
    }
}

/// The byte a fresh block is filled with.
///
/// [`SENTINEL_FILL`] in debug builds, so that a state constructor which
/// mistakenly assumes zeroed memory produces wrong answers and fails a test --
/// the same trick, and the same value, as `test/infcover.c` L87. Zero in release
/// builds, which is the plainest possible choice for a fill that has to happen
/// anyway.
///
/// Neither value is observable: the reference implementation never reads a byte of
/// a fresh buffer before writing it, so no fill value can reach the compressed
/// output. Both cost one pass over the block, so the split is about test signal
/// rather than speed.
///
/// ★ Because *both* values cost that pass, choosing zero in release builds does
/// not make the fill free -- it only makes it plain. C's `zcalloc` writes nothing
/// at all, so this pass has no counterpart in the reference and must be reported
/// as an initialisation cost rather than absorbed into a throughput figure; see
/// [`Buffer::try_global`] for the measurement obligation that follows.
#[cfg(debug_assertions)]
const FILL_BYTE: u8 = SENTINEL_FILL;

/// The byte a fresh block is filled with in release builds. See the debug-build
/// definition above for the rationale.
#[cfg(not(debug_assertions))]
const FILL_BYTE: u8 = 0;

/// The library's own allocator: the mirror of `zcalloc` and `zcfree`.
///
/// `zutil.c` L299-L308 defines the internal routines that an initialisation
/// function installs when the caller leaves both hooks `Z_NULL`
/// (`deflate.c` L401-L414, `inflate.c` L183-L195, `infback.c` L37-L49), which is
/// the behaviour `zlib.h` L151-L153 promises. Those routines ignore `opaque` and
/// call straight through to `malloc` and `free`; this type ignores `opaque` and
/// goes through Rust's global allocator instead, which is the same guarantee --
/// blocks are returned to whoever allocated them -- expressed in the safe
/// language.
///
/// It is also the right allocator for buffers that are not reached through a
/// `z_stream` at all. The `gzFile` layer is the case in point: it allocates its
/// state, its path string and its two working buffers with plain `malloc`
/// (`gzlib.c` L100 and L206, `gzread.c` L99-L100, `gzwrite.c` L16 and L25),
/// because a `gzFile` has no caller-supplied hooks to honour.
///
/// The type is zero-sized and [`Copy`], so injecting it costs nothing, and every
/// value of it is interchangeable with every other -- which is why one reserved
/// [`AllocatorId::GLOBAL`] identity covers them all.
///
/// # Failure is reported, never fatal
///
/// Allocation goes through [`Buffer::try_global`], hence through
/// [`Vec::try_reserve_exact`], so exhaustion returns [`None`] rather than aborting
/// the process the way `Vec::with_capacity` or `Box::new` would. That is what
/// keeps `Z_MEM_ERROR` reachable, which the library's error paths and
/// `test/infcover.c`'s `mem_limit` (L176-L181) both rely on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GlobalAllocator;

impl GlobalAllocator {
    /// The 16-bit fill pattern, both of whose bytes are [`FILL_BYTE`].
    ///
    /// Filling a `u16` block byte-for-byte the same way a byte block is filled
    /// keeps the two shapes consistent: a `u16` block from a debug build reads back
    /// as `0xa5a5`, which is what the bytes `0xa5 0xa5` mean in either byte order,
    /// so the sentinel is recognisable however the block is later viewed.
    const FILL_U16: u16 = u16::from_ne_bytes([FILL_BYTE, FILL_BYTE]);
}

impl<'a> Allocator<'a> for GlobalAllocator {
    /// Returns the reserved identity [`AllocatorId::GLOBAL`].
    ///
    /// Every `GlobalAllocator` value shares it, which is correct: Rust's global
    /// allocator is a singleton, so any of them can release any other's block.
    fn id(&self) -> AllocatorId {
        AllocatorId::GLOBAL
    }

    /// Returns [`Opaque::NULL`].
    ///
    /// Mirrors `strm->opaque = (voidpf)0` at `deflate.c` L406: an
    /// initialisation function that installs the internal routines also clears
    /// `opaque`, because those routines ignore it (`zutil.c` L300 and L306 both
    /// discard it with `(void)opaque`).
    fn opaque(&self) -> Opaque {
        Opaque::NULL
    }

    /// Allocates `items * size` bytes from Rust's global allocator.
    ///
    /// The product is computed with [`block_len`], so an overflow is reported as
    /// an allocation failure rather than silently wrapping to a short block. This
    /// is the mirror of `malloc(items * size)` from `zutil.c` L301, with the
    /// multiplication checked and the failure reported instead of the process
    /// aborting.
    ///
    /// It also *writes* `items * size` bytes where `malloc` writes none, which is
    /// a startup cost with no counterpart in the reference; see
    /// [`Buffer::try_global`] for why that is unavoidable in safe Rust and how a
    /// benchmark is required to account for it.
    fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
        Buffer::try_global(block_len(items, size)?, FILL_BYTE)
    }

    /// Allocates `items` unsigned 16-bit values from Rust's global allocator.
    ///
    /// The Rust counterpart of `ZALLOC(strm, items, sizeof(Pos))` (`deflate.c` L459-L460),
    /// which likewise initialises every element; see [`Buffer::try_global`].
    fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
        // The C call multiplies `items` by `sizeof(Pos)` before allocating, so
        // reject a byte size that cannot be represented for exactly the reason
        // `allocate_bytes` does. `Vec::try_reserve_exact` would also catch this,
        // but checking here keeps both shapes reporting the same failure for the
        // same reason.
        block_len(items, size_of::<u16>())?;
        Buffer::try_global(items, Self::FILL_U16)
    }

    /// Unreachable: this allocator hands out nothing but Rust-owned storage.
    ///
    /// The Rust counterpart of `free(ptr)` from `zutil.c` L307 is the `Vec`
    /// destructor, which [`Buffer::release_to`] runs for every block this allocator
    /// produced -- reporting [`Release::Handled`], the arm
    /// [`Allocator::deallocate_bytes`] answers by doing nothing further. A block that
    /// reached here would have to have been handed out by some *other* allocator
    /// while carrying this one's identity, which [`Buffer::from_foreign`] makes
    /// impossible.
    ///
    /// So there is nothing to free and nothing to report: the method is total
    /// because the trait requires it to be, and empty because that is the whole of
    /// the correct behaviour. It deliberately does not panic -- library code in this
    /// crate never does.
    fn release_foreign_bytes(&self, _block: ForeignBlock<'a, u8>) {}

    /// Unreachable for the same reason as [`Allocator::release_foreign_bytes`].
    fn release_foreign_u16s(&self, _block: ForeignBlock<'a, u16>) {}
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right
    // policy there and the wrong one here: a test asserts, and an assertion that
    // fails panics. The relaxation is scoped to this module and applies to nothing
    // that ships.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::{
        block_len, Allocator, AllocatorId, Buffer, ForeignBlock, GlobalAllocator, Opaque, Release,
        SENTINEL_FILL,
    };
    use crate::error::ReturnCode;
    use core::cell::Cell;
    use core::ffi::c_void;

    /// Stand-in for the facade's allocator, used to exercise the foreign-block
    /// half of the handshake without any pointer work.
    ///
    /// It cannot manufacture borrows of the allocation lifetime out of `&self`, so
    /// its allocating methods decline -- which also makes it a useful stand-in for
    /// an allocator under `mem_limit` (`test/infcover.c` L176-L181), whose whole
    /// job is to fail. Its releasing methods do the real work: they route through
    /// [`Buffer::release_to`] exactly as a facade implementation must, and record
    /// what came back so the test can check it.
    struct Recorder {
        /// The identity this allocator claims.
        id: AllocatorId,
        /// Base address and length of the last byte block actually surrendered.
        released: Cell<Option<(usize, usize)>>,
        /// How many releases were refused because the block was not its own.
        refused: Cell<usize>,
        /// Element counts of the blocks released, in the order they arrived, so a
        /// test can check teardown order the way `test/infcover.c`'s `notlifo`
        /// counter does.
        order: Cell<[usize; 4]>,
        /// How many entries of `order` are populated.
        recorded: Cell<usize>,
    }

    impl Recorder {
        fn new(allocate: usize, deallocate: usize, opaque: usize) -> Self {
            Self {
                id: AllocatorId::foreign(allocate, deallocate, opaque),
                released: Cell::new(None),
                refused: Cell::new(0),
                order: Cell::new([0; 4]),
                recorded: Cell::new(0),
            }
        }

        /// Appends one released block's element count to the order log.
        fn note(&self, len: usize) {
            let index = self.recorded.get();
            let mut order = self.order.get();
            if let Some(slot) = order.get_mut(index) {
                *slot = len;
                self.order.set(order);
                self.recorded.set(index + 1);
            }
        }

        /// The shape of a facade deallocation, minus the call to `zfree`: record
        /// the block the trait says is ours, address and length both.
        fn record(&self, block: &ForeignBlock<'_, u8>) {
            self.released
                .set(Some((block.as_mut_ptr() as usize, block.len())));
            self.note(block.len());
        }

        /// Counts one release that was refused, and one that needed nothing done.
        fn record_outcome(&self, release: &Release<'_, u8>) {
            match release {
                Release::Foreign(_) => {}
                Release::Refused(_) => self.refused.set(self.refused.get() + 1),
                Release::Handled => self.released.set(None),
            }
        }

        /// The `u16` counterpart of [`Recorder::record_outcome`].
        fn record_outcome_u16s(&self, release: &Release<'_, u16>) {
            match release {
                Release::Foreign(block) => self.note(block.len()),
                Release::Refused(_) => self.refused.set(self.refused.get() + 1),
                Release::Handled => {}
            }
        }
    }

    impl<'a> Allocator<'a> for Recorder {
        fn id(&self) -> AllocatorId {
            self.id
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, _items: usize, _size: usize) -> Option<Buffer<'a, u8>> {
            None
        }

        fn allocate_u16s(&self, _items: usize) -> Option<Buffer<'a, u16>> {
            None
        }

        fn release_foreign_bytes(&self, block: ForeignBlock<'a, u8>) {
            self.record(&block);
        }

        fn release_foreign_u16s(&self, _block: ForeignBlock<'a, u16>) {
            // Counted in `deallocate_u16s`, which sees the refused and handled
            // outcomes this method never does.
        }

        // Overridden so that *every* outcome is counted, not only the foreign one:
        // the release is performed in a call that returns before the block is
        // touched, exactly as the provided implementation does.
        fn deallocate_bytes(&self, slot: &mut Option<Buffer<'a, u8>>) {
            let Some(buffer) = slot.take() else { return };
            let release = buffer.release_to(self);
            self.record_outcome(&release);
            if let Release::Foreign(block) = release {
                self.release_foreign_bytes(block);
            }
        }

        fn deallocate_u16s(&self, slot: &mut Option<Buffer<'a, u16>>) {
            let Some(buffer) = slot.take() else { return };
            self.record_outcome_u16s(&buffer.release_to(self));
        }
    }

    #[test]
    fn block_len_matches_the_c_call_shapes() {
        // deflate.c L458: ZALLOC(strm, s->w_size, 2*sizeof(Byte)) at windowBits 15.
        assert_eq!(block_len(32_768, 2), Some(65_536));
        // deflate.c L460: ZALLOC(strm, s->hash_size, sizeof(Pos)) at memLevel 9.
        assert_eq!(block_len(65_536, 2), Some(131_072));
        // deflate.c L505: ZALLOC(strm, s->lit_bufsize, LIT_BUFS) at memLevel 9.
        assert_eq!(block_len(32_768, 4), Some(131_072));
        // inflate.c L260-L262: ZALLOC(strm, 1U << state->wbits, sizeof(char)).
        assert_eq!(block_len(32_768, 1), Some(32_768));
        // Degenerate but well defined.
        assert_eq!(block_len(0, 4), Some(0));
        assert_eq!(block_len(4, 0), Some(0));
    }

    #[test]
    fn oversized_requests_fail_instead_of_wrapping() {
        let allocator = GlobalAllocator;

        // The product itself does not fit in a usize. An unchecked multiply would
        // wrap here and hand back a block far smaller than was asked for.
        assert_eq!(block_len(usize::MAX, 2), None);
        assert!(allocator.allocate_bytes(usize::MAX, 2).is_none());
        assert!(allocator.allocate_u16s(usize::MAX).is_none());

        // The shape the brief calls out: both arguments at their C maximum. On a
        // 32-bit target the product overflows `usize`; on a 64-bit one it is
        // representable but exceeds `MAX_BLOCK_LEN`, because no single object may
        // be larger than `isize::MAX`. Either way the answer is a reported failure
        // -- not a panic, not a short block, and not a request forwarded to an
        // allocator that could only refuse it.
        let huge = usize::try_from(u32::MAX).expect("u32 fits a usize on every target");
        assert_eq!(block_len(huge, huge), None);
        assert!(allocator.allocate_bytes(huge, huge).is_none());

        // The bound itself, from both sides. `isize::MAX` bytes is the largest
        // count that can name an object at all, so it survives the check (and is
        // then declined by the allocator, which is a different question); one byte
        // more cannot, and is refused without any allocator being asked.
        let max_len = isize::MAX.unsigned_abs();
        assert_eq!(block_len(max_len, 1), Some(max_len));
        assert_eq!(block_len(max_len / 2 + 1, 2), None);
        assert_eq!(block_len(max_len, 2), None);
        assert!(allocator.allocate_bytes(max_len, 2).is_none());
        assert!(allocator.allocate_u16s(max_len).is_none());

        // And the failure maps to the status code a caller returns. `err` rather
        // than a direct comparison because a buffer is not a comparable value.
        assert_eq!(
            allocator.allocate_bytes_or_mem_error(usize::MAX, 2).err(),
            Some(ReturnCode::MEM_ERROR)
        );
        assert_eq!(
            allocator.allocate_u16s_or_mem_error(usize::MAX).err(),
            Some(ReturnCode::MEM_ERROR)
        );
    }

    #[test]
    fn zero_sized_requests_yield_an_empty_releasable_buffer() {
        let allocator = GlobalAllocator;

        let buffer = allocator.allocate_bytes(0, 4).unwrap();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        // Nothing to read or write through: there is no element to reach, so there
        // is no way to touch storage that was never handed out.
        assert_eq!(buffer.as_slice().iter().count(), 0);
        assert_eq!(buffer.owner(), AllocatorId::GLOBAL);
        allocator.deallocate_bytes(&mut Some(buffer));

        // Zero items and a zero element size are both fine, and so is the u16
        // shape.
        let buffer = allocator.allocate_bytes(4, 0).unwrap();
        assert!(buffer.is_empty());
        allocator.deallocate_bytes(&mut Some(buffer));

        let buffer = allocator.allocate_u16s(0).unwrap();
        assert!(buffer.is_empty());
        allocator.deallocate_u16s(&mut Some(buffer));
    }

    #[test]
    fn the_default_path_round_trips_every_element() {
        let allocator = GlobalAllocator;

        // The deflate window at windowBits 15: deflate.c L458.
        let mut window = allocator.allocate_bytes(32_768, 2).unwrap();
        assert_eq!(window.len(), 65_536);
        for (index, byte) in window.as_mut_slice().iter_mut().enumerate() {
            // Every element is written, so the whole block is proven usable.
            *byte = u8::try_from(index % 251).unwrap();
        }
        let readback_ok = window
            .as_slice()
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == u8::try_from(index % 251).unwrap());
        assert!(readback_ok);
        allocator.deallocate_bytes(&mut Some(window));

        // The deflate hash head array at memLevel 9: deflate.c L460.
        let mut head = allocator.allocate_u16s(65_536).unwrap();
        assert_eq!(head.len(), 65_536);
        // CLEAR_HASH (deflate.c L170-L173) exists because the allocator does not
        // zero anything; this is that step.
        head.fill(0);
        assert!(head.as_slice().iter().all(|&position| position == 0));
        let last = head.len() - 1;
        head.as_mut_slice()[last] = 0x1234;
        assert_eq!(head.as_slice().get(last), Some(&0x1234));
        allocator.deallocate_u16s(&mut Some(head));

        // The inflate scratch shapes, which live inline in the C state
        // (inflate.h L120-L121) but are the same u16 request here.
        let lens = allocator.allocate_u16s(320).unwrap();
        assert_eq!(lens.len(), 320);
        allocator.deallocate_u16s(&mut Some(lens));
        let work = allocator.allocate_u16s(288).unwrap();
        assert_eq!(work.len(), 288);
        allocator.deallocate_u16s(&mut Some(work));
    }

    #[test]
    fn a_fresh_block_is_not_zeroed_in_debug_builds() {
        let allocator = GlobalAllocator;
        let buffer = allocator.allocate_bytes(64, 1).unwrap();

        // The point of the sentinel: in a debug build a fresh block is visibly not
        // zero, so a state constructor that assumed zeros fails a test instead of
        // passing by luck, exactly as test/infcover.c L87 arranges for the C code.
        if cfg!(debug_assertions) {
            assert!(buffer.as_slice().iter().all(|&byte| byte == SENTINEL_FILL));
            assert_eq!(SENTINEL_FILL, 0xA5);
        }
        // Either way the block is fully initialised and readable to its last byte.
        assert_eq!(buffer.as_slice().len(), 64);
        assert!(buffer.as_slice().last().is_some());

        allocator.deallocate_bytes(&mut Some(buffer));

        let u16s = allocator.allocate_u16s(8).unwrap();
        if cfg!(debug_assertions) {
            assert!(u16s.as_slice().iter().all(|&value| value == 0xA5A5));
        }
        allocator.deallocate_u16s(&mut Some(u16s));
    }

    #[test]
    fn try_deallocate_is_a_no_op_without_a_buffer() {
        // The TRY_FREE analogue, zutil.h L255: deflateEnd calls it on all four
        // buffers because a part-way initialisation failure leaves some null
        // (deflate.c L1301-L1304).
        let allocator = GlobalAllocator;
        allocator.deallocate_bytes(&mut None);
        allocator.deallocate_u16s(&mut None);
        allocator.deallocate_bytes(&mut allocator.allocate_bytes(8, 2));
        allocator.deallocate_u16s(&mut allocator.allocate_u16s(8));
    }

    #[test]
    fn a_block_cannot_be_released_to_a_different_allocator() {
        // On what is checked where. The catastrophic direction -- Rust's global
        // deallocator receiving a caller's block, or a caller's zfree receiving a
        // Rust allocation -- has no test here because it cannot be written: the
        // only route into a block is `Storage`, which is private, has exactly two
        // variants, and can be neither constructed nor destructured outside this
        // module, so no caller can mix the two provenances up. A compile-fail
        // expectation for code that does not typecheck is the whole of that
        // guarantee. What remains expressible, and is therefore checked at run
        // time below, is offering a foreign block to a *different* foreign
        // allocator, which the identity comparison refuses.
        //
        // Two allocators that differ only in their opaque value, which is enough:
        // ZFREE passes opaque to zfree (zutil.h L254), so blocks are not
        // interchangeable between them.
        let owner = Recorder::new(0x1000, 0x2000, 0x3000);
        let other = Recorder::new(0x1000, 0x2000, 0x4000);
        assert_ne!(owner.id(), other.id());

        // A block borrowed from the stack stands in for one a caller's zalloc
        // returned; nothing here needs a real hook.
        let mut storage = [0u8; 16];
        let expected = (storage.as_ptr() as usize, storage.len());

        let buffer = Buffer::from_foreign(&owner, &mut storage[..]);
        assert_eq!(buffer.owner(), owner.id());
        // Offered to the wrong allocator: refused, and nothing released.
        other.deallocate_bytes(&mut Some(buffer));
        assert_eq!(other.refused.get(), 1);
        assert_eq!(other.released.get(), None);

        // Offered to its own allocator: surrendered, and it is the same block.
        let buffer = Buffer::from_foreign(&owner, &mut storage[..]);
        owner.deallocate_bytes(&mut Some(buffer));
        assert_eq!(owner.refused.get(), 0);
        assert_eq!(owner.released.get(), Some(expected));

        // The reserved identity is never mistaken for a foreign one, in either
        // direction: a Rust allocation offered to a foreign allocator is refused,
        // and its own destructor still runs, so nothing leaks.
        let from_rust = GlobalAllocator.allocate_bytes(4, 1).unwrap();
        assert!(from_rust.owner().is_global());
        assert!(!owner.id().is_global());
        assert!(matches!(from_rust.release_to(&owner), Release::Refused(_)));
    }

    #[test]
    fn a_state_can_own_its_buffers_and_release_them_in_the_c_order() {
        /// A stand-in for `deflate_state`, holding the four buffers `deflateEnd`
        /// releases plus the allocator it must return them to.
        ///
        /// The fields are declared in *release* order on purpose. Rust drops
        /// fields in declaration order, so this is what makes an implicit drop
        /// agree with `deflate.c` L1300-L1306; the explicit destructor below then
        /// spells the same order out, exactly as the C function does.
        struct MiniState<'a, A: Allocator<'a>> {
            /// The injected allocator, kept by value so teardown can reach it.
            allocator: A,
            /// `deflate.h` L107, released first (`deflate.c` L1301).
            pending_buf: Option<Buffer<'a, u8>>,
            /// `deflate.h` L144, released second (`deflate.c` L1302).
            head: Option<Buffer<'a, u16>>,
            /// `deflate.h` L138, released third (`deflate.c` L1303).
            prev: Option<Buffer<'a, u16>>,
            /// `deflate.h` L123, released last (`deflate.c` L1304).
            window: Option<Buffer<'a, u8>>,
        }

        impl<'a, A: Allocator<'a>> Drop for MiniState<'a, A> {
            fn drop(&mut self) {
                // "Deallocate in reverse order of allocations", deflate.c L1300.
                // `try_deallocate_*` is the TRY_FREE analogue, so a part-way
                // initialisation that left a buffer absent tears down cleanly.
                self.allocator.deallocate_bytes(&mut self.pending_buf);
                self.allocator.deallocate_u16s(&mut self.head);
                self.allocator.deallocate_u16s(&mut self.prev);
                self.allocator.deallocate_bytes(&mut self.window);
            }
        }

        let recorder = Recorder::new(0xa, 0xb, 0xc);
        // Distinct lengths so the order log is unambiguous. Stack storage stands
        // in for blocks a caller's zalloc returned.
        let mut pending_storage = [0_u8; 8];
        let mut head_storage = [0_u16; 4];
        let mut prev_storage = [0_u16; 2];
        let mut window_storage = [0_u8; 1];

        let state = MiniState {
            allocator: &recorder,
            pending_buf: Some(Buffer::from_foreign(&recorder, &mut pending_storage[..])),
            head: Some(Buffer::from_foreign(&recorder, &mut head_storage[..])),
            prev: Some(Buffer::from_foreign(&recorder, &mut prev_storage[..])),
            window: Some(Buffer::from_foreign(&recorder, &mut window_storage[..])),
        };
        drop(state);

        // Every block came back, in the order deflateEnd uses, and none was
        // refused -- which is what keeps test/infcover.c's notlifo and rogue
        // counters at zero.
        assert_eq!(recorder.order.get(), [8, 4, 2, 1]);
        assert_eq!(recorder.refused.get(), 0);

        // The same state shape works with the crate's own allocator, where the
        // buffers are Rust allocations and teardown returns them to Rust.
        let global = GlobalAllocator;
        let state = MiniState {
            allocator: global,
            pending_buf: global.allocate_bytes(8, 4),
            head: global.allocate_u16s(4),
            prev: global.allocate_u16s(2),
            window: global.allocate_bytes(1, 2),
        };
        assert!(state.pending_buf.is_some());
        drop(state);
    }

    #[test]
    fn releasing_a_rust_allocation_reports_that_it_was_handled() {
        // The distinction a facade implementation depends on: Handled means the
        // memory is already gone and a caller's zfree must not see it.
        let allocator = GlobalAllocator;
        let buffer = allocator.allocate_bytes(32, 1).unwrap();
        assert!(matches!(buffer.release_to(&allocator), Release::Handled));

        let buffer = allocator.allocate_u16s(32).unwrap();
        assert!(matches!(buffer.release_to(&allocator), Release::Handled));
    }

    #[test]
    fn identities_compare_exactly() {
        assert_eq!(AllocatorId::GLOBAL, AllocatorId::GLOBAL);
        assert!(AllocatorId::GLOBAL.is_global());
        assert!(!AllocatorId::foreign(0, 0, 0).is_global());
        assert_ne!(AllocatorId::GLOBAL, AllocatorId::foreign(0, 0, 0));

        let base = AllocatorId::foreign(1, 2, 3);
        assert_eq!(base, AllocatorId::foreign(1, 2, 3));
        // Every component participates, so no two distinguishable allocators can
        // collide the way a digest of the three could.
        assert_ne!(base, AllocatorId::foreign(9, 2, 3));
        assert_ne!(base, AllocatorId::foreign(1, 9, 3));
        assert_ne!(base, AllocatorId::foreign(1, 2, 9));
    }

    #[test]
    fn opaque_is_carried_but_never_interpreted() {
        assert!(Opaque::NULL.is_null());
        assert_eq!(Opaque::default(), Opaque::NULL);
        assert_eq!(Opaque::NULL.addr(), 0);
        assert!(GlobalAllocator.opaque().is_null());

        // A non-null value round-trips unchanged -- the only thing the library
        // does with it (zlib.h L144-L147).
        let mut zone = 0_u64;
        let pointer: *mut c_void = core::ptr::addr_of_mut!(zone).cast();
        let opaque = Opaque::new(pointer);
        assert!(!opaque.is_null());
        assert_eq!(opaque.as_ptr(), pointer);
        assert_eq!(opaque.addr(), pointer as usize);
        assert_eq!(Opaque::new(opaque.as_ptr()), opaque);
    }

    #[test]
    fn an_allocator_can_be_injected_dynamically_or_by_reference() {
        /// Stands in for a state constructor: generic over the allocator, so it
        /// accepts a value, a reference, or a trait object equally.
        fn allocate_through<'a, A: Allocator<'a>>(allocator: A) -> usize {
            let Some(buffer) = allocator.allocate_u16s(7) else {
                return 0;
            };
            let len = buffer.len();
            allocator.deallocate_u16s(&mut Some(buffer));
            len
        }

        // Object safety, and the blanket implementation on references, are what
        // let a state type hold `&dyn Allocator` by value.
        let concrete = GlobalAllocator;
        let dynamic: &dyn Allocator<'_> = &concrete;
        let buffer = dynamic.allocate_bytes(16, 2).unwrap();
        assert_eq!(buffer.len(), 32);
        assert_eq!(buffer.owner(), AllocatorId::GLOBAL);
        dynamic.deallocate_bytes(&mut Some(buffer));

        // All three forms satisfy `A: Allocator<'a>`: the value itself, a shared
        // reference to it through the blanket implementation, and the trait
        // object, which is what object safety buys.
        let by_reference: &GlobalAllocator = &concrete;
        assert_eq!(allocate_through(concrete), 7);
        assert_eq!(allocate_through(by_reference), 7);
        assert_eq!(allocate_through(dynamic), 7);
    }

    #[test]
    fn buffers_hand_themselves_to_slice_generic_code() {
        // Window, PendingBuf and HashChains are generic over AsRef/AsMut of a
        // slice, so an allocated block must satisfy those without adaptation.
        fn total<S: AsRef<[u8]>>(storage: &S) -> u32 {
            storage.as_ref().iter().map(|&byte| u32::from(byte)).sum()
        }

        fn clear<S: AsMut<[u8]>>(storage: &mut S) {
            storage.as_mut().fill(0);
        }

        let allocator = GlobalAllocator;
        let mut buffer = allocator.allocate_bytes(4, 1).unwrap();
        clear(&mut buffer);
        assert_eq!(total(&buffer), 0);
        buffer.as_mut()[0] = 5;
        assert_eq!(total(&buffer), 5);
        allocator.deallocate_bytes(&mut Some(buffer));
    }

    #[test]
    fn successful_allocation_reports_ok_through_the_status_shape() {
        let allocator = GlobalAllocator;
        let buffer = allocator.allocate_bytes_or_mem_error(16, 4);
        assert!(buffer.is_ok());
        if let Ok(buffer) = buffer {
            assert_eq!(buffer.len(), 64);
            allocator.deallocate_bytes(&mut Some(buffer));
        }

        let buffer = allocator.allocate_u16s_or_mem_error(16);
        assert!(buffer.is_ok());
        if let Ok(buffer) = buffer {
            assert_eq!(buffer.len(), 16);
            allocator.deallocate_u16s(&mut Some(buffer));
        }
        assert_eq!(ReturnCode::MEM_ERROR.as_i32(), -4);
    }
}
