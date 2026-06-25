//! DEFLATE decompressor (`inflate.c` / `inffast.c` / `inftrees.c` /
//! `inffixed.h` / `infback.c`).
//!
//! This module root wires together the decompressor's submodules. At the
//! current foundation milestone it declares the data-model leaves:
//!
//! * [`tables`] — the decode-table builder (`inflate_table`) and the
//!   [`tables::Code`] entry type / `ENOUGH` sizing constants (`inftrees.c` /
//!   `inftrees.h`).
//! * [`fixed`] — the precomputed fixed literal/length and distance decode
//!   tables (`inffixed.h`).
//! * [`state`] — the owned decompressor state ([`state::InflateState`]) and its
//!   [`state::InflateMode`] machine, the safe-Rust counterpart of the C
//!   `inflate_state` struct.
//! * [`fast`] — the hot-path inner decode loop ([`fast::inflate_fast`]), the
//!   safe-Rust counterpart of C `inflate_fast` (`inffast.c`).
//! * [`back`] — the callback-driven raw-DEFLATE decoder
//!   ([`back::inflate_back`] and friends), the safe-Rust counterpart of C
//!   `infback.c`. It layers on top of [`state`], [`tables`], [`fixed`], so it
//!   is declared after them.
//!
//! The remaining `inflate` streaming driver entry point layers on top of these
//! types in a subsequent milestone.

pub mod tables;

pub mod fixed;
pub mod state;

pub mod fast;

pub mod back;
