# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K codestream + JP2 file format) decoder for the
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
  irreversible, and 2×2-tile bypass paths.
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
  parameters. The Table A.19 code-block **style** byte is held global
  to the code; a `COC` that diverges from the `COD` style — or gives
  different components different kernels — is cleanly rejected.
- **Progression** — all five §B.12.1 orders (LRCP, RLCP, RPCL, PCRL,
  CPRL), §B.12.2 POC volume iteration, and **multi-layer** /
  **multi-precinct** reassembly.
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

### Not yet implemented

These surface a clean `Error::NotImplemented` rather than mis-decoding:

- A `COC` whose Table A.19 code-block **style** byte diverges from the
  `COD`, or that gives different components different wavelet kernels
  (the common `COC` override of per-component `NL` / code-block size /
  precincts / kernel *is* honoured), in both the main and tile-part
  headers (main-header *and* tile-part `COD` / `COC` / `QCD` / `QCC`
  overrides are otherwise honoured).
- A non-Maxshift `RGN` style. T.800 Table A.25 (Part 1) defines **only**
  `Srgn = 0` (implicit ROI / Maxshift) — all other values are reserved
  in Part 1, and the main-header *and* tile-part Maxshift `RGN` *are*
  honoured. The "scaling based" arbitrary-shaped ROI (`Srgn = 1`
  rectangle / `Srgn = 2` ellipse) is an **ISO/IEC 15444-2 (Part 2)**
  extension (extended RGN marker + `Rsiz` capability + the Annex L
  wavelet-domain ROI-mask generation and mask-driven L.1 de-scaling),
  outside this Part-1 decoder's scope; an `Srgn ≠ 0` (or a Part-2
  extended-length) `RGN` surfaces a clean error rather than mis-decoding.
- `POC` order changes mid-decode (main-header or tile-part), and `PPM` /
  `PPT` packed-header markers.
- Position-keyed orders under non-power-of-two sub-sampling.
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

The crate also registers a software decoder through the standard
`oxideav-core` registry path.

## Clean-room provenance

Every module was written from the T.800 / ISO-IEC 15444-1 standards
documents under `docs/image/jpeg2000/` only — the codestream and JP2
syntax (Annex A + Annex I), tier-2 packet headers (§B.10), tile /
sub-band / precinct / code-block geometry (§B.2 – §B.9), the MQ
arithmetic decoder (Annex C), coefficient bit modelling (Annex D), and
progression orders (§B.12). PDF figures are transcribed to integer
operations from the accompanying prose. No external JPEG 2000
implementation is read or wrapped.

## License

MIT — see [LICENSE](LICENSE).
