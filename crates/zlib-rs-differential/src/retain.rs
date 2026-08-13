//! The retention discipline: a `z_stream` held together with everything the library points into.
//!
//! # The problem this module exists to solve
//!
//! Three of zlib's entry points hand the library a pointer it keeps **after the call returns**:
//!
//! * `deflateSetHeader` / `inflateGetHeader` -- `deflate.c` stores the `gz_header` in
//!   `s->gzhead` and reads it until the header has been emitted; `inflate.c` stores it in
//!   `state->head` and *writes through* `head->extra`, `head->name` and `head->comment` as it
//!   parses (`zlib.h` L724-L740 and L1070-L1093).
//! * `inflateBackInit_` -- the window belongs to the caller and must stay untouched until
//!   `inflateBackEnd` returns (`zlib.h` L1175-L1177).
//! * `zalloc`/`zfree`/`opaque` -- the `opaque` word is handed to every allocation and free the
//!   stream performs, for the whole life of the stream (`zlib.h` L85-L86).
//!
//! A safe wrapper written as `fn inflate_get_header(strm: &mut z_stream, head: &mut gz_header)`
//! cannot express any of that. Its borrows end when it returns, so *safe* code is then free to
//! drop, move or re-borrow storage the library still holds a pointer into, and the next
//! `inflate()` call writes through a dangling pointer -- a use-after-free reachable without
//! writing `unsafe` anywhere. Every consumer of the harness carried that obligation in a comment
//! instead, and a comment is not a borrow checker.
//!
//! # The shape of the fix
//!
//! [`crate::retain::Session`] owns the stream and is the only way to reach it, and its lifetime parameter `'r`
//! is the lifetime of *everything the library retains*. Handing storage over goes through
//! [`crate::retain::Session::install`] or [`crate::retain::Session::install_shared`], which take `&'r mut T` / `&'r T`, so:
//!
//! * the storage stays borrowed for as long as the session's type is nameable, which is as long
//!   as the stream is reachable -- reaching it at all means going through
//!   [`crate::retain::Session::stream`] or the `Deref` impls;
//! * `'r` is **invariant** (through `PhantomData<&'r mut ()>`), so it cannot be shortened to slip
//!   the borrow out from underneath the stream;
//! * the [`Drop`] impl is load-bearing rather than decorative. It gives the type drop-check
//!   obligations, which makes `'r` have to *strictly outlive* the session value -- and that is
//!   what forces the storage to be declared **before** the session in the caller's frame. Declare
//!   it after and the program does not compile, which is precisely the mistake that used to be a
//!   comment saying "the probe must not be moved while the stream is live".
//!
//! The stream is boxed. That is not a convenience: `deflate_state` keeps a `strm` back-pointer
//! (`deflate.c`'s `s->strm = strm`), so an initialised `z_stream` that is *moved* leaves the state
//! block pointing at its old address. Owning it behind a `Box` makes the address stable no matter
//! how the session itself is moved around, so that hazard cannot be reached either.
//!
//! # What this module is not
//!
//! It contains no `unsafe`, makes no library call of its own and asserts nothing. The two boundary
//! modules keep every `extern "C"` call and every `// SAFETY:` comment: [`crate::retain::Session::install`] hands
//! the closure a `&mut` to the stream and a `&mut` to the storage and does nothing else with
//! either. One session type serves both implementations because it is generic over the stream
//! type -- [`crate::port`] drives `libz_rs_sys::z_stream` and [`crate::oracle`] drives its own
//! `#[repr(C)]` mirror, which are different Rust types with the same layout.

use core::marker::PhantomData;

/// A `z_stream` together with every allocation the library retains a pointer into.
///
/// `S` is the stream type -- `libz_rs_sys::z_stream` for the port, `crate::oracle::z_stream` for
/// the reference. `'r` is the lifetime of the retained storage; see the module docs for why it is
/// invariant and why the [`Drop`] impl matters.
///
/// # Example
///
/// ```ignore
/// // The header is declared FIRST.  Swap the two lines and this does not compile.
/// let mut header = header_pointing_at(&mut buffers);
/// let mut session = port::Session::new(port::zeroed_stream());
///
/// assert_eq!(port::inflate_init2(session.stream(), 47), Z_OK);
/// assert_eq!(port::inflate_get_header(&mut session, &mut header), Z_OK);
/// // ... drive the stream through `session.stream()` ...
/// assert_eq!(port::inflate_end(session.stream()), Z_OK);
/// ```
#[derive(Debug)]
pub struct Session<'r, S> {
    /// The stream itself, boxed so that its address does not move with the session.
    stream: Box<S>,

    /// The extent of the window handed to `inflateBackInit_`, recorded by
    /// [`Session::install_back_window`] as `(base address, length)`.
    ///
    /// Kept as an address rather than as the slice so that the callback bridge can decide whether a
    /// region the library reports lies inside the window using arithmetic alone, without forming a
    /// second reference to memory the library is writing through. The borrow that keeps the
    /// allocation alive is `'r`; this pair is only how the bridge recognises it.
    back_window: Option<(usize, usize)>,

    /// Every retained storage is borrowed for `'r`. Invariant, so `'r` cannot be shortened.
    retained: PhantomData<&'r mut ()>,
}

impl<'r, S> Session<'r, S> {
    /// Opens a session over `stream`, retaining nothing yet.
    #[must_use]
    pub fn new(stream: S) -> Self {
        Self {
            stream: Box::new(stream),
            back_window: None,
            retained: PhantomData,
        }
    }

    /// The stream, for every entry point that retains nothing.
    pub fn stream(&mut self) -> &mut S {
        &mut self.stream
    }

    /// The stream, read-only, for inspecting members after a call.
    #[must_use]
    pub fn peek(&self) -> &S {
        &self.stream
    }

    /// Hands `storage` to the library through `install`, and keeps it borrowed for `'r`.
    ///
    /// `install` receives the stream and the storage for the duration of the call and is where the
    /// `extern "C"` call and its `// SAFETY:` comment live. What this function contributes is the
    /// *signature*: `storage: &'r mut T` is what makes the borrow outlive the call rather than end
    /// with it.
    ///
    /// Calling this twice with different storage is sound and is the case where the library itself
    /// replaces the pointer -- a second `inflateGetHeader` overwrites `state->head`, so the first
    /// header's borrow may legitimately end. The session's `'r` covers whichever storage is longest
    /// lived, because the caller must produce `&'r mut T` for each.
    pub fn install<T, R>(
        &mut self,
        storage: &'r mut T,
        install: impl FnOnce(&mut S, &mut T) -> R,
    ) -> R
    where
        T: ?Sized,
    {
        install(&mut self.stream, storage)
    }

    /// [`Session::install`] for storage the library only reads, or reaches through an opaque word.
    ///
    /// The `zalloc`/`zfree`/`opaque` triple is the case: the ledger behind `opaque` is shared
    /// rather than exclusively borrowed, because the allocator callbacks reach it through that word
    /// while the harness reads it through its own handle.
    pub fn install_shared<T, R>(
        &mut self,
        storage: &'r T,
        install: impl FnOnce(&mut S, &'r T) -> R,
    ) -> R
    where
        T: ?Sized,
    {
        install(&mut self.stream, storage)
    }

    /// Hands `window` to `inflateBackInit_` through `init`, keeps it borrowed for `'r`, and records
    /// its extent for the callback bridge.
    ///
    /// The window is `&'r mut [u8]` and not `&'r [u8]`: the decoder writes decompressed bytes into
    /// this exact allocation and then reports regions of it to the output callback, so a shared
    /// reference to it would be a shared reference to memory that changes underneath it. The extent
    /// is recorded here rather than passed again at `inflateBack` time, which also retires the
    /// "must be the same slice you passed to init" obligation -- there is no second slice to get
    /// wrong.
    ///
    /// The extent is recorded even when `init` refuses, and that is deliberate: it costs nothing,
    /// and a refused initialisation leaves no state for `inflateBack` to run against anyway.
    pub fn install_back_window<R>(
        &mut self,
        window: &'r mut [u8],
        init: impl FnOnce(&mut S, &mut [u8]) -> R,
    ) -> R {
        self.back_window = Some((window.as_ptr() as usize, window.len()));
        init(&mut self.stream, window)
    }

    /// The `(base address, length)` of the window `inflateBackInit_` was given, if any.
    #[must_use]
    pub fn back_window(&self) -> Option<(usize, usize)> {
        self.back_window
    }
}

impl<S> core::ops::Deref for Session<'_, S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.stream
    }
}

impl<S> core::ops::DerefMut for Session<'_, S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

/// The load-bearing `Drop`.
///
/// It has nothing to release -- the `Box` and the borrows handle themselves -- and it must exist
/// anyway. Without a `Drop` impl, drop-check lets a value whose type mentions `'r` be dropped
/// *after* `'r` has ended, so the compiler would accept storage declared after the session and
/// dropped before it. With one, `'r` must strictly outlive the session, which is the rule this
/// module exists to enforce: **declare the retained storage before the session**.
///
/// Verified by construction: reversing the two declarations in any consumer of
/// [`Session::install`] produces `E0597: borrowed value does not live long enough`.
impl<S> Drop for Session<'_, S> {
    fn drop(&mut self) {
        // Forgetting the recorded extent is not required for soundness; it is here so that this
        // impl has a body a reader can attribute rather than an empty one they have to guess at,
        // and so that a use-after-drop of the session through some future accessor could not read
        // a stale window address.
        self.back_window = None;
    }
}

/// Where a region the `inflateBack` output callback reports sits inside the window, if it sits
/// inside it at all.
///
/// `base`/`len` are the window's recorded extent, `start`/`count` the region the library named.
/// The answer is `Some(offset)` only when the whole region lies within the window, and the caller
/// must treat `None` as "do not touch that memory" rather than as "offset unknown": a pointer
/// outside the window is a library defect, and forming a slice from it in order to report the
/// defect would be undefined behaviour in the harness before the defect could be recorded.
///
/// A zero-length region is answered as `Some` when its start is in range, because `infback.c` can
/// legitimately report an empty flush at the end of a stream, and the caller then hands the sink an
/// empty slice rather than reading anything.
#[must_use]
pub fn window_offset(base: usize, len: usize, start: usize, count: usize) -> Option<usize> {
    let offset = start.checked_sub(base)?;
    let end = offset.checked_add(count)?;
    (end <= len).then_some(offset)
}

#[cfg(test)]
mod tests {
    use super::window_offset;

    /// The whole point: a region that is not inside the window is refused, not clamped.
    #[test]
    fn a_region_outside_the_window_is_refused() {
        let (base, len) = (0x1000, 0x100);

        // Before the window.
        assert_eq!(window_offset(base, len, base - 1, 1), None);
        assert_eq!(window_offset(base, len, 0, 1), None);
        // Starting inside but running past the end -- the case a clamp would have hidden.
        assert_eq!(window_offset(base, len, base + len - 1, 2), None);
        assert_eq!(window_offset(base, len, base, len + 1), None);
        // Entirely past the end.
        assert_eq!(window_offset(base, len, base + len, 1), None);
        // A count that would overflow the address space.
        assert_eq!(window_offset(base, len, base, usize::MAX), None);
    }

    /// And a region that is inside it is answered with its offset.
    #[test]
    fn a_region_inside_the_window_is_answered_with_its_offset() {
        let (base, len) = (0x1000, 0x100);

        assert_eq!(window_offset(base, len, base, 0), Some(0));
        assert_eq!(window_offset(base, len, base, len), Some(0));
        assert_eq!(window_offset(base, len, base + 1, len - 1), Some(1));
        assert_eq!(window_offset(base, len, base + len, 0), Some(len));
        assert_eq!(window_offset(base, len, base + len - 1, 1), Some(len - 1));
    }

    /// An empty window accepts nothing but an empty region at its base.
    #[test]
    fn an_empty_window_accepts_only_an_empty_region_at_its_base() {
        assert_eq!(window_offset(0x2000, 0, 0x2000, 0), Some(0));
        assert_eq!(window_offset(0x2000, 0, 0x2000, 1), None);
        assert_eq!(window_offset(0x2000, 0, 0x2001, 0), None);
    }
}
