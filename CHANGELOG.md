# Changelog

All notable changes to `oxideav-jpeg2000` are recorded here.

## [Unreleased]

### Added

* **Clean-room round 6 (2026-05-22).** Per-tile + per-component
  coordinate-geometry derivation (`geometry` submodule, T.800 §B.2 /
  §B.3 / §B.5). New `geometry::derive_tile_geometry(siz, t)` takes a
  parsed `Siz` and a tile-grid index `t` (the `Isot` from a `SOT`
  marker) and returns a typed `TileGeometry { tile_index, p, q, tx0,
  ty0, tx1, ty1, components: Vec<TileComponentGeometry> }`. Reference-
  grid corners follow T.800 Equations B-6 (`p = t mod numXtiles`,
  `q = floor(t / numXtiles)`), B-7 (`tx0 = max(XTOsiz + p*XTsiz,
  XOsiz)`), B-8 (`ty0` symmetric), B-9 (`tx1 = min(XTOsiz +
  (p+1)*XTsiz, Xsiz)`), B-10 (`ty1` symmetric). Per-component bounds
  follow Equation B-12 with ceiling division (`tcx0 =
  ceil(tx0/XRsizi)`, etc.). `geometry::image_area(siz)` exposes the
  per-component image-area bounding box per Equation B-1 (`x0 =
  ceil(XOsiz/XRsizc)`, `x1 = ceil(Xsiz/XRsizc)`, …), and
  `geometry::tile_grid_extent(siz)` returns `(numXtiles, numYtiles)`
  per Equation B-5. `geometry::validate_siz(siz)` checks the
  inter-field invariants from Equations B-3 (`XTOsiz <= XOsiz`,
  `YTOsiz <= YOsiz`), B-4 (`XTsiz + XTOsiz > XOsiz`, `YTsiz + YTOsiz
  > YOsiz`), and §B.2's non-empty image-area requirement (`Xsiz >
  XOsiz`, `Ysiz > YOsiz`). Internal `ceil_div_u32` uses
  `(a + b - 1) / b` with `checked_add` overflow guard. Tile-grid
  arithmetic widens to `u64` for the `XTOsiz + (p+1)*XTsiz` term to
  preserve correctness on extreme-corner `XTsiz` values near
  `u32::MAX` before clipping back to `min(Xsiz)`. Sixteen new unit
  tests, all driven by spec-quoted numeric examples: image-area
  matches §B.4's two-component 1432×954 worked example (component 0
  → 1280×720 at (152, 234)..(1432, 954); component 1 → 640×360 at
  (76, 117)..(716, 477)); tile-grid extent matches §B.4's 4×4 = 16
  tiles; per-tile derivation matches §B.4's quoted tx0 / tx1 / ty0 /
  ty1 quartets across all sixteen tile indices; interior-tile
  per-component dims match §B.4's "interior tiles are 396×297 on
  component 0 but (198×148, 198×149) on component 1 depending on
  q-parity" observation; first-tile clamping to image offset and
  last-tile clamping to image extent both verified; out-of-range
  tile index rejected as `InvalidTilePartIndex`; single-tile
  single-component grid; three-to-one sub-sampling exercising the
  per-component ceiling-divide corner; and three `validate_siz`
  rejection cases (XTOsiz > XOsiz, XTsiz + XTOsiz <= XOsiz, empty
  image area). Eighty tests total pass (64 prior + 16 new); cargo
  fmt-check + clippy `-D warnings` clean (both default +
  `--no-default-features` builds). No new error variants — geometry
  failures are surfaced via the existing `Error::InvalidMarkerLength`
  (invariant violation) and `Error::InvalidTilePartIndex` (out-of-
  range `t`).

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (§B.2 — Equation B-1 / B-2 image-area + per-component bounds; §B.3
  — Equations B-3 / B-4 invariants, B-5 tile-grid extent, B-6 tile
  index to `(p, q)`, B-7 / B-8 / B-9 / B-10 per-tile reference-grid
  bounds, B-11 dimensions; §B.4 worked example for test corpus; §B.5
  — Equation B-12 / B-13 per-component tile mapping). No external
  library source — OpenJPEG, OpenJPH, Kakadu, FFmpeg, libavcodec,
  jpeg2000-rs, etc. — was consulted, quoted, paraphrased, or used
  as a cross-check oracle.

  Resolution-level + sub-band + precinct partitioning (T.800 §B.5
  Equation B-14 / Table B.1 for sub-band corners, §B.6 Equation B-16
  for precinct counts, §B.7 Equations B-17 / B-18 for code-block
  dims) and the §B.12 progression-order packet iterator lands in
  round 7.

* **Clean-room round 5 (2026-05-22).** Tier-2 packet-header reading
  primitives (`packet` submodule, T.800 §B.10). New
  `packet::PacketBitReader` implements the §B.10.1 bit-stuffing rule
  (MSB-first; after every `0xFF` byte the next byte's MSB is a
  stuffed zero, stripped on read). `packet::TagTree` is a stateful
  2-D hierarchical-minimum tag-tree decoder per §B.10.2: levels are
  built root-first by halving the leaf grid, each node carries a
  `(current_value, fully_decoded)` pair, and the
  `decode_below_threshold(x, y, T, reader)` / `decode_value(x, y,
  reader)` query forms commit only as many bits as needed and preserve
  causality across calls so adjacent code-blocks / layers do not
  re-read bits the spec already committed. `packet::decode_coding_passes`
  decodes the §B.10.6 / Table B.4 Huffman for 1..164 coding passes
  (`0` → 1; `10` → 2; `1100`/`1101`/`1110` → 3/4/5; prefix `1111`
  + 5 bits → 6..36; prefix `1111 11111` + 7 bits → 37..164).
  `packet::LblockState` + `packet::decode_segment_length` implement
  the §B.10.7.1 codeword-segment length read: leading `k` ones plus
  terminating zero increment `Lblock` by `k` (initial 3, monotone
  non-decreasing), then `(Lblock + floor(log2 passes))` bits encode
  the length. `packet::PrecinctState` + `packet::SubBandState`
  carry the per-(precinct, sub-band) inclusion + zero-bitplane tag
  trees, the per-block `already_included` flag, and the per-block
  `Lblock` state across the layers of one precinct's packet
  sequence; layout is initialised from the first packet's
  `PacketGeometry` and a mismatch on subsequent packets is
  rejected. `packet::decode_packet_header(bytes, geometry, state,
  sop_eph)` reads one full packet header per the §B.10.8 master
  order — zero-length bit; for each sub-band, for each code-block in
  raster order: inclusion-tag-tree query (or 1-bit signal if
  already included), zero-bitplane tag-tree value (on first
  inclusion only), coding-passes Huffman, Lblock increment + segment
  length — and returns a typed `PacketHeader { non_zero_length,
  contributions: Vec<CodeBlockContribution>, bytes_consumed,
  num_code_blocks }`. Optional SOP / EPH framing per `SopEphMode`
  (T.800 §A.8.1 / §A.8.2, COD `Scod` bits `0x02` / `0x04`).
  `packet::walk_packet_headers(body, packets, sop_eph)` composes the
  per-packet reader across a tile-part body (typically
  `TilePart::body_offset .. body_offset + body_len`): given a slice
  of `(precinct_index, PacketGeometry)` tuples in codestream order it
  decodes each header, advances `bytes_consumed + total_body_bytes`
  bytes for the packet's body, and returns `Vec<PacketHeader>`.
  Twenty-four new unit tests cover the bit reader (MSB-first ordering
  + `0xFF`-stuffing + pack/unpack round-trip), tag tree (1×1
  decode_value, 1×1 threshold partial + threshold true, state
  retention, 2×2 with shared root), the coding-passes Huffman
  across all three ranges (1..5, 6..36, 37..164), Lblock-incremented
  segment lengths (initial, +2 increment, multi-pass extra bits),
  packet-header happy paths (empty, single-block first inclusion,
  already-included one-bit, not-yet-included partial tag tree,
  three-sub-band packet at resolution > 0), two-packet walker
  retaining inclusion across layers, overrun rejection against a
  short body, SOP+EPH consumption, and precinct-state layout
  mismatch rejection. Sixty-four tests total pass; cargo fmt-check +
  clippy `-D warnings` clean (both default + `--no-default-features`
  builds). Two new error variants `Error::InvalidPacketHeader`
  (malformed bit sequence or geometry mismatch) and
  `Error::PacketHeaderOverrun` (walker exhausted body before
  geometry's packet count was satisfied). The codestream parser
  (rounds 1-3) and JP2 wrapper (round 4) are untouched.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (T.800 §B.10.1 — bit-stuffing, §B.10.2 + Figure B.12 — tag trees,
  §B.10.3 — zero-length packet bit, §B.10.4 — code-block inclusion,
  §B.10.5 — zero bit-plane information, §B.10.6 + Table B.4 —
  coding-passes Huffman, §B.10.7.1 — single codeword-segment
  length, §B.10.8 — master order, §A.8.1 — SOP marker, §A.8.2 —
  EPH marker). No external library source — OpenJPEG, OpenJPH,
  Kakadu, FFmpeg, libavcodec, jpeg2000-rs, etc. — was consulted,
  quoted, paraphrased, or used as a cross-check oracle when writing
  this module.

  Geometry computation (T.800 §B.6 precinct partitioning, §B.7
  sub-band → code-block partitioning, §B.12 progression-order
  iteration) lands in round 6; round 5 takes the geometry as caller
  input. §B.10.7.2 multi-codeword-segment splitting is also deferred
  — round 5 emits one segment length per included code-block.

* **Clean-room round 4 (2026-05-21).** JP2 ISO BMFF box wrapper
  parser (`jp2` submodule, T.800 / ISO/IEC 15444-1 Annex I). New
  `jp2::parse_jp2(&[u8]) -> Result<Jp2Container, Error>` walks the
  top-level box chain — `jP  ` signature (§I.5.1), `ftyp` (§I.5.2 /
  Tables I.3 / I.4), `jp2h` superbox (§I.5.3 / Figure I.7) carrying
  `ihdr` (§I.5.3.1 / Tables I.5 / I.6) + optional `bpcc` (§I.5.3.2 /
  Tables I.7 / I.8) + one or more `colr` (§I.5.3.3 / Tables I.9 /
  I.10 / I.11), and the first `jp2c` Contiguous Codestream box
  (§I.5.4) — into a typed `Jp2Container { ftyp: Ftyp, header:
  Jp2Header, codestream_offset, codestream_len }`. `Ftyp` preserves
  brand + minor version + the compatibility-list `CLi` entries and
  exposes `is_jp2_compatible()` (true iff one CLi is `'jp2 '`).
  `Ihdr` preserves the raw `BPC` byte plus convenience accessors
  `bit_depth()` / `is_signed()` / `varies_in_bit_depth()`. `Colr`
  decodes both enumerated (`METH = 1`, EnumCS 16 = sRGB, 17 =
  greyscale, 18 = sYCC, other = `Reserved(u32)`) and ICC-profile
  (`METH = 2`, raw bytes preserved) methods; reserved methods are
  accepted-and-skipped per T.800 §I.5.3.3. All three box-length
  encodings handled per T.800 §I.4: standard `LBox`, extended
  `LBox = 1` + 8-byte `XLBox`, and `LBox = 0` ("until end of file").
  Spec invariants enforced: `jp2h` first-child-is-`ihdr`, at most
  one `bpcc`, at least one `colr`, `bpcc` required when `BPC =
  0xFF`. Optional `xml ` / `jp2i` / `uuid` etc. boxes appearing
  between `ftyp` and `jp2c` are tolerated and skipped by length.
  Fourteen new unit tests against synthetic JP2 byte buffers
  covering happy path, ICC-profile colr, 3-component `bpcc`,
  extended-length `jp2c`, `LBox = 0` last-box framing, intermediate
  unknown box skip, plus rejection cases (missing signature, bad
  signature magic, missing ftyp, `BPC = 0xFF` without `bpcc`,
  `jp2h` with no `colr`, truncated box, reserved `LBox` value, and
  brand-compatibility recognition). Forty tests total pass; cargo
  fmt-check + clippy `-D warnings` clean. The codestream parser
  (rounds 1-3) is untouched.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (T.800 Annex I §I.4, §I.5.1, §I.5.2 + Tables I.3 / I.4, §I.5.3 +
  Figure I.7, §I.5.3.1 + Tables I.5 / I.6, §I.5.3.2 + Tables I.7 /
  I.8, §I.5.3.3 + Tables I.9 / I.10 / I.11, §I.5.4). No external
  library source consulted.

* **Clean-room round 3 (2026-05-21).** Typed tile-part marker parsers.
  Six new typed marker structs — `Coc` (T.800 §A.6.2), `Qcc`
  (§A.6.5), `Rgn` (§A.6.3), `Poc` + `PocProgression` (§A.6.6),
  `Plt` (§A.7.3), `Ppt` (§A.7.5) — plus a new `TilePartMarker` enum
  exposing them along with the existing `Cod` / `Qcd` and a `Com`
  catch-all (§A.9.2). `TilePart` now surfaces a
  `markers: Vec<TilePartMarker>` field carrying the marker chain
  parsed out of each tile-part header in codestream order; the
  walker no longer length-skips these segments. 8-bit vs 16-bit
  component-index width is selected from the codestream's `Csiz`
  per T.800 (`Csiz < 257` → 8 bits, `Csiz >= 257` → 16 bits) for
  COC, QCC, RGN, and POC. PLT decodes its `Iplt` 7-bit
  variable-length packet-length stream (T.800 Table A.36) into a
  `Vec<u32>`, validates that every PLT segment ends with a
  completed packet length (`A.7.3`), and rejects 32-bit overflow.
  `TilePart` is now `Clone` (no longer `Copy`) because it owns a
  `Vec` of marker payloads. Ten new unit tests covering COC, QCC,
  RGN, POC (with `CEpoc = 0` → 256 interpretation), PLT (single
  and multi-segment with distinct `Zplt`), PLT VLQ overrun
  rejection, PPT, full-marker-chain ordering across all 9 typed
  variants, and an out-of-range COC `NL` rejection. Twenty-six
  tests total pass.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (T.800 §A.6.2 / Table A.22 / A.23 / A.15 (COC), §A.6.3 / Table
  A.24 / A.25 / A.26 (RGN), §A.6.5 / Table A.31 (QCC), §A.6.6 /
  Table A.32 (POC), §A.7.3 / Table A.37 / Table A.36 (PLT), §A.7.5 /
  Table A.39 (PPT), §A.9.2 (COM)). No external library source
  consulted.

* **Clean-room round 2 (2026-05-21).** SOT / SOD tile-part walker.
  New `Sot` / `TilePart` / `J2kCodestream` types and
  `walk_tile_parts(bytes, header)` / `parse_codestream(bytes)` entry
  points return an ordered list of tile-parts with the parsed
  `(Isot, Psot, TPsot, TNsot)` quartet plus byte offsets of the SOT
  marker, SOD marker, and bit-stream body inside the input slice.
  Both fixed-`Psot` and `Psot == 0` ("body until EOC") framings are
  supported per T.800 §A.4.2. Tile-part-header markers are
  validated against T.800 Table A.2's per-header allow-list — main-
  header-only markers (`SOC`, `SIZ`, `CAP`, `PRF`, `CRG`, `TLM`,
  `PLM`, `PPM`) trigger `Error::UnexpectedMainHeaderMarker`; legal
  in-tile-part markers (`COD`, `COC`, `RGN`, `QCD`, `QCC`, `POC`,
  `PLT`, `PPT`, `COM`) are skipped by length. Nine new unit tests
  covering single/multi-tile-part happy paths, Psot-zero streaming,
  overrun rejection, missing-EOC, illegal-marker-in-tile-part, COM
  injection, wrong-Lsot, and offset reporting against synthetic
  buffers. Sixteen tests total pass.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (T.800 §A.2 / Table A.2 / §A.4.2 / Table A.5 / Table A.6 /
  §A.4.3 / Table A.7 / §A.4.4 / Table A.8). No external library
  source consulted.

* **Clean-room round 1 (2026-05-20).** Initial JPEG 2000 Part-1
  main-header parser: `SOC`, `SIZ`, `COD`, `QCD` marker segments are
  recognised, length-checked, and decoded into a typed `J2kHeader`
  struct (image extent, tile layout, per-component sample precision +
  sign + sub-sampling, progression order, decomposition levels,
  code-block geometry exponents, wavelet kernel, quantisation style,
  guard bits). Optional `CAP` / `PRF` / `COM` / `COC` / `QCC` / `RGN`
  / `POC` / `PLM` / `PPM` / `TLM` markers are skipped by length.
  Seven unit tests against synthesised byte buffers covering the
  happy path, multi-component case, optional-marker skip, missing
  `SOC` / `COD`, and invalid `Csiz`.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (ITU-T T.800 / ISO/IEC 15444-1, §A.4 / §A.5 / §A.6 — Tables A.4,
  A.9–A.11, A.12–A.21, A.27–A.30). No external library source
  consulted.

  `decode_jpeg2000` and `encode_jpeg2000` still return
  `Error::NotImplemented`; body-decode (tier-1, tier-2, wavelet,
  dequant, MCT) is queued for future rounds.

### Changed

* **Orphan rebuild (2026-05-20).** The crate was reset to a clean-room
  scaffold. The prior implementation contained module-level docstrings
  and inline comments whose provenance could not be defended against
  the workspace clean-room rule (no external library source as
  reference, not even as a sanity check). Per the workspace's
  Implementer-Round procedure, such audit failures are unrecoverable
  via incremental cleanup and require an orphan rebuild.

  No `old` branch is retained; long-standing audit failures forfeit
  the archive per workspace policy.
