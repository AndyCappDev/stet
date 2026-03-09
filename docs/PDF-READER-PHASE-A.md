# Phase A: PDF Parser Foundation

## Scope

Everything needed to open a PDF, parse its structure, find pages, and extract raw content streams. NO rendering, NO content stream interpretation.

## Crate Setup

```
crates/stet-pdf-reader/
├── Cargo.toml          # Apache-2.0 OR MIT
├── src/
│   ├── lib.rs          # Public API: PdfDocument
│   ├── error.rs        # PdfError enum
│   ├── lexer.rs        # PDF tokenizer
│   ├── objects.rs      # PdfObj enum (read-side)
│   ├── xref.rs         # Xref table/stream parsing
│   ├── resolver.rs     # Indirect object resolution + stream decompression
│   ├── filters.rs      # Decode filter chain (FlateDecode, LZW, ASCII85, etc.)
│   └── page_tree.rs    # Page tree traversal, attribute inheritance
```

**Dependencies**: `flate2`, `weezl`, `thiserror`. No dependency on stet-core (avoids AGPL contamination in Phase A). stet-core dependency arrives in Phase B when we need Matrix, PsPath, DisplayList.

## Implementation Order

### Step 1: `error.rs`

```rust
#[derive(Debug, Error)]
pub enum PdfError {
    NotAPdf,
    UnsupportedVersion(String),
    NoStartXref,
    MalformedXref(usize),
    MalformedTrailer,
    ObjectNotFound { obj_num: u32, gen_num: u16 },
    UnexpectedToken { expected: String, got: String },
    Unterminated(&'static str),
    InvalidObject(usize),
    StreamMissingLength,
    UnsupportedFilter(String),
    DecompressionError(String),
    MissingKey(&'static str),
    TypeMismatch { key: String, expected: &'static str },
    PageOutOfRange(usize, usize),
    CircularReference(u32, u16),
    Encrypted,
    Other(String),
}
```

### Step 2: `objects.rs` — PDF Object Model

Read-side counterpart to stet-pdf's write-side PdfObj. Key differences: has `Ref(u32, u16)` for indirect references, `Stream` carries offset+length instead of owned data, strings/names are owned (require unescape/hex decode).

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum PdfObj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<PdfObj>),
    Dict(PdfDict),
    Stream { dict: PdfDict, data_offset: usize, data_len: usize },
    Ref(u32, u16),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfDict(Vec<(Vec<u8>, PdfObj)>);

impl PdfDict {
    pub fn get(&self, key: &[u8]) -> Option<&PdfObj>;
    pub fn get_name(&self, key: &[u8]) -> Option<&[u8]>;
    pub fn get_int(&self, key: &[u8]) -> Option<i64>;
    pub fn get_f64(&self, key: &[u8]) -> Option<f64>;   // int or real
    pub fn get_array(&self, key: &[u8]) -> Option<&[PdfObj]>;
    pub fn get_dict(&self, key: &[u8]) -> Option<&PdfDict>;
    pub fn get_ref(&self, key: &[u8]) -> Option<(u32, u16)>;
    pub fn insert(&mut self, key: Vec<u8>, val: PdfObj);
    pub fn entries(&self) -> &[(Vec<u8>, PdfObj)];
}
```

### Step 3: `lexer.rs` — PDF Tokenizer

Operates on `&[u8]` with cursor position. Simpler than PS — no procedures, no executable names.

```rust
pub struct Lexer<'a> { data: &'a [u8], pos: usize }

pub enum Token {
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),          // /SomeName → b"SomeName"
    LitString(Vec<u8>),     // (hello) → b"hello"
    HexString(Vec<u8>),     // <48656C6C6F> → b"Hello"
    ArrayBegin,             // [
    ArrayEnd,               // ]
    DictBegin,              // <<
    DictEnd,                // >>
    Keyword(Vec<u8>),       // obj, endobj, stream, endstream, R, null, etc.
    Eof,
}
```

**Parsing details:**
- Numbers: `+5`, `-3.2`, `.5` — distinguish int vs real by presence of `.`
- Names: `/` prefix, `#XX` hex escape decoding
- Literal strings: nested parens, backslash escapes (`\n \r \t \b \f \\ \( \)`), octal (`\012`), line continuation
- Hex strings: pairs of hex digits, odd count gets implicit trailing 0, whitespace ignored
- `<<` vs `<hex>`: peek after `<` — if another `<`, DictBegin; else hex string
- Comments: `%` to EOL, skipped
- Whitespace: space, tab, CR, LF, FF, NUL per PDF spec 7.2.2

### Step 4: `filters.rs` — Stream Decode Filters

Standalone decompression (no stet-core dependency).

```rust
pub enum Filter {
    FlateDecode,
    LZWDecode,
    ASCIIHexDecode,
    ASCII85Decode,
    RunLengthDecode,
}

pub fn decode_stream(
    raw_data: &[u8],
    filters: &[Filter],
    decode_parms: &[Option<&PdfDict>],
) -> Result<Vec<u8>, PdfError>;

pub fn parse_filters(dict: &PdfDict) -> Result<(Vec<Filter>, Vec<Option<PdfDict>>), PdfError>;
```

- **FlateDecode**: `flate2::Decompress`. PNG predictors (10-15) and TIFF predictor (2) via `/DecodeParms`.
- **LZWDecode**: `weezl::decode::Decoder`, MSB ordering, `/EarlyChange` param.
- **ASCIIHexDecode**: Inline hex pair decoder, stops at `>`.
- **ASCII85Decode**: Base-85 groups, `z` shortcut, stops at `~>`.
- **RunLengthDecode**: PackBits-style RLE.

PNG predictor post-processing is critical — it's the most common predictor in real-world PDFs.

### Step 5: `xref.rs` — Cross-Reference Table

```rust
pub enum XrefEntry {
    InFile { offset: usize, gen: u16 },
    InStream { stream_obj_num: u32, index_within: u16 },
    Free,
}

pub struct XrefTable {
    entries: Vec<Option<XrefEntry>>,
    pub trailer: PdfDict,
}

pub fn parse_xref(data: &[u8]) -> Result<XrefTable, PdfError>;
```

**Classic xref** (PDF spec 7.5.4):
1. `find_startxref()` — scan last 1024 bytes backwards for `startxref`.
2. Seek to offset, expect `xref\n`.
3. Parse subsections: `<first_obj> <count>`, then `count` × 20-byte entries.
4. Parse `trailer` keyword + dict.
5. Follow `/Prev` chain for incremental updates (most recent entry wins).

**Xref streams** (PDF 1.5+):
1. At `startxref` offset, find an indirect object with `/Type /XRef`.
2. Dict has `/W [w1 w2 w3]`, optionally `/Index [first count ...]`.
3. Decompress stream, decode packed binary entries.
4. Stream dict IS the trailer dict.

**Tolerance:**
- Allow extra whitespace around `startxref`.
- Accept `\r`, `\n`, or `\r\n` line endings in xref entries.
- Search last 1024 bytes for `startxref` (not just last line).

### Step 6: `resolver.rs` — Object Resolution

The central integration piece: ties xref entries to parsed objects with lazy caching.

```rust
pub struct Resolver<'a> {
    data: &'a [u8],
    xref: XrefTable,
    cache: RefCell<HashMap<(u32, u16), PdfObj>>,
    resolving: RefCell<HashSet<(u32, u16)>>,
}

impl<'a> Resolver<'a> {
    pub fn resolve(&self, obj_num: u32, gen_num: u16) -> Result<PdfObj, PdfError>;
    pub fn deref(&self, obj: &PdfObj) -> Result<PdfObj, PdfError>;
    pub fn stream_data(&self, obj_num: u32, gen_num: u16) -> Result<Vec<u8>, PdfError>;
    pub fn trailer(&self) -> &PdfDict;

    fn parse_object_at(&self, offset: usize) -> Result<PdfObj, PdfError>;
    fn parse_object_from_stream(&self, stream_obj_num: u32, index: u16) -> Result<PdfObj, PdfError>;
}
```

**Object parsing** (`parse_object_at`):
1. Lexer at offset → expect `<num> <gen> obj`.
2. Parse object value (recursive descent).
3. Stream detection: after dict, check for `stream` keyword. Data starts after `stream\n` (or `stream\r\n`). Length from `/Length` (may itself be an indirect ref — resolve immediately).
4. Expect `endobj`.

**Object streams** (`parse_object_from_stream`):
1. Resolve the ObjStm container (must be InFile, not nested).
2. Decompress, parse header (`/N` objects, `/First` offset).
3. Parse target object at its offset within decompressed data.
4. Cache ALL objects from the stream (amortize decompression).

**Circular reference protection**: `resolving` HashSet guards against infinite loops.

### Step 7: `page_tree.rs` — Page Tree Traversal

```rust
pub struct PageInfo {
    pub obj_num: u32,
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub rotate: i32,
    pub resources: PdfDict,
    pub contents: Vec<(u32, u16)>,
}

pub fn collect_pages(resolver: &Resolver) -> Result<Vec<PageInfo>, PdfError>;
```

1. `/Root` → Catalog → `/Pages` → Pages tree root.
2. DFS through `/Kids`. Track inherited attributes: MediaBox, CropBox, Resources, Rotate.
3. Leaf (`/Type /Page`): create PageInfo with resolved/inherited attributes.
4. `/Contents`: normalize absent/single-ref/array to `Vec<(u32, u16)>`.
5. `/CropBox` defaults to MediaBox. `/Rotate` defaults to 0.

### Step 8: `lib.rs` — Public API

```rust
pub struct PdfDocument<'a> {
    resolver: Resolver<'a>,
    pages: Vec<PageInfo>,
}

impl<'a> PdfDocument<'a> {
    pub fn from_bytes(data: &'a [u8]) -> Result<Self, PdfError>;
    pub fn page_count(&self) -> usize;
    pub fn page_size(&self, page: usize) -> Result<(f64, f64), PdfError>;
    pub fn page_info(&self, page: usize) -> Result<&PageInfo, PdfError>;
    pub fn page_contents(&self, page: usize) -> Result<Vec<u8>, PdfError>;
    pub fn resolver(&self) -> &Resolver<'a>;
}
```

## Test Strategy

| Module | Tests |
|--------|-------|
| lexer | Token round-trips (int, real, name, strings, keywords), edge cases (empty name, nested parens, `#XX`, `<<` vs `<hex>`), comments |
| filters | FlateDecode round-trip, PNG predictors, LZW known vectors, ASCII85 known vectors, filter chains |
| xref | Parse our own PDFs as golden tests, synthesized minimal PDFs, incremental updates, xref streams |
| resolver | Indirect refs, object streams, circular reference detection, stream with indirect `/Length` |
| page_tree | Single/multi-page, attribute inheritance, CropBox default, rotation |
| integration | Parse `samples/*.pdf`: verify page count, page sizes, content streams decompress |

## Challenges

1. **Stream `/Length` as indirect ref** — Must resolve during `parse_object_at`, before stream bounds are known. The referenced object must be InFile (not in the same stream).

2. **`RefCell` reentrancy** — Cache uses `RefCell<HashMap>`. Recursive resolution (e.g., `/Length` ref) must not hold borrows across calls. Clone out of cache before returning.

3. **Xref stream bootstrap** — No chicken-and-egg: xref stream is located by `startxref` offset (direct), not by xref lookup. Decompression available before xref is fully parsed.

## Verification

```bash
cargo build -p stet-pdf-reader
cargo test -p stet-pdf-reader

# Integration: parse our own PDFs
cargo test -p stet-pdf-reader -- integration

# Verify against known page counts
# xref_test.pdf: 1 page, 612×792
# hospital.pdf: 1 page (EPS)
# javaplatform.pdf: 25 pages, 612×792
```
