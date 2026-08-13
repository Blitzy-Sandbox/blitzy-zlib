//@ error: E0503
//@ proves: a gz_header installed on a live stream cannot be read while the library may still be writing through it.

//! `inflateGetHeader` gives the library a pointer it writes through during `inflate`
//! until the header completes -- `zlib.h` L1070-L1096. Reading the structure before the
//! stream has been ended races that write. The exclusive loan the session holds is what
//! makes the read a borrow error rather than a data race nobody notices.  `E0503` rather
//! than `E0502` because the read is of a *field* of the mutably borrowed structure.

use zlib_rs_differential::port::{
    inflate_get_header, inflate_init2, zeroed_header, zeroed_stream, Session,
};

fn main() {
    let mut head = zeroed_header();
    let mut session = Session::new(zeroed_stream());
    let _ = inflate_init2(session.stream(), 47);
    let _ = inflate_get_header(&mut session, &mut head);
    // The stream is still live, so the library may still write here.
    let _peek = head.done;
    drop(session);
}
