// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

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
        /// All decoded data (weezl decodes all at once).
        decoded: Vec<u8>,
        pos: usize,
    },
    /// JPEG: lazily decoded on first read from source.
    DCTDecode {
        decoded: bool,
    },
    SubFileDecode {
        eod_string: Vec<u8>,
        eod_count: i32,
        bytes_remaining: Option<i64>,
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
}

/// Decoded-data buffer state for a filter file.
pub struct FilterState {
    pub kind: FilterKind,
    pub source: EntityId,
    pub output_buf: Vec<u8>,
    pub output_pos: usize,
    pub putback: Vec<u8>,
    pub eof: bool,
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
        let normalized = path
            .trim_start_matches('/')
            .replace("//", "/");
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
            "%stdin" => return Ok(FILE_STDIN),
            "%stdout" => return Ok(FILE_STDOUT),
            "%stderr" => return Ok(FILE_STDERR),
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
        if mode == "r" && let Some(data) = self.embedded_files.get(name) {
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
            })),
            name: "%filter".to_string(),
            mode: "r".to_string(),
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
            FileHandle::Closed => Err(io::Error::other("file closed")),
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
            self.files[entity.0 as usize].handle = FileHandle::Filter(state);
            return Ok(Some(b));
        }

        // 2. Return from output_buf if data available
        if state.output_pos < state.output_buf.len() {
            let b = state.output_buf[state.output_pos];
            state.output_pos += 1;
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
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => f.get_mut().write_all(&[byte]),
            FileHandle::Stdout => io::stdout().write_all(&[byte]),
            FileHandle::Stderr => io::stderr().write_all(&[byte]),
            FileHandle::Closed => Err(io::Error::other("file closed")),
            _ => Err(io::Error::other("not writable")),
        }
    }

    /// Write bytes from a buffer.
    pub fn write_from(&mut self, entity: EntityId, buf: &[u8]) -> io::Result<()> {
        let entry = &mut self.files[entity.0 as usize];
        match &mut entry.handle {
            FileHandle::Real(f) => f.get_mut().write_all(buf),
            FileHandle::Stdout => io::stdout().write_all(buf),
            FileHandle::Stderr => io::stderr().write_all(buf),
            FileHandle::Closed => Err(io::Error::other("file closed")),
            _ => Err(io::Error::other("not writable")),
        }
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
                if let FileHandle::Real(f) = &mut entry.handle {
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
            FilterKind::LZWDecode { decoded, pos } => {
                // LZW was decoded all at once; just return a chunk
                let remaining = decoded.len() - *pos;
                if remaining == 0 {
                    state.eof = true;
                } else {
                    let chunk = remaining.min(4096);
                    state
                        .output_buf
                        .extend_from_slice(&decoded[*pos..*pos + chunk]);
                    *pos += chunk;
                    if *pos >= decoded.len() {
                        state.eof = true;
                    }
                }
            }
            FilterKind::DCTDecode { decoded } => {
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
        // Produce exactly 1 output byte per refill, matching PostForge's
        // byte-at-a-time approach.  The underlying source stream does its own
        // buffering; over-reading here would cause the source position to
        // drift past the encrypted section into the cleartext padding.
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

    /// Create a new LZWDecode filter from already-decoded data.
    pub fn lzw_decode(decoded: Vec<u8>) -> Self {
        Self::LZWDecode { decoded, pos: 0 }
    }

    /// Create a new DCTDecode filter (lazily decoded on first read).
    pub fn dct_decode() -> Self {
        Self::DCTDecode { decoded: false }
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

    /// Create a new EexecDecode filter.
    pub fn eexec_decode() -> Self {
        Self::EexecDecode {
            r: 55665,
            is_hex: None,
            skip_count: 0,
            hex_leftover: None,
        }
    }
}

impl FileStore {
    /// Decode LZW-compressed data and create a filter.
    pub fn create_lzw_filter(&mut self, source: EntityId) -> io::Result<EntityId> {
        let compressed = self.read_all(source)?;
        let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8);
        let decoded = decoder
            .decode(&compressed)
            .map_err(|e| io::Error::other(format!("LZW decode error: {}", e)))?;
        Ok(self.create_filter(source, FilterKind::lzw_decode(decoded)))
    }

    /// Create a lazy DCTDecode filter (decodes on first read).
    pub fn create_dct_filter(&mut self, source: EntityId) -> EntityId {
        self.create_filter(source, FilterKind::dct_decode())
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
        let path = "/tmp/stet_test_file_store.txt";

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
        let path = "/tmp/stet_test_readline.txt";

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
        let path = "/tmp/stet_test_filepos.txt";
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
        let path = "/tmp/stet_test_close.txt";
        let id = store.open(path, "w").unwrap();
        store.write_from(id, b"test").unwrap();
        store.close(id).unwrap();
        assert!(!store.is_open(id));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_invalid_mode() {
        let mut store = FileStore::new();
        assert!(store.open("/tmp/test", "z").is_err());
    }

    #[test]
    fn test_read_byte() {
        let mut store = FileStore::new();
        let path = "/tmp/stet_test_readbyte.txt";
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
        // We'll test with weezl-encoded data
        let original = b"TOBEORNOTTOBEORTOBEORNOT";
        let mut encoder = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8);
        let compressed = encoder.encode(original).unwrap();

        let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8);
        let decoded_data = decoder.decode(&compressed).unwrap();

        let mut store = FileStore::new();
        let filt = store.create_filter(
            EntityId(0), // dummy source, not used
            FilterKind::LZWDecode {
                decoded: decoded_data,
                pos: 0,
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
}
