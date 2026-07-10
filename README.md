# oxideav-jpeg2000

[![CI](https://github.com/OxideAV/oxideav-jpeg2000/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-jpeg2000/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-jpeg2000.svg)](https://crates.io/crates/oxideav-jpeg2000) [![docs.rs](https://docs.rs/oxideav-jpeg2000/badge.svg)](https://docs.rs/oxideav-jpeg2000) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

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

On the irreversible 9-7 path — including **rate-truncated** streams
(the §E.1.1.2 per-coefficient `Nb(u, v)` midpoint reconstruction) —
the decode is **byte-exact against an independent black-box reference
decoder** across the committed fixtures and a 60-case ISO/IEC
15444-4-style sweep (§B.2.4 peak / MSE metrics over an encode matrix
of sizes, levels, styles, progressions, truncations and ROI). A second
reference decoder disagrees with the first by ±1 at a handful of
pixels whose reconstructed continuous value lands within ~0.004 of a
half-integer; that inter-reference rounding latitude is exactly what
ISO/IEC 15444-4 budgets (Table C.1 allows peak ≤ 109 / MSE ≤ 743 on
its 9-7 test codestreams — this decoder measures peak ≤ 1,
MSE ≤ 0.005 against that reference and 0 against the other), and the
tests pin both verdicts per fixture.

What is implemented:

- **Containers** — J2K raw codestream and the JP2 ISO BMFF box wrapper
  (`jP`, `ftyp`, `jp2h` / `ihdr` / `bpcc` / `colr`, `jp2c`), with all
  three box length encodings; plus the **JPH** (HTJ2K, T.814 Annex D)
  profile of the same layout — the `'jph '` brand, the §D.2
  no-`colr`-under-`UnkC` exemption, and the §D.4 `METH` values 3 (any
  ICC profile) and 5 (H.273 parameterized colourspace).
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
  termination** style bit (Table A.19 Scod bit 4) is parsed and carried,
  and — per §D.4.2 — treated as an *encoder-side* flush contract:
  decoding is unchanged (the §D.4.1 synthesised `0xFF` extension applies
  as usual; real predictable-termination streams routinely finish their
  final renormalisations inside it, so no landing-position check can be
  made without rejecting conforming streams — a mis-rejection this
  decoder performed through round 409). All six Table A.19 style bits
  are pinned pixel-exact against real black-box-encoder fixtures,
  including the 0x11 / 0x14 / 0x30 / 0x3F combinations. Bits 0/1/2/4/5
  forced off for HT code-blocks (T.814 Table A.13).
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
  that *also* signals an MCT (`Rmct = 1`) is rejected. **The Table
  A.19 code-block style byte also resolves per component**: a `COC`
  whose style diverges from the `COD` gives its component its own
  §B.10.7 segment split and tier-1 dispatch — an Annex D component
  with the §D.6 bypass / §D.4.2 termination styles coexists with a
  default-style sibling, and a component whose `SPcoc` bit 6 signals
  HT block coding coexists with an Annex D sibling: the T.814 §8.2
  **HTDECLARED** set. Both mixes are validated end-to-end by a
  clean-room assembler that splices an HT (or styled) and a plain
  single-component stream into one two-component codestream (`Rsiz`
  bit 14 + `CAP` with the HTDECLARED `Ccap15`) and asserts each
  component reconstructs identically to its standalone decode, in
  both component orders.
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
  (a transcription audit diffs all 802 entries). The §B.2 set-`T`
  codeword-segment split is honoured on read — a packet whose HT
  contribution carries a refinement segment (`Z_blk = 3`) slices the
  cleanup and SigProp + MagRef lengths separately, and the block
  decoder records per-coefficient `Nb` (a refined sample carries one
  more decoded plane) so the §E.1 reconstruction is exact. Beyond the
  SINGLEHT / HTONLY case, **MULTIHT** codestreams (§8.3) decode: the
  accumulated codeword segments group into per-set §B.1 cleanup /
  refinement HT segments (a refinement segment split across packets is
  concatenated), each set's `Z_blk` follows §B.3 (a zero-length
  refinement segment demotes its SigProp / MagRef passes; a zero-length
  cleanup segment marks a bit-plane-skip set), and the block decodes
  from the **last** set whose cleanup segment is present — each set
  re-codes the block one bit-plane finer, `S_blk = P + P0 + S_skip`.
  **Placeholder passes** (§B.1, `P0 > 0`) are resolved with no side
  channel: the §B.3 one-cleanup-per-first-packet rule leaves a single
  candidate index for the first cleanup pass in a contribution, and the
  required `Lcup > 1` (vs. a placeholder run's mandatory zero length)
  pins `3·P0` from the first §B.10.7 length field, which then anchors
  the set-`T` boundaries, the Equation B-19 widths and `S_blk`. (The
  available opaque HTJ2K decoders are SINGLEHT-only and decline these
  streams, so the MULTIHT shapes are validated against this crate's own
  encoder plus spec-level unit tests of the split and the `P0`
  pinning.) The long-standing small-block / high-energy /
  non-power-of-two decode divergence is **resolved**: differential
  tracing against this crate's own independently written HT *encoder*
  isolated it to the §7.3.4 / §7.3.6 first-line-pair interleave — when
  `s_mel = 0` and `u_q1 > 2`, the second quad's single `u` bit replaces
  the *prefix step* and therefore precedes the first quad's suffix bits
  (decidable from the prefix alone per the §7.3.6 NOTE). With the fix a
  264-stream black-box sweep (odd and even dimensions to 100×80, 1–5
  decomposition levels, 4×4–64×64 code-blocks, full-range noise)
  decodes byte-identical.

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
  - **Multiple tile-parts per tile** (§A.4.2, `TPsot > 0`): a
    `TilePartSplit` cuts each tile's packet sequence into
    `TPsot`-indexed tile-parts wherever the resolution / layer /
    component axis changes along the emission order (each part with
    its own `SOT` + `SOD`; `Nsop` numbering continues across a tile's
    parts; `TNsot > 255` rejected).
  - **SOP / EPH packet framing** (§A.8.1 / §A.8.2, `Scod` bits 1 / 2):
    6-byte `SOP` segments with per-tile `Nsop` numbering and/or the
    2-byte `EPH` after every packet header, composing with layers,
    styles, tiles, and rate control (the PCRD budget binds on the
    framed length).
  - **POC emission** (§A.6.6 / Table A.32): progression-order-change
    entries carried in a main-header `POC` and emitted through the
    decoder's own §B.12.2 volume walk (layer cursors included), with
    full-coverage validation so no packet is silently dropped.
  - **Component sub-sampling** (§B.2, SIZ `XRsiz` / `YRsiz` 1..=255
    per component): planes on their own component grids, per-tile
    Equation B-12 tile-component regions, the §B.12.1.3–.5
    position-order projections, and the RPCL / PCRL power-of-two
    gate; 4:2:0 / 4:2:2 / asymmetric layouts round-trip bit-exactly.
  - **Region of interest** (Annex H, Maxshift): a reference-grid
    rectangle (`EncodeParams::roi`) is traced backwards through the
    wavelet cascade into each component's §H.3.1 coefficient mask
    (5-3 reach `L(n)…L(n+1)` / `H(n−1)…H(n+1)`, 9-7 reach
    `L(n−1)…L(n+2)` / `H(n−2)…H(n+2)`, per level and axis), the
    masked quantized coefficients scale up by the §H.2.2 value
    `s = max(Mb)` (Equation H-6, per component — the RCT chroma bit
    and the lossy `fine_bits` excess grow it), and one `RGN` marker
    per component signals `Srgn = 0` / `SPrgn = s`. Full decodes are
    unchanged (lossless stays bit-exact); under a PCRD budget every
    ROI bit-plane precedes the background so the region reconstructs
    first. The coded budget `M'b = 2s` must fit the 30-bit magnitude
    lane (all 8-bit shapes fit; 9-7 up to `fine_bits = 4`, deeper
    inputs up to 12-bit) — an overflowing combination is cleanly
    rejected. Composes with RCT, tiles, sub-sampling and PPM / PPT.
  - **Packed packet headers** (§A.7.4 / §A.7.5): every §B.10 packet
    header relocated out of the tile-part bodies into per-tile `PPT`
    marker segments (carried in the tile's first tile-part header) or
    whole-codestream main-header `PPM` segments (one `(Nppm, Ippm)`
    entry per tile-part in codestream order), each marker segment cut
    only on a completed packet header (multi-segment `Zppm` / `Zppt`
    runs when the payload outgrows the 16-bit length); a signalled
    `SOP` stays in the body before each packet's data and a signalled
    `EPH` trails each relocated header (§A.8.1 / §A.8.2). Composes
    with tiles, tile-part splits, layers and PCRD (the budget binds on
    the relocated stream).
  - **Per-component `COC` / `QCC` overrides** (§A.6.2 / §A.6.5):
    per-component `NL` / code-block size / precinct partition /
    wavelet kernel (mixed 5-3 / 9-7 siblings when the MCT is off),
    with a `QCC` emitted whenever the implied quantisation table
    diverges (unified with the RCT chroma `QCC`).
- **>8-bit input** (`encode_j2k_u16`) — any Table A.11 unsigned depth
  up to 16 bits through the whole pipeline (both kernels, both MCT
  pairings, sub-sampling, framing); 9/12/16-bit lossless round-trips
  are bit-exact and the lossy `Δb` error bound is depth-independent.
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

The crate also **encodes HTJ2K** (T.814): setting
`EncodeParams::high_throughput` routes every code-block through the
HT forward block coder and assembles a conformant HTJ2K codestream.
The forward coder covers the §7.3 cleanup pass
(`htenc::encode_ht_cleanup_segment` — the three §7.1 bit-stream
writers with their stuffing rules and the backward VLC byte layout,
the §7.3.3 adaptive MEL run-length encoder, Annex C CxtVLC entry
selection, §7.3.6 U-VLC residuals with the §7.3.4 quad-pair
interleave, and §7.3.8 MagSgn emission) **and** the §7.4 SigProp +
§7.5 MagRef refinement passes (`htenc::encode_ht_refinement_segment`
— forward duals of the stripe-oriented scans writing the §7.1.5
forward and §7.1.6 backward refinement bit-streams, both stuffing
state machines included). With `EncodeParams::ht_refinement` each
block's cleanup stops one bit-plane short and a `Z_blk = 3`
refinement segment carries bit-plane 0 wherever that stays lossless
(blocks with a SigProp-unreachable `mag = 1` sample fall back to the
full-depth cleanup). Codestream assembly signals the capability per
T.814 Annex A: `Rsiz` bit 14, a `CAP` marker segment (`Pcap15`;
HTONLY / SINGLEHT / RGNFREE / HOMOGENEOUS `Ccap15` with the measured
§8.7.3 MAGB bits and the §A.3.6 HTIRV flag when a 9-7 kernel is
involved), `SPcod` / `SPcoc` bit 6, and the T.814 §B.2 / §B.3
codeword-segment lengths (cleanup, then SigProp + MagRef) in every
packet header. Composes with RCT / ICT, both kernels, tiles,
precincts, all five progression orders, SOP / EPH framing, PPM / PPT
relocation, component sub-sampling, per-component COC / QCC overrides
**and the Annex H Maxshift ROI** (T.814 §A.5 — `Ccap15` bit 12 flags
the RGN, `SPrgn` stays ≤ 37 by the lane bound; the available opaque
HTJ2K decoders decline RGN so that shape is validated by this crate's
own §H.1-honouring decoder); the Annex-D-only styles and PCRD are
cleanly rejected in combination. **Quality layers compose as
MULTIHT**: with `layers > 1` each layer carries one §B.1 HT set per
code-block (each set one magnitude bit-plane finer; sets before the
last signal their unused refinement passes with a zero-length segment
per §B.3 NOTE 3), a block too shallow for the early layers emits §B.1
placeholder triples instead, and `Ccap15` bit 13 signals MULTIHT —
decoded bit-exactly by this crate's own §B.1 / §B.3 set grouping (the
opaque HTJ2K decoders are SINGLEHT-only). Validated bit-exact through
this crate's own decoder and **byte-identical through two independent
opaque HTJ2K decoders** (gray reversible at 0–3 decomposition levels,
the `Z_blk = 3` refinement shape, and the 9-7 irreversible path —
single-layer shapes; the decoders decline multi-layer HT).

### Not yet implemented

These surface a clean `Error::NotImplemented` rather than mis-decoding:

- A mixed-kernel tile that also signals a multiple-component transform
  (`Rmct = 1`) — the RCT / ICT requires one kernel across components
  0–2. (The `COC` overrides themselves — per-component `NL` /
  code-block size / precincts / kernel *and* the Table A.19 style
  byte, including the T.814 HTDECLARED HT / Annex D mix — *are*
  honoured, in both the main and tile-part headers.)
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
- HTJ2K MIXED-set codestreams (T.814 §8.2: `SPcod` bits 6 + 7 marking
  a tile-component whose code-blocks are *individually* either HT or
  T.800 Annex D blocks, distinguished only by trial decoding). The
  HTONLY sets — including MULTIHT and placeholder passes — *are*
  decoded (see above).

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
