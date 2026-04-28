// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! File handle storage for PostScript file I/O.
//!
//! Files are identified by `EntityId` indices. Stdin (0), stdout (1), and
//! stderr (2) are pre-allocated at well-known positions.
//!
//! Phase 5 adds filter support: `FileHandle::Filter` wraps a `FilterState`
//! that decodes data from an underlying source file, and `FileHandle::StringSource`
//! provides a read-only byte stream from in-memory data.

use std::collections::HashMap;
use std::io::{self, BufReader, IsTerminal, Read, Seek, Write};

use crate::object::EntityId;

/// State for a RunLengthDecode filter.
#[derive(Debug)]
pub enum RleState {
    /// Waiting for a length byte.
    Init,
    /// Copying `remaining` literal bytes.
    Literal { remaining: u16 },
    /// Repeating `byte` for `remaining` times.
    Repeat { byte: u8, remaining: u16 },
    /// End of data (saw 128).
    Eod,
}

/// Which filter to apply.
pub enum FilterKind {
    /// Hex digit pairs → bytes, EOD = `>`.
    ASCIIHexDecode,
    /// Base-85 groups → bytes, EOD = `~>`.
    ASCII85Decode {
        group: Vec<u8>,
    },
    RunLengthDecode {
        state: RleState,
    },
    FlateDecode {
        decompressor: flate2::Decompress,
        raw_buf: Vec<u8>,
        predictor: u8,
        columns: u32,
        colors: u32,
        bpc: u32,
        prev_row: Vec<u8>,
    },
    LZWDecode {
        decoder: weezl::decode::Decoder,
        raw_buf: Vec<u8>,
    },
    /// JPEG: lazily decoded on first read from source.
    DCTDecode {
        decoded: bool,
        color_transform: Option<bool>,
    },
    /// JBIG2 (bi-level raster): buffered, lazily decoded on first read.
    /// Output is row-packed 1-bit-per-pixel (8 pixels/byte, MSB first), with
    /// 0=black and 1=white per PDF DeviceGray convention.
    JBIG2Decode {
        decoded: bool,
        /// Optional /JBIG2Globals stream contents (shared globals segment
        /// referenced by the embedded image data). Decoded once and used
        /// as the `globals` argument to `hayro_jbig2::decode_embedded`.
        globals: Option<Vec<u8>>,
    },
    /// JPEG 2000 (JP2 / J2K): buffered, lazily decoded on first read.
    /// Output is interleaved pixel data; the caller declares the colour
    /// space via `setcolorspace` (the JPX-internal CS is informational).
    JPXDecode {
        decoded: bool,
    },
    SubFileDecode {
        eod_string: Vec<u8>,
        eod_count: i32,
        bytes_remaining: Option<i64>,
    },
    /// CCITT Group 3/4 fax decode (lazily decoded on first read).
    CCITTFaxDecode {
        /// Whether decoding has been performed yet.
        decoded: bool,
        /// K parameter: <0 = Group 4, =0 = Group 3 1-D, >0 = Group 3 2-D.
        k: i32,
        /// Image width in pixels.
        columns: u32,
        /// Image height (0 = unknown).
        rows: u32,
        /// Whether to expect EOL patterns.
        end_of_line: bool,
        /// Whether encoded lines are byte-aligned.
        encoded_byte_align: bool,
        /// Whether EOFB/RTC terminates data.
        end_of_block: bool,
        /// Pixel polarity: false = 0 is black (PS default).
        black_is1: bool,
    },
    /// eexec decryption filter (Type 1 font encryption).
    EexecDecode {
        /// Current cipher state (initial: 55665).
        r: u16,
        /// None = not yet detected, Some(true) = hex, Some(false) = binary.
        is_hex: Option<bool>,
        /// Number of plaintext bytes produced so far (first 4 are random, skip them).
        skip_count: u32,
        /// Leftover hex digit from previous refill (hex mode only).
        hex_leftover: Option<u8>,
    },
    // -- Encode filters (write direction) --
    /// Bytes → hex digit pairs, EOD = `>`.
    ASCIIHexEncode,
    /// Bytes → base-85 groups, EOD = `~>`.
    ASCII85Encode {
        /// Accumulates up to 4 bytes before encoding a group.
        buf: Vec<u8>,
        /// Column counter for line breaking (~80 chars).
        col: usize,
    },
    /// Bytes → run-length encoded data, EOD = byte 128.
    /// Encodes incrementally during writes (streaming).
    RunLengthEncode {
        /// Pending literal bytes not yet emitted (max 128).
        pending: Vec<u8>,
        /// Current repeat byte being tracked.
        run_byte: Option<u8>,
        /// Count of current repeat run.
        run_count: usize,
    },
    /// Bytes → zlib-compressed data (with optional predictor pre-processing).
    FlateEncode {
        compressor: flate2::Compress,
        predictor: u8,
        columns: u32,
        colors: u32,
        bpc: u32,
        /// Row width in bytes (columns * colors * bpc / 8).
        row_width: usize,
        /// Bytes per pixel for PNG Sub filter.
        bpp: usize,
        /// Buffer for accumulating input until a full row is available.
        encode_buf: Vec<u8>,
        /// Previous row for PNG predictor (unused for TIFF predictor 2).
        prev_row: Vec<u8>,
    },
    /// Bytes → LZW-compressed data.
    LZWEncode {
        encoder: weezl::encode::Encoder,
    },
    /// Identity encode filter (pass-through).
    NullEncode,
    /// JPEG encode: buffers all input, encodes on close.
    DCTEncode {
        buf: Vec<u8>,
        columns: u32,
        rows: u32,
        colors: u32,
        quality: u8,
        color_transform: bool,
    },
}

/// Decoded-data buffer state for a filter file.
pub struct FilterState {
    pub kind: FilterKind,
    pub source: EntityId,
    pub output_buf: Vec<u8>,
    pub output_pos: usize,
    pub putback: Vec<u8>,
    pub eof: bool,
    /// Total bytes consumed by the caller (for fileposition).
    pub bytes_read: u64,
}

/// The underlying handle for a file.
pub enum FileHandle {
    /// Real file on disk (buffered for efficient byte-at-a-time reads).
    Real(BufReader<std::fs::File>),
    /// Standard input (uses stdin).
    Stdin,
    /// Standard output (uses stdout).
    Stdout,
    /// Standard error (uses stderr).
    Stderr,
    /// File has been closed.
    Closed,
    /// Decode/encode filter wrapping another file.
    Filter(Box<FilterState>),
    /// In-memory byte source (for string-backed data).
    StringSource { data: Vec<u8>, pos: usize },
}

/// Encode a byte slice using PostScript RLE format.
///
/// Emit a literal run (prefix = len-1, then the bytes).
fn rle_emit_literals(pending: &[u8]) -> Vec<u8> {
    if pending.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(pending.len() + 1);
    out.push((pending.len() - 1) as u8);
    out.extend_from_slice(pending);
    out
}

/// Emit a repeat run (prefix = 257-count, then the byte).
fn rle_emit_repeat(byte: u8, count: usize) -> [u8; 2] {
    [(257 - count) as u8, byte]
}

/// Metadata and handle for one open file.
pub struct FileEntry {
    pub handle: FileHandle,
    pub name: String,
    pub mode: String,
    /// Current line number (1-based), incremented as newlines are consumed.
    pub line_num: u32,
    /// Newlines consumed but not yet applied to `line_num`. Flushed at the
    /// start of the next token read so that `line` reports the line the
    /// current token is on, not the line after it.
    pub pending_newlines: u32,
}

/// Storage for all open PostScript files.
pub struct FileStore {
    files: Vec<FileEntry>,
    /// Virtual filesystem: maps paths to embedded byte data.
    /// Used in WASM builds to serve resources without real filesystem access.
    embedded_files: HashMap<String, &'static [u8]>,
}

/// Well-known file entity IDs.
pub const FILE_STDIN: EntityId = EntityId(0);
pub const FILE_STDOUT: EntityId = EntityId(1);
pub const FILE_STDERR: EntityId = EntityId(2);

impl FileStore {
    /// Create a new FileStore with stdin/stdout/stderr pre-allocated.
    pub fn new() -> Self {
        let mut store = Self {
            files: Vec::new(),
            embedded_files: HashMap::new(),
        };
        // Pre-allocate standard streams at known positions
        store.files.push(FileEntry {
            handle: FileHandle::Stdin,
            name: "%stdin".to_string(),
            mode: "r".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        store.files.push(FileEntry {
            handle: FileHandle::Stdout,
            name: "%stdout".to_string(),
            mode: "w".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        store.files.push(FileEntry {
            handle: FileHandle::Stderr,
            name: "%stderr".to_string(),
            mode: "w".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        store
    }

    /// Register an embedded file mapping (path → static byte data).
    pub fn add_embedded_file(&mut self, path: &str, data: &'static [u8]) {
        self.embedded_files.insert(path.to_string(), data);
    }

    /// Look up an embedded file by path. Returns the data if found.
    ///
    /// Normalizes the path by stripping leading `/` and collapsing `//` to `/`
    /// to handle paths like `/resources/Font//Helvetica.t1` built by PS code.
    pub fn get_embedded_file(&self, path: &str) -> Option<&'static [u8]> {
        if let Some(data) = self.embedded_files.get(path) {
            return Some(*data);
        }
        // Normalize: strip leading "/" and collapse "//" → "/"
        let normalized = path.trim_start_matches('/').replace("//", "/");
        if normalized != path {
            return self.embedded_files.get(normalized.as_str()).copied();
        }
        None
    }

    /// Open a file, returning its EntityId.
    ///
    /// For read mode, checks the embedded file map first (for WASM builds).
    pub fn open(&mut self, name: &str, mode: &str) -> io::Result<EntityId> {
        // Handle special names
        match name {
            "%stdin" => {
                if mode != "r" {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "%stdin is read-only",
                    ));
                }
                return Ok(FILE_STDIN);
            }
            "%stdout" => {
                if mode != "w" {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "%stdout is write-only",
                    ));
                }
                return Ok(FILE_STDOUT);
            }
            "%stderr" => {
                if mode != "w" {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "%stderr is write-only",
                    ));
                }
                return Ok(FILE_STDERR);
            }
            "%lineedit" | "%statementedit" => {
                if mode != "r" {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "read-only special file",
                    ));
                }
                // Read one line from stdin, strip trailing newline
                let mut line = String::new();
                let n = io::stdin().read_line(&mut line)?;
                if n == 0 {
                    // EOF — signal undefinedfilename (executive catches this and exits)
                    return Err(io::Error::new(io::ErrorKind::NotFound, "EOF on stdin"));
                }
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                return Ok(self.create_string_source(line.into_bytes()));
            }
            _ => {}
        }

        // Check embedded files for read access
        if mode == "r"
            && let Some(data) = self.embedded_files.get(name)
        {
            let id = EntityId(self.files.len() as u32);
            self.files.push(FileEntry {
                handle: FileHandle::StringSource {
                    data: data.to_vec(),
                    pos: 0,
                },
                name: name.to_string(),
                mode: mode.to_string(),
                line_num: 1,
                pending_newlines: 0,
            });
            return Ok(id);
        }

        let file = match mode {
            "r" => std::fs::File::open(name)?,
            "w" => std::fs::File::create(name)?,
            "a" => std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(name)?,
            "r+" => std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(name)?,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid mode")),
        };

        let id = EntityId(self.files.len() as u32);
        self.files.push(FileEntry {
            handle: FileHandle::Real(BufReader::new(file)),
            name: name.to_string(),
            mode: mode.to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        Ok(id)
    }

    /// Create a filter file that reads from `source` through `kind`.
    pub fn create_filter(&mut self, source: EntityId, kind: FilterKind) -> EntityId {
        let id = EntityId(self.files.len() as u32);
        self.files.push(FileEntry {
            handle: FileHandle::Filter(Box::new(FilterState {
                kind,
                source,
                output_buf: Vec::new(),
                output_pos: 0,
                putback: Vec::new(),
                eof: false,
                bytes_read: 0,
            })),
            name: "%filter".to_string(),
            mode: "r".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        id
    }

    /// Create a filter file with pre-filled output buffer (for DCTDecode).
    pub fn create_filter_with_data(
        &mut self,
        source: EntityId,
        kind: FilterKind,
        data: Vec<u8>,
    ) -> EntityId {
        let id = EntityId(self.files.len() as u32);
        self.files.push(FileEntry {
            handle: FileHandle::Filter(Box::new(FilterState {
                kind,
                source,
                output_buf: data,
                output_pos: 0,
                putback: Vec::new(),
                eof: false,
                bytes_read: 0,
            })),
            name: "%filter".to_string(),
            mode: "r".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        id
    }

    /// Create an encode filter that writes encoded data to `target`.
    pub fn create_encode_filter(&mut self, target: EntityId, kind: FilterKind) -> EntityId {
        let id = EntityId(self.files.len() as u32);
        self.files.push(FileEntry {
            handle: FileHandle::Filter(Box::new(FilterState {
                kind,
                source: target, // "source" is the write target for encode filters
                output_buf: Vec::new(),
                output_pos: 0,
                putback: Vec::new(),
                eof: false,
                bytes_read: 0,
            })),
            name: "%filter".to_string(),
            mode: "w".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        id
    }

    /// Create a string-backed data source.
    pub fn create_string_source(&mut self, data: Vec<u8>) -> EntityId {
        let id = EntityId(self.files.len() as u32);
        self.files.push(FileEntry {
            handle: FileHandle::StringSource { data, pos: 0 },
            name: "%stringsource".to_string(),
            mode: "r".to_string(),
            line_num: 1,
            pending_newlines: 0,
        });
        id
    }

    /// Get remaining unread bytes from a StringSource file as a slice.
    ///
    /// Returns a borrowed slice of the remaining bytes (from `pos` to end).
    /// For non-StringSource files, returns an empty slice.
    pub fn get_remaining_bytes(&self, entity: EntityId) -> &[u8] {
        let entry = &self.files[entity.0 as usize];
        match &entry.handle {
            FileHandle::StringSource { data, pos } => &data[*pos..],
            _ => &[],
        }
    }

    /// Advance the read position of a StringSource file by `n` bytes.
    pub fn advance_position(&mut self, entity: EntityId, n: usize) {
        let entry = &mut self.files[entity.0 as usize];
        if let FileHandle::StringSource { pos, .. } = &mut entry.handle {
            *pos += n;
        }
    }

    /// Get the current line number (1-based) for a file.
    pub fn line_num(&self, entity: EntityId) -> u32 {
        self.files[entity.0 as usize].line_num
    }

    /// Record newlines as pending. They will be applied to `line_num` at the
    /// start of the next token read via `flush_pending_newlines`.
    pub fn add_pending_newlines(&mut self, entity: EntityId, count: u32) {
        self.files[entity.0 as usize].pending_newlines += count;
    }

    /// Apply any pending newlines to `line_num`. Call this at the start of
    /// each token read so the line number reflects the current token's line.
    pub fn flush_pending_newlines(&mut self, entity: EntityId) {
        let entry = &mut self.files[entity.0 as usize];
        entry.line_num += entry.pending_newlines;
        entry.pending_newlines = 0;
    }

    /// Close a file.
    pub fn close(&mut self, entity: EntityId) -> io::Result<()> {
        // Check if this is an encode filter that needs finalization
        let is_encode = matches!(
            &self.files[entity.0 as usize].handle,
            FileHandle::Filter(state) if state.kind.is_encode()
        );
        if is_encode {
            return self.close_encode_filter(entity);
        }

        let entry = &mut self.files[entity.0 as usize];
        match entry.handle {
            FileHandle::Stdin | FileHandle::Stdout | FileHandle::Stderr => Ok(()),
            FileHandle::Closed => Ok(()),
            FileHandle::Filter(_) => {
                // Close the filter but NOT its underlying source.
                entry.handle = FileHandle::Closed;
                Ok(())
            }
            _ => {
                entry.handle = FileHandle::Closed;
                Ok(())
            }
        }
    }

    /// Read one byte from a file. Returns None on EOF.
    pub fn read_byte(&mut self, entity: EntityId) -> io::Result<Option<u8>> {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => {
                let mut buf = [0u8; 1];
                let n = f.read(&mut buf)?;
                if n == 0 { Ok(None) } else { Ok(Some(buf[0])) }
            }
            FileHandle::Stdin => {
                if std::io::stdin().is_terminal() {
                    Ok(None) // EOF when stdin is a terminal
                } else {
                    let mut buf = [0u8; 1];
                    let n = io::stdin().read(&mut buf)?;
                    if n == 0 { Ok(None) } else { Ok(Some(buf[0])) }
                }
            }
            FileHandle::StringSource { data, pos } => {
                if *pos < data.len() {
                    let b = data[*pos];
                    *pos += 1;
                    Ok(Some(b))
                } else {
                    Ok(None)
                }
            }
            FileHandle::Filter(_) => {
                // Take the filter state out to avoid aliasing issues
                self.read_byte_filter(entity)
            }
            FileHandle::Closed => Ok(None), // Closed files return EOF
            _ => Err(io::Error::other("not readable")),
        }
    }

    /// Read a byte from a filter file (handles temporary swap to avoid &mut aliasing).
    fn read_byte_filter(&mut self, entity: EntityId) -> io::Result<Option<u8>> {
        let entry = &mut self.files[entity.0 as usize];
        let mut state = match std::mem::replace(&mut entry.handle, FileHandle::Closed) {
            FileHandle::Filter(s) => s,
            other => {
                entry.handle = other;
                return Err(io::Error::other("not a filter"));
            }
        };

        // 1. Return from putback buffer if non-empty
        if let Some(b) = state.putback.pop() {
            state.bytes_read += 1;
            self.files[entity.0 as usize].handle = FileHandle::Filter(state);
            return Ok(Some(b));
        }

        // 2. Return from output_buf if data available
        if state.output_pos < state.output_buf.len() {
            let b = state.output_buf[state.output_pos];
            state.output_pos += 1;
            state.bytes_read += 1;
            self.files[entity.0 as usize].handle = FileHandle::Filter(state);
            return Ok(Some(b));
        }

        // 3. If already at EOF, done
        if state.eof {
            self.files[entity.0 as usize].handle = FileHandle::Filter(state);
            return Ok(None);
        }

        // 4. Refill from source
        self.refill_filter(&mut state)?;

        let result = if state.output_pos < state.output_buf.len() {
            let b = state.output_buf[state.output_pos];
            state.output_pos += 1;
            state.bytes_read += 1;
            Ok(Some(b))
        } else {
            Ok(None)
        };

        self.files[entity.0 as usize].handle = FileHandle::Filter(state);
        result
    }

    /// Read into a buffer. Returns number of bytes actually read.
    pub fn read_into(&mut self, entity: EntityId, buf: &mut [u8]) -> io::Result<usize> {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => f.read(buf),
            FileHandle::Stdin => {
                // Return EOF immediately when stdin is a terminal (not a pipe)
                // to avoid blocking the interpreter during file execution.
                if std::io::stdin().is_terminal() {
                    Ok(0)
                } else {
                    io::stdin().read(buf)
                }
            }
            FileHandle::StringSource { data, pos } => {
                let remaining = data.len() - *pos;
                let n = buf.len().min(remaining);
                buf[..n].copy_from_slice(&data[*pos..*pos + n]);
                *pos += n;
                Ok(n)
            }
            FileHandle::Filter(_) => {
                // Read byte-by-byte through the filter
                let mut count = 0;
                for slot in buf.iter_mut() {
                    match self.read_byte(entity)? {
                        Some(b) => {
                            *slot = b;
                            count += 1;
                        }
                        None => break,
                    }
                }
                Ok(count)
            }
            FileHandle::Closed => Err(io::Error::other("file closed")),
            _ => Err(io::Error::other("not readable")),
        }
    }

    /// Write one byte to a file.
    pub fn write_byte(&mut self, entity: EntityId, byte: u8) -> io::Result<()> {
        self.write_from(entity, &[byte])
    }

    /// Write bytes from a buffer.
    pub fn write_from(&mut self, entity: EntityId, buf: &[u8]) -> io::Result<()> {
        // Check if this is an encode filter (needs swap-out pattern)
        let is_encode = matches!(
            &self.files[entity.0 as usize].handle,
            FileHandle::Filter(state) if state.kind.is_encode()
        );
        if is_encode {
            return self.encode_write(entity, buf);
        }
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => f.get_mut().write_all(buf),
            FileHandle::Stdout => io::stdout().write_all(buf),
            FileHandle::Stderr => io::stderr().write_all(buf),
            FileHandle::Closed => Err(io::Error::other("file closed")),
            _ => Err(io::Error::other("not writable")),
        }
    }

    /// Write data through an encode filter using swap-out pattern.
    fn encode_write(&mut self, entity: EntityId, data: &[u8]) -> io::Result<()> {
        // Swap out filter state to avoid borrow conflicts
        let entry = &mut self.files[entity.0 as usize];
        let mut state = match std::mem::replace(&mut entry.handle, FileHandle::Closed) {
            FileHandle::Filter(s) => s,
            other => {
                entry.handle = other;
                return Err(io::Error::other("not an encode filter"));
            }
        };

        let target = state.source;
        let result = match &mut state.kind {
            FilterKind::ASCIIHexEncode => {
                // Each byte → 2 uppercase hex chars
                let mut hex = Vec::with_capacity(data.len() * 2);
                for &b in data {
                    hex.push(b"0123456789ABCDEF"[(b >> 4) as usize]);
                    hex.push(b"0123456789ABCDEF"[(b & 0xF) as usize]);
                }
                self.write_from(target, &hex)
            }
            FilterKind::ASCII85Encode { buf, col } => {
                let mut encoded = Vec::new();
                for &byte in data {
                    buf.push(byte);
                    if buf.len() == 4 {
                        let val = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        if val == 0 {
                            encoded.push(b'z');
                            *col += 1;
                        } else {
                            let mut chars = [0u8; 5];
                            let mut v = val;
                            for c in chars.iter_mut().rev() {
                                *c = (v % 85) as u8 + b'!';
                                v /= 85;
                            }
                            encoded.extend_from_slice(&chars);
                            *col += 5;
                        }
                        buf.clear();
                        if *col >= 75 {
                            encoded.push(b'\n');
                            *col = 0;
                        }
                    }
                }
                if encoded.is_empty() {
                    Ok(())
                } else {
                    self.write_from(target, &encoded)
                }
            }
            FilterKind::RunLengthEncode {
                pending,
                run_byte,
                run_count,
            } => {
                // Streaming RLE: emit runs incrementally as input arrives
                for &b in data {
                    if let Some(rb) = *run_byte {
                        if b == rb {
                            *run_count += 1;
                            if *run_count >= 128 {
                                // Max repeat run — flush
                                let lit = rle_emit_literals(pending);
                                if !lit.is_empty() {
                                    self.write_from(target, &lit)?;
                                    pending.clear();
                                }
                                let rep = rle_emit_repeat(rb, 128);
                                self.write_from(target, &rep)?;
                                *run_byte = None;
                                *run_count = 0;
                            }
                        } else {
                            // Different byte — decide what to do with accumulated run
                            if *run_count >= 3 {
                                // Emit pending literals, then the repeat run
                                let lit = rle_emit_literals(pending);
                                if !lit.is_empty() {
                                    self.write_from(target, &lit)?;
                                    pending.clear();
                                }
                                let rep = rle_emit_repeat(rb, *run_count);
                                self.write_from(target, &rep)?;
                            } else {
                                // Short run (1-2) — absorb into pending literals
                                for _ in 0..*run_count {
                                    pending.push(rb);
                                }
                                if pending.len() >= 128 {
                                    let lit = rle_emit_literals(pending);
                                    self.write_from(target, &lit)?;
                                    pending.clear();
                                }
                            }
                            *run_byte = Some(b);
                            *run_count = 1;
                        }
                    } else {
                        // No current run byte — start tracking
                        *run_byte = Some(b);
                        *run_count = 1;
                    }
                }
                Ok(())
            }
            FilterKind::FlateEncode {
                compressor,
                predictor,
                row_width,
                bpp,
                encode_buf,
                prev_row,
                ..
            } => {
                if *predictor <= 1 {
                    // No predictor — compress directly
                    flate_compress_data(compressor, data, |chunk| self.write_from(target, chunk))
                } else {
                    // Buffer input, process complete rows with predictor
                    encode_buf.extend_from_slice(data);
                    let rw = *row_width;
                    let pred = *predictor;
                    let bp = *bpp;
                    while encode_buf.len() >= rw {
                        let row: Vec<u8> = encode_buf.drain(..rw).collect();
                        let encoded = if pred >= 10 {
                            encode_png_row(&row, bp)
                        } else {
                            encode_tiff_row(&row, bp)
                        };
                        prev_row.copy_from_slice(&row);
                        flate_compress_data(compressor, &encoded, |chunk| {
                            self.write_from(target, chunk)
                        })?;
                    }
                    Ok(())
                }
            }
            FilterKind::LZWEncode { encoder } => {
                let mut out = vec![0u8; data.len() * 2 + 64];
                let result = encoder.encode_bytes(data, &mut out);
                if result.consumed_out > 0 {
                    self.write_from(target, &out[..result.consumed_out])?;
                }
                Ok(())
            }
            FilterKind::NullEncode => self.write_from(target, data),
            FilterKind::DCTEncode { buf, .. } => {
                // Buffering is inherent to JPEG — DCT transform, Huffman table
                // optimization, and quantization all require the full image.
                // Not convertible to streaming.
                buf.extend_from_slice(data);
                Ok(())
            }
            _ => Err(io::Error::other("not an encode filter")),
        };

        // Put state back
        self.files[entity.0 as usize].handle = FileHandle::Filter(state);
        result
    }

    /// Finalize and close an encode filter, flushing remaining data and EOD markers.
    fn close_encode_filter(&mut self, entity: EntityId) -> io::Result<()> {
        // Swap out filter state
        let entry = &mut self.files[entity.0 as usize];
        let mut state = match std::mem::replace(&mut entry.handle, FileHandle::Closed) {
            FileHandle::Filter(s) => s,
            other => {
                entry.handle = other;
                return Ok(());
            }
        };

        let target = state.source;
        match &mut state.kind {
            FilterKind::ASCIIHexEncode => {
                self.write_from(target, b">")?;
            }
            FilterKind::ASCII85Encode { buf, .. } => {
                // Flush remaining 1-3 bytes
                if !buf.is_empty() {
                    let n = buf.len();
                    // Pad to 4 bytes with zeros
                    while buf.len() < 4 {
                        buf.push(0);
                    }
                    let val = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
                    let mut chars = [0u8; 5];
                    let mut v = val;
                    for c in chars.iter_mut().rev() {
                        *c = (v % 85) as u8 + b'!';
                        v /= 85;
                    }
                    // Output n+1 chars (for n input bytes)
                    self.write_from(target, &chars[..n + 1])?;
                }
                self.write_from(target, b"~>")?;
            }
            FilterKind::RunLengthEncode {
                pending,
                run_byte,
                run_count,
            } => {
                // Flush remaining state
                if let Some(rb) = *run_byte {
                    if *run_count >= 3 {
                        let lit = rle_emit_literals(pending);
                        if !lit.is_empty() {
                            self.write_from(target, &lit)?;
                        }
                        let rep = rle_emit_repeat(rb, *run_count);
                        self.write_from(target, &rep)?;
                    } else {
                        for _ in 0..*run_count {
                            pending.push(rb);
                        }
                        let lit = rle_emit_literals(pending);
                        if !lit.is_empty() {
                            self.write_from(target, &lit)?;
                        }
                    }
                } else if !pending.is_empty() {
                    let lit = rle_emit_literals(pending);
                    self.write_from(target, &lit)?;
                }
                // EOD byte
                self.write_from(target, &[128])?;
            }
            FilterKind::FlateEncode {
                compressor,
                predictor,
                row_width,
                bpp,
                encode_buf,
                ..
            } => {
                // Flush any remaining partial row with predictor
                if *predictor > 1 && !encode_buf.is_empty() {
                    let rw = *row_width;
                    let pred = *predictor;
                    let bp = *bpp;
                    // Pad partial row to row_width with zeros
                    encode_buf.resize(rw, 0);
                    let row: Vec<u8> = std::mem::take(encode_buf);
                    let encoded = if pred >= 10 {
                        encode_png_row(&row, bp)
                    } else {
                        encode_tiff_row(&row, bp)
                    };
                    flate_compress_data(compressor, &encoded, |chunk| {
                        self.write_from(target, chunk)
                    })?;
                }
                // Flush with Finish
                let mut out = vec![0u8; 256];
                loop {
                    let before_out = compressor.total_out() as usize;
                    let status = compressor
                        .compress(&[], &mut out, flate2::FlushCompress::Finish)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    let produced = compressor.total_out() as usize - before_out;
                    if produced > 0 {
                        self.write_from(target, &out[..produced])?;
                    }
                    if matches!(status, flate2::Status::StreamEnd) {
                        break;
                    }
                }
            }
            FilterKind::LZWEncode { encoder } => {
                // Mark stream as ended, then flush remaining encoded data
                encoder.finish();
                let mut out = vec![0u8; 256];
                loop {
                    let result = encoder.encode_bytes(&[], &mut out);
                    if result.consumed_out > 0 {
                        self.write_from(target, &out[..result.consumed_out])?;
                    }
                    if result.consumed_out == 0 {
                        break;
                    }
                }
            }
            FilterKind::NullEncode => {}
            FilterKind::DCTEncode {
                buf,
                columns,
                rows,
                colors,
                quality,
                color_transform,
            } => {
                let w = *columns as u16;
                let h = *rows as u16;
                let nc = *colors as u8;
                let q = *quality;
                let _ct = *color_transform;

                // Determine color type for jpeg-encoder
                let color_type = match nc {
                    1 => jpeg_encoder::ColorType::Luma,
                    3 => jpeg_encoder::ColorType::Rgb,
                    4 => jpeg_encoder::ColorType::Cmyk,
                    _ => jpeg_encoder::ColorType::Rgb,
                };

                let expected_len = w as usize * h as usize * nc as usize;
                // Pad or truncate to expected size
                buf.resize(expected_len, 0);

                let mut jpeg_data = Vec::new();
                let encoder = jpeg_encoder::Encoder::new(&mut jpeg_data, q);
                encoder
                    .encode(buf, w, h, color_type)
                    .map_err(|e| io::Error::other(format!("JPEG encode error: {e}")))?;
                self.write_from(target, &jpeg_data)?;
            }
            _ => {}
        }

        // Close the target file (per PLRM, closing a filter closes its target)
        self.close(target)?;

        // entry.handle is already Closed from swap-out
        Ok(())
    }

    /// Read a line (up to newline or EOF). Returns (bytes_read, hit_newline).
    pub fn readline(&mut self, entity: EntityId, buf: &mut [u8]) -> io::Result<(usize, bool)> {
        let mut count = 0;
        let mut hit_newline = false;
        loop {
            if count >= buf.len() {
                break;
            }
            match self.read_byte(entity)? {
                None => break,
                Some(b'\n') => {
                    hit_newline = true;
                    break;
                }
                Some(b'\r') => {
                    hit_newline = true;
                    // Consume optional \n after \r (CR+LF is one line ending)
                    match self.read_byte(entity)? {
                        Some(b'\n') => {} // \r\n consumed as single line ending
                        Some(other) => self.putback_bytes(entity, &[other]),
                        None => {}
                    }
                    break;
                }
                Some(b) => {
                    buf[count] = b;
                    count += 1;
                }
            }
        }
        Ok((count, hit_newline))
    }

    /// Get file position.
    pub fn position(&mut self, entity: EntityId) -> io::Result<u64> {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => f.stream_position(),
            FileHandle::StringSource { pos, .. } => Ok(*pos as u64),
            FileHandle::Filter(state) => Ok(state.bytes_read),
            _ => Err(io::Error::other("not seekable")),
        }
    }

    /// Set file position.
    pub fn set_position(&mut self, entity: EntityId, pos: u64) -> io::Result<()> {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => {
                f.seek(io::SeekFrom::Start(pos))?;
                Ok(())
            }
            FileHandle::StringSource { data, pos: p } => {
                *p = (pos as usize).min(data.len());
                Ok(())
            }
            _ => Err(io::Error::other("not seekable")),
        }
    }

    /// Flush a file.
    pub fn flush(&mut self, entity: EntityId) -> io::Result<()> {
        let entry = &self.files[entity.0 as usize];
        match &entry.handle {
            FileHandle::Real(_) => {
                let entry = &mut self.files[entity.0 as usize];
                if entry.mode.starts_with('r') {
                    // Read file: consume remaining data and close (PLRM)
                    entry.handle = FileHandle::Closed;
                } else if let FileHandle::Real(f) = &mut entry.handle {
                    f.get_mut().flush()?;
                }
            }
            FileHandle::Stdout => io::stdout().flush()?,
            FileHandle::Stderr => io::stderr().flush()?,
            FileHandle::Filter(_) => {
                // For read filters, flushfile consumes all remaining data
                // until EOF. This is critical for SubFileDecode — it advances
                // the underlying source past the filtered content.
                let mut buf = [0u8; 4096];
                loop {
                    match self.read_into(entity, &mut buf) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            }
            FileHandle::StringSource { .. } => {
                // For string sources, advance to EOF
                let entry = &mut self.files[entity.0 as usize];
                if let FileHandle::StringSource { data, pos } = &mut entry.handle {
                    *pos = data.len();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Check if a file entity is valid (not closed).
    pub fn is_open(&self, entity: EntityId) -> bool {
        if (entity.0 as usize) >= self.files.len() {
            return false;
        }
        !matches!(self.files[entity.0 as usize].handle, FileHandle::Closed)
    }

    /// Check if a file entity is readable (open, not stdout/stderr).
    pub fn is_readable(&self, entity: EntityId) -> bool {
        if (entity.0 as usize) >= self.files.len() {
            return false;
        }
        !matches!(
            self.files[entity.0 as usize].handle,
            FileHandle::Closed | FileHandle::Stdout | FileHandle::Stderr
        )
    }

    /// Read exactly N bytes from a file. Returns error on premature EOF.
    pub fn read_n_bytes(&mut self, entity: EntityId, n: usize) -> io::Result<Vec<u8>> {
        let mut result = Vec::with_capacity(n);
        for _ in 0..n {
            match self.read_byte(entity)? {
                Some(b) => result.push(b),
                None => return Err(io::Error::other("unexpected EOF")),
            }
        }
        Ok(result)
    }

    /// Put bytes back into a file so they'll be returned on the next read.
    /// For filters, uses the filter's putback buffer. For StringSource,
    /// decrements position.
    pub fn putback_bytes(&mut self, entity: EntityId, bytes: &[u8]) {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Filter(state) => {
                // Push in reverse so they come out in the right order (LIFO)
                for &b in bytes.iter().rev() {
                    state.putback.push(b);
                }
            }
            FileHandle::StringSource { pos, .. } => {
                *pos = pos.saturating_sub(bytes.len());
            }
            FileHandle::Real(f) => {
                // Seek backwards to put bytes back
                let _ = f.seek(std::io::SeekFrom::Current(-(bytes.len() as i64)));
            }
            _ => {}
        }
    }

    /// Check if a file entity is seekable (disk file).
    pub fn is_seekable(&self, entity: EntityId) -> bool {
        matches!(self.files[entity.0 as usize].handle, FileHandle::Real(_))
    }

    /// Get bytes available for reading. Returns -1 for stdin/filters/unknown,
    /// closed files, write-only files, and files at EOF.
    /// For disk files opened for reading, returns remaining bytes.
    pub fn bytes_available(&mut self, entity: EntityId) -> i32 {
        let entry = &mut self.files[entity.0 as usize];
        // Write-only files return -1
        if entry.mode.starts_with('w') || entry.mode.starts_with('a') {
            return -1;
        }
        match &mut entry.handle {
            FileHandle::Real(f) => {
                if let Ok(cur) = f.stream_position() {
                    if let Ok(end) = f.seek(io::SeekFrom::End(0)) {
                        let _ = f.seek(io::SeekFrom::Start(cur));
                        let remaining = end.saturating_sub(cur);
                        if remaining == 0 {
                            -1 // at EOF
                        } else {
                            remaining.min(i32::MAX as u64) as i32
                        }
                    } else {
                        -1
                    }
                } else {
                    -1
                }
            }
            FileHandle::StringSource { data, pos } => {
                let remaining = data.len() - *pos;
                if remaining == 0 { -1 } else { remaining as i32 }
            }
            FileHandle::Closed => -1,
            _ => -1,
        }
    }

    /// Get the name of a file.
    pub fn name(&self, entity: EntityId) -> &str {
        &self.files[entity.0 as usize].name
    }

    /// Set the name of a file (e.g. to record the resolved path for `run`).
    pub fn set_name(&mut self, entity: EntityId, name: String) {
        self.files[entity.0 as usize].name = name;
    }

    /// Get the mode of a file.
    pub fn mode(&self, entity: EntityId) -> &str {
        &self.files[entity.0 as usize].mode
    }

    /// Number of files allocated.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether no files are allocated.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Push a byte back onto a filter's putback buffer.
    pub fn putback_byte(&mut self, entity: EntityId, byte: u8) {
        let entry = &mut self.files[entity.0 as usize];
        if let FileHandle::Filter(ref mut state) = entry.handle {
            state.putback.push(byte);
        }
    }

    /// Read all remaining bytes from a source (used by DCTDecode at creation).
    pub fn read_all(&mut self, entity: EntityId) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        while let Some(b) = self.read_byte(entity)? {
            data.push(b);
        }
        Ok(data)
    }

    // ---- Filter refill logic ----

    /// Refill the filter's output buffer by reading from the source and decoding.
    fn refill_filter(&mut self, state: &mut FilterState) -> io::Result<()> {
        state.output_buf.clear();
        state.output_pos = 0;

        match &mut state.kind {
            FilterKind::ASCIIHexDecode => {
                self.refill_ascii_hex(state.source, &mut state.output_buf, &mut state.eof)?;
            }
            FilterKind::ASCII85Decode { .. } => {
                // Need to pass group around without aliasing
                let source = state.source;
                self.refill_ascii85(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            FilterKind::RunLengthDecode { .. } => {
                let source = state.source;
                self.refill_rle(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            FilterKind::FlateDecode { .. } => {
                let source = state.source;
                self.refill_flate(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            FilterKind::LZWDecode { .. } => {
                let source = state.source;
                self.refill_lzw(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            FilterKind::DCTDecode { decoded, .. } => {
                if !*decoded {
                    // Lazy decode: read all JPEG data from source, then decode
                    let source = state.source;
                    let jpeg_data = self.read_all(source)?;
                    let mut decoder = jpeg_decoder::Decoder::new(jpeg_data.as_slice());
                    let pixels = decoder
                        .decode()
                        .map_err(|e| io::Error::other(format!("JPEG decode error: {e}")))?;
                    state.output_buf = pixels;
                    state.output_pos = 0;
                    *decoded = true;
                } else {
                    state.eof = true;
                }
            }
            FilterKind::JBIG2Decode { decoded, .. } => {
                if !*decoded {
                    let source = state.source;
                    let raw = self.read_all(source)?;
                    // Snapshot globals before mutating state.kind.
                    let globals = match &state.kind {
                        FilterKind::JBIG2Decode { globals, .. } => globals.clone(),
                        _ => None,
                    };
                    let image = hayro_jbig2::decode_embedded(&raw, globals.as_deref())
                        .map_err(|e| io::Error::other(format!("JBIG2 decode error: {e}")))?;
                    // Pack the bool grid into PDF DeviceGray bytes (1 bit per
                    // pixel, MSB-first; 0=black, 1=white). Pad each row to a
                    // byte boundary so the consumer can treat it as a
                    // standard 1-bpc raster.
                    let row_bytes = (image.width as usize).div_ceil(8);
                    let mut packed = vec![0xFFu8; row_bytes * image.height as usize];
                    for y in 0..image.height as usize {
                        for x in 0..image.width as usize {
                            if image.data[y * image.width as usize + x] {
                                packed[y * row_bytes + x / 8] &= !(0x80 >> (x % 8));
                            }
                        }
                    }
                    state.output_buf = packed;
                    state.output_pos = 0;
                    if let FilterKind::JBIG2Decode { decoded, .. } = &mut state.kind {
                        *decoded = true;
                    }
                } else {
                    state.eof = true;
                }
            }
            FilterKind::JPXDecode { decoded } => {
                if !*decoded {
                    let source = state.source;
                    let raw = self.read_all(source)?;
                    let image = hayro_jpeg2000::Image::new(
                        &raw,
                        &hayro_jpeg2000::DecodeSettings::default(),
                    )
                    .map_err(|e| io::Error::other(format!("JPXDecode error: {e}")))?;
                    let pixels = image
                        .decode()
                        .map_err(|e| io::Error::other(format!("JPXDecode error: {e}")))?;
                    state.output_buf = pixels;
                    state.output_pos = 0;
                    *decoded = true;
                } else {
                    state.eof = true;
                }
            }
            FilterKind::CCITTFaxDecode { decoded, .. } => {
                if !*decoded {
                    let source = state.source;
                    let ccitt_data = self.read_all(source)?;
                    let decoded_bytes = Self::decode_ccittfax(&ccitt_data, &mut state.kind)?;
                    state.output_buf = decoded_bytes;
                    state.output_pos = 0;
                    // Mark as decoded (re-borrow after decode_ccittfax)
                    if let FilterKind::CCITTFaxDecode { decoded, .. } = &mut state.kind {
                        *decoded = true;
                    }
                } else {
                    state.eof = true;
                }
            }
            FilterKind::SubFileDecode { .. } => {
                let source = state.source;
                self.refill_subfile(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            FilterKind::EexecDecode { .. } => {
                let source = state.source;
                self.refill_eexec(
                    source,
                    &mut state.kind,
                    &mut state.output_buf,
                    &mut state.eof,
                )?;
            }
            // Encode filters are write-only, never refilled via read path
            _ => {
                state.eof = true;
            }
        }
        Ok(())
    }

    /// Refill ASCIIHexDecode: read hex pairs, skip whitespace, stop at `>`.
    fn refill_ascii_hex(
        &mut self,
        source: EntityId,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let mut nibble: Option<u8> = None;
        let target = 4096;

        while out.len() < target {
            match self.read_byte(source)? {
                None => {
                    // Pad odd nibble
                    if let Some(high) = nibble.take() {
                        out.push(high << 4);
                    }
                    *eof = true;
                    return Ok(());
                }
                Some(b'>') => {
                    // EOD marker
                    if let Some(high) = nibble.take() {
                        out.push(high << 4);
                    }
                    *eof = true;
                    return Ok(());
                }
                Some(b) => {
                    let nib = match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None, // whitespace skipped
                    };
                    if let Some(n) = nib {
                        match nibble.take() {
                            None => nibble = Some(n),
                            Some(high) => out.push((high << 4) | n),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Refill ASCII85Decode: read 5-char groups, handle `z` and partial final group.
    fn refill_ascii85(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let group = match kind {
            FilterKind::ASCII85Decode { group } => group,
            _ => return Err(io::Error::other("not ASCII85")),
        };
        let target = 4096;

        while out.len() < target {
            match self.read_byte(source)? {
                None => {
                    // Flush partial group at EOF
                    if group.len() >= 2 {
                        ascii85_decode_partial(group, out);
                    }
                    group.clear();
                    *eof = true;
                    return Ok(());
                }
                Some(b'~') => {
                    // Start of EOD `~>` — consume the `>`
                    let _ = self.read_byte(source)?; // consume '>'
                    if group.len() >= 2 {
                        ascii85_decode_partial(group, out);
                    }
                    group.clear();
                    *eof = true;
                    return Ok(());
                }
                Some(b'z') => {
                    // Special: four zero bytes
                    out.extend_from_slice(&[0, 0, 0, 0]);
                }
                Some(b) if b.is_ascii_whitespace() => {
                    // Skip whitespace
                }
                Some(b) if (b'!'..=b'u').contains(&b) => {
                    group.push(b - b'!');
                    if group.len() == 5 {
                        // Decode full group
                        let val = group[0] as u64 * 85 * 85 * 85 * 85
                            + group[1] as u64 * 85 * 85 * 85
                            + group[2] as u64 * 85 * 85
                            + group[3] as u64 * 85
                            + group[4] as u64;
                        out.push((val >> 24) as u8);
                        out.push((val >> 16) as u8);
                        out.push((val >> 8) as u8);
                        out.push(val as u8);
                        group.clear();
                    }
                }
                Some(_) => {
                    // Invalid character — skip
                }
            }
        }
        Ok(())
    }

    /// Refill RunLengthDecode.
    fn refill_rle(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let rle_state = match kind {
            FilterKind::RunLengthDecode { state } => state,
            _ => return Err(io::Error::other("not RLE")),
        };
        let target = 4096;

        while out.len() < target {
            match rle_state {
                RleState::Init => {
                    match self.read_byte(source)? {
                        None | Some(128) => {
                            *rle_state = RleState::Eod;
                            *eof = true;
                            return Ok(());
                        }
                        Some(b) if b < 128 => {
                            *rle_state = RleState::Literal {
                                remaining: b as u16 + 1,
                            };
                        }
                        Some(b) => {
                            // 129..=255 → repeat next byte (257−b) times
                            let count = 257 - b as u16;
                            match self.read_byte(source)? {
                                None => {
                                    *rle_state = RleState::Eod;
                                    *eof = true;
                                    return Ok(());
                                }
                                Some(val) => {
                                    *rle_state = RleState::Repeat {
                                        byte: val,
                                        remaining: count,
                                    };
                                }
                            }
                        }
                    }
                }
                RleState::Literal { remaining } => {
                    if *remaining == 0 {
                        *rle_state = RleState::Init;
                        continue;
                    }
                    match self.read_byte(source)? {
                        None => {
                            *rle_state = RleState::Eod;
                            *eof = true;
                            return Ok(());
                        }
                        Some(b) => {
                            out.push(b);
                            *remaining -= 1;
                        }
                    }
                }
                RleState::Repeat { byte, remaining } => {
                    if *remaining == 0 {
                        *rle_state = RleState::Init;
                        continue;
                    }
                    out.push(*byte);
                    *remaining -= 1;
                }
                RleState::Eod => {
                    *eof = true;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Refill FlateDecode: read compressed data from source, decompress.
    fn refill_flate(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let (decompressor, raw_buf, predictor, columns, colors, bpc, prev_row) = match kind {
            FilterKind::FlateDecode {
                decompressor,
                raw_buf,
                predictor,
                columns,
                colors,
                bpc,
                prev_row,
            } => (
                decompressor,
                raw_buf,
                *predictor,
                *columns,
                *colors,
                *bpc,
                prev_row,
            ),
            _ => return Err(io::Error::other("not Flate")),
        };

        // Read a chunk of compressed data from source
        if raw_buf.is_empty() {
            let mut chunk = vec![0u8; 8192];
            let mut total = 0;
            // Try to read a chunk
            while let Some(b) = self.read_byte(source)? {
                chunk[total] = b;
                total += 1;
                if total >= chunk.len() {
                    break;
                }
            }
            if total == 0 {
                *eof = true;
                return Ok(());
            }
            raw_buf.extend_from_slice(&chunk[..total]);
        }

        // Decompress
        let mut decompressed = vec![0u8; 16384];
        let before_in = decompressor.total_in();
        let before_out = decompressor.total_out();
        let status = decompressor
            .decompress(raw_buf, &mut decompressed, flate2::FlushDecompress::None)
            .map_err(|e| io::Error::other(format!("flate2 decompress error: {}", e)))?;

        let consumed = (decompressor.total_in() - before_in) as usize;
        let produced = (decompressor.total_out() - before_out) as usize;

        // Remove consumed bytes from raw_buf
        raw_buf.drain(..consumed);

        if status == flate2::Status::StreamEnd {
            *eof = true;
        }

        let decompressed = &decompressed[..produced];

        // Apply predictor if needed
        if predictor == 1 || predictor == 0 {
            // No prediction
            out.extend_from_slice(decompressed);
        } else if predictor == 2 {
            // TIFF predictor 2
            apply_tiff_predictor(decompressed, out, columns, colors, bpc);
        } else if (10..=15).contains(&predictor) {
            // PNG predictors
            apply_png_predictor(decompressed, out, columns, colors, bpc, prev_row);
        } else {
            out.extend_from_slice(decompressed);
        }

        Ok(())
    }

    /// Refill LZWDecode: read compressed data from source, decompress incrementally.
    fn refill_lzw(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let (decoder, raw_buf) = match kind {
            FilterKind::LZWDecode { decoder, raw_buf } => (decoder, raw_buf),
            _ => return Err(io::Error::other("not LZW")),
        };

        let mut decompressed = vec![0u8; 16384];

        loop {
            // Read a chunk of compressed data from source if buffer is empty
            if raw_buf.is_empty() {
                let mut chunk = vec![0u8; 8192];
                let mut total = 0;
                while let Some(b) = self.read_byte(source)? {
                    chunk[total] = b;
                    total += 1;
                    if total >= chunk.len() {
                        break;
                    }
                }
                if total == 0 {
                    // Source exhausted — try one more decode to flush decoder internals
                    let result = decoder.decode_bytes(&[], &mut decompressed);
                    if result.consumed_out > 0 {
                        out.extend_from_slice(&decompressed[..result.consumed_out]);
                    }
                    *eof = true;
                    return Ok(());
                }
                raw_buf.extend_from_slice(&chunk[..total]);
            }

            // Decompress
            let result = decoder.decode_bytes(raw_buf, &mut decompressed);
            let consumed_in = result.consumed_in;
            let consumed_out = result.consumed_out;

            // Remove consumed bytes from raw_buf
            raw_buf.drain(..consumed_in);

            match result.status {
                Ok(weezl::LzwStatus::Done) => {
                    out.extend_from_slice(&decompressed[..consumed_out]);
                    *eof = true;
                    return Ok(());
                }
                Ok(weezl::LzwStatus::NoProgress) => {
                    if consumed_out > 0 {
                        out.extend_from_slice(&decompressed[..consumed_out]);
                        return Ok(());
                    }
                    // No progress — need more input data
                    if raw_buf.is_empty() {
                        continue; // will try to read more from source
                    }
                    // raw_buf has data but decoder made no progress — done
                    *eof = true;
                    return Ok(());
                }
                Ok(weezl::LzwStatus::Ok) => {
                    out.extend_from_slice(&decompressed[..consumed_out]);
                    if consumed_out > 0 {
                        return Ok(());
                    }
                    // Consumed input but no output yet — keep going
                    continue;
                }
                Err(e) => {
                    return Err(io::Error::other(format!("LZW decode error: {}", e)));
                }
            }
        }
    }

    /// Refill SubFileDecode.
    fn refill_subfile(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let (eod_string, eod_count, bytes_remaining) = match kind {
            FilterKind::SubFileDecode {
                eod_string,
                eod_count,
                bytes_remaining,
            } => (eod_string, eod_count, bytes_remaining),
            _ => return Err(io::Error::other("not SubFile")),
        };

        if eod_string.is_empty() {
            // Byte-count mode
            if let Some(remaining) = bytes_remaining {
                let target = (*remaining).min(4096) as usize;
                for _ in 0..target {
                    match self.read_byte(source)? {
                        Some(b) => {
                            out.push(b);
                            *remaining -= 1;
                        }
                        None => {
                            *eof = true;
                            return Ok(());
                        }
                    }
                }
                if *remaining <= 0 {
                    *eof = true;
                }
            } else {
                *eof = true;
            }
        } else {
            // String-search mode: pass data until N occurrences of EOD string found
            let eod = eod_string.clone();
            let target_count = *eod_count;
            let mut match_pos = 0;
            let mut found_count = 0;

            for _ in 0..4096 {
                match self.read_byte(source)? {
                    None => {
                        *eof = true;
                        return Ok(());
                    }
                    Some(b) => {
                        if b == eod[match_pos] {
                            match_pos += 1;
                            if match_pos == eod.len() {
                                found_count += 1;
                                // Per PLRM: EOD string is included in output
                                out.extend_from_slice(&eod);
                                match_pos = 0;
                                if found_count >= target_count {
                                    *eof = true;
                                    return Ok(());
                                }
                            }
                        } else {
                            // Output any partially matched bytes
                            if match_pos > 0 {
                                out.extend_from_slice(&eod[..match_pos]);
                                match_pos = 0;
                            }
                            out.push(b);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Decode CCITT Group 3 or Group 4 fax data using the `fax` crate.
    fn decode_ccittfax(data: &[u8], kind: &mut FilterKind) -> io::Result<Vec<u8>> {
        let FilterKind::CCITTFaxDecode {
            k,
            columns,
            rows,
            end_of_block,
            black_is1,
            ..
        } = kind
        else {
            return Err(io::Error::other("not a CCITTFaxDecode filter"));
        };
        let k = *k;
        let width = *columns as u16;
        let rows_limit = *rows;
        let end_of_block = *end_of_block;
        let black_is1 = *black_is1;

        // Bytes per scan line (1 bit per pixel, padded to byte boundary)
        let row_bytes = (width as usize).div_ceil(8);
        let mut output = Vec::new();
        let mut line_count: u32 = 0;

        // Callback to process each decoded line
        let mut process_line = |transitions: &[u16]| {
            // Stop after Rows lines if EndOfBlock is false and Rows > 0
            if !end_of_block && rows_limit > 0 && line_count >= rows_limit {
                return;
            }

            // Convert transitions to pixels and pack into bytes
            let line = fax::decoder::Line { transitions, width };
            let mut row = vec![0u8; row_bytes];
            for (i, color) in line.pels().enumerate() {
                if i >= width as usize {
                    break;
                }
                // fax crate: Color::Black = mark bit, Color::White = no bit
                // Pack MSB-first: pixel 0 in bit 7
                let is_set = matches!(color, fax::Color::Black);
                if is_set {
                    row[i / 8] |= 0x80 >> (i % 8);
                }
            }

            // Apply BlackIs1 polarity:
            // CCITT convention: 1 = black (mark). fax crate follows this.
            // PostScript convention (BlackIs1=false, default): 0 = black, 1 = white
            // So when BlackIs1 is false, we invert all bits.
            if !black_is1 {
                for byte in &mut row {
                    *byte = !*byte;
                }
            }

            output.extend_from_slice(&row);
            line_count += 1;
        };

        if k < 0 {
            // Group 4
            let height = if rows_limit > 0 {
                Some(rows_limit as u16)
            } else {
                None
            };
            fax::decoder::decode_g4(data.iter().copied(), width, height, |transitions| {
                process_line(transitions)
            });
        } else {
            // Group 3 (K=0: 1-D only, K>0: mixed 1-D/2-D)
            fax::decoder::decode_g3(data.iter().copied(), |transitions| {
                process_line(transitions);
            });
        }

        Ok(output)
    }

    /// Refill EexecDecode: read from source, decrypt with eexec cipher.
    /// Uses a small target (256 bytes) to limit read-ahead, since the
    /// encrypted section is followed by a finite cleartext padding area.
    fn refill_eexec(
        &mut self,
        source: EntityId,
        kind: &mut FilterKind,
        out: &mut Vec<u8>,
        eof: &mut bool,
    ) -> io::Result<()> {
        let FilterKind::EexecDecode {
            r,
            is_hex,
            skip_count,
            hex_leftover,
        } = kind
        else {
            return Ok(());
        };

        const C1: u16 = 52845;
        const C2: u16 = 22719;
        // Produce exactly 1 output byte per refill (byte-at-a-time). The
        // underlying source stream does its own buffering; over-reading
        // here would cause the source position to drift past the
        // encrypted section into the cleartext padding.
        const TARGET: usize = 1;

        // Auto-detect format on first call: read 8 bytes and check if hex
        if is_hex.is_none() {
            let mut probe = Vec::with_capacity(8);
            for _ in 0..8 {
                match self.read_byte(source)? {
                    Some(b) => probe.push(b),
                    None => break,
                }
            }
            if probe.is_empty() {
                *eof = true;
                return Ok(());
            }
            let hex = probe
                .iter()
                .all(|&b| b.is_ascii_hexdigit() || is_ps_whitespace(b));
            *is_hex = Some(hex);

            // Process probe bytes through the cipher
            if hex {
                let mut hex_digits = Vec::new();
                for &b in &probe {
                    if b.is_ascii_hexdigit() {
                        hex_digits.push(b);
                    }
                }
                let mut i = 0;
                while i + 1 < hex_digits.len() {
                    let cipher = (hex_val(hex_digits[i]) << 4) | hex_val(hex_digits[i + 1]);
                    let plain = cipher ^ (*r >> 8) as u8;
                    *r = (cipher as u16)
                        .wrapping_add(*r)
                        .wrapping_mul(C1)
                        .wrapping_add(C2);
                    if *skip_count < 4 {
                        *skip_count += 1;
                    } else {
                        out.push(plain);
                    }
                    i += 2;
                }
                if i < hex_digits.len() {
                    *hex_leftover = Some(hex_digits[i]);
                }
            } else {
                for &cipher in &probe {
                    let plain = cipher ^ (*r >> 8) as u8;
                    *r = (cipher as u16)
                        .wrapping_add(*r)
                        .wrapping_mul(C1)
                        .wrapping_add(C2);
                    if *skip_count < 4 {
                        *skip_count += 1;
                    } else {
                        out.push(plain);
                    }
                }
            }
        }

        // Continue reading and decrypting until we have TARGET bytes
        while out.len() < TARGET {
            let cipher_byte = if *is_hex == Some(true) {
                // Hex mode: read pairs of hex digits
                let hi = if let Some(h) = hex_leftover.take() {
                    h
                } else {
                    match read_next_hex_digit(self, source)? {
                        Some(d) => d,
                        None => {
                            *eof = true;
                            return Ok(());
                        }
                    }
                };
                // Odd hex digit at end — pad with 0.
                let lo = read_next_hex_digit(self, source)?.unwrap_or(b'0');
                (hex_val(hi) << 4) | hex_val(lo)
            } else {
                // Binary mode: read raw bytes
                match self.read_byte(source)? {
                    Some(b) => b,
                    None => {
                        *eof = true;
                        return Ok(());
                    }
                }
            };

            let plain = cipher_byte ^ (*r >> 8) as u8;
            *r = (cipher_byte as u16)
                .wrapping_add(*r)
                .wrapping_mul(C1)
                .wrapping_add(C2);
            if *skip_count < 4 {
                *skip_count += 1;
            } else {
                out.push(plain);
            }
        }

        Ok(())
    }
}

/// Read the next hex digit from a file, skipping whitespace.
fn read_next_hex_digit(store: &mut FileStore, source: EntityId) -> io::Result<Option<u8>> {
    loop {
        match store.read_byte(source)? {
            Some(b) if b.is_ascii_hexdigit() => return Ok(Some(b)),
            Some(b) if is_ps_whitespace(b) => continue,
            Some(_) | None => return Ok(None),
        }
    }
}

/// PostScript whitespace check.
fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// Convert a hex digit character to its numeric value.
fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Decode a partial ASCII85 group (2–4 chars) at end of data.
fn ascii85_decode_partial(group: &[u8], out: &mut Vec<u8>) {
    let n = group.len();
    if n < 2 {
        return;
    }
    // Pad with 84 (value of 'u' - '!')
    let mut padded = [84u8; 5];
    padded[..n].copy_from_slice(group);

    let val = padded[0] as u64 * 85 * 85 * 85 * 85
        + padded[1] as u64 * 85 * 85 * 85
        + padded[2] as u64 * 85 * 85
        + padded[3] as u64 * 85
        + padded[4] as u64;

    let bytes = [
        (val >> 24) as u8,
        (val >> 16) as u8,
        (val >> 8) as u8,
        val as u8,
    ];
    // Output n-1 bytes
    out.extend_from_slice(&bytes[..n - 1]);
}

/// Compress data through a flate compressor, writing output via callback.
fn flate_compress_data(
    compressor: &mut flate2::Compress,
    data: &[u8],
    mut write_fn: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut out = vec![0u8; data.len() + 64];
    let mut input_pos = 0;
    loop {
        let before_in = compressor.total_in() as usize;
        let before_out = compressor.total_out() as usize;
        let status = compressor
            .compress(&data[input_pos..], &mut out, flate2::FlushCompress::None)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let consumed = compressor.total_in() as usize - before_in;
        let produced = compressor.total_out() as usize - before_out;
        input_pos += consumed;
        if produced > 0 {
            write_fn(&out[..produced])?;
        }
        if input_pos >= data.len() || matches!(status, flate2::Status::StreamEnd) {
            break;
        }
    }
    Ok(())
}

/// Encode a row using PNG Sub filter (type 1) for FlateEncode predictor.
fn encode_png_row(row: &[u8], bpp: usize) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(1 + row.len());
    encoded.push(1); // Sub filter type byte
    for i in 0..row.len() {
        let left = if i >= bpp { row[i - bpp] } else { 0 };
        encoded.push(row[i].wrapping_sub(left));
    }
    encoded
}

/// Encode a row using TIFF Predictor 2 (horizontal differencing) for FlateEncode.
fn encode_tiff_row(row: &[u8], colors: usize) -> Vec<u8> {
    let mut encoded = Vec::from(row);
    // Work backwards to avoid clobbering values we still need
    for i in (colors..encoded.len()).rev() {
        encoded[i] = encoded[i].wrapping_sub(encoded[i - colors]);
    }
    encoded
}

/// Apply TIFF horizontal differencing predictor.
fn apply_tiff_predictor(data: &[u8], out: &mut Vec<u8>, columns: u32, colors: u32, bpc: u32) {
    let bytes_per_pixel = (colors * bpc).div_ceil(8);
    let row_bytes = (columns * colors * bpc).div_ceil(8);

    for row in data.chunks(row_bytes as usize) {
        let mut prev = vec![0u8; bytes_per_pixel as usize];
        for (i, &b) in row.iter().enumerate() {
            let pi = i % bytes_per_pixel as usize;
            let val = b.wrapping_add(prev[pi]);
            out.push(val);
            prev[pi] = val;
        }
    }
}

/// Apply PNG row predictor filters.
fn apply_png_predictor(
    data: &[u8],
    out: &mut Vec<u8>,
    columns: u32,
    colors: u32,
    bpc: u32,
    prev_row: &mut Vec<u8>,
) {
    let bytes_per_pixel = (colors * bpc).div_ceil(8) as usize;
    let row_bytes = (columns * colors * bpc).div_ceil(8) as usize;
    let stride = row_bytes + 1; // +1 for filter type byte

    if prev_row.is_empty() {
        prev_row.resize(row_bytes, 0);
    }

    let mut pos = 0;
    while pos + stride <= data.len() {
        let filter_type = data[pos];
        let row_data = &data[pos + 1..pos + stride];
        let mut decoded_row = vec![0u8; row_bytes];

        for i in 0..row_bytes {
            let raw = row_data[i];
            let a = if i >= bytes_per_pixel {
                decoded_row[i - bytes_per_pixel]
            } else {
                0
            };
            let b = prev_row[i];
            let c = if i >= bytes_per_pixel {
                prev_row[i - bytes_per_pixel]
            } else {
                0
            };

            decoded_row[i] = match filter_type {
                0 => raw,                                                 // None
                1 => raw.wrapping_add(a),                                 // Sub
                2 => raw.wrapping_add(b),                                 // Up
                3 => raw.wrapping_add(((a as u16 + b as u16) / 2) as u8), // Average
                4 => raw.wrapping_add(paeth_predictor(a, b, c)),          // Paeth
                _ => raw,
            };
        }

        out.extend_from_slice(&decoded_row);
        prev_row.copy_from_slice(&decoded_row);
        pos += stride;
    }

    // Handle any remaining bytes that don't form a complete row
    if pos < data.len() {
        out.extend_from_slice(&data[pos..]);
    }
}

/// PNG Paeth predictor function.
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

impl FilterKind {
    /// Create a new ASCIIHexDecode filter.
    pub fn ascii_hex_decode() -> Self {
        Self::ASCIIHexDecode
    }

    /// Create a new ASCII85Decode filter.
    pub fn ascii85_decode() -> Self {
        Self::ASCII85Decode { group: Vec::new() }
    }

    /// Create a new RunLengthDecode filter.
    pub fn run_length_decode() -> Self {
        Self::RunLengthDecode {
            state: RleState::Init,
        }
    }

    /// Create a new FlateDecode filter with predictor parameters.
    pub fn flate_decode(predictor: u8, columns: u32, colors: u32, bpc: u32) -> Self {
        Self::FlateDecode {
            decompressor: flate2::Decompress::new(true),
            raw_buf: Vec::new(),
            predictor,
            columns,
            colors,
            bpc,
            prev_row: Vec::new(),
        }
    }

    /// Create a new streaming LZWDecode filter.
    pub fn lzw_decode(early_change: bool) -> Self {
        let decoder = if early_change {
            weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
        } else {
            weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
        };
        Self::LZWDecode {
            decoder,
            raw_buf: Vec::new(),
        }
    }

    /// Create a new DCTDecode filter (lazily decoded on first read).
    pub fn dct_decode(color_transform: Option<bool>) -> Self {
        Self::DCTDecode {
            decoded: false,
            color_transform,
        }
    }

    /// Create a new JBIG2Decode filter (lazily decoded on first read).
    /// `globals` is the optional /JBIG2Globals stream referenced by the
    /// PDF spec; pass `None` for an embedded stream that doesn't share a
    /// globals segment.
    pub fn jbig2_decode(globals: Option<Vec<u8>>) -> Self {
        Self::JBIG2Decode {
            decoded: false,
            globals,
        }
    }

    /// Create a new JPXDecode filter (lazily decoded on first read).
    pub fn jpx_decode() -> Self {
        Self::JPXDecode { decoded: false }
    }

    /// Create a new DCTEncode filter.
    pub fn dct_encode(
        columns: u32,
        rows: u32,
        colors: u32,
        quality: u8,
        color_transform: bool,
    ) -> Self {
        Self::DCTEncode {
            buf: Vec::new(),
            columns,
            rows,
            colors,
            quality,
            color_transform,
        }
    }

    /// Create a new SubFileDecode filter.
    pub fn sub_file_decode(
        eod_string: Vec<u8>,
        eod_count: i32,
        bytes_remaining: Option<i64>,
    ) -> Self {
        Self::SubFileDecode {
            eod_string,
            eod_count,
            bytes_remaining,
        }
    }

    /// Create a new CCITTFaxDecode filter with PLRM defaults.
    pub fn ccittfax_decode(
        k: i32,
        columns: u32,
        rows: u32,
        end_of_line: bool,
        encoded_byte_align: bool,
        end_of_block: bool,
        black_is1: bool,
    ) -> Self {
        Self::CCITTFaxDecode {
            decoded: false,
            k,
            columns,
            rows,
            end_of_line,
            encoded_byte_align,
            end_of_block,
            black_is1,
        }
    }

    /// Create a new EexecDecode filter.
    pub fn eexec_decode() -> Self {
        Self::EexecDecode {
            r: 55665,
            is_hex: None,
            skip_count: 0,
            hex_leftover: None,
        }
    }

    /// Create a new ASCIIHexEncode filter.
    pub fn ascii_hex_encode() -> Self {
        Self::ASCIIHexEncode
    }

    /// Create a new ASCII85Encode filter.
    pub fn ascii85_encode() -> Self {
        Self::ASCII85Encode {
            buf: Vec::with_capacity(4),
            col: 0,
        }
    }

    /// Create a new RunLengthEncode filter.
    pub fn run_length_encode() -> Self {
        Self::RunLengthEncode {
            pending: Vec::new(),
            run_byte: None,
            run_count: 0,
        }
    }

    /// Create a new FlateEncode filter with optional predictor parameters.
    pub fn flate_encode(predictor: u8, columns: u32, colors: u32, bpc: u32) -> Self {
        let row_width = (columns as usize * colors as usize * bpc as usize).div_ceil(8);
        let bpp = (colors as usize * bpc as usize).div_ceil(8);
        Self::FlateEncode {
            compressor: flate2::Compress::new(flate2::Compression::default(), true),
            predictor,
            columns,
            colors,
            bpc,
            row_width,
            bpp: bpp.max(1),
            encode_buf: Vec::new(),
            prev_row: vec![0u8; row_width],
        }
    }

    /// Create a new LZWEncode filter (EarlyChange=1 by default).
    pub fn lzw_encode(early_change: bool) -> Self {
        let encoder = if early_change {
            weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
        } else {
            weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8)
        };
        Self::LZWEncode { encoder }
    }

    /// Create a new NullEncode filter.
    pub fn null_encode() -> Self {
        Self::NullEncode
    }

    /// Returns true if this is an encode (write-direction) filter.
    pub fn is_encode(&self) -> bool {
        matches!(
            self,
            Self::ASCIIHexEncode
                | Self::ASCII85Encode { .. }
                | Self::RunLengthEncode { .. }
                | Self::FlateEncode { .. }
                | Self::LZWEncode { .. }
                | Self::NullEncode
                | Self::DCTEncode { .. }
        )
    }
}

impl FileStore {
    /// Create a lazy DCTDecode filter (decodes on first read).
    pub fn create_dct_filter(
        &mut self,
        source: EntityId,
        color_transform: Option<bool>,
    ) -> EntityId {
        self.create_filter(source, FilterKind::dct_decode(color_transform))
    }

    /// Create a DCTEncode filter.
    pub fn create_dct_encode_filter(
        &mut self,
        target: EntityId,
        columns: u32,
        rows: u32,
        colors: u32,
        quality: u8,
        color_transform: bool,
    ) -> EntityId {
        self.create_encode_filter(
            target,
            FilterKind::dct_encode(columns, rows, colors, quality, color_transform),
        )
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a path inside the OS temp directory. Portable across Linux
    /// (`/tmp`), macOS (`/var/folders/...`), and Windows
    /// (`%LOCALAPPDATA%\Temp`).
    fn tmp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn test_prealloc_standard_streams() {
        let store = FileStore::new();
        assert_eq!(store.len(), 3);
        assert!(store.is_open(FILE_STDIN));
        assert!(store.is_open(FILE_STDOUT));
        assert!(store.is_open(FILE_STDERR));
        assert_eq!(store.name(FILE_STDIN), "%stdin");
    }

    #[test]
    fn test_open_special_names() {
        let mut store = FileStore::new();
        assert_eq!(store.open("%stdin", "r").unwrap(), FILE_STDIN);
        assert_eq!(store.open("%stdout", "w").unwrap(), FILE_STDOUT);
        assert_eq!(store.open("%stderr", "w").unwrap(), FILE_STDERR);
    }

    #[test]
    fn test_file_round_trip() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_file_store.txt");
        let path = path.as_str();

        // Write
        let wid = store.open(path, "w").unwrap();
        store.write_from(wid, b"hello\n").unwrap();
        store.flush(wid).unwrap();
        store.close(wid).unwrap();

        // Read
        let rid = store.open(path, "r").unwrap();
        let mut buf = vec![0u8; 6];
        let n = store.read_into(rid, &mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"hello\n");
        store.close(rid).unwrap();

        // Cleanup
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_readline() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_readline.txt");
        let path = path.as_str();

        // Write test data
        {
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(b"line1\nline2\n").unwrap();
        }

        let id = store.open(path, "r").unwrap();
        let mut buf = vec![0u8; 20];
        let (n, nl) = store.readline(id, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"line1");
        assert!(nl);

        let (n, nl) = store.readline(id, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"line2");
        assert!(nl);

        store.close(id).unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_file_position() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_filepos.txt");
        let path = path.as_str();
        {
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(b"abcdef").unwrap();
        }

        let id = store.open(path, "r").unwrap();
        assert_eq!(store.position(id).unwrap(), 0);
        let mut buf = [0u8; 3];
        store.read_into(id, &mut buf).unwrap();
        assert_eq!(store.position(id).unwrap(), 3);
        store.set_position(id, 0).unwrap();
        assert_eq!(store.position(id).unwrap(), 0);
        store.close(id).unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_close_and_reopen() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_close.txt");
        let path = path.as_str();
        let id = store.open(path, "w").unwrap();
        store.write_from(id, b"test").unwrap();
        store.close(id).unwrap();
        assert!(!store.is_open(id));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_invalid_mode() {
        let mut store = FileStore::new();
        assert!(
            store
                .open(&tmp_path("stet_test_invalid_mode"), "z")
                .is_err()
        );
    }

    #[test]
    fn test_read_byte() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_readbyte.txt");
        let path = path.as_str();
        {
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(b"AB").unwrap();
        }
        let id = store.open(path, "r").unwrap();
        assert_eq!(store.read_byte(id).unwrap(), Some(b'A'));
        assert_eq!(store.read_byte(id).unwrap(), Some(b'B'));
        assert_eq!(store.read_byte(id).unwrap(), None);
        store.close(id).unwrap();
        std::fs::remove_file(path).ok();
    }

    // --- String source tests ---

    #[test]
    fn test_string_source() {
        let mut store = FileStore::new();
        let id = store.create_string_source(b"Hello".to_vec());
        assert_eq!(store.read_byte(id).unwrap(), Some(b'H'));
        assert_eq!(store.read_byte(id).unwrap(), Some(b'e'));
        let mut buf = [0u8; 3];
        let n = store.read_into(id, &mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"llo");
        assert_eq!(store.read_byte(id).unwrap(), None);
    }

    #[test]
    fn test_string_source_empty() {
        let mut store = FileStore::new();
        let id = store.create_string_source(Vec::new());
        assert_eq!(store.read_byte(id).unwrap(), None);
    }

    // --- ASCIIHexDecode tests ---

    #[test]
    fn test_ascii_hex_decode() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"48 65 6C 6C 6F>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCIIHexDecode);
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"Hello");
    }

    #[test]
    fn test_ascii_hex_decode_odd_nibble() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"4>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCIIHexDecode);
        let b = store.read_byte(filt).unwrap().unwrap();
        assert_eq!(b, 0x40);
        assert_eq!(store.read_byte(filt).unwrap(), None);
    }

    #[test]
    fn test_ascii_hex_decode_whitespace() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"4 1\n4 2>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCIIHexDecode);
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, &[0x41, 0x42]);
    }

    // --- ASCII85Decode tests ---

    #[test]
    fn test_ascii85_decode() {
        let mut store = FileStore::new();
        // "Man " in ASCII85 is "9jqo^"
        let src = store.create_string_source(b"9jqo^~>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCII85Decode { group: Vec::new() });
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"Man ");
    }

    #[test]
    fn test_ascii85_decode_z() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"z~>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCII85Decode { group: Vec::new() });
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_ascii85_decode_partial() {
        let mut store = FileStore::new();
        // Partial group: 2 chars "9j" → 1 byte
        let src = store.create_string_source(b"9j~>".to_vec());
        let filt = store.create_filter(src, FilterKind::ASCII85Decode { group: Vec::new() });
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(result.len(), 1);
    }

    // --- RunLengthDecode tests ---

    #[test]
    fn test_rle_literal() {
        let mut store = FileStore::new();
        // Length 2 (= 3 literal bytes), then EOD
        let src = store.create_string_source(vec![2, b'A', b'B', b'C', 128]);
        let filt = store.create_filter(
            src,
            FilterKind::RunLengthDecode {
                state: RleState::Init,
            },
        );
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"ABC");
    }

    #[test]
    fn test_rle_repeat() {
        let mut store = FileStore::new();
        // 253 → repeat next byte (257-253)=4 times, then EOD
        let src = store.create_string_source(vec![253, b'X', 128]);
        let filt = store.create_filter(
            src,
            FilterKind::RunLengthDecode {
                state: RleState::Init,
            },
        );
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"XXXX");
    }

    // --- SubFileDecode tests ---

    #[test]
    fn test_subfile_byte_count() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"Hello, World!".to_vec());
        let filt = store.create_filter(
            src,
            FilterKind::SubFileDecode {
                eod_string: Vec::new(),
                eod_count: 0,
                bytes_remaining: Some(5),
            },
        );
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"Hello");
    }

    // --- FlateDecode tests ---

    #[test]
    fn test_flate_decode() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;

        // Compress some data
        let original = b"Hello, stet PostScript interpreter! This is a test of FlateDecode.";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut store = FileStore::new();
        let src = store.create_string_source(compressed);
        let filt = store.create_filter(
            src,
            FilterKind::FlateDecode {
                decompressor: flate2::Decompress::new(true),
                raw_buf: Vec::new(),
                predictor: 1,
                columns: 0,
                colors: 0,
                bpc: 8,
                prev_row: Vec::new(),
            },
        );
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
    }

    // --- LZWDecode test ---

    #[test]
    fn test_lzw_decode() {
        // LZW-compressed data for "TOBEORNOTTOBEORTOBEORNOT"
        // Encode with early-change (tiff_size_switch) to match default PS behavior
        let original = b"TOBEORNOTTOBEORTOBEORNOT";
        let mut encoder = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
        let compressed = encoder.encode(original).unwrap();

        let mut store = FileStore::new();
        let src = store.create_string_source(compressed);
        let filt = store.create_filter(src, FilterKind::lzw_decode(true));
        let mut result = Vec::new();
        loop {
            match store.read_byte(filt).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
    }

    #[test]
    fn test_lzw_encode_single_byte() {
        let mut store = FileStore::new();
        let path = tmp_path("stet_test_lzw_single.bin");
        let path = path.as_str();

        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::lzw_encode(true));
        store.write_from(enc, b"Q").unwrap();
        store.close(enc).unwrap();

        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::lzw_decode(true));
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, b"Q");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_lzw_encode_roundtrip() {
        let mut store = FileStore::new();
        let original = b"Hello LZW!";

        // Create a string source as the write target (we'll use a temp file)
        let path = tmp_path("stet_test_lzw_encode.bin");
        let path = path.as_str();
        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::lzw_encode(true));

        // Write data through encoder
        store.write_from(enc, original).unwrap();
        store.close(enc).unwrap();

        // Read back and decode
        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::lzw_decode(true));
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_ascii_hex_encode_roundtrip() {
        let mut store = FileStore::new();
        let original = b"Hello";
        let path = tmp_path("stet_test_hex_encode.bin");
        let path = path.as_str();

        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::ascii_hex_encode());
        store.write_from(enc, original).unwrap();
        store.close(enc).unwrap();

        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::ascii_hex_decode());
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_ascii85_encode_roundtrip() {
        let mut store = FileStore::new();
        let original = b"Man sure.";
        let path = tmp_path("stet_test_a85_encode.bin");
        let path = path.as_str();

        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::ascii85_encode());
        store.write_from(enc, original).unwrap();
        store.close(enc).unwrap();

        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::ascii85_decode());
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_rle_encode_roundtrip() {
        let mut store = FileStore::new();
        let original = b"AAAAAABCBCBCBC";
        let path = tmp_path("stet_test_rle_encode.bin");
        let path = path.as_str();

        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::run_length_encode());
        store.write_from(enc, original).unwrap();
        store.close(enc).unwrap();

        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::run_length_decode());
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_flate_encode_roundtrip() {
        let mut store = FileStore::new();
        let original = b"Hello Flate compression test data!";
        let path = tmp_path("stet_test_flate_encode.bin");
        let path = path.as_str();

        let target = store.open(path, "w").unwrap();
        let enc = store.create_encode_filter(target, FilterKind::flate_encode(1, 1, 1, 8));
        store.write_from(enc, original).unwrap();
        store.close(enc).unwrap();

        let src = store.open(path, "r").unwrap();
        let dec = store.create_filter(src, FilterKind::flate_decode(1, 1, 1, 8));
        let mut result = Vec::new();
        loop {
            match store.read_byte(dec).unwrap() {
                Some(b) => result.push(b),
                None => break,
            }
        }
        assert_eq!(&result, original);
        std::fs::remove_file(path).ok();
    }

    // --- JBIG2Decode / JPXDecode tests ---

    #[test]
    fn test_jbig2_decode_constructor() {
        let kind = FilterKind::jbig2_decode(None);
        match kind {
            FilterKind::JBIG2Decode {
                decoded: false,
                globals: None,
            } => {}
            _ => panic!("unexpected variant"),
        }
        let kind = FilterKind::jbig2_decode(Some(vec![1, 2, 3]));
        match kind {
            FilterKind::JBIG2Decode {
                decoded: false,
                globals: Some(ref g),
            } if g == &[1, 2, 3] => {}
            _ => panic!("globals not stored"),
        }
    }

    #[test]
    fn test_jpx_decode_constructor() {
        let kind = FilterKind::jpx_decode();
        match kind {
            FilterKind::JPXDecode { decoded: false } => {}
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn test_jbig2_decode_malformed_returns_ioerror() {
        // Random bytes that don't form a valid JBIG2 stream — the
        // decoder must surface the failure as an io::Error rather than
        // panicking. We don't assert the exact error string; just that
        // reading the filter produces an Err.
        let mut store = FileStore::new();
        let src = store.create_string_source(b"not a jbig2 stream".to_vec());
        let filt = store.create_filter(src, FilterKind::jbig2_decode(None));
        assert!(store.read_byte(filt).is_err());
    }

    #[test]
    fn test_jpx_decode_malformed_returns_ioerror() {
        let mut store = FileStore::new();
        let src = store.create_string_source(b"not a jpeg2000 stream".to_vec());
        let filt = store.create_filter(src, FilterKind::jpx_decode());
        assert!(store.read_byte(filt).is_err());
    }
}
