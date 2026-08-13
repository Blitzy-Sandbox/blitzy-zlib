//@ error: E0597
//@ proves: an inflateBack window that would be dropped before the stream cannot be installed on it.

//! `inflateBackInit_` keeps the window pointer for the life of the stream -- `infback.c`
//! L25-L64 decodes directly into it on every call -- and only `inflateBackEnd` releases
//! it. A window declared after the session is dropped first.

use zlib_rs_differential::port::{inflate_back_init, zeroed_stream, Session};

fn main() {
    let mut session = Session::new(zeroed_stream());
    let mut window = vec![0u8; 1 << 15];
    let _ = inflate_back_init(&mut session, 15, &mut window);
}
