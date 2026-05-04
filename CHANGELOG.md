# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
