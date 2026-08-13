//@ error: none
//@ proves: the correct declaration order type-checks, so the five refusals below are about order and not about a broken API.

//! The arrangement every caller is supposed to use: each retained storage declared
//! before the session that borrows it, and every read of that storage placed after the
//! session is gone.
//!
//! This case exists so the suite cannot pass vacuously. If the gates ever stop
//! type-checking at all -- a renamed method, a changed signature, a lifetime that no
//! longer unifies -- the five negative cases would keep "failing correctly" while
//! proving nothing. This one turns that into a gate failure.

//! Only `zlib_rs_differential` is named, here and in every other case. That is deliberate:
//! the driver hands `rustc` one `--extern` and lets it resolve the rest from metadata, so a
//! case can never be paired with a differently-configured build of a dependency. Measured --
//! naming `libz_rs_sys` too made the release profile fail with `E0308`, because that profile
//! holds two `libz.rlib` spellings and only one of them is the crate the harness was compiled
//! against.

use zlib_rs_differential::port::{
    deflate_end, deflate_init2, deflate_set_header, inflate_back_end, inflate_back_init,
    zeroed_header, zeroed_stream, Session, TrackingAllocator,
};

fn main() {
    // The allocator ledger: installed into `opaque`, kept until `deflateEnd`.
    let allocator = TrackingAllocator::new();
    // The header: kept from `deflateSetHeader` until the header has been emitted.
    let mut head = zeroed_header();
    let mut session = Session::new(zeroed_stream());
    allocator.install(&mut session);
    // 8 is `Z_DEFLATED`, 31 selects the gzip container, 0 is `Z_DEFAULT_STRATEGY`.
    let _ = deflate_init2(session.stream(), 6, 8, 31, 8, 0);
    let _ = deflate_set_header(&mut session, &mut head);
    let _ = deflate_end(session.stream());
    drop(session);
    // Only now is the header readable, and only now does the ledger have a final answer.
    let _ = head.done;
    let _ = allocator.finish();

    // The window: kept from `inflateBackInit_` until `inflateBackEnd`.
    let mut window = vec![0u8; 1 << 15];
    let mut back = Session::new(zeroed_stream());
    let _ = inflate_back_init(&mut back, 15, &mut window);
    let _ = inflate_back_end(back.stream());
    drop(back);
    let _ = window.len();
}
