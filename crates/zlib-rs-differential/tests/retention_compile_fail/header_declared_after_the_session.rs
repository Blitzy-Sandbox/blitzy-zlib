//@ error: E0597
//@ proves: a gz_header that would be dropped before the stream cannot be installed on it.

//! `deflateSetHeader` stores the caller's pointer in the deflate state and reads through
//! it while the gzip header is being emitted, which happens on later `deflate` calls --
//! `zlib.h` L681-L697. A header declared after the session is dropped first.

use zlib_rs_differential::port::{deflate_set_header, zeroed_header, zeroed_stream, Session};

fn main() {
    let mut session = Session::new(zeroed_stream());
    let mut head = zeroed_header();
    let _ = deflate_set_header(&mut session, &mut head);
}
