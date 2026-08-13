# `retention_compile_fail`

Five programs that must **not** compile, and one that must.

`crates/zlib-rs-differential/src/retain.rs` makes three retention obligations into
type-system facts rather than comments: the tracking allocator's ledger, a
`gz_header`, and the `inflateBack` window are all addresses the library keeps
*after* the call that installed them returns, so each is lent to a
`retain::Session` for the session's whole life. The claim that follows -- "getting
this wrong is a build failure, not a latent use-after-free" -- is only worth making
if something checks it.

These cases are that check. Each is a whole program whose only defect is one of the
mistakes the design exists to prevent, annotated with the diagnostic it must
produce. `.github/scripts/retention_compile_fail.py` type-checks every one of them
and fails when a case compiles, when it fails with a *different* error, or when the
control case stops compiling -- that last one being what keeps the suite honest, since
five cases that fail for an unrelated reason would look identical to five cases that
fail for the right one.

Nothing here is a Cargo target: Cargo auto-discovers `tests/*.rs` and
`tests/*/main.rs`, and this directory has neither, so `cargo test`, `cargo clippy`
and `cargo fmt` never see these files. The driver builds the harness library itself
-- so it always type-checks against current metadata rather than whatever happens to
be on disk -- and then invokes `rustc --emit=metadata` per case, which stops before
linking and therefore needs none of the C oracle archive the crate normally links.
The whole suite takes about a second.

Each case names **only** `zlib_rs_differential`. That is a correctness requirement,
not economy: `rustc` then resolves every other crate from that one's metadata, so a
case can never be checked against a differently configured build of a dependency.
Anything a case needs from a dependency is reached through a gate the harness
re-exports, such as `port::zeroed_header()`.

Run it locally -- no prior build needed:

```
python3 .github/scripts/retention_compile_fail.py             # debug profile
python3 .github/scripts/retention_compile_fail.py --release   # the CI job's profile
```

## Annotation format

Two directives, both required, in a comment before any code:

* `//@ error: E0597` -- the error codes the case must produce, comma-separated, or
  `none` for the control case.
* `//@ proves: <one line>` -- what the reader learns from this case failing.
