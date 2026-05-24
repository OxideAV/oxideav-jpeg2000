# Changelog

All notable changes to `oxideav-jpeg2000` are recorded here.

## [Unreleased]

### Added

* **Clean-room round 125 (2026-05-25).** Tier-2 **§B.12.1.1 LRCP
  progression-order packet iterator** in a new `progression` submodule
  — the structural bridge between the §B.6 / §B.7 / §B.9 precinct +
  code-block enumeration of round 9 and the §B.10 per-precinct
  packet-header reader of round 5. New types:

  - `progression::PacketDescriptor { layer, resolution, component,
    precinct }` — one descriptor per packet in codestream order, with
    `precinct` matching the raster index handed to
    `geometry::derive_precinct_code_blocks` and bounded by
    `geometry::PrecinctPartition::num_precincts()`.
  - `progression::ComponentProgressionInfo {
    num_decomposition_levels, precincts_per_resolution }` — per-component
    input describing `NL_i` from the component's `COD` / `COC` marker and
    `numprecincts(r, i)` for `r = 0..=NL_i`. `precincts_per_resolution`
    is indexed by `r`; its length must equal `NL_i + 1` (`validate()`
    enforces this and returns `Error::InvalidPacketHeader` otherwise).
    Accessors `max_resolution()` and `precincts_at(r)` surface the
    component's resolution range; `precincts_at(r)` returns 0 for
    `r > NL_i` (the §B.12 NOTE rule).
  - `progression::lrcp_packet_order(layers, components) -> Result<
    Vec<PacketDescriptor>, Error>` — drives the verbatim §B.12.1.1
    four-nested loop:

    ```text
    for each l = 0..L
      for each r = 0..=Nmax       Nmax = max_i(NL_i)
        for each i = 0..Csiz
          for each k = 0..numprecincts(r, i)
            emit (l, r, i, k)
    ```

    Components with `NL_i < r` contribute no packet at that `r` per
    the §B.12 NOTE on synchronising resolution-level indices across
    components with different decomposition depth. Empty precincts
    (zero code-blocks) still produce one packet each per §B.6 / §B.9.
    Defensive: empty `components` slice → `Error::InvalidComponentCount`
    (T.800 Table A.9 constrains `Csiz` to `1..=16384`); `layers = 0` is
    a valid empty progression (the `POC` start/end pair can carve a
    sub-range out of a higher `L`).

  Sixteen new unit tests: the minimal `(L = 1, Csiz = 1, NL = 0)`
  single-packet case; resolution-level order within one layer
  (`r = 0, 1, 2`); layers-outermost ordering across two layers; the
  component-interleave within one resolution level; raster precinct
  order within one `(l, r, i)`; a full nested `(2 × 2 × 2 × 2)` order
  matched against a hand-built reference sequence; the §B.12 NOTE
  worked example transcribed verbatim (two components with 7 + 3
  resolution levels — both interleave at `r = 0..=2`, only component 0
  at `r = 3..=6`); the zero-precinct resolution-level corner; the
  `layers = 0` empty corner; the empty-components rejection
  (`Error::InvalidComponentCount`); the per-component length-mismatch
  rejection (`Error::InvalidPacketHeader`); the `precincts_at(r)`
  past-top-resolution returning zero; the `max_resolution()`
  echo-NL check; a single-component LRCP ordering sanity check
  (lexicographic `(layer, resolution, precinct)`); and a capacity-hint
  match check (the `estimate_packet_count` upper bound equals the actual
  output length for non-degenerate inputs). 195 tests total pass (179
  prior + 16 new); cargo fmt-check + clippy `-D warnings` clean (both
  default + `--no-default-features` builds). No new `Error` variants
  beyond the two `InvalidComponentCount` and `InvalidPacketHeader`
  reuses.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  §B.12 (§B.12.1.1 the LRCP four-nested `for l for r for i for k`
  loop body, with `L` from the `COD` `SGcod` layers field and `Nmax`
  the maximum `NL` over all components; the §B.12 NOTE on
  synchronising the resolution-level index across components with
  different decomposition depth; §B.6 / §B.9 on empty precincts still
  producing packets so they remain counted in the driver's
  `precincts_per_resolution`). No external library source — OpenJPEG,
  OpenJPH, Kakadu, Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. — was
  consulted, quoted, paraphrased, or used as a cross-check oracle. No
  WebSearch / WebFetch was used for any reason.

  The next tier-2 rounds: the remaining four progression orders
  (RLCP / RPCL / PCRL / CPRL) share the §B.12.1.3 / Equation B-20
  position-iteration machinery and land separately; §B.8 layer
  formation + §B.9 packet assembly that drives the per-precinct
  `PrecinctState` against the emitted descriptor sequence; §F.4.4
  inverse 9/7 + §F.4.3 inverse 5-3 wavelet; §E.1 / §E.2 dequantisation;
  Annex G MCT.

* **Clean-room round 122 (2026-05-25).** Tier-1 **bit-plane sequencer**
  (T.800 §D.3) that chains the three Annex D coding passes across a
  code-block from the packet reader's per-packet pass counts. New types
  in the `t1` submodule:

  - `t1::Pass` — the three §D.3 passes (`Sp` / `Mr` / `Cleanup`),
    exposed so callers (and tests) can introspect the sequencer's
    next-pass state without reproducing the §D.3 control flow
    themselves.
  - `t1::BitPlaneSequencer` — per-code-block state machine that drives
    the §D.3 three-pass order. Constructed with
    `BitPlaneSequencer::new(starting_bitplane)` where
    `starting_bitplane` is the first non-empty bit-plane index
    (`Mb − 1 − P` per §B.10.5: `Mb` from the QCD / QCC quantisation
    marker, `P` from the §B.10.5 zero-bit-plane tag tree carried by
    the packet header). Per §D.3 the initial pass is **cleanup only**;
    after that, each subsequent bit-plane runs significance propagation
    → magnitude refinement → cleanup, then drops one bit-plane and
    starts over with significance propagation.
  - `BitPlaneSequencer::decode_packet(block, bytes, passes, ctx)` —
    the high-level entry point. Builds a fresh [`MqDecoder`] over the
    single codeword segment the packet header reserved for this
    code-block (`CodeBlockContribution::segment_lengths[0]` bytes) and
    drives exactly `passes` Annex D passes
    (`CodeBlockContribution::coding_passes`). `passes = 0` is a valid
    no-op (the contribution's `included` was false and no body bytes
    were drawn). State is **per code-block**, not per packet: a
    multi-packet code-block resumes from the prior call's
    `(current_bitplane, next_pass)`.
  - `BitPlaneSequencer::decode_passes(block, decoder, ctx, passes)` —
    lower-level entry point that takes a caller-owned [`MqDecoder`],
    the right shape when COD bit-4 "termination on each pass" requires
    one decoder per pass (each pass gets its own codeword segment per
    Tables D.8 / D.9).
  - Accessors `next_pass()` / `current_bitplane()` / `passes_decoded()`
    surface the sequencer state for higher layers (e.g. the future
    progression-order driver decides whether to keep advancing a
    code-block based on its `passes_decoded` vs the per-layer
    coding-pass total).

  The MQ decoder's §C.3.4 / §D.4.1 `0xFF`-fill end-of-stream behaviour
  means the sequencer does **not** track a per-pass byte budget — the
  byte budget is the packet's responsibility (every pass's
  in-progress symbols are decoded against the synthesised `0xFF` fill
  past the signalled byte count). The sequencer also does not yet
  implement §D.4.2 / §D.5 / §D.6 termination, segmentation symbol, or
  raw-bit bypass — `decode_passes` runs every pass against the same
  caller-supplied decoder.

  Ten new unit tests: a fresh sequencer reports `Pass::Cleanup` at
  `current_bitplane()`; a single-pass call advances bit-plane K → K−1
  with the next pass = `Pass::Sp`; a three-pass call after the initial
  cleanup completes the bit-plane and returns to `Pass::Sp` on K−1; a
  `passes = 0` call is a noop on every accessor; a multi-packet
  scenario (2 + 2 passes across two `decode_packet` calls) preserves
  state across the boundary; the first cleanup-only call produces
  byte-for-byte identical coefficient state to a direct
  `cleanup_pass()` call; a four-pass run (cleanup-only first + SP / MR
  / cleanup) matches a manual three-direct-calls oracle on coefficient
  state; the lower-level `decode_passes` runs against the caller's
  `MqDecoder` correctly; running N passes in one call equals N
  single-pass calls on the same decoder (state-machine independence
  from the call boundary); and a saturating bit-plane-0 corner so a
  buggy caller still gets defined behaviour. 179 tests total pass
  (169 prior + 10 new); cargo fmt-check + clippy `-D warnings` clean
  (both default + `--no-default-features` builds). No new `Error`
  variants — the sequencer reuses the existing `Result<usize, Error>`
  shape of the per-pass methods.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  Annex D (§D.3 — the three-pass order: cleanup-only on the first
  non-empty bit-plane, then SP → MR → cleanup on each subsequent
  bit-plane from MSB toward LSB; §D.4.1 — the decoder extends the
  input bit stream with `0xFF` bytes as needed so each pass can
  decode its residual symbols past the signalled byte count, the
  basis for "the sequencer does not track a per-pass byte budget")
  and Annex B (§B.10.5 — the `Mb − 1 − P` starting bit-plane from
  the zero-bit-plane tag tree; §B.10.6 — the §B.10.6 / Table B.4
  Huffman that produces the per-packet pass count `coding_passes` the
  sequencer consumes). No external library source — OpenJPEG, OpenJPH,
  Kakadu, Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. — was consulted,
  quoted, paraphrased, or used as a cross-check oracle. No WebSearch
  / WebFetch was used for any reason.

  The next tier-1 rounds: §D.4.2 predictable-termination + §D.5
  segmentation-symbol + §D.6 selective arithmetic-coding bypass (raw
  bit mode); §B.12 progression-order packet iteration (LRCP / RLCP /
  RPCL / PCRL / CPRL); and the §F inverse 9/7 / 5-3 wavelet transform
  that consumes the sequencer's reconstructed code-block magnitudes
  through §E dequantisation.

## [0.0.11](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.10...v0.0.11) - 2026-05-24

### Other

- implement §D.3.3 magnitude refinement pass (Table D.4)
- Annex D significance-propagation coding pass (§D.3.1 + §D.3.2)
- tier-1 MQ arithmetic decoder (T.800 Annex C §C.3)
- precinct → code-block enumeration (T.800 §B.7 / §B.9)
- §B.6 precinct + §B.7 code-block partition (Eq B-16/B-17/B-18)
- round 7: per-resolution-level + per-sub-band geometry (T.800 §B.5 / Eq B-14 / Eq B-15 / Table B.1)
- round 6: SIZ-derived per-tile + per-component geometry (T.800 §B.3 / §B.5)
- round 5: tier-2 packet-header reading primitives (T.800 §B.10)
- round 4: JP2 ISO BMFF box wrapper parser (T.800 Annex I)
- round 3: typed COC/QCC/POC/RGN/PLT/PPT tile-part markers
- round 2: SOT/SOD tile-part walker
- round 1: clean-room main-header parser (SOC/SIZ/COD/QCD)
- orphan rebuild: clean-room scaffold post 2026-05-20 audit

### Added

* **Clean-room round 118 (2026-05-24).** Third and final Annex D Tier-1
  coding pass — the **cleanup pass** (T.800 §D.3.4 + Table D.5) — on top
  of the significance-propagation + sign and magnitude-refinement passes.
  Extends the `t1` submodule:

  - `t1::CodeBlock::cleanup_pass(bitplane, decoder, ctx)` runs one cleanup
    pass over the **§D.1 stripe-major scan order**, coding every
    coefficient the SP and MR passes left insignificant. It applies the
    **run-length shortcut** of Table D.5 when a column inside a full
    (4-row) stripe has all four coefficients still insignificant and each
    carrying the Table D.1 context label `0`: one MQ decision against the
    run-length context (label 17); on a `1`, two UNIFORM-context bits
    (label 18, MSB-then-LSB) give the 0-based first-significant index,
    that coefficient's sign is decoded per §D.3.2, and the followers down
    the column are coded "in the manner of §D.3.1". Ineligible columns (a
    short bottom stripe, an already-coded coefficient, or any non-zero
    context) fall back to per-coefficient significance + sign coding with
    the Table D.1 contexts. Already-significant coefficients are skipped.
    Returns the newly-significant count.
  - A shared `make_significant_with_sign` helper (set σ, accumulate the
    bit-plane weight, decode the sign via §D.3.2, flag newly-significant)
    drives both the run-length and normal-mode arms, and a
    `column_run_length_eligible` predicate encodes the §D.3.4 four-zero-
    context gate.
  - `t1::RUN_LENGTH_CTX` (17) and `t1::UNIFORM_CTX` (18) are now consumed;
    the `[MqContext; 19]` array drives **every** Annex D context.

  Seven new unit tests: run-length symbol-0 leaves a 1×4 column
  insignificant; run-length symbol-1 + UNIFORM first-index decode matched
  bit-for-bit against a reference MQ replay (including the followers down
  the column); the short-stripe path never consulting the run-length
  context; the symbol-0 path never consulting the UNIFORM context;
  skipping an already-significant / non-zero-context column; a
  cleanup-only first-bit-plane isolated-coefficient decode; and a
  three-pass (SP → MR → cleanup) significance-monotonicity self-check.
  169 tests total pass (162 prior + 7 new); cargo fmt-check + clippy
  `-D warnings` clean. No new `Error` variants — the cleanup pass returns
  the existing `Result<usize, Error>`.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  Annex D (§D.3.4 + Table D.5 the cleanup-pass run-length / UNIFORM logic;
  §D.3.1 + Table D.1 re-applied for ineligible columns; §D.3.2 sign
  subroutine; §D.1 scan pattern; §D.4 + Table D.7 initial states). Table
  D.5 is transcribed verbatim. No external library source — OpenJPEG,
  OpenJPH, Kakadu, Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. — was
  consulted, quoted, paraphrased, or used as a cross-check oracle. No
  WebSearch / WebFetch was used for any reason.

  The bit-plane **sequencer** that drives the §D.3 three-pass order
  (cleanup-only first bit-plane, then SP → MR → cleanup) per code-block
  from the packet reader's byte ranges is the next tier-1 round.

* **Clean-room round 115 (2026-05-24).** Second Annex D Tier-1 coding
  pass — the **magnitude refinement pass** (T.800 §D.3.3) — on top of the
  significance-propagation + sign passes. Extends the `t1` submodule:

  - `t1::CodeBlock::magnitude_refinement_pass(bitplane, decoder, ctx)`
    runs one MR pass over the **§D.1 stripe-major scan order** (the same
    walk as the SP pass). It refines exactly the coefficients that are
    **already significant** *and* did **not** just become significant in
    the immediately preceding SP pass (tracked via the `newly_significant`
    carry — §D.3.3). For each eligible coefficient one MQ decision is
    drawn against the **Table D.4 context**, the decoded bit is OR-ed into
    `magnitude` at the bit-plane weight `1 << bitplane`, and
    `already_refined` is set. Returns the refined-coefficient count.
  - `t1::refinement_context_label(nb, already_refined)` — Table D.4
    mapping: context 16 once a coefficient has been refined at least once
    (neighbour state is a don't-care), else context 14 / 15 for the first
    refinement keyed on whether `∑(Hi+Vi+Di)` over the *current*
    significance states is `0` or `≥ 1`. The neighbour summation merges
    all three axes into one count (§D.3.3).
  - `t1::REFINEMENT_CTX_OFFSET` is now consumed (labels `14..=16`); the
    `[MqContext; 19]` array's significance (`0..=8`), sign (`9..=13`) and
    refinement (`14..=16`) slots are all driven, leaving only `17` / `18`
    for the cleanup pass.

  Twelve new unit tests: the three Table D.4 label cases (first-no-
  neighbours → 14, first-with-neighbour → 15, already-refined → 16
  regardless of neighbours); the pass skipping insignificant + newly-
  significant coefficients; the no-eligible-coefficient no-MQ-decision /
  no-byte-consumption case; a first-refinement bit matching a reference MQ
  decoder against context 14; the first→subsequent context transition
  (14/15 → 16) verified via adaptive-context state movement; the context-15
  path when a neighbour is significant; and the stripe-major scan-order
  exhaustiveness check. 162 tests total pass (150 prior + 12 new); cargo
  fmt-check + clippy `-D warnings` clean (both default +
  `--no-default-features` builds). No new `Error` variants — the MR pass
  returns the existing `Result<usize, Error>` (the refined count).

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  Annex D (§D.3.3 + Table D.4 the 3 magnitude-refinement contexts; §D.1
  the scan pattern; §D.3 the σ-significance state + Figure D.2 neighbour
  layout). Table D.4 is transcribed verbatim. No external library source —
  OpenJPEG, OpenJPH, Kakadu, Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. —
  was consulted, quoted, paraphrased, or used as a cross-check oracle. No
  WebSearch / WebFetch was used for any reason.

* **Clean-room round 11 (2026-05-24).** First Annex D Tier-1 coding pass —
  the **significance propagation pass** (T.800 §D.3.1) plus the §D.3.2
  **sign-bit subroutine** — on top of the round-10 MQ decoder. New `t1`
  submodule:

  - `t1::CodeBlock::new(orientation, width, height)` — an
    all-insignificant coefficient grid in raster-major order. Each
    `t1::Coefficient` carries `magnitude` (reconstructed MSB-first), the
    §D.3 significance state `sigma`, the §D.2 sign bit `sign` (`true` =
    negative), and the `already_refined` carry the future §D.3.3 pass
    reads.
  - `t1::CodeBlock::significance_propagation_pass(bitplane, decoder, ctx)`
    runs one SP pass over the bit-plane with positional weight
    `1 << bitplane`. It walks the **§D.1 stripe-major scan order** (height-4
    horizontal stripes top-to-bottom; column-by-column top-to-bottom within
    each stripe — Figure D.1), and for each currently-insignificant
    coefficient with a non-zero **Table D.1 significance context** draws one
    MQ decision against context `0..=8`. A `1` flips `sigma`, accumulates the
    bit-plane weight into `magnitude`, marks the coefficient newly-significant
    (the §D.3.3 carry), and runs the **§D.3.2 sign subroutine**: the Table
    D.2 vertical/horizontal contributions reduce to a Table D.3 context
    (`9..=13`) + XORbit, and the MQ decision XORed with the XORbit recovers
    the sign per Equation D-1 (`signbit = D ⊕ XORbit`).
  - `t1::significance_context_label(orientation, nb)` — Table D.1 mapping
    from the eight Figure D.2 neighbour σ-states: LL/LH read directly, HL
    with the H/V axes swapped, HH from `(∑(Hi+Vi), ∑Di)`. Out-of-block
    neighbours are insignificant per §D.3.
  - `t1::sign_context_label(nb)` — Table D.2 → Table D.3 sign-context +
    XORbit. `t1::Neighbours` is the 8-slot σ/sign snapshot;
    `t1::reset_contexts()` builds the `[MqContext; 19]` array in its Table
    D.7 initial states (label 0 → index 4, run-length label 17 → index 3,
    UNIFORM label 18 → index 46, all others index 0), reserving slots
    `14..=16` (refinement) and `17` / `18` so the refinement / cleanup passes
    drop in without a layout shift.

  Twenty-two new unit tests: Table D.7 context-array reset + length; Table
  D.1 spot checks (zero-neighbours label 0 on all four orientations, the
  LL/LH `∑Hi=2` top row, the HL `∑Vi=2` top row vs the LL `∑Vi=2` label 4,
  the HH three-diagonal top row, labels 5 / 1 on LL, label 1 on HH) and a
  full Table D.1 round-trip across LL / HL / HH for every row; Table D.2 /
  D.3 sign-context spot checks (the `(0, 0)` label-9 row, positive/negative
  horizontal → label 12 XORbit 0/1, pos-pos / neg-neg → label 13, the
  mixed-sign-cancels-to-0 row) and the XORbit top/bottom-half symmetry; the
  §D.1 scan order (all-zero-context pass makes no MQ decision and consumes
  no bytes); a single-significant-neighbour end-to-end SP decode against a
  reference MQ decoder; the newly-significant carry clearing between passes;
  and out-of-block / corner neighbour clipping. 153 tests total pass (131
  prior + 22 new); cargo fmt-check + clippy `-D warnings` clean (both
  default + `--no-default-features` builds). No new `Error` variants — the
  SP pass returns the existing `Result<usize, Error>` (the count of
  newly-significant coefficients).

  Built solely against
  `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex D (§D.1 the
  code-block scan pattern; §D.2 the coefficient-bit / sign-bit notations;
  §D.3 the σ-significance definition + Figure D.2 eight-neighbour layout +
  out-of-block-is-insignificant rule; §D.3.1 + Table D.1 the 9 significance
  contexts per orientation; §D.3.2 + Table D.2 + Table D.3 + Equation D-1 the
  sign contexts + XORbit; §D.4 / Table D.7 the initial context states).
  Tables D.1 / D.2 / D.3 are transcribed verbatim; Figures D.1 / D.2 are
  transcribed to scan order + neighbour offsets. No external library source
  — OpenJPEG, OpenJPH, Kakadu, Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. —
  was consulted, quoted, paraphrased, or used as a cross-check oracle. No
  WebSearch / WebFetch was used for any reason.

  The §D.3.3 magnitude refinement pass (Table D.4 contexts 14–16) and the
  §D.3.4 cleanup pass (Table D.1 re-applied + run-length context + UNIFORM
  escape + Table D.5 four-zero-column shortcut) are the next tier-1 rounds,
  followed by the bit-plane sequencing that drives all three passes per
  code-block.

* **Clean-room round 10 (2026-05-24).** Tier-1 **MQ arithmetic decoder**
  (T.800 Annex C §C.3) — the first tier-1 code, the byte-consuming
  engine the future significance / refinement / cleanup coding passes
  (Annex D) will drive. New `mq` submodule:

  - `mq::MqDecoder<'a>` over a compressed-byte slice, holding the §C.3.1
    register state (`A`, `C`, `CT`, `BP`). `MqDecoder::new` is INITDEC
    (§C.3.5, Figure C.20): primes `C` with the first byte, runs BYTEIN,
    shifts `C` left 7 and `CT -= 7` to align with the starting
    `A = 0x8000`. `MqDecoder::decode(&mut MqContext) -> u8` is DECODE
    (§C.3.2, Figure C.15) with the MPS-path (Figure C.16) and LPS-path
    (Figure C.17) conditional MPS/LPS exchange and the §C.2.5 adaptive
    probability update embedded. Private `renormd` (RENORMD, §C.3.3,
    Figure C.18) and `bytein` (BYTEIN, §C.3.4, Figure C.19) handle
    renormalization and the `0xFF`-prefixed stuff-bit / end-of-stream
    marker (`0xFF` followed by `> 0x8F`, or off the end of the slice →
    feed `0xFF00`, `CT = 8`, `BP` parked on the prefix, per §C.3.4 /
    §D.4.1). The whole 32-bit `Chigh:Clow` code register lives in one
    `u32`; the §C.3.2 comparison uses `c >> 16` (Chigh) against `Qe`.
  - `mq::QE` — T.800 Table C.2 transcribed as 47 `QeEntry { qe, nmps,
    nlps, switch }` rows (indices `0..=46`). Index 35's OCR `0x02Al` is
    resolved to `0x02A1` from its binary column `0000 0010 1010 0001`.
  - `mq::MqContext` — the per-context adaptive state `(I(CX), MPS(CX))`
    with Table D.7 reset constructors (`default` index 0 / `uniform`
    index 46 / `run_length` index 3 / `zero_neighbours` index 4, all
    MPS 0) plus `index()` / `mps()` / `reset_to`. The decoder is
    stateless w.r.t. contexts — the caller (the Annex D coding-pass
    round) owns the `CX → MqContext` array, exactly mirroring the spec's
    "I(CX) / MPS(CX) stored at CX" model.

  Eighteen new unit tests: Table C.2 length / index-range / SWITCH-only-
  at-{0,6,14} / spot values (including the resolved 0x02A1 row) / the
  self-looping index-45 and index-46 rows; Table D.7 initial states +
  accessors + `reset_to`; INITDEC `A = 0x8000` alignment with a
  hand-traced known-byte case (`[0x12, 0x34]` → `C = 0x091A_0000`,
  `CT = 1`) and the empty-input `0xFF`-fill case (`C = 0x7FFF_8000`);
  BYTEIN stuff-bit and end-of-stream-marker handling; DECODE
  binary-output, determinism across two decoders, the `0x8000 ≤ A <
  0x10000` renormalization invariant over 300 decisions, UNIFORM-context
  index stability, and `0xFF`-fill deterministic-tail behaviour. 131
  tests total pass (113 prior + 18 new); cargo fmt-check + clippy
  `-D warnings` clean (both default + `--no-default-features` builds).
  No new `Error` variants — the MQ engine is infallible per §C.3.4 /
  §D.4.1 (it never errors; it synthesises the `0xFF` end-of-stream
  fill).

  Built solely against
  `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex C (§C.1.2 the
  `0x8000 ≈ 0.75` fixed-point convention and the `A ∈ [0.75, 1.5)`
  renormalization range; §C.2.5 the probability-estimation state
  machine; §C.3.1 / Table C.3 the Chigh:Clow register split; §C.3.2 /
  Figures C.15–C.17 DECODE + MPS/LPS exchange; §C.3.3 / Figure C.18
  RENORMD; §C.3.4 / Figure C.19 BYTEIN + the stuff-bit / marker rule;
  §C.3.5 / Figure C.20 INITDEC; Table C.2 the Qe/NMPS/NLPS/SWITCH rows)
  and Annex D (§D.4 / Table D.7 the initial context states; §D.4.1 the
  decoder's `0xFF`-fill extension of the input bit stream). The
  figures are images in the PDF; the register operations are the
  Figures' prose descriptions transcribed to integer ops. No external
  library source — OpenJPEG, OpenJPH, Kakadu, Grok, FFmpeg, libavcodec,
  jpeg2000-rs, etc. — was consulted, quoted, paraphrased, or used as a
  cross-check oracle. No WebSearch / WebFetch was used for any reason.

  The Annex D context formation (significance / sign / magnitude / run-
  length / UNIFORM context labelling that decides which `MqContext` each
  decision uses) is the next tier-1 round; this round is the pure §C.3
  engine it sits on. The MQ **encoder** (§C.2) and the §D.6 raw-bit
  bypass mode land later.

* **Clean-room round 9 (2026-05-24).** Precinct → code-block enumeration
  (T.800 §B.7 / §B.9) on top of the round-8 `PrecinctPartition` +
  `CodeBlockDimensions` (`geometry` submodule). New
  `geometry::derive_precinct_code_blocks(level, pp, xcb, ycb,
  precinct_index)` returns a `PrecinctCodeBlocks { r, precinct_index,
  px, py, sub_bands: Vec<PrecinctSubBand> }` — one `PrecinctSubBand`
  per sub-band of the `ResolutionLevel` in §B.9 packet order (just `LL`
  at `r = 0`; `[HL, LH, HH]` at `r ≥ 1`). Each `PrecinctSubBand`
  carries `grid_wide` × `grid_high` (exactly the
  `packet::SubBandGeometry { width, height }` the round-5 packet
  reader consumes) plus a raster-order `Vec<PrecinctCodeBlock>` matching
  the §B.10.8 walk order. Each `PrecinctCodeBlock { cbx, cby, x0, y0,
  x1, y1 }` records its in-precinct grid index and its sample corners
  on the sub-band domain, **clipped to both** the precinct projection
  and the sub-band's own bounds per §B.7 NOTE (a partition cell may
  extend past the sub-band edge; only the inside coefficients are
  coded, so `width()` / `height()` give the real coefficient count for
  rectangular interior blocks and a smaller-than-`2^xcb'` rectangle for
  edge blocks).

  The precinct projection onto each sub-band follows from §B.6 (precinct
  anchored at `(0, 0)` on the resolution-level domain, step `2^PPx`),
  §B.5 (the high-pass sub-bands at resolution level `r ≥ 1` sit at
  decomposition level `nb = NL - r + 1`, one wavelet level finer than
  the resolution-level domain at scale `2^(NL - r)`), and Equation B-20
  (the reference-grid precinct step `2^(PPx + NL - r)`): dividing by the
  sub-band scale `2^(NL - r + 1)` gives projected exponent `PPx - 1` at
  `r ≥ 1`. At `r = 0` the LL sub-band coincides with the resolution-
  level domain and the projection is the identity (exponent `PPx`). The
  enumeration anchors the projected precinct partition at `(0, 0)` on
  each sub-band (`anchor = floor(tb_lo / 2^pcb_exp)`, precinct cell `p`
  covers `[(anchor + p)·2^pcb_exp, (anchor + p + 1)·2^pcb_exp)` clipped
  to `[tb_lo, tb_hi)`), then enumerates the §B.7 code-block cells (step
  `2^xcb'`, anchored at `(0, 0)`) intersecting each precinct cell.

  Per §B.9 ("code-blocks confined to the relevant precinct") each
  code-block must belong to exactly one precinct, so the enumeration
  clamps the §B.7 effective exponent to the projected footprint
  exponent. In a conformant stream this is a no-op (default `PPx = 15`
  → footprint `2^14`, real code-blocks ≤ `2^6`); it matters only at the
  degenerate literal-§B.7 edge where `r ≥ 1` and `xcb' = min(xcb, PPx)
  = PPx > PPx - 1`, where without the clamp a single code-block would
  span two adjacent precincts. The clamp is the only reading of §B.9
  under which "confined to the precinct" remains well-defined and is
  flagged in the doc comment for downstream auditors.

  Ten new unit tests against the aligned 64×64 NL = 1 tile-component
  with `PPx = PPy = 4` (4 r=0 precincts each with a 2×2 grid of 8×8 LL
  blocks; 16 r=1 precincts each with one 8×8 block per HL/LH/HH
  sub-band; first + last precinct corner anchoring), a tiling-coverage
  check (all 16 precincts × all code-blocks across the HL band cover
  every sub-band sample exactly once), an offset `[5, 37)×[5, 37)`
  tile-component exercising left-edge clipping (precinct 0 anchored at
  resolution-level zero, first code-block clipped to a 3-wide block at
  `[5, 8)`), a `[0, 20)×[0, 20)` max-precinct sub-band exercising right-
  edge §B.7-NOTE clipping (bottom-right block clipped to `[16, 20)²`),
  the `SubBandGeometry` bridge (grid sums == `(32/8)² = 16`), max-
  precinct single-precinct mode (one 64×64 code-block), out-of-range
  index → `Error::InvalidTilePartIndex`, and the empty-resolution-level
  corner. 113 tests total pass (103 prior + 10 new); cargo fmt-check +
  clippy `-D warnings` clean (both default + `--no-default-features`
  builds). No new error variants — the function reuses the existing
  `Error::InvalidTilePartIndex` for the out-of-range precinct index.

  Built solely against
  `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` (§B.5 — lead-in
  describing the high-pass sub-bands at decomposition level `nb = NL -
  r + 1`, Equation B-15 sub-band corners on the sub-band domain; §B.6 —
  precinct partition anchored at `(0, 0)`, step `2^PPx`; §B.7 —
  Equation B-17 / B-18 effective code-block exponents, code-block
  partition anchored at `(0, 0)`, §B.7 NOTE on code-blocks extending
  past the sub-band edge; §B.9 — "the code-block contributions appear
  in raster order, confined to the bounds established by the relevant
  precinct" and "only code-blocks that contain samples from the
  relevant sub-band, confined to the precinct, have any representation
  in the packet"; §B.10.8 — the raster order the packet header walks
  the per-precinct code-blocks in; §B.12.1.3 / Equation B-20 — the
  `2^(PP + NL - r)` reference-grid precinct step that establishes the
  projected precinct exponent on each sub-band when divided by the
  sub-band scale `2^(NL - r + 1)`). No external library source —
  OpenJPEG, OpenJPH, Kakadu, FFmpeg, libavcodec, jpeg2000-rs, etc. —
  was consulted, quoted, paraphrased, or used as a cross-check oracle.
  No WebSearch / WebFetch was used for any reason.

  §B.12 progression-order packet iteration (Equation B-20 / B-21
  across all five orders LRCP / RLCP / RPCL / PCRL / CPRL) and §B.8
  layer / §B.9 packet assembly land in a later round.

* **Clean-room round 8 (2026-05-24).** Precinct partitioning (T.800
  §B.6 — Equation B-16) and code-block partitioning (§B.7 — Equation
  B-17 / Equation B-18) on top of the round-7 `ResolutionLevel`
  (`geometry` submodule). New
  `geometry::derive_precinct_partition(level, exponents)` takes a
  `ResolutionLevel` and a `PrecinctExponents { ppx, ppy }` and returns
  a `PrecinctPartition { exponents, num_wide, num_high }` whose
  `num_wide` / `num_high` follow Equation B-16:
  `numprecinctswide = ceil(trx1/2^PPx) - floor(trx0/2^PPx)` when
  `trx1 > trx0` (else 0), symmetrically for `numprecinctshigh`.
  `PrecinctPartition::num_precincts()` returns
  `num_wide * num_high` widened to `u64`. The partition is anchored at
  `(0, 0)` on the reduced-resolution tile-component domain, so the
  origin term is `floor(trx0/2^PPx)` (not `ceil`), which is what lets
  an offset tile-component straddle one extra precinct cell.
  `geometry::precinct_exponents_at(precincts, r)` decodes the `(PPx,
  PPy)` in force at resolution level `r` from a `COD` / `COC` precinct
  byte vector per Table A.21 (low nibble = `PPx`, high nibble = `PPy`,
  first byte → `r = 0` / NLLL band); an empty vector returns the
  maximum-precinct default `PPx = PPy = 15` per Table A.13 (`Scod`
  bit 0 clear). New
  `geometry::derive_code_block_dimensions(r, xcb, ycb, exponents)`
  returns `CodeBlockDimensions { xcb, ycb }` (the effective `xcb'` /
  `ycb'`) per Equation B-17 / B-18: `xcb' = min(xcb, PPx - 1)` at
  `r = 0`, `min(xcb, PPx)` at `r > 0` (symmetrically for `ycb'`), with
  the `PP - 1` computed via saturating subtraction so the
  Table-A.21-legal `PPx = PPy = 0` at the NLLL band clamps to a `1×1`
  partition rather than wrapping. `xcb` / `ycb` are the **real**
  exponents (Table A.18: the `COD` / `COC` stored byte `+ 2`); the
  caller adds the `+ 2`, the function applies the §B.7 clamp only.
  `CodeBlockDimensions::{width, height}` expose `2^xcb'` / `2^ycb'`.
  Eleven new unit tests: max-precinct default; Table A.21 nibble
  decode; aligned 64×64 precinct count (`NL = 1`, 16×16 precinct → 4
  precincts at `r = 0`, 16 at `r = 1`); offset tile-component
  exercising the `floor` origin term; single-precinct max-precinct
  mode; empty-resolution-level zero count; code-block exponents
  unclamped / clamped at `r > 0`, the `PP - 1` shave at `r = 0`, the
  `PP = 0` saturation corner, and asymmetric per-axis clamping. 103
  tests total pass (92 prior + 11 new); cargo fmt-check + clippy
  `-D warnings` clean (both default + `--no-default-features` builds).
  No new error variants — both functions are total (the precinct count
  and code-block clamp never fail; geometry validity is established by
  the `COD` / SIZ parsers upstream).

  Built solely against
  `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` (§B.6 — Equation B-16
  precinct count, precinct anchoring at `(0, 0)`; §B.7 — Equation B-17
  / B-18 effective code-block exponents, code-block partition anchored
  at `(0, 0)`, §B.7 NOTE on code-blocks extending past the sub-band
  edge; Table A.18 — `xcb = value + 2`; Table A.21 — precinct nibble
  layout; Table A.13 — maximum-precinct `PPx = PPy = 15` default). No
  external library source — OpenJPEG, OpenJPH, Kakadu, FFmpeg,
  libavcodec, jpeg2000-rs, etc. — was consulted, quoted, paraphrased,
  or used as a cross-check oracle.

  §B.8 layer formation, §B.9 packet assembly, and the §B.12
  progression-order packet iterator (Equation B-20 / B-21) land in
  round 9. The precinct → code-block enumeration (which actual
  code-blocks fall in a given precinct of a given sub-band, clipped
  to both the sub-band and precinct bounds) is the bridge between this
  round's counts and the round-5 `packet` reader's `PacketGeometry`
  input; it lands in round 9.

* **Clean-room round 7 (2026-05-22).** Per-resolution-level +
  per-sub-band geometry on top of the round-6 `TileComponentGeometry`
  (`geometry` submodule, T.800 §B.5 — Equation B-14 / Equation B-15 /
  Table B.1). New `geometry::derive_resolution_levels(tc, NL)` takes a
  `TileComponentGeometry` plus the `NL` (number of decomposition
  levels) signalled by the `COD` or `COC` marker for that component
  and returns a typed `Vec<ResolutionLevel>` of length `NL + 1`,
  indexed by resolution level `r = 0..=NL`. Each `ResolutionLevel
  { r, n_l, trx0, try0, trx1, try1, sub_bands: Vec<SubBand> }` carries
  its own bounding-sample rectangle on the tile-component domain per
  Equation B-14 (`trx0 = ceil(tcx0 / 2^(NL - r))`, symmetrically for
  the other three corners), implemented via a `ceil_div_pow2(tc, n)`
  helper that uses the closed-form `(tc + (1 << n) - 1) >> n` for
  `n < 32` and a saturating branch for `n = 32` to dodge `1u64 << 32`
  overflow. Each `SubBand { orientation: SubBandOrientation, nb,
  tbx0, tby0, tbx1, tby1 }` carries its corners per Equation B-15
  (`tbx0 = ceil((tcx0 - 2^(nb-1)·xob) / 2^nb)`, symmetrically), with
  the orientation displacements `(xob, yob)` looked up from Table B.1
  (`LL = (0, 0)`, `HL = (1, 0)`, `LH = (0, 1)`, `HH = (1, 1)`).
  Sub-band corners are computed in signed `i64` arithmetic to surface
  the `tcx0 - 2^(nb-1)·xob < 0` corner, then clamped to zero per
  §B.5's implicit non-negativity assumption for sub-band coordinates.
  `SubBandOrientation::{xob, yob}` expose the Table B.1 entries as
  `u32`. The `sub_bands` vector follows §B.5's lead-in ("The lowest
  resolution level, r = 0, is represented by the NLLL band"): a
  **single** `SubBand` with orientation `LL` and `nb = NL` at `r = 0`,
  and **three** sub-bands `[HL, LH, HH]` at decomposition level
  `nb = NL - r + 1` for every `r ≥ 1`. The `NL = 0` corner (no
  wavelet decomposition) emits a single resolution level with one
  `LL` sub-band identical to the tile-component. `NL = 32` (the
  Table A.15 upper bound) is handled without overflow. Twelve new
  unit tests against the geometry of an aligned `64×64` tile-component
  (`NL = 1`, `NL = 3`) plus an offset `[1, 5)×[1, 5)` tile-component
  exercising the signed-corner math (HL → `(0, 1)..(2, 3)`, LH →
  `(1, 0)..(3, 2)`, HH → `(0, 0)..(2, 2)`), plus Table B.1 lookup,
  `NL = 0` corner, `NL = 32` no-overflow corner, and resolution-level
  counting + LL-only-at-r=0 + HL/LH/HH-at-r>=1 + dimension-halving
  invariants. Ninety-two tests total pass (80 prior + 12 new); cargo
  fmt-check + clippy `-D warnings` clean (both default +
  `--no-default-features` builds). No new error variants — the
  function never fails; `NL` is bounded by the `COD` parser at parse
  time (Table A.15: `0..=32`) and the function's `debug_assert!`
  guards on `NL ≤ 32` reflect that invariant.

  Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
  (§B.5 lead-in describing `r = 0` as the NLLL band; Equation B-14
  resolution-level corners; Equation B-15 sub-band corners; Table B.1
  sub-band orientation displacements `(xob, yob)`; §B.5 closing
  paragraph on sub-band width = `tbx1 - tbx0` and height =
  `tby1 - tby0`). No external library source — OpenJPEG, OpenJPH,
  Kakadu, FFmpeg, libavcodec, jpeg2000-rs, etc. — was consulted,
  quoted, paraphrased, or used as a cross-check oracle.

  §B.6 precinct partitioning (Equation B-16 `numprecinctswide` /
  `numprecinctshigh` from the `COD` / `COC` `PPx` / `PPy` bytes),
  §B.7 sub-band → code-block partitioning (Equations B-17 / B-18
  with `xcb` / `ycb` exponent offsets), and §B.12 progression-order
  packet iteration land in round 8.

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
