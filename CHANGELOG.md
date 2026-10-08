# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`PdfDocument::set_max_image_pixels`: an application's own ceiling on
  image size.** stet holds every image to limits sized for prepress
  (100,000 samples a side, four billion in all) and to nothing lower,
  because the right figure depends on the machine. That leaves JPEG 2000
  exposed: its decoder needs about 39 bytes of memory per sample the
  image's header declares, whatever the size of the stream, so a
  legitimate 212-megapixel scan costs 8.5 GB and a hostile 25 KB stream
  declaring 60,000 × 60,000 asks for over 100 GB — and nothing in the
  file tells the two apart. With a ceiling set, an image over it is left
  out of the page with a warning naming its size. The size counted is the
  one decoded: for JPEG 2000 the size in the stream's own header, or the
  reduced size under `ImageResolution::Rendered`, so a very large image
  drawn small is still drawn; for every other image, mask and soft mask,
  the size in its dictionary. Off by default. Also new:
  `DecodeBudget::with_image_pixels`. See `docs/PDF-READER-API.md`, "A
  ceiling on image size".

- **`--box-snap`, and `PdfDocument::page_area_rect_on_pixel_grid`** in
  `stet-pdf-reader`: a `--box` area whose edges fall between device pixels
  is rasterised at its own sub-pixel phase, so it antialiases differently
  from the same area cropped out of a full-page render — 20 % of the pixels
  of a 751 × 666 crop of a textbook spread at 150 dpi differed, by up to 200
  levels. `--box-snap` widens the area outward to whole pixels of the crop
  box's pixel grid at the chosen resolution, so it renders the page's own
  pixels; the output can be up to one pixel larger per side. An area
  outside the crop box, a bleed for one, keeps its reach. `--device png`
  only, and not with `--width`/`--height`. Contributed by @jungseohaan
  (#5).

- **`PdfDocument::set_image_resolution`, and `ImageResolution`.** A
  display list keeps every image at its stored resolution, so it can be
  zoomed into or written to another format; that is
  `ImageResolution::Full`, the default, and it is unchanged. A caller that
  builds a list with `render_page` only to rasterise it at that resolution
  can now say so with `ImageResolution::Rendered`, and get what
  `render_page_to_rgba` does on its own (see "Changed"). See
  `docs/PDF-READER-API.md`, "Display lists and resolution".

- **`PdfDocument::from_owned`: a document that owns its bytes.**
  `from_bytes` borrows the file, so a `PdfDocument<'a>` cannot outlive the
  buffer, and an application that keeps documents open — a viewer, a
  server with a cache — had to store each buffer beside its document or
  resort to a self-referential struct. `from_owned`, `from_owned_with_icc`
  and `from_owned_with_password` take the buffer and return a
  `PdfDocument<'static>`. They accept a `Vec<u8>`, a `Box<[u8]>` or an
  `Arc<[u8]>` (through the new `PdfBytes`) and copy none of them; an `Arc`
  stays shared, so a caller can keep its handle and try again with a
  password. The `from_bytes` constructors are unchanged.

- **`PdfDocument::set_annotation_filter`: draw some annotations and not
  others.** `set_render_annotations` is all or nothing, which does not
  suit an editor that draws review comments itself, as objects the user
  can move, but wants form fields left in the page. An `AnnotationFilter`
  selects by class — `AnnotationClass::Markup`, `Widget`, `Link`, `Other`,
  from the new `AnnotationKind::class()` — and by what the render is for:
  `RenderIntent::View` leaves out `Hidden` and `NoView` annotations,
  `RenderIntent::Print` draws only those with the `Print` flag.
  `AnnotationFilter::draws(&annotation)` is the renderer's own test, for a
  caller that draws the rest. The default is every class, for viewing;
  `set_render_annotations(false)` still turns them all off. See
  `docs/PDF-READER-API.md`, "Drawing some annotations and not others".

- **`stet_render::RegionRender`: a region of a page rendered with a layer
  set.** `render_region_prepared` and its parallel variants take twelve to
  fifteen positional arguments and no `LayerSet`, so a viewer that draws a
  page in tiles could not show or hide layers without interpreting the
  page again. `RegionRender::new(list, prepared, viewport, w, h, dpi)`
  takes the options by name — `.icc()`, `.image_cache()`, `.no_aa()`,
  `.layer_set()` — and renders with `.render()`, `.render_parallel()`,
  `.render_parallel_with_progress()`, `.render_parallel_cancellable()` or
  `.render_band()`. With no layer set it gives the pixels the existing
  functions give, and those are unchanged. One `PreparedDisplayList` and
  one `ImageCache` serve a page under every layer set: the precomputed
  bounds used to leave out a layer hidden by default, which an override
  could then not bring back in a tile that the layer alone touched. Also
  re-exported from the `stet` facade.

- **`stet-wasm`: `set_max_vm(interp, bytes)`** caps PostScript VM in the
  browser, as `--max-vm` does on the command line. The default there is
  1 GiB, a quarter of the address space, which is more than most pages
  embedding the viewer want to grant one document.
- **Licence note for the embedded CMYK profile.** `default_cmyk.icc` has
  been the public-domain (CC0) `CGATS001Compat-v2-micro` profile since
  before 0.1.0, but only its filename — Ghostscript's — said anything about
  where it came from, and a reader could take it for Ghostscript's
  AGPL-licensed profile. `LICENSE-CMYK-PROFILE` in the `stet` and
  `stet-wasm` crates now records its source and terms, and the prebuilt
  archives' `THIRD-PARTY-NOTICES.txt` carries it.

### Changed

- **`stet` exits 1 when a job fails.** A PostScript job stopped by an
  error, a timeout or a memory limit was reported on standard error as
  `Job N FAILED` while the process exited 0, so a script, a `make` rule
  or a CI step took a failed conversion for a good one. The status is now
  1 if any job failed — the remaining files are still processed, as
  before, and the closing line counts the failures (`Processed 3 jobs, 1
  failed`). A status a program asks for with `.quitwithcode` still wins,
  `quit` is still a success, and the interactive viewer is unaffected.
  Anything that ran `stet` over PostScript known to raise errors and
  relied on a zero status needs to allow for the new one.

- **A page that draws one gradient thousands of times renders in a tenth
  of the time and a third of the memory.** Some producers build a gradient
  out of thin clipped strips, each a separate `sh` of the same shading:
  pdf.js `bug1721218_reduced.pdf` draws 41 shadings 3,531 times on one
  letter page, and took 12.6 s and 2.6 GB at 72 dpi. It now takes 1.2 s
  and 1.0 GB, and the output is byte for byte the same, as it is for the
  first two pages of all 692 PDFs in the test set. Three things were
  wrong. The renderer visited every pixel of a shading's bounding box —
  the whole page, when it has no `/BBox` — before asking the clip whether
  to paint it; it now finds the stretch of each row the clip lets through
  first. Its cache of rasterised clip masks had no limit and held one for
  each of the page's thousands of clips; it is now bounded at 32 MB. And
  the reader parsed and sampled a shading's function afresh for every use;
  the colour stops are now kept for the page.

- **A very large JPEG 2000 image drawn small is decoded small.** Where a
  page is rasterised at once — `render_page_to_rgba` and its variants, and
  `stet --device png` for PDF input — a JPEG 2000 image the page draws at
  half its stored size or less is decoded at a lower resolution level of
  its codestream rather than in full and then scaled down. A 212-megapixel
  scan (pdf.js `issue19517.pdf`) rendered 3,152 pixels wide went from
  8.6 GB and 8 s to 0.6 GB and 1.1 s. The level is chosen to be at least
  the size drawn and to keep the picture within a quarter of a device
  pixel of where the full image would put it; a soft mask is averaged down
  to match. Pixels of such pages differ slightly from before: the
  wavelet's own smaller image is not the same filter as averaging blocks
  of the large one. Images with a `/Mask`, palette images, images inside
  tiling patterns or Type 3 glyphs and images in encrypted files are
  decoded in full as before, and so is everything in a display list from
  `render_page`, the viewer and PDF output, which stay independent of
  resolution.

- **`Resolver::data()` returns `&[u8]` borrowed from the resolver**, where
  it returned `&'a [u8]` borrowed from the file. A resolver may now own
  its bytes, so the longer lifetime can no longer be promised. Code that
  uses the slice while it holds the resolver is unaffected; code that kept
  the slice after letting the resolver go needs to keep the resolver, or
  its own reference to the file.

- **What the reader skips, it now reports.** `stet-pdf-reader` carries on
  past most of what is wrong with a page, which is right, but several of
  the things it carried on past left no trace: the page simply had
  something missing. These are now in `PdfDocument::parse_warnings()`:
  - **An operator that fails.** It is skipped and the stream goes on, as
    before, with a `ParsePhase::Content` warning naming it: `operator Do:
    object 12 0 not found`, `operator Do: decompression error: …`,
    `operator sh: sampled function has 18 inputs (1 to 16 supported)`,
    `operator c: need 6 operands, have 4`. So an image that will not
    decode and a shading whose function is refused both say so.
  - **A form, pattern, glyph procedure or soft-mask group whose stream
    ends in an error**, and **a page content stream that cannot be
    decoded** when the page has others that can.
  - **A page tree that loses pages**, under the new `ParsePhase::PageTree`:
    nested more than 256 levels deep, or a node listed twice. Such a
    document could open with no pages and no explanation.

  A page keeps at most 64 distinct content warnings, then one saying the
  list is cut short, and a message repeated by every operator in a stream
  costs a hash lookup. Nothing is drawn differently. **The `stet` command
  line prints these**, so a file that rendered silently with something
  missing now says what: of 1,681 test PDFs, 29 gain `warning:` lines (at
  most 28 each), most of them paths and text operators with the wrong
  number of operands.

- **`stet-pdf-reader` no longer prints to stderr.** A content stream
  error, a font or soft mask that would not load, a missing predefined
  CMap, a page cut short by the nesting budget and a CCITT image that
  needed a fallback were each written straight to the terminal of
  whatever application linked the library. They are now recorded in
  `PdfDocument::parse_warnings()` under the new `ParsePhase::Content`,
  with the page as the warning's location, when the page is first
  rendered; rendering it again adds nothing. Two of them were silenced
  after their first occurrence in the whole process, so a second document
  with the same fault said nothing; each document now has its own record.
  The `stet` command line prints them itself, so its output is unchanged
  apart from the wording of the two CCITT messages. Also new:
  `Resolver::warnings()` and `WarningSink::record_once`. A lint keeps
  the library from printing again.

- **An annotation flagged `NoView` is no longer drawn.** The flag means
  "do not display on screen"; it marks print-only content such as a
  watermark that should appear on paper alone. stet honoured `Hidden` and
  ignored `NoView`, so such an annotation showed in every render. A render
  is now for viewing unless told otherwise, and
  `set_annotation_filter(AnnotationFilter::new(RenderIntent::Print))` gives
  the print reading, in which these annotations are drawn.

- **The minimum supported Rust version is now 1.92** (was 1.88). The JBIG2,
  CCITT and JPEG 2000 decoders stet uses — `hayro-jbig2`, `hayro-ccitt` and
  `hayro-jpeg2000` — were two to three releases behind, held there by the
  old floor: their current versions need 1.92. They are now 0.3.1, 0.4.0
  and 0.4.1. A JBIG2 image is also packed to one bit per pixel as it is
  decoded, where it used to pass through a one-byte-per-pixel copy first.
  The 212-megapixel JPEG 2000 page of pdf.js's `issue19517.pdf` peaks at
  8.6 GB where it took 11 GB; that is still far too much, and is not
  finished work.

### Fixed

- **A gradient mesh from Illustrator raised `typecheck` in `shfill`.** A
  mesh or patch shading (ShadingType 4 to 7) may take its `DataSource`
  from a file as well as a string or an array (PLRM 3, 4.9.3), and
  Illustrator writes a gradient mesh that way: the vertex data follows
  the dictionary in the program, behind a filter on `currentfile`. stet
  accepted only the string and the array. A file is now read to its end,
  from where it stands, and painted as the same bytes in a string would
  be. Reported by @vivozi (#6), with a reproducer and the place in the
  source. Two more of the same kind, found beside it:
  - **A sampled (Type 0) function** may likewise take its samples from a
    positionable file; a `ReusableStreamDecode` filter, the way in-line
    data becomes one, was a `typecheck` there too. The samples are read
    from position 0, as the manual says, wherever the file was left.
  - **`setfileposition` raised `ioerror` for a `ReusableStreamDecode`
    filter**, the one filter that exists to be positioned. A program that
    paints one reusable stream twice repositions it in between, and could
    not.

- **A PostScript shading pattern painted nothing, and a tiling pattern
  painted fills only.** A pattern is a paint, used by whatever `fill`,
  `stroke` and `show` mark (PLRM 3, 4.9). A shading pattern
  (`PatternType 2`) was accepted by `makepattern` and `setpattern` and
  then drew nothing at all, for any shading type; a tiling pattern was
  honoured by `fill`, `eofill` and `rectfill`, while a stroke or text set
  in it came out in a solid colour. Now, for both kinds:
  - **Fills, strokes (`stroke`, `rectstroke`) and text are painted with
    the pattern.** A patterned stroke is painted as the area the stroke
    covers and patterned text as its outlines, so every output device sees
    an ordinary pattern fill; in PDF output such text is therefore
    outlines rather than text.
  - **A shading pattern** stays where `makepattern` placed it, whatever
    the current matrix when it is used, paints its shading's `Background`
    across what it fills, and reads a mesh's `DataSource` from the start
    of a positionable file each time one is made.
  - **`makepattern` no longer disturbs the graphics state.** `PaintProc`
    ran in the program's own state, so the colour, line width and path it
    set stayed set afterwards and the current path was lost; and a cell
    made while another pattern was current was painted with that pattern,
    which turned an uncoloured pattern's cell into stripes of the wrong
    colour. It now runs between an implicit `gsave` and `grestore`, with
    no pattern current.

- **A JPEG 2000 image whose own header claims an absurd size aborted the
  process.** `stet-pdf-reader` checks an image's `/Width` and `/Height`,
  but a JPEG 2000 stream carries its own, and those are the ones the
  decoder allocates for. A real 25 KB image with its header rewritten to
  2³¹ × 2³¹ samples, in a dictionary still saying 512 × 384, asked for
  1.8 × 10¹⁶ bytes: an allocation failure, which no caller can catch. The
  size in the stream is now held to the limits every other image is
  (100,000 samples a side, 4 × 10⁹ in all), and an image over them is
  refused with a warning. Found while measuring where the memory goes on a
  212-megapixel JPEG 2000 page.

- **A CMap with a byte that is not UTF-8 inside a hex string panicked.**
  `stet-pdf-reader` reads CMap streams as text, so such a byte becomes a
  three-byte replacement character, and two places then cut the hex string
  at a fixed byte offset — inside the character. A `/ToUnicode` stream with
  `<0041> <ÿÿÿ>` did it, and so did an embedded encoding CMap with a
  codespace range of `<ÿ>`; both are a panic in a release build, on opening
  the page. Found by the fuzzer. Such a string is now not a hex string, and
  the entry is skipped.

- **A PDF function could abort the process, panic, or take all the memory
  there is.** Functions colour every shading and convert every spot colour,
  and `stet-pdf-reader` took their dictionaries at their word. Found by the
  fuzzer, then by reading the rest of the file; each of these is a PDF
  under 1 KB:
  - A sampled (Type 0) function whose `/Size` is `[65535 65535 65535]`
    asked for its whole table in one reservation, 2.2 × 10¹⁵ bytes, and the
    allocator **aborted the process** — which no caller can catch. One
    entry of two billion, or `-1`, filled memory instead. The table now
    holds the samples the stream contains; one the stream does not reach
    reads as zero, as it did before.
  - `/Size []` panicked on an index; an entry of `0`, or of `1` with two or
    more inputs, and a `/BitsPerSample` of 64, overflowed (a panic in a
    debug build). `/Size` entries must now be positive, one per input, and
    `/BitsPerSample` one of the eight values the format allows; a function
    that fails either is refused, and what it would have coloured is not
    drawn.
  - A sampled function with more than 16 inputs is refused, as poppler
    refuses it: evaluating one point reads 2^m samples. One of the pdf.js
    test files, `axial_shading_many_inputs.pdf`, has 18; it drew a black
    page and now draws nothing, as poppler and Ghostscript do.
  - A calculator (Type 4) function has no loops, but `2 copy 4 copy 8 copy
    …` doubles its operand stack with each pair of tokens: forty of them
    took 4 GB. The stack now stops at 100 operands, the figure in the
    standard, which poppler and pdf.js also use.
  - `idiv` and `mod` by a number between −1 and 1 divided by zero and
    **panicked in a release build**, since both work on truncated
    integers; they give 0, as division by zero already did. `bitshift` by
    64 or more, and a function leaving fewer results than it has outputs,
    no longer overflow.

  Every valid file renders as before: two pages of each of 1,681 PDFs are
  byte-identical, bar the one above.

- **A PostScript program could raise `--max-vm`.** `MaxLocalVM` is a user
  parameter, and `<< /MaxLocalVM 2000000000 >> setuserparams` replaced
  whatever ceiling the command line had set — so `--max-vm 16` bounded only
  a program that left it alone. The host's ceiling now holds: a program may
  lower its limit and raise it again, but not past what the host allows,
  and `currentuserparams` reports the limit in force rather than the
  request. The built-in 8 GiB default is a ceiling in the same sense; pass
  `--max-vm` to allow more. Library users get the same guarantee from
  `Context::max_local_vm`, and `Context::vm_limit()` returns the limit in
  force.
- **A TrueType or OpenType font among the embedded resources never
  loaded.** `resources/Font/` may hold `.ttf` and `.otf` files beside the
  Type 1 ones, but they were read from disk even where the resources are
  embedded in the binary and there is no disk — a WebAssembly build. None
  of the 35 bundled fonts is affected, being Type 1; this matters to a
  build that embeds fonts of its own.
- **A large image drawn small no longer costs its full size in memory, and
  an image with a soft mask no longer costs it once per band.** Rendering
  converted every image to RGBA at full size before scaling it down, and
  an image inside a soft mask or a transparency group — every PDF image
  with an `/SMask` is one — was converted again by each band that touched
  it, with bands running in parallel. A 4500×6442 image with a soft mask,
  rendered 800 pixels wide, peaked at 1,075 MB on 24 threads; it now peaks
  at about 156 MB on any number, most of which is the decoded image
  itself. Four changes, none of which alters a pixel:
  - images inside soft masks and transparency groups are converted once
    per page, like images outside them;
  - an image that is scaled down is converted a strip of rows at a time,
    straight into the scaling filter, and never held at full size;
  - a page's images are converted in parallel;
  - an image drawn several times on a page — a logo, a texture — is
    converted once for all the placements that come out as the same
    pixels. That image drawn nine times renders in a quarter of the time
    on 24 threads and a tenth of it on one.

  One cost: converted images inside groups and soft masks are now held
  until the page is finished, so a single-threaded render of a page with
  many of them that are *not* drawn small can peak higher than before.

- **A colour key on a 16-bit image cleared the wrong pixels.** The
  renderer looked up each pixel's key one byte per sample, so with
  `bits_per_component: 16` it tested low bytes and neighbouring pixels'
  samples. It now tests each sample's high byte, the value the pixel is
  painted with. stet's own PostScript and PDF front ends never produce
  such an image (they reduce samples to 8 bits first), so only a display
  list built elsewhere was affected. `docs/DISPLAY-LIST.md` now says what
  sample layouts and keys a renderer is given ("Sample depth"); it listed
  depths of 1, 2 and 4 bits that no display list carries.

- **A PDF image declaring 9 to 15 bits per component rendered as noise.**
  PDF allows 1, 2, 4, 8 and 16; the reader unpacked the first three and
  reduced 16-bit samples, but handed any depth in between on as packed
  bytes, which were then read as 8-bit samples. Such an image, or soft
  mask, is now unpacked bit by bit and reduced to the nearest 8-bit
  value, as the other depths are, and a colour key or `/Decode` on it
  applies as it does to them. Ghostscript draws nothing for these files
  and poppler keeps the low 8 bits of each sample.
- **A PDF cut off just after its `startxref` keyword did not open.** The
  reader took the keyword as a promise of an offset and failed with
  "expected integer" when none followed, though a missing keyword was
  already handled. Such a file now opens the same way: from the
  cross-reference table when one is there, otherwise by scanning for its
  objects. Found with `issue6069.pdf` from the pdf.js test set.
- **A PDF page with a zero-size MediaBox crashed the PNG writer.** A page
  declaring `/MediaBox [0 0 0 0]` rendered to a 0 × 0 image, and `stet
  --device png` panicked writing it. A MediaBox that encloses no area is
  now treated as a missing one always was, and replaced by US Letter; a
  CropBox with nothing left inside the MediaBox — empty, or lying wholly
  outside it — is replaced by the MediaBox. Each replacement is reported
  by `PdfDocument::parse_warnings`, as `ParsePhase::PageBoxes`. The
  command line also reports a page too small to have any pixels at the
  chosen resolution as an error and exits 1, where it would have
  panicked. Found with `boundingBox_invalid.pdf` from the pdf.js test set.
- **A DeviceN colour space with many colourants could exhaust memory, in
  PDF and in PostScript.** A tint transform is sampled into a table of
  `samples ^ colourants` entries, and the colourant count is the file's to
  choose. A 1.1 KB PDF with an axial shading in a nine-colourant space took
  4.2 GB to open; eight colourants cost 540 to 690 MB for each such space;
  and in PostScript `setcolorspace` ran the tint transform procedure once
  per entry, 387 million times for nine colourants. Every such table now
  shares one ceiling, `TintLookupTable::MAX_GRID_POINTS`, sized by
  `TintLookupTable::grid_samples`. Up to seven colourants nothing changes.
  Eight get six samples a side where they had nine (eight, in a shading).
  Beyond eight there is no table: colours are computed from the tint
  transform itself, which is exact — a fill or stroke as before, an image
  pixel by pixel, an Indexed palette entry by entry — and what is given up
  is the spot identity of that colour in `--device pdf` output, which is
  written in the alternate space. `TintLookupTable::lookup_1d` and
  `lookup_nd` also return zeros for a table they cannot interpolate, where
  `lookup_nd` indexed out of bounds for more than eight inputs. Found with
  `postscript_type4_many_outputs.pdf` from the pdf.js test set.
- **A small PDF could ask for an unbounded amount of nested content.**
  Form XObjects, tiling patterns, Type 3 glyphs, soft-mask groups and
  annotation appearances may nest twenty deep, and nothing limited how many
  ran: a form drawing itself eight times, or a Type 3 glyph showing three
  copies of itself, ran until the process was killed, and so did nineteen
  ordinary forms each drawing the next twice. Two things bound it now. A
  stream that is already being interpreted is not entered again — a form
  cannot draw itself, so there is nothing to draw — which makes every
  self-referencing file finish at once. And each page has a budget for
  nested content, charged by depth, 115 times what the busiest of 33,567
  real pages used; a page that spends it draws what it had drawn, warns,
  and stops. The files that used to be killed now take up to a second and
  1.2 GB, which is the display list of what they did draw.
  Reuse is unaffected: a form or glyph used any number of times in a row is
  drawn each time.
- **An AES-256 PDF did not open when its password needed Unicode
  preparation.** PDF 2.0 has the writer run a password through SASLprep
  (RFC 4013) before hashing it: compatibility forms are folded, characters
  such as the soft hyphen are dropped. stet hashed the bytes as typed, so
  a password entered with a full-width letter, a decomposed accent or a
  non-breaking space was refused although it was the right one. The reader
  now prepares the password and tries that first, then the bytes as given
  for files whose writer skipped the step; both are cut to the 127 bytes
  the algorithm reads. The preparation is the new `saslprep` feature of
  `stet-pdf-reader`, on by default; it brings in the `stringprep` crate and
  its Unicode tables (about 170 KB in the `stet` binary), and a build
  without it behaves as before. RC4 and AES-128 files are unaffected.

- **A PDF's owner password did not open it.** An encrypted PDF has two
  passwords, and either one decrypts the file. stet tried the password it
  was given only as the user password, so `--password` with the owner
  password, or `PdfDocument::from_bytes_with_password` with it, was refused
  as wrong at every revision of the Standard security handler. It is now
  tried as each, user first: for RC4 and AES-128 files by decrypting `/O`
  to recover the user password, for AES-256 files against the owner hash
  and the key wrapped in `/OE`.

- **A password with an accented letter did not open an RC4 or AES-128
  PDF.** Those security handlers store the password in PDFDocEncoding —
  Latin-1 for accented letters, its own codes for a few characters such as
  the euro sign — and a caller passes what the user typed, which is UTF-8.
  The two agree only for ASCII, so `--password æøå` was refused on a file
  whose password is exactly that. The bytes as given are still tried
  first; when they are UTF-8 with something beyond ASCII, the
  PDFDocEncoding form is tried after them, for the user and the owner
  password alike. AES-256 files, which store UTF-8, already worked.

- **An encrypted PDF stating an impossible key length panicked.** The
  `/Length` in an `/Encrypt` dictionary was used as the key size without a
  check: zero divided by zero in RC4, and anything over 128 bits read past
  the end of a digest. The value is now held to the 40 to 128 bits the RC4
  and AES-128 handlers define, so such a file is refused as needing a
  password, or opens if its key works at the nearest valid length.

- **A `/Matte` image whose soft mask was cut short panicked.** An image
  premultiplied against a matte colour is divided back out by its soft
  mask; when the mask's stream ended early the loop read past its last
  sample. Pixels with no mask sample are now left as they are. Found in a
  public PDF corpus.

## [0.8.4] — 2026-10-04

### Added

- **`rendering_intent` on the shading parameter structs**
  (`AxialShadingParams`, `RadialShadingParams`, `MeshShadingParams`,
  `PatchShadingParams`): the intent the shading's colours were converted
  with, for renderers that convert them again. Code that builds these
  structs with a struct literal must add the field (or use
  `..Default::default()`); as with `GraphicsState.overprint_mode` in
  0.8.3, the policy is that outside code reads these structs rather than
  writes them.
- **`color_lut_components` on `MeshShadingParams` and
  `PatchShadingParams`**: the shading's function sampled at the
  `color_lut` inputs as components of the shading's own colour space —
  gray, spot tints, CMYK — before conversion, for writers that emit the
  function again. Struct literals must add it (or use
  `..Default::default()`), under the same policy.
- **CMYK conversions that take a rendering intent:**
  `IccCache::convert_cmyk_with_intent`, `convert_cmyk_readonly_with_intent`
  and `DeviceColor::from_cmyk_icc_with_intent` in `stet-graphics`.
  `convert_color_readonly_with_intent`, `convert_color_with_intent` and
  `convert_image_8bit_with_intent` now honour the intent for CMYK profiles;
  the intent-less conversions are relative colorimetric, as before.
- **`--default-intent relative|perceptual|saturation|absolute`**: the
  rendering intent pages start with, for PostScript and PDF input, in
  place of relative colorimetric. A document that selects an intent — an
  `ri` operator, an ExtGState `/RI`, an image's `/Intent`,
  `setrenderingintent` — still gets it; this is a default, not an
  override. With most press profiles the intent picks a different CMYK
  table, so `--default-intent perceptual` gives the colours lcms2-based
  tools such as ImageMagick produce by default. In the library:
  `PdfDocument::set_default_rendering_intent` in `stet-pdf-reader`,
  `InterpreterBuilder::default_rendering_intent` in `stet`, and
  `Context::set_default_rendering_intent` / `Context::initial_gstate` in
  `stet-core`, taking the new `RenderingIntent` enum
  (`stet_graphics::rendering_intent`, re-exported by both).
- **`GraphicsState.icc_components`** in `stet-core`: the components of
  the current colour in an ICCBased space, kept so `setrenderingintent`
  can convert them again. Adding it breaks struct literals of
  `GraphicsState`, under the same reader-not-writer policy.
- **`FileEntry.global`, `FileEntry.created_after_save`,
  `FilterState.owns_source` and `VmMarks.files`** in `stet-core`: a file
  now belongs to local or global VM, so `restore` can close and reclaim
  the local files a save level opened, and a filter knows whether closing
  it closes its source. These are interpreter internals that happen to be
  public; adding the fields breaks struct literals of the four types,
  under the same reader-not-writer policy.
- **`SkiaDevice::set_bpc_mode` and `build_icc_cache_for_list_with_bpc`**
  in `stet-render`: the black-point compensation mode for colours the
  renderer converts, which `build_icc_cache_for_list` leaves at the
  default.
- **`Context::newerror`** in `stet-core`: whether a job that ended in
  `PsError::Stop` ended on an error or on `quit`, which also stops.
- **`ViewerOptions` and `run_viewer_with_options`** in `stet-viewer`:
  the viewer's settings as a struct, with the new `bpc_mode` among them —
  the black-point compensation of the colour cache the display lists were
  built with, which the viewer must convert images and overprint with too.
- **Render a page box, or a region, of a PDF page as the page:** `--box`
  on the command line and `PdfDocument::set_page_area` in
  `stet-pdf-reader`. `--box art` (or `media`, `crop`, `bleed`, `trim`)
  renders that box; `--box llx,lly,urx,ury` renders a rectangle in PDF
  points in the page's unrotated space. The area becomes the page — its
  size is the output size, `/Rotate` applies to it, and `--width`,
  `--height` and `--transparent` work on it — and only its pixels are
  rasterised, so cropping placed artwork from a large page no longer means
  rendering the whole page. A box the page does not declare is its crop
  box, and areas are clipped to the MediaBox rather than the crop box, so
  a bleed renders. For PDF input, with `--device png` or `--device pdf`.
  `PageArea`, `PdfDocument::page_area_rect` and
  `PdfError::EmptyPageArea` (an area that misses the page) are new.
- **`stet_graphics::image_samples`**: `to_8bit`, the nearest 8-bit value
  of a sample of any depth, and `ColorKey`, a colour-key mask tested on
  samples as encoded — what both front ends now use.
- **`transfer` on `ImageParams` and the shading parameter structs**
  (`AxialShadingParams`, `RadialShadingParams`, `MeshShadingParams`,
  `PatchShadingParams`): the transfer function in force, as `FillParams`
  and `StrokeParams` already carry it, and `TransferState::rgb_tables`,
  `apply_rgb`, `rgb_luts`, `apply_to_rgba` and `transfer_lookup` in
  `stet-graphics` to apply one. The renderer now applies the field (see
  Fixed); a third-party renderer should apply it to the final RGB of fully
  opaque paints. Struct literals must add the field, under the same
  reader-not-writer policy as `rendering_intent`.
- **`PdfDevice::in_memory`** in `stet-pdf`: a PDF device whose document is
  taken as bytes (`take_pdf_bytes_with_context`) rather than written to a
  file at the end of the job. It derives no output path from page names,
  and writes a `/Title` only when a pdfmark gives one.
- **Font copies for devices that read fonts after the page is sent**:
  `OutputDevice::keeps_text_fonts` and `Context::font_snapshots` in
  `stet-core` (module `font_snapshot`), `TextParams::font_snapshot` in
  `stet-graphics`. While the device asks, the interpreter copies each font
  text is shown with before any `restore` can reclaim it or revert glyphs
  added to it, and marks the `Text` element with its copy. The font's
  `font_entity` is valid only while the font lives.
- **`stet_ops::remove_pdf_authoring_ops`**: takes `pdfmark`,
  `setdistillerparams` and `currentdistillerparams` back out of
  `systemdict`, for a context that runs screen jobs after PDF ones.
  `register_pdf_authoring_ops` may now be called again; it adds nothing to
  the operator table the second time.
- **`OutputDevice::resize_page`** in `stet-core`: `setpagedevice` offers
  a new page size to the current device before replacing it with one from
  the device factory. A device that holds the job's pages until the end
  takes the size and returns true; the default, false, keeps the old
  behaviour. A new device from the factory is told its size the same way.
- **`PdfDevice::set_page_size_in_points`** in `stet-pdf`: the next page's
  size in points, for a display list drawn at exactly that size, as
  `stet_pdf_reader` renders a page. `set_page_size` takes whole device
  pixels, which rounds a page such as A4.
- **`Context::set_gstate_object`** in `stet-core`: replace a `gstate`
  object's state so that `restore` can revert it, backing up what the
  object held if it predates the current `save`. Write `gstate_store`
  through it, as dictionaries are written through `dict_put_cow`.

### Deprecated

- **`stet_viewer::run_viewer`**, for `run_viewer_with_options`. It has no
  way to take the black-point compensation mode, so it keeps the default;
  it will be removed in 0.9.0.

### Fixed

- **CMYK colours, images and shadings in PDF honour the rendering
  intent.** stet converted every CMYK source — DeviceCMYK and ICCBased
  alike — through its profile's relative colorimetric table whatever the
  document asked for, so `/Perceptual ri`, an ExtGState `/RI` or an
  image's `/Intent` changed nothing — and most press profiles (ISO
  Coated v2, FOGRA39, Japan Color 2001, SWOP) have perceptual tables that
  differ. Each intent now reads the table lcms2, and so Ghostscript, uses:
  `A2B1` for relative colorimetric, `A2B0` for perceptual, `A2B2` for
  saturation, with black-point compensation as lcms2 applies it
  (perceptual and saturation always; a profile with no `A2B2` gives
  saturation its `A2B0`, uncompensated). Absolute colorimetric renders as
  relative colorimetric, as before. Content that states no intent is
  relative colorimetric, as before. Each extra table is built the first
  time a page uses it. An `ri` after `k`/`K` applies to the colour
  already set, as it does after `sc`/`scn`. Overprint follows the intent
  of the element painting; transparency groups composited in CMYK stay
  relative colorimetric.

  This applies where the CMYK profile is a *source*: `--cmyk-profile`, the
  system profile, or an ICCBased CMYK colour. **In a PDF/X document the
  output intent's CMYK is the output**, so DeviceCMYK, and anything
  converted into the output intent, is shown the same way whatever the
  intent; there the intent governs the conversion into the output
  intent (ICCBased and Lab colours), as before. The Ghent Workgroup's
  output-intent tests (GWG 13.0, 22.1) depend on that.
- **CMYK colours, images and shadings in PostScript honour
  `setrenderingintent`** the same way. PostScript has no output intent, so
  the CMYK profile is always a source: `setcmykcolor`, colours in ICCBased,
  Indexed, Separation and DeviceN spaces that convert through a CMYK
  profile, shadings and images now take the current intent, and
  `setrenderingintent` converts the current colour again, as `ri` does in
  PDF and as Ghostscript does.
- **ICCBased CMYK colours in a PDF/X document ignored the rendering
  intent.** In a document with an output intent, an ICCBased CMYK colour,
  image or shading — FOGRA39 content in an ISO Coated v2 document, say —
  is converted into the output intent and shown through it, and that
  conversion is where the intent applies. stet made it with moxcms's
  perceptual transform whatever the content asked for, including at the
  default, relative colorimetric. It now reads the intent's tables on both
  sides as lcms2 does without black-point compensation — within 0.06 of
  255 ink levels on the press profiles in the corpus — so relative
  colorimetric content moves by 3–12 levels on average and up to 69, and
  saturation by about 30. A profile stet cannot read itself (one with an
  XYZ PCS) goes through moxcms's transform for the intent, about one ink
  level from lcms2. `IccCache::convert_color` and `convert_image_8bit`,
  which take no intent, now convert RGB and CMYK sources in a proofing
  chain relative colorimetric rather than perceptual. Overprint and CMYK
  blending still use an ICCBased CMYK colour's own numbers, as GWG 16.4
  expects.
- **ICCBased Gray colours in a PDF/X document ignored the rendering intent
  and black-point compensation.** moxcms could build the conversion into
  the output intent only as absolute colorimetric, so that is what every
  Gray colour got. Gray now converts as lcms2 does:
  - by its intent;
  - with black-point compensation when `--bpc` is on, the default, from
    the gray's black to the output intent's black, as lcms2 detects it
    for a destination (`cmsDetectDestinationBlackPoint`, which stet now
    implements);
  - into an ICC v4 output intent, compensated under perceptual and
    saturation whatever `--bpc` says, as lcms2 does.

  The Ghent Workgroup's 16-bit gray image test (GWG 18.3) made its
  reference that way. Its X, which showed at 3.4 sRGB levels on average and
  up to 36, is now at the 1-level noise of a test that compares an image
  with itself. With `--bpc off` it shows at about 6, which is lcms2's
  answer without compensation. Gray profiles with a LUT or a Lab PCS keep
  the old conversion; every Gray profile under an output intent in the
  test corpus is the usual tone-curve kind.
- **ICCBased RGB colours with a pure-gamma profile went wrong in a PDF/X
  document wherever a channel was zero.** Converting an RGB colour into
  the output intent runs it through its profile's tone curves. When a
  curve is a single gamma, as in Apple RGB, ColorMatch RGB or any other
  γ 1.8 profile, stet evaluated it with moxcms's approximate power
  function, which returns garbage at 0. Pure red or a saturated green then
  collapsed, up to 240 ink levels from lcms2 (42 on average) on the γ 1.8
  profiles GWG 13.0 embeds; no page in the test corpus happens to paint
  such a colour. Tone curves now evaluate as lcms2 does: a pure gamma
  exactly, a table with lcms2's 16-bit interpolation, and parametric
  curves by lcms2's own formulas. The table change moves a few hundred
  pixels of sRGB and eciRGB content in the corpus by one level.

  RGB profiles built on tables also follow lcms2 now: the table comes from
  the intent's tag, else `A2B0`, else the tone curves and matrix (rather
  than any table the profile has), and it is interpolated tetrahedrally
  rather than trilinearly. ICC v2 `lut8Type` tables are read too.
- **`--bpc` now reaches ICCBased RGB and CMYK colours, and Lab, in a PDF/X
  document.** Converting them into the output intent ignored black-point
  compensation, so `--bpc on`, the default, meant one thing for Gray and
  another for everything else. Ghostscript and Acrobat both compensate
  this conversion by default. It now compensates as lcms2 does: from the
  source's black, as lcms2 detects it, to the output intent's.
  - **Relative colorimetric RGB gets lighter, more open shadows:** about 6
    sRGB levels on average and up to 40, compressed into the press's
    range instead of clipped at its ink limit. This is the default intent,
    so it covers most PDF/X files made from RGB artwork. Tagged CMYK moves
    by 1.6–9 levels on average.
  - **Perceptual barely moves:** into an ICC v2 output intent lcms2 drops
    a compensation that small. Into an ICC v4 output intent it compensates
    perceptual and saturation whatever the flag, and so does stet.
  - **The Ghent Workgroup's X marks fade:** GWG 17.2's, which showed at 6
    levels, is gone, and GWG 16.1's are fainter. Every region the change
    moves comes closer to Ghostscript's output.
  - **What moves:** what is shown, the ink recorded for CMYK blending, and
    a Lab colour's ink (only its ink: Lab is shown without the output
    intent).
  - **Gray:** it now also skips a compensation too small for lcms2 to
    apply.
  - **Not compensated:** a CMYK profile stet cannot read itself (one with
    an XYZ PCS), which still goes through moxcms.
  - **`--bpc off`** keeps the previous output.
- **ICC v4 profiles built on `lutAToBType`/`lutBToAType` tables now convert
  into a PDF/X output intent as lcms2 does, with black-point
  compensation.** This covers such a profile as the source, such as the
  Ghent suite's estprofile.icc, and as the output intent. stet used to
  hand these tables to moxcms, and three things went wrong:
  - **Accuracy:** moxcms was up to 19 ink levels from lcms2 without
    compensation and 31 with it.
  - **Compensation:** none was applied.
  - **Black point:** a v4 press profile's relative colorimetric black
    point fell back to 400% ink.

  stet now evaluates the tables itself, within 0.12 of 255 ink levels of
  lcms2 on estprofile, including that black point. The black point is
  also what CMYK display through such a profile compensates from.

  moxcms 0.8.1 misreads a set of curves in these tables that holds an
  identity (empty) curve before another: every curve after it comes back
  as identity. stet reads them from the profile's bytes. Only GWG 20.5's
  image moves in the test corpus, by up to 5 levels, toward Ghostscript.
- **CMYK shown through an ICC v4 or 8-bit (`lut8Type`) profile now matches
  lcms2.** This covers such a profile as `--cmyk-profile`, as a PDF/X
  output intent, or as ICCBased CMYK. stet used to bake its display
  tables from moxcms, which was:
  - up to 3.5 levels from lcms2 (0.9 on average) through the Ghent
    suite's v4 estprofile.icc;
  - up to 14.7 levels (2.0 on average, 41% of colours more than 2 off)
    through an 8-bit profile built from FOGRA39;
  - wrong altogether on some v4 tables, 150 levels off on average on one
    of stet's test profiles;
  - unable to build a transform from a v4 table whose grid differs per
    input, so the profile's CMYK fell back to the uncalibrated PostScript
    formula.

  stet now bakes these tables through the evaluator its proofing chain
  already uses, within half a level of lcms2: the same as `lut16Type`
  profiles, which are unchanged. Nothing in the test corpus moves.
- **CMYK shown through a profile with an XYZ PCS now matches lcms2.**
  Such profiles include Ghostscript's own `ps_cmyk.icc` and
  `gray_to_k.icc`. stet read only Lab-PCS tables itself and left these to
  moxcms, which was up to 17 levels from lcms2 through `ps_cmyk.icc` and
  4.4 through the ICCBased CMYK in the test corpus's `5403.pdf`. They are
  now within half a level, black-point compensation included:
  - a black point that needs the profile's `B2A0` now uses it;
  - a three-input table's matrix is now applied, as lcms2 applies it.

  `5403.pdf` moves by up to 3 levels, toward Ghostscript.
- **Profiles with an XYZ PCS now convert into a PDF/X output intent as
  lcms2 does, and serve as one.** This covers CMYK, RGB (camera and
  scanner profiles are usually XYZ-based) and Gray sources, and Lab. They
  used to go through moxcms, with no black-point compensation.
- **Matrix RGB colours (sRGB, Adobe RGB and the like) in a PDF/X document
  convert into the output intent as lcms2 does.** stet took their XYZ to
  Lab with a D50 white 0.0003 off lcms2's in Z, which tinted every colour
  by a few hundredths of a b\*. In the test corpus, two pages of the PDFX
  output test and one GWG transparency page move by up to 4 levels, on
  0.1% of pixels at most.
- **An RGB profile with only some of its A2B tables, and no colorant
  matrix, is no longer compensated under an intent it has no table for.**
  This is common in camera profiles, which carry `A2B0` alone. lcms2
  treats such an intent as unsupported and its black as zero; stet took
  `A2B0`'s darkest colour instead.
- **RGB composited in CMYK, and RGB colour in a DeviceCMYK page group,
  convert to CMYK as lcms2 does.** Both take the colour stet displays back
  to the CMYK profile: the renderer's CMYK buffer, which CMYK blend modes,
  non-isolated CMYK groups and overprint simulation read, for a paint with
  no CMYK of its own; and a PDF page whose transparency group is
  DeviceCMYK, where an `rg` colour goes to CMYK and back. moxcms did it,
  with no black-point compensation, so with `--bpc` on (the default)
  black landed on 400% ink rather than the profile's black, and it was up
  to 150 ink levels wrong through ICC v4 and XYZ-PCS profiles. It now
  inverts stet's display into the profile's `B2A1`, compensated as `--bpc`
  says, within 0.1 ink levels of lcms2, and the page group shows that ink
  as DeviceCMYK is shown. In the test corpus, three PDFs with a DeviceCMYK
  page group move by up to 14 levels, their flat colours now Ghostscript's.
- **ICCBased RGB colours and images whose profile has A2B tables honour
  the rendering intent and black-point compensation, and display as lcms2
  does.** Scanner, camera and RGB printer profiles carry a table per
  intent. stet used the perceptual table whatever the document asked
  for, though PDF's default is relative colorimetric, and compensated
  nothing. It went through moxcms, which also misreads some ICC v4
  tables: up to 92 levels off. These profiles now go through stet's own
  evaluators, within a quarter of a level of lcms2, and each image pixel
  is exactly the colour of its value. Matrix-shaper profiles (sRGB,
  Adobe RGB, Display P3) are unchanged. In the test corpus, one scanner
  image in `2142.pdf` moves by a level.
- **16- and 12-bit image samples are rounded to 8 bits, not truncated.**
  stet carries 8 bits per sample and cut deeper ones to their high bits,
  so every 16-bit image, in PDF and PostScript, and every 12-bit
  PostScript image came out up to a level dark before conversion, up to 3
  after a CMYK profile. They now take the nearest 8-bit value, within a
  level of Ghostscript, which converts them at full precision. 16-bit PDF
  soft masks are rounded too. Indexed images keep their indices. In the
  test corpus, `issue6289.pdf` and three pages of the two PDFX output
  tests move, by up to 2 levels.
- **Colour-key masks mask the samples the file names.** A PDF image's
  `/Mask` array and PostScript ImageType 4's `MaskColor` give ranges of
  samples as encoded: at the image's own bit depth, before `/Decode`.
  stet tested them on the samples after expansion to 8 bits and
  `/Decode`, and:
  - masked the wrong pixels, or none, on a 1-, 2- or 4-bit image or one
    with a `/Decode`: a 4-bit key of 15 missed samples of 15, which stet
    held as 255;
  - kept the low byte of each value of a 16-bit PDF key, so
    `[32768 33023]` became `[0 255]` and masked every pixel;
  - scaled a PostScript key on an Indexed image as if its indices were
    colours;
  - wrapped a value past the depth: a key of 300 masked 44.

  Keys are now tested on the encoded samples. A PDF key also holds on a
  multi-input DeviceN image, under a transfer function, and on a gray
  image painted as K under a CMYK output intent, each of which changed
  the samples it was tested on. In the test corpus, page 4 of
  `image-qa.pdf` moves: its DeviceN images now cut out what Ghostscript
  does.
- **An image with a stencil `/Mask` painted its masked pixels under a
  transfer function.** The function was applied to the masked pixels
  too, so an inverting `/TR` painted them white.
- **Inline images are painted as image XObjects are.** They went through
  a second, thinner pipeline. It ignored `/Decode`, so an inverted
  inline image (`/D [1 0]`) painted inverted, and it also ignored
  `/Interpolate` and transfer functions. It approximated multi-input
  DeviceN through a sampled table, and painted all four inks for a
  K-only Indexed CMYK image under overprint. Both forms now share one
  pipeline. No inline image in the test corpus uses any of these, and
  nothing in it moves.
- **Multi-input DeviceN images honour `/Decode` and depths other than 8
  bits.** They were evaluated from the raw stream, read as 8-bit
  samples, before expansion and `/Decode`.
- **PostScript transfer functions are rendered.** `settransfer` and
  `setcolortransfer` were recorded for PDF output and otherwise ignored, so
  fills, images and shadings painted as if no function were set.
  `pmaster.ps` and `birthday.ps`, which set a tone curve, came out up to
  30 levels from Ghostscript; the pixels more than 8 levels from it fall
  from 5% to 1% and from 28% to 3.5%.
- **PDF transfer functions apply to images in every colour space, and to
  shadings.** The reader applied `/TR` to the samples of images with three
  or more components — right for DeviceRGB, wrong for ICCBased RGB and for
  CMYK, whose cyan, magenta and yellow took the red, green and blue
  functions — and to no Gray, Separation, Indexed or two-component image,
  nor to any shading. The renderer now applies the function to the final
  RGB of every paint, after colour management, as Ghostscript does. An
  image XObject drawn twice no longer keeps the function in force the
  first time.
- **Transfer functions apply only to fully opaque paints** (ISO 32000-1
  §11.7.5.2): not to a paint with alpha below 1, a blend mode other than
  Normal, or a soft mask, nor inside a transparency group drawn so, nor
  inside a soft mask's own mask. stet applied them to every paint, as
  poppler does; Ghostscript follows the spec except for the group rule.
- **PDF output no longer applies a document's transfer function twice.**
  The reader baked `/TR` into fill colours and also passed it on, so
  `--device pdf` from a PDF wrote both. Output now carries the function
  once, as `/TR2`, for images and shadings as well as fills and strokes,
  and resets it to `/Identity` when the function returns to identity —
  before, everything painted after it stayed under it.
- **Cached Type 3 glyphs take the transfer function in force where they
  are shown**, as they take its colour, rather than the one in force when
  the glyph was first built.
- **An image inside a PDF layer could be drawn as another image on the
  page.** The renderer's per-page image caches were indexed by position in
  the page's top-level list, and an image inside an optional-content group
  looked itself up by its position within the group, so it took whichever
  top-level image shared that position (CLI, viewer and WASM). The caches
  now nest like the layers. Images inside layers are also converted once
  per page rather than once per band: a 3000×3000 CMYK image in a layer at
  300 dpi went from 2.3 s and 1.5 GB to 1.3 s and 240 MB, the same as
  outside one. Images in layers that are hidden are no longer converted.
- **Small PostScript pages render as larger ones do.** The CLI's PNG
  output drew a page small enough to need no banding (Letter below about
  80 dpi, or a small EPS) onto what the previous page left behind, unlike
  every other page and unlike the library. Over a blank page, blend modes
  blended with white paper rather than a transparent page (ISO 32000-1
  §11.4.7: red under `/Difference` came out cyan); a page sent with
  `copypage` showed under the next one, which LanguageLevel 3 erases; and
  `flushpage` painted the page twice, doubling translucent paint. These
  pages now render banded, from a clear page. Anti-aliased edges on them
  can shift by a level or two, as on larger pages.
- **An EPS that calls `showpage` renders as one page in the library.**
  `Interpreter::render` and `render_to_display_list` add a `showpage` when
  an EPS leaves one out, and decided by asking the device whether it had
  received a page; the device renders in the background, so it often had
  not, and the EPS came back with a second, blank page — every time, once
  the page was large enough to band (a 600 pt EPS at 300 dpi). They now
  ask whether the EPS's `showpage` ran.
- **`copypage` follows LanguageLevel 3**: it passes `EndPage` reason code
  0, as `showpage` does, counts as a `showpage` execution, and calls
  `BeginPage` afterwards. It was passing the LanguageLevel 2 code, 1, and
  skipping `BeginPage`.
- **A page `EndPage` declines carries over.** When a job's `EndPage`
  returns false, the page is neither sent nor erased (PLRM 6.2.6), so the
  next page is drawn on top of it — how n-up imposition gathers pages. It
  was erased. `PageCount` now counts pages produced, as in Ghostscript, and
  numbers the output files; the count `BeginPage` and `EndPage` receive
  still counts every `showpage`.
- **`EndPage` runs with reason code 2 when the page device is
  deactivated**: at the end of a job and when `setpagedevice` replaces it.
  An `EndPage` that answers true there sends the page no `showpage` ended,
  such as the last, partly filled sheet of an n-up job. `setpagedevice`
  also erases marks made before it, as the PLRM and Ghostscript do; they
  used to carry onto the next page.
- **`--device pdf` no longer leaves an integer on the operand stack at
  every page.** Its `EndPage` consumed the reason code but not the count.
- **Erasing the page paints gray 1 through the transfer function**, as the
  PLRM defines `erasepage` and Ghostscript does: under an inverting
  function the page is erased to black. That covers `erasepage` and the
  erase `showpage` and `copypage` perform, so a job that sets an inverting
  function once (film negatives) gets black paper from page 2 on, as in
  Ghostscript; page 1 was erased before the function was set.
  `setpagedevice` reinitializes the transfer function before it erases, so
  its erase stays white. Pages whose transfer function leaves white white
  are unchanged.
- **The library renders each PostScript page once.** `Interpreter::render`
  and `render_to_display_list` interpreted onto a `SkiaDevice` that
  rasterised every page into a sink that threw the pixels away, only to
  learn the page's size, and then `render` rasterised the page again. The
  device installed while interpreting now records the size and nothing
  else: `render` takes about 40% less time per page at 300 dpi
  (0.25 s → 0.14 s on `tiger.ps`, `escher.ps`, `grade.ps`, `colorcir.ps`),
  with the same pixels. The WASM build did the same and is fixed the same
  way.
- **`Interpreter::render_to_pdf` returns the PDF and touches nothing
  else.** It finished the job as the command line does, writing the
  document to `output.pdf` in the working directory — overwriting any file
  of that name — and printing `PDF written: output.pdf` to stderr, then
  built the document a second time for the bytes it returned, whose
  `/Title` was "output" after that file. It now builds the document once,
  in memory, with no `/Title` unless a `/DOCINFO` pdfmark sets one.
- **An `Interpreter` used for PDF output no longer shows `pdfmark` to
  later screen renders, or fails after ~21 700 PDF renders.** Each
  `render_to_pdf` call registered `pdfmark` and the distiller-parameter
  operators afresh and left them in `systemdict`. A later `render` saw
  `systemdict /pdfmark known` as true, so a prologue that asks took its
  Distiller branch on screen too — FrameMaker's, for one, then paints its
  colours as RGB instead of CMYK. And the operator table grew by three per
  call until its 16-bit operator numbers wrapped, after 21 723 calls, and
  `pdfmark` ran another operator. They are now registered once and are in
  `systemdict` for PDF jobs only.
- **`setpagedevice` no longer keeps every page device for the life of the
  process.** It copied each page device dictionary into global VM, which
  `restore` does not reclaim, so each call left about 8 KB behind — from
  the command line too, and in every `render` call of an `Interpreter`,
  whose own device setup also ran outside the job's `save`. Page devices
  now live in local VM and go with the `restore` that ends their job, and
  the library sets up its device inside the job.
- **`PageCount` counts the pages a `restore` reaches back past.** After
  `save … setpagedevice … showpage restore`, `currentpagedevice` reported
  the count from before the page, since the count lived in the page device
  dictionary the `restore` reinstated. It is now the device's count, as in
  Ghostscript.
- **A file is closed when reading or executing it reaches its end**, as
  PLRM 3e `file` says and Ghostscript does. Only `closefile` closed one, so
  a file kept what it held for the life of the process: every job the
  library ran, which it copies into memory (an 864 KB job rendered 50 times
  kept 43 MB), every file `run` reads in whole, every font file loaded from
  disk, and the descriptor of every file read to the end. `read`,
  `readstring`, `readhexstring`, `readline`, `token` and executing a file
  now close it at end of file, and `status` then answers false — so
  `setfileposition` on a file read to its end is an `ioerror`, as in
  Ghostscript. A filter that reads its source to the end leaves the source
  open, as Ghostscript does, except ReusableStreamDecode, which reads it
  eagerly and closes it. `readstring` on a closed file answers `() false`
  rather than `ioerror`, like `read`.
- **`restore` closes the files made since its `save`**, as PLRM 3e `file`
  says, and reclaims them, so the file table no longer grows with every
  file a job opens — three for each library render call, and every file and
  filter a command-line job opened. A file not read to its end, such as a
  real file's descriptor or a filter's buffers, was kept until the process
  exited. Writes are flushed, as `closefile` would.
- **File objects are in local or global VM**, like other composites, by
  `currentglobal` when they are made. `gcheck` answered true for every file,
  while `def`, `put` and the other stores refused every file as local; now
  a global file can be stored in global VM and a local one cannot (PLRM
  3.7.2), and a `restore` that a local file made since its `save` would
  outlive on a stack is an `invalidrestore`. A filter over a local source is
  local even under `true setglobal`, and the standard files are global, as
  in Ghostscript. The job's own file is local (Ghostscript's is global), so
  `currentfile gcheck` answers false.
- **PDF output is reproducible.** The same job gives the same PDF, apart
  from its `/CreationDate`: a page's `/Font` resources were listed in an
  order that changed from run to run, and are now in their own order (`F0`,
  `F1`, …).
- **PDF output joins a line's strings into `TJ` runs in every size of a
  font and on every page.** Only a font's first instance joined — the same
  face at a second size placed each string with its own `Tm` and `Tj` — and
  only for the characters the font's first page used. Across 160 corpus
  files, PS → PDF now writes 94% fewer `Tm` operators (241,429 → 15,708),
  which keeps a line's text together for whatever extracts it and shrinks
  the content streams. The kerns between a run's strings now come from the
  same code as the font's `/Widths` (or `/W`), which a viewer advances by;
  they were computed from a copy that had drifted from it for re-encoded
  Type 1 instances, Type 42 codes without a glyph, and CIDFontType 2 fonts
  rebuilt from `GlyphDirectory`, where a kern could move the text after it.
- **A transparency group without a `/BBox` is bounded by the page.**
  `begintransparencygroup` with no `/BBox`, and no clip set, took the page
  size in points as its device-space bounds, so above 72 dpi the group drew
  only in the device's top-left corner — at 300 dpi, the first 24% of the
  page across and down — in every output. It now takes the page as
  `clippath` does.
- **PostScript → PDF writes the text inside transparency groups, soft
  masks, layers and pattern tiles.** Only a page's top-level text had its
  fonts tracked. Text inside a group, soft mask or layer in a font used
  nowhere else vanished from the PDF; characters used only there were
  missing from the font's `/Widths`, so viewers drew them on top of one
  another; and a page whose only text was in a group drew it twice — as
  glyph outlines and as text in a font its resources did not name. A
  pattern tile's fonts were tracked after the fonts were embedded, so a
  font used only in a tile was named but never written: pdftops output
  that fills glyphs with a pattern of text came out with the glyphs empty.
- **PostScript → PDF keeps every page of a job that calls `setpagedevice`
  more than once.** Each call replaced the PDF device, and with it every
  page so far, so only the pages after the last call reached the PDF.
  Ghostscript's `ps2write` calls `setpagedevice` before every page, and
  documents whose page sizes change call it between them: in a 120-file
  corpus sample, 6 PDFs were short — 4 of 12 `ps2write` files, one
  `pdftops` document 3 pages of 264. `Interpreter::render_to_pdf` lost
  them the same way. An EPS also lost the TrimBox of its bounding box if
  its program called `setpagedevice`.
- **PDF output writes each page's exact size.** The MediaBox was the page
  rounded to the device's pixels, so at the command line's 300 dpi an A4
  `[595 842]` page came out 594.96 × 841.92, and an EPS 556.56 square
  instead of its 556.49 `%%HiResBoundingBox`; only page sizes that are
  whole pixels, such as letter, were exact. PDF → PDF rounded each page to
  whole points instead — 595.276 × 841.89 became 595 × 842 — and moved its
  content by the difference. Content placement from PostScript is
  unchanged.
- **`restore` reverts `currentgstate`, and no longer leaves a `gstate`
  naming what it reclaimed.** `currentgstate` into a `gstate` made before a
  `save` kept the state from inside the save after the `restore`, with the
  fonts, colour spaces and page device the restore had reclaimed;
  reinstating it could panic the interpreter (`g setgstate
  currentpagedevice` after a `setpagedevice` inside the save). PLRM 3.7.3:
  `restore` undoes changes to such an object, as Ghostscript does.
- **`gstate` objects work with `copy`, `eq`, `==` and `pstack`.**
  `gstate1 gstate2 copy` raised `typecheck`; it now copies the state, which
  `restore` reverts like a `currentgstate`. A `gstate` was not `eq` to
  itself, `==` raised `undefined`, and `pstack` printed `--nostringval--`;
  they print `-gstate-`, as in Ghostscript.
- **The page device comes back with the graphics state, and `nulldevice`
  no longer ends the job's output.** PLRM 3e §6.1 makes the device part of
  the graphics state; stet restored only the page device dictionary.
  - `grestore`, `grestoreall`, `restore` and `setgstate` now reactivate the
    page device of the state they reinstate, with its page size: after
    `gsave … setpagedevice … grestore` the next page came out at the inner
    device's size, its marks off the page. Switching between two page
    devices runs the outgoing `EndPage` (reason 2) and the incoming
    `BeginPage` and erases the page; switching to or from the null device
    runs neither and leaves the page alone (PLRM Examples 6.1, 6.2). The
    calls match Ghostscript's, which differs in one case: reinstating
    another page device from under the null device, Ghostscript keeps
    printing at the old page size while reporting the new one; stet takes
    the new one.
  - `nulldevice` replaced the output device for the rest of the job, so
    after `gsave nulldevice … grestore` no page was written — PNG output
    lost every later page, PDF output wrote nothing. Marks made under the
    null device, such as a string `show`n to measure it, now go nowhere
    rather than onto the page.
  - `setpagedevice` back from the null device keeps counting pages, as in
    Ghostscript; it started again from 0.
- **PDF output keeps the function of a mesh or patch shading.** For a
  Type 4–7 shading whose colours come from a `/Function`, the writer wrote
  each vertex's function input as its colour and dropped the function. A
  gray or spot shading came out as a ramp of that input — a flat 0.8 gray
  as black to white, a soft mask that should pass everything as a radial
  ball — and in any other colour space the function's curve between
  vertices was lost, the colours written converted to RGB. The shading now
  names its function again, sampled in its own colour space, so a spot
  shading keeps its tints.
- **PDF output writes what a pattern tile contains.** A tile's resources
  were written from a partial copy of the page's, so a pattern whose cell
  held a transparency group, a soft mask, a layer or another pattern named
  resources it did not have: the group and the nested pattern drew
  nothing, the soft mask's content drew unmasked, an uncoloured nested
  pattern had no colour space, and the layer was missing from the
  document's layers. PDF → PDF lost the whole page of one corpus file,
  and charts and nested patterns in others; PostScript that nests
  patterns, as Ghostscript's `eps2write` does for nested PDF patterns,
  lost the inner one. Each form (group or soft-mask mask) now names its
  resources too, rather than relying on the PDF 1.1 rule that a form
  without them takes the page's, which inside a tile is not where they
  are.
- **A clip restored inside a group no longer unbalances the PDF.** The
  group's form continued the clip scope the page had open, so a clip set
  and restored inside it closed the page's `q` from inside the form;
  viewers reported "Restoring state when no valid states to pop".
- **A pattern painted inside a transparency group or soft mask is placed
  correctly in PDF output.** A pattern's matrix maps to the space of the
  content stream painting it — for a form, the form's — but every pattern
  was given the page's: in PostScript → PDF at 300 dpi a pattern filled
  inside a group came out at about a quarter of its size, and at 72 dpi
  flipped and shifted. A pattern used both on the page and inside a form is
  now written once for each, since viewers disagree on one object shared
  between the two (Ghostscript keeps its first placement, poppler places
  it per stream).
- **A soft mask inside a pattern tile renders wherever the tile lands.**
  A tile's contents are moved into device space by their transform, their
  paths left in pattern space, and the soft mask's raster was bounded by
  the raw paths — built where the cell sits in pattern space, not where it
  is drawn — so it masked everything out. A tile whose device origin was
  at x = 0 rendered nothing; elsewhere it happened to take another route
  and rendered. PostScript and PDF alike.
- **An uncoloured pattern's tile sets no colour in PDF output.** A
  PaintType 2 pattern takes its colour from where it is used, and its
  content may not set one (ISO 32000-1 § 8.7.3.3), but stet wrote each
  mark's captured colour into the tile — usually black. Ghostscript
  ignores it; poppler obeys it, so the pattern painted black, or in
  whatever colour the PostScript last set, rather than the colour it was
  filled with.
- **PostScript → PDF no longer crashes on fonts a `restore` reclaimed**
  (`--device pdf` and `Interpreter::render_to_pdf`). PDF output builds the
  document at the end of the job and read each font out of the
  interpreter's memory then, but a font defined inside a `save … restore`
  is gone by that point: much of Ghostscript's `eps2write` and `ps2write`
  output defines its fonts inside each page's `save`, and an EPS figure
  placed on a page brings its fonts inside its own. The job panicked, about
  2% of a PostScript corpus sample, in 0.8.x releases. A glyph a page added
  to a font inside its `save` (PLRM 3e §5.9.2) was silently missing from the
  PDF. The interpreter now copies each font as text is shown with it,
  before a `restore` can change it, and PDF output embeds the copies.
- **An EPS that calls `showpage` renders as one page** on the command line
  and in the library's PDF output. Both added a `showpage` of their own
  regardless, writing a blank second page (the `ps_samples` files
  `golfer.ps`, `printerarea.ps`, `testprinter.ps` and `tiger.ps` did).
- **Relative colorimetric CMYK rendered lighter than Ghostscript and
  Acrobat** with black-point compensation on, the default. Compensation
  maps the profile's black to sRGB black, and stet took that black to be
  400% ink. lcms2 — inside Ghostscript — takes an output profile's
  *ink-limited* black for relative colorimetric: Lab L\*=0 through the
  profile's perceptual `B2A0` table and back through `A2B1`, the darkest
  colour the press is allowed to make. That is lighter than 400% ink, so
  stet compensated too little: K=100 through Ghostscript's
  `default_cmyk.icc` — the CLI's system profile where Ghostscript is
  installed — came out RGB(44, 41, 42) where Acrobat and Ghostscript give
  (35, 31, 32), which stet now matches. On a 17⁴ grid of CMYK values stet
  is now 0.25–0.32 levels from lcms2 on average through ISO Coated v2 300%,
  FOGRA39, SWOP v2 and Japan Color 2001 (was 0.7–5.9, up to 24), and
  K=100 matches Ghostscript exactly through each. **Most CMYK content
  through a press profile renders a little darker**, in PostScript and
  PDF, including PDF/X documents through their output intent; profiles
  of input class, such as the library's embedded default, and profiles
  whose `B2A0` reaches 400% ink are unchanged. lcms2's other black-point
  rules come with it: an ICC v4 profile's perceptual and saturation
  intents compensate from the fixed v4 perceptual black, and an output
  profile with no perceptual table is not compensated under relative
  colorimetric.
- **`--bpc` did not reach PostScript images.** The PNG device converts
  DeviceCMYK images and overprinted colours as it renders, through a
  colour cache it builds for itself with the default options, so
  `--bpc off` changed fills and left images beside them compensated.
- **Inline images ignored the rendering intent**, always using relative
  colorimetric; they now take the current intent, or their own `/Intent`.
- **Images with an explicit `/Mask` skipped their ICC profile.** An
  ICCBased image with a stencil `/Mask` showed its raw samples (RGB) or
  went through the default CMYK profile instead of its own (CMYK); both
  now convert through their profile, with the image's intent.
- **CMYK profiles with ICC v4 `mAB` or 8-bit `lut8Type` tables** were
  converted through their perceptual table even for relative
  colorimetric, the default intent. They now use the colorimetric table,
  like every other profile, so their colours change.
- **Black-point compensation for CMYK is applied before the sRGB gamut
  clip**, as lcms2 does, rather than after. Colours outside sRGB, such as
  saturated cyans, move by up to 2 levels.
- **`PageBoxes` documented the wrong defaults.** It said an absent
  BleedBox, TrimBox or ArtBox falls back to the MediaBox; the PDF
  specification makes it the crop box. The values `page_boxes` returns
  are unchanged.
- **The viewer ignored `--bpc off` and `--no-icc` for images.** It
  converts DeviceCMYK and ICCBased images, and overprinted colours, as it
  renders, through a cache built with the default black-point
  compensation, so those images stayed compensated beside fills that were
  not — up to ~20 levels darker for K=100 through Ghostscript's default
  CMYK profile.
- **`Interpreter::render` ignored a reconfigured `context().icc_cache` for
  images.** It converted DeviceCMYK images and overprint through the
  embedded CMYK profile with the default black-point compensation, so a
  caller who set their own profile or compensation got images unlike the
  fills beside them. It now converts them as the context does. The default
  configuration renders the same as before.
- **A PostScript program ending in `quit` failed in the `stet` library.**
  `Interpreter::render`, `render_to_display_list`, `render_to_pdf` and
  `exec` returned "PostScript error: stop" and discarded the job's pages,
  because `quit` stops the job the way an error does. They now treat it as
  the clean end of the job, as the CLI always has; a real error is still
  an error.

## [0.8.3] — 2026-10-01

### Added

- **Transparent page backgrounds: `--transparent` for `--device png`,
  and `PageBackground` in the library.** Pages normally render onto white
  paper; with the option, every pixel no mark covers is left at alpha 0 and
  the output is straight-alpha RGBA, so artwork (EPS, AI, PDF) can be
  placed over other content with its unpainted areas clear. PostScript and
  PDF input both honour it, on every page. In the library,
  `InterpreterBuilder::page_background(PageBackground::Transparent)` makes
  `Interpreter::render` return clear pages, and `stet-render` adds
  `render_to_rgba_with_background` and `SkiaDevice::set_page_background`,
  and `stet-pdf-reader` adds `PdfDocument::render_page_to_rgba_with_background`
  (with `PageBackground` re-exported); `render_to_rgba`,
  `render_to_rgba_with_layers` and `render_page_to_rgba` keep their
  signatures and white paper. Contributed by @jungseohaan (#4).
- **`stet-fonts` writes CFF and converts Type 1 charstrings to Type 2.**
  `cff_writer` serialises a CID-keyed CFF font (and `read_cid_font` /
  `CidFont::subset` read and subset one); `type1_to_type2` converts a
  Type 1 charstring to Type 2, keeping stem hints, hint replacement (as
  `hintmask`) and flex; `cid_type0` reads the glyph data of a CIDFontType 0
  font with Type 1 charstrings. They are what PDF output of those fonts is
  built on.
- **`setoverprintmode` and `currentoverprintmode`,** the PostScript
  spelling of PDF's `/OPM`. They are Adobe extensions outside the PLRM,
  which Ghostscript also provides. With `true setoverprintmode` and
  overprint on, a DeviceCMYK paint leaves the plates whose component is 0
  untouched, so `0 1 0 0 setcmykcolor` over cyan gives blue rather than
  magenta, and `0 0 0 0 setcmykcolor` paints nothing, as in Ghostscript.
  pdftops output tests for the operator before calling it, so until now
  every `/OPM 1` in a PDF converted to PostScript was silently treated as
  mode 0. Converting PostScript to PDF now carries the mode into `/OPM`.
  As with `setoverprint`, images do not overprint yet.

### Changed

- **`stet_core::graphics_state::GraphicsState` has a new field,
  `overprint_mode`,** which `setoverprintmode` sets (see Added). Code that
  builds a `GraphicsState` with a struct literal no longer compiles; use
  `GraphicsState::new()` and set the fields you need. Reading the struct is
  unaffected. It is the interpreter's graphics state, public only because
  its module is, and will be marked `#[non_exhaustive]` in a later release
  like `PdfGraphicsState`.
- **The embedded URW++ base 35 fonts are distributed under the SIL Open
  Font License 1.1** instead of the GNU AGPL v3 with a font exception. In
  2017 URW++ licensed the Version 2.0 fonts under a choice of AGPL, LPPL
  1.3c or OFL 1.1; stet takes the OFL option. The fonts are unchanged. A
  program that links `stet`, `stet-pdf-reader` or `stet-wasm` no longer
  carries AGPL-licensed data: the OFL allows bundling the fonts with any
  software, commercial included, as long as they are not sold on their own
  and their licence goes with them. `LICENSE-URW-FONTS` in each of those
  crates, and `THIRD-PARTY-NOTICES.txt` in the release archives, carry the
  OFL text and the basis for it.

### Fixed

- **A bare `stet` now opens the viewer when a page is shown.** With no
  input file, stet starts the PostScript REPL as before, and the first
  `showpage` opens the viewer on the page drawn. `--help` and both READMEs
  described this behaviour, but a bare `stet` selected the `png` device,
  so `showpage` in the REPL wrote nothing and opened no window. A session
  that never shows a page still opens no window. Builds without the
  viewer are unchanged.
- **`-o` without `--device` now writes a PNG in viewer builds.** The
  `--help` examples `stet -o out.png --pages 1 doc.pdf` and
  `stet -o 'p-%03d.png' doc.pdf` failed with "--output does not apply to
  --device viewer", naming a device the command never asked for. `-o`
  now selects `png` unless a device is given, as headless builds
  already did.
- **The REPL banner shows the real version.** It printed
  `stet Version 0.1.0 (2026-02-25)` whatever the release, and
  `revisionstring` and `revisiondate` in `systemdict` held the same stale
  values, and the integer `revision` was always `1`. They now carry the
  release version and date; `revision` is the version with two digits
  each for minor and patch, so 0.8.2 is `802`.
- **`setoverprint` now works for spot colours in PostScript.** A
  Separation or DeviceN colour painted with overprint on knocked out the
  process colours beneath it, where PDF input with the same content
  overprinted correctly. Now only the colorants the colour space names are
  painted: a spot over cyan keeps the cyan, and `/Separation /Magenta` or
  a DeviceN of `[/Yellow …]` leaves the other process plates alone. This
  covers fills, strokes and text; images and `imagemask` do not overprint
  yet. DeviceCMYK still paints all four plates unless `setoverprintmode`
  selects mode 1 (see Added). Two related bugs went with it: a cached Type 3 glyph kept
  the overprint setting from when it was first drawn, and PostScript
  overprint turned off entirely on pages small enough to render in one
  piece, so the same file could overprint at 300 dpi and not at 72.

- **Text in CIDFontType 0 fonts built with `StartData` is drawn.** These
  CID fonts carry Type 1 charstrings reached through a CID map, and pdftops
  writes every CFF-based CID font this way, Chinese, Japanese and Korean
  text and subset OpenType fonts included. stet read the glyph data only to
  skip it, so `show` raised `invalidfont` and the text vanished: 161 files
  in the test corpus lost some or all of theirs. `show`, `stringwidth`,
  `charpath` and `xshow`/`yshow`/`xyshow` now draw them, in both writing
  modes, with each `FDArray` font's own `FontMatrix`, `lenIV` and
  subroutines. A CID the font has no glyph for shows CID 0, as the CIDFont
  specification requires, instead of an empty advance.

  Converting such PostScript to PDF embeds the fonts, subset to the CIDs
  used, as CFF (`/CIDFontType0C`, the only CIDFont program PDF has for
  them): each Type 1 charstring is converted to Type 2 with its hints,
  hint replacement and flex kept. CFF CID fonts loaded through
  `FontSetInit` are embedded the same way. Both used to fall back to an
  unembedded simple font, which garbled the text.
- **TrueType CID fonts find their glyphs through `CIDMap`.** A Type 2
  CIDFont maps each CID to a TrueType glyph through its `CIDMap` table
  (PLRM Table 5.17), which pdftops writes for every CID-keyed TrueType
  font it converts. stet used the CID itself as the glyph index, correct
  only when the map is the identity, so text in subset fonts came out as
  gibberish (22 files in the test corpus). PDF output did worse: it looked
  CIDs up in the font's TrueType `cmap` table, which these fonts do not
  have, so every glyph became `.notdef`. Both now read `CIDMap` in its
  string and array forms, and in the integer and dictionary forms
  Ghostscript also accepts; a CID the map does not define shows CID 0's
  glyph.
- **Vertical CID text follows the PLRM, and PDF output keeps it
  vertical.** A glyph gets its vertical (writing mode 1) metrics from the
  CIDFont's `Metrics2` or `CDevProc` (PLRM 5.9.2), which stet now
  supports; `CDevProc` may change widths in horizontal text too. A font
  with neither has one set of metrics, and the PLRM ignores the writing
  mode for it: its text runs horizontally, as in Ghostscript. stet instead
  gave every CIDFont default vertical metrics, and read `DW2`, which is a
  PDF key, not a PostScript one; pdftops' conversions of vertical text,
  whose fonts carry no vertical metrics, now render as Ghostscript renders
  them. PDF output wrote every CID font with a horizontal CMap, so
  vertical text ran across the page; it now uses `Identity-V` with `/W2`
  holding the metrics the interpreter used, and a CIDFont shown in both
  directions becomes one PDF font per direction.
- **Loading a CFF FontSet leaves the dictionary stack as it was, and
  defines the FontSet.** A FontSet file begins the `FontSetInit` ProcSet
  and has no `end` after its binary data (Adobe TN 5176, Appendix E), so
  `StartData` must end it, as Ghostscript's does; stet's left the ProcSet
  on the dictionary stack for the rest of the job. `StartData` also
  discarded the FontSet's name: it now defines the fonts as that FontSet
  resource, and `/NimbusRoman-Regular-CFF /FontSet findresource` finds
  stet's own FontSet, whose file named it differently.
- **Text in PDF output keeps its spacing.** Strings on one line are
  joined into a `TJ` run spaced by the glyph widths, which were looked up
  byte by byte instead of by 2-byte CID: when the page also used the CIDs
  matching those bytes (CID `0x0105` read as CIDs 1 and 5), the next
  string landed in the wrong place. `ashow` and `widthshow` spacing on CID
  text was dropped altogether. And in any font, each kern in a run was
  rounded to a thousandth of an em, so a line of separately placed glyphs,
  as pdftops writes, drifted by a fraction of a point by its end. All
  three now match the interpreter.
- **`definefont` no longer replaces the font a copy was made from.** It
  filed each font in `FontDirectory` under its `/FontName` rather than
  under the key it was defined with, which the PLRM requires. A re-encoded
  copy keeps its original's `FontName`, so defining one — pdftops does it
  for every font a page uses, `/F1_0` from `/Helvetica` — silently
  replaced the original, and a later `/Helvetica findfont` returned the
  copy with its encoding.
- **PDF output keeps each encoding of a font.** Two instances of one font
  with different encodings — pdftops writes ZapfDingbats re-encoded beside
  the standard one — shared one PDF font resource, whose encoding was
  merged code by code, first come first served, so the second instance
  drew the first one's glyphs. Each such instance now gets its own
  resource; instances whose encodings agree, like dvips's re-encoded
  copies, still share one, and the font program keeps every instance's
  glyphs.
- **Subset CFF CID fonts draw the right glyphs.** A CID-keyed CFF font
  loaded through `FontSetInit` had its glyphs looked up as if each CID
  were the glyph's index in the font, which holds only for a complete
  font; in a subset one, as producers embed them, text drew the wrong
  glyphs or none.
- **Type 1 glyphs that draw without an explicit `rmoveto` no longer
  sprout lines from the page corner.** The Type 1 format lets a glyph
  start drawing at the point `hsbw` sets, and `closepath`, unlike
  PostScript's, leaves the current point in place, so a following line
  needs no `rmoveto` either. stet began such a path with a line and no
  move, which the renderer drew from the device origin. This affects Type
  1 fonts in PostScript and in PDF alike; pdftops' conversions of CFF
  fonts rely on it.

- **Separation and DeviceN colorant names given as strings are now
  recognised.** The PLRM lets a colorant be a name or a string, and
  pdftops writes every one as a string (`(Black)`, `(PANTONE 273 C)`,
  `(All)`), but stet read any string as an empty name. Under overprint,
  process colorants were then taken for spots, so `(Black)` or `(Magenta)`
  knocked out the other plates, and a DeviceN backdrop of `(Black)` plus a
  spot lost its black. The GWG 3.0 gray-overprint test page converted with
  pdftops now matches Ghostscript. PDF output carries the real spot names
  rather than empty ones. A colorant that is neither a name nor a string is
  now a `typecheck`, as in Ghostscript, instead of being accepted silently.

- **`initgraphics` and `showpage` no longer reset the whole graphics
  state.** The PLRM has `initgraphics` reset the transformation matrix,
  path, clip, colour and line settings only, and leave everything else
  alone: stroke adjustment, the font, and all the device-dependent
  parameters (overprint, flatness, smoothness, transfer, halftone, black
  generation and undercolor removal). stet reset all of them, and since
  `showpage` performs an `initgraphics`, a program that set overprint or a
  transfer function once lost it from the second page on. Ghostscript
  keeps them. Two smaller fixes go with it: `setpagedevice` now keeps the
  current font, as the PLRM requires, and a clip set after `initgraphics`
  inside a `gsave` is now undone by `grestore`, which could previously
  leave the inner clip in force.

- **`stet-cli`'s crates.io page no longer shows a failing docs.rs badge.**
  docs.rs documents libraries only, and `stet-cli` is a binary, so its
  build has failed for every release. The badge is gone and the
  crate's Documentation link now leads to the command-line usage.

## [0.8.2] — 2026-09-24

### Added

- **`DisplayElement::TextRun`, a display-list element for text
  extraction,** with `TextRunParams`, `ShownGlyph` and `UnicodeSource` in
  `stet_graphics::device` (re-exported by the `stet` facade). A run is a
  stretch of shown text in one font along one baseline, however many show
  operations displayed it, in Unicode, with each glyph's device-space
  origin and advance, its character code, and where its text came from;
  `glyph_to_device`, `ascent` and `descent` give each glyph's box, rotated
  or skewed like the text, `start` and `end` span the run, `vertical` marks
  vertical writing, and `word_breaks` marks where words were set apart by
  distance rather than by a space character, as TeX sets them. The rule
  that cuts runs and finds word gaps is `TextRunParams::step_to`, in the
  new `stet_graphics::text` module, which both producers share. It paints
  nothing, and every renderer and the PDF writer skip it. Both producers
  take a `TextExtraction` level, `Off` by default —
  `InterpreterBuilder::text_extraction` (or `Context::text_extraction`) for
  PostScript and `PdfDocument::set_text_extraction` for PDF. With it off,
  display lists are unchanged; `Glyphs` records runs with every glyph, and
  `Runs` the same runs with `glyphs` left empty, in about half the memory,
  for callers that want a page's text and lines but not each glyph's
  place. Both record runs (below). Renderers that match on
  `DisplayElement` already have the wildcard arm the enum's
  `#[non_exhaustive]` requires.
- **Text extraction from PDF.** With `PdfDocument::set_text_extraction`,
  the text-showing operators (`Tj`, `TJ`, `'`, `"`) record `TextRun`s
  beside the glyphs they draw — in forms, annotation appearances, layers and
  transparency groups, nested like the content, so a viewer extracts only
  visible layers' text with the `LayerSet` it renders with. Text inside an
  `/ActualText` marked-content span is the span's text — the author's
  statement of what ligatures, symbols or drawn characters say, or, when
  empty, that a glyph such as a line-end hyphen says nothing: the span's
  first glyph carries it all and the rest none, the outermost span wins,
  and forms drawn inside the span are covered by it. Outside a span, a
  glyph's text comes from the font's `/ToUnicode` CMap, else its glyph
  name through the Adobe Glyph List, else — for CJK fonts on Adobe's Japan1, CNS1, GB1 and
  Korea1 collections, or with a `Uni…` encoding CMap — its CID or code;
  failing all of those it is left empty. One rule is a heuristic, taken
  from Poppler so TeX documents made with dvips extract: a glyph name
  outside the AGL that spells a number (`a80`, `g65`) is read as that
  character code in Latin-1. Invisible text
  (render modes 3 and 7, as OCR layers use) is recorded and flagged. Text
  that is not the document's is not recorded: text drawn inside a Type 3
  glyph procedure (the glyph is the text), in a tiling pattern cell, or in
  a soft-mask group.
- **Text extraction from PostScript.** With
  `InterpreterBuilder::text_extraction`, every show operator — `show`,
  `ashow`, `widthshow`, `awidthshow`, `kshow`, `xshow`, `yshow`, `xyshow`
  and `glyphshow` — records `TextRun`s for Type 1, CFF, Type 42
  and Type 3 fonts, and for composite fonts: CID-keyed (CFF and
  TrueType, horizontal and vertical) and FMapType, a new run starting
  wherever the descendant font changes. Text comes from each glyph's name
  through the Adobe Glyph List (with the same dvips numeric-name rule as
  PDF); for a CIDFont, from the character code when the CMap is a `Uni…`
  one, else from the CID through Adobe's Japan1, CNS1, GB1 or Korea1
  table. PostScript has no ToUnicode, so a font whose glyph names mean
  nothing gives empty text. Glyph space is the font's own, with ascent and
  descent from its `FontBBox` (a TrueType font's `hhea` table; for a
  Type 3 font with an empty `FontBBox`, as dvips writes, its glyphs'
  `setcachedevice` boxes). Text drawn inside a
  Type 3 `BuildChar` / `BuildGlyph` or a pattern cell is not recorded; a
  `show` inside a `kshow` procedure carries on the kshow's run, and one
  inside a `cshow` procedure records the whole character
  code `cshow` selected (its procedure sees only the last byte). Forms
  record once and are placed wherever `execform` draws them. A
  `glyphshow` glyph, shown by name, records code 0.
- **`stet text <file>`**, a CLI subcommand that prints the text a PDF,
  PostScript or EPS file shows, a line at a time in the order the file
  draws it, each page ending with a form feed. TeX's placed words come out
  separated, invisible OCR text is included, and layers hidden by default
  are left out. `--json` prints JSON with each line's position in points
  from the page's top-left corner, and `--word-boxes` adds each word's;
  `-o` / `--output` writes every selected page to one file instead of
  stdout, and `--pages` and `--password` work as for rendering.
- **Assembling extracted text into words and lines**, in
  `stet_graphics::text` and re-exported by the `stet` facade (with
  `LayerSet`). `text_runs` collects a page's `TextRun`s in content order,
  skipping layers a `LayerSet` hides; `text_lines` joins them into lines —
  superscripts and changes of font stay in their line and word — and
  splits the lines into words at shown spaces and at gaps, which is how
  TeX output ("PaperTitle", drawn as two placed words) reads as "Paper
  Title". It gives the same text at both extraction levels, with line
  boxes at both and word boxes when glyphs were recorded. It is
  deliberately simple: content order is kept, and columns, tables and
  reading order are not detected.
- **`PdfDocument::set_render_annotations(bool)`**, to render pages without
  their annotations' appearances — for an application that draws
  annotations itself as editable objects, where the baked-in appearances
  would show twice. On by default; `page_annotations` still reads them.
- **`DisplayList::remove`**, to take an element out of a display list.
- **`Debug` for `DisplayList` and `DisplayElement`** (and the param structs
  that lacked it: `PatternFillParams`, `GroupParams`, `SoftMaskParams`), so
  a display list can be printed or compared as text.
- **Unicode tables for text extraction in `stet-fonts`**, the groundwork for
  extracting text from both PostScript and PDF input:
  - `stet_fonts::to_unicode::ToUnicodeMap` parses a PDF `/ToUnicode` CMap
    without losing text: surrogate pairs decode to one supplementary-plane
    character, multi-character destinations stay whole (`<00660069>` is
    `fi`, not U+FB01), and codes up to four bytes stay distinct. The PDF
    reader's rendering-side parser, which keeps one BMP code point per code
    for glyph selection, is unchanged.
  - `stet_fonts::agl::glyph_name_to_text` resolves a glyph name to text by
    the Adobe Glyph List specification's full algorithm over the complete
    AGL (4,281 names, against the 542 plus letters the renderer's table
    carries) — `a.sc` → `a`, `f_f_i` → `ffi`, `uni00660069` → `fi`,
    `u1D400` → 𝐀, `afii10017` → А — and returns nothing for names that
    carry no text, such as `g123`. `zapf_dingbats_glyph_name_to_text` does
    the same for the ZapfDingbats font (`a20` → ✔). The lists are Adobe's
    files, embedded unmodified with their BSD-3-Clause notice
    (`crates/stet-fonts/LICENSE-ADOBE-AGL`). `glyph_name_to_unicode`, which
    glyph selection uses, is unchanged.
  - `stet_fonts::cid_unicode`, the CID ↔ Unicode tables for Adobe-Japan1,
    CNS1, GB1 and Korea1, moved here from `stet-pdf-reader` so the
    PostScript interpreter can use them too, with a new `cid_to_text` that
    gives the text Adobe assigns each CID — every CID in the collection,
    including supplementary-plane characters and variation sequences.

### Changed

- **`stet_pdf_reader::content::graphics_state::PdfGraphicsState` has two new
  fields, `fill_color_source` and `stroke_color_source`,** which the reader
  uses to re-convert colours when the rendering intent changes before
  painting (see Fixed). Code that builds a `PdfGraphicsState` with a struct
  literal no longer compiles; use `PdfGraphicsState::new(ctm)` and set the
  fields you need. Reading the struct is unaffected. This is interpreter
  state that is public only because its module is; a later release will mark
  it `#[non_exhaustive]` so outside code cannot construct it directly.
- **`stet_core::glyph_cache::CachedType3Glyph` has a new field, `bbox`,**
  the glyph's `setcachedevice` box, which text extraction needs for Type 3
  fonts with an empty `FontBBox` (see "Text extraction from PostScript"
  under Added). Code that builds a `CachedType3Glyph` with a struct literal no longer compiles; add
  `bbox: None`. It is the interpreter's glyph-cache entry, public only
  because its module is, and will be marked `#[non_exhaustive]` in a later
  release like `PdfGraphicsState`.

### Deprecated

- **`stet_pdf_reader::content::cid_unicode::{cid_to_unicode,
  unicode_to_cid}`.** The tables moved to `stet_fonts::cid_unicode`; the old
  functions forward there and give the same results.
- **`stet_graphics::icc::intent_from_pdf_byte`.** It decoded the PDF
  reader's former private intent numbering, which display lists no longer
  carry. Use `stet_graphics::icc::intent_from_byte`, which decodes the
  documented encoding, and the constants in the new
  `stet_graphics::rendering_intent` module.

### Removed

- **`stet-wasm`: the `set_page_callback()` / `clear_page_callback()` JS
  exports.** These registered a callback for streaming rendered bands out of
  WASM memory. The only sink that ever invoked it, `MemorySink`, stopped
  being constructed when the browser viewer moved to on-demand viewport
  rasterization — the change that fixed a 4.6 GB OOM on a 139-page document
  at 300 DPI. Registering a callback has been a silent no-op ever since, so
  the exports and the dead sink are removed rather than left looking
  functional. Nothing in the bundled frontend called them.

### Fixed

- **An error inside a `cshow` procedure no longer changes the next glyph
  shown.** With a CID-keyed font, `cshow` hands its procedure the
  character's code and sets aside the CID it selected for a `show` inside
  the procedure to draw. A procedure that failed — caught by `stopped` —
  left that CID set aside, so the next `show` anywhere drew it in place of
  its own first glyph.
- **PostScript vertical CJK text (WMode 1) is drawn where PLRM puts it.**
  A vertical glyph's outline is drawn from its origin 0, the current point
  less its position vector v (half its width across, 880 units up by
  default); stet drew it from the current point, so every vertical glyph
  sat half a character right and most of a character high. That held for
  `show`, the `xshow` family and `charpath`. Separately, a TrueType
  CIDFont's default vertical advance and position vector — defined in
  1000-unit text space — were applied in the font's own units, so a font
  with 2048 units per em advanced less than half an em per glyph, in
  `show`, `charpath` and `stringwidth` alike. The PDF reader was already
  right.
- **A PDF form XObject whose content fails to parse part-way no longer
  corrupts the rest of the page.** An error such as an unterminated string
  inside an array made the reader return from the form before restoring
  what it had saved, so everything after it on the page drew under the
  form's transformation and resources — misplaced, or in a fallback font.
  A form with a transparency group also left its group in place of the
  page's display list and its nesting level counted, so after twenty such
  forms no further form, pattern or Type 3 glyph was drawn. The form now
  keeps what it drew before the error and the page carries on as it was.
- **Text in a PostScript CMYK transparency group no longer changes how the
  group blends.** A non-isolated `/CS /DeviceCMYK` group with Difference,
  Exclusion or a non-separable blend mode composites in CMYK when all it
  paints is CMYK. The check treated the text element that `show` records
  as non-CMYK content, so one line of text anywhere in the group switched
  the whole group to sRGB blending and changed the colour of everything in
  it. PDF input was unaffected.
- **Forms drawn with `execform` keep their transparency groups, soft masks
  and layers.** `execform` caches a form's output and replays it at each
  use, and the replay discarded groups, soft masks and layers as if only
  PDF input could produce them; stet's PostScript transparency operators
  (`begintransparencygroup`, `beginsoftmask`, `beginoptionalcontent`) make
  them too, so a form built with them drew nothing. Their bounding boxes are
  fixed in device space when they are made, which a replay from form space
  cannot correct, so such a form is now executed afresh at each use instead
  of being replayed.
- **PDF output keeps every glyph of a Type 3 font that wraps another
  font.** When a Type 3 glyph procedure draws its glyph with `show` (effect
  fonts that outline or shadow a real font do), the glyph cache replayed
  each later use of a glyph without moving the text it had recorded. PDF
  output draws that text rather than the glyph outlines, so every repeat of
  a character landed on its first occurrence: `AAAA` came out as one `A`.
  Raster output was unaffected.
- **The `stet` facade no longer leaves released objects on its stacks at the
  end of a job.** A program that ended with composite objects on the operand
  stack, dictionaries it created still on the dictionary stack, or an
  unfinished loop — all common in real PostScript — had them discarded only
  after the `restore` that released them. Release builds recovered, but in a
  debug build the dangling-reference audit panicked, so a library user's
  test suite failed on such a program. `render`, `render_to_display_list`,
  `render_to_pdf` and `exec` now discard the job's stacks first, as the CLI
  always has. The WASM viewer had the same order and is fixed too.
- **The licences of third-party material stet ships are now shipped with
  it.** The `stet`, `stet-pdf-reader` and `stet-wasm` crates embed the
  URW++ base 35 fonts, which are under the GNU AGPL v3 with a font
  exception, not stet's Apache-2.0 OR MIT; each crate now carries that
  licence as `LICENSE-URW-FONTS`. The prebuilt release archives, which
  shipped only stet's own licence files, now include
  `THIRD-PARTY-NOTICES.txt`: the font licence, Adobe's terms for the CMap
  and glyph-list data in `stet-fonts`, and the licence of every Rust crate
  linked into that binary, generated per target by `cargo about`
  (`scripts/gen-third-party-notices.sh`). CI fails a change that adds a
  dependency under a licence not accepted in `about.toml`.
- **CJK text drawn with a substitute font picks the right glyphs.** When a
  PDF uses a CJK CID font it does not embed, the reader maps each CID to
  Unicode to find a glyph in the substitute font. The table it used was
  built by inverting Adobe's Unicode → CID CMaps, which kept an arbitrary
  one of the code points that share a CID and dropped every CID that no
  Unicode encoding reaches, or that lies outside the Basic Multilingual
  Plane. Visible effects:
  - Some common ideographs came out as the Kangxi radical sharing their CID
    (Japan1 CID 3284, 日, as ⽇ U+2F47), which substitute fonts often lack.
  - Proportional, half-width and other variant glyphs, and characters such
    as 𠮷, drew nothing — about 6,600 CIDs in Adobe-Japan1 alone.

  The tables are now generated reproducibly
  (`scripts/gen_cid_unicode_tables.py`) from Adobe's CID → Unicode CMaps
  for text and its Unicode → CID CMaps for glyph selection, with Adobe's
  BSD-3-Clause notice alongside in `crates/stet-fonts/LICENSE-ADOBE-CMAP`.
  The PDF reader's width lookup for UCS2-encoded fonts, which maps Unicode
  back to a CID, also found no CID for such characters and now does.
- **Rendering intents are no longer scrambled between the PDF reader and
  everything downstream of it.** The display list's `rendering_intent` byte
  is documented as 0=RelativeColorimetric, 1=Absolute, 2=Perceptual,
  3=Saturation, but the PDF reader wrote its own numbering (0=Perceptual,
  1=RelativeColorimetric, 2=Saturation, 3=Absolute) and the rasterizer
  decoded with the reader's. Visible effects:
  - `--device pdf` on PDF input rewrote every explicit intent as a different
    one: `/RelativeColorimetric` became `/AbsoluteColorimetric`,
    `/Saturation` became `/Perceptual`, and so on.
  - PostScript images in ICC-based colour spaces were converted with the
    wrong intent; the PostScript default, RelativeColorimetric, was applied
    as Perceptual.
  - Third-party renderers that followed `docs/DISPLAY-LIST.md` misread every
    display list built from PDF input.

  All producers and consumers now share `stet_graphics::rendering_intent`.
  `stet_pdf_reader::content::color_space::components_to_device_color_icc_with_intent`
  and `PdfGraphicsState::rendering_intent` use the same encoding.
- **The rendering intent in effect when a shape is painted now applies**, as
  the PDF specification requires, rather than the one in effect when its
  colour was set. A content stream that sets a colour and then selects an
  intent (`60 0 0 sc /Perceptual ri … f`) was converted with the earlier
  intent. Shadings painted with `sh` ignored the intent entirely, and shading
  patterns used the intent current when the pattern was selected. Only
  documents with an output intent are affected; the GWG 22.1 output-intent
  test is built this way.
- **PDF content that selects no rendering intent now uses
  RelativeColorimetric**, the initial value the PDF specification gives,
  instead of Perceptual; so does an unrecognised intent name. This changes
  rendered colour only for RGB, gray and Lab ICC-based colour in documents
  with an output intent, where stet builds a separate conversion chain per
  intent; other colour is converted as before. PDF-to-PDF rewrites no
  longer add a rendering intent the source never selected.
- **PostScript images now honour `setrenderingintent`.** Sampled images and
  rasterized shadings carried a fixed intent whatever the graphics state
  said.

## [0.8.1] — 2026-08-31

A malformed-font crash fix. Font data arrives embedded in a PDF or a
PostScript program, so it is attacker-controlled in the same way any PDF
object is, and the affected code shipped in every release to date.

### Fixed

- **A malformed CFF font no longer panics the process.** The Private DICT
  operator carries `[size, offset]`, and both were bounds-checked with
  `offset + size <= data.len()`. CFF permits a real-number operand wherever an
  integer is expected, and `f64 as usize` saturates rather than wrapping, so a
  font declaring a size of `1e49` reached that check as `usize::MAX` and
  overflowed the add before it could reject anything. **Shipped release builds
  are affected**, not only overflow-checked ones: with checks off the add wraps
  to just below the offset, the `<= data.len()` bound then *passes*, and the
  slice panics instead. Three sibling sites had the same shape: the per-FD
  Private DICT of a CID-keyed font, and both local-Subr offsets, which are
  added to their Private DICT offset. DICT operands destined for an offset or
  length are now rejected up front unless they are finite, non-negative, and
  inside what a CFF offset can address.

  Font data is attacker-controlled in the same way any PDF object is — it
  arrives embedded in a PDF or in a PostScript program — so this was reachable
  from an untrusted input. Found by the weekly fuzz job.

### Changed

- Crate descriptions and keywords across all eleven published crates now name
  what distinguishes stet — pure Rust, no C dependencies, prepress-grade CMYK
  and spot colour — rather than restating the crate name. `stet-pdf-reader`
  was previously described as "PDF parser and renderer", five words that fit
  every PDF crate on the registry.
- The `stet` facade no longer describes itself as a "PDF rendering engine". It
  depends on `stet-render` and `stet-pdf` (PDF *output*) and carries
  `stet-pdf-reader` as a dev-dependency only, so it cannot read a PDF; the old
  wording promised the one thing the crate does not do. PDF reading is
  `stet-pdf-reader`.

## [0.8.0] — 2026-08-30

The release you can download. Every release before this one shipped source
only, so trying stet meant installing a Rust toolchain and compiling a
workspace including a GUI stack — a barrier for exactly the people most
likely to want it, who are replacing a Ghostscript call in a pipeline. This
one attaches prebuilt binaries for Linux, macOS and Windows, and adds the
`-o` flag such a pipeline needs to say where output goes.

### Added

- **Prebuilt binaries on every release.** Linux (static musl and glibc+viewer),
  macOS (Apple Silicon and Intel) and Windows, with a `SHA256SUMS` file.

  The **static musl build is the one most people want**: no glibc version
  requirement, no runtime libraries, no GUI, and nothing to install alongside
  it — all 56 resources are embedded in the binary. The glibc build adds the
  interactive viewer and needs glibc 2.35 or newer.

  The binaries are unsigned, and the release notes say so rather than letting
  you find out: macOS Gatekeeper quarantines browser downloads (install with
  `curl` instead), and Windows SmartScreen warns about an unknown publisher.

- **`-o` / `--output` for the CLI.** Output no longer has to land next to the
  input. The path is a *template*: a `%d` in it is replaced by the page number
  and `%0Nd` zero-pads (`p-%03d.png` gives `p-001.png`), while a path without
  a token names a single file, written exactly as given with no extension
  mangling. This is the shape `gs -sOutputFile=` uses, so an existing
  shell-out pipeline can point at stet without restructuring.

  Naming is decided by the template rather than by the page count, which is
  what makes PostScript and PDF input behave identically — a PostScript page
  count is not knowable in advance, since pages appear as `showpage` runs.
  Where Ghostscript resolves that by opening the literal path once and
  streaming every page into it (leaving several concatenated images in one
  file, exit 0, no warning), stet **stops with an error on the second page**,
  names a `%03d` form to use, and leaves page 1 intact. For PDF input the page
  count is known up front, so the same mistake is refused before anything is
  rendered.

  `-o` takes one input file, and is rejected for `--device viewer` and
  `--device null`, which write no file. `--device pdf` collects every page
  into one file, so a `%d` token there is an error rather than being ignored.
  Writing to stdout (`-o -`) is not implemented and says so.

  Default naming without `-o` is unchanged: `in-0001.png` for PostScript,
  `doc.png` / `doc-001.png` for PDF.

### Fixed

- **`stet-cli` did not build with `--no-default-features`.** The `viewer`
  feature was not genuinely optional: `render_dropped_pdf` names viewer
  channel types in its signature but carried no `#[cfg]`, so a headless build
  failed to compile even though both of its callers sit inside the
  viewer-gated `run_viewer_mode`. This is the configuration a server, CI or
  container install wants, and the one a static musl binary requires.

- **A headless build's `--help` advertised a viewer it does not have.** It
  listed `--device viewer` — which exits with "viewer not available" — and
  claimed a bare `stet` launches the viewer, when it starts the REPL. Both
  lines are now conditional on the feature.

- **A job that requested a non-zero exit status was reported as having
  completed.** `.quitwithcode 1` and the new `--output` failure both end the
  job through `quit`, which printed "completed (quit)" regardless of the code
  requested. A non-zero code now prints "FAILED". The process exit status was
  already correct.

## [0.7.0] — 2026-08-30

A dependency-hygiene release. A PDF-only consumer no longer compiles the
PostScript interpreter, a mesh-shading memory fault that scaled with core
count is fixed, and `--device null` works for the scripting use it is
documented for.

### Breaking

One change, affecting only consumers who already opt out of `stet-render`'s
default features. This is why the bump is minor rather than patch.

- **`stet-render` with `default-features = false` no longer provides
  `SkiaDevice` or its `OutputDevice` implementation.** They now sit behind the
  new default-on `ps-device` feature. At 0.6.0, opting out of defaults dropped
  only `parallel`; it now drops the device as well, so a consumer who declared
  `stet-render = { version = "0.6", default-features = false }` to get a
  single-threaded build will fail to compile against 0.7. Add the features you
  want by name:

  ```toml
  stet-render = { version = "0.7", default-features = false, features = ["ps-device"] }
  ```

  Nothing changes for consumers on default features, which is the overwhelming
  majority: `default = ["parallel", "ps-device"]`.

### Changed

- **`stet-pdf-reader` no longer links the PostScript interpreter.** Its
  default features pulled `stet-render`, which depended unconditionally on
  `stet-core` for a single trait, so a PDF-only consumer compiled the whole
  PostScript VM — contradicting the README's "PDF-only users don't pay for the
  VM". `stet-render` now gates `SkiaDevice` and its `OutputDevice`
  implementation behind a new default-on `ps-device` feature, and the reader
  takes the crate without it. `stet-pdf-reader`'s dependency closure is now
  `stet-fonts`, `stet-graphics`, `stet-render` and the two tiny-skia forks,
  with PDF → RGBA rendering and parallelism unchanged.

  Consumers on default features are unaffected; see Breaking above for the
  one case that is not.

### Fixed

- **`--device null` aborted PostScript programs that query the page device.**
  The flag is documented for "test / scripting use", but it installed the PLRM
  `nulldevice` *operator* rather than a page device. That resets the CTM to
  identity and leaves no page-device parameters, so `currentpagedevice
  /OutputDevice get` raised `undefined` and `initmatrix` had no device matrix
  to restore — stet's own PostScript test suite aborted partway through under
  the flag meant for running it. `--device null` now installs a real page
  device (a new `OutputDevice/null.ps` resource) that reports itself as
  `/null`, carries a page size and resolution like any other device, and still
  produces no output: nothing is rasterized and `/EndPage` never transmits a
  page. The suite now passes identically on `png`, `pdf` and `null`, and CI
  runs it on `null` as well so this cannot regress unnoticed.

- **Mesh-shaded PDFs could exhaust memory and fail to render, worse the more
  cores the machine had.** `render_patch_shading` triangulated every patch of
  a shading regardless of which band was being drawn, and bands render
  concurrently — one per thread — so the same triangle list was built once per
  core. Cost scaled with core count rather than with the file. A prepress PDF
  with a page-spanning Coons mesh at 300 dpi peaked at 2.1 GB on one thread and
  31.4 GB on sixteen, and on a 24-core machine with less than ~50 GB of RAM it
  did not render at all. Patches and triangles that cannot reach the band being
  drawn are now skipped before they are built. The same file now renders in
  0.94 s at 2.2 GB on 24 threads, and CPU time at 16 threads fell from 99.1 s
  to 5.8 s — the surplus was duplicated work, so throughput improves alongside
  memory. Rendered output is byte-identical.

## [0.6.0] — 2026-08-27

A hardening release. The public Rust API is strictly additive — nothing was
removed and no signature changed — but the interpreter and the PDF reader now
refuse a number of inputs they previously accepted, which is why this is a
minor bump rather than a patch.

### Breaking

Nothing here breaks compilation. Every item changes what happens to a *file*,
so audit these if you render input you do not control the shape of. All were
previously ways to abort the process, produce silently wrong output, or run
without bound; each is documented in full under Security below.

- **Numeric overflow now raises `undefinedresult`.** `1e308 1e308 mul`
  returned `inf` and `inf 0 mul` then returned `NaN`; both now error, per PLRM
  and matching Ghostscript at its own boundary. A literal `1e999` no longer
  scans as `inf` — it declines to be a number and becomes an undefined name.
  A program that relied on either value will now stop.
- **Non-finite path coordinates raise `undefinedresult`.** Reachable through a
  CTM composed past the representable range. Previously drew arbitrary output.
- **`VMerror` now halts execution.** `errordict` registered the handler under
  a name that could never match, so the interpreter printed the error and
  carried on past the failed allocation. Programs that appeared to survive an
  allocation failure will now stop at it.
- **PostScript VM is capped at 8 GiB by default** (`--max-vm`,
  `setuserparams /MaxLocalVM`). Jobs above it raise `VMerror` instead of
  growing until the OS intervenes. Separate from the renderer's image and band
  buffers, so this does not cap rendering resolution.
- **Page size is bounded at 14400 pt** (200 in) for PostScript input.
  Resolution is deliberately *not* capped — 1200 dpi at 11x17 and larger is
  ordinary prepress.
- **Image dimensions are bounded** to 100,000 per side and 4e9 pixels total,
  on both the PostScript and PDF paths, with `/BitsPerComponent` limited to
  1..=16. Sized for prepress, not for the sample corpus: a 60x40 inch page at
  1200 dpi is 3.46 Gpx and is accepted.
- **Decompressed streams are bounded** at 512 MiB, raised to whatever an image
  raster or an embedded file declares for itself. A chain of decompression
  filters no longer multiplies without limit.
- **`i64::MIN -1 idiv`** (and `mod`) raise `undefinedresult` instead of
  panicking in release.

All 691 sample PDFs render byte-identically across every change above, the
6268-file PostScript corpus has the same 31 failures with an identical failing
set, and both visual suites pass.

### Fixed

- **`stet-core` failed to compile for `wasm32-unknown-unknown`.** The 8 GiB VM
  default is not a large `usize` on a 32-bit target but a const-evaluation
  error. The default is now computed in `u64`, falling back to `usize::MAX / 4`
  where 8 GiB does not fit.
- **`currentuserparams` reported `MaxLocalVM` as 0**, telling a program there
  was no limit moments before it hit one.

### Added

- `--timeout <SECONDS>` and `--max-vm <MB>` CLI options.
- `Context::set_timeout`, `Context::check_deadline`, `Context::check_vm_alloc`,
  `Context::vm_bytes`.
- `stet_graphics::image_limits` — `MAX_IMAGE_DIMENSION`, `MAX_IMAGE_PIXELS`,
  `MAX_BITS_PER_COMPONENT`, and validators, shared by the PostScript and PDF
  paths so two prepress-calibrated numbers cannot drift apart.
- `stet_pdf_reader::filters::{DecodeBudget, decode_stream_bounded,
  MAX_DECODED_STREAM_BYTES}`. `decode_stream` is unchanged.
- `PdfError::NestingTooDeep`. `PdfError` is `#[non_exhaustive]`, so this is
  not a breaking change.
- `stet-pdf-reader`'s `parse_object_at_depth`,
  `parse_object_from_token_at_depth`, `parse_dict_body_at_depth`, and
  `MAX_OBJECT_DEPTH`. The existing depth-0 entry points are unchanged.
- `scripts/check-cli-docs.sh`, wired into CI and `.githooks/pre-push`: every
  CLI option must appear in `--help` and in both READMEs. The crates.io page
  had been listing ten of nineteen options.
- `crates/stet-cli/examples/profile_images.rs` and `profile_alloc.rs` —
  per-stage memory attribution for the render path.
- Five `cargo-fuzz` targets in `fuzz/`, with seeded corpora and a CI smoke gate.
- `[profile.hardened]` — release codegen with overflow checks left on.

### Security

Five unbounded-recursion vectors in the PDF reader let a small crafted file
abort the process with a native stack overflow. A stack overflow is not a
panic, so none of these could be contained by `catch_unwind` — any program
rendering untrusted PDFs was exposed to an uncatchable denial of service.
This is the same vulnerability class as RUSTSEC-2026-0187 in `lopdf`.

Three were depth-based, and are now capped:

- **Nested arrays and dictionaries in an object body** (`lexer.rs`).
  `parse_object_from_token` and `parse_dict_body` are mutually recursive with
  no bound, so `[[[[…` or `<</A<</A…` in any object exhausted the stack. Both
  now thread a depth counter and stop at `MAX_OBJECT_DEPTH` (256), returning
  the new `PdfError::NestingTooDeep`. The existing `parse_object`,
  `parse_object_from_token`, and `parse_dict_body` signatures are unchanged
  and enter at depth 0; `parse_object_at_depth`,
  `parse_object_from_token_at_depth`, and `parse_dict_body_at_depth` are new.
- **Nested arrays in a content stream** (`content/mod.rs`). Content-stream
  operands go through a separate parser, `parse_inline_array`, which needed
  its own cap; it shares `MAX_OBJECT_DEPTH`.
- **Nested procedures in a Type 4 (PostScript calculator) function**
  (`resources/function.rs`). `parse_token_sequence` recurses once per `{`
  body; now capped at `MAX_CALC_DEPTH` (64).

Two were cycle-based, which no depth cap alone can fix — the recursion is
infinite, so file size is irrelevant (both reproduce in under 1 KB):

- **A Type 3 stitching function that reaches itself through `/Functions`**
  (`resources/function.rs`), directly or through a ring of siblings.
  `PdfFunction::parse` now carries a set of the object numbers on the current
  path and raises `PdfError::CircularReference` on re-entry. It is a path set,
  not a seen-set — entries are popped on the way out, so the legitimate shape
  `/Functions [7 0 R 7 0 R]` still parses and renders.
- **A Type 3 CharProc that shows its own glyph** (`content/mod.rs`), directly
  or through a pair of fonts naming each other. This path incremented the
  interpreter's `depth` field but never tested it: the only check lived in
  `handle_form_xobject`. Type 3 glyphs and soft-mask groups — which likewise
  re-enter `interpret_stream` without passing through the Form XObject path —
  now check it too. The bound, `MAX_CONTENT_NESTING`, is 20, the value the
  Form XObject and pattern guards already used, so nothing that renders today
  changes.

Added `PdfError::NestingTooDeep`. `PdfError` is `#[non_exhaustive]`, so this
is not a breaking change.

Separately, image dictionary integers are now validated before use.

- **`/Width` and `/Height` were cast with `as u32` and then multiplied in
  `u32`.** The product overflowed: 65537 x 65536 is `2^32 + 65536`, so
  `width * height` came back as 65536 and the buffer allocated from it was far
  smaller than the loops that filled it — an "attempt to multiply with
  overflow" panic in debug builds, a silently undersized allocation in
  release. The truncating cast was wrong on its own too: `/Width 4294967297`
  became a 1-pixel image rather than an error.
- **The loop counts alone were a denial of service.** Even where the
  arithmetic survived, an 800-byte file declaring a 65537 x 65536 image spent
  9-19 seconds in release. It now completes in 0.1 s.
- Both are fixed by validating at the four points where an image dictionary is
  read (image XObject, inline image, `/SMask`, `/Mask`): dimensions must be
  positive and at most `MAX_IMAGE_DIMENSION` (100,000), and their product at
  most `MAX_IMAGE_PIXELS` (4,000,000,000). The ceiling is sized for prepress
  rather than for the sample corpus: a 40x28 inch press sheet at 600 dpi is
  403M pixels, an A0 poster at 600 dpi 558M, and 60x40 inch grand format at
  1200 dpi 3.46G, all of which a RIP must accept. It stays under 2^32 because
  a dozen sites compute `width * height` in `u32`; anything multiplying
  further by a component count uses saturating `usize`.
- **`/BitsPerComponent` is validated too**, to 1..=16. It reaches
  `1u32 << bpc` in `expand_bits_to_bytes`, which panics in debug builds at 32
  or more. That function now also reserves its three-way
  `width * height * components` product in `usize`, which overflows a `u32`
  sooner than the two-way one does.

Filter and font parameters are now validated the same way.

- **`/Columns`, `/Colors`, and `/BitsPerComponent` in `/DecodeParms`** were
  cast straight to `usize` and multiplied. A zero in any of them drove
  `row_bytes` to zero and reached `slice::chunks(0)` — "chunk size must be
  non-zero", which panics in **release** builds, not only debug. A negative
  became astronomical under the cast and aborted the process on a 2.3-exabyte
  reservation. Both are now range-checked with the row-size products computed
  via `checked_mul`; a malformed `/DecodeParms` leaves the stream unchanged
  rather than failing it, which is what the caller would have had if
  `/Predictor` were absent.
- **PS CIDFont header counts** (`/CIDCount`, `/SubrCount`, `/FDBytes`,
  `/GDBytes`, `/SDBytes`). `/SubrCount` was passed to `Vec::with_capacity`
  *before* the bounds check that would have rejected it, so a bogus count
  panicked with "capacity overflow" in release as well as debug. Separately,
  `FDBytes + GDBytes == 0` made the CID map size zero for any `/CIDCount`, so
  the "binary data too short" check passed and an 8 TB reservation followed
  from a 700-byte file. Counts are now bounded against the binary segment
  actually present rather than against a fixed ceiling, the byte-widths are
  capped at 8, and the reservation happens after the check.

Neither bound rejects anything real: all 691 sample PDFs were re-rendered with
the predictor fallback instrumented, and none takes it.

The font parsers in `stet-fonts` got the same treatment. Font programs arrive
embedded in both PDF and PostScript input, so these are attacker controlled in
the same way a PDF object is.

- **TrueType composite glyph recursion** — a component naming its own glyph,
  directly or through a ring, recursed until the stack was gone. Now capped at
  depth 8 with a path set of glyph ids, popped on exit so a font that
  legitimately reuses one accent twice still renders both copies. A depth cap
  alone is not sufficient here: a composite naming many components, each itself
  such a composite, repeats no id on any path, and the work is
  `fan_out ^ depth` — 64 components at depth 8 is 2.8e14 expansions from a
  400-byte glyph. A shared expansion budget (4096) bounds the total work.
- **Type 1 `seac`** re-entered through `execute()`, which restarts the
  subroutine depth counter at 0, so the existing depth-10 guard never fired on
  a `seac` naming its own glyph. The depth is now threaded through.
- **`/Subrs N`** reserved `N` entries before reading any of them;
  `/Subrs 999999999` panicked with "capacity overflow" (≈24 GB in release).
  Clamped to the bytes remaining after the marker, since each entry needs at
  least a `dup i n RD ` introducer.
- **cmap format 12** walked `for code in start..=end` over raw u32 — 4.3
  billion iterations for a full-range group — and computed
  `start_gid + (code - start_char)` as an unchecked u32 add. The span is now
  clamped to 0xFFFF (past which no glyph id can land in the 16-bit range
  anyway, so nothing mappable is lost) and the add is checked.
- **Type 2 `callsubr` / `callgsubr`** computed `idx + bias` as an unchecked
  i32 add. The number encodings top out at 32767, but Type 2 implements `add`,
  `sub`, `mul`, and `div`, so a charstring can multiply past `i32::MAX`, where
  the `as i32` cast saturates and the bias add overflows. Now `checked_add`.
- **`read_u16` / `read_i16` / `read_u32`** are now internally bounds-checked,
  returning 0 past the end of the slice. No caller changes: the ~40 call sites
  already pre-check (confirmed by probing every truncation of a synthetic font
  and 408 mutations of its offset and count fields, with zero panics), but the
  invariant was manual and unenforced.

All 691 sample PDFs render byte-identically before and after these font
changes.

Decompressed stream size is now bounded, closing a decompression-bomb vector.
`decode_stream` applied its filter chain with no ceiling on the output, so the
amplification was unbounded *and* multiplicative: a single Deflate pass tops
out near 1032:1 on a run of zeros, but a 707-byte file carrying
`/Filter [/FlateDecode /FlateDecode /FlateDecode]` measured a 2058 MB peak RSS
here, and aborted with `memory allocation of N bytes failed` — a core dump, not
a catchable error — as soon as the address space could not satisfy it. Rust
aborts on allocation failure, so like the recursion vectors above this had to
be prevented rather than handled.

- **The whole chain now shares one budget**, rather than each filter starting
  fresh, which is what stops nesting from multiplying. `FlateDecode`,
  `LZWDecode`, and `RunLengthDecode` check it from inside their decode loops —
  checking the finished buffer would mean the allocation the ceiling exists to
  prevent has already happened — and each stage's result is checked afterwards
  as well, covering the image codecs that size their own output.
- **A budget overrun is an error, never a truncation.** `decode_flate`
  recovers from a genuinely truncated stream by retrying it as raw deflate and
  keeping the longer result; without care an overrun would have taken that
  path and come back as a silently truncated success.
- **The ceiling is raised by what the stream declares about itself.** The
  general allowance, `MAX_DECODED_STREAM_BYTES`, is 512 MiB, which covers
  content streams, object and cross-reference streams, font programs, ICC
  profiles, and sampled-function tables with roughly 4x headroom over the
  largest of those. A dictionary that declares an image raster (`/Width`,
  `/Height`, `/BitsPerComponent`, `/ColorSpace`) or an attachment length
  (`/Params /Size`) gets that instead, so a 60x40 inch grand-format image at
  1200 dpi — a legitimate 13.8 GB stream — is unaffected. The declared value
  only ever raises the bound, never lowers it, so a stream that declares
  nothing, or declares something small, keeps the full general allowance.

New public API: `DecodeBudget`, `decode_stream_bounded`, and
`MAX_DECODED_STREAM_BYTES` in `stet_pdf_reader::filters`. `decode_stream` is
unchanged and now decodes under the general ceiling.

All 691 sample PDFs render byte-identically before and after this change.

The 8 GiB PostScript VM default broke the `wasm32-unknown-unknown` build.
`8 * 1024 * 1024 * 1024` does not fit a 32-bit `usize`, and const evaluation
rejects it outright, so `stet-core` failed to compile for that target at all —
`error[E0080]: attempt to compute 8388608_usize * 1024_usize, which would
overflow`. The default is now computed in `u64` and falls back to
`usize::MAX / 4` where 8 GiB does not fit: on a 32-bit target the whole
address space is 4 GiB, so an 8 GiB ceiling would be no ceiling at all, and a
quarter of the space leaves the rest for the renderer's buffers, the module,
and the stack. The 64-bit value is unchanged.

`VMerror` was raised under a name nothing could catch. `errordict` registered
the handler as `/VMError` while `PsError::VMError` displays as `VMerror` —
PLRM's spelling, used 35 times there, and Ghostscript's. The lookup missed, so
the interpreter printed the error and **continued past the failed
allocation**, leaving the program running as though it had succeeded. That was
harmless while the variant had no producer and became reachable the moment
`--max-vm` started raising it. Now `stopped` catches it and `$error
/errorname` reports `/VMerror`.

`currentuserparams` reported `MaxLocalVM` as 0. The ceiling can be set three
ways — the built-in default, `--max-vm`, and `setuserparams` — and only the
last writes the dict the query copied, so a program asking for the limit was
told there was none moments before hitting one. It now reports the value
actually in force.

Non-finite numbers and integer-overflow traps in the PostScript interpreter.
The backlog listed this as cosmetic — "garbage output rather than a panic" —
which was wrong in both directions: one case was a release-mode crash, and the
rest were a PLRM conformance gap rather than a cosmetic one.

- **`-9223372036854775808 -1 idiv` panicked in release.** `i64::MIN / -1` is
  the one pair that overflows, and integer division overflow is a trap in
  Rust's semantics rather than something `overflow-checks` enables, so this
  aborted an optimised build from 30 bytes of PostScript. `idiv` and `mod` now
  use `checked_div` / `checked_rem` and raise `undefinedresult`.
- **Real overflow produced `inf` instead of an error.** PLRM: "A numeric
  computation would produce a meaningless result or one that cannot be
  represented as a number. Possible causes include numeric overflow or
  underflow, division by 0…" — and every arithmetic operator that can return a
  real lists `undefinedresult` among its errors. `1e308 1e308 mul` yielded
  `inf`, and `inf 0 mul` then yielded `NaN`. `add`, `sub`, `mul`, `div`, and
  `exp` now raise `undefinedresult` when the result is not finite, matching
  Ghostscript, which does the same at its own (single-precision) boundary.
  stet's boundary is `f64`'s, as with the `i64` integer width: PLRM Appendix B
  lists real limits under "Typical Limits" as properties of the host
  architecture, not as conformance requirements.
- **A literal `1e999` scanned straight to `inf`**, introducing a non-finite
  value with no arithmetic at all — `"1e999".parse::<f64>()` succeeds. The
  scanner now declines such a token, which falls through to the name scanner
  exactly as `1e999x` already did, so a program using one gets `undefined`
  rather than a value. Ghostscript raises `limitcheck` here and stet
  deliberately does not: that was tried first and it broke a 35 MB corpus file
  that renders correctly, whose hex image data contains byte runs such as
  `5657564e574` — syntactically a real with a 580-digit exponent, scanned and
  discarded harmlessly as a name.
- **Path construction rejects non-finite device coordinates.** With the two
  sources above closed, a `NaN` could still arrive through a CTM composed past
  the representable range (`1e300 1e300 scale` twice). `moveto`, `rmoveto`,
  `lineto`, `rlineto`, `curveto`, `rcurveto`, `arc`, `arcn`, `arcto`, and
  `arct` now raise `undefinedresult` instead, which is the error PLRM assigns
  to graphics operators under an unusable CTM. The check is on the path rather
  than on the matrix operators because composing a wild CTM is not itself an
  error — a program may `scale` extravagantly, draw nothing, and `grestore`.
  This also closes a latent hang: `arc` normalises with
  `while stop < start { stop += 360.0 }`, which never terminates for a `stop`
  of negative infinity.

A `NaN` reaching geometry never crashed — it makes every comparison against it
false, so bounds, banding, and winding quietly take the wrong branch. Silent
wrong output was the real exposure.

All 691 sample PDFs render byte-identically. The 6268-file PostScript corpus
has the same 31 failures before and after, with no file newly failing; all 86
`ps_samples` and the `unit_tests/` suite pass unchanged.

### Added

- **A ceiling on PostScript VM**, via `Context::max_local_vm`,
  `setuserparams /MaxLocalVM`, and the CLI's `--max-vm <MB>`. Exceeding it
  raises `PsError::VMError`, which previously had no producer.
  **The default is 8 GiB rather than unlimited**: a failed allocation aborts
  the process, so there is no error to catch afterwards and an opt-in limit
  would leave the abort reachable by default. `500000000 array` requested
  16 GB and took stet down; it now raises `VMerror`. This bounds PostScript VM
  — strings, arrays, dictionaries — which is a separate pool from the
  renderer's band and image buffers.

  The check measures reserved capacity rather than length, and bounds what may
  be *requested* rather than what is held: the arena stores grow geometrically,
  so one sitting at capacity asks the allocator for roughly twice that. Steady
  growth therefore stops at about half the nominal ceiling; a single large
  request is bounded by the full one. It also counts global VM, unlike PLRM's
  local-only `MaxLocalVM`, since a local-only ceiling is sidestepped with
  `true setglobal`.

- **`--timeout <SECONDS>`** and `Context::set_timeout` — a wall-clock deadline
  for interpretation, raising `PsError::Timeout`. PostScript is
  Turing-complete, so nothing static bounds how long a program runs, and a
  deadline is the only thing that stops one which makes progress but never
  terminates. **There is no limit by default**, preserving existing REPL and
  CLI behaviour; set one when the input is untrusted. The check counts down a
  `u32` and consults the clock every 4096 iterations, and short-circuits when
  no deadline is set, so the default path measures as free (-0.42% on a tight
  4M-iteration arithmetic loop).

### Added

- **Fuzzing (`fuzz/`)** — five `cargo-fuzz` targets covering the parsers that
  consume untrusted input: `fuzz_pdf_parse` (open + render + the structural
  API), `fuzz_font_truetype`, `fuzz_font_cff`, `fuzz_font_type1`, and
  `fuzz_ps_tokenizer`. `fuzz/seed-corpus.sh` seeds them from the in-tree
  samples (703 PDFs, 6410 PostScript inputs, 35 Type 1 faces) and
  `fuzz/run.sh` runs them with settings suited to a sanitizer build. The crate
  is excluded from the workspace, like `stet-wasm`, because cargo-fuzz needs
  nightly and stet is stable-only with a pinned MSRV. A weekly scheduled
  workflow (`.github/workflows/fuzz.yml`) runs 300s per target; it is not on
  the push path, where it gated nothing and dominated the run. See
  `fuzz/README.md`.

### Added

- **`[profile.hardened]`** — release codegen with `overflow-checks` and
  `debug-assertions` left on, for finding silent arithmetic wraps at release
  speed. Build with `cargo build --profile hardened`. It is a testing profile,
  not a shipping one: published binaries stay on `release`, since a trapped
  overflow is a panic and that is not what a renderer should do to a user over
  a malformed file. Gated in CI by a new `Overflow checks` job. The vendored
  `stet-tiny-skia` forks are excluded per-package — their SIMD-lane emulation
  is modular arithmetic by definition, matching the hardware instructions the
  aarch64 paths use.

### Fixed

- **The PostScript `image`, `imagemask`, and `colorimage` operators had no
  upper bound on their dimensions.** Only the lower bound was checked, so
  sixty bytes of PostScript could request a 4 x 10^18 byte allocation, which
  aborts the process rather than failing catchably. All three now validate
  against the shared prepress-scale limits: a non-positive dimension raises
  `rangecheck` and one past the ceiling raises `limitcheck`, per PLRM.
  A 24000 x 16800 press sheet (403M pixels) still draws normally.
- **Image size limits moved to `stet_graphics::image_limits`,** shared by the
  PostScript operators and the PDF image handler. The two crates cannot see
  each other, and duplicating a prepress-calibrated ceiling would let the two
  copies drift.
- **`setpagedevice` with a degenerate `/PageSize` panicked the renderer.**
  `<< /PageSize [-1 -1] >>` reached `Pixmap::new(0, 0)`, which returns `None`,
  through an `.expect()`. The allocation is now non-panicking, and
  `setpagedevice` clamps a file-declared page to 14400 pt per side (200 in,
  the Adobe PDF 1.7 `/MediaBox` implementation limit), falling back to US
  Letter for a non-finite or non-positive value. **Render resolution is
  deliberately not capped** — pixel dimensions are `points * dpi / 72`, and
  while the points come from the file, the DPI is the caller's explicit
  request; a 1200 dpi proof of a large-format page is a legitimate gigapixel
  render.
- **Two native-stack recursion vectors in the interpreter.** A `/Separation`
  colour space whose tint transform sets that same colour space re-entered
  `exec_sync` without bound — about 200 bytes of PostScript aborted the
  process. `exec_sync` is now capped at depth 100 via `Context::exec_sync_depth`,
  raising `PsError::ExecStackOverflow`. Separately, `parse_procedure` and
  `stream_parse_procedure` had no `{`-nesting cap, so 200000 nested braces
  (a 400 KB file) aborted; both now stop at `MAX_PROC_DEPTH` (100), matching
  the existing `MAX_BOS_DEPTH`.
- **Four unchecked `u16` range ends in the CFF parser** (charset formats 1 and
  2, CID-map formats 1 and 2). `for sid in first..=first + n_left` overflows
  when a range starts near 0xFFFF — a panic under overflow checks, a wrapped
  range otherwise. Found by `cargo fuzz` within 60s of a cold start. The same
  pass fixed `n_glyphs - 1` in both format 0 readers, which underflows `usize`
  for a font declaring zero glyphs.

- **Octal escapes in PDF literal strings** (`\ddd`) accumulated into a `u8`,
  so a three-digit escape above `\377` overflowed the accumulator. The
  rendered byte was already correct — PDF 32000-1 7.3.4.2 specifies that
  high-order overflow is ignored, which is what the release build's silent
  wrap produced — but the arithmetic was wrong and panicked under overflow
  checks. Found by sweeping the sample corpus under the new `hardened`
  profile (`pdf_samples/142.pdf`).
- Shading color-stop sampling now sorts with `f64::total_cmp` instead of
  `partial_cmp().unwrap()`, and clamps its own sample count so the divisor in
  `i / (n - 1)` cannot be zero. No crafted file was found that reaches either
  path — the discontinuity filter excludes NaN and callers already clamp the
  count — so this is hardening against a future caller, not a live fix.

## [0.5.0] — 2026-08-25

Minor release. PostScript integers are now 64-bit, which fixes the standard
LCG idiom that programs use for pseudo-randomness and is the reason this is a
breaking release rather than a patch. Three Type 3 font defects and a
`clippath` coordinate-space bug are also fixed, and the CLI gains `--page`.

### Breaking

Downstream Rust code that reads PostScript integers needs attention; nothing
in the PostScript language surface changed incompatibly.

- **`PsValue::Int` now carries `i64` instead of `i32`.** A
  `match obj.value { PsValue::Int(v) => … }` binds an `i64`, so any use site
  that needs an `i32` no longer compiles. `DictKey::Int` and `Token::Int`
  widened with it, as did `Context::rand_seed`.
- **`PsObject::as_i32()` now range-checks.** It returns `None` for a value
  outside `i32`, where before it always returned `Some` for an integer. This
  is the one change with no compiler error behind it — audit call sites that
  treat `None` as "not an integer". Use the new `as_i64()` for the full
  range; keep `as_i32()` where the value is genuinely bounded (array and
  string indices, character codes), since a too-large value should fail those
  callers' range checks rather than wrap into a valid-looking index.
- **`PsObject::int()` takes `impl Into<i64>`.** Calls are unaffected; only
  code coercing it to a `fn(i32) -> PsObject` pointer breaks.

### Fixed

- **Type 3 fonts supplying only `BuildGlyph` raised `invalidfont`.** The show
  path required `BuildChar` unconditionally and pushed the character code.
  PLRM 5.7 lists `BuildGlyph` as preferred and makes `BuildChar` required only
  "for LanguageLevel 1 or if `BuildGlyph` is absent", so such a font is
  well-formed and must be handed the character *name* from `Encoding`.
  Ghostscript renders these; stet refused them. `xshow`/`yshow`/`xyshow` had
  the identical defect.
- **`stringwidth` raised `invalidfont` on every Type 3 font**, `BuildChar`
  ones included. It branched for font types 2, 0 and 42 and then fell through
  to the Type 1 path, which looks for `CharStrings` — a Type 3 font has none.
  There is no width table to consult: the width is whatever the build
  procedure hands `setcachedevice`/`setcharwidth`, so the procedure now runs
  inside a `gsave`/`grestore` with its marks drained, and measuring paints
  nothing.
- **`glyphshow` raised `invalidfont` on every Type 3 font.** It read
  `FontType` but never branched on 3, going straight to the `CharStrings`
  lookup. Per the PLRM it now invokes `BuildGlyph` with the name directly —
  bypassing `Encoding`, which is what lets `glyphshow` reach glyphs no
  character code maps to — or, with only `BuildChar`, reverse-searches
  `Encoding` for the name and pushes the array index, retrying with
  `/.notdef` and raising `invalidfont` only when neither is encoded.
- **PostScript integers are now 64-bit**, matching Ghostscript, which fixes the
  standard LCG idiom PostScript programs use for pseudo-randomness:
  `/seed seed 1103515245 mul 12345 add 2147483648 mod def`. On 32-bit integers
  the product overflowed, promoted to a real, and `mod` — which is
  integer-only — raised `typecheck`. Widening only the real fallback would not
  have fixed it: the product needs 55 bits and a real carries 53, so the seed
  would have come out one too high and every later draw would have diverged
  silently from what other interpreters produce. PLRM Appendix B's 32-bit
  range is listed under "Typical Limits" for interpreters "running on 32-bit
  machines" which "do not necessarily apply to all PostScript
  implementations", so this is not a conformance change. Overflow past the
  64-bit range still promotes to a real. `bitshift` is correspondingly 64-bit
  wide, and `cvi` accepts the wider range.
- **`clippath` returned the page in device space instead of current user
  space**, so a program that had transformed its coordinate system got a clip
  rectangle dragged along with the transform. The `clippath fill` idiom for
  painting a background then filled an offset region and left part of the page
  bare — visible in the tiger EPS, whose grey backdrop was displaced by its
  `%%BoundingBox` origin. The default clip is a fixed region of the device, so
  it is now derived with the default CTM; `pathbbox` and `fill` map it back
  through the current CTM, which is what puts it in user space for the caller.
  Ghostscript's values now match exactly under translate, scale and rotate.
- **`cvi` on a long integer-valued string was off by one.** The string scanner
  returned `f64`, so `(22358003463039195) cvi` came back as `...196` after the
  round trip through a 53-bit mantissa. Integer literals now stay integral.

### Added

- **The CLI reports a page that was painted but never shown.** A program that
  paints marks and then ends without a matching `showpage` leaves them on a
  page the device is never asked to emit, and the page is discarded. That is
  correct — it is what the PLRM specifies and what Ghostscript's file devices
  do — but it was indistinguishable from a broken renderer: no file appeared
  and nothing said why. The warning distinguishes a program that produced no
  output at all from one that lost only its trailing page.
- **`Interpreter::warnings()`** surfaces the same diagnostic to library
  callers, where the silence was worse: `render()` returned `Ok(vec![])`, an
  empty page list that reads as a legitimate result. New public types
  `ExecWarning` and `ExecWarningKind` in `stet::diagnostics`; the CLI shares
  the detector, so the two cannot drift. Programs that install `nulldevice`
  are exempt — that is the PLRM-sanctioned way to ask for no output, so marks
  left unemitted are the point rather than a mistake.
- **`--page` sets the page size for PostScript/EPS input** — a named size
  (`letter`, `legal`, `tabloid`, `ledger`, `executive`, `a0`-`a6`, `b4`, `b5`)
  or `WIDTHxHEIGHT` in points, with an optional `-landscape` / `-portrait`
  suffix that swaps the dimensions. There was previously no way to render a
  plain `%!PS` program whose artwork is larger than the default page:
  `%%BoundingBox` sets the page only for EPS — for a non-EPS document DSC
  makes it a description of the artwork's extent, not a page-size request, so
  both stet and Ghostscript fall back to US Letter and clip. `--page`
  overrides an EPS `%%BoundingBox` when both apply, and is rejected for PDF
  input, whose pages carry their own size.
- `GlyphCache::by_type3_name`, a name-keyed Type 3 glyph cache. `glyphshow`
  can name a glyph that no character code maps to, which leaves nothing for
  the existing code-keyed cache to key on.

### WebAssembly

- **`stet-wasm` 0.2.0.** Its JavaScript API is unchanged, but the browser
  build inherits everything above, so rendering output moves: `clippath`
  backgrounds fill the page, Type 3 fonts that previously raised
  `invalidfont` render, and PostScript programs using the standard LCG for
  pseudo-randomness run instead of failing. A minor rather than a patch
  because the pixels change, not because anything you call does.

## [0.4.1] — 2026-08-15

Patch release. Two `currentsystemparams` values were wrong in every release
up to and including 0.4.0, and PDFs now record which build wrote them. No API
changes; no rendering changes.

### Fixed

- **`/PrinterName` returned `(stetIE)` instead of `(stet)`.** The string was
  allocated with the four bytes of `stet` but declared six bytes long, so
  reading it ran two bytes into the next allocation — which happened to be
  `/RealFormat`. Not memory-unsafe (the arena is a single buffer), but it put
  a neighbouring allocation's bytes into a value any PostScript program can
  read, and the value would have changed as soon as allocation order did.
- **`/RealFormat` returned `(IEE)` instead of `(IEEE)`** — a missing `E` in
  the literal, independent of the overrun above. The PLRM specifies this key
  as naming the internal real representation, and Ghostscript reports
  `(IEEE)`.

  Both lengths are now derived from the literal rather than written out
  twice, which is what allowed them to disagree. The other three
  allocate-then-declare sites in `Context::new` were audited and are correct.
  Regression coverage in `unit_tests/interpreter_param_tests.ps` asserts
  lengths as well as contents — the contents alone read plausibly, and it was
  the overrun that made them wrong.

### Changed

- **PDF `/Producer` now carries the version**, e.g. `stet 0.4.1`, where it
  previously wrote a bare `stet`. Every other producer does this —
  Ghostscript writes `GPL Ghostscript 10.05.1`, Distiller
  `Acrobat Distiller 20.0` — and it is the first thing checked when a
  prepress shop is chasing a rendering difference between two files. A
  `pdfmark` `/DOCINFO /Producer` override still takes precedence; this
  changes only the default. Note that this alters bytes in the `/Info` dict
  of every PDF stet writes, at every release.
- **Documented MSRV corrected to Rust 1.88.** The README badge had claimed
  1.85 since it was added — a number inferred from `edition = "2024"` and
  never compiled against. The real floor is 1.88: first-party code uses
  let-chains in 282 places across nine crates, `jpeg-encoder` declares 1.87,
  and `fearless_simd` declares 1.86. Nothing about what stet requires has
  changed; only the claim is now true. `rust-version = "1.88"` is declared in
  `[workspace.package]` and inherited by all eleven first-party crates, so
  cargo now reports a clear "requires rustc 1.88" instead of failing with a
  confusing edition parse error on an older toolchain.
- A pinned `MSRV 1.88` CI job builds the workspace on exactly that toolchain
  on every push, and `scripts/check-release-versions.sh` now ties the README
  badge, the README prose, and the CI job's pin to `rust-version` so the four
  cannot drift apart. The script also asserts every publishable crate
  declares an MSRV, so none can reach crates.io without one.
- **Switched to `resolver = "3"`** (MSRV-aware dependency resolution). Cargo
  now prefers dependency versions compatible with the declared
  `rust-version` rather than always taking the newest, so a routine
  `cargo update` can no longer silently break the floor. The lockfile was
  byte-identical on adoption, but this is already doing work: it holds back
  `hayro-jpeg2000` 0.4.0 (needs 1.92) and `moxcms` 0.9.0 (needs 1.89).

### Note for downstream users

Releases up to and including 0.4.0 published with no `rust-version` in their
manifests, so crates.io and docs.rs show no MSRV for them and cargo cannot
warn an old toolchain before it fails to compile. Published versions are
immutable; this release is the first to carry the metadata.

## [0.4.0] — 2026-08-14

Minor release focused on **memory and PostScript conformance**. `restore`
now reclaims local VM instead of only reverting values, several
long-standing PLRM 3.7.2/3.7.3 violations in the interpreter's own writes
are fixed, and a 6384-file PostScript corpus sweep drove the job-abort
count from 1158 to 147 — of which 116 fail identically in Ghostscript,
leaving 31 that are genuinely ours.

This is a `0.x` minor bump. No public API was removed; `Context` gained
fields, which is source-breaking only for code constructing one
literally (it has no public constructor other than `Context::new`).

### Highlights

- **`restore` reclaims local VM.** Allocations made above a `save`'s
  high-water mark are released rather than left resident. A
  save/restore loop that peaked at 2108 MB now peaks at 74 MB.
- **`restore` actually reverts what it is supposed to.** Several
  interpreter-internal writes bypassed copy-on-write, so `restore` had
  no backup to revert to: `defineresource`, `FontDirectory`, `reverse`,
  `execstack` and `dictstack`. This was a live PLRM 3.7.3 violation, not
  a theoretical one.
- **Global/local VM enforcement is no longer silently disabled.**
  `.error` did not restore `setglobal`, so any caught error left the
  interpreter in whatever VM mode the failing code had set.
- **Eleven interpreter defects found by a 6384-file corpus sweep**, each
  A/B'd against the previous sweep with no regressions: procedure data
  sources, the Pattern colour space, `bind` on nested procedures,
  array-form colour spaces, `cvi`/`cvr` string conversion, CIDFontType 0
  `StartData`, 16-bit image samples, EOI-less JPEG, `shareddict`/`scheck`,
  self-registering resource files, `rectclip`, `cshow`, and `charpath` on
  Type 3 fonts.

### Added

- `charpath` support for Type 3 fonts: the glyph procedure runs without
  marking the page and the paths it would have painted become part of
  the current path.
- The `Pattern` colour space — `setcolorspace`/`setcolor`/`currentcolor`
  with a pattern, including the uncoloured (PaintType 2) base-space form.
- `shareddict` and `scheck` in `systemdict`.
- 16 bits per component for `image` / `imagemask` / `colorimage`.
- `stet_core::vm_audit` and `--example audit_vm`: a machine check for
  dangling references and PLRM 3.7.2 global/local violations.
- PostScript corpus build and sweep tooling under `scripts/`.

### Fixed

- `restore` now releases local VM allocated above the save mark, and
  copy-on-writes the dictionaries and arrays it is required to revert.
- Every allocation is stamped with its save level and VM mode, so
  `restore` can tell a surviving reference from a dangling one and raise
  `invalidrestore` when PLRM requires it.
- `.error` restores the VM allocation mode; page-device arrays are
  allocated in the page device's own VM and deep-copied on promotion.
- `filter` accepts procedure data sources everywhere, runs them when the
  data is read rather than at `filter` time, and honours SubFileDecode's
  EOD semantics.
- `bind` marks nested procedures read-only per PLRM, and terminates on
  cyclic procedure graphs.
- `setcolorspace` accepts array-form base and alternate colour spaces.
- `cvi` and `cvr` convert strings through the scanner, as PLRM specifies.
- `CIDInit`'s `StartData` consumes its charstring blob instead of
  leaving it to be scanned as tokens.
- DCTDecode accepts a JPEG stream that ends before its EOI marker.
- Resource files that register themselves are loaded once, through a
  shared `.LoadResource`, so `composefont` can find a CMap on disk.
- `rectclip` takes every rectangle in a multi-rectangle argument, and
  accepts an empty array.
- `cshow` hands its procedure the character code, not the CID.
- CIE decode tables are memoised, and 8-bit image samples are no longer
  copied in `unpack_samples` — together these took the corpus from 24
  out-of-memory jobs to none.

## [0.3.0] — 2026-08-12

Minor release adding **PDF→PDF round-trip** through the PDF output
device. A PDF parsed by `stet-pdf-reader` into the display list can now
be re-emitted as PDF with its prepress semantics preserved — spot
(Separation/DeviceN) colors, ICCBased spaces, overprint, soft masks,
transparency groups, optional-content layers, and `/OutputIntents` all
survive the round-trip rather than collapsing to flat process color.

This is a `0.x` minor bump. The document-structure IR moved from
`stet-core` into `stet-graphics` (still re-exported by `stet-core`), so
code that reaches those types through `stet-core` is unaffected.

### Highlights

- **PDF→PDF round-trip** — `--device pdf` on a PDF input now routes
  through `PdfDocument` → `PdfDevice`, so a PDF can be read to the
  display list and written back out as PDF (previously only PS/EPS
  input reached `PdfDevice`).
- **Prepress color preserved** — Separation/DeviceN spot colors (and
  their base spaces, with DeviceGray promotion), ICCBased fill/stroke
  spaces, and ICCBased bases inside Indexed image spaces all round-trip;
  `/Catalog /OutputIntents` is carried through so the CMYK-driving ICC
  profile is retained.
- **Overprint preserved** — `/OP`/`/op` forced on the first paint of
  each content stream, `/OPM` carried through the display list, and
  overprint state emitted for Image and Shading paints.
- **Transparency & layers emitted from the display list** —
  `DisplayElement::Group` as a Form XObject, `SoftMasked` with per-paint
  alpha/blend, and `DisplayElement::OcgGroup` with `/OCProperties`
  optional-content groups.

### Added

- PDF output: Separation/DeviceN spot color + base round-trip,
  Separation/DeviceN shadings and spot imagemasks, and CMYK imagemask
  fill preservation.
- PDF output: ICCBased fill/stroke color-space round-trip and ICCBased
  base preservation inside Indexed image color spaces.
- PDF output: `/Catalog /OutputIntents` round-trip through PDF→PDF.
- PDF output: `Group` → Form XObject, `SoftMasked` + per-paint
  alpha/blend, `OcgGroup` → `/OCProperties`, per-paint alpha on the
  Image and Shading writer arms, overprint state on Image/Shading, and
  `/OPM` round-trip.

### Changed

- Document-structure IR lifted from `stet-core` into `stet-graphics`
  (re-exported by `stet-core`).
- PDF writer: graphics-state tracker restored across `q`/`Q`
  boundaries; replayed clips collapsed so a round-trip no longer grows
  the display list; implicit page-box clip skipped on PDF→PDF.
- `stet-pdf-reader`: spot tint-transform table cached per content
  stream.
- README: added a Commercial Support section.

### Fixed

- Removed a dead `emit_fill_color_rgb` helper, superseded by the
  DeviceColor-aware imagemask fill path (cleared a `dead_code` warning).

### Crates published at 0.3.0

`stet`, `stet-cli`, `stet-fonts`, `stet-graphics`, `stet-core`,
`stet-ops`, `stet-engine`, `stet-render`, `stet-viewer`,
`stet-pdf-reader`, `stet-pdf`. The vendored `stet-tiny-skia` /
`stet-tiny-skia-path` forks remain at `0.11.4`. `stet-wasm` remains at
`0.1.1` (excluded from crates.io, independent cadence).

## [0.2.1] — 2026-05-09

Patch release focused on PDF/X CMYK rendering correctness against the
[Ghent PDF Output Suite](https://gwg.org/pdf-output-suite/) (GWG)
test corpus. Fixes a family of bugs where ICCBased / Lab / DeviceN
fills, images, and transparency groups didn't round-trip through the
document's `/OutputIntents` profile correctly, producing visible "X"
markers in calibration swatches that should render uniform.

This is an **additive, non-breaking** release. Cargo will auto-bump
`stet = "0.2"` to `0.2.1`; downstream code does not need to change.
New public API on `stet-graphics::IccCache` and a new
`rendering_intent: u8` field on `stet-graphics::ImageParams` are
documented under "Added" below.

### Highlights

- **GWG 13.3** — ICCBased RGB paints with `/OP true` no longer route
  into the custom-spot overprint path; per PDF 1.7 §11.7.4.5 they
  paint as if `/OP` were false.
- **GWG 16.1** — per-intent PDF/X proofing chain (`source A2B → PCS
  → OI B2A → CMYK`) is built for every registered ICCBased RGB
  profile, threaded through `op_ri` / ExtGState `/RI`.
- **GWG 16.4** — transparency groups with no `/CS` (inherit) now
  resolve correctly to the parent's CMYK compositing space when
  the parent is a `/CS DeviceCMYK` group.
- **GWG 17.2** — ICCBased images now go through the proofing chain
  via `convert_image_8bit_with_intent` (was bypassing the
  OutputIntent roundtrip and rendering via direct source→sRGB).
- **GWG 22.1** — Lab fills populate `DeviceColor::native_cmyk` via a
  direct `Lab → PCS → OI B2A → CMYK` chain (matches Adobe ACE),
  and the OutputIntent install path pre-warms the sRGB→CMYK
  reverse transform so the parallel CMYK buffer never falls back
  to the PLRM `(1−r, 1−g, 1−b, 0)` formula.
- **WASM viewer** — `open_pdf` now applies the document's
  OutputIntent before storing the cached state, so PDF/X documents
  render in the browser the same way they do in the CLI.

### Added — public API (additive, non-breaking)

`stet-graphics`:

- `IccCache::convert_to_oi_cmyk(hash, components, intent)` — run an
  RGB ICC color through the proofing chain at the given intent and
  return the intermediate OutputIntent CMYK.
- `IccCache::convert_lab_to_oi_cmyk(l, a, b, intent)` — direct
  `Lab → OI CMYK` via the OI's per-intent B2A LUT.
- `IccCache::convert_image_8bit_with_intent(hash, samples,
  pixel_count, intent)` — bulk image conversion with explicit
  rendering intent.
- `IccCache::convert_color_with_intent` and
  `convert_color_readonly_with_intent` — per-intent single-color
  conversion.
- `IccCache::prepare_lab_to_oi_cmyk()` — pre-build per-intent
  Lab→OI samplers; pair with `prepare_reverse_cmyk()`.
- `IccCache::intent_from_pdf_byte(b: u8)` — map PDF rendering-intent
  bytes (`0..3`) to `IccRenderingIntent`.
- `pub use moxcms::RenderingIntent as IccRenderingIntent`.
- `pub struct LabToCmykSampler` (in `icc::perceptual`) with
  `pub fn sample_pdf_lab(l, a, b)`.
- New field `ImageParams::rendering_intent: u8`. Default is `0`
  (Perceptual). Per the documented "be a reader, not a writer"
  policy for param structs (CLAUDE.md), this is additive and not
  treated as a SemVer break.

`stet-pdf-reader`:

- `PdfDocument::apply_output_intent_as_default_cmyk()` now also
  pre-warms the sRGB→CMYK reverse and per-intent Lab→OI samplers
  in addition to its previous behaviour. No signature change.
- Image XObjects with `/Intent` now propagate the per-image
  rendering intent into `ImageParams.rendering_intent`, overriding
  the gstate `/RI` per ISO 32000 §11.3.4.

`stet-render`:

- `build_icc_cache_for_list` now also pre-warms the per-intent
  Lab→OI samplers when proofing is enabled.

### Fixed

- DeviceGray painted in a PDF/X DeviceCMYK page group now routes
  through the K plate (matches DeviceCMYK 0/0/0/(1−g) byte-for-byte).
- DeviceN images with a non-CMYK alternate space go through the
  overprint path so process plates aren't disturbed.
- Paired `/OP true /op true` ExtGStates are now treated as a
  "strict overprint" signal (matches Adobe Illustrator's emit).
- The custom-spot overprint dispatch and the parallel CMYK buffer's
  `is_custom_spot` heuristic both now require
  `process_cmyk.is_some()` so proofing-chain ICCBased RGB stays out.

### Crates published at 0.2.1

`stet`, `stet-cli`, `stet-fonts`, `stet-graphics`, `stet-core`,
`stet-ops`, `stet-engine`, `stet-render`, `stet-viewer`,
`stet-pdf-reader`, `stet-pdf`. The vendored `stet-tiny-skia` /
`stet-tiny-skia-path` forks remain at `0.11.4`. `stet-wasm` is
excluded from crates.io and bumped to `0.1.1` independently.

## [0.2.0] — 2026-05-01

This release lands a substantial expansion of the `stet-pdf-reader`
structural API, the PDF imaging-extension operators (transparency,
soft masks, optional content), and the `pdfmark` PostScript-to-PDF
authoring bridge. Several public match-surface enums are now
`#[non_exhaustive]` to lock in additive evolution — the breaking
changes are deliberate and documented per-crate below.

### ⚠ Breaking changes

This is a **breaking release**. Cargo treats the `0.1 → 0.2` bump as
incompatible (per the SemVer rules for `0.x`), so existing users
pinned at `stet = "0.1"` won't be auto-upgraded.

The breaking surface is concentrated in two places:

1. **`#[non_exhaustive]` markers** were added to ~40 public
   match-surface enums across `stet-graphics`, `stet-core`, and
   `stet-pdf-reader`. Any downstream `match` over `DisplayElement`,
   `PsError`, `Destination`, `AnnotationKind`, the various pdfmark
   record enums, etc. now requires a `_ => { ... }` wildcard arm.
   See the "Changed — public API breaking changes" subsection below
   for the complete list.

2. **`stet-pdf` no longer emits PDF/X-3 OutputIntents.** PDF output
   is now plain PDF 1.7. `PdfDevice::set_output_profile()` is
   `#[deprecated]` as a no-op; existing call sites compile but stop
   producing the (previously broken) PDF/X-3 conformance label.

For a typical downstream renderer that pattern-matches on
`DisplayElement`, the migration is one wildcard arm per `match`
site:

```diff
 match element {
     DisplayElement::Fill { .. } => { /* … */ }
     DisplayElement::Stroke { .. } => { /* … */ }
     DisplayElement::Image { .. } => { /* … */ }
+    _ => { /* fall through; new variants in 0.2.x are additive */ }
 }
```

The `#[non_exhaustive]` ratchet is intentional: it makes future
variant additions non-breaking, so 0.2.x → 0.3.x will be smaller.

### Added — `stet-pdf-reader` structural API

A read-only structural-content API for PDF inspection and tooling.
Every accessor parses lazily on first call and caches its result.

- `metadata()` — `/Info` dict (title, author, dates, …) and the
  catalog's `/Metadata` XMP stream.
- `viewer_preferences()` — page layout, page mode, print preferences,
  and reading direction hints.
- `outline()` — bookmark tree as `OutlineItem`s with
  destination/action resolution.
- `destinations()`, `resolve_named_destination(name)` — named
  destination table merged from `/Catalog /Dests` (legacy) and the
  `/Names /Dests` name tree.
- `page_annotations(page)` — typed `Annotation` list with
  destination/action resolution.
- `form()`, `form_fields()` — AcroForm field tree (text, choice,
  button, signature) with widget cross-references.
- `page_boxes(page)` — MediaBox / CropBox / BleedBox / TrimBox /
  ArtBox.
- `embedded_files()`, `embedded_file_bytes(name)` — `/EmbeddedFiles`
  name-tree walker.
- `layers()`, `layer(ocg_id)`, `configurations()`,
  `default_configuration()`, `layer_tree()`, `layer_set_for(intent)`
  — Optional Content Group (OCG) metadata, hierarchy, render-intent
  rules, and a runtime `LayerSet` for visibility overrides.
- `parse_warnings()` — diagnostic sink for non-fatal parse issues
  (broken outlines, bad name trees, malformed `/VE` expressions, …).
- New `stet inspect <file.pdf>` CLI subcommand surfaces the structural
  API at the command line.

See `docs/PDF-READER-API.md` and `docs/PDF-LAYERS.md` for full
references.

### Added — PDF imaging extensions

Display-list-level support for the PDF transparency and optional-content
imaging models, layered on top of the PostScript interpreter.

- **Alpha and blend modes**: `setblendmode`, `setfillalpha`,
  `setstrokealpha`, `setalphaisshape`. All 16 PDF blend modes.
- **Transparency groups**: `begintransparencygroup` /
  `endtransparencygroup` with `Knockout`, `Isolated`, and group
  colour space (`DeviceGray` / `DeviceRGB` / `DeviceCMYK` / ICC).
- **Soft masks**: `begintransparencymaskgroup` /
  `endtransparencymaskgroup` with `Alpha` and `Luminosity` subtypes,
  transfer functions, and backdrop-colour handling.
- **Optional Content (OCG)**: `setocg` / `endocg` operators wrap
  display-list content in `OcgGroup` elements with
  `OcgVisibility::Single` / `Membership` / `Expression` predicates.
  `LayerSet` (in `stet-graphics`) is the consumer's per-render override
  map; `render_page_to_rgba_with_layers` honours it.
- **Filters**: `JBIG2Decode` and `JPXDecode` for embedded image
  streams.

See `docs/PDF-EXTENSIONS.md` for the full reference and
`docs/PDF-LAYERS.md` for the runtime layer-visibility model.

### Added — `pdfmark` PostScript-to-PDF authoring

`pdfmark` operator dispatch in `stet-ops` (gated behind
`register_pdf_authoring_ops` so it's only visible to systemdict on the
PDF output path) plus matching emitters in `stet-pdf`. Five phases of
authoring support:

- `/DOCINFO` — document info dictionary (title, author, subject,
  keywords, creator, producer, dates, trapped).
- `/OUT` — outline (bookmark) tree authoring with destination /
  action targets.
- `/ANN` — Link, Text, FreeText annotations.
- `/DEST`, `/PAGE`, `/PAGES` — named destinations and per-page-box
  overrides.
- `/VIEWERPREFERENCES`, `/Metadata` — viewer preferences and
  document-level XMP metadata.
- `/Widget` and `/FORM` — AcroForm widget annotations and field-tree
  emission.
- `/EMBED`, JavaScript / Named actions, page-level `/AA` triggers.

See `docs/PDFMARK-AUTHORING.md` for the full reference.

### Added — colour management

- **Hand-rolled colorimetric A2B1 CLUT sampler**
  (`stet-graphics::icc::perceptual`). moxcms 0.8's `create_transform`
  pipeline over-saturates CMYK→sRGB output relative to lcms2 / Acrobat
  / Ghostscript on midtone colours; this module bypasses it for v2
  `lut16Type` CMYK profiles and matches lcms2 RelCol output to ±1 RGB
  level on a 17⁴ sweep against ISO Coated v2 300% (ECI). Out-of-gamut
  colours clip to the sRGB boundary (matching lcms2 / GS) so pure
  process primaries remain saturated. BPC is calibrated against the
  sampler's own (1, 1, 1, 1) output so K-heavy CMYK lands at the
  correct darkness. Profiles whose tables are mAB / mft1 fall back
  to the moxcms-driven bake.
- Soft-mask CMYK-domain blend gate widened to accept Group-wrapped
  flat CMYK fills (GWG 16.11 "Gradient Feather"). The GWG 16.10
  outer-glow protection still rejects on the inner Fill's blend-mode
  check.

### Changed — public API breaking changes

These match-surface enums are now `#[non_exhaustive]` so adding
variants is non-breaking for any consumer that includes a `_ =>` arm.
Existing consumers must add wildcard arms (or update their match
expressions) to keep building.

- `stet-graphics`: `DisplayElement`, `ImageColorSpace`,
  `ShadingColorSpace`, `SpotColorSpace`, `LineCap`, `LineJoin`,
  `FillRule`.
- `stet-core`: `PsError`, `FilterKind`, `RleState`.
- `stet-core::pdfmark`: `PdfMarkRecord`, `AnnotationSubtype`,
  `AnnotationTarget`, `OutlineDestination`, `OutlineAction`,
  `GoToTarget`, `ViewSpec`, `FieldType`, `FieldValue`, `DocDate`,
  `TrappedState`, `TzSign`, `LinkHighlight`, `TextAnnotationIcon`,
  `PageOverrideScope`.
- `stet-pdf-reader`: `PdfError`, `Destination`, `ViewSpec`, `Action`,
  `AnnotationDate`, `AnnotationKind`, `AnnotationColor`,
  `AnnotationKindData`, `FieldKind`, `ButtonType`, `FieldValue`,
  `TrappedFlag`, `PageLayout`, `PageMode`, `ReadingDirection`,
  `PrintScaling`, `Duplex`, `AfRelationship`, `ParsePhase`,
  `LocationHint`, `Severity`, `RenderIntent`, `LayerIntent`,
  `UsageState`, `PageElementSubtype`, `LayerTreeNode`, `BaseState`,
  `ListMode`, `AutoStateEvent`.

Param **structs** (`FillParams`, `StrokeParams`, `ImageParams`, the
pdfmark record structs, `Annotation`, `FormField`, `Layer`, etc.) are
**not** marked `#[non_exhaustive]` — adding fields lands additively
and consumers should pattern-match with `..` for forward
compatibility.

A `scripts/check-non-exhaustive.sh` audit runs in the local pre-push
hook; new public enums in the listed files must either carry the
marker or be allow-listed with a one-line justification. See the
"Stable extension points" section of CLAUDE.md and the per-doc
"Stability" sections of `docs/DISPLAY-LIST.md`,
`docs/PDF-READER-API.md`, and `docs/PDFMARK-AUTHORING.md`.

### Changed — other

- **`stet-pdf`**: removed the PDF/X-3 OutputIntent emission. The writer
  was emitting soft-mask transparency (prohibited by PDF/X-3) while
  labelling output as `PDF/X-3:2003` — a conformance conflict any
  preflight tool would flag. PDF output is now plain PDF 1.7 with no
  PDF/X conformance claim. A correct PDF/X-4 implementation is
  planned.
- **`stet-pdf`**: `PdfDevice::set_output_profile()` is `#[deprecated]`
  as a no-op. Retained for forward API compatibility with the planned
  PDF/X-4 work.
- **`stet-cli`**: `--width` / `--height` flags for PDF input override
  the page's MediaBox at render time.

### Added — documentation

- `docs/PDF-READER-API.md` — full reference for the structural API.
- `docs/PDF-LAYERS.md` — full reference for the OCG / layer API.
- `docs/PDF-EXTENSIONS.md` — full reference for the imaging extension
  operators and the JBIG2 / JPX filters.
- `docs/PDFMARK-AUTHORING.md` — full reference for the pdfmark
  authoring bridge.
- New **Rendering Correctness** section in the root README covering
  seam-free rendering on adjacent clipped regions and full overprint
  simulation.

## [0.1.2] — 2026-04-20

Backfilled 2026-08-27. This release was tagged and published but never written
up, which `scripts/check-tags.sh` caught when it began requiring a CHANGELOG
entry for every `vX.Y.Z` tag. Only `stet` and `stet-cli` were bumped; the
workspace version stayed at 0.1.0, which is why no other crate carries a 0.1.2.

### Fixed

- **`stet-cli` 0.1.0 and 0.1.1 shipped without embedded-resource
  registration** and were yanked from crates.io. The `stet` binary fell back
  to a PNG-writing rasterizer whenever a PostScript file was opened in the
  interactive viewer. 0.1.2 is self-contained; users of the earlier versions
  needed `cargo install stet-cli --force`. The `stet` library crate, used via
  `Interpreter`, was never affected.
- The root `resources/` tree was retired in favour of crate-local trees, then
  partially restored when `stet-cli` turned out to still read it at runtime;
  `stet-wasm` was pointed at the crate-local tree in the same pass.

There was no `v0.1.1` tag.

## [0.1.0] — 2026-04-18

Initial public release.

### PostScript interpreter

- Level 3 interpreter with ~320 operators covering stack, math, type,
  dict, array, string, control, file, graphics state, path construction,
  painting, clipping, colour, font, show, image, halftone/transfer,
  pattern/device, resource, and param categories.
- Arena + entity-indirection memory model with full save/restore (COW).
- Dual VM (local/global) with unified stores and `vm_alloc_mode`.
- Name interning via `NameTable`; dict version cache for O(1) name
  resolution on the hot path.
- Full Type 1, CFF/Type 2, Type 3, TrueType, and Type 42 (CID) font
  support with URW substitutions for the 35 standard PostScript fonts.
- Eexec, ASCIIHex, ASCII85, RLE, Flate, LZW, DCT, SubFile, and their
  encode counterparts as streaming filters.
- CIE-based colour spaces (A/ABC/DEF/DEFG) and ICC-based via moxcms.
- Smooth shading types 1–7 (function, axial, radial, triangle meshes,
  Coons/tensor patches) with native PS function evaluation.

### PDF reader (`stet-pdf-reader`)

- Self-contained PDF parser: xref (including xref streams), decryption
  (RC4/AES), object-stream decompression, page tree, resource
  resolution.
- Content-stream interpreter producing the same `DisplayList` type the
  PostScript interpreter produces — **no dependency on `stet-core`**.
- Transparency groups, soft masks (alpha & luminosity), tiling
  patterns, shadings 1–7.
- All standard stream filters including Flate (with PNG predictors),
  LZW, DCT (two backends), CCITT, JBIG2, JPEG 2000, ASCII85, ASCIIHex.
- Optional Content Groups (OCG) captured in the display list for future
  layer toggling.
- PDF OutputIntent profile honoured by default for PDF/X documents.
- CJK CMap loading (poppler-data / `STET_CMAP_DIR`).

### Rendering (`stet-render`)

- tiny-skia–based rasterizer (vendored as `stet-tiny-skia`) producing
  RGBA output.
- Banded rendering sized to L2 cache; rayon-parallel band processing.
- Clip fast path with rect detection, mask caching, and spare mask
  recycling.
- Viewport rendering: render any rectangular region of a display list
  at any zoom without re-interpreting the source.
- ICC-aware CMYK path with black-point compensation and per-pixel
  consistency checks for transparency-group blending.
- Overprint simulation (OPM 0/1), including strict OPM-1 "preserve
  zero components" semantics.
- Hairline and stroke-adjust handling for thin lines.

### PDF output (`stet-pdf`)

- Display list → PDF with embedded fonts (Type 1, TrueType, CFF),
  image compression, shadings, and transparency groups.
- Preserves native CMYK and spot colour spaces (Separation, DeviceN)
  without lossy RGB round-tripping.
- Pre-sampled transfer, halftone, and black-generation/UCR tables
  carried per paint element.
- Print-workflow quality output suitable for pre-press.

### Viewer & frontends

- `stet-viewer`: egui desktop viewer with pan/zoom, minimap,
  multi-page navigation, and drag-and-drop.
- On-demand viewport rendering: zoom/pan without re-interpretation.
- WASM frontend (`stet-wasm`, excluded from the main workspace):
  browser-side PDF viewer with viewport rendering and SIMD-enabled
  tiny-skia.

### Public library API (`stet` facade)

- `Interpreter::new()` / `Interpreter::builder()` for batteries-included
  PostScript rendering.
- `render()` → RGBA pages, `render_to_display_list()` → display lists,
  `render_to_pdf()` → PDF bytes, `exec()` → side-effects only.
- All 53 resources (fonts, encodings, CMaps, ICC profile) embedded in
  the binary via `include_bytes!`.
- Example programs: `render_ps`, `render_pdf`, `display_list`.

### Workspace

- 13 crates under Apache-2.0 OR MIT, plus two vendored tiny-skia forks
  (`stet-tiny-skia`, `stet-tiny-skia-path`) under BSD-3-Clause.
- `stet-pdf-reader` is intentionally independent of `stet-core` — it
  can be used as a standalone PDF parser/renderer without pulling in
  the PostScript VM.

[0.5.0]: https://github.com/AndyCappDev/stet/compare/v0.4.1...v0.5.0
[0.2.0]: https://github.com/AndyCappDev/stet/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/AndyCappDev/stet/releases/tag/v0.1.0
