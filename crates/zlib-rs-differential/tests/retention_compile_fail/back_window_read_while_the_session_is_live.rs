//@ error: E0502
//@ proves: an inflateBack window cannot be read while the stream that decodes into it is still live.

//! The decoder writes into the window on every `inflateBack` call and the caller has no
//! business reading it before `inflateBackEnd`; the offsets the sink is handed are the
//! supported way to see what was produced. This is the case that makes the window's
//! `&mut` real: with the old shared `&[u8]` shape, this read looked entirely legitimate
//! while the library was writing the same bytes.

use zlib_rs_differential::port::{inflate_back_init, zeroed_stream, Session};

fn main() {
    let mut window = vec![0u8; 1 << 15];
    let mut session = Session::new(zeroed_stream());
    let _ = inflate_back_init(&mut session, 15, &mut window);
    let _peek = window[0];
    drop(session);
}
