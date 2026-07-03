# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K codestream + JP2 file format) decoder **and
encoder** for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
Written from scratch against the ITU-T T.800 / ISO-IEC 15444-1 standard
documents under `docs/image/jpeg2000/` only.

## Capability

The decoder reconstructs **pixel-exact** images across the core Part-1
decode path, validated against committed end-to-end fixtures (gray,
lossless 5-3 and lossy 9-7, multiple decomposition levels, code-block
sizes, precinct sizes, and quality layers). Fixtures are encoded and
COM-scrubbed with an opaque CLI codec used strictly as a black box; no
external library source is consulted.

What is implemented:

- **Containers** — J2K raw codestream and the JP2 ISO BMFF box wrapper
  (`jP`, `ftyp`, `jp2h` / `ihdr` / `bpcc` / `colr`, `jp2c`), with all
  three box length encodings.
- **Main header** — `SOC`, `SIZ`, `COD`, `QCD`, plus the typed
  tile-part-header markers (`COD`, `COC`, `QCD`, `QCC`, `RGN`, `POC`,
  `PLT`, `PPT`, `COM`); 8- vs 16-bit component-index width is selected
  from `Csiz`.
- **Tile-part chain** — `SOT` / `SOD` / `EOC` walk, both fixed-`Psot`
  and `Psot = 0` ("body until EOC") framings.
- **Geometry** — SIZ-derived tile / tile-component bounds, per-resolution
  and per-sub-band corners, precinct partition, and precinct →
  code-block enumeration (T.800 §B.2 – §B.9).
- **Tier-2** — the bit-stuffed packet-header reader (§B.10): tag trees,
  code-block inclusion, zero-bit-plane counts, coding-pass codewords,
  and `Lblock` segment-length reads, with optional SOP / EPH framing.
  When SOP framing is enabled the §A.8.1 `Nsop` packet sequence number
  is validated against the running per-tile packet ordinal (rolling over
  at 65 536), so a desynchronised or lost packet is rejected rather than
  mis-decoded; the per-packet-optional SOP rule is honoured.
  **Relocated packet headers** (`PPT`, §A.7.5; `PPM`, §A.7.4) are
  decoded: when a tile's tile-part headers carry `PPT` marker segments
  (or the main header carries a `PPM`), every packet header is read from
  the relocated payload while the tile body supplies only packet data.
  `PPT` payloads are concatenated per tile in `Zppt` order; a `PPM`
  payload is gathered in `Zppm` order across the main header, split into
  the per-tile-part `(Nppm, Ippm)` series (handling an `Nppm` run that
  straddles a `PPM` segment boundary), and mapped onto each tile's
  tile-parts by codestream ordinal. A gap / duplicate in either
  `Z`-index run is rejected as a lost segment, and `PPM` alongside `PPT`
  is rejected (§A.7.4 mutual exclusion). The §A.8.1 / §A.8.2 framing
  split is honoured — an in-body `SOP` (with its `Nsop` still
  validated) precedes each packet's data and a required `EPH` trails
  each header inside the relocated header buffer. Both relocations are
  validated **pixel-exact** end-to-end: a clean-room transcoder moves a
  real fixture's in-stream headers into `PPT` / `PPM` and the decoded
  output is asserted identical to the in-stream original across the 5-3
  lossless and 9-7 irreversible multi-resolution, multi-precinct,
  multi-layer and RGB/RCT paths.
- **Tier-1** — the MQ arithmetic decoder (Annex C) and all three Annex D
  coding passes (significance-propagation + sign, magnitude refinement,
  cleanup with the run-length / UNIFORM shortcut), the §D.5
  segmentation symbol, the §C.3.6 / §D.4 **reset of context
  probabilities** style bit (Table A.19 Scod bit 1) — contexts
  re-initialise to their Table D.7 states at each coding-pass boundary
  over the same single codeword segment — and the §D.4.2 **termination
  on each coding pass** style bit (Table A.19 Scod bit 2): every pass is
  flushed into its own terminated §C.3 codeword segment, so the
  §B.10.7.2 multi-segment packet-header lengths are read (`K = passes`,
  one increase-`Lblock` prefix) and a fresh MQ decoder is opened per
  pass while the Annex D contexts persist across the per-pass
  boundaries. The §D.6 **selective arithmetic-coding bypass** style bit
  (Table A.19 Scod bit 0) is honoured: from bit-plane 5 onward the
  significance-propagation and magnitude-refinement passes read raw
  (lazy) bits from a §D.6 bit-stuffed stream while the cleanup passes
  stay arithmetic-coded, the code-block contribution carves into the
  §B.10.7.2 / Table D.9 AC + raw codeword segments (the terminated-pass
  set `T` is keyed off the absolute pass index, so it carries across
  layers), and the tier-1 driver alternates a fresh MQ decoder and a
  raw-bit reader on one continuous §D.3 schedule. Bit-2 composes with
  bypass per the §D.6 prose (every pass terminated, both raw passes
  included). The raw spans honour the §D.4.1 / §D.6-NOTE-2 model — once
  a span's stored bytes run out the reader extends it with synthesised
  `0xFF` fill (stuff-bit rule applied) so a truncated or in-progress raw
  pass still decodes. Validated end-to-end on the 5-3 lossless, 9-7
  irreversible, and 2×2-tile bypass paths. The §D.4.2 **predictable
  termination** style bit (Table A.19 Scod bit 4) is enforced as a
  decode-time conformance check: each terminated MQ codeword segment's
  decoder must land exactly on the `§B.10.7` segment boundary, so a
  stream that signals predictable termination but whose segments were
  not flushed by the §D.4.2 procedure (or is truncated) is rejected
  rather than silently mis-decoded. Forced off for HT code-blocks
  (T.814 Table A.13).
- **Reassembly** — per-coefficient `Nb(u, v)` magnitude-bit tracking for
  rate-truncated streams, dequantisation, the 5-3 and 9-7 inverse DWT,
  and the inverse multi-component transform.
- **Per-component quantisation** — main-header `QCC` overrides of the
  `QCD` default (T.800 §A.6.5, `Main QCC > Main QCD`): each component's
  quantisation style, guard bits and step sizes are resolved
  independently.
- **Per-component coding style** — main-header `COC` overrides of the
  `COD` default (T.800 §A.6.2, `Main COC > Main COD`): each component's
  decomposition-level count `NL`, code-block size, precinct partition
  and wavelet kernel are resolved independently, so the per-component
  geometry, tier-1 and inverse-DWT cascade all run against the right
  parameters. **Mixed wavelet kernels per component** are honoured when
  no multiple-component transform is active (`Rmct = 0`): Table A.17
  only pairs the MCT (RCT / ICT) with one kernel shared across
  components 0–2, but with the MCT off §G.1.2 collapses to a
  per-component DC level-shift + clamp with no cross-component coupling,
  so a tile whose `COC` gives one component the 5-3 kernel and another
  the 9-7 kernel reconstructs each in its own `i32` / `f64` lane and
  re-interleaves them into component order. Validated end-to-end by a
  clean-room assembler that splices a 5-3 and a 9-7 single-component
  stream into one two-component codestream and asserts each component
  reconstructs identically to its standalone decode. A mixed-kernel tile
  that *also* signals an MCT (`Rmct = 1`) is rejected. The Table A.19
  code-block **style** byte is held global to the code; a `COC` that
  diverges from the `COD` style is cleanly rejected.
- **Progression** — all five §B.12.1 orders (LRCP, RLCP, RPCL, PCRL,
  CPRL) and the §A.6.6 **progression order change** (`POC`) wired into
  the decode driver: a main-header or first-tile-part `POC` drives the
  §B.12.2 volume enumeration (each volume's component / resolution /
  layer sub-range in its own order, with the per-(component, resolution,
  precinct) "next unsent layer" cursor), under the §A.6.6 precedence
  `Tile-part POC > Main POC > Tile-part COD > Main COD`. Plus
  **multi-layer** / **multi-precinct** reassembly. The position-keyed
  orders project each precinct to its reference-grid corner for any
  integer `XRsiz` / `YRsiz`; the power-of-two requirement is enforced
  only for RPCL (§B.12.1.3) and PCRL (§B.12.1.4), while **CPRL**
  (§B.12.1.5) decodes at **non-power-of-two sub-sampling** too.
- **Region of interest** — main-header `RGN` implicit-ROI (Maxshift)
  decode (T.800 §A.6.3 / §H.1): the `SPrgn` scaling value `s` is
  resolved per component, the tier-1 schedule runs against the
  increased coded bit budget `M'b = Mb + s`, and the §H.1 three-branch
  de-scaling re-anchors each coefficient to the background `Mb` and
  rewrites its per-coefficient `Nb(u, v)` before reassembly (background
  coefficients keep their magnitude and drop `Nb` by `s`; ROI
  coefficients keep their top `Mb` bits and cap `Nb = Mb`).
- **Tile-part header overrides** — a tile's first tile-part
  (`TPsot = 0`) `COD` / `COC` / `QCD` / `QCC` / `RGN` markers override
  the main-header defaults for that tile only (T.800 §A.6.1 – §A.6.5).
  The coding parameters are resolved **per tile** along the §A.6
  precedence chains `Tile-part COC > Tile-part COD > Main COC > Main
  COD` and `Tile-part QCC > Tile-part QCD > Main QCC > Main QCD`: a tile
  `COD` supersedes the main `COD` and `COC`s for the whole tile (only
  the tile `COC`s then refine it per component) and the quantisation
  chain mirrors that shape; a tile `RGN` overrides the main ROI shift
  for its component. The §A.6 "overrides only in `TPsot = 0`" rule and
  the at-most-one / duplicate / out-of-range / divergent-style faults
  are enforced.
- **High-Throughput JPEG 2000 (HTJ2K)** — the ITU-T T.814 | ISO/IEC
  15444-15 high-throughput block coder, decoded end-to-end. The `CAP`
  marker is parsed and accepted when it signals HTJ2K (Pcap bit 15) and
  the `SPcod` / `SPcoc` bit-6 flag (T.814 A.4) routes each code-block to
  the HT block decoder instead of the Annex D MQ path. The HT decoder
  implements the full clause-7 algorithm: the 7.1 bit-stream recovery
  state machines (MagSgn, MEL, VLC, SigProp, MagRef, each with the
  spec's `0xFF`-stuffing rule), the 7.3.3 MEL adaptive run-length
  decoder, the 7.3.5 context-adaptive VLC over the Annex C CxtVLC
  tables (444 + 358 entries transcribed verbatim), the 7.3.6 U-VLC
  prefix/suffix/extension (with the first-line-pair both-offset MEL
  special case), the 7.3.5 / 7.3.7 quad contexts and exponent
  predictors over the 7.2 quad scan, the 7.3.8 MagSgn value recovery,
  and the 7.4 SigProp + 7.5 MagRef refinement passes folded into the
  7.6 sample output. Validated **bit-exact** against the
  `ojph_compress` / `ojph_expand` black-box validator across grayscale,
  RGB (RCT), reversible 5-3 and irreversible 9-7, 1-4 decomposition
  levels, and **multiple HT code-blocks per sub-band** (a 32×32 band
  tiling into four 16×16 blocks, and a 128×128 / 4-decomposition image
  whose high-pass bands each carry several 32×32 HT code-blocks). The
  Annex C CxtVLC tables are confirmed byte-identical to the spec listing
  (a transcription audit diffs all 802 entries). Currently covers the
  SINGLEHT / HTONLY / single-HT-set case; the MULTIHT (multiple HT sets
  per code-block, bit-plane skipping via zero-length HT sets) and
  placeholder-pass (`P0 > 0`) variants are not yet exercised. One
  **known limitation** remains: a small HT code-block (≈ ≤ 12 samples
  per side) carrying *very high energy* coefficients — as arises in the
  high-pass sub-bands of a **non-power-of-two** image dimension — can
  over-read the §7.1.2 MagSgn bit-stream and surface
  `Error::HtCorruptSegment`. Every clause-7 procedure and both CxtVLC
  tables have been verified faithful to T.814, so the divergence is a
  not-yet-isolated emergent decode error in that corner; pinning it
  needs a clean-room per-quad MagSgn/VLC bit-position reference trace
  (see the docs-gap note below).

## Encoder

The crate carries a full **encode** path built from the same clean-room
spec surface, round-trip-validated against this crate's own decoder and
independently confirmed conformant by an opaque black-box decoder
(every configuration below reconstructs **byte-identically** through
it):

- **MQ arithmetic encoder** (Annex C §C.2) — INITENC / ENCODE
  (CODEMPS / CODELPS with the conditional exchange) / RENORME / BYTEOUT
  bit-stuffing + carry handling / FLUSH, the exact inverse of the §C.3
  decoder (validated over pseudo-random multi-context decision streams).
- **Tier-1 forward coding passes** (Annex D §D.3) — encode-side
  significance-propagation, magnitude-refinement, and cleanup passes
  (incl. the Table D.5 run-length mode and the §D.3.2 sign subroutine),
  sharing the decoder's scan order and context formation so the
  progressive state stays in lock-step by construction. A segmented
  scheduler terminates codeword segments per Table D.9 / §D.4.2 when a
  termination style is signalled.
- **Coding styles on encode** — the §D.6 **selective
  arithmetic-coding bypass** (Table A.19 bit 0: SP / MR passes from
  bit-plane 5 write raw bits through a §D.6 stuff-bit writer while
  cleanups stay MQ) and §D.4.2 **termination on each coding pass**
  (bit 2), separately or composed, with the §B.10.7.2 multi-segment
  length sequences written by the generalised tier-2 writer.
- **Forward DWT** (§F.4) — 1-D + 2-D 5-3 (bit-exact inverse pair) and
  9-7 (round-off-exact) analysis over the same PSEO extension, with the
  lifting parity and Table B.1 band corners anchored at each tile's
  absolute reference-grid coordinates.
- **Tier-2 packet-header writer** (§B.10) — bit-stuffing writer,
  tag-tree encoder, Table B.4 coding-passes codewords,
  minimal-`Lblock` single- and multi-segment length sequences, and the
  §B.10.8 packet-header composer with §B.10.3 empty packets, driven
  across quality layers by a persistent per-precinct encoder state.
- **Codestream assembly** — `SOC` / `SIZ` / `COD` / `QCD` / `QCC` /
  per-tile `SOT` / `SOD` / `EOC` in the §A.3 order; geometry and packet
  order are derived from the same `geometry` / `progression` code the
  decoder uses.
- **Structured parameters** (`encode::EncodeParams` +
  `encode::encode_j2k`) — decomposition levels, code-block exponents,
  kernel, MCT, and:
  - **All five §B.12.1 progression orders** (LRCP / RLCP / RPCL /
    PCRL / CPRL), signalled in `SGcod` and emitted by the decoder's own
    progression drivers.
  - **User-defined precinct partitions** (§B.6 / Table A.21, `Scod`
    bit 0) with the §B.7 precinct-capped code-block grid — one packet
    per precinct, making the position-keyed orders genuinely
    interleave.
  - **Quality layers** (Annex J.13.2 guidance): each code-block's
    passes are distributed over `L` layers by coded depth on a global
    bit-plane scale and its codeword segment is cut at the Annex J.13.4
    per-pass truncation rates `R^n` (encoder-state snapshots), so an
    independent decoder's layer-limited decodes improve monotonically
    (measured MSE 4373 → 50 → 1.3 → exact on a lossless 4-layer
    stream) while full decode stays bit-exact.
  - **PCRD rate control** (Annex J.13.3): per-block monotone-slope
    truncation sets over `(R^n, D^n)` — distortions from a §E.1.1.2
    midpoint-reconstruction model weighted by the sub-band
    synthesis-waveform L2 norm (J.13.4.1, computed by running an
    impulse through this crate's own synthesis) — with the Equation
    J-13 threshold λ bisected to the largest stream not exceeding
    `target_bytes` (observed within ≤ 5 bytes of budget); truncated
    blocks are re-encoded so the emitted segment is exactly
    §C.2.9-terminated.
  - **Multi-tile encode** (§B.3): an `XTsiz × YTsiz` grid, each tile
    transformed and coded independently into its own tile-part —
    including odd-anchored tiles (absolute-parity lifting) and tiny
    tiles whose deeper levels go empty.
- **Lossless** (`encode_j2k_lossless`) — reversible 5-3, Table A.28
  style 0, `εb = RI + gain` (Table E.1); decodes back **bit-exactly**.
  Optional §G.2 **RCT** (`encode_j2k_lossless_rct`, `SGcod` MCT = 1)
  with the chroma dynamic-range bit signalled via per-component `QCC`.
- **Lossy** (`encode_j2k_lossy`) — irreversible 9-7 with Annex E
  scalar-expounded quantisation (Table A.28 style 2); a `fine_bits`
  knob sets the uniform Equation E-3 step `Δb = 2^(−fine_bits)`.
  Optional §G.3.1 **ICT** (`encode_j2k_lossy_ict`, MCT = 1 with the
  9-7 kernel per Table A.17).
- The `oxideav-core` registry installs the **`Encoder` trait** impl
  alongside the decoder (`make_encoder`), and the historical
  `encode_jpeg2000(pixels, w, h)` byte-vector entry point encodes 1-
  (gray) and 3-component (RGB via RCT) interleaved 8-bit input.

Not yet on the encode side: component sub-sampling, >8-bit input,
SOP / EPH framing, POC emission, multiple tile-parts per tile
(`TPsot > 0`), PPM / PPT relocation, per-component `COC` / `QCC`
overrides, ROI, and HTJ2K encoding.

### Not yet implemented

These surface a clean `Error::NotImplemented` rather than mis-decoding:

- A `COC` whose Table A.19 code-block **style** byte diverges from the
  `COD` (the common `COC` override of per-component `NL` / code-block
  size / precincts / kernel *is* honoured, including **different kernels
  per component** when the MCT is off), in both the main and tile-part
  headers (main-header *and* tile-part `COD` / `COC` / `QCD` / `QCC`
  overrides are otherwise honoured). A mixed-kernel tile that also
  signals a multiple-component transform (`Rmct = 1`) is rejected — the
  RCT / ICT requires one kernel across components 0–2.
- A non-Maxshift `RGN` style. T.800 Table A.25 (Part 1) defines **only**
  `Srgn = 0` (implicit ROI / Maxshift) — all other values are reserved
  in Part 1, and the main-header *and* tile-part Maxshift `RGN` *are*
  honoured. The "scaling based" arbitrary-shaped ROI (`Srgn = 1`
  rectangle / `Srgn = 2` ellipse) is an **ISO/IEC 15444-2 (Part 2)**
  extension (extended RGN marker + `Rsiz` capability + the Annex L
  wavelet-domain ROI-mask generation and mask-driven L.1 de-scaling),
  outside this Part-1 decoder's scope; an `Srgn ≠ 0` (or a Part-2
  extended-length) `RGN` surfaces a clean error rather than mis-decoding.
- **RPCL / PCRL** under non-power-of-two sub-sampling — §B.12.1.3
  ("must") and §B.12.1.4 ("shall") require power-of-two `XRsiz` / `YRsiz`
  for those two orders, so a non-power-of-two factor there is rejected.
  **CPRL** (§B.12.1.5) carries no such restriction and *is* decoded at
  any integer sub-sampling.
- HTJ2K MULTIHT codestreams (more than one HT set per code-block) and
  HT code-blocks that begin with placeholder passes (`P0 > 0`); the
  SINGLEHT / single-HT-set HTJ2K path *is* decoded (see above).
- A small, very-high-energy HT code-block from a **non-power-of-two**
  sub-band can over-read the §7.1.2 MagSgn stream
  (`Error::HtCorruptSegment`). Power-of-two block geometries decode
  bit-exact at any energy. **Docs gap:** isolating this needs a
  clean-room per-quad MagSgn / VLC bit-position reference trace for a
  high-energy odd-dimension HT cleanup segment — every clause-7
  procedure and both Annex C CxtVLC tables have been verified faithful
  to T.814, so the spec text alone is insufficient to localise the
  emergent divergence.

## Public API

```rust
let codestream = oxideav_jpeg2000::parse_codestream(bytes)?;
let header     = oxideav_jpeg2000::parse_j2k_header(bytes)?;
let container  = oxideav_jpeg2000::jp2::parse_jp2(bytes)?;
# Ok::<(), oxideav_jpeg2000::Error>(())
```

Decoding: `decode_j2k` (planar) / `decode_jpeg2000` (interleaved bytes).
Encoding: `encode::encode_j2k` with `encode::EncodeParams` (kernel,
MCT, progression order, precincts, quality layers, PCRD
`target_bytes`, tile grid, bypass / termination styles), the
`encode_j2k_lossless` / `encode_j2k_lossless_rct` / `encode_j2k_lossy`
/ `encode_j2k_lossy_ict` wrappers, or the historical
`encode_jpeg2000(pixels, w, h)`.

The crate also registers a software decoder **and encoder** through the
standard `oxideav-core` registry path.

## Clean-room provenance

Every module was written from the T.800 / ISO-IEC 15444-1 standards
documents under `docs/image/jpeg2000/` only — the codestream and JP2
syntax (Annex A + Annex I), tier-2 packet headers (§B.10, both read and
write sides), tile / sub-band / precinct / code-block geometry
(§B.2 – §B.9), the MQ arithmetic coder (Annex C, decoder §C.3 and
encoder §C.2), coefficient bit modelling (Annex D, decode and forward
passes), the wavelet transforms (Annex F, synthesis and §F.4 analysis),
quantisation (Annex E), component transforms (Annex G), and progression
orders (§B.12). PDF figures are transcribed to integer operations from
the accompanying prose. No external JPEG 2000 implementation is read or
wrapped; opaque CLI codecs are used strictly as black-box validators.

## License

MIT — see [LICENSE](LICENSE).
