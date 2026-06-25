//! DEFLATE compressor (`deflate.c` / `deflate.h` / `trees.c` / `trees.h`).
//!
//! This module root wires together the compressor's submodules. At the current
//! foundation milestone it declares the data-model leaves:
//!
//! * [`state`] — the owned compressor state ([`state::DeflateState`]) and its
//!   status enum, the safe-Rust counterpart of the C `deflate_state` struct.
//! * [`trees`] — the Huffman tree *data structures* (`ct_data`, the static tree
//!   descriptors, and the per-code extra-bit tables) consumed by `state`.
//!
//! The match-search strategies and the tree-construction / block-emission
//! algorithms (`deflate_fast`/`deflate_slow`/`_tr_*`) layer on top of these
//! types in subsequent milestones.

pub mod state;
pub mod trees;
