//! Cargo build script for the `libz-rs-sys` C-ABI shim crate.
//!
//! # Responsibility
//!
//! This script bridges Rust → C: it runs [`cbindgen`](https://github.com/mozilla/cbindgen)
//! over this crate's `extern "C"` / `#[unsafe(no_mangle)]` surface and `#[repr(C)]`
//! types to (re)generate the committed C header `include/zlib.h`, then performs a
//! best-effort *signature* diff of that header against the canonical root `zlib.h`
//! to prove ABI/signature parity (AAP §0.4.1, goal G4 — a true zlib drop-in).
//!
//! It is the mechanism that turns "we *intend* to match the zlib ABI" into a
//! machine-checked, regenerated-on-every-relevant-change artifact.
//!
//! # Robustness contract (CRITICAL)
//!
//! Header generation is a *developer / CI convenience*, never a hard build
//! requirement for downstream consumers. Therefore this script is written to be
//! **non-fatal**: in restricted environments (docs.rs, packaged crates, read-only
//! source trees) where cbindgen cannot run or the `include/` directory is not
//! writable, it emits a `cargo:warning=...` and continues rather than panicking.
//! `cargo build -p libz-rs-sys` must always stay green.
//!
//! The one *opt-in* exception is a strict CI gate: setting the environment
//! variable [`STRICT_ENV`] (`LIBZ_RS_SYS_STRICT_HEADER`) to any value promotes
//! detected signature drift to a hard `panic!`. This is intended for use *after*
//! the full export surface exists, to fail CI if the C header ever diverges from
//! canonical `zlib.h`. It is deliberately OFF by default.
//!
//! # Why generate unconditionally
//!
//! Generation is *not* gated behind the `capi`/feature axis: `cbindgen` is always
//! available as a `[build-dependencies]` crate, and keeping generation
//! unconditional-but-non-fatal (rather than `CARGO_FEATURE_*`-gated) means the
//! committed `include/zlib.h` is refreshed whenever the FFI surface changes in any
//! build configuration, while never being able to break a build. (See the agent
//! prompt's Phase 4 "Recommended default".)
//!
//! # Implementation notes
//!
//! * Pure **safe** Rust — no `unsafe` is needed or used here. Although build
//!   scripts run on the host with `std` available and are *not* subject to the
//!   core crate's `#![forbid(unsafe_code)]`, this script honours the spirit of the
//!   project's safety re-architecture.
//! * Only the standard library plus the `cbindgen` build-dependency are used.
//! * The generated header is produced **in memory** via [`cbindgen::Bindings::write`]
//!   and then persisted with explicit, `Result`-checked filesystem calls. We avoid
//!   [`cbindgen::Bindings::write_to_file`] on purpose: it `.unwrap()`s its I/O and
//!   would *panic* on a read-only target, violating the robustness contract above.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Environment variable that, when present (any value), promotes header-parity
/// drift from a non-fatal `cargo:warning` to a hard `panic!`. Intended for CI use
/// once the full C-ABI export surface exists; OFF by default.
const STRICT_ENV: &str = "LIBZ_RS_SYS_STRICT_HEADER";

/// File name of the committed, generated C header (under `include/`).
const HEADER_NAME: &str = "zlib.h";

/// The complete set of public zlib symbols the C-ABI drop-in must export,
/// transcribed verbatim from the AAP §0.4.1 export surface. The parity check
/// verifies each of these appears as a function prototype in the generated header
/// and (where present in both) that its argument *arity* matches canonical
/// `zlib.h`.
///
/// At early milestones, where `src/lib.rs` has not yet layered the
/// `#[unsafe(no_mangle)]` wrappers over the safe core, most of these will be
/// reported as "not yet exported" — that is expected and non-fatal. The fix for a
/// genuinely missing symbol is always in `src/lib.rs` / `src/zstream.rs` (make the
/// item `pub`, `#[unsafe(no_mangle)] extern "C"`, named exactly), never here.
const EXPECTED_SYMBOLS: &[&str] = &[
    // ---- deflate family (14) -------------------------------------------------
    "deflateInit_",
    "deflateInit2_",
    "deflate",
    "deflateEnd",
    "deflateReset",
    "deflateParams",
    "deflateBound",
    "deflatePrime",
    "deflateSetDictionary",
    "deflateGetDictionary",
    "deflateSetHeader",
    "deflateCopy",
    "deflateTune",
    "deflatePending",
    // ---- inflate family (16) -------------------------------------------------
    "inflateInit_",
    "inflateInit2_",
    "inflate",
    "inflateEnd",
    "inflateReset",
    "inflateReset2",
    "inflateSync",
    "inflateCopy",
    "inflatePrime",
    "inflateMark",
    "inflateGetHeader",
    "inflateSetDictionary",
    "inflateGetDictionary",
    "inflateBackInit_",
    "inflateBack",
    "inflateBackEnd",
    // ---- one-shot helpers (5) ------------------------------------------------
    "compress",
    "compress2",
    "compressBound",
    "uncompress",
    "uncompress2",
    // ---- checksums (9) -------------------------------------------------------
    "adler32",
    "adler32_z",
    "adler32_combine",
    "crc32",
    "crc32_z",
    "crc32_combine",
    "crc32_combine_gen",
    "crc32_combine_op",
    "get_crc_table",
    // ---- version / diagnostics (3) ------------------------------------------
    "zlibVersion",
    "zlibCompileFlags",
    "zError",
    // ---- gz file API (26; feature `gz-io`, guarded by WITH_GZFILEOP) ---------
    "gzopen",
    "gzdopen",
    "gzbuffer",
    "gzsetparams",
    "gzread",
    "gzfread",
    "gzwrite",
    "gzfwrite",
    "gzprintf",
    "gzputs",
    "gzputc",
    "gzgets",
    "gzgetc",
    "gzungetc",
    "gzflush",
    "gzseek",
    "gzrewind",
    "gztell",
    "gzoffset",
    "gzeof",
    "gzdirect",
    "gzclose",
    "gzclose_r",
    "gzclose_w",
    "gzerror",
    "gzclearerr",
];

/// `#[repr(C)]` aggregate tags whose field layout is compared, field-name by
/// field-name and in order, between the generated header and canonical `zlib.h`.
/// A mismatch here would be an ABI break. The comparison is best-effort: if either
/// side cannot be parsed it is silently skipped (never a false positive).
const EXPECTED_STRUCTS: &[&str] = &["z_stream_s", "gz_header_s"];

fn main() {
    // ---- Phase 1: tell Cargo precisely when to re-run this script -----------
    emit_rerun_directives();

    // The crate root, provided by Cargo for every build-script invocation.
    let crate_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(e) => {
            // Should never happen under Cargo; degrade gracefully regardless.
            warn(&format!(
                "CARGO_MANIFEST_DIR is unavailable ({e}); skipping header generation"
            ));
            return;
        }
    };

    // ---- Phase 2: generate the C header (in memory, non-fatal) --------------
    let generated = match generate_header(&crate_dir) {
        Some(header) => header,
        None => return, // a cargo:warning was already emitted
    };

    // ---- Phase 2 (cont.): persist the committed include/zlib.h deliverable --
    persist_header(&crate_dir, &generated);

    // ---- Phase 3: best-effort signature parity vs canonical zlib.h ----------
    check_parity(&crate_dir, &generated);
}

/// Phase 1 — emit the `cargo:rerun-if-changed` / `cargo:rerun-if-env-changed`
/// directives so the header is regenerated only when an input that affects it
/// actually changes (the FFI surface, the cbindgen config, the canonical header,
/// or the strict-mode toggle), and never re-runs unnecessarily otherwise.
///
/// Paths are interpreted relative to `CARGO_MANIFEST_DIR`; `../zlib.h` therefore
/// refers to the canonical header at the repository root.
fn emit_rerun_directives() {
    const WATCHED: &[&str] = &[
        "build.rs",
        "cbindgen.toml",
        "src/lib.rs",
        "src/zstream.rs",
        "src/translate.rs",
        "../zlib.h",
    ];
    for path in WATCHED {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed={STRICT_ENV}");
}

/// Emit a non-fatal build warning, tagged with this script's name so the origin
/// is unambiguous in `cargo build` output.
fn warn(message: &str) {
    println!("cargo:warning=libz-rs-sys/build.rs: {message}");
}

/// Phase 2 — load the cbindgen configuration and generate the C header into an
/// in-memory `String`. Returns `None` (after emitting a `cargo:warning`) if
/// cbindgen cannot run, so the caller can return gracefully without panicking.
fn generate_header(crate_dir: &Path) -> Option<String> {
    // Load `cbindgen.toml`; fall back to defaults (with a warning) if it is
    // missing or malformed, so generation still proceeds best-effort.
    let config_path = crate_dir.join("cbindgen.toml");
    let config = match cbindgen::Config::from_file(&config_path) {
        Ok(config) => config,
        Err(e) => {
            warn(&format!(
                "could not load {} ({e}); falling back to cbindgen defaults",
                config_path.display()
            ));
            cbindgen::Config::default()
        }
    };

    // Parse this crate and emit bindings. `parse_deps = false` in cbindgen.toml
    // keeps cbindgen scoped to this crate's own modules (the idiomatic, non-repr-C
    // `zlib-rs` core types must never leak into the C header).
    let bindings = match cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => bindings,
        Err(e) => {
            warn(&format!("cbindgen header generation skipped: {e}"));
            return None;
        }
    };

    // Render to memory. Writing to a `Vec<u8>` is infallible, unlike
    // `Bindings::write_to_file`, which `.unwrap()`s its filesystem I/O.
    let mut buffer: Vec<u8> = Vec::new();
    bindings.write(&mut buffer);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// Phase 2 (cont.) — persist the generated header to `include/zlib.h`,
/// best-effort. Any filesystem error (read-only tree, permission denied, …) is
/// reported as a `cargo:warning` and swallowed; it never fails the build.
///
/// If an identical header already exists on disk the write is skipped entirely,
/// which avoids needless mtime churn / version-control noise and avoids touching a
/// read-only file whose contents are already correct.
fn persist_header(crate_dir: &Path, generated: &str) {
    let include_dir = crate_dir.join("include");
    let header_path = include_dir.join(HEADER_NAME);

    // Skip if the on-disk header is already byte-identical.
    if let Ok(existing) = fs::read_to_string(&header_path) {
        if existing == generated {
            return;
        }
    }

    if let Err(e) = fs::create_dir_all(&include_dir) {
        warn(&format!(
            "could not create {} ({e}); generated header not persisted",
            include_dir.display()
        ));
        return;
    }

    if let Err(e) = fs::write(&header_path, generated.as_bytes()) {
        warn(&format!(
            "could not write {} ({e}); generated header not persisted",
            header_path.display()
        ));
    }
}

/// Phase 3 — best-effort signature-parity check of the generated header against
/// the canonical root `zlib.h` (`../zlib.h`).
///
/// The comparison is intentionally *signature-level*, not a byte diff: cbindgen
/// has its own house style (comment formatting, item ordering, include-guard
/// name) and those cosmetic differences are acceptable. What is checked:
///
/// 1. **Function presence** — every [`EXPECTED_SYMBOLS`] entry should appear as a
///    real prototype in the generated header. Missing entries are reported.
/// 2. **Function arity** — for symbols present in *both* headers, the argument
///    count must agree. Arity is robust to harmless type-spelling differences
///    (e.g. `z_streamp` vs `struct z_stream *`) that a raw text diff would
///    falsely flag.
/// 3. **Struct field layout** — the ordered field names of [`EXPECTED_STRUCTS`]
///    must agree (best-effort; skipped silently if unparseable).
///
/// All findings are emitted as `cargo:warning`. If [`STRICT_ENV`] is set and any
/// drift was found, the script `panic!`s (CI gate). If the canonical header is
/// absent (e.g. a consumer building only the packaged crate), the check is skipped
/// silently.
fn check_parity(crate_dir: &Path, generated: &str) {
    let canonical_path = crate_dir.join("..").join("zlib.h");
    if !canonical_path.is_file() {
        // Nothing to diff against (packaged/standalone crate build). Not an error.
        return;
    }
    let canonical = match fs::read_to_string(&canonical_path) {
        Ok(text) => text,
        Err(e) => {
            warn(&format!(
                "could not read canonical {} ({e}); skipping signature parity check",
                canonical_path.display()
            ));
            return;
        }
    };

    // Normalise both headers: drop comments, then drop preprocessor lines (with
    // backslash continuations). The latter is essential — cbindgen's
    // `after_includes` block defines convenience macros whose *bodies* call the
    // underscore entry points (`deflateInit_((strm), ...)`), which would otherwise
    // be mistaken for real prototypes and mask genuinely-missing exports.
    let generated_clean = strip_preprocessor_lines(&strip_c_comments(generated));
    let canonical_clean = strip_preprocessor_lines(&strip_c_comments(&canonical));

    let mut drift: Vec<String> = Vec::new();

    // (1) + (2): function presence and arity.
    let mut missing: Vec<&str> = Vec::new();
    for &symbol in EXPECTED_SYMBOLS {
        match find_arity(&generated_clean, symbol) {
            None => missing.push(symbol),
            Some(generated_arity) => {
                if let Some(canonical_arity) = find_arity(&canonical_clean, symbol) {
                    if generated_arity != canonical_arity {
                        let message = format!(
                            "symbol `{symbol}` arity differs: generated header declares \
                             {generated_arity} argument(s), canonical zlib.h declares \
                             {canonical_arity}"
                        );
                        warn(&format!("header parity: {message}"));
                        drift.push(message);
                    }
                }
            }
        }
    }
    if !missing.is_empty() {
        let total = EXPECTED_SYMBOLS.len();
        let present = total - missing.len();
        let sample: Vec<&str> = missing.iter().take(8).copied().collect();
        let ellipsis = if missing.len() > sample.len() {
            ", …"
        } else {
            ""
        };
        warn(&format!(
            "header parity: {present}/{total} expected zlib symbols present in generated \
             include/{HEADER_NAME}; {} not yet exported (e.g. {}{ellipsis}) — add the missing \
             `#[unsafe(no_mangle)] extern \"C\"` wrappers in src/lib.rs",
            missing.len(),
            sample.join(", "),
        ));
        drift.push(format!(
            "{} expected zlib symbol(s) missing from generated header",
            missing.len()
        ));
    }

    // (3): struct field-name layout (best-effort, never a false positive).
    for &struct_tag in EXPECTED_STRUCTS {
        if let (Some(generated_fields), Some(canonical_fields)) = (
            extract_struct_fields(&generated_clean, struct_tag),
            extract_struct_fields(&canonical_clean, struct_tag),
        ) {
            if generated_fields != canonical_fields {
                let message = format!(
                    "struct `{struct_tag}` field layout differs: generated {generated_fields:?} \
                     vs canonical zlib.h {canonical_fields:?}"
                );
                warn(&format!("header parity: {message}"));
                drift.push(message);
            }
        }
    }

    // Strict CI gate (opt-in): turn any detected drift into a hard failure.
    if env::var_os(STRICT_ENV).is_some() && !drift.is_empty() {
        panic!(
            "{STRICT_ENV} is set and the generated C header diverges from canonical zlib.h:\n  - {}",
            drift.join("\n  - ")
        );
    }
}

/// Return `true` if `byte` may appear within a C identifier (`[A-Za-z0-9_]`).
fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// Replace C/C++ comments (`/* … */` and `// …`) with whitespace, preserving
/// newlines and string/char literals. A small state machine tracks string and
/// character literals (honouring backslash escapes) so that comment markers that
/// appear *inside* literals are not mistaken for real comments.
fn strip_c_comments(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        StringLit,
        CharLit,
    }

    let bytes = source.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut state = State::Code;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let next = bytes.get(i + 1).copied().unwrap_or(0);
        match state {
            State::Code => match (c, next) {
                (b'/', b'/') => {
                    state = State::LineComment;
                    i += 2;
                }
                (b'/', b'*') => {
                    state = State::BlockComment;
                    out.push(b' ');
                    i += 2;
                }
                (b'"', _) => {
                    state = State::StringLit;
                    out.push(c);
                    i += 1;
                }
                (b'\'', _) => {
                    state = State::CharLit;
                    out.push(c);
                    i += 1;
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            },
            State::LineComment => {
                if c == b'\n' {
                    state = State::Code;
                    out.push(b'\n');
                }
                i += 1;
            }
            State::BlockComment => {
                if c == b'*' && next == b'/' {
                    state = State::Code;
                    out.push(b' ');
                    i += 2;
                } else {
                    if c == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
            }
            State::StringLit | State::CharLit => {
                out.push(c);
                if c == b'\\' && i + 1 < bytes.len() {
                    // Copy the escaped byte verbatim so an escaped quote does not
                    // prematurely terminate the literal.
                    out.push(next);
                    i += 2;
                } else {
                    let closer = if state == State::StringLit {
                        b'"'
                    } else {
                        b'\''
                    };
                    if c == closer {
                        state = State::Code;
                    }
                    i += 1;
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Drop every C preprocessor directive line (a line whose first non-whitespace
/// character is `#`) together with any lines it continues onto via a trailing
/// backslash. Dropped lines are replaced with empty lines so that overall line
/// structure is preserved. Real declarations (which never begin with `#`) survive
/// unchanged, including function prototypes nested inside `#if …`/`#endif` guards.
fn strip_preprocessor_lines(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_directive = false;
    for line in source.lines() {
        let starts_directive = line.trim_start().starts_with('#');
        if in_directive || starts_directive {
            // Continue swallowing while the (logical) directive line ends with `\`.
            in_directive = line.trim_end().ends_with('\\');
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Locate a function declaration for `name` in a comment- and preprocessor-stripped
/// header and return its argument *arity* (number of top-level parameters; `void`
/// or an empty list counts as `0`). Returns `None` if no such prototype is found.
///
/// Matching is whole-word: the byte preceding `name` must not be an identifier
/// byte, and `name` must be followed (after optional whitespace) by `(`. This
/// cleanly distinguishes, e.g., `compress` from `uncompress`/`compress2`,
/// `crc32` from `crc32_z`, and `deflate` from `deflateInit`.
fn find_arity(header: &str, name: &str) -> Option<usize> {
    let bytes = header.as_bytes();
    let mut from = 0;
    while let Some(rel) = header[from..].find(name) {
        let start = from + rel;
        let end = start + name.len();
        from = end; // strictly monotonic ⇒ guaranteed termination

        // Word boundary on the left.
        if start > 0 && is_ident_byte(bytes[start - 1]) {
            continue;
        }
        // After the name, skip whitespace and require an opening parenthesis.
        let mut j = end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            // Either end-of-input, a longer identifier (left boundary already
            // handled the prefix case), or not a call/declaration — keep looking.
            continue;
        }
        if let Some(args) = capture_balanced(bytes, j, b'(', b')') {
            return Some(arity_of(&args));
        }
    }
    None
}

/// Count the top-level parameters in a captured argument list. A trimmed list of
/// `""` or `"void"` yields `0`; otherwise the count is one more than the number of
/// commas that sit at bracket/paren depth zero (so nested function-pointer or
/// array commas are ignored).
fn arity_of(args: &str) -> usize {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "void" {
        return 0;
    }
    let mut depth: i32 = 0;
    let mut count = 1usize;
    for ch in trimmed.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

/// Capture the text enclosed by a balanced `open`/`close` delimiter pair, given
/// the index of the opening delimiter. The outermost delimiters are excluded from
/// the returned string. Returns `None` if the delimiters are unbalanced.
fn capture_balanced(bytes: &[u8], open_idx: usize, open: u8, close: u8) -> Option<String> {
    let mut depth = 0usize;
    let mut inner: Vec<u8> = Vec::new();
    for &c in &bytes[open_idx..] {
        if c == open {
            depth += 1;
            if depth > 1 {
                inner.push(c);
            }
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(String::from_utf8_lossy(&inner).into_owned());
            }
            inner.push(c);
        } else {
            inner.push(c);
        }
    }
    None
}

/// Extract the ordered field names of `struct <struct_tag> { … }` from a comment-
/// and preprocessor-stripped header. Returns `None` if no struct *definition*
/// (as opposed to a forward declaration or pointer use) is found, or if the body
/// cannot be parsed — callers treat `None` as "skip silently".
fn extract_struct_fields(header: &str, struct_tag: &str) -> Option<Vec<String>> {
    let bytes = header.as_bytes();
    let needle = format!("struct {struct_tag}");
    let mut from = 0;
    while let Some(rel) = header[from..].find(&needle) {
        let start = from + rel;
        let end = start + needle.len();
        from = end;

        // Left word boundary (so `struct z_stream_s` is not matched inside a
        // hypothetical `struct my_z_stream_s`).
        if start > 0 && is_ident_byte(bytes[start - 1]) {
            continue;
        }
        // The tag must be followed (after optional whitespace) by `{` — i.e. this
        // is the definition, not `struct z_stream_s;` or `struct z_stream_s *p;`.
        let mut j = end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'{' {
            continue;
        }
        let body = capture_balanced(bytes, j, b'{', b'}')?;
        return Some(parse_field_names(&body));
    }
    None
}

/// Parse a struct body into its ordered list of field names. Each top-level
/// `;`-separated member contributes the last identifier in its declarator (after
/// stripping any array subscript), which for ordinary and pointer fields is the
/// field name.
fn parse_field_names(body: &str) -> Vec<String> {
    let mut fields = Vec::new();
    for member in split_top_level(body, ';') {
        let declarator = member.trim();
        if declarator.is_empty() {
            continue;
        }
        // Drop any array subscript before extracting the name.
        let head = declarator.split('[').next().unwrap_or(declarator);
        if let Some(name) = last_identifier(head) {
            fields.push(name);
        }
    }
    fields
}

/// Split `text` on `separator`, but only where the separator sits at bracket/
/// paren/brace depth zero. Trailing whitespace-only fragments are dropped.
fn split_top_level(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(ch);
            }
            c if c == separator && depth == 0 => parts.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

/// Return the last maximal C-identifier run in `text` (e.g. the field name in
/// `struct internal_state *state`). Returns `None` if there is no run or the final
/// run begins with a digit (i.e. is a numeric literal, not an identifier).
fn last_identifier(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            best = Some(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        best = Some(current);
    }
    match best {
        Some(ref name) if name.as_bytes().first().is_some_and(u8::is_ascii_digit) => None,
        other => other,
    }
}
