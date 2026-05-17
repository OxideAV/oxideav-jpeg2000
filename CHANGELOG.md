# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- decoder: Region of Interest (RGN) Maxshift method (T.800 §A.6.3 +
  Annex H). The codestream parser now exposes parsed `Rgn` segments
  (`Crgn`, `Srgn`, `SPrgn`) via `Codestream::rgn` for main-header
  segments and `TilePart::rgn` for per-tile-part segments; tile-part
  RGNs override the main-header RGN for the same component on a
  per-tile basis. The decoder threads the resulting per-component
  shift `s` through `DecodeParams::roi_shifts` and applies a per-
  codeblock Maxshift correction in both `synth_component_53` and
  `synth_component_97`: a codeblock whose `missing_msb < s` has
  exercised the extra `s` bit-planes (i.e. the encoder either
  upshifted ROI samples or applied OpenJPEG's component-wide
  "Component of Interest" mode), so its `band_numbps` is bumped by
  `s` and the post-T1 magnitude is right-shifted by `s`. Codeblocks
  with `missing_msb >= s` decode unchanged. New `tests/rgn_decode.rs`
  pins eight integration cases against `opj_compress -ROI`-generated
  fixtures: constant Gray8 (U=4, U=8), gradient Gray8 (U=4, U=8 —
  both bit-exact lossless), RGB+MCT with ROI on luma (bit-exact
  lossless), and a 9/7 irreversible RGN-on-Gray fixture (max
  deviation ≤ 4 LSB against the `opj_decompress` reference). The
  parser tests confirm `Rgn { crgn, srgn=0, sprgn }` is populated
  correctly for main-header RGN markers.

### Fixed

- 5/3 lossless encoder produced unrecoverable streams for any input
  whose tile-component band needed more than one code-block per
  precinct (e.g. `1x129` / `129x1` / `2x129` RGB at the default 64x64
  cblk). Two underlying bugs in `encode/tile.rs`:
  1. The tier-2 emitter wrote ALL inclusion bits, then ALL
     zero-bitplane bits, then ALL num-passes / Lblock / length bits —
     while the matching decoder reads those four streams INTERLEAVED
     per code-block (T.800 §B.10.4 / §B.10.7 / §B.10.8 / §B.10.9). With
     >= 2 cblks per precinct the streams diverged and every packet
     decoded as empty.
  2. The zero-bitplane tag tree was emitted as one independent
     `OneLeafTree` per code-block, while the decoder uses a SHARED
     per-precinct multi-leaf `TagTree` (T.800 §B.10.7) — the shortcut
     happened to match decoder output only when the precinct held a
     single cblk.
  Fixed by interleaving the four per-cblk bit streams and replacing
  `OneLeafTree` with the shared `TagTreeEnc`. New regression tests in
  `tests/roundtrip_53.rs` pin `1x129`, `129x1`, and a 16x16-cblk small
  variant. Caught by `jp2_lossless_self_roundtrip` libFuzzer harness.
- 5/3 (and 9/7) encoder skipped the forward DWT entirely whenever
  **either** axis was < 2 (e.g. `2x1`, `1xN`) while still signalling
  `num_decomp = 1` in COD — the decoder then ran a full inverse DWT on
  raw level-shifted samples and produced garbage. Per T.800 §F.4.2 the
  1-D analysis on a length-1 axis is a no-op, so the fix is to keep
  applying the level as long as **either** axis has length >= 2 (the
  `[LL, HL]` / `[LL; LH]` band layout matches what `build_subbands`
  already produces). Caught by both fuzz harnesses.

## [0.0.10](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.9...v0.0.10) - 2026-05-08

### Other

- HTJ2K encoder round 6: SigProp + MagRef passes wired into codestream
- rustfmt the `__oxideav_entry` re-export line
- drop dead `linkme` dep
- re-export __oxideav_entry from registry sub-module
- jpeg2000 encoder round 5: explicit precincts + HTJ2K SigProp/MagRef + 9/7 QCD clarity
- registry calls: rename make_decoder/make_encoder → first_decoder/first_encoder
- auto-register via oxideav_core::register! macro (linkme distributed slice)
- HTJ2K encoder round 4 — 9/7 + multi-tile + sub-sampled chroma + PPM/PPT (task #477)
- unify entry point on register(&mut RuntimeContext) ([#502](https://github.com/OxideAV/oxideav-jpeg2000/pull/502))
- add register_containers for .j2k/.jp2/.jpf/.jpx/.jpm/.j2c/.jpc

### Added

- HTJ2K encoder round 6: SigProp + MagRef refinement passes wired into
  the codestream. New `EncodeOptionsHt::pass_count: HtPassCount` selector
  (`Cleanup` / `CleanupSigprop` / `CleanupSigpropMagref`) drives the
  per-codeblock pipeline through `encode_sigprop` + `encode_magref` (the
  primitives have existed since rounds 4-5 but were not exercised by the
  packet emitter). Per-codeblock the encoder runs the cleanup pass to
  produce `Dcup`, then constructs an in-memory `CleanupOutput`
  equivalent from the sample magnitudes and runs the SigProp + MagRef
  encoders with all-zero refinement bits to obtain `Dref`. The packet
  body emits two codeword segments — `Dcup` (cleanup) followed by
  `Dref` (refinement) — and the packet header writes
  `num_passes ∈ {1, 2, 3}` plus two length fields when `num_passes ≥ 2`
  (widths `lblock + ⌊log2(passes_added)⌋` per ISO/IEC 15444-15 §B.3 +
  T.800 §B.10.7.2). lblock growth now picks the smallest value that
  fits both length fields. New `tile_enc::tests` self-roundtrip tests
  cover Z_blk = 2 and Z_blk = 3 across solid, sparse, RGB-with-MCT, and
  multi-tile inputs; `htj2k_encoder.rs` integration tests add
  `ojph_expand` cross-decode validation that the new `Z_blk = 2` /
  `Z_blk = 3` codestreams are accepted bit-exactly by OpenJPH.
- encoder (classic Part-1, round 5): three encoder improvements.
  - **Explicit per-resolution precinct sizes** (`EncodeOptions::precincts`).
    New `precincts: Option<Vec<(u8, u8)>>` field on `EncodeOptions`
    carries one `(PPx, PPy)` pair per resolution (from LL outward,
    `num_decomp + 1` entries). When `Some`, the COD marker sets
    `Scod` bit 0 = 1 and appends one `(PPy<<4)|PPx` byte per
    resolution immediately after the fixed 10-byte `SPcod` block,
    conformant with T.800 §A.6.1 Table A.13. When `None` (the
    default), the encoder omits the precinct-size bytes and decoders
    assume one giant precinct per resolution (`PPx=PPy=15`). Two new
    integration tests in `encode_progression.rs` verify the COD byte
    layout and round-trip correctness via our decoder and
    `opj_decompress`.
  - **9/7 QCD self-consistency** (`build_97_band_params`): clarified
    that `enc_stepsize_b = 2^(precision - eps_b)` must match the
    decoder's dequantisation scale `q · stepsize_dec` (rather than
    the old erroneous per-band log2_gain factor). The encoder uses
    `eps_b = precision` for all bands (uniform `stepsize = 1.0`),
    matching the decoder's `Rb = precision` convention and yielding
    > 47 dB PSNR on a 64×64 gray gradient, > 43 dB for RGB+ICT.
    PSNR thresholds in `roundtrip_97.rs` updated from 30 dB to
    43 / 40 dB respectively.
  - **HTJ2K SigProp pass encoder** (`encode::htj2k::sigprop_enc`).
    New `encode_sigprop(cleanup, ref_mag, ref_sign)` function walks
    quad-scan order, checks the 8-neighbourhood `mbr` flag, and
    emits one magnitude bit per eligible not-yet-significant sample
    using `MagSgnWriter` (forward LSB-first with MagSgn stuffing).
    Sign bits follow immediately for newly-significant samples.
    Exported as `SigPropEncOutput { z, sign, bits }` (the `bits`
    field is the forward portion of `Dref`).
  - **HTJ2K MagRef pass encoder** (`encode::htj2k::magref_enc`).
    New `encode_magref(cleanup, ref_bit)` function collects one
    refinement bit per already-significant sample and packs them
    into the reverse VLC byte stream (last-byte > 0x8F stuffing
    rule, then byte-reversed). The result is the tail portion of
    `Dref`, ready for concatenation after the SigProp bytes.
  - `encode::htj2k::mod` registers both `sigprop_enc` and
    `magref_enc` as public submodules.

- encoder (HTJ2K, round 4 — task #477): four new sub-features land
  cleanly in one drop.
  - **9/7 irreversible transform** path. New `HtTransform` enum
    selector on `EncodeOptionsHt`; the encoder applies forward 9/7
    lifting (`encode::dwt::fdwt_97`), per-band scalar quantisation
    with `eps_b = precision`, `mu = 0` (so `stepsize_b = 1`), and
    emits the QCD in expounded form (qntsty = 2). The transform byte
    in COD is set to 0 for irreversible. Self-roundtrip MAD ≤ 2 LSB
    on a 64×64 gray gradient at NL=2; `ojph_expand` cross-decodes the
    32×32 solid-DC fixture within ±2 LSB.
  - **Multi-tile codestream output** via the new `tile_size:
    Option<(u32, u32)>` knob. Per-tile forward DWT + tier-1 + tier-2
    are completely independent; the SOT/SOD pair is repeated per tile
    in raster order with the right `Isot` / `Psot` values. Self-
    roundtrip is bit-exact on a 64×64 image with `XTsiz=YTsiz=32`
    (4-tile grid) and on a non-aligned 48×48 image with the same
    tile size.
  - **Sub-sampled chroma input** for `Yuv420P` (chroma at half H+V)
    and `Yuv422P` (chroma at half H). SIZ writes per-component
    `(XRsiz, YRsiz)`; per-component DWT runs at the sub-sampled
    extent. Forward MCT is rejected (and surfaced as
    `Error::Unsupported`) for sub-sampled input, since the RCT
    requires same-sized R/G/B and is meaningless for chroma at half
    resolution.
  - **PPM / PPT packed packet headers** via the new
    `HtPacketHeaderPlacement` knob (`Inline` (default) /
    `PackedMainHeader` / `PackedPerTilePart`). The encoder builds
    the inline-headers tile body first, then re-routes per-tile
    headers through the existing classic-encoder
    `decode::tile::split_packet_headers` helper into either a single
    PPM segment in the main header or one PPT segment per tile-part.
  - The HT decoder's tier-2 driver (`decode::htj2k::decode_frame_htj2k`)
    is extended in-place to support multi-tile codestreams (Isot
    grouping per T.800 §B.3) and PPM/PPT packed packet headers
    (separate `header_cursor` consumed by `parse_packet`).
  - 4 new self-roundtrip tile_enc tests (multi-tile 2×2 / non-aligned,
    multi-tile RGB+MCT, PPM, PPT) plus 9/7 single-tile + 9/7 gradient
    self-roundtrip, plus 1 new `ojph_expand` cross-decode for 9/7
    solid-DC.
  - `EncodeOptionsHt` gains `transform: HtTransform`, `tile_size:
    Option<(u32, u32)>`, `packet_header_placement:
    HtPacketHeaderPlacement` fields. All field defaults preserve
    round-3 behaviour (`Reversible53`, `None`, `Inline`).
  - Encoder gaps remaining: SigProp/MagRef refinement passes (Z_blk
    ∈ {2, 3}), multi-layer, multi-set HT (T.814 Annex B), constrained
    sets (T.814 §8), POC progression schedule.

## [0.0.9](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.8...v0.0.9) - 2026-05-05

### Other

- HTJ2K encoder round 3 — multi-component RGB + MCT (RCT) (task #477)

### Added

- encoder (HTJ2K, round 3 — task #477): multi-component encode for
  `Gray8`, `Rgb24`, and `Yuv444P` input pixel formats with optional
  forward 5/3 reversible component transform (RCT, T.800 §G.1) for RGB.
  - SIZ now writes `Csiz = N` (1 or 3) with the matching per-component
    sub-sampling `(XRsiz, YRsiz)` fields. The tier-2 packet emit loop
    walks `(resolution, component)` in LRCP order, one packet per tuple.
  - COD signals `MCT = 1` when the encoder applies forward RCT to
    `Rgb24` input (Y = (R + 2G + B) >> 2, Cb = B - G, Cr = R - G); the
    crate's HTJ2K decoder already inverts the RCT for 5/3 + MCT = 1
    streams.
  - QCD epsilons are bumped by one bit when MCT is active to give the
    chroma's extra dynamic range room (Cb / Cr can hit ±255 from 8-bit
    RGB input). For luma this is over-allocation but the cleanup pass
    still round-trips bit-exactly.
  - 5 new self-roundtrip + 2 new `ojph_expand` cross-decode integration
    tests cover RGB+MCT (32×32 NL=1, 64×64 NL=2), RGB no-MCT, and
    `Yuv444P` planar input.
  - `EncodeOptionsHt` gains a `use_color_transform: bool` field
    (defaults to `true`).

## [0.0.8](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.7...v0.0.8) - 2026-05-05

### Other

- HTJ2K encoder: refresh module doc to reflect round 2 scope
- HTJ2K encoder round 2 — multi-significance + 5/3 DWT plumbing (task #477)
- place HTJ2K encoder round 1 entry under [Unreleased]
- HTJ2K encoder bootstrap (round 1) — task #456

### Added

- encoder (HTJ2K, round 2 — task #477): multi-significance per quad,
  forward 5/3 DWT plumbing for `NL ∈ [0, 5]`, and the §7.3.6 Eq-4
  first-line-pair both-`u_off=1` special case.
  - `cxt_vlc_enc::pick_emb_for_uoff1` searches Annex C for a row whose
    `(emb_k, emb_1)` mask is compatible with the per-sample
    `bit(bigu - kbit_j, v_j) == ibit_j` constraint; the cleanup encoder
    falls back to `(u_off=0, emb_k=0, emb_1=0)` (universally available
    across `(cq, ρ)`) when `kappa >= max bit_len(v_j)`.
  - `cleanup_enc::pick_quad_plan` picks the per-quad plan; multi-sig
    ρ ∈ {3, 5, 6, 7, 9..15} now round-trips through the same crate's
    decoder.
  - First-line-pair Eq-4 path: when both quads of a pair need
    `u_off=1`, the encoder picks between `s=1` (Eq 4, both `u >= 3`)
    and `s=0` (with optional q2 prefix collapse when `u_q1 > 2` and
    `u_q2 ∈ {1, 2}`).
  - `tile_enc` now wires `crate::encode::dwt::fdwt_53` level-by-level,
    builds per-resolution / per-band / per-codeblock layouts via the
    decoder's `build_subbands` helper, and emits one tier-2 packet per
    resolution covering all bands of that resolution. QCD signals
    `1 + 3 * NL` bands with the canonical reversible eps_b values
    (LL = precision, HL/LH = precision + 1, HH = precision + 2).
  - Reverse-VLC stuffing rule fixed: the encoder now mirrors the
    decoder's predicate exactly (only force bit-7 = 0 in the next
    byte when `prev > 0x8F` AND the next 7 input bits are all 1),
    eliminating spurious bit insertions.
  - 11 new self-roundtrip + 2 new `ojph_expand` cross-decode
    integration tests cover NL=0..3 with sparse/dense/gradient
    fixtures.

- encoder (HTJ2K, round 1): minimum-viable HT cleanup-pass encoder.
  New `encode::htj2k::encode_image_htj2k` plus the
  `encode::EncodeOptionsHt` knob set produce a Part-15 codestream
  (SOC + SIZ-with-Rsiz-bit-14 + CAP/Pcap15 + COD-with-SPcod-cblk_style-bit-6
  + QCD + SOT/SOD/EOC) for a single 32×32 Gray8 luma codeblock at
  NL=0, 1 quality layer, LRCP. Internally:
  - `MagSgnWriter` / `MelWriter` / `VlcWriter` mirror the §7.1
    forward / forward / reverse bit-stream readers, including the
    `0xFF` MSB-zero stuffing rule and the reverse-VLC `>0x8F` /
    low-7-bits-all-1 stuffing predicate.
  - `mel_enc` walks an internal MEL state machine to emit the long-run
    / short-run branches of T.814 §7.3.3.
  - `uvlc_enc::split_u` implements Table 3 prefix/suffix/extension
    width selection (covers `u ∈ [0, 91]`).
  - `cxt_vlc_enc::encode_cxt_vlc` looks up the Annex C codeword for
    a `(cq, ρ, u_off, ε^k, ε^1)` tuple and emits the bits LSB-first.
    The Annex C tables are the same `CXT_VLC_TABLE_0` / `_1` arrays
    the decoder consumes (now `pub(crate)` for cross-side reuse) — no
    duplication, no third-party transcription.
  - `cleanup_enc::encode_cleanup` walks the codeblock quad-by-quad
    in §7.3.5 row-pair scan order, computes `cq` / `κ_q` exactly as
    the decoder does, and emits the dual MagSgn + MEL + VLC streams
    plus the trailing 12-bit `Scup` reservoir into `Dcup`.
  - `tile_enc` wraps the cleanup segment in the round-1 marker chain
    plus a hand-built tier-2 packet header (1×1 inclusion + 1×1
    zero-bit-plane tag-trees, comma-coded `num_passes = 1`, adaptive
    `Lblock` growth).
  Verified by `tests/htj2k_encoder.rs`: 32×32 solid-DC and 32×32
  sparse (two ±1 samples in their own quads) self-roundtrip
  bit-exactly, AND `ojph_expand` (binary, NOT source) cross-decodes
  both fixtures bit-exactly. Unit-test sweep covers each substream
  writer + every Annex C entry's codeword bit-pattern + cleanup-pass
  small-codeblock round-trips through the FBCOT decoder.

## [0.0.7](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.6...v0.0.7) - 2026-05-04

### Other

- HTJ2K round 9 — multi-band 9/7 fixture + pblk algebraic unit-test sweep
- HTJ2K round 8 — wire pblk into 9/7 dequant, drop spurious 0.5 multiplier
- move HTJ2K round 7 + standalone shape under [Unreleased]
- HTJ2K round 7 — three cleanup-pass bugs unblocking HF-band magnitude convergence

### Added

- decoder (HTJ2K, round 9): multi-band 64×64 5-decomposition-level 9/7
  fixture (`htj2k_lossy97_64x64_nl5_lrcp.j2c`, with paired
  `_input.pgm` + `_opj_ref.pgm`) closes the integration-test gap left
  by round 4: the previous 32×32 NL=1 fixture had `included = false`
  on every HF code-block, so the 9/7 float decode path was only
  exercised on the LL band. The new fixture populates all 16 sub-bands
  (LL_5 + 5×{HL,LH,HH}) and decodes at MAD ≈ 0.47 / max-deviation 2
  vs. the OpenJPH-binary reference.
- decoder (HTJ2K, round 9): per-codeblock M_b-grid reconstruction
  (T.800 Eq E-1 with `N_b = S_blk + 1 + z_n`) extracted into a
  reviewable `mb_grid_value_97` helper plus a unit-test sweep covering
  the `pblk = 0` / `pblk > 0` / `pblk < 0` / `z = 0` / `z = 1` /
  `μ = 0, r = 1` algebraic cases — the half-step refinement (`pblk = 0,
  z = 1` ⇒ 0.5 multiplier) and the SigProp-only LSB
  (`μ = 0, r = 1`) are exercised directly. Both the 5/3 integer
  fixture and the 9/7 float multi-band fixture continue to round-trip;
  the unit tests close the algebraic-coverage gap that the OpenJPH
  fixtures (single-cleanup-pass, `missing_msb = M_b`) cannot hit by
  design.

### Fixed

- decoder (HTJ2K, round 7): three cleanup-pass bugs that prevented HF
  sub-band magnitudes from converging to the spec-correct integers
  for non-first-line-pair quads. The 8×8 ramp fixture (§12.2 of the
  trace doc) and the 7×7 boundary-parity fixture (§12.3) now decode
  byte-exactly, and the round-4 `htj2k_rev53` 32×32 `ojph_compress`
  fixture round-trips bit-exactly:
  1. Eq (2) of T.814 §7.3.5 was implemented as
     `c_q = (σ^nw|σ^n) + 2(σ^n|σ^nw) + 4(σ^ne|σ^nf)`. The middle
     term should be `2(σ^w|σ^sw)` per the spec; same-row left-neighbour
     samples (TR/BR of `q − 1`) were silently dropped from the cq
     context for non-first-line-pair quads, mis-decoding ρ_q, the
     CxtVLC table lookup, and ultimately the per-sample magnitude bits.
  2. Eq (5) of T.814 §7.3.7 was implemented without the γ_q multiplier
     defined in Eq (6). For multi-significant-sample quads in
     non-first line-pairs whose neighbour exponents were uniformly 0
     this had no visible effect, but quads with γ_q = 0 (≤ 1
     significant sample) and γ_q = 1 (otherwise) were both treated as
     if γ = 1, biasing κ_q by one bit-plane in the asymmetric
     situations.
  3. Per-quad U-VLC decoding (`prefix → suffix → extension`) was being
     run sequentially per quad: q1's full U-VLC then q2's full U-VLC.
     T.814 §7.3.4 (Figure 4) requires the steps to be **interleaved**
     across the quad-pair: prefix(q1), prefix(q2), suffix(q1),
     suffix(q2), ext(q1), ext(q2). With the sequential order, q1's
     suffix was reading bits from positions intended for q2's prefix,
     yielding U_q values one bit-plane shy of what the encoder
     emitted.
- decoder (HTJ2K, round 7): per-block bit-plane shift `pblk =
  band_numbps − missing_msb` is now applied during signed-integer
  reconstruction in the 5/3 reversible synthesis path (T.800 Eq E-1
  with N_b = S_blk + 1 + z_n). Before this fix the cleanup μ_n was
  written into the sub-band buffer at the wrong bit-plane whenever the
  encoder used non-zero num_zero_bitplanes.
- decoder (HTJ2K, round 8): same per-block `pblk` shift now wired
  through the 9/7 irreversible synthesis (`decode_subband_htj2k_97`),
  using float arithmetic to preserve the half-step refinement
  (`pblk_eff = pblk − 1 = −1` ⇒ multiplicative 0.5) that the integer
  5/3 path has to truncate per T.800 Eq E-7. In the same path the
  dequantisation multiplier is corrected from `0.5 · stepsize` to
  `stepsize`: T.814 §7.6 specifies μ_n as a plain integer at the M_b
  grid, with no implicit oneplushalf bit (the half-step that the
  classic Part-1 MQ tier-1 carries does not apply to HT cleanup
  outputs). The `htj2k_lossy97_decodes_close_to_opj_reference`
  fixture-based test is unignored and now passes (mean absolute
  deviation drops from 22.87 LSB to 3.05 LSB on the 32×32 8-bit
  gradient at qstep 0.05).

## [0.0.6](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.5...v0.0.6) - 2026-05-03

### Added

- standalone-friendly Cargo feature shape ([#359](https://github.com/OxideAV/oxideav-jpeg2000/pull/359))

### Other

- *(ppm_ppt)* adapt to Jpeg2000Image return type from decode_frame
- external opj_compress / opj_decompress lossless RGB roundtrip ([#314](https://github.com/OxideAV/oxideav-jpeg2000/pull/314))
- cargo-fuzz harnesses + daily Fuzz workflow (task #296)

## [0.0.5](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.4...v0.0.5) - 2026-05-03

### Other

- drop unused PixelFormat import from baseline + lossy97
- silence unused import + variable from clippy CI
- replace never-match regex with semver_check = false
- HTJ2K round 6 — fix two CxtVLC table-0 transcription typos
- migrate to centralized OxideAV/.github reusable workflows
- HTJ2K round 4 — multi-pass dispatch + 9/7 wiring
- HTJ2K round 3 — tier-2 walker + 8x8 end-to-end fixture
- HTJ2K rounds 1 + 2 + Frame API update
- adopt slim VideoFrame shape
- round 16 — fix RGB MCT bit-exactness (T.800 §G.1)
- round 15 — encoder POC + progression order + PPM/PPT
- round 14 — end-to-end PPM/PPT decode via splitter
- round 13 — POC marker + PPM/PPT plumbing
- round 12 — user precincts + PCRL/CPRL progression decode
- round 11 — multi-layer + RPCL progression decode
- pin release-plz to patch-only bumps

### Added

- decoder: multi-layer (progressive quality) codestreams (T.800 §B.10).
  Per-code-block coding-pass contributions accumulate across packets;
  the tier-1 decoder runs once at the end on the concatenated MQ
  stream (valid under Table D.8 default termination).
- decoder: RPCL progression order (T.800 §B.12.1.3) for codestreams
  using the default precinct geometry. User-precinct streams are
  rejected up front rather than silently mis-walked.
- tests: 6 new OPJ-interop fixtures covering 3- and 5-layer LRCP /
  RLCP / RPCL variants, plus a 9/7 irreversible 3-layer fixture; all
  decode bit-exactly against `opj_decompress`.
- decoder (HTJ2K, round 4): multi-pass code-blocks. Z_blk values 2
  and 3 (cleanup + SigProp [+ MagRef]) now decode end-to-end. The
  HTJ2K tier-2 walker reads two length fields per packet contribution
  (Lcup + Lref) per ISO/IEC 15444-15 §B.3 + T.800 §B.10.7.2; the
  per-codeblock state stores cleanup and refinement bytes in
  separate buffers (`CblkState::data_ref`).
- decoder (HTJ2K, round 4): irreversible 9/7 transform. The existing
  classic-J2K float lifting + `0.5 * stepsize` dequantisation now
  feeds samples decoded through FBCOT, producing pixel output
  numerically close (mean absolute deviation < 8 LSB at qstep 0.05)
  to `opj_decompress` on the same codestream.
- tests: 2 new HTJ2K OPJ-interop fixtures (5/3 reversible bit-exact;
  9/7 irreversible MAD-bounded), generated by `ojph_compress` and
  cross-decoded with `opj_decompress`.

### Fixed

- decoder (HTJ2K, round 6): two transcription typos in
  `CXT_VLC_TABLE_0` against ISO/IEC 15444-15 Annex C / ITU-T T.814
  Annex C. Row `(c_q=0, ρ=0xC, w=0x17, l_w=7)` had `ε^1=0x0` where
  the spec lists `0xC`. Row `(c_q=6, ρ=0xD, w=0x33, ε^k=0x5,
  ε^1=0x5)` had `l_w=6` where the spec lists `7`. The earlier
  `cxt_vlc_tables.rs` was a manual transcription of the long Annex C
  bracketed listing; both errors caused right-column drift on the
  trace-doc §12.2 8×8 ramp fixture (the affected codewords appeared
  in the LH/HL_R1 4×4 codeblocks). Re-audited via direct PDF text
  extraction and `diff` against every row of the 444-row table; only
  these two cells differ.
- tests: 3 new behavioural-fixture pins from
  `docs/image/jpeg2000/openjph-htj2k-trace-analysis.md` §12 (the 1×1
  smallest-possible, 8×8 ramp, 7×7 boundary-parity case). §12.1 pins
  green; §12.2 + §12.3 pin and are `#[ignore]`d pending the round-7+
  per-codeblock `p`-shift wiring inside `decode_cleanup`.

## [0.0.4](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.3...v0.0.4) - 2026-04-25

### Fixed

- mark test fixtures as binary so Windows CI doesn't CRLF them

### Other

- drop oxideav-codec/oxideav-container shims, import from oxideav-core
- round 9 — tighten HH-interop regression tests
- un-ignore round-9-closed OPJ interop / ffmpeg / multi-tile tests
- round 9 — fix ZC context for HH sub-band (spec Table D.1)
- round 8 — black-box probe rules out HH lifting as root cause
- round 7 — swap FDWT/IDWT axis order to spec-conformant VER-then-HOR / HOR-then-VER
- round-6 MQ trace harness + LL/HL/LH bit-exact OPJ interop
- add T1 sub-band diff harness for OPJ interop debugging
- add 16x16 1-level 5/3 round-trip test + tighten opj ignore notes
- mark sigprop-tested samples as pi-tested even on bit=0
- swap MQ state-table nlps/nmps transitions
- add opj_compress interop diagnostics + passing const fixture
- multi-tile decode (T.800 §B.3)
- README + crate docs — document 9/7 encoder + JP2 wrapper
- add JP2 ISOBMFF wrapper (encode + transparent decode)
- add 9/7 irreversible encoder + RGB / forward RCT / ICT
- add forward 9/7 irreversible DWT
- add 5/3 reversible lossless encoder
- wire 9/7 irreversible wavelet through decoder
- Merge remote-tracking branch 'origin/master' into wt/complete
- add Part-1 sample decoder (MQ + EBCOT + 5/3 IDWT + tier-2)
