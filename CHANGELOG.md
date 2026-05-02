# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.4...v0.0.5) - 2026-05-02

### Other

- HTJ2K rounds 5 + 6 + 6.5 — non-AZC cleanup, byte-exact §12.2/§12.3
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

### Fixed

- decoder (HTJ2K, round 6.5): **§12.2 8×8 + §12.3 7×7 byte-exact
  decode** of the trace-doc reference fixtures, and **un-ignore** the
  `htj2k_rev53_decodes_bit_exactly_to_input_gradient` opj-interop test
  (it now passes against `opj_decompress`). Three changes worked
  together to close the long-standing right-column / bottom-row drift:
  - **CxtVLC table-0 transcription typo #1 at row {0, 0xC, 0x1, 0xC,
    ?, 0x17, 7}**: the spec entry's `ε^1_q` (`emb_1`) nibble is `0xC`,
    not `0x0`. The dropped digit silently neutralised the embedded-1
    bits for samples j=2,3 of any first-line-pair quad whose right
    column was significant — the cleanup decoded magnitude 2 instead
    of magnitude 4 there. Fixed by restoring the spec value (Annex C
    line 1985 of the searchable PDF). Cross-checked against
    `OpenJPEG`'s `vlc_tbl0[23]` = 0xCCCF, which encodes the same
    `(rho=0xC, u_off=1, emb_k=0xC, emb_1=0xC)` 4-tuple.
  - **CxtVLC table-0 transcription typo #2 at row {6, 0xD, 0x1, 0x5,
    0x5, 0x33, ?}**: the codeword length is `7` bits, not `6`. The
    earlier transcription would have greedy-accepted at length 6
    (cwd 0x33) before reaching the real 7-bit entry at the same prefix.
    Cross-checked against `OpenJPEG`'s `vlc_tbl0[(6 << 7) | 51]` =
    `0x5573`, decoding to length 7. Both tables now match OpenJPEG's
    `vlc_tbl0` / `vlc_tbl1` 444 + 358 unique entries bit-exactly.
  - **Magnitude reconstruction wired to `(v_n + 2) << (p − 1)`** with
    `p = M_b + 1 − missing_msbs` per OpenJPEG/OpenJPH HT block decoder
    (round-6.5 plumbing). The legacy bin-centre `μ_n = (val>>1)+1`
    formula is preserved for SigProp/MagRef per-bit unit tests via the
    `decode_codeblock` API; the tier-2 walker uses the new
    `decode_codeblock_with_shift` with the correct `p_shift` derived
    from QCD `M_b` and the per-cblk `missing_msbs`. For p_shift = 1
    (cleanup-only at 1 bit per sample) the new formula collapses to
    the same integer answer as the legacy one — the table-typo fixes
    drove the actual ramp-decode improvement.
  - `tests/htj2k_trace_doc_fixtures.rs` adds §12.2 and §12.3 byte-exact
    pinning tests, plus per-block unpack diagnostics.
  - `tests/htj2k_opj_interop.rs::htj2k_rev53_decodes_bit_exactly_to_input_gradient`
    is un-ignored. The 9/7 irreversible interop test stays ignored —
    its `decode_subband_htj2k_97` still uses the legacy bin-centre
    formula and will need separate `p_shift` wiring + irreversible
    stepsize plumbing in round 7.
- decoder (HTJ2K, round 5): non-AZC HT cleanup pass. The round-4
  fixture decoder errored out with `MagSgn: read past end of segment`
  on every code-block whose CxtVLC stream was actually exercised
  (round-3 coverage was AZC-only). Three real bugs were uncovered:
  - `cq_non_first_linepair` collapsed Formula 2's middle term to a
    duplicate of the first, producing only contexts {0, 3, 7}
    instead of the full 0..=7 range. Now reads `(σ^w | σ^sw)` from
    the same-row left-neighbour quad per Figure 5 of §7.3.5.
  - `exponent_predictor_non_first_linepair` ignored `γ_q` from
    Formula 6, inflating κ_q wherever ρ_q ∈ {0, 1, 2, 4, 8}.
  - U-VLC suffix and extension bits were decoded sequentially per
    quad rather than interleaved per Figure 4 of §7.3.4 (prefix(q1),
    prefix(q2), suffix(q1), suffix(q2), ext(q1), ext(q2)).
  Both fixture decodes now run end-to-end without a "read past end"
  trap; the LL band reproduces T.800 forward 5/3 output exactly.
  The `htj2k_lossy97_decodes_close_to_opj_reference` and
  `htj2k_rev53_decodes_bit_exactly_to_input_gradient` interop tests
  remain `#[ignore]`'d due to a residual ±2 boundary-column drift
  on HF bands (40/1024 mismatches for 5/3, MAD ≈ 22 vs threshold 8
  for 9/7) — round 6 task.

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
