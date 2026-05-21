# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

## Status — 2026-05-21 (clean-room round 4)

**Codestream-structural + JP2-wrapper parsers.** The crate parses
the JPEG 2000 Part-1 **main header** (`SOC`, `SIZ`, `COD`, `QCD`),
walks the **tile-part chain** (`SOT` / `SOD` / `EOC`), and decodes
the **JP2 ISO BMFF box wrapper** (Annex I).

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

What is **not** implemented yet:

* Tier-1 (EBCOT MQ-coder block coding).
* Tier-2 (packet-header walking, layer assembly).
* Inverse 5-3 and 9-7 wavelet transforms.
* Dequantisation (E.1 / E.2 reconstruction formulas).
* Multiple-component-transform (MCT, Annex G).
* Tile-part body decode (the walker delimits but does not decode).
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
