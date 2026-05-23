# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

## Status — 2026-05-24 (clean-room round 8)

**Codestream-structural + JP2-wrapper + tier-2 packet-header reader +
SIZ-derived tile geometry + resolution-level / sub-band geometry +
precinct / code-block partition.**
The crate parses the JPEG 2000 Part-1 **main header** (`SOC`, `SIZ`,
`COD`, `QCD`), walks the **tile-part chain** (`SOT` / `SOD` / `EOC`),
decodes the **JP2 ISO BMFF box wrapper** (Annex I), reads the
**tier-2 packet-header bit stream** (T.800 §B.10), derives **per-tile
+ per-component coordinate geometry** from the SIZ marker (T.800 §B.2
/ §B.3 / §B.5 — Equations B-1..B-13), lifts each tile-component to
**per-resolution-level + per-sub-band geometry** using COD/COC's `NL`
(T.800 §B.5 — Equation B-14 for the resolution level corners, Equation
B-15 + Table B.1 for the sub-band corners), and now partitions each
resolution level into **precincts** (T.800 §B.6 — Equation B-16) and
its sub-bands into **code-blocks** (T.800 §B.7 — Equation B-17 / B-18)
from the COD/COC `PPx` / `PPy` and `xcb` / `ycb` exponents.

`parse_codestream` returns a `J2kCodestream` with the main header
plus an ordered `Vec<TilePart>`. Each `TilePart` carries its parsed
`Sot` (tile index, `Psot`, `TPsot`, `TNsot`), byte offsets of the
`SOT` marker, `SOD` marker, and bit-stream body inside the input
buffer, plus a `Vec<TilePartMarker>` of the **typed marker
segments** parsed out of the tile-part header between `SOT` and
`SOD`. Recognised tile-part-header markers parse into typed structs:

* `COD` → `Cod` (T.800 §A.6.1, override of main header)
* `COC` → `Coc` (T.800 §A.6.2, per-component coding-style override)
* `QCD` → `Qcd` (T.800 §A.6.4, quantisation override)
* `QCC` → `Qcc` (T.800 §A.6.5, per-component quantisation override)
* `RGN` → `Rgn` (T.800 §A.6.3, region-of-interest declaration)
* `POC` → `Poc` (T.800 §A.6.6, progression-order change list)
* `PLT` → `Plt` (T.800 §A.7.3, packet-length list, 7-bit VLQ decoded)
* `PPT` → `Ppt` (T.800 §A.7.5, opaque packet-header payload)
* `COM` → `Com(Vec<u8>)` (T.800 §A.9.2, comment payload verbatim)

8-bit vs 16-bit component-index width is selected automatically from
the codestream's `Csiz`. Markers forbidden in tile-part headers
(`SOC`, `SIZ`, `CAP`, `PRF`, `CRG`, `TLM`, `PLM`, `PPM`) are
hard-rejected. Both fixed-`Psot` and `Psot = 0` ("body until EOC")
tile-part framings are supported per T.800 §A.4.2.

`jp2::parse_jp2` walks an ISO BMFF box chain — `jP  ` signature,
`ftyp` (brand / minor version / compatibility list), `jp2h`
superbox (`ihdr` + optional `bpcc` + one or more `colr`), and
`jp2c` Contiguous Codestream — into a typed `Jp2Container` with
`codestream_offset` / `codestream_len` pointing at the slice that
callers may hand to `parse_codestream`. All three box length
encodings (standard `LBox`, extended `LBox = 1` + `XLBox`, and
"until end of file" `LBox = 0`) are supported per T.800 §I.4. `colr`
recognises enumerated (`METH = 1`, sRGB / greyscale / sYCC) and
ICC-profile (`METH = 2`, raw bytes preserved) methods; other
methods are accepted-and-skipped per T.800 §I.5.3.3.

`packet::decode_packet_header` (and the multi-packet
`packet::walk_packet_headers`) reads the bit-stuffed packet-header
bit stream described in T.800 §B.10 from a tile-part body, given a
caller-supplied `PacketGeometry` slice describing each packet's
sub-band → code-block layout. The reader composes the primitives
defined in the same submodule:

* `PacketBitReader` — MSB-first reader honouring §B.10.1's stuffed-
  zero-after-`0xFF` rule.
* `TagTree` — stateful 2-D hierarchical-minimum tag tree per §B.10.2;
  `decode_below_threshold` and `decode_value` cover the §B.10.4 /
  §B.10.5 query forms.
* `decode_coding_passes` — §B.10.6 / Table B.4 Huffman for 1..164
  passes.
* `LblockState` + `decode_segment_length` — §B.10.7.1 length read
  with the `Lblock`-increment prefix.
* `PrecinctState` + `SubBandState` — per-precinct carry across
  layers (inclusion + zero-bitplane trees + `already_included` flags
  + per-block `Lblock`).
* Optional `SopEphMode` for SOP / EPH framing around each packet.

`PacketHeader` carries `non_zero_length`, the per-code-block
`Vec<CodeBlockContribution>` (`included` / `zero_bit_planes` /
`coding_passes` / `segment_lengths`), `bytes_consumed`, and
`num_codeblocks`.

`geometry::derive_tile_geometry(siz, t)` derives the geometry of tile
`t` (the `Isot` value from a `SOT` marker) directly from a parsed
[`Siz`] per T.800 §B.3 — Equations B-6 (`p = t mod numXtiles`, `q =
t / numXtiles`), B-7 / B-8 / B-9 / B-10 (`tx0(p,q) = max(XTOsiz +
p·XTsiz, XOsiz)`, `tx1(p,q) = min(XTOsiz + (p+1)·XTsiz, Xsiz)` and
symmetrically for y), and per-component bounds per §B.5 Equation B-12
(`tcx0 = ceil(tx0/XRsizi)`, etc.). Returned `TileGeometry` carries
`(p, q)`, the reference-grid corners `(tx0, ty0, tx1, ty1)`, and one
`TileComponentGeometry { tcx0, tcy0, tcx1, tcy1 }` per component in
SIZ-declaration order. `geometry::image_area(siz)` exposes the
whole-image per-component bounding box per Equation B-1, and
`geometry::tile_grid_extent(siz)` returns the `(numXtiles, numYtiles)`
pair from Equation B-5. `geometry::validate_siz(siz)` enforces the
inter-field invariants from Equations B-3 / B-4 plus the §B.2
non-empty image-area requirement. The §B.4 worked example (two
components, 1432×954 reference grid, (1,1) and (2,2) sub-sampling,
4×4 tile grid with the spec-quoted tx/ty quartet) drives the
test suite.

`geometry::derive_resolution_levels(tc, NL)` lifts one
`TileComponentGeometry` to a `Vec<ResolutionLevel>` of length `NL + 1`
covering resolution levels `r = 0..=NL`. Each `ResolutionLevel`
carries its own `(trx0, try0, trx1, try1)` per Equation B-14
(`trx0 = ceil(tcx0 / 2^(NL - r))`, etc.) plus a `Vec<SubBand>` whose
membership follows §B.5's lead-in: `r = 0` carries **one** sub-band
with orientation `LL` (the "NLLL" band; `nb = NL`), while `r ≥ 1`
carries **three** sub-bands with orientations `HL`, `LH`, `HH` at
decomposition level `nb = NL - r + 1`. Each `SubBand` records
`(tbx0, tby0, tbx1, tby1)` per Equation B-15
(`tbx0 = ceil((tcx0 - 2^(nb-1)·xob) / 2^nb)`, symmetrically for the
other corners), with the orientation displacements `(xob, yob)`
looked up from Table B.1 (`LL = (0, 0)`, `HL = (1, 0)`, `LH = (0, 1)`,
`HH = (1, 1)`). Sub-band corner math runs in signed `i64` to surface
the `tcx0 - 2^(nb-1)·xob < 0` corner (clamped to zero per §B.5's
implicit non-negativity assumption). `NL = 0` collapses to a single
`r = 0` level with one full-tile-component LL band; `NL = 32` (the
Table A.15 upper bound) is handled without overflow via 64-bit
intermediates.

`geometry::derive_precinct_partition(level, exponents)` counts the
precincts spanning one `ResolutionLevel` per T.800 §B.6 / Equation
B-16: `numprecinctswide = ceil(trx1/2^PPx) - floor(trx0/2^PPx)` when
`trx1 > trx0` (else 0), symmetrically for `numprecinctshigh`, returning
a `PrecinctPartition { exponents, num_wide, num_high }` whose
`num_precincts()` is `num_wide * num_high`. The partition is anchored
at `(0, 0)` on the reduced-resolution domain, so the origin term is a
**floor** (an offset tile-component can straddle one extra precinct
cell). `geometry::precinct_exponents_at(precincts, r)` reads the
`(PPx, PPy)` in force at resolution level `r` from a `COD` / `COC`
precinct byte vector per Table A.21 (low nibble = `PPx`, high nibble =
`PPy`); an empty vector means maximum-precinct mode and returns the
Table A.13 default `PPx = PPy = 15`.
`geometry::derive_code_block_dimensions(r, xcb, ycb, exponents)`
applies the §B.7 clamp (Equation B-17 / B-18):
`xcb' = min(xcb, PPx - 1)` at `r = 0`, `min(xcb, PPx)` at `r > 0`
(symmetrically for `ycb'`), returning `CodeBlockDimensions { xcb,
ycb }` with `width()` / `height()` = `2^xcb'` / `2^ycb'`. `xcb` /
`ycb` are the **real** exponents (Table A.18 stored byte `+ 2`); the
`PP - 1` shave at `r = 0` is a saturating subtraction so the
Table-A.21-legal NLLL-band `PP = 0` clamps to a `1×1` partition.

What is **not** implemented yet:

* Tier-1 (EBCOT MQ-coder block coding) — the packet-header reader
  reports byte ranges per code-block, but the codeword bytes are not
  yet decoded.
* Precinct → code-block **enumeration** (which code-blocks of a
  sub-band fall in a given precinct, clipped to both bounds) + §B.12
  progression-order packet iteration (Equation B-20 / B-21). Round 8
  closes the precinct / code-block **counts** (§B.6 / §B.7); round 9
  will bridge those counts to the round-5 `packet` reader's
  `PacketGeometry` input and emit the packet-precinct sequence for
  each progression order, plus §B.8 layer / §B.9 packet assembly.
* §B.10.7.2 multi-codeword-segment splitting (round 5 emits one
  segment length per included code-block; termination boundaries are
  a tier-1 input we don't have yet).
* Inverse 5-3 and 9-7 wavelet transforms.
* Dequantisation (E.1 / E.2 reconstruction formulas).
* Multiple-component-transform (MCT, Annex G).
* `pclr` / `cmap` / `cdef` / `res` JP2 boxes (skipped silently;
  `jp2h` enforces `ihdr` first + at least one `colr` only).
* HTJ2K Part-15 block coder.
* Any encoder path.

`decode_jpeg2000` and `encode_jpeg2000` still return
`Error::NotImplemented` and will until the body-decode path lands.

## Clean-room provenance

This module was written from scratch against the JPEG 2000 standards
documents under `docs/image/jpeg2000/` only. The specific sections
consulted:

* T.800 §A.4 (delimiting markers — SOC, SOT, SOD, EOC) +
  Tables A.4 / A.5 / A.6 / A.7 / A.8.
* T.800 §A.5.1 + Tables A.9 / A.10 / A.11 (SIZ).
* T.800 §A.6.1 + Tables A.12 / A.13 / A.14 / A.15 / A.16 / A.17 /
  A.18 / A.19 / A.20 / A.21 (COD).
* T.800 §A.6.2 + Tables A.22 / A.23 (COC).
* T.800 §A.6.3 + Tables A.24 / A.25 / A.26 (RGN).
* T.800 §A.6.4 + Tables A.27 / A.28 / A.29 / A.30 (QCD).
* T.800 §A.6.5 + Table A.31 (QCC).
* T.800 §A.6.6 + Table A.32 (POC).
* T.800 §A.7.3 + Tables A.37 / A.36 (PLT — Iplt 7-bit VLQ decoding).
* T.800 §A.7.5 + Table A.39 (PPT).
* T.800 §A.2 / Tables A.2 / A.3 (per-header marker allow-lists used
  to validate the tile-part walker).
* T.800 Annex I (JP2 file format) — §I.4 + Figure I.4 / Table I.1
  (binary box layout), §I.5.1 (Signature box), §I.5.2 + Tables I.3
  / I.4 (File Type box), §I.5.3 + Figure I.7 (JP2 Header superbox),
  §I.5.3.1 + Figure I.8 / Tables I.5 / I.6 (Image Header box),
  §I.5.3.2 + Tables I.7 / I.8 (Bits Per Component box), §I.5.3.3 +
  Figure I.10 / Tables I.9 / I.10 / I.11 (Colour Specification
  box), §I.5.4 (Contiguous Codestream box).
* T.800 §B.10 (Packet header information coding) — §B.10.1 (bit-
  stuffing routine), §B.10.2 + Figure B.12 (tag trees), §B.10.3
  (zero-length packet bit), §B.10.4 (code-block inclusion — partial
  tag tree on first inclusion, 1-bit signal thereafter), §B.10.5
  (zero bit-plane information tag tree), §B.10.6 + Table B.4
  (codewords for number of coding passes), §B.10.7.1 (`Lblock`-
  based single codeword-segment length), §B.10.8 (master order of
  information within a packet header), §A.8.1 / §A.8.2 (SOP / EPH
  framing markers).
* T.800 §B.2 (Image area definition — Equation B-1 / B-2 per-component
  bounding box on the component domain), §B.3 (Image area division
  into tiles and tile-components — Equations B-3 / B-4 inter-field
  invariants, Equation B-5 tile-grid extent, Equation B-6 tile-index
  to `(p, q)`, Equations B-7 / B-8 / B-9 / B-10 per-tile
  reference-grid bounds, Equation B-11 tile dimensions), §B.5
  (Transformed tile-component division — Equation B-12 per-component
  tile mapping, Equation B-13 tile-component dimensions, Equation
  B-14 resolution-level corners, Equation B-15 sub-band corners,
  Table B.1 sub-band orientation displacements `(xob, yob)`), §B.4
  worked example (1432×954 reference grid, 4×4 tile grid, two
  components with (1,1) and (2,2) sub-sampling, asymmetric
  ceiling-divide on the y-axis for the sub-sampled component), §B.6
  (Division of resolution levels into precincts — Equation B-16
  precinct count, precinct partition anchored at `(0, 0)` so the
  origin term is a floor; Table A.13 maximum-precinct `PPx = PPy = 15`
  default; Table A.21 precinct-byte nibble layout, low = `PPx`, high =
  `PPy`), §B.7 (Division of the sub-bands into code-blocks — Equation
  B-17 / B-18 effective code-block exponents `xcb'` / `ycb'` clamped to
  the precinct, code-block partition anchored at `(0, 0)`, §B.7 NOTE on
  code-blocks extending past the sub-band edge; Table A.18 code-block
  exponent `xcb = value + 2`).

No external library source — OpenJPEG, OpenJPH, Kakadu, FFmpeg, etc.
— was consulted, quoted, paraphrased, or used as a cross-check
oracle. Black-box `opj_compress` / `opj_decompress` / `ojph_compress`
/ `ojph_expand` invocations remain on the allow-list for future
round body-decode validation, but were not invoked in round 1
(synthetic-byte-buffer tests cover the marker-parser surface).

## Planned future rounds

The clean-room rebuild will continue against:

* ITU-T Rec. T.800 | ISO/IEC 15444-1 — JPEG 2000 Part 1 (core).
* ITU-T Rec. T.801 | ISO/IEC 15444-2 — Part 2 (extensions).
* ISO/IEC 15444-15 — High-Throughput JPEG 2000 (HTJ2K).
* ITU-T Rec. T.814 | ISO/IEC 15444-15 supporting material.
* Black-box invocations of the validator binaries above.

## License

MIT. See `LICENSE`.
