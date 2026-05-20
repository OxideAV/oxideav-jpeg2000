# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

## Status — 2026-05-20 (clean-room round 1)

**Codestream-header parser only.** The crate now parses the
JPEG 2000 Part-1 main-header marker chain — `SOC`, `SIZ`, `COD`, and
`QCD` — and returns a fully-typed [`J2kHeader`] describing image
extent, tile layout, component count, sample precision/sign, wavelet
kernel, progression order, decomposition levels, and quantisation
style. Optional `CAP`, `PRF`, `COM`, `COC`, `QCC`, `RGN`, `POC`,
`PLM`, `PPM`, and `TLM` markers are recognised and skipped via their
length field.

What is **not** implemented yet:

* Tier-1 (EBCOT MQ-coder block coding).
* Tier-2 (packet-header walking, layer assembly).
* Inverse 5-3 and 9-7 wavelet transforms.
* Dequantisation (E.1 / E.2 reconstruction formulas).
* Multiple-component-transform (MCT, Annex G).
* Tile-part body reassembly.
* JP2 box-structured file format (ISO BMFF wrapper around the J2K codestream).
* HTJ2K Part-15 block coder.
* Any encoder path.

`decode_jpeg2000` and `encode_jpeg2000` still return
`Error::NotImplemented` and will until the body-decode path lands.

## Clean-room provenance

This module was written from scratch against the JPEG 2000 standards
documents under `docs/image/jpeg2000/` only. The specific sections
consulted:

* T.800 §A.4 (delimiting markers — SOC, SOT, SOD, EOC).
* T.800 §A.5.1 + Tables A.9 / A.10 / A.11 (SIZ).
* T.800 §A.6.1 + Tables A.12 / A.13 / A.14 / A.15 / A.16 / A.17 /
  A.18 / A.19 / A.20 / A.21 (COD).
* T.800 §A.6.4 + Tables A.27 / A.28 / A.29 / A.30 (QCD).

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
