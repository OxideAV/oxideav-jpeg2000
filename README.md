# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

## Status — 2026-06-03 (clean-room round 214)

Round 214 lands the T.800 **§D.5 error-resilience segmentation symbol**
and the Table A.19 code-block-style flag surface that the COD / COC
parsers store but were not previously decoded:

* `CodeBlockStyle::from_byte(u8)` — typed view of the SPcod / SPcoc
  Table A.19 byte. Six accessors return one flag each:
  `selective_arithmetic_coding_bypass` (bit 0, §D.6),
  `reset_context_probabilities` (bit 1),
  `termination_on_each_coding_pass` (bit 2, §D.4.2),
  `vertically_causal_context` (bit 3, §D.7),
  `predictable_termination` (bit 4, §D.4.2), and
  `segmentation_symbols` (bit 5, §D.5). The two high bits Table A.19
  reserves are preserved verbatim via `reserved_high_bits` for
  diagnostic-only inspection. `Cod::code_block_style_flags()` and
  `Coc::code_block_style_flags()` thread the stored byte through.
* `t1::SEGMENTATION_SYMBOL` (= `0xA`) and
  `t1::decode_segmentation_symbol(decoder, ctx)`. The standalone
  decoder reads four UNIFORM decisions MSB-first and verifies the
  4-bit result against `0xA` (binary `1010`). Returns `Ok(())` on
  match, `Err(Error::SegmentationSymbolMismatch)` on any other
  4-bit value — the §D.5 "bit-plane carries an error" outcome. The
  four UNIFORM decisions consume their Table C.2 NMPS / NLPS
  transitions exactly like any other UNIFORM decode (the symbol is
  not "free").
* `BitPlaneSequencer::with_segmentation_symbols(enabled)` —
  builder-style toggle the COD / COC Table A.19 flag drives. When
  on, the cleanup-pass branch in `decode_passes` / `decode_packet`
  calls `decode_segmentation_symbol` against the same `MqDecoder`
  / context array after the cleanup pass returns and propagates
  `SegmentationSymbolMismatch` up. Default off — with the toggle
  clear, the cleanup-pass flow is byte-for-byte identical to the
  round-208 sequencer (verified by a bit-for-bit oracle test).
* `Error::SegmentationSymbolMismatch` — new variant carrying the
  §D.5 bit-plane-corruption outcome through the public surface.

12 new lib tests cover the addition (suite total: 400 lib tests, was
388):

* `code_block_style_zero_has_no_flags_set` — all six accessors
  return `false` for `0x00`; `reserved_high_bits == 0`.
* `code_block_style_per_bit_table_a19` — each Table A.19 flag in
  isolation: `0x01..=0x20`.
* `code_block_style_all_six_flags_combined` — `0x3F` sets every
  bit; reserved high bits clear.
* `code_block_style_preserves_reserved_high_bits` — `0xC0` (the
  two-bit reserved field set) preserves the bits through `raw()`
  and `reserved_high_bits()` without affecting any flag; `0xE0`
  (reserved + segmentation symbol) decodes one flag plus the two
  reserved bits.
* `cod_code_block_style_flags_routes_through_byte` — the COD parser
  stores byte `0x20` at the SPcod code-block-style position; the
  accessor decodes `segmentation_symbols == true`.
* `segmentation_symbol_constant_matches_d5` — `SEGMENTATION_SYMBOL
  == 0x0A`.
* `decode_segmentation_symbol_accepts_target_0xa` — a byte stream
  whose four UNIFORM decisions land on `1010` decodes successfully.
* `decode_segmentation_symbol_rejects_non_0xa_values` — sweeps all
  15 non-`1010` 4-bit values; each rejects with
  `SegmentationSymbolMismatch`.
* `decode_segmentation_symbol_consumes_four_uniform_decisions` —
  the UNIFORM context's `index` and `mps` after the call match a
  manual replay of four `dec.decode(&mut ctx[UNIFORM_CTX])` calls.
* `segmentation_symbol_off_matches_bare_cleanup_pass` — with the
  toggle clear, `decode_packet` produces identical coefficient
  state and UNIFORM context state to an isolated `cleanup_pass`
  call.
* `sequencer_with_segmentation_symbols_enables_flag` — the builder
  toggles the flag in both directions; default is off.
* `sequencer_propagates_segmentation_symbol_mismatch` — with the
  toggle on, a cleanup pass over a zero stream (whose first four
  UNIFORM bits are not `1010`) propagates
  `SegmentationSymbolMismatch` through `decode_packet`.

Pending after r214:

* §D.4.2 arithmetic-coder termination + per-pass termination
  segmentation when the COD `termination_on_each_coding_pass` flag
  is set (the lower-level `decode_passes` entry already supports
  one-decoder-per-segment dispatch — the missing piece is the
  packet-reader emitting per-pass byte ranges).
* §D.6 selective arithmetic-coding bypass (raw-bit mode). Adds the
  bit-stuffed raw-bit reader from §D.6's bit-stuffing rule + a
  `bypass` toggle on the sequencer.
* §D.7 vertically causal context formation toggle. The neighbour
  read inside the three pass methods always uses the freshest
  σ-state; the §D.7 mode clips the bottom row of each stripe.

Previous round status follows:

## Status — 2026-06-02 (clean-room round 208)

Round 208 lands the **§F.3.1 IDWT cascade** in the `reassemble`
submodule — the resolution-level loop the spec describes verbatim:
initialise `lev` to `NL`, iterate the §F.3.2 `2D_SR` procedure over the
`levLL` band produced at each iteration, decrement `lev` each pass,
until `NL` iterations are done; the final `a0LL` is the output
`I(x, y)`. The cascade ties three previously-isolated submodules into
the single end-to-end inverse path:

* `reassemble::idwt_5x3(levels, source, mb_per_level, r)` — reversible
  5-3 path. Reassembles the `NLLL` band at `levels[0]`, then for each
  `k = 1..=NL` reassembles the `[HL, LH, HH]` triple at `levels[k]`
  and folds them through `dwt::sr_2d_5x3` with origin
  `(levels[k].trx0, levels[k].try0)`. The resulting `(k − 1) LL → k LL`
  array is carried forward into the next iteration. After `NL`
  iterations the carried array is the reconstructed tile-component
  coefficient grid `I(x, y)` returned as an `Interleaved2D<i32>` at
  full tile-component resolution.
* `reassemble::idwt_9x7(levels, source, quant_per_level, r)` —
  irreversible 9-7 counterpart on `f64`. Same cascade structure; the
  per-band reassembly takes a `SubBandQuantization` rather than a bare
  `Mb` and the inner 2D sub-band reconstruction runs `dwt::sr_2d_9x7`.
* Handles the `NL = 0` corner (no decomposition applied at the
  encoder) per §F.3.1's "the sub-band `a0LL` is the output array
  `I(x, y)`" rule: returns the `LL` band wrapped in an `Interleaved2D`
  of the same extent.

7 new unit tests cover the cascade (suite total: 388 lib tests, was
381):

* `idwt_5x3_nl_zero_returns_ll_unchanged` — NL = 0 no-op identity at
  4×2.
* `idwt_5x3_nl_one_constant_signal_round_trip` — NL = 1, 4×4
  tile-component, `LL = constant 5`, zero high-pass → reconstructs to
  a 4×4 grid of `5` (validating the LL-carry-forward + sub-band sizing
  contract against `dwt::sr_2d_5x3`).
* `idwt_5x3_nl_two_constant_signal_round_trip` — NL = 2, 8×8
  tile-component, `LL = constant 7`, every high-pass band zero →
  reconstructs to a constant grid of `7`. Drives a per-level
  `BlockSource` so the two HL / LH / HH triples (one at 2×2, one at
  4×4) are dispatched to their matching code-block group by sub-band
  width.
* `idwt_5x3_propagates_resolution_origin_to_sr_2d` — two byte-
  identical NL = 1 cascades that differ only in the resolution
  level's `(trx0, try0)` — their outputs diverge under the §F.3.7
  even-vs-odd boundary-extension parity rule. Proves the cascade
  actually forwards the resolution-level origin into `sr_2d_5x3`
  (i.e. it isn't hard-wired to `(0, 0)`).
* `idwt_5x3_rejects_mb_per_level_length_mismatch` /
  `idwt_5x3_rejects_empty_levels` — input-shape rejection paths.
* `idwt_9x7_nl_zero_returns_ll_unchanged` — irreversible NL = 0
  identity at 2×2 (`(εb, µb) = (8, 0)`, RI = 8, guard_bits = 1, LL
  gain = 1 → Δb = 1; `r = 0` so no midpoint lift; the recovered Rqb
  is exactly the signed magnitude).

Pending after r208:

* Per-coefficient (not per-block) `Nb` — a code-block can mix
  per-pass `Nb` values when the packet header's pass count stops
  mid-bit-plane. The cascade inherits `reassemble_subband_*`'s
  uniform-`Nb` contract.
* Tile-component reconstruction wiring (the §B.12 progression-walker
  output → `BlockSource` adapter, plus the per-component MCT inverse
  + DC level-shift threading once `idwt_*` returns). The `mct::`
  primitives from r195 / r201 are the one-line switches the
  threading layer will call between this cascade and the final pixel
  output.
* Encoder MCT toggle in `encode_jpeg2000` (forward §G.2.1 / §G.3.1
  + forward §G.1.1 primitives already exist; the missing piece is
  the tile-reconstruction wiring picking between them based on
  `Cod::mct`).

Previous round status follows:

## Status — 2026-06-01 (clean-room round 201)

Round 201 closes the **§G.1 DC level-shifting** surface in the `mct`
submodule. The crate now exposes the full forward + inverse
symmetric pair, the `i64`-widened variants that cover the Table A.11
`Ssiz ≤ 38` range, the signed-aware dispatchers a tile-reconstruction
caller will use, and the §G.1.2-NOTE recommended clip-to-dynamic-
range helper:

* `mct::forward_dc_level_shift_unsigned(samples, precision)` —
  T.800 §G.1.1 Equation G-1 (`I'(x, y) = I(x, y) − 2^(Ssiz − 1)`).
  `i32` in / `i32` out, `precision ∈ 1..=31`.
* `mct::forward_dc_level_shift_unsigned_i64(samples, precision)` /
  `mct::inverse_dc_level_shift_unsigned_i64(samples, precision)` —
  `i64`-widened pair covering the full Table A.11 range
  (`precision ∈ 1..=38`). The previous round capped `Ssiz` at `31`
  because the `i32` shift bound couldn't represent `1 << 31`; the
  `i64` surface lifts that.
* `mct::forward_dc_level_shift(samples, precision, is_signed)` /
  `mct::inverse_dc_level_shift(samples, precision, is_signed)` —
  signed-aware dispatchers. When `is_signed == true` the call is a
  no-op (per the §G.1.1 / §G.1.2 prologue "unsigned only" rule);
  otherwise it forwards to the bare unsigned primitive. These are
  the entry points the tile-reconstruction round will call once per
  component without each call site repeating the SIZ-marker MSB
  check.
* `mct::clamp_to_dynamic_range(samples, precision, is_signed)` —
  the §G.1.2 NOTE's "typical solution" to the quantisation-driven
  overflow / underflow problem ("clipping the value to the nearest
  value within the original dynamic range"). Returns samples to
  `[0, 2^Ssiz − 1]` (unsigned) or `[-2^(Ssiz-1), 2^(Ssiz-1) − 1]`
  (signed).

17 new unit tests cover the additions, bringing the `mct` module to
29 unit tests total (381 lib tests in the suite):

* `forward_dc_level_shift_unsigned_8bit` /
  `_12bit` — `(0, 127, 128, 129, 255)` → `(-128, -1, 0, 1, 127)`
  for Ssiz = 8; same shape at Ssiz = 12.
* `forward_dc_level_shift_rejects_invalid_precision` — `0` and
  `32` both reject; `1` and `31` both succeed.
* `dc_level_shift_round_trip_8bit_full_range` —
  `[0..=255]` → forward → `[-128..=127]` → inverse → `[0..=255]`.
* `dc_level_shift_round_trip_12bit_stride` — `0..4096 step 7`
  self-cancels through §G.1.1 → §G.1.2.
* `dc_level_shift_i64_round_trip_32bit` — Ssiz = 32 probes at `0`,
  `1`, `2^31` (the midpoint), `2^32 − 1` (top of unsigned range);
  forward yields `[-2^31, 2^31 − 1]` centring, inverse restores.
* `dc_level_shift_i64_round_trip_38bit` — Ssiz = 38 (Table A.11
  upper bound) round-trips the `0`, `1`, `2^37`, `2^38 − 1` probes.
* `dc_level_shift_i64_rejects_invalid_precision` — `0` and `39+`
  both reject across the `i64` pair; `1` and `38` both succeed.
* `dc_level_shift_signed_dispatcher_is_noop` — `is_signed = true`
  leaves the buffer untouched on both directions, at 8-bit and
  12-bit ranges.
* `dc_level_shift_unsigned_dispatcher_round_trips_8bit` — the
  dispatcher forwards correctly when `is_signed = false`.
* `dc_level_shift_signed_dispatcher_validates_precision` — the
  signed-side no-op path still rejects out-of-range Ssiz.
* `clamp_dynamic_range_unsigned_8bit` / `_12bit` — `[-10..=1_000_000]`
  → `[0..=255]`; `[-1, i32::MAX]` → `[0, 4095]`.
* `clamp_dynamic_range_signed_8bit` / `_16bit` — `[-200..=200]` →
  `[-128..=127]`; `[-40_000..=40_000]` → `[-32_768..=32_767]`.
* `clamp_dynamic_range_unsigned_31bit_upper_bound` — Ssiz = 31
  saturates at `i32::MAX` (`2^31 − 1`).
* `clamp_dynamic_range_rejects_invalid_precision` — `0` and `32`
  both reject; `1` and `31` both succeed.

Pending after r201:

* Per-coefficient (not per-block) `Nb` — a code-block can mix
  per-pass `Nb` values when the packet header's pass count stops
  mid-bit-plane. The reassembly bridge still accepts uniform-`Nb`.
* Encoder MCT toggle in `encode_jpeg2000` (the forward §G.2.1 /
  §G.3.1 primitives plus the §G.1.1 forward DC level shift now
  exist; what's missing is the tile-reconstruction wiring that
  picks between them based on `Cod::mct`).
* Tile reconstruction wiring (the §B.12 walk + per-resolution
  inverse 2D_SR cascade across resolution levels — once that lands,
  `forward_dc_level_shift` / `inverse_dc_level_shift` /
  `clamp_to_dynamic_range` are the one-line per-component switches
  threaded between the MCT and the wavelet pass).

Previous round status follows:

## Status — 2026-05-31 (clean-room round 195)

Round 195 lands the **multi-component transformation** (`mct`
submodule, T.800 Annex G). This is the post-DWT step that lifts the
three reconstructed tile-components `(Y0, Y1, Y2)` back into colour-
space samples `(I0, I1, I2)` when the COD marker's MCT byte (Table
A.16 `0` / `1`) signals that a forward RCT or ICT was applied. Both
inverse paths plus the inverse §G.1.2 DC level shift are now in
crate:

* `mct::inverse_rct(c0, c1, c2)` — T.800 §G.2.2 inverse Reversible
  Component Transform. `i32` in / `i32` out, three slices in place.
  Equations G-6 / G-7 / G-8 verbatim, with `⌊·/4⌋` realised as an
  arithmetic right-shift of two (floors toward minus infinity for
  negative `Y1 + Y2` sums too, matching the Annex F prologue).
* `mct::forward_rct(c0, c1, c2)` — T.800 §G.2.1 forward RCT
  (Equations G-3 / G-4 / G-5). Encoder-only; exposed now so the
  round-trip test battery can exercise §G.2.1 → §G.2.2 in pure-Rust
  without an encoder-side glue layer.
* `mct::inverse_ict(c0, c1, c2)` — T.800 §G.3.2 inverse Irreversible
  Component Transform. `f32` in / `f32` out, the 3×3 inverse-Y'CbCr
  matrix of Equations G-12 / G-13 / G-14 (literals `1.402`,
  `0.34413`, `0.71414`, `1.772`). §G.3.2's closing note about
  unspecified coefficient precision applies.
* `mct::forward_ict(c0, c1, c2)` — T.800 §G.3.1 forward ICT
  (Equations G-9 / G-10 / G-11). Encoder-only; exposed for the
  round-trip test battery for the same reason as `forward_rct`.
* `mct::inverse_dc_level_shift_unsigned(samples, precision)` — T.800
  §G.1.2 inverse DC level shift for unsigned tile-components
  (`+2^(Ssiz - 1)`). `precision ≤ 31` (the `i32` shift bound;
  Table A.11's full `Ssiz ≤ 38` range is deferred to an `i64` widen
  callable from the tile-reconstruction round).

12 new unit tests cover the submodule:

* `forward_rct_matches_g_2_1_worked_example` — `(200, 100, 50)` →
  `(112, -50, 100)` per the §G.2.1 equations.
* `inverse_rct_matches_g_2_2_worked_example` — the §G.2.1 example
  fed back through §G.2.2 recovers `(200, 100, 50)` exactly.
* `rct_roundtrips_unit_axes` — for every `k ∈ 0..=255`, the four
  axes (grayscale + R + G + B) self-cancel through §G.2.1 then
  §G.2.2.
* `rct_roundtrips_full_8bit_cube_diagonal_slice` — 3 375
  `(R, G, B)` triples on a 17-step `0..=255³` grid all self-cancel.
* `inverse_rct_floor_division_handles_negative_sums` — three spot
  checks at `Y1 + Y2 ∈ {-1, -4, -5}` prove `⌊·/4⌋` floors toward
  minus infinity (not toward zero) on the negative side.
* `ict_roundtrips_8bit_axes_within_tolerance` — `(R, G, B) =
  (200, 100, 50)` plus the grayscale axis `k ∈ {0, 32, …, 224}`
  self-cancel through §G.3.1 then §G.3.2 within `5e-3` ULPs.
* `forward_ict_red_matches_y_cb_cr_601_textbook` — `(255, 0, 0)`
  forward-ICT gives `(76.245, -43.031, 127.5)`, the textbook
  Y'CbCr-601 red triple, confirming none of the §G.3.1 coefficients
  are transposed or signed wrong.
* `length_mismatch_returns_invalid_marker_length` — both RCT
  directions and both ICT directions return
  `Error::InvalidMarkerLength` on slice-length mismatch instead of
  panicking.
* `inverse_dc_level_shift_unsigned_8bit` / `_12bit` —
  Ssiz = 8 ⇒ `+128`, Ssiz = 12 ⇒ `+2048`.
* `inverse_dc_level_shift_rejects_invalid_precision` — `0` and `32`
  both report `Error::InvalidSamplePrecision`; `1` and `31` both
  succeed.
* `empty_inputs_are_a_noop` — empty slices through all four
  transforms succeed silently.

Pending after r195:

* Per-coefficient (not per-block) `Nb` — a code-block can mix
  per-pass `Nb` values when the packet header's pass count stops
  mid-bit-plane. The reassembly bridge still accepts uniform-`Nb`.
* `Ssiz ≥ 32` DC level-shift (needs an `i64`-widening surface).
* Encoder MCT toggle in `encode_jpeg2000` (the forward primitives
  already exist; what's missing is the tile-reconstruction wiring
  that picks between them based on `Cod::mct`).
* Tile reconstruction wiring (the §B.12 walk + per-resolution
  inverse 2D_SR cascade across resolution levels — once that lands,
  the COD MCT byte will be the one-line switch between
  `inverse_rct` / `inverse_ict` / no-op).

Previous round status follows:

## Status — 2026-05-30 (clean-room round 192)

Round 192 lands the **code-block → sub-band reassembly bridge**
(`reassemble` submodule, T.800 §B.7 / §B.9 + Annex E). This is the
piece that takes tier-1's per-code-block [`t1::CodeBlock`] (magnitude
+ sign) and scatters every coefficient — Annex-E-dequantised — into a
per-sub-band coefficient array sized exactly to feed
[`dwt::sr_2d_5x3`] / [`dwt::sr_2d_9x7`]:

* `reassemble::CodedCodeBlock<'a>` — one decoded code-block (borrowed
  [`t1::CodeBlock`] + its clipped sub-band placement from
  [`geometry::PrecinctCodeBlock`] + uniform `Nb` per the §B.10.5 zero-
  bit-plane truncation model).
* `reassemble::SubBandQuantization` + `::resolve(precision, guard_bits,
  orientation, step)` — bundles `(εb, µb, Mb, Rb)` so the caller
  resolves Equation E-2 (`Mb = G + εb − 1`) and Equation E-4 (`Rb =
  RI + log₂(gainb)`) once per (sub-band × component) and passes the
  result straight through.
* `reassemble::reassemble_subband_5x3(band, blocks, mb, r)` — the
  reversible path. For each [`CodedCodeBlock`] it scatters
  `placement.x0 - band.tbx0` / `placement.y0 - band.tby0` offsets,
  calls `dequant::qb_signed` then `dequant::reconstruct_reversible`
  (Equations E-7 / E-8: `Δb = 1`, exact integer at `Nb = Mb` and
  `r · 2^(Mb − Nb)` midpoint lift otherwise), then truncates toward
  zero into `i32` with `i32::MIN` / `i32::MAX` saturation.
* `reassemble::reassemble_subband_9x7(band, blocks, quant, r)` — the
  irreversible path. Same scatter; Equation E-6
  (`Rqb = (qb + sign(qb) · r · 2^(Mb − Nb)) · Δb`) through
  `dequant::reconstruct_irreversible`, output stays in `f64`.
* `reassemble::BlockSource<'a>` trait + the blanket impl on
  `&[&[CodedCodeBlock<'a>]]` — directs each sub-band's reassembly to
  the matching code-block slice by orientation (so the caller can
  collect blocks in whatever order its §B.12 progression walker
  produced, and the bridge picks the right group per
  `SubBandOrientation`).
* `reassemble::reassemble_resolution_5x3(level, source, mb_per_band,
  r)` and `_9x7(level, source, quant_per_band, r)` — assemble all
  sub-bands of one [`ResolutionLevel`] into the four-tuple of (slice,
  `(w, h)`) the [`dwt::sr_2d_*`] entry points consume. The result is
  a `ResolutionArrays5x3` / `ResolutionArrays9x7` struct whose `ll` /
  `ll_dims` are empty at `r ≥ 1` (the caller carries the LL band
  forward from the previous step's inverse 2D_SR output).

[`t1::CodeBlock`] grows a `from_coefficients(orientation, width,
height, Vec<Coefficient>)` constructor — useful for the reassembly
bridge's test suite to drive a known coefficient state into the
scatter without first running the §D.3 passes, and a small piece of
public API the future fuzzing of the reassembly path will need.

22 new unit tests cover the bridge:

* Single-sub-band scatter of one block and of two blocks side-by-
  side (raster placement); placement with a non-`(0, 0)` band origin
  (`tbx0` / `tby0` subtraction).
* Reversible Equation E-8 midpoint lift (`Nb < Mb` carries a `r ·
  2^(Mb − Nb)` magnitude / sign-preserving offset).
* Rejection paths: placement outside the sub-band rectangle,
  orientation mismatch, [`CodeBlock`] dimensions vs.
  [`PrecinctCodeBlock`] clipped extent mismatch, and two code-blocks
  claiming the same coefficient.
* Empty sub-band returns an empty `Vec`.
* Irreversible scatter with non-unit `Δb` (`Rb = 9`, `εb = 8` ⇒
  `Δb = 2`); the Equation-E-6 midpoint at `r = 0.5` even when `Nb =
  Mb` (the irreversible path always lifts); the `r = 0` no-midpoint
  identity; the `qb = 0` always-zero special case.
* `r_qb_to_i32` saturation above / below `i32` range, NaN handling,
  and the truncate-toward-zero rounding of Equation E-8's lift.
* `SubBandQuantization::resolve` for `LL` (`Rb = RI + 0`), `HH` (`Rb =
  RI + 2`), and the §B.12.1.3 sample-precision pass-through.
* `ResolutionArrays5x3` round-trip through `dwt::sr_2d_5x3` on a 4×4
  tile-component, `NL = 1`, all-zero high-pass bands and `LL = 5`
  (constant signal): the reconstructed image is `5` at every pixel,
  validating that the four scatter targets line up exactly with the
  inverse 2D_SR's expected input shape.
* `BlockSource` orientation matching: HH-listed-first still
  dispatches HL / LH / HH calls to the right slices.
* `mb_per_band` length validation rejects an array-vs.-sub-band-count
  mismatch with `Error::InvalidMarkerLength`.

Pending after r192:

* Per-coefficient (not per-block) `Nb` — a code-block can mix
  per-pass `Nb` values when the packet header's pass count stops
  mid-bit-plane. The bridge accepts uniform-`Nb` for now; the future
  per-pass tracking will be threaded through [`BitPlaneSequencer`].
* Multiple-component transformation (MCT, Annex G).
* Tile reconstruction wiring (the §B.12 walk + per-resolution
  inverse 2D_SR cascade across resolution levels).

Previous round status follows:

## Status — 2026-05-30 (clean-room round 187)

Round 187 stands up the **cargo-fuzz harness** under `fuzz/`. The
standalone `oxideav-jpeg2000-fuzz` sub-package (its own `[workspace]`
table so the umbrella's `cargo build` doesn't drag the libFuzzer
runtime in) carries four panic-free libFuzzer targets:

* `parse_codestream` — drives [`parse_codestream`] over arbitrary
  attacker-controlled bytes, exercising T.800 §A.4 delimiting markers
  (SOC / SOT / SOD / EOC), §A.5.1 SIZ parsing (including the
  `Csiz`-driven per-component triple table), §A.6.1 COD parsing
  (including the `NL`-keyed variable-length precinct-byte tail),
  §A.6.4 QCD parsing (all three quantisation styles), and the §A.2 /
  Tables A.2 / A.3 marker allow-lists used to validate the tile-part
  walker. 64 KiB input cap.
* `parse_j2k_header` — drives the lower-level [`parse_j2k_header`]
  main-header entry point at a higher rate per second (no tile-part
  walk), so libFuzzer can steer mutations toward the SIZ
  component-table arithmetic and the COD precinct-byte tail without
  spending coverage budget on the tile-part chain. 256 KiB input cap
  (allows exploration of the maximum-`Csiz = 16384` corner per Table
  A.10).
* `parse_jp2` — drives [`jp2::parse_jp2`] over arbitrary bytes,
  exercising the T.800 Annex I ISO BMFF box-wrapper surface — §I.4
  box layout in all three length encodings (`LBox`, `LBox = 1 +
  XLBox`, `LBox = 0` = "until EOF"), §I.5.1 `jP  ` signature, §I.5.2
  `ftyp`, §I.5.3 `jp2h` superbox (`ihdr` + `bpcc` + `colr` in both
  `METH = 1` enumerated and `METH = 2` ICC-profile forms), and §I.5.4
  `jp2c` payload offset / length arithmetic. 256 KiB input cap.
* `mq_decoder` — drives [`mq::MqDecoder`] for up to 4 096 decisions
  over arbitrary bytes, cycling through the four Table D.7 initial
  contexts (`MqContext::default` / `uniform` / `run_length` /
  `zero_neighbours`) so each context's §C.2.5 adaptive probability
  transition is exercised on every fourth decision. Surfaces any
  bit-shift / integer-overflow / unbounded-loop corner the §C.3 spec's
  prose doesn't make obvious in the §C.3.5 INITDEC + §C.3.4 BYTEIN +
  §C.3.3 RENORMD + §C.3.2 DECODE chain. 64 KiB input cap.

The `.github/workflows/fuzz.yml` shared workflow's 30-minute daily
budget is now split roughly evenly across the four targets. Round 187
closes the open CI gap noted by the prior `no fuzz targets discovered
under fuzz/fuzz_targets/` failure.

Previous round status follows:

## Status — 2026-05-29 (clean-room round 181)

Round 181 adds the **inverse discrete wavelet transform submodule**
(T.800 Annex F.3). The new `dwt` submodule implements the §F.3
sub-band reconstruction path that lifts the de-quantised wavelet
coefficients of [`crate::dequant`] back to image-domain samples
for a tile-component:

* `dwt::pseo(i, i0, il)` — Equation F-4's closed-form periodic-
  symmetric-extension index. Returns a valid in-range index in
  `[i0, il)` for any `i: i32`, supporting the §F.3.7 generalisation
  to extension distances exceeding the signal length (required at
  higher decomposition levels).
* `dwt::extension_amounts_5x3` / `dwt::extension_amounts_9x7` —
  Tables F.2 and F.3 minimum-extension parameters keyed on the
  parity of `i0` and `il`.
* `dwt::idwt_1d_5x3(y, x, i0, il)` — 1D_SR for the 5-3 reversible
  filter (§F.3.6 + §F.3.7 + §F.3.8.1). Length-one parity rule plus
  Equations F-5 / F-6 with floor-division (`⌊·/4⌋` / `⌊·/2⌋`)
  matching the §F prologue's round-toward-minus-infinity convention.
* `dwt::idwt_1d_9x7(y, x, i0, il)` — 1D_SR for the 9-7 irreversible
  filter (§F.3.6 + §F.3.7 + §F.3.8.2). Length-one parity rule plus
  Equation F-7's six-step lifting (`STEP1` scaling `X(2n) =
  K · Yext(2n)`, `STEP2` scaling `X(2n+1) = (1/K) · Yext(2n+1)`,
  `STEP3` even-update `X(2n) -= δ · (X(2n-1) + X(2n+1))`,
  `STEP4` odd-update `X(2n+1) -= γ · (X(2n) + X(2n+2))`,
  `STEP5` even-update `X(2n) -= β · (X(2n-1) + X(2n+1))`,
  `STEP6` odd-update `X(2n+1) -= α · (X(2n) + X(2n+2))`)
  with the `(α, β, γ, δ, K)` parameters of Table F.4 as named `pub
  const`s (`ALPHA_9X7`, `BETA_9X7`, `GAMMA_9X7`, `DELTA_9X7`,
  `K_9X7`). The working buffer is sized dynamically to the actual
  spec-mandated intermediate-step access range (always ≥ Table F.3
  minimums, per §F.3.7's "values equal to or greater than … will
  produce the same array X" rider).
* `dwt::interleave_2d_i32` / `dwt::interleave_2d_f64` — §F.3.3
  2D_INTERLEAVE: place LL / HL / LH / HH coefficients at the
  `(2u, 2v)` / `(2u+1, 2v)` / `(2u, 2v+1)` / `(2u+1, 2v+1)` lattice
  positions of a single 2D array. Validates the §F.3.3 sample-grid
  consistency (`LL.w == LH.w`, `HL.w == HH.w`, `LL.h == HL.h`,
  `LH.h == HH.h`).
* `dwt::hor_sr_5x3` / `dwt::ver_sr_5x3` / `dwt::hor_sr_9x7` /
  `dwt::ver_sr_9x7` — §F.3.4 / §F.3.5 row-wise / column-wise
  applications of the 1D inverse filter to the interleaved array.
* `dwt::sr_2d_5x3` / `dwt::sr_2d_9x7` — §F.3.2 single-level 2D_SR:
  `2D_INTERLEAVE` followed by `HOR_SR` followed by `VER_SR`,
  returning the reconstructed `(lev - 1) LL` sub-band.
* `dwt::kernel_for(WaveletTransform)` — dispatch helper from the
  Table A.20 transformation byte to the `KernelKind` enum
  (`Reversible5x3` / `Irreversible9x7`).
* `dwt::interleave_position(SubBandOrientation, u, v)` — round-
  trip helper: given a §F.3.3 sub-band position, compute the
  corresponding `(2u + d_u, 2v + d_v)` position in the interleaved
  array.

32 new unit tests cover the §F.3 path:

* `pseo` reflection / period / length-one degenerate corner.
* `extension_amounts_{5x3,9x7}` Tables F.2 / F.3.
* `idwt_1d_5x3` length-one parity + zero-signal + **bit-exact
  round-trip** through an in-test forward 5-3 (constant, ramp,
  sawtooth, odd-length, odd-origin signals).
* `idwt_1d_9x7` length-one parity + zero-signal + structural
  properties on the inverse filter alone (DC-coefficient
  reconstructs to a constant signal in the interior across
  even/odd lengths and origins; linearity `f(s·y) = s·f(y)`;
  additivity `f(a + b) = f(a) + f(b)`; impulse-response decay
  away from the impulse position).
* `interleave_2d_*` lattice placement and §F.3.3 sub-band-grid
  validation failure path.
* `sr_2d_5x3` 8×8 round-trip end-to-end through forward 5-3 →
  inverse 2D_SR.
* `kernel_for` Table A.20 dispatch.

The 9-7 path's "validate against a forward DWT in the same test"
strategy is replaced with linearity / additivity / DC / impulse
properties because the encoder's boundary-extension handling is a
separate informative §F.4 procedure (and outside this round's
scope); the structural tests pin down the §F.3.8.2 step order and
sign conventions of Equation F-7 against the spec text directly,
without requiring an encoder oracle.

Previous round status follows:

## Status — 2026-05-29 (clean-room round 174)

Round 174 adds the **tier-2 inverse-quantisation submodule** (T.800
Annex E). The `dequant` submodule lifts a tier-1 [`t1::Coefficient`]
to a reconstructed transform coefficient `Rqb(u, v)`. The
implementation covers all of §E.1.1 (irreversible) and §E.1.2
(reversible):

* `dequant::StepSize { epsilon, mantissa }` — typed `(εb, µb)` pair
  parsed from a single `SPqcd` entry. `StepSize::from_reversible_byte`
  reads the high-5 / low-3 layout of Table A.29; 
  `StepSize::from_irreversible_word` (and `_bytes`) reads the 5-bit
  exponent + 11-bit mantissa big-endian word of Table A.30. Full-
  payload parsers `parse_reversible_payload`, 
  `parse_irreversible_payload` and `parse_derived_payload` cover the
  three `QuantizationStyle` variants the QCD / QCC parser at lib.rs
  already returns raw.
* `dequant::subband_gain_log2(orientation)` — T.800 Table E.1
  sub-band-gain exponents (`LL → 0`, `HL → 1`, `LH → 1`, `HH → 2`).
* `dequant::nominal_dynamic_range(precision, orientation)` — Equation
  E-4 `Rb = RI + log₂(gainb)`.
* `dequant::derive_from_nlll(nlll, nl, nb)` — Equation E-5 expansion
  of the single `(ε₀, µ₀)` NLLL pair to per-sub-band `(εb, µb)` under
  `ScalarDerived` quantisation: `εb = ε₀ − NL + nb`, `µb = µ₀`. Out-
  of-range `nb > nl` errors out with `Error::InvalidDecompositionLevels`;
  a negative-`εb` underflow surfaces as `Error::InvalidMarkerLength`.
* `dequant::mb(guard_bits, epsilon)` — Equation E-2 `Mb = G + εb − 1`,
  the bit-width of the integer representation of `qb(u, v)`.
* `dequant::irreversible_step_size(rb, step)` — Equation E-3
  `Δb = 2^(Rb − εb) · (1 + µb / 2^11)`, returned as `f64` to retain
  sub-bit precision (the denominator `2^11` is the 11-bit allocation
  of `µb` in Table A.30; the exponent may be negative).
* `dequant::qb_signed(coeff)` — Equation E-1 signed-integer recovery
  from the tier-1 [`t1::Coefficient`]: `qb = (1 − 2·sb) · magnitude`.
* `dequant::reconstruct_irreversible(qb, mb, nb, step, r)` — Equation
  E-6 `Rqb = (qb ± r · 2^(Mb − Nb)) · Δb` with the `qb == 0` branch
  collapsing to zero (no dead-zone midpoint lift). `r` is the §E.1.1.2
  reconstruction parameter — typically `0.5`.
* `dequant::reconstruct_reversible(qb, mb, nb, r)` — Equation E-7
  (`Nb = Mb`: `Rqb = qb`, exact integer) or Equation E-8 (`Nb < Mb`:
  `Rqb = qb ± r · 2^(Mb − Nb)`, `Δb = 1` per §E.1.2.1). The exact
  path returns `qb` verbatim so round-trip integer wavelet samples
  pass through losslessly.
* `dequant::quantise_irreversible(ab, step)` — Equation E-9 (§E.2,
  informative): `qb = sign(ab) · ⌊|ab| / Δb⌋`. Used by the test
  suite to validate the round-trip `encode → reconstruct` bound
  without any external reference; the decoder never invokes this.

42 new unit tests cover every equation in isolation plus a worked
example (8-bit grayscale, NL = 1, `ScalarDerived` NLLL gives the
three sub-band step sizes Δ_LL = 1.0, Δ_HL = Δ_LH = 2.0, Δ_HH = 4.0
under Rb_LL = 8, Rb_HL = Rb_LH = 9, Rb_HH = 10), Equation-E-9 round-
trip bounds (dead-zone bin: |Rqb − ab| ≤ Δb; mid-tread bin: ≤ Δb/2,
exhaustive over a representative ab range), the malformed-payload
rejection paths, and the boundary cases (εb = 0, εb = 31, full-
mantissa 2047, zero / positive / negative qb).

## Status — 2026-05-26 (clean-room round 143)

**Codestream-structural + JP2-wrapper + tier-2 packet-header reader +
SIZ-derived tile geometry + resolution-level / sub-band geometry +
precinct / code-block partition + precinct → code-block enumeration +
tier-1 MQ arithmetic decoder + all three tier-1 Annex D coding passes
(significance-propagation + sign, magnitude-refinement, and cleanup with
the run-length / UNIFORM four-zero-column shortcut) + bit-plane
sequencer chaining the §D.3 three-pass order across a code-block from
the packet reader's per-packet pass counts + **all five §B.12.1
progression-order packet iterators** (§B.12.1.1 LRCP, §B.12.1.2 RLCP,
§B.12.1.3 RPCL, §B.12.1.4 PCRL, §B.12.1.5 CPRL) enumerating one tile's
`(layer, resolution, component, precinct)` packet sequence under the
layer/resolution-keyed loop variants (LRCP / RLCP) or the
reference-grid-position-keyed variants (RPCL / PCRL / CPRL, ordered by
each precinct's Equation B-20 reference-grid corner) + **§B.12.2
POC progression-order volume iteration** (`progression::PocVolume` +
`progression::poc_volume_packet_order`) chaining a sequence of
`(CSpoc, CEpoc, RSpoc, REpoc, LYEpoc, Ppoc)` volumes under Equation
B-21's half-open bounds, dispatching each volume to whichever of the
five §B.12.1 orders its `Ppoc` selects, and enforcing the §B.12.2
"no packet ever repeated" / "the layer always starts with the next
one" rule via a per-`(component, resolution, precinct)` "next unsent
layer" cursor that crosses volume boundaries (and clamping `LYEpoc`
that exceeds `L` per the spec's allowance for POC marker segments to
describe more volumes than the codestream carries).**
The crate parses the JPEG 2000 Part-1 **main header** (`SOC`, `SIZ`,
`COD`, `QCD`), walks the **tile-part chain** (`SOT` / `SOD` / `EOC`),
decodes the **JP2 ISO BMFF box wrapper** (Annex I), reads the
**tier-2 packet-header bit stream** (T.800 §B.10), derives **per-tile
+ per-component coordinate geometry** from the SIZ marker (T.800 §B.2
/ §B.3 / §B.5 — Equations B-1..B-13), lifts each tile-component to
**per-resolution-level + per-sub-band geometry** using COD/COC's `NL`
(T.800 §B.5 — Equation B-14 for the resolution level corners, Equation
B-15 + Table B.1 for the sub-band corners), partitions each resolution
level into **precincts** (T.800 §B.6 — Equation B-16) and its sub-bands
into **code-blocks** (T.800 §B.7 — Equation B-17 / B-18) from the
COD/COC `PPx` / `PPy` and `xcb` / `ycb` exponents, and now **enumerates
the code-blocks of each sub-band confined to a given precinct** (T.800
§B.7 / §B.9), the bridge that feeds the round-5 packet reader's
`PacketGeometry`.

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

`geometry::derive_precinct_code_blocks(level, pp, xcb, ycb,
precinct_index)` enumerates, for one precinct of a `ResolutionLevel`,
the code-blocks of **every** sub-band confined to that precinct per
T.800 §B.7 / §B.9. It returns a `PrecinctCodeBlocks { r, precinct_index,
px, py, sub_bands: Vec<PrecinctSubBand> }`, one `PrecinctSubBand` per
sub-band (just `LL` at `r = 0`; `HL` / `LH` / `HH` at `r ≥ 1`, in §B.9
packet order). Each `PrecinctSubBand` carries `grid_wide` × `grid_high`
— the exact `packet::SubBandGeometry { width, height }` the round-5
packet reader consumes — plus a raster-order `Vec<PrecinctCodeBlock>`
matching the §B.10.8 walk order. Each `PrecinctCodeBlock` records its
in-precinct grid index `(cbx, cby)` and its sample corners `(x0, y0,
x1, y1)` on the sub-band domain, **clipped to both** the precinct
projection and the sub-band's own bounds (§B.7 NOTE: a partition cell
may extend past the sub-band edge; only the inside coefficients are
coded, so `width()` / `height()` give the real coefficient count). The
precinct partition is anchored at `(0, 0)`; its footprint projects onto
each sub-band with exponent `PPx` at `r = 0` (the LL band coincides
with the resolution-level domain) and `PPx - 1` at `r ≥ 1` (the
high-pass sub-bands sit one wavelet level finer — the Equation B-20
`2^(PPx + NL - r)` reference-grid step divided by the sub-band's
`2^(NL - r + 1)` scale). The code-block partition is anchored at `(0,
0)` with step `2^xcb'`; in a conformant stream `xcb' ≤` the footprint
exponent (default `PPx = 15` → footprint `2^14`, real blocks ≤ `2^6`),
and the enumeration clamps the exponent to the footprint so the
partition stays a tiling (no code-block claimed by two precincts) even
at the degenerate literal-§B.7 `xcb' = PPx > PPx - 1` edge. An
out-of-range `precinct_index` returns `Error::InvalidTilePartIndex`.

The `mq` submodule implements the **tier-1 MQ arithmetic decoder**
(T.800 Annex C §C.3) — the first tier-1 code, the byte-consuming engine
the future significance / refinement / cleanup coding passes (Annex D)
will drive. `mq::MqDecoder::new(bytes)` is INITDEC (§C.3.5): it primes
the code register `C` with the first compressed byte, runs BYTEIN, then
shifts `C` left 7 bits and decrements `CT` by 7 to align with the
starting interval `A = 0x8000`. `mq::MqDecoder::decode(&mut MqContext)
-> u8` is DECODE (§C.3.2): it reduces `A` by `Qe(I(CX))`, compares
`Chigh` (the high half of the 32-bit `Chigh:Clow` register, `c >> 16`)
to `Qe`, and — taking the MPS-path (Figure C.16) or LPS-path (Figure
C.17) conditional MPS/LPS exchange and the §C.2.5 adaptive probability
update — returns the binary decision `D ∈ {0, 1}`. Renormalization
(RENORMD, §C.3.3) shifts `A` and `C` left until `A ≥ 0x8000`, pulling
fresh bytes via BYTEIN (§C.3.4). BYTEIN compensates for the
`0xFF`-prefixed stuff bit and synthesises the §C.3.4 / §D.4.1
end-of-stream behaviour: a `0xFF` followed by `> 0x8F` (or off the end
of the input) is the terminating marker, after which the decoder is fed
`0xFF00`-fill and keeps producing decisions so the residual MPS run can
be decoded past the signalled byte count. The MQ engine is **infallible**
(it never errors — it extends the bit stream rather than failing), so it
adds no new `Error` variant. `mq::QE` is T.800 Table C.2 (47
`QeEntry { qe, nmps, nlps, switch }` rows, indices `0..=46`); the
per-context adaptive state `(I(CX), MPS(CX))` lives in `mq::MqContext`
with Table D.7 reset constructors (`default` index 0, `uniform` index
46, `run_length` index 3, `zero_neighbours` index 4 — all MPS 0). The
decoder is stateless w.r.t. contexts: the caller (the Annex D
coding-pass round) owns the `CX → MqContext` array, mirroring the
spec's "I(CX) / MPS(CX) stored at CX" model.

The `t1` submodule implements **all three Annex D Tier-1 coding passes**
(T.800 §D.3.1 + §D.3.2 significance propagation + sign, §D.3.3 magnitude
refinement, and §D.3.4 cleanup) on top of the MQ decoder.
`t1::CodeBlock::new(orientation, width, height)` builds an
all-insignificant coefficient grid; each `t1::Coefficient` carries its
reconstructed `magnitude` (bits arrive MSB-first), the §D.3 significance
state `sigma`, the §D.2 sign bit `sign` (`true` = negative), and the
`already_refined` flag the §D.3.3 pass reads and sets.
`t1::CodeBlock::significance_propagation_pass(bitplane, decoder, ctx)`
runs one significance-propagation pass over the bit-plane with
positional weight `1 << bitplane`: it walks the **§D.1 stripe-major scan
order** (horizontal stripes of height 4 top-to-bottom; within a stripe,
column-by-column top-to-bottom — Figure D.1), and for each currently-
insignificant coefficient whose **Table D.1 significance context** is
non-zero, draws one MQ decision against context `0..=8`. The context
label is selected per sub-band orientation from the eight Figure D.2
neighbour σ-states: `t1::significance_context_label(orientation, nb)`
reads the LL/LH column directly, the HL column with the H/V axes swapped,
and the HH column from `(∑(Hi+Vi), ∑Di)`. A `1` decision flips `sigma`,
accumulates the bit-plane weight into `magnitude`, marks the coefficient
"newly significant" (the §D.3.3 carry), and immediately runs the
**§D.3.2 sign-bit subroutine**: `t1::sign_context_label(nb)` reduces the
Table D.2 vertical/horizontal contributions to a Table D.3 context
(`9..=13`) and XORbit, the MQ decision against that context is XORed with
the XORbit per Equation D-1 (`signbit = D ⊕ XORbit`) to recover the sign.
Neighbours outside the code-block are insignificant per §D.3.

`t1::CodeBlock::magnitude_refinement_pass(bitplane, decoder, ctx)` runs
one **§D.3.3 magnitude-refinement pass** over the same §D.1 stripe-major
scan order. It refines exactly the coefficients that are **already
significant** and did **not** become significant in the immediately
preceding significance-propagation pass (tracked via the
`newly_significant` carry). For each refined coefficient one MQ decision
is drawn against the **Table D.4 context**
(`t1::refinement_context_label(nb, already_refined)`): context 16 once a
coefficient has been refined at least once (neighbour state is a
don't-care), else context 14 / 15 for the first refinement depending on
whether `∑(Hi+Vi+Di)` over the current significance states is `0` or
`≥ 1`. The decoded bit is OR-ed into `magnitude` at the bit-plane weight
and `already_refined` is set.

`t1::CodeBlock::cleanup_pass(bitplane, decoder, ctx)` runs one **§D.3.4
cleanup pass** — the last of the three Annex D passes — over the same
§D.1 stripe-major scan order. It codes every coefficient the
significance-propagation and magnitude-refinement passes left
insignificant. Per Table D.5 it applies the **run-length shortcut** when
a column inside a full (4-row) stripe has all four coefficients still
insignificant and each currently carrying the Table D.1 context label
`0`: one MQ decision against the run-length context (label 17) signals
whether any of the four becomes significant; on a `1` two further bits
against the UNIFORM context (label 18, decoded MSB-then-LSB) give the
0-based index of the first significant coefficient, whose sign is then
decoded per §D.3.2 and whose followers down the column are decoded "in
the manner of §D.3.1" (Table D.1 significance context + sign).
Run-length-ineligible columns (a short bottom stripe, an already-coded
coefficient, or any non-zero context) fall back to per-coefficient
significance coding with the same Table D.1 contexts and sign subroutine
as the significance-propagation pass. Coefficients already significant in
this bit-plane are skipped. The pass shares
`t1::make_significant_with_sign` (set σ, accumulate the bit-plane weight,
decode the sign, flag newly-significant) with the run-length and
normal-mode arms.

The caller-owned `[MqContext; 19]` array (`t1::reset_contexts()` sets the
Table D.7 initial states — label 0 → index 4, run-length label 17 →
index 3, UNIFORM label 18 → index 46, all others index 0) now drives
**every** Annex D context: significance / cleanup (`0..=8`), sign
(`9..=13`), refinement (`14..=16`), run-length (`17`), and UNIFORM
(`18`).

`progression::lrcp_packet_order(layers, components)` and
`progression::rlcp_packet_order(layers, components)` enumerate one
tile's packets in **layer-resolution level-component-position** (LRCP,
T.800 §B.12.1.1) and **resolution level-layer-component-position**
(RLCP, T.800 §B.12.1.2) progression order respectively. Both return a
`Vec<PacketDescriptor>` listing `(layer, resolution, component,
precinct)` tuples in the exact four-nested order the spec specifies;
the two functions differ only in the order of the outer two loops
(`for each l in 0..L for each r in 0..=Nmax …` for LRCP vs. `for each
r in 0..=Nmax for each l in 0..L …` for RLCP), with `Nmax =
max_i(NL_i)`. The inner two loops (`for each i in 0..Csiz for each k
in 0..numprecincts(r, i)`), the §B.12 NOTE rule that a component `i`
with `NL_i < r` contributes no packet at `r`, and the §B.6 / §B.9
rule that empty precincts still produce packets (zero-length-bit
header, empty body) are identical between the two. The driver takes
the *results* of the upstream §B.6 partition computation (one
`ComponentProgressionInfo { num_decomposition_levels,
precincts_per_resolution }` per component, where
`precincts_per_resolution.len() == NL + 1`) so it stays decoupled from
the COD / COC / SIZ marker parsing path; downstream callers can then
drive `packet::decode_packet_header` against the emitted descriptor
sequence with `PrecinctState` keyed by `(component, resolution,
precinct)`.

`progression::rpcl_packet_order`, `progression::pcrl_packet_order` and
`progression::cprl_packet_order` add the three **position-keyed**
orders — **resolution-position-component-layer** (RPCL, T.800
§B.12.1.3), **position-component-resolution-layer** (PCRL, §B.12.1.4)
and **component-position-resolution-layer** (CPRL, §B.12.1.5). These
interleave packets by **reference-grid position** rather than
per-(resolution, component) raster index: instead of the literal `for
y / for x` reference-grid sweep, the §B.12.1.3 NOTE permits computing
each precinct's reference-grid top-left corner directly (Equation
B-20's `2^(PP + NL − r)` precinct step scaled by the component
sub-sampling `XRsiz` / `YRsiz`, anchored at the §B.6 partition origin
and clipped to the tile origin) and ordering the visits by that corner,
which is what these drivers do. The layer loop is innermost in all
three. They take a richer `ComponentPositionInfo { num_decomposition_
levels, xrsiz, yrsiz, resolutions }` per component (one
`ResolutionPrecinctLayout { num_wide, num_high, anchor_x, anchor_y,
trx0, try0, ppx, ppy }` per resolution level) so the precinct corner
can be derived without re-reading the marker path. All five orders emit
the same packet multiset for a given tile; only the ordering differs.

`t1::BitPlaneSequencer` chains the three passes per code-block in the
§D.3 order across the packet reader's per-packet pass counts. Its state
is per code-block, not per packet: a code-block carried in multiple
packets across layers resumes exactly where the previous packet left it.
`BitPlaneSequencer::new(starting_bitplane)` arms the sequencer with the
first non-empty bit-plane (`Mb − 1 − P` per §B.10.5: `Mb` from the QCD /
QCC quantisation marker, `P` from the packet header's zero-bit-plane
tag tree); per §D.3 the first pass is cleanup only, after which each
subsequent bit-plane runs significance propagation (`Pass::Sp`) → 
magnitude refinement (`Pass::Mr`) → cleanup (`Pass::Cleanup`), then
drops one bit-plane and starts over with significance propagation.
`BitPlaneSequencer::decode_packet(block, bytes, passes, ctx)` builds a
fresh [`MqDecoder`] over the packet's single codeword segment for this
code-block and drives exactly `passes` passes (`coding_passes` from the
contribution); `passes = 0` is a valid no-op (the contribution was not
included in the packet). `BitPlaneSequencer::decode_passes` is the
lower-level entry point that takes a caller-owned [`MqDecoder`], the
right shape when COD bit-4 "termination on each pass" requires one
decoder per pass.

What is **not** implemented yet:

* The §D.4.2 termination + §D.6 selective arithmetic-coding bypass
  (raw bit) modes, and §D.7 vertically causal context formation (a
  COD `Scod` bit-3 mode). **§D.5 error-resilience segmentation
  symbols are now decoded** (round 214) via
  `t1::decode_segmentation_symbol` and the
  `BitPlaneSequencer::with_segmentation_symbols` toggle that the
  Table A.19 COD / COC flag drives.
* The MQ **encoder** (§C.2 — INITENC / ENCODE / RENORME / BYTEOUT /
  FLUSH) and the §D.6 selective arithmetic-coding bypass (raw bit mode).
* §B.12.3 POC-marker placement validation — §B.12.2 progression order
  volume iteration is now implemented (Equation B-21 `(CSpoc, CEpoc) ×
  (RSpoc, REpoc) × (0, LYEpoc)` bounds + the per-`(component,
  resolution, precinct)` "next unsent layer" cursor that prevents
  packet repetition across chained volumes), but §B.12.3 layout rules
  ("if a POC marker is used for an individual tile, there shall be a
  POC marker in the first tile-part header of that tile and all of
  the progression order changes shall be signalled in the tile-part
  headers of that tile") are not yet enforced at the codestream-walker
  level. §B.8 layer / §B.9 packet assembly is also pending.
* §B.10.7.2 multi-codeword-segment splitting (round 5 emits one
  segment length per included code-block; termination boundaries are
  a tier-1 input we don't have yet).
* Inverse 5-3 and 9-7 wavelet transforms.
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
  exponent `xcb = value + 2`), §B.9 (precinct → code-block confinement
  — "the code-block contributions appear in raster order, confined to
  the bounds established by the relevant precinct"; only code-blocks
  that contain samples from the relevant sub-band, confined to the
  precinct, have any representation in the packet), §B.12.1.3 /
  Equation B-20 (the `2^(PP + NL - r)` reference-grid precinct step
  that, divided by the sub-band's `2^(NL - r + 1)` scale, yields the
  projected precinct exponent on each high-pass sub-band — `PP - 1`
  at `r ≥ 1`, `PP` at `r = 0`).
* T.800 Annex C (Arithmetic entropy coding — decoder) — §C.1.2 (the
  `0x8000 ≈ 0.75` fixed-point convention and the `A ∈ [0.75, 1.5)`
  renormalization range), §C.2.5 (the probability-estimation state
  machine driving NMPS / NLPS / SWITCH on renormalization), §C.3.1 /
  Table C.3 (the Chigh:Clow decoder register split — comparison uses
  Chigh alone), §C.3.2 / Figures C.15 / C.16 / C.17 (DECODE + the
  MPS-path and LPS-path conditional MPS/LPS exchange), §C.3.3 / Figure
  C.18 (RENORMD), §C.3.4 / Figure C.19 (BYTEIN — the `0xFF`-prefixed
  stuff-bit rule + the `> 0x8F` marker / `0xFF`-fill end of stream),
  §C.3.5 / Figure C.20 (INITDEC), §C.3.6 (statistics reset), and Table
  C.2 (the 47 `Qe` / NMPS / NLPS / SWITCH rows — index 35's OCR
  `0x02Al` resolved to `0x02A1` from its binary column). The figures
  are images in the PDF; their register operations are transcribed from
  the accompanying §C.3 prose to integer ops.
* T.800 Annex D §D.1–§D.3 (Coefficient bit modelling) — §D.1 (the
  code-block scan pattern: horizontal stripes of height 4, scanned
  column-by-column within each stripe, top to bottom; Figure D.1), §D.2
  (the §D.2.1 coefficient-bit / sign-bit `sb(u, v)` / `Nb(u, v)`
  notations), §D.3 (the significance-state σ definition + the Figure D.2
  eight-neighbour context layout + the "out-of-block neighbours are
  insignificant" rule + the three-pass / cleanup-only-first-bit-plane
  framing), §D.3.1 + Table D.1 (the 9 significance-propagation context
  labels per sub-band orientation from `∑Hi` / `∑Vi` / `∑Di`, with the
  LL/LH ↔ HL H/V-axis swap and the HH `∑(Hi+Vi)` / `∑Di` reduction),
  §D.3.2 + Table D.2 + Table D.3 + Equation D-1 (the sign-context
  two-step: vertical/horizontal contribution from neighbour signs, then
  the 5 sign-context labels + XORbit, `signbit = D ⊕ XORbit`), and
  §D.3.3 + Table D.4 (the 3 magnitude-refinement context labels: 14 / 15
  for a first refinement keyed on `∑(Hi+Vi+Di) = 0` vs `≥ 1`, 16 for any
  later refinement, with the "already significant except just-made-
  significant" eligibility rule), and §D.3.4 + Table D.5 (the cleanup
  pass: the run-length context for a four-zero-context column inside a
  full 4-row stripe, the UNIFORM-context 2-bit MSB-then-LSB first-
  significant index, and the Table D.1 fall-back for ineligible columns).
  Tables D.1 / D.2 / D.3 / D.4 / D.5 are transcribed verbatim; the
  Figure D.1 / D.2 diagrams are transcribed to scan order + neighbour
  offsets.
* T.800 Annex D §D.4 (Initializing and terminating) — Table D.7 (the
  initial context states: UNIFORM index 46, run-length index 3,
  all-zero-neighbours index 4, all other contexts index 0) and §D.4.1
  (the decoder extends the input bit stream with `0xFF` bytes until all
  symbols are decoded — the basis for the `mq` BYTEIN end-of-stream
  fill).
* T.800 Annex D §D.5 (Error resilience segmentation symbol) — the four-
  bit `1010` (= `0xA`) symbol coded under the UNIFORM context at the
  end of every cleanup pass when the COD / COC Table A.19
  segmentation-symbols flag is set, decoded MSB-first; a non-`0xA`
  decoded value flags bit-plane corruption. The §D.5 NOTE on
  independence from the §D.4.2 predictable-termination flag is the
  basis for the toggle living on the sequencer rather than being gated
  by termination.
* T.800 §A.6.1 / §A.6.2 — Table A.19 SPcod / SPcoc code-block-style
  byte: bit 0 selective arithmetic-coding bypass, bit 1 reset context
  probabilities on coding pass boundaries, bit 2 termination on each
  coding pass, bit 3 vertically causal context, bit 4 predictable
  termination, bit 5 segmentation symbols. The two-bit reserved high
  field (the Table A.19 "Decoders may ignore the first and second
  most significant bits …" prose) is preserved in raw form for
  diagnostic inspection.
* T.800 §B.12 (Progression order) — all five §B.12.1 base orders:
  §B.12.1.1 (the LRCP four-nested `for l for r for i for k` loop body),
  §B.12.1.2 (the RLCP `for r for l for i for k` loop body — the same
  packet set emitted resolution-first), and the three position-keyed
  orders §B.12.1.3 RPCL (`for r for y for x for i / for l`), §B.12.1.4
  PCRL (`for y for x for i for r / for l`) and §B.12.1.5 CPRL (`for i
  for y for x for r / for l`). The position-keyed orders use the
  §B.12.1.3 NOTE's efficient precinct-corner enumeration (most `(x, y)`
  reference-grid samples include no packet; the corners are computed
  directly via Equation B-20's `2^(PP + NL − r)` precinct step scaled
  by `XRsiz` / `YRsiz`, anchored at the §B.6 partition origin and
  clipped to the tile origin) in place of the literal `(x, y)` sweep.
  Also the `Nmax = max_i(NL_i)` definition shared by LRCP / RLCP; the
  §B.12 NOTE (components with `NL_i < r` contribute no packet at that
  `r` — resolution-level indices synchronise at `r = 0`); §B.6 / §B.9
  (empty precincts still produce packets so they remain in the precinct
  count / grid handed to the drivers). Not yet covered: §B.12.2
  progression-order volumes (the Equation B-21 `POC`-bounded
  sub-ranges) and §B.12.3 POC order changes.

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
