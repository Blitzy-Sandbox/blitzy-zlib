//@ error: E0597
//@ proves: a tracking allocator that would be dropped before the stream it is installed on cannot be installed at all.

//! `TrackingAllocator::install` writes the ledger's address into `opaque`, and every
//! allocation and free the stream makes until `deflateEnd`/`inflateEnd` goes through it.
//! A ledger declared *after* the session is dropped *before* it, so the library would
//! spend its whole teardown calling hooks against freed memory.

use zlib_rs_differential::port::{zeroed_stream, Session, TrackingAllocator};

fn main() {
    let mut session = Session::new(zeroed_stream());
    let allocator = TrackingAllocator::new();
    allocator.install(&mut session);
}
