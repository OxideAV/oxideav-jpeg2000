# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

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

* The §D.4.2 / §D.5 / §D.6 termination / error-resilience segmentation
  symbol / selective arithmetic-coding bypass (raw bit) modes, and §D.7
  vertically causal context formation (a COD `Scod` bit-3 mode).
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
