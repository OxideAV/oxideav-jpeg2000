# Changelog

All notable changes to `oxideav-jpeg2000` are recorded here.

## [Unreleased]

### Added

* **Clean-room round 265 (2026-06-09).** T.800 §G.1.2 NOTE
  **`i64`-widened dynamic-range clip** — `Ssiz ≥ 32` mirror of
  `clamp_to_dynamic_range`, completing the `i64` §G.1 primitive set
  alongside the existing `*_dc_level_shift_unsigned_i64` pair.
  * `mct::clamp_to_dynamic_range_i64(samples, precision, is_signed)`
    — `precision ∈ 1..=38` (the full Table A.11 range); unsigned
    clip is `[0, 2^precision - 1]`, signed clip is
    `[-2^(precision - 1), 2^(precision - 1) - 1]`. Out-of-range
    `precision` (`0`, `> 38`) reports
    `Error::InvalidSamplePrecision`. Empty slices are accepted.
  * 11 new lib tests: i32 / i64 endpoint parity at 8-bit unsigned;
    12-bit signed; 32-bit unsigned + signed (the headline reason for
    the `i64` surface — `1_i32 << 32` would overflow); 38-bit
    unsigned + signed (Table A.11 upper bound); 1-bit unsigned
    corner; in-range passthrough; empty-slice ok; out-of-range
    `precision` rejection (`0`, `39`, `255`); composition with
    `inverse_dc_level_shift_unsigned_i64(_, 32)` showing the chain
    pulls overshoot back to `[0, 2^32 - 1]`. Suite total: 496 lib
    tests (was 485).

* **Clean-room round 252 (2026-06-08).** T.800 Annex G **per-tile
  three-component reconstruction threading** — the per-tile glue that
  sits between the §F.3.1 IDWT cascade (`dwt::idwt_5x3` /
  `dwt::idwt_9x7`) and the caller's final per-tile pixel buffer.
  Composes the inverse multi-component transform, the per-component
  inverse DC level shift, and the §G.1.2 NOTE dynamic-range clamp
  into one entry point per kernel.
  * `mct::ComponentDescriptor { precision_bits, is_signed }` — the
    smallest per-component invariant the §G pipeline reads from the
    SIZ marker. Built directly from a parsed `SizComponent` via
    `mct::ComponentDescriptor::from_siz_component(&siz_c)`. Drops the
    two SIZ sub-sampling factors because §G operates per `(x, y)`
    after §B / §F have realised the per-component grid.
  * `mct::InverseMctMode { None, Rct, Ict }` — the SGcod
    multi-component-transform-byte dispatch enum (Table A.17). `None`
    is Figure G.2; `Rct` is Figure G.1 paired with the 5-3 kernel;
    `Ict` is Figure G.1 paired with the 9-7 kernel.
  * `mct::reconstruct_tile_components_5x3(c0, c1, c2, descriptors,
    mode)` — the i32 5-3 / RCT threading entry point. When `mode ==
    Rct`, validates the §G.2 prologue "same separation and bit-depth"
    rule (uniform `(precision_bits, is_signed)` across all three
    descriptors → `Error::InvalidComponentCount` on mismatch), runs
    `inverse_rct`, then per-component runs `inverse_dc_level_shift`
    + `clamp_to_dynamic_range`. When `mode == None`, the inverse RCT
    is skipped and each component is independently level-shifted +
    clamped per its own descriptor (so a `(p, signedness)`-mixed
    tile is supported). `mode == Ict` is rejected with
    `Error::NotImplemented` (wrong kernel pairing — the 9-7 entry
    point owns ICT).
  * `mct::reconstruct_tile_components_9x7(c0, c1, c2, out0, out1,
    out2, descriptors, mode)` — the f32 9-7 / ICT threading entry
    point. Runs the inverse ICT when `mode == Ict` under the same
    "same separation and bit-depth" enforcement, then for each
    component rounds the f32 samples ties-to-even into i32 (with
    saturation at the cast point so a pathological ICT-amplified
    value is well-defined), level-shifts, and clamps. `mode == Rct`
    is rejected with `Error::NotImplemented`.
  * 17 new lib tests cover the threading layer. Recovery checks:
    `(R, G, B) = (200, 100, 50)` round-trips through the §G.2.1
    forward-RCT encoder side then the 5-3 / RCT threading layer back
    to `(200, 100, 50)`; the 256-entry grayscale diagonal `(k, k,
    k)` round-trips exactly across the same path; the analogous
    9-7 / ICT round-trip lands within ±1 LSB of the input (matching
    the §G.3.2 closing-paragraph "no required precision" rule).
    Per-component independence: a `(8, 10, 12)`-bit unsigned tile
    flows through `mode == None` with each component getting its
    own `+2^(p - 1)` shift. Clamp: an oversized DWT output is
    pulled to the unsigned-`[0, 255]` bound; a signed component
    skips the level-shift and gets clamped to `[-128, 127]`.
    Rejection paths: mismatched precision under MCT
    (`InvalidComponentCount`); mismatched signedness under MCT;
    cross-mode misrouting (`Ict` against the 5-3 entry / `Rct`
    against the 9-7 entry, `NotImplemented`); mismatched slice
    lengths (`InvalidMarkerLength`); non-three descriptor count
    (`InvalidMarkerLength`); out-of-range precision
    (`InvalidSamplePrecision`); 9-7 output-slot length mismatch
    (`InvalidMarkerLength`); 9-7 saturation of a 1e30 / -1e30
    pathological f32 input through the cast-saturate then
    wrapping-level-shift then NOTE-clamp chain. Suite total: 485
    lib tests (was 467).

* **Clean-room round 244 (2026-06-07).** T.800 **§B.12 walker →
  `BlockSource` bridge** — the `reassemble::WalkerBlockSource<'a>`
  adapter that fans the §B.12 packet-walker's per-precinct output
  into the per-orientation `Vec<CodedCodeBlock>` slots the §F.3.1
  IDWT cascade (`reassemble_resolution_5x3` / `_9x7`) consumes.
  * `reassemble::WalkerBlockEntry<'a>` — one tier-1 decoded
    code-block paired with its `(sub_band, cbx, cby)` precinct
    coordinate and caller-computed uniform `Nb`. Sub-band index is
    into the §B.9-ordered `PrecinctCodeBlocks::sub_bands` slice;
    `cbx` / `cby` index the `PrecinctSubBand::code_blocks` raster
    grid matching the packet header's §B.10.8 walk order.
  * `reassemble::PrecinctBlocks<'a>` — one precinct's geometry
    (`&PrecinctCodeBlocks`) paired with every tier-1 decoded
    `WalkerBlockEntry` it produced across every layer (§B.10.4 lets
    a block first appear in any layer; entries carry the merged
    final coefficients).
  * `reassemble::WalkerBlockSource::from_precincts(precincts)` —
    collects every `PrecinctBlocks` into per-orientation
    `Vec<CodedCodeBlock>` slots keyed by §B.5 `SubBandOrientation`
    (`LL` / `HL` / `LH` / `HH`). Cross-checks per entry: sub-band
    index + `cbx` / `cby` in bounds against the precinct geometry;
    tier-1 `CodeBlock` dimensions match the precinct's clipped
    placement (§B.7 NOTE); orientation matches Table B.1; no
    duplicate `(precinct_index, sub_band, cbx, cby)` triple. Returns
    `Error::InvalidPacketHeader` / `Error::InvalidMarkerLength` on
    constraint violations.
  * `WalkerBlockSource::len(orientation)` /
    `WalkerBlockSource::is_empty()` — population accessors.
  * `impl BlockSource<'a> for WalkerBlockSource<'a>` — `blocks_for`
    dispatches by `SubBand::orientation` into the matching
    pre-collected slot in O(1); the §F.3.1 cascade per-band
    reassembly call therefore sees a zero-copy slice of the same
    `&'a CodeBlock`s the caller pinned via `WalkerBlockEntry`.
  * 11 new lib tests cover the bridge end-to-end, including the
    rejection paths (out-of-range sub-band index, out-of-range
    `cbx` / `cby`, dimension mismatch, orientation mismatch,
    duplicate-triple), the multi-precinct concatenation order, and
    a byte-identity check against a hand-built direct
    `CodedCodeBlock` slice fed to `reassemble_subband_5x3`. Suite
    total: 467 lib tests (was 456).

* **Clean-room round 241 (2026-06-06).** T.800 §D.4.2 **predictable
  termination** check on `MqDecoder` plus the matching COD / COC
  Table A.19 bit-4 toggle on `BitPlaneSequencer`.
  * `MqDecoder::predictable_termination_satisfied(segment_len)` — the
    decoder-side §D.4.2 validator. Returns `true` iff no synthetic
    `0xFF`-fill was ever consumed and the byte pointer landed on
    exactly `segment_len`, **or** on `segment_len − 1` with `data[BP]
    == 0xFF` (the §C.3.4 BYTEIN rule that parks `BP` on the `0xFF`
    prefix of an end-of-segment marker). The encoder side of §D.4.2
    pushes out `k = (11 − CT) + 1` bits via repeated BYTEOUT calls
    and forbids the optional 0xFF tail-byte elision, so every bit the
    decoder asks for must be materialised in the codestream — the
    check rejects any decoder run that pulled the §C.3.4
    end-of-stream marker fill, which is mutually exclusive with a
    predictably-terminated segment.
  * `MqDecoder::synthetic_fill_used()` — the sticky internal flag
    surfaced for diagnostic introspection. Set the first time BYTEIN
    reads past the end of the input slice (either the `B` lookup or
    the `B1` peek that follows a `0xFF` prefix at end-of-segment) and
    never cleared. Also set by INITDEC when the input is empty.
  * `BitPlaneSequencer::with_predictable_termination(enabled)` /
    `BitPlaneSequencer::predictable_termination()` — builder +
    accessor for the COD / COC Table A.19 bit-4 flag. Default
    `false`. The bit composes with the §D.5 / §D.6 / §D.7 / bit-2
    toggles per the spec's §D.5 NOTE "this can be used with or
    without the predictable termination"; it does not influence
    `next_pass_is_terminated` or `raw_mode_for_next_pass` — those
    dispatch predicates are bit-2 / bit-0 driven.
  * 16 new lib tests covering: synthetic-fill clear on a non-empty
    input; synthetic-fill set by INITDEC on empty input; predictable
    accept when `BP == segment_len`; reject when `BP` is short of
    `segment_len`; reject when `BP > segment_len`; accept the
    BP-parked-on-0xFF-prefix marker case (segment_len = BP + 1);
    reject when synthetic-fill fired; reject `segment_len == 0` when
    `BP > 0`; reject the empty-input segment_len-zero degenerate
    case (synthetic-fill gate priority); synthetic-fill flag
    stickiness; the `0xFF 0xFF` marker stream does not trip
    synthetic-fill (BP parks on the prefix); sequencer bit-4 default
    off; builder monotonicity; bit-4 does not change
    `next_pass_is_terminated` / `raw_mode_for_next_pass` across the
    Table D.9 schedule rows; bit-4 composes with every other
    Table A.19 toggle; bit-4 is invariant across a `decode_packet`
    call. Suite is now 456 lib tests (was 440).

* **Clean-room round 235 (2026-06-05).** T.800 §D.4.2 **termination
  dispatch** surface on `BitPlaneSequencer` — the COD / COC Table A.19
  bit-2 (`termination_on_each_coding_pass`) toggle plus the combined
  classifier that tells a packet reader which passes own their own
  terminated codeword segment under bit-2 alone, bit-0 (§D.6 bypass)
  alone, both bits, or neither.
  * `BitPlaneSequencer::with_termination_on_each_coding_pass(enabled)`
    / `BitPlaneSequencer::termination_on_each_coding_pass()` — builder
    + accessor for the Table A.19 bit-2 flag. Default `false`.
  * `BitPlaneSequencer::next_pass_is_terminated()` — the §D.4.2 /
    Table D.9 dispatch predicate. Returns `true` iff the **next** pass
    (per `next_pass()` / `current_bitplane()`) owns its own terminated
    codeword segment, per the spec's three-way state space: bit-2 →
    every pass terminated (including every §D.6 raw pass); neither
    bit → the default single-segment packet of §D.4.1 (false for
    every pass); bit-0 alone → Table D.9 schedule with the fourth
    cleanup, every bp5+ MR raw, and every bp5+ Cleanup AC pass
    terminated, the bp5+ SP raw passes not, and the bp1/2/3 cleanups
    and pre-bypass SP/MR passes all unterminated.
  * The sequencer itself still drives every pass against the supplied
    `MqDecoder`; termination is a packet-reader-level concern (which
    decoder to feed each pass), not a sequencer-internal one. The
    lower-level `decode_passes` entry point lets a §D.4.2-aware
    caller construct one `MqDecoder` per terminated segment and
    drive the sequencer one pass at a time.
  * 12 new lib tests covering: bit-2 default off; builder
    monotonicity; predicate `false` for every state under no-flags;
    predicate `true` for every state under bit-2 alone; bit-2 wins
    over bit-0 at the bp5 SP boundary; the full Table D.9 row
    schedule under bit-0 alone for `passes_decoded == 0..=12`; the
    bp6 / bp7 SP/MR/Cleanup repeat pattern; bit-2 alone (no bypass)
    terminates every AC pass; the bp4-cleanup gate row isolated; the
    bp1/2/3 cleanups stay unterminated under bypass-only; the
    predicate consults `passes_decoded` and not just `next_pass`;
    §D.5 / §D.7 toggles do not affect the §D.4.2 classification.
    Suite is now 440 lib tests (was 428).

* **Clean-room round 227 (2026-06-04).** T.800 §D.6 **selective
  arithmetic-coding bypass** raw-bit reader plus the raw-mode SP /
  MR coding pass entry points and the sequencer-level toggle.
  * `RawBitReader<'a>` — bit-stuffed raw-bit reader. `read_bit()`
    returns one payload bit MSB-first per byte; after a `0xFF` byte
    the top bit of the next byte is the §D.6 stuff bit and is
    discarded before the next payload bit is produced.
    `bits_consumed()` / `bytes_consumed()` expose progress;
    exhausting the segment surfaces `Error::UnexpectedEof`.
  * `CodeBlock::significance_propagation_pass_raw(bitplane, raw)` —
    raw-mode SP pass. Same §D.1 scan, same "non-zero Table D.1
    context only" filter, same §D.3.3 newly-significant carry, but
    each per-coefficient decision (and sign on a `1`) is read from
    the supplied `RawBitReader`. §D.6 Equation D-2 collapses the
    sign-context XOR — the raw bit is the sign bit directly.
  * `CodeBlock::magnitude_refinement_pass_raw(bitplane, raw)` —
    raw-mode MR pass. Same scan + filter as the AC variant; one raw
    bit per refinable coefficient is OR-ed into `magnitude` at the
    bit-plane's positional weight.
  * `BitPlaneSequencer::with_selective_arithmetic_coding_bypass(enabled)`
    / `BitPlaneSequencer::selective_arithmetic_coding_bypass()` —
    builder + accessor for the §D.6 toggle. Default `false`.
  * `BitPlaneSequencer::raw_mode_for_next_pass()` — dispatch query.
    Returns `true` iff the toggle is on, the next pass is SP or MR,
    and the sequencer has driven at least 10 passes (i.e. the next
    SP / MR pass would fire on bit-plane 5 or later per Table D.9).
    The cleanup pass remains AC for every bit-plane.
  * 18 new lib tests covering: `RawBitReader` MSB-first byte
    packing, byte-boundary crossing, stuff-bit drop after a single
    `0xFF`, consecutive `0xFF` stuff bits, EoF paths (empty input,
    exhaustion, `0xFF`-then-EoF); raw SP pass decoding two
    significant coefficients with §D.6 Eq. D-2 sign reads; raw SP
    pass skipping zero-context coefficients; raw SP pass propagating
    `UnexpectedEof`; raw MR pass refining two already-significant
    coefficients; raw MR pass honouring the §D.3.3 newly-significant
    carry; raw MR pass on a fully-insignificant block; sequencer
    builder monotonicity; `raw_mode_for_next_pass` returning false
    while bypass is off; the pass-state walk from bit-plane 1
    cleanup through bit-plane 5 SP showing AC → AC → raw transition
    at the right place; the toggle-off `decode_packet` matching the
    bare `cleanup_pass` byte-for-byte; and the §D.3.3 carry-clearing
    behaviour on the raw SP pass. Suite is now 428 lib tests (was
    410).

* **Clean-room round 220 (2026-06-03).** T.800 §D.7
  **vertically-causal context formation** toggle wired into the tier-1
  decoder.
  * `CodeBlock::with_vertically_causal_context(enabled)` /
    `CodeBlock::vertically_causal_context()` — builder + accessor.
    When `true`, the §D.3 pass methods (significance propagation,
    magnitude refinement, cleanup) clip the three Figure D.2
    below-row neighbour slots `D2`, `V1`, `D3` to insignificant for
    any coefficient sitting on the **bottom row of its 4-row stripe**
    — exactly the §D.7 worked example ("Figure D.1 bit 15 is decoded
    assuming D2 = V1 = D3 = 0"). Coefficients above the stripe
    bottom retain the full Figure D.2 neighbour read.
  * `BitPlaneSequencer::with_vertically_causal_context(enabled)` /
    `BitPlaneSequencer::vertically_causal_context()` — the
    sequencer-level twin. `decode_passes` / `decode_packet` push the
    toggle onto the supplied `CodeBlock` at the start of every call
    so the COD / COC Table A.19 bit drives the entire packet-level
    pipeline from a single sequencer-level flag.
  * The §D.3.4 cleanup pass's run-length escape now consults the
    §D.7-clipped Table D.1 context label for the column's bottom
    coefficient via the same stripe-aware neighbour read, so the
    run-length decisions stay consistent with the per-coefficient
    SP pass under the toggle.
  * Default `false` everywhere — the round-208 (un-clipped)
    behaviour is byte-for-byte preserved when the toggle is clear.
  * 10 new lib tests covering: both constructor defaults, builder
    monotonicity on both `CodeBlock` and `BitPlaneSequencer`, the
    stripe-aware neighbour read matching the bare `neighbours()`
    everywhere when off, the bottom-row `D2 / V1 / D3` clip when on,
    above-stripe-bottom positions left untouched even with the
    toggle on, the short trailing-stripe corner, idempotent
    sequencer-to-block toggle sync, the `cleanup_pass` byte-for-byte
    baseline match with the toggle off, and a fixture demonstrating
    that the toggle does change the SP pass's coefficient grid when
    the next-stripe row carries significance. Suite is now 410 lib
    tests (was 400).

* **Clean-room round 214 (2026-06-03).** T.800 §D.5 **error-resilience
  segmentation symbol** decoding and the Table A.19 code-block-style
  flag surface.
  * `CodeBlockStyle::from_byte(u8)` decodes the SPcod / SPcoc
    code-block-style byte into six individually-queryable flags
    (`selective_arithmetic_coding_bypass`,
    `reset_context_probabilities`, `termination_on_each_coding_pass`,
    `vertically_causal_context`, `predictable_termination`,
    `segmentation_symbols`) per Table A.19. The two reserved high
    bits are preserved verbatim via `reserved_high_bits`.
  * `Cod::code_block_style_flags()` and `Coc::code_block_style_flags()`
    convenience accessors decode the raw byte that the parser stores.
  * `t1::SEGMENTATION_SYMBOL = 0xA` — the §D.5 reference symbol
    (binary `1010`).
  * `t1::decode_segmentation_symbol(decoder, ctx)` reads four UNIFORM
    decisions MSB-first and verifies the result against
    `SEGMENTATION_SYMBOL`. Returns `Ok(())` on match,
    `Err(Error::SegmentationSymbolMismatch)` otherwise (the §D.5
    "bit-plane carries a bit error" outcome).
  * `BitPlaneSequencer::with_segmentation_symbols(enabled)` builder
    threads the COD / COC flag through to the cleanup-pass branch:
    when on, the sequencer drains the four-bit symbol after every
    cleanup pass against the same `MqDecoder` / context array and
    propagates `SegmentationSymbolMismatch` up through
    `decode_packet` / `decode_passes`. Default off (the cleanup-pass
    flow is byte-for-byte unchanged when the COD / COC flag is
    clear).
  * `Error::SegmentationSymbolMismatch` — new variant carrying the
    §D.5 mismatch outcome.
  * 12 new lib tests covering Table A.19 per-bit decoding,
    all-flags-set, reserved-high-bit preservation, COD parser
    routing, the `0xA` constant, accept / reject sweep over all 16
    4-bit values, UNIFORM context consumption, the
    segmentation-off bit-for-bit oracle match against bare
    `cleanup_pass`, builder threading, and end-to-end sequencer
    propagation of the mismatch. Suite is now 400 lib tests
    (was 388).

* **Clean-room round 208 (2026-06-02).** §F.3.1 **IDWT cascade** added
  to the `reassemble` submodule. The cascade is the §F.3.1
  "iterate 2D_SR over the levLL band, NL times" loop that turns a
  per-resolution-level layout (from
  `geometry::derive_resolution_levels`) and a `BlockSource` into the
  reconstructed tile-component coefficient grid:
  * `reassemble::idwt_5x3(levels, source, mb_per_level, r)` — the
    reversible 5-3 path. Reassembles the NLLL band at `levels[0]`,
    then for each `k = 1..=NL` reassembles the `[HL, LH, HH]` triple
    at `levels[k]` and folds them through `dwt::sr_2d_5x3` with origin
    `(levels[k].trx0, levels[k].try0)`, carrying the resulting LL
    forward to the next iteration. Returns the final
    `Interleaved2D<i32>` at full tile-component resolution.
  * `reassemble::idwt_9x7(levels, source, quant_per_level, r)` — the
    irreversible 9-7 counterpart on `f64`. Same cascade structure;
    the per-band reassembly takes a `SubBandQuantization` rather than
    a bare `Mb` and the 2D sub-band reconstruction runs `sr_2d_9x7`.
  * Handles the NL = 0 corner (no decomposition was applied at the
    encoder) per §F.3.1's "the sub-band a0LL is the output array
    I(x, y)" rule: returns the LL band itself wrapped in an
    `Interleaved2D` of the same extent.
  * 7 new unit tests — NL = 0 / NL = 1 / NL = 2 constant-signal
    round-trips (proving the cascade's LL-carry-forward wiring lines
    up with the inverse 2D_SR's expected input shape), an `(i0, j0)`
    parity differentiation probe (two byte-identical NL = 1 cascades
    that differ only in `(trx0, try0)` — their outputs must diverge,
    proving the cascade forwards the resolution-level origin into
    `sr_2d_5x3`), `mb_per_level` length-vs.-levels-length rejection,
    empty-`levels` rejection, and the 9-7 NL = 0 path. Suite is now
    388 lib tests (was 381).

* **Clean-room round 201 (2026-06-01).** §G.1 **DC level-shifting**
  surface completed in `mct`. New entry points:
  * `mct::forward_dc_level_shift_unsigned(samples, precision)` —
    T.800 §G.1.1 Equation G-1 (`I'(x, y) = I(x, y) − 2^(Ssiz − 1)`).
    `i32` in / `i32` out, `precision ∈ 1..=31`.
  * `mct::forward_dc_level_shift_unsigned_i64(samples, precision)` /
    `mct::inverse_dc_level_shift_unsigned_i64(samples, precision)` —
    `i64`-widened pair covering the full Table A.11 range
    (`precision ∈ 1..=38`). Removes the prior round's `Ssiz ≤ 31`
    cap.
  * `mct::forward_dc_level_shift(samples, precision, is_signed)` /
    `mct::inverse_dc_level_shift(samples, precision, is_signed)` —
    signed-aware dispatchers. `is_signed == true` is a no-op per
    the §G.1.1 / §G.1.2 prologue "unsigned only" rule; otherwise
    forwards to the bare unsigned primitive. Validates `precision`
    against Table A.11 even on the signed pass-through branch.
  * `mct::clamp_to_dynamic_range(samples, precision, is_signed)` —
    the §G.1.2 NOTE's "typical solution" clip to the original
    dynamic range (`[0, 2^Ssiz − 1]` unsigned;
    `[-2^(Ssiz-1), 2^(Ssiz-1) − 1]` signed).
  * 17 new unit tests — §G.1.1 / §G.1.2 8-bit / 12-bit worked
    examples and round-trips, `i64` 32-bit + 38-bit round-trips,
    out-of-range precision rejection on every surface, signed-
    dispatcher no-op probes, and clip helper coverage across
    unsigned 8 / 12 / 31-bit and signed 8 / 16-bit ranges.

* **Clean-room round 195 (2026-05-31).** **Multi-component
  transformation** (T.800 Annex G). New `mct` submodule:
  * `mct::inverse_rct(c0, c1, c2)` — §G.2.2 inverse Reversible
    Component Transform. `i32` in / `i32` out, three slices in place;
    Equations G-6 / G-7 / G-8 with `⌊·/4⌋` realised as an
    arithmetic right-shift of two (floors toward minus infinity per
    the Annex F prologue).
  * `mct::forward_rct(c0, c1, c2)` — §G.2.1 forward RCT (Equations
    G-3 / G-4 / G-5). Encoder-only; exposed so the test battery can
    round-trip §G.2.1 → §G.2.2 without an encoder-side glue layer.
  * `mct::inverse_ict(c0, c1, c2)` — §G.3.2 inverse Irreversible
    Component Transform. `f32` in / `f32` out, the 3×3 inverse-
    Y'CbCr matrix of Equations G-12 / G-13 / G-14 (literals `1.402`,
    `0.34413`, `0.71414`, `1.772`); §G.3.2's closing precision note
    applies.
  * `mct::forward_ict(c0, c1, c2)` — §G.3.1 forward ICT (Equations
    G-9 / G-10 / G-11). Encoder-only; exposed for round-trip tests.
  * `mct::inverse_dc_level_shift_unsigned(samples, precision)` —
    §G.1.2 inverse DC level shift for unsigned tile-components
    (`+2^(Ssiz − 1)`). `precision` clamped to `1..=31` (the `i32`
    shift bound; Table A.11's full `Ssiz ≤ 38` range is deferred to
    an `i64`-widened surface in the tile-reconstruction round).
  * 12 new unit tests — §G.2.1 / §G.2.2 worked examples, RCT
    round-trip across the 8-bit unit axes + a 17-step `0..=255³`
    grid (3 375 triples), negative-sum `⌊·/4⌋` floor probes, ICT
    round-trip within `5e-3` ULPs, the textbook
    `(255, 0, 0) → (76.245, -43.031, 127.5)` Y'CbCr-601 red check,
    length-mismatch / out-of-range-precision rejection, empty-slice
    no-op.

## [0.0.13](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.12...v0.0.13) - 2026-05-30

### Other

- code-block → sub-band scatter + Annex E dequant bridge
- stand up cargo-fuzz harness for parser surface + MQ decoder

### Added

* **Clean-room round 192 (2026-05-30).** **Code-block → sub-band
  reassembly bridge** (T.800 §B.7 / §B.9 + Annex E). New `reassemble`
  submodule:
  * `reassemble::CodedCodeBlock<'a>` — one decoded code-block
    (borrowed `t1::CodeBlock` + its clipped sub-band placement from
    `geometry::PrecinctCodeBlock` + uniform `Nb` per the §B.10.5
    zero-bit-plane truncation model).
  * `reassemble::SubBandQuantization` + `::resolve(precision,
    guard_bits, orientation, step)` — bundles `(εb, µb, Mb, Rb)` so
    Equation E-2 (`Mb = G + εb − 1`) and Equation E-4 (`Rb = RI +
    log₂(gainb)`) are resolved once per (sub-band × component).
  * `reassemble::reassemble_subband_5x3(band, blocks, mb, r)` — the
    reversible path. Scatters each `CodedCodeBlock` into an `i32`
    array sized exactly `(tbx1 − tbx0) × (tby1 − tby0)` via
    `dequant::qb_signed` + `dequant::reconstruct_reversible`
    (Equations E-7 / E-8 — exact integer at `Nb = Mb`, midpoint
    `r · 2^(Mb − Nb)` lift otherwise), truncating toward zero into
    `i32` with saturation at `i32::MIN` / `i32::MAX`.
  * `reassemble::reassemble_subband_9x7(band, blocks, quant, r)` —
    the irreversible path. Equation E-6
    (`Rqb = (qb + sign(qb) · r · 2^(Mb − Nb)) · Δb`) through
    `dequant::reconstruct_irreversible`, output in `f64`.
  * `reassemble::BlockSource<'a>` trait + the blanket impl on
    `&[&[CodedCodeBlock<'a>]]` so the bridge picks the right group
    per `SubBandOrientation` regardless of insertion order.
  * `reassemble::reassemble_resolution_5x3` /
    `reassemble::reassemble_resolution_9x7` — assemble all sub-bands
    of one `ResolutionLevel` into the four-tuple of (slice, `(w, h)`)
    the `dwt::sr_2d_*` entry points consume.

  `t1::CodeBlock` grows a `from_coefficients(orientation, width,
  height, Vec<Coefficient>)` constructor — useful for the reassembly
  bridge's test suite to drive a known coefficient state into the
  scatter without first running the §D.3 passes.

  22 new unit tests cover the bridge (single-sub-band scatter, two-
  block side-by-side, non-zero band origin, Equation-E-8 truncated-
  block midpoint lift, four placement / dimension / orientation /
  overlap rejection paths, empty sub-band, irreversible scatter with
  non-unit `Δb`, Equation-E-6 midpoint at `r = 0.5` / `r = 0` /
  `qb = 0` corners, `r_qb_to_i32` saturation + NaN + truncate-toward-
  zero rounding, `SubBandQuantization::resolve` for LL / HH,
  `ResolutionArrays5x3` round-trip through `dwt::sr_2d_5x3` on a 4×4
  constant signal, `BlockSource` orientation matching independent of
  insertion order, and `mb_per_band` length validation).

## [0.0.12](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.11...v0.0.12) - 2026-05-29

### Other

- T.800 Annex F.3 inverse discrete wavelet transform submodule
- T.800 Annex E inverse quantisation submodule
- §B.12.2 POC progression-order volume iteration
- RPCL / PCRL / CPRL position-keyed iterators (T.800 §B.12.1.3–5)
- RLCP packet iterator (T.800 §B.12.1.2)
- LRCP packet iterator (T.800 §B.12.1.1)
- bit-plane sequencer chaining §D.3 three-pass order per code-block
- land the §D.3.4 cleanup pass (Annex D third coding pass)

### Added

* **Clean-room round 187 (2026-05-30).** **cargo-fuzz harness for the
  parser surface and the MQ arithmetic decoder.** Adds a standalone
  `fuzz/` sub-package (`oxideav-jpeg2000-fuzz`, outside the umbrella
  workspace via its own `[workspace]` table) with four panic-free
  libFuzzer targets:
  * `parse_codestream` — drives `parse_codestream` over arbitrary
    bytes, exercising T.800 §A.4 delimiting markers, §A.5.1 SIZ
    parsing (including the `Csiz`-driven per-component triple table),
    §A.6.1 COD parsing (including the `NL`-keyed variable-length
    precinct-byte tail), §A.6.4 QCD parsing (all three quantisation
    styles), and the §A.2 / Tables A.2 / A.3 marker allow-lists in the
    tile-part walker. 64 KiB input cap.
  * `parse_j2k_header` — drives the lower-level `parse_j2k_header`
    main-header entry point at a higher rate per second (no tile-part
    walk) so libFuzzer can steer mutations toward the SIZ
    component-table arithmetic and the COD precinct-byte tail without
    spending budget on the tile-part chain. 256 KiB input cap (allows
    exploration of the maximum-`Csiz = 16384` corner per Table A.10).
  * `parse_jp2` — drives `jp2::parse_jp2` over arbitrary bytes,
    exercising the T.800 Annex I ISO BMFF box-wrapper surface — §I.4
    box layout in all three length encodings (`LBox`, `LBox = 1 +
    XLBox`, `LBox = 0` = "until EOF"), §I.5.1 `jP  ` signature, §I.5.2
    `ftyp`, §I.5.3 `jp2h` superbox (`ihdr` + `bpcc` + `colr` in both
    `METH = 1` enumerated and `METH = 2` ICC-profile forms), and §I.5.4
    `jp2c` payload offset / length arithmetic. 256 KiB input cap.
  * `mq_decoder` — drives `mq::MqDecoder` for up to 4 096 decisions
    over arbitrary attacker-controlled bytes, cycling through the four
    Table D.7 initial contexts (`default`, `uniform`, `run_length`,
    `zero_neighbours`) so each context's §C.2.5 adaptive probability
    transition is exercised. Surfaces any bit-shift / integer-overflow
    / unbounded-loop corner the §C.3 spec's prose doesn't make obvious
    in the §C.3.5 INITDEC + §C.3.4 BYTEIN + §C.3.3 RENORMD + §C.3.2
    DECODE chain. 64 KiB input cap.
  Fixes the CI `Fuzz` workflow which has been red since the orphan
  rebuild (`no fuzz targets discovered under fuzz/fuzz_targets/`).

* **Clean-room round 181 (2026-05-29).** **Inverse discrete wavelet
  transform submodule** (T.800 Annex F.3). New `dwt::pseo(i, i0,
  il)` implements Equation F-4's closed-form periodic-symmetric-
  extension index, generalised to arbitrary out-of-range `i: i32`
  per the §F.3.7 higher-decomposition-level rider. New
  `dwt::extension_amounts_5x3` / `dwt::extension_amounts_9x7`
  transcribe Tables F.2 and F.3 (minimum left/right extension
  parameters keyed on `i0` / `il` parity). New
  `dwt::idwt_1d_5x3(y, x, i0, il)` implements 1D_SR for the 5-3
  reversible filter (§F.3.6 length-one parity rule + §F.3.7
  periodic-symmetric extension + §F.3.8.1 Equations F-5 and F-6
  with floor-division `⌊·/4⌋` / `⌊·/2⌋` per the §F prologue's
  round-toward-minus-infinity convention). New
  `dwt::idwt_1d_9x7(y, x, i0, il)` implements 1D_SR for the 9-7
  irreversible filter (§F.3.6 length-one + §F.3.7 extension +
  §F.3.8.2 Equation F-7's six-step lifting in the spec-mandated
  STEP1 → STEP6 order, with the `(α, β, γ, δ, K)` parameters of
  Table F.4 exposed as named `pub const`s: `ALPHA_9X7` =
  `-1.586_134_342_059_924`, `BETA_9X7` = `-0.052_980_118_572_961`,
  `GAMMA_9X7` = `0.882_911_075_530_934`, `DELTA_9X7` =
  `0.443_506_852_043_971`, `K_9X7` = `1.230_174_104_914_001`). The
  9-7 working buffer is sized dynamically to the actual spec-
  mandated intermediate-step access range — always ≥ Table F.3
  minimums per the §F.3.7 "values equal to or greater than … will
  produce the same array X" rider. New `dwt::interleave_2d_i32` /
  `dwt::interleave_2d_f64` implement §F.3.3 2D_INTERLEAVE: place
  LL / HL / LH / HH on the `(2u, 2v)` / `(2u+1, 2v)` / `(2u, 2v+1)`
  / `(2u+1, 2v+1)` lattice, with the §F.3.3 sub-band-dimension
  consistency check (`LL.w == LH.w`, `HL.w == HH.w`,
  `LL.h == HL.h`, `LH.h == HH.h`). New `dwt::hor_sr_{5x3,9x7}` /
  `dwt::ver_sr_{5x3,9x7}` implement §F.3.4 / §F.3.5 row-wise and
  column-wise applications of the 1D inverse filter. New
  `dwt::sr_2d_{5x3,9x7}` implement §F.3.2 single-level 2D_SR:
  `2D_INTERLEAVE` → `HOR_SR` → `VER_SR`. New `dwt::kernel_for(t)`
  dispatches a Table A.20 transformation byte to a `KernelKind`
  (`Reversible5x3` / `Irreversible9x7`). New
  `dwt::interleave_position(orientation, u, v)` round-trip helper
  computes the `(2u + d_u, 2v + d_v)` position of a sub-band sample
  in the interleaved 2D array. 32 new unit tests cover the §F.3
  surface: `pseo` reflection / period / length-one corner; Tables
  F.2 / F.3 extension amounts; 5-3 length-one parity and zero-
  signal and **bit-exact round-trip** through an in-test forward
  5-3 (constant, ramp, sawtooth, odd-length, odd-origin); 9-7
  length-one parity and zero-signal and structural properties
  (DC-coefficient → constant in interior across even/odd lengths
  and origins; linearity `f(s·y) = s·f(y)`; additivity
  `f(a + b) = f(a) + f(b)`; impulse-response decay); §F.3.3 lattice
  placement and validation failure; §F.3.2 5-3 round-trip on an 8×8
  image through forward 5-3 → inverse 2D_SR; Table A.20 dispatch.

* **Clean-room round 174 (2026-05-29).** Tier-2 **inverse-quantisation
  submodule** (T.800 Annex E). New `dequant::StepSize { epsilon,
  mantissa }` parses single `SPqcd` entries per Tables A.29 / A.30
  (reversible: 8-bit, εb in high 5 bits, low 3 reserved; irreversible:
  16-bit big-endian, εb in high 5 bits, µb in low 11 bits), with the
  full-payload helpers `parse_reversible_payload` /
  `parse_irreversible_payload` / `parse_derived_payload` matching the
  three `QuantizationStyle` variants of the existing QCD / QCC parser.
  New `dequant::subband_gain_log2(orientation)` transcribes Table E.1
  (`LL → 0`, `HL → 1`, `LH → 1`, `HH → 2`). New
  `dequant::nominal_dynamic_range(precision, orientation)` implements
  Equation E-4 `Rb = RI + log₂(gainb)`. New
  `dequant::derive_from_nlll(nlll, nl, nb)` implements Equation E-5
  derived-quantisation expansion: `(εb, µb) = (ε₀ − NL + nb, µ₀)`,
  with `Error::InvalidDecompositionLevels` on `nb > nl` and
  `Error::InvalidMarkerLength` on the `εb` underflow corner. New
  `dequant::mb(guard_bits, epsilon)` implements Equation E-2
  `Mb = G + εb − 1`. New
  `dequant::irreversible_step_size(rb, step)` implements Equation
  E-3 `Δb = 2^(Rb − εb) · (1 + µb / 2^11)` as `f64` (the negative-
  exponent corner `εb > Rb` is handled). New
  `dequant::qb_signed(coeff)` implements Equation E-1's `(1 − 2·sb)`
  sign multiplication from a tier-1 [`t1::Coefficient`]. New
  `dequant::reconstruct_irreversible(qb, mb, nb, step, r)` implements
  Equation E-6 with `r` (the §E.1.1.2 reconstruction parameter,
  typically 0.5) and the `qb == 0` dead-zone-bin → 0 branch. New
  `dequant::reconstruct_reversible(qb, mb, nb, r)` implements Equations
  E-7 (full decode: `Rqb = qb` exact integer pass-through) and E-8
  (truncated bit-plane: `Rqb = qb ± r · 2^(Mb − Nb)` with `Δb = 1`).
  Informative encoder-side `dequant::quantise_irreversible(ab, step)`
  implements Equation E-9 (§E.2) for round-trip validation; the
  decoder never calls it. 42 new unit tests cover the SPqcd byte /
  word decoders, the gain table, the dynamic-range / derived-εb /
  Mb / step-size equations, qb_signed, both reconstruction modes
  (positive / negative / zero qb, full and truncated decode), the
  worked example (8-bit grayscale, NL = 1, ScalarDerived NLLL =
  (8, 0) → (Δ_LL, Δ_HL, Δ_HH) = (1.0, 2.0, 4.0)), the Equation-E-9
  round-trip error bound (|Rqb − ab| ≤ Δb in the dead-zone bin, ≤
  Δb/2 in every other bin under r = 0.5), the malformed-payload
  rejection paths (odd-length irreversible payload →
  `InvalidMarkerLength`; out-of-range `nb` →
  `InvalidDecompositionLevels`), and the boundary corners (εb = 0,
  εb = 31, µb = 0 / 1024 / 2047, `nb = nl`, `nb = 0`). Built solely
  against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex E
  (§E.1 prologue + Equations E-1 / E-2; §E.1.1.1 + Equations E-3 /
  E-4 / E-5 + Table E.1; §E.1.1.2 + Equation E-6; §E.1.2.1; §E.1.2.2
  + Equations E-7 / E-8; §E.2 + Equation E-9) and §A.6.4 + Tables
  A.28 / A.29 / A.30 (SPqcd byte / 16-bit-word layouts). No external
  library source was consulted, quoted, paraphrased, or used as a
  cross-check oracle. No WebSearch / WebFetch was used for any
  reason.

* **Clean-room round 143 (2026-05-26).** Tier-2 **§B.12.2 POC
  progression-order volume iteration** layered on the five §B.12.1
  base orders. New `progression::PocVolume {
  component_start, component_end, resolution_start, resolution_end,
  layer_end, order }` runtime descriptor mirroring one row of the
  POC marker segment (T.800 §A.6.6 / Table A.32) under Equation B-21's
  half-open bounds `CSpoc ≤ i < CEpoc`, `RSpoc ≤ r < REpoc`,
  `0 ≤ l < LYEpoc`; `PocVolume::from_poc(&PocProgression)` adapts a
  parsed marker entry (the `CEpoc = 0 → 256 / 16 384` footnote is
  already resolved by `parse_poc` so the conversion is a pure copy).
  New driver `progression::poc_volume_packet_order(volumes,
  layers_total, components_lrcp, components_position) ->
  Result<Vec<PacketDescriptor>, Error>` walks a sequence of volumes
  in order; for each volume it dispatches to whichever of the five
  §B.12.1 orders the volume's `Ppoc` selects (LRCP / RLCP consume
  the same `ComponentProgressionInfo` slice as the base iterators;
  RPCL / PCRL / CPRL consume the `ComponentPositionInfo` slice and
  reuse the same `ordered_precinct_visits` reference-grid sorter
  filtered by Equation B-21's component / resolution rectangle).
  The §B.12.2 "no packet ever repeated in the codestream" /
  "the layer always starts with the next one for a given
  tile-component, resolution level and precinct" invariants are
  enforced via a per-`(component, resolution, precinct)` "next
  unsent layer" cursor that crosses volume boundaries (so a later
  volume revisiting the same triple emits only layers
  `cursor..LYEpoc`, never any layer that an earlier volume already
  emitted). Per the spec's "the POC marker segments may describe
  more progression order volumes than exist in the codestream" the
  driver clamps each volume's `LYEpoc` to `layers_total` before
  iteration, and clamps `REpoc` / `CEpoc` to the achievable
  per-`Nmax` / `Csiz` range so an overlong volume stays bounded.
  Reserved `Ppoc` bytes (Table A.16 reserves `0x05..=0xFF`) are
  rejected with `Error::InvalidPacketHeader`; empty-axis volumes
  (`CSpoc >= CEpoc`, `RSpoc >= REpoc`, `LYEpoc == 0`) contribute
  nothing and do not advance any cursor. Validation propagates the
  underlying base-order checks: empty / unbalanced component slices
  return `Error::InvalidComponentCount`, malformed
  `ComponentProgressionInfo` / `ComponentPositionInfo` return
  `Error::InvalidPacketHeader`. 24 new unit tests cover the
  full-cube identity vs every base order, the Equation B-21
  half-open bounds on each axis, the layer-cursor advance across
  chained LRCP / mixed-order / RPCL-partition volumes (including
  the "cursor is per-triple, not global" property), the spec's
  `LYEpoc > L` / `REpoc > Nmax + 1` / `CEpoc > Csiz` clamps, all
  empty-axis combinations, the `PocVolume::from_poc` relabel, and
  the reserved-`Ppoc` / empty-/unbalanced-slice / malformed-component
  rejection paths. Built solely against
  `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` §B.12.2 (Equation
  B-21 + the no-repeat / next-layer invariants + the
  more-volumes-than-codestream allowance) and §A.6.6 / Table A.32
  (POC marker, layout already parsed in lib.rs). No external library
  source was consulted, quoted, paraphrased, or used as a
  cross-check oracle. No WebSearch / WebFetch was used for any
  reason.

* **Clean-room round 133 (2026-05-25).** The three remaining
  **position-keyed §B.12.1 progression orders** — §B.12.1.3 **RPCL**,
  §B.12.1.4 **PCRL** and §B.12.1.5 **CPRL** — completing all five base
  progression orders. New `progression::rpcl_packet_order`,
  `progression::pcrl_packet_order` and `progression::cprl_packet_order`,
  each `(layers, components) -> Result<Vec<PacketDescriptor>, Error>`.
  Unlike LRCP / RLCP these interleave packets by **reference-grid
  position** rather than per-(resolution, component) raster index.
  Per the §B.12.1.3 NOTE ("Most of the (x, y) pairs generated by this
  loop will generally result in the inclusion of no packets … More
  efficient iterations can be found based upon the minimum of the
  dimensions of the various precincts, mapped into the reference grid"),
  the drivers compute each precinct's reference-grid top-left corner
  directly — Equation B-20's `2^(PP + NL − r)` precinct step scaled by
  the component sub-sampling `XRsiz` / `YRsiz`, anchored at the §B.6
  partition origin and clipped to the tile origin — then order the
  visits by that corner (RPCL: `resolution → y → x → component`; PCRL:
  `y → x → component → resolution`; CPRL: `component → y → x →
  resolution`), expanding each precinct over the `L` layers
  (layer-innermost in all three). New input types
  `progression::ComponentPositionInfo { num_decomposition_levels,
  xrsiz, yrsiz, resolutions }` and
  `progression::ResolutionPrecinctLayout { num_wide, num_high,
  anchor_x, anchor_y, trx0, try0, ppx, ppy }` (one layout per
  resolution level, validated `length == NL + 1` via
  `Error::InvalidPacketHeader`; zero sub-sampling factors rejected via
  `Error::InvalidComponentCount`). 26 new unit tests cover the loop
  nesting, cross-component / cross-resolution position interleaving,
  sub-sampling scaling, partition-origin clipping, the shared-multiset
  invariant across all five orders, empty-resolution and layer-zero
  corners, and the validation paths.

* **Clean-room round 128 (2026-05-25).** Tier-2 **§B.12.1.2 RLCP
  progression-order packet iterator** as a sibling of round 125's LRCP
  driver. New `progression::rlcp_packet_order(layers, components) ->
  Result<Vec<PacketDescriptor>, Error>` walks the verbatim §B.12.1.2
  four-nested loop:

  ```text
  for each r = 0..=Nmax         Nmax = max_i(NL_i)
    for each l = 0..L
      for each i = 0..Csiz
        for each k = 0..numprecincts(r, i)
          emit (l, r, i, k)
  ```

  RLCP differs from LRCP only in the relative order of the outer two
  loops (resolution-first vs. layer-first). The inner two loops, the
  per-component `ComponentProgressionInfo { num_decomposition_levels,
  precincts_per_resolution }` input shape (`length == NL + 1`,
  validated via `Error::InvalidPacketHeader`), the §B.12 NOTE rule
  that a component with `NL_i < r` contributes no packet at that `r`,
  the §B.6 / §B.9 rule that empty precincts (`numprecincts(r, i) = 0`)
  still produce packets, and the defensive empty-components check
  (`Error::InvalidComponentCount` per T.800 Table A.9 / §A.5's
  `Csiz ∈ 1..=16384` bound) are all shared verbatim with the round-125
  LRCP driver. `layers = 0` is a valid empty progression (the inner
  `l`-loop runs `0..0` for every `r`). The `Vec::with_capacity` hint
  is shared with LRCP — total packet count is invariant under the r↔l
  swap.

  Fourteen new RLCP-specific unit tests mirror the LRCP coverage
  (minimal one-packet input, resolution-outermost / layer-inner
  ordering, three-component interleave, raster-order precinct emission,
  full nested `(L=2, Nmax=1, Csiz=2, K=2) → 16 packet` shape, the
  §B.12 NOTE worked example with two layers — `(NL=6, NL=2)` →
  20 packets across both layers, empty-precinct corner, zero-layers
  empty-output, defensive `Error::InvalidComponentCount` /
  `Error::InvalidPacketHeader` checks, single-component
  `(r, l, k)`-lexicographic order, capacity-estimate-equals-output for
  no-skip inputs) plus two cross-iterator equivalence tests proving
  (a) LRCP and RLCP emit the same multiset of descriptors on a
  non-trivial `(L=3, NL=2, NL=1)` input and (b) the two diverge at the
  outermost loop on a small `(L=2, NL=1)` input.

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
