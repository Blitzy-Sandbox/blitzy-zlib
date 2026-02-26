//! minigzip — minimal gzip/gunzip utility.
//!
//! A port of `test/minigzip.c` (593 lines, Copyright 1995‒2026 Jean‑loup
//! Gailly) to idiomatic Rust.  Demonstrates use of the [`zlib_rs::gz`] module
//! for buffered gzip file I/O.
//!
//! minigzip is a minimal implementation of the gzip utility.  This is only an
//! example of using `zlib‑rs` and is not meant to replace the full‑featured
//! `gzip`.  Error checking is limited.  Use minigzip only for testing; use
//! gzip for the real thing.
//!
//! # Usage
//!
//! ```text
//! minigzip [-c] [-d] [-f] [-h] [-r] [-1 to -9] [files...]
//! ```
//!
//! ## Options
//!
//! | Flag | Effect |
//! |------|--------|
//! | `-c` | Write to standard output (copyout mode) |
//! | `-d` | Decompress |
//! | `-f` | Compress with `Z_FILTERED` strategy |
//! | `-h` | Compress with `Z_HUFFMAN_ONLY` strategy |
//! | `-r` | Compress with `Z_RLE` strategy |
//! | `-1`…`-9` | Compression level (1 = fastest, 9 = best) |
//!
//! ## Program‑Name Detection
//!
//! When invoked as `gunzip`, decompression mode is enabled automatically.
//! When invoked as `zcat`, both decompression and copyout modes are enabled.
//!
//! ## Pipe Mode
//!
//! When no file arguments are given the utility reads from standard input and
//! writes to standard output.  In pipe mode, temporary files are used as
//! intermediaries because `GzReader` / `GzWriter` require owned `File`
//! handles and Rust does not expose a safe API for wrapping `stdin` /
//! `stdout` as `std::fs::File`.

use std::env;
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process;

use zlib_rs::gz::{GzReader, GzWriter};

// ─── Constants ──────────────────────────────────────────────────────────────

/// Gzip file extension suffix.
///
/// Matches the C `GZ_SUFFIX` constant from `minigzip.c` line 140.
const GZ_SUFFIX: &str = ".gz";

/// I/O buffer size for compression and decompression operations (16 KiB).
///
/// Matches the C `BUFLEN` constant from `minigzip.c` line 144.
const BUFLEN: usize = 16384;

/// Maximum allowed file‑name length.
///
/// Matches the C `MAX_NAME_LEN` constant from `minigzip.c` line 145.
const MAX_NAME_LEN: usize = 1024;

// ─── Error Handling ─────────────────────────────────────────────────────────

/// Prints an error message to `stderr` and terminates the process with exit
/// code 1.
///
/// Port of the C `error()` function from `minigzip.c` lines 326‒329.
fn error(prog: &str, msg: &str) -> ! {
    eprintln!("{prog}: {msg}");
    process::exit(1);
}

// ─── Mode‑String Construction ───────────────────────────────────────────────

/// Constructs a mode string for [`GzWriter::open_with_mode`].
///
/// The mode string encodes the write mode (`'w'`), binary mode (`'b'`),
/// compression level (digit `'0'`–`'9'`), and optionally the compression
/// strategy character (`'f'` for filtered, `'h'` for Huffman‑only, `'R'`
/// for RLE).
///
/// Replaces the C `outmode` character‑array manipulation from
/// `minigzip.c` line 511.
fn build_mode_string(level: u8, strategy: Option<char>) -> String {
    let mut mode = String::with_capacity(5);
    mode.push('w');
    mode.push('b');
    // Level is guaranteed to be 0..=9 by the CLI parser.
    mode.push(char::from(b'0' + level));
    if let Some(s) = strategy {
        mode.push(s);
    }
    mode
}

// ─── Temporary‑File Helpers ─────────────────────────────────────────────────

/// Generates a temporary‑file path with the given suffix.
///
/// Uses the system temp directory and the current process ID to create a
/// unique filename.  This is used for pipe and copyout mode operations where
/// `stdin` / `stdout` cannot be directly wrapped in a `File` handle without
/// `unsafe` code.
fn temp_path(suffix: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!("minigzip_{}{suffix}", process::id()));
    path
}

/// Converts a [`Path`] reference to `&str`, returning an I/O error if the
/// path contains non‑UTF‑8 characters.
fn path_to_str(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "path contains invalid UTF-8 characters",
        )
    })
}

// ─── Core Compression / Decompression ───────────────────────────────────────

/// Compresses data from an input reader to a gzip writer, then finalises
/// the gzip stream by calling [`GzWriter::close`].
///
/// Reads input in [`BUFLEN`]‑sized chunks and writes compressed gzip output.
///
/// Port of `gz_compress()` from `minigzip.c` lines 369‒392.  The
/// `USE_MMAP` optimisation path from the C version is deliberately not
/// ported—standard buffered I/O is sufficient for this utility.
fn gz_compress(input: &mut impl Read, mut output: GzWriter) -> io::Result<()> {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = input.read(&mut buf)?;
        if len == 0 {
            break;
        }
        output.write_all(&buf[..len])?;
    }
    output.close()
}

/// Decompresses data from a gzip reader to an output writer.
///
/// Reads decompressed data in [`BUFLEN`]‑sized chunks from the gzip reader
/// and writes to the output.  The [`GzReader`] is cleaned up automatically
/// via its `Drop` implementation when it goes out of scope.
///
/// Port of `gz_uncompress()` from `minigzip.c` lines 397‒414.
fn gz_uncompress(mut input: GzReader, output: &mut impl Write) -> io::Result<()> {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = input.read(&mut buf)?;
        if len == 0 {
            break;
        }
        output.write_all(&buf[..len])?;
    }
    Ok(())
}

// ─── File Operations ────────────────────────────────────────────────────────

/// Creates a [`GzWriter`] for the given path and mode string.
///
/// Dispatches between [`GzWriter::open`] (for default compression settings,
/// i.e. mode `"wb6"`) and [`GzWriter::open_with_mode`] (when a custom level
/// or strategy is specified).  `GzWriter::open` uses
/// [`Z_DEFAULT_COMPRESSION`](zlib_rs::constants::Z_DEFAULT_COMPRESSION)
/// which is equivalent to level 6.
fn create_gz_writer(path: &str, mode: &str) -> io::Result<GzWriter> {
    if mode == "wb6" {
        GzWriter::open(path)
    } else {
        GzWriter::open_with_mode(path, mode)
    }
}

/// Compresses a file, creating a `.gz` version and removing the original.
///
/// Constructs the output filename by appending [`GZ_SUFFIX`] to the input
/// filename.  After successful compression the original file is deleted.
///
/// Port of `file_compress()` from `minigzip.c` lines 421‒448.
fn file_compress(prog: &str, file: &str, mode: &str) -> io::Result<()> {
    let outfile = format!("{file}{GZ_SUFFIX}");
    if outfile.len() >= MAX_NAME_LEN {
        error(prog, "filename too long");
    }

    let input_file =
        fs::File::open(file).map_err(|e| io::Error::new(e.kind(), format!("{file}: {e}")))?;
    let mut input = BufReader::new(input_file);

    let output = create_gz_writer(&outfile, mode)
        .map_err(|e| io::Error::new(e.kind(), format!("can't gzopen {outfile}: {e}")))?;

    gz_compress(&mut input, output)?;

    fs::remove_file(file)?;
    Ok(())
}

/// Decompresses a `.gz` file, creating the uncompressed version and removing
/// the compressed original.
///
/// Filename handling (matching C `file_uncompress` behaviour):
/// - If the filename ends with `.gz`: input = filename, output = filename
///   without `.gz`
/// - Otherwise: input = filename + `.gz`, output = filename
///
/// Port of `file_uncompress()` from `minigzip.c` lines 454‒492.
fn file_uncompress(prog: &str, file: &str) -> io::Result<()> {
    let (infile, outfile) = if let Some(out) = file.strip_suffix(GZ_SUFFIX) {
        (file.to_string(), out.to_string())
    } else {
        let inf = format!("{file}{GZ_SUFFIX}");
        (inf, file.to_string())
    };

    if infile.len() >= MAX_NAME_LEN {
        error(prog, "filename too long");
    }

    let input = GzReader::open(&infile)
        .map_err(|e| io::Error::new(e.kind(), format!("can't gzopen {infile}: {e}")))?;

    let output_file = fs::File::create(&outfile)
        .map_err(|e| io::Error::new(e.kind(), format!("{outfile}: {e}")))?;
    let mut output = BufWriter::new(output_file);

    gz_uncompress(input, &mut output)?;
    output.flush()?;

    fs::remove_file(&infile)?;
    Ok(())
}

// ─── Pipe‑Mode Operations ───────────────────────────────────────────────────

/// Compresses data from `stdin` to `stdout` via temporary files.
///
/// Since [`GzWriter`] requires a [`File`](fs::File) handle and
/// `stdin` / `stdout` cannot be safely converted to `File` handles without
/// `unsafe` code, this function uses temporary files as intermediaries:
///
/// 1. Copies `stdin` to a temporary input file.
/// 2. Compresses the temporary file to a temporary `.gz` file.
/// 3. Copies the `.gz` file to `stdout`.
/// 4. Cleans up temporary files.
///
/// This preserves the pipe semantics of the C version's
/// `gzdopen(fileno(stdout), outmode)` pattern.
fn pipe_compress(prog: &str, mode: &str) -> io::Result<()> {
    let tmp_in = temp_path(".in");
    let tmp_gz = temp_path(".gz");

    // Stage 1: Copy stdin to a temporary input file.
    {
        let mut stdin_handle = io::stdin().lock();
        let mut f = fs::File::create(&tmp_in)?;
        io::copy(&mut stdin_handle, &mut f)?;
    }

    // Stage 2: Compress the temporary input to a temporary .gz output.
    // Uses GzWriter::from_file() for default compression settings (when the
    // mode string is "wb6"), otherwise uses GzWriter::open_with_mode() for
    // custom level or strategy settings.
    let compress_result = (|| -> io::Result<()> {
        let mut input = BufReader::new(fs::File::open(&tmp_in)?);
        let output = if mode == "wb6" {
            let f = fs::File::create(&tmp_gz)?;
            GzWriter::from_file(f).map_err(|e| {
                io::Error::new(e.kind(), format!("{prog}: can't create temp gz: {e}"))
            })?
        } else {
            let tmp_gz_str = path_to_str(&tmp_gz)?;
            GzWriter::open_with_mode(tmp_gz_str, mode).map_err(|e| {
                io::Error::new(e.kind(), format!("{prog}: can't create temp gz: {e}"))
            })?
        };
        gz_compress(&mut input, output)
    })();

    // Stage 3: Copy compressed data to stdout.
    if compress_result.is_ok() {
        let mut gz_file = fs::File::open(&tmp_gz)?;
        let mut stdout_handle = io::stdout().lock();
        io::copy(&mut gz_file, &mut stdout_handle)?;
    }

    // Stage 4: Clean up temporary files (ignore removal errors).
    let _ = fs::remove_file(&tmp_in);
    let _ = fs::remove_file(&tmp_gz);

    compress_result
}

/// Decompresses gzip data from `stdin` to `stdout` via a temporary file.
///
/// Since [`GzReader`] requires a [`File`](fs::File) handle and `stdin`
/// cannot be safely converted to a `File` handle without `unsafe` code,
/// this function uses a temporary file as intermediary:
///
/// 1. Copies `stdin` to a temporary `.gz` file.
/// 2. Opens the temporary file with [`GzReader`] and decompresses to
///    `stdout`.
/// 3. Cleans up the temporary file.
///
/// This preserves the pipe semantics of the C version's
/// `gzdopen(fileno(stdin), "rb")` pattern.
fn pipe_decompress(prog: &str) -> io::Result<()> {
    let tmp_gz = temp_path(".gz");

    // Stage 1: Copy stdin to a temporary .gz file.
    {
        let mut stdin_handle = io::stdin().lock();
        let mut f = fs::File::create(&tmp_gz)?;
        io::copy(&mut stdin_handle, &mut f)?;
    }

    // Stage 2: Decompress the temporary .gz file to stdout.
    // Uses GzReader::from_file() (the fd-based open equivalent) since the
    // file was just created by us and we already have the path.
    let decompress_result = (|| -> io::Result<()> {
        let file = fs::File::open(&tmp_gz)
            .map_err(|e| io::Error::new(e.kind(), format!("{prog}: can't gzopen temp: {e}")))?;
        let reader = GzReader::from_file(file)
            .map_err(|e| io::Error::new(e.kind(), format!("{prog}: can't gzopen temp: {e}")))?;
        let mut stdout_handle = io::stdout().lock();
        gz_uncompress(reader, &mut stdout_handle)
    })();

    // Stage 3: Clean up the temporary file (ignore removal errors).
    let _ = fs::remove_file(&tmp_gz);

    decompress_result
}

// ─── Copyout‑Mode Operations ────────────────────────────────────────────────

/// Compresses a file and writes the gzip output to `stdout` via a temporary
/// file.
///
/// Used when the `-c` flag is specified for compression: the compressed
/// output goes to `stdout` instead of a `.gz` file, and the original file
/// is **not** deleted (matching the C `minigzip` behaviour).
fn copyout_compress(prog: &str, path: &str, mode: &str) -> io::Result<()> {
    let input_file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            // Non‑fatal: print error and skip this file (matches C behaviour
            // where copyout compress prints perror and continues to next arg).
            eprintln!("{path}: {e}");
            return Ok(());
        }
    };

    let tmp_gz = temp_path(".gz");
    let compress_result = (|| -> io::Result<()> {
        let tmp_gz_str = path_to_str(&tmp_gz)?;
        let mut input = BufReader::new(input_file);
        let output = GzWriter::open_with_mode(tmp_gz_str, mode)
            .map_err(|e| io::Error::new(e.kind(), format!("{prog}: can't create temp gz: {e}")))?;
        gz_compress(&mut input, output)
    })();

    // Copy compressed data to stdout on success.
    if compress_result.is_ok() {
        let mut gz_file = fs::File::open(&tmp_gz)?;
        let mut stdout_handle = io::stdout().lock();
        io::copy(&mut gz_file, &mut stdout_handle)?;
    }

    // Clean up (ignore removal errors).
    let _ = fs::remove_file(&tmp_gz);

    compress_result
}

/// Decompresses a `.gz` file and writes the output to `stdout`.
///
/// Used when the `-c` flag is specified for decompression: the decompressed
/// output goes to `stdout` instead of a file, and the `.gz` file is **not**
/// deleted (matching the C `minigzip` behaviour).
fn copyout_decompress(prog: &str, path: &str) -> io::Result<()> {
    let Ok(reader) = GzReader::open(path) else {
        // Non‑fatal: print error and skip this file (matches C behaviour
        // where copyout decompress prints fprintf and continues).
        eprintln!("{prog}: can't gzopen {path}");
        return Ok(());
    };

    let mut stdout_handle = io::stdout().lock();
    gz_uncompress(reader, &mut stdout_handle)
}

// ─── Main Entry Point ───────────────────────────────────────────────────────

/// Entry point for the minigzip utility.
///
/// Parses command‑line arguments and dispatches to the appropriate
/// compression or decompression function.
///
/// Port of `main()` from `minigzip.c` lines 505‒592.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let prog = args.first().map_or("minigzip", String::as_str);

    // ── Detect gunzip / zcat via program‑name basename ──
    //
    // Port of minigzip.c lines 513‒523.
    let basename = Path::new(prog)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("minigzip");

    let mut copyout = false;
    let mut uncompr = false;
    let mut level: u8 = 6;
    let mut strategy: Option<char> = None;

    if basename == "gunzip" {
        uncompr = true;
    } else if basename == "zcat" {
        copyout = true;
        uncompr = true;
    }

    // ── Parse command‑line flags ──
    //
    // Flags are consumed left‑to‑right; the loop stops at the first
    // non‑flag argument and all remaining arguments are treated as file
    // paths.  This mirrors the C `while (argc > 0) { ... else break; }`
    // pattern from minigzip.c lines 525‒542.
    let mut idx: usize = 1;
    while idx < args.len() {
        let arg = &args[idx];
        let bytes = arg.as_bytes();
        match arg.as_str() {
            "-c" => copyout = true,
            "-d" => uncompr = true,
            "-f" => strategy = Some('f'),
            "-h" => strategy = Some('h'),
            "-r" => strategy = Some('R'),
            _ if bytes.len() == 2 && bytes[0] == b'-' && bytes[1] >= b'1' && bytes[1] <= b'9' => {
                level = bytes[1] - b'0';
            }
            _ => break,
        }
        idx += 1;
    }

    // Build the mode string for GzWriter (e.g. "wb6", "wb9f", "wb1h").
    let mode = build_mode_string(level, strategy);

    // Remaining arguments are file names.
    let file_args: Vec<&str> = args[idx..].iter().map(String::as_str).collect();

    if file_args.is_empty() {
        // ── Pipe mode: stdin → stdout ──
        //
        // Port of minigzip.c lines 545‒556.
        if uncompr {
            pipe_decompress(prog)?;
        } else {
            pipe_compress(prog, &mode)?;
        }
    } else {
        // ── File mode ──
        //
        // Port of minigzip.c lines 557‒589.
        for file in &file_args {
            if uncompr {
                if copyout {
                    copyout_decompress(prog, file)?;
                } else {
                    file_uncompress(prog, file)?;
                }
            } else if copyout {
                copyout_compress(prog, file, &mode)?;
            } else {
                file_compress(prog, file, &mode)?;
            }
        }
    }

    Ok(())
}
