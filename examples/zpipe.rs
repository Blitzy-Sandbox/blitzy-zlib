//! zpipe — streaming deflate/inflate pipe compression example.
//!
//! Port of `zpipe.c` (Version 1.5, 11 February 2026, Mark Adler) to
//! idiomatic safe Rust. Demonstrates proper use of the `zlib-rs` crate's
//! streaming compression and decompression APIs.
//!
//! # Usage
//!
//! ```text
//! zpipe [-d] < source > dest
//! ```
//!
//! Without arguments: compresses stdin to stdout using the zlib format.
//! With `-d`: decompresses stdin to stdout.
//!
//! # Examples
//!
//! ```text
//! # Compress a file
//! zpipe < input.txt > compressed.zz
//!
//! # Decompress
//! zpipe -d < compressed.zz > output.txt
//! ```
//!
//! # Original License
//!
//! Not copyrighted — provided to the public domain.

use std::env;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process;

use zlib_rs::constants::{Z_DEFAULT_COMPRESSION, Z_FINISH, Z_NO_FLUSH};
use zlib_rs::error::ReturnCode;
use zlib_rs::stream::ZStream;
use zlib_rs::{deflate, inflate};

/// Size of the I/O buffer chunks, matching the original C version.
///
/// This is the same value (`16384`) used by `zpipe.c` for both input and
/// output buffers. It provides a good balance between memory usage and
/// throughput: large enough to amortize per-call overhead, small enough
/// to avoid excessive memory consumption.
const CHUNK: usize = 16384;

/// Compress from reader to writer using the streaming deflate API.
///
/// Port of the C `def()` function from `zpipe.c` lines 37–85.
///
/// Reads input in [`CHUNK`]-sized blocks, compresses them through the deflate
/// engine, and writes the compressed output. Uses [`Z_DEFAULT_COMPRESSION`]
/// (level 6) for the compression level.
///
/// The function follows the same streaming pattern as the original C code:
/// 1. Read a chunk of input from the reader.
/// 2. Feed the chunk to the deflate engine.
/// 3. Drain all compressed output from the engine into the writer.
/// 4. Repeat until EOF, then finish with [`Z_FINISH`].
///
/// Resource cleanup is automatic: the [`ZStream`] and its internal
/// [`DeflateState`](zlib_rs::deflate::DeflateState) are freed via Rust's
/// ownership model when they go out of scope. The explicit
/// [`deflate_end`](zlib_rs::deflate::deflate_end) call mirrors the C code
/// and documents the lifecycle boundary.
///
/// # Errors
///
/// Returns an error if:
/// - The deflate engine cannot be initialized (e.g., out of memory).
/// - An I/O error occurs while reading input or writing output.
/// - The deflate engine enters an invalid state ([`ReturnCode::StreamError`]).
fn compress_stream(
    mut reader: impl Read,
    mut writer: impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = ZStream::new();

    // Initialize the deflate engine with default compression level (6).
    // C equivalent: ret = deflateInit(&strm, level);
    let ret = deflate::deflate_init(&mut stream, Z_DEFAULT_COMPRESSION);
    if ret != ReturnCode::Ok {
        return Err(Box::new(ret));
    }

    let mut in_buf = [0u8; CHUNK];

    // Compress until end of file.
    // C equivalent: do { ... } while (flush != Z_FINISH);
    loop {
        // Read a chunk of input.
        // C equivalent: strm.avail_in = fread(in, 1, CHUNK, source);
        let n = reader.read(&mut in_buf)?;

        // Determine the flush mode: Z_FINISH on EOF, Z_NO_FLUSH otherwise.
        // C equivalent: flush = feof(source) ? Z_FINISH : Z_NO_FLUSH;
        let flush = if n == 0 { Z_FINISH } else { Z_NO_FLUSH };
        stream.set_input(&in_buf[..n]);

        // Run deflate() on input until the output buffer is not full, finishing
        // compression if all of source has been read in.
        // C equivalent: do { ... } while (strm.avail_out == 0);
        loop {
            stream.set_output_buffer(CHUNK);
            let ret = deflate::deflate(&mut stream, flush);

            // C equivalent: assert(ret != Z_STREAM_ERROR); — state not clobbered.
            // In Rust, we return an error instead of asserting.
            if ret == ReturnCode::StreamError {
                return Err(Box::new(ReturnCode::StreamError));
            }

            // Write compressed output to the writer.
            // C equivalent: fwrite(out, 1, have, dest)
            let written = stream.output_written();
            if !written.is_empty() {
                writer.write_all(written)?;
            }

            // If the output buffer is not full, all compressed output for this
            // input chunk has been produced.
            if stream.avail_out() != 0 {
                break;
            }
        }

        // C equivalent: assert(strm.avail_in == 0); — all input will be used.
        debug_assert_eq!(
            stream.avail_in(),
            0,
            "all input should be consumed after deflate"
        );

        // Done when last data in file processed.
        // C equivalent: } while (flush != Z_FINISH);
        if flush == Z_FINISH {
            break;
        }
    }

    // Clean up deflate state and flush the output writer.
    // C equivalent: (void)deflateEnd(&strm);
    let _ = deflate::deflate_end(&mut stream);
    writer.flush()?;
    Ok(())
}

/// Decompress from reader to writer using the streaming inflate API.
///
/// Port of the C `inf()` function from `zpipe.c` lines 93–149.
///
/// Reads compressed input in [`CHUNK`]-sized blocks, decompresses them
/// through the inflate engine, and writes the decompressed output.
///
/// The function follows the same streaming pattern as the original C code:
/// 1. Read a chunk of compressed input from the reader.
/// 2. Feed the chunk to the inflate engine.
/// 3. Drain all decompressed output from the engine into the writer.
/// 4. Repeat until [`ReturnCode::StreamEnd`] or EOF.
///
/// Error handling matches the C code: [`ReturnCode::NeedDict`] is treated as
/// a data error (the example does not support preset dictionaries).
///
/// # Errors
///
/// Returns an error if:
/// - The inflate engine cannot be initialized (e.g., out of memory).
/// - An I/O error occurs while reading input or writing output.
/// - The compressed data is invalid or incomplete ([`ReturnCode::DataError`]).
/// - A preset dictionary is required ([`ReturnCode::NeedDict`] → treated as
///   [`ReturnCode::DataError`]).
/// - The inflate engine runs out of memory ([`ReturnCode::MemError`]).
fn decompress_stream(
    mut reader: impl Read,
    mut writer: impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = ZStream::new();

    // Initialize the inflate engine.
    // C equivalent: ret = inflateInit(&strm);
    let ret = inflate::inflate_init(&mut stream);
    if ret != ReturnCode::Ok {
        return Err(Box::new(ret));
    }

    let mut in_buf = [0u8; CHUNK];
    let mut ret = ReturnCode::Ok;

    // Decompress until deflate stream ends or end of file.
    // C equivalent: do { ... } while (ret != Z_STREAM_END);
    loop {
        // Read a chunk of compressed input.
        // C equivalent: strm.avail_in = fread(in, 1, CHUNK, source);
        let n = reader.read(&mut in_buf)?;

        // C equivalent: if (strm.avail_in == 0) break;
        if n == 0 {
            break;
        }
        stream.set_input(&in_buf[..n]);

        // Run inflate() on input until the output buffer is not full.
        // C equivalent: do { ... } while (strm.avail_out == 0);
        loop {
            stream.set_output_buffer(CHUNK);

            // Use inflate_run which handles state extraction internally.
            // C equivalent: ret = inflate(&strm, Z_NO_FLUSH);
            ret = inflate::inflate_run(&mut stream, Z_NO_FLUSH);

            // C equivalent: assert(ret != Z_STREAM_ERROR); — state not clobbered.
            // C equivalent: switch(ret) { case Z_NEED_DICT: ... case Z_DATA_ERROR:
            //               case Z_MEM_ERROR: (void)inflateEnd(&strm); return ret; }
            match ret {
                ReturnCode::NeedDict => {
                    // C equivalent: ret = Z_DATA_ERROR; /* and fall through */
                    let _ = inflate::inflate_end_stream(&mut stream);
                    return Err(Box::new(ReturnCode::DataError));
                }
                ReturnCode::DataError | ReturnCode::MemError | ReturnCode::StreamError => {
                    let _ = inflate::inflate_end_stream(&mut stream);
                    return Err(Box::new(ret));
                }
                _ => {} // Ok or StreamEnd — continue processing output
            }

            // Write decompressed output to the writer.
            // C equivalent: have = CHUNK - strm.avail_out;
            //               fwrite(out, 1, have, dest);
            let written = stream.output_written();
            if !written.is_empty() {
                writer.write_all(written)?;
            }

            // If the output buffer is not full, all decompressed output for
            // this input chunk has been produced.
            if stream.avail_out() != 0 {
                break;
            }
        }

        // Done when inflate() says it's done.
        // C equivalent: } while (ret != Z_STREAM_END);
        if ret == ReturnCode::StreamEnd {
            break;
        }
    }

    // Clean up inflate state and flush the output writer.
    // C equivalent: (void)inflateEnd(&strm);
    let _ = inflate::inflate_end_stream(&mut stream);
    writer.flush()?;

    // C equivalent: return ret == Z_STREAM_END ? Z_OK : Z_DATA_ERROR;
    if ret == ReturnCode::StreamEnd {
        Ok(())
    } else {
        Err(Box::new(ReturnCode::DataError))
    }
}

/// Report a zlib or I/O error to stderr.
///
/// Port of the C `zerr()` function from `zpipe.c` lines 152–174.
///
/// Prints a descriptive error message to stderr based on the [`ReturnCode`]
/// variant. For I/O errors (not represented as `ReturnCode`), the caller
/// handles output directly.
fn report_error(code: &ReturnCode) {
    eprint!("zpipe: ");
    match *code {
        ReturnCode::Errno => {
            eprintln!("error reading stdin or writing stdout");
        }
        ReturnCode::StreamError => {
            eprintln!("invalid compression level");
        }
        ReturnCode::DataError => {
            eprintln!("invalid or incomplete deflate data");
        }
        ReturnCode::MemError => {
            eprintln!("out of memory");
        }
        ReturnCode::VersionError => {
            eprintln!("zlib version mismatch!");
        }
        _ => {
            eprintln!("unknown error ({})", code);
        }
    }
}

/// Compress or decompress from stdin to stdout.
///
/// Port of the C `main()` function from `zpipe.c` lines 177–206.
///
/// Dispatches between compression (no arguments) and decompression (`-d`
/// flag). On error, prints a diagnostic message to stderr and exits with
/// a non-zero status code.
///
/// # Exit Codes
///
/// - `0` — Operation completed successfully.
/// - `1` — An error occurred (invalid usage, I/O error, or zlib error).
fn main() {
    let args: Vec<String> = env::args().collect();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = BufReader::new(stdin.lock());
    let writer = BufWriter::new(stdout.lock());

    // Do compression if no arguments.
    // C equivalent: if (argc == 1) { ret = def(stdin, stdout, Z_DEFAULT_COMPRESSION); }
    let result = if args.len() == 1 {
        compress_stream(reader, writer)
    }
    // Do decompression if -d specified.
    // C equivalent: else if (argc == 2 && strcmp(argv[1], "-d") == 0) { ret = inf(stdin, stdout); }
    else if args.len() == 2 && args[1] == "-d" {
        decompress_stream(reader, writer)
    }
    // Otherwise, report usage.
    // C equivalent: fputs("zpipe usage: zpipe [-d] < source > dest\n", stderr); return 1;
    else {
        eprintln!("zpipe usage: zpipe [-d] < source > dest");
        process::exit(1);
    };

    // Handle errors: print diagnostics and exit with non-zero status.
    // C equivalent: if (ret != Z_OK) zerr(ret); return ret;
    if let Err(e) = result {
        if let Some(code) = e.downcast_ref::<ReturnCode>() {
            report_error(code);
        } else {
            // I/O error or other non-zlib error.
            eprintln!("zpipe: {e}");
        }
        process::exit(1);
    }
}
