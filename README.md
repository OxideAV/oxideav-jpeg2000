# oxideav-jpeg2000

[![CI](https://github.com/OxideAV/oxideav-jpeg2000/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-jpeg2000/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-jpeg2000.svg)](https://crates.io/crates/oxideav-jpeg2000) [![docs.rs](https://docs.rs/oxideav-jpeg2000/badge.svg)](https://docs.rs/oxideav-jpeg2000) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust JPEG 2000 for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) workspace: the
ITU-T T.800 | ISO/IEC 15444-1 Part-1 codestream **decoder and
encoder**, the T.814 | 15444-15 **HTJ2K** block coder (HTONLY,
HTDECLARED and MIXED sets), and the Annex I **JP2** / T.814 Annex D
**JPH** file formats. Written from scratch against the standards
documents under `docs/image/jpeg2000/` only.

The crate root follows the OxideAV
[image-crate API contract](../../IMAGE_CRATE_API.md): `decode` hands
back the picture in the layout its component set describes (packed
gray / RGB / RGBA at 8 or 16 bits, planar YCbCr when the chroma
components are sub-sampled, `Pal8` for a JP2 palette) with the JP2
header's colour and ICC profile, and `encode` takes the same shape.
The JPEG 2000 depth — per-component `i32` planes for any component
set, every `SIZ` / `COD` / `QCD` / tile-part marker, the full Annex I
box surface, raw-plane encoders with every coding-style knob — lives
under its own names alongside.

## Standalone use

```toml
oxideav-jpeg2000 = { version = "0.0", default-features = false }
```

```rust
let bytes = std::fs::read("in.jp2")?;             // a JP2 / JPH file or a bare .j2k codestream
if oxideav_jpeg2000::probe(&bytes) {
    let info = oxideav_jpeg2000::info(&bytes)?;    // header only: size, layout, depth, colour, tiles, layers
    let img = oxideav_jpeg2000::decode(&bytes)?;   // Jpeg2000Image, native layout
    let rgba: Vec<u8> = img.to_rgba8();            // tightly packed RGBA, 4 * width bytes per row
    let (w, h) = (img.width(), img.height());

    let opts = oxideav_jpeg2000::EncodeOptions::default()   // lossless 5-3, JP2 container
        .with_lossy(6)                                       // 9-7 kernel, Δb = 2^-6
        .with_layers(3)
        .with_target_psnr(42.0);
    std::fs::write("out.jp2", oxideav_jpeg2000::encode_rgba8(w, h, &rgba, &opts)?)?;
}
# Ok::<(), oxideav_jpeg2000::Error>(())
```

Root vocabulary: `probe`, `info -> ImageInfo`, `decode -> Jpeg2000Image`,
`decode_with(&DecodeOptions)`, `decode_rgb8 -> RgbImage`,
`decode_rgba8 -> RgbaImage`, `decode_from<R: Read>`,
`encode(&Jpeg2000Image, &EncodeOptions)`, `encode_rgb8`, `encode_rgba8`,
`encode_to<W: Write>`; types `Jpeg2000Image { width, height, format,
planes, color, metadata, palette, bit_depth }` (alias `J2kImage`),
`Plane`, `ColorInfo`, `ColorRange`, `Metadata`, `Palette`, `RgbImage`,
`RgbaImage`, `ImageInfo`, `PixelFormat` (= `Jpeg2000PixelFormat`,
alias `J2kPixelFormat`), `EncodeOptions` + `Container`, `DecodeOptions`,
`Jpeg2000Error` (= `Error`: `InvalidData`, `Unsupported`,
`LimitExceeded`, `Io`, plus the T.800 diagnostics). A JP2 file holds
one codestream, so there is no `decode_all`.

`Jpeg2000Image` is built with the fallible constructors `new` /
`packed` / `from_rgb8` / `from_rgba8` (geometry validated, so `to_rgb8`
/ `to_rgba8` never fail); `with_color` / `with_metadata` /
`with_palette` / `with_bit_depth` fill the rest in; `as_bytes()` is the
single plane of a packed layout (`None` for the planar YCbCr layouts),
`into_raw()` concatenates the planes.

### The native layout and the depth API

A codestream is a list of components (`SIZ`), each with its own
precision and reference-grid sub-sampling; a JP2 header may map them
through a palette and reorder them (`cmap` / `cdef`). `decode` derives
the contract layout from that list:

| Channels | Sub-sampling | Precision | `Jpeg2000Image::format` |
|---|---|---|---|
| 1 | any, uniform | 1–8 / 10 / 12 / 9–16 | `Gray8` / `Gray10Le` / `Gray12Le` / `Gray16Le` |
| 2 | uniform | 1–8 / 9–16 | `Ya8` / `Ya16Le` (second channel = alpha) |
| 3 | uniform, RGB signalling | 1–8 / 9–16 | `Rgb24` / `Rgb48Le` |
| 4 | uniform, RGB signalling | 1–8 / 9–16 | `Rgba` / `Rgba64Le` |
| 3 / 4 | uniform, sYCC or parameterized YCC `colr` | 8 / 10 / 12 / 16 | `Yuv444P` / `Yuva444P` families |
| 3 / 4 | chroma 2×2 (alpha full) | 8 / 10 / 12 / 16 | `Yuv420P` / `Yuva420P` families |
| 3 / 4 | chroma 2×1 | 8 / 10 / 12 / 16 | `Yuv422P` / `Yuva422P` families |
| 3 | chroma 1×2 | 8 / 10 / 12 / 16 | `Yuv440P` family |
| 3 | chroma 4×1 | 8 | `Yuv411P` |
| 1 index + JP2 `pclr` (≤ 256 entries, 1 / 3 / 4 unsigned ≤ 8-bit columns) | — | 1–8 | `Pal8` + `palette` |

"Uniform" sub-sampling means every component shares one `(XRsiz,
YRsiz)`; the picture is then the component grid (a 3× sub-sampled
24×24 codestream is an 8×8 image). Samples are stored LSB-aligned at
their exact value: `bit_depth` carries the `Ssiz` precision (a 4-bit
file rides `Gray8` with `bit_depth = 4`, a 14-bit one `Gray16Le` with
`14`; `Gray10Le` / `Gray12Le` and the 10 / 12-bit YUV labels are used
when the precision matches exactly). Every other component set —
signed samples, more than 16 bits, mixed precisions, five or more
components, chroma geometry outside the table — is
`Error::Unsupported` from `info` and `decode`, naming the set; the depth
API decodes them all: `decode_j2k` / `jp2::decode_jp2` /
`decode_j2k_reduced` / `decode_j2k_layers -> DecodedImage` (one `i32`
plane per component or JP2 channel), `parse_j2k_header` /
`parse_codestream` (typed markers), `jp2::parse_jp2` (every Annex I
box), and on the encode side `encode::encode_j2k` / `encode_j2k_u16` /
`encode_jp2` / `encode_jp2_with` / `jp2::write_jp2` (raw planes with any
`SIZ` sub-sampling, `COC` / `QCC` overrides, ROI, `POC`, tile-part
splits, relocated headers).

The pre-contract entry points `decode_jpeg2000` (interleaved bytes,
dimensions dropped), `encode_jpeg2000` and `looks_like_jp2`, and the
option type name `EncodeParams`, remain for one release as deprecated
wrappers; see the CHANGELOG for the mapping.

## Framework use

```toml
oxideav-jpeg2000 = "0.0"    # default `registry` feature: pulls oxideav-core
```

`oxideav_jpeg2000::register(&mut RuntimeContext)` installs the
`jpeg2000` codec (decoder + encoder, `jpeg2000_sw`) and two containers:
`jpeg2000` (the bare codestream, `.j2k` / `.j2c`) and `jp2` (the JP2 /
JPH file, `.jp2` / `.jph`), each with a probe, a demuxer and a muxer;
`register_codecs` / `register_containers` take the individual
registries, `make_decoder` / `make_encoder` are the factories.
`oxideav_meta::register_all` calls `register` for you, so
`oxideav_image::open(&ctx, "photo.jp2")` resolves through the registry.

The containers (`oxideav_jpeg2000::container`) are single-image: the
demuxer declares one video stream — `width` / `height`, the native
layout of the table below as `pixel_format`, the JP2 palette as RGB
triples in `extradata` for `Pal8`, and `color_signal` only when the
JP2 header carries a `colr` box (a bare codestream signals no colour) —
and emits the whole file as one keyframe packet (time base 1/1, `pts`
0). A component set with no contract layout (signed, mixed depths, five
or more components) still opens with `pixel_format = None`; the decoder
then reports `Unsupported`. `metadata()` carries `("icc", "present")`
when a `colr` box holds an ICC profile (the blob itself has no
framework carriage yet). A multi-codestream (`jpx`) file yields its
first `jp2c`. The muxers take the registry encoder's packets and write
what their name says: `jpeg2000` strips a JP2 wrapper to the codestream
(refusing `Pal8`, whose palette would be lost), `jp2` wraps a bare
codestream in a JP2 / JPH header built from the stream's layout,
palette and colour signal (the layout's conventional colourspace when
the stream signals none). One picture per file.

The framework `Decoder` and `Encoder` are thin adapters over
`decode_with` / `encode` (one implementation). Each packet is one
complete file or codestream (the framing is sniffed); the decoder
emits the native layout above as a `VideoFrame` — one packed plane, or
one plane per component for the YCbCr layouts — with the palette
side-channel for `Pal8`, the colour signal when the JP2 header carried
one, and the per-plane significant-bits side-channel when `bit_depth`
differs from the label's depth. Decoder options: `reduce`, `layers`,
`strict`; the framework `DecoderLimits` tighten the standalone limits.
The encoder accepts every `Jpeg2000PixelFormat` (plus `Bgr24` / `Bgra`,
re-ordered) and the `CodecOptions` keys `lossless`, `fine_bits`,
`psnr`, `target_bytes` (or `bit_rate` / `frame_rate`), `levels`,
`layers`, `progression`, `tile`, `ht`, `plt`, `tlm`, `sop`, `eph`,
`comment`, `container` (default `j2k` — the framework carries colour on
the frame; `jp2` / `jph` wrap the file). `From<Jpeg2000Image> for
VideoFrame`, `Jpeg2000Image::from_video_frame(&VideoFrame,
&CodecParameters)` and `TryFrom<(&VideoFrame, &CodecParameters)>` bridge
the two worlds.

## Supported layouts

Decode — every row of the table above; `Pal8` only from a JP2 file.
Deep layouts are little-endian 16-bit words; `to_rgb8` / `to_rgba8`
scale by `round(v × 255 / (2^bit_depth − 1))`, expand the palette,
replicate gray, and convert YCbCr with the signalled H.273 matrix
(BT.709 = 1, BT.2020 = 9, otherwise BT.601 — the sYCC coefficients) at
the signalled range (limited only when a parameterized `colr` says so),
chroma replicated nearest-sample.

Encode — the same table, in both containers:

| `format` | Codestream | JP2 header |
|---|---|---|
| `Gray8` / `Gray10Le` / `Gray12Le` / `Gray16Le` | 1 component at `bit_depth` | greyscale `colr` |
| `Ya8` / `Ya16Le` | 2 components | greyscale `colr` + opacity `cdef` |
| `Rgb24` / `Rgb48Le` | 3 components, RCT / ICT on by default | sRGB `colr` |
| `Rgba` / `Rgba64Le` | 4 components, RCT / ICT across the first three | sRGB `colr` + opacity `cdef` |
| `Yuv*` / `Yuva*` | components 1–2 at the layout's `XRsiz` / `YRsiz`, no MCT (T.800 J.14.1) | sYCC `colr` (+ opacity `cdef`) |
| `Pal8` | the index component at `bit_depth` | `pclr` + `cmap` (+ opacity `cdef`); `Container::J2k` is `Unsupported` |

`encode_rgb8` / `encode_rgba8` write `Rgb24` / `Rgba`. `encode` never
converts a layout: what you pass is what the `SIZ` describes. Lossless
(`EncodeKernel::Lossless5x3`, the default) round-trips `decode(encode(img))
== img` for every layout above, pinned by `tests/contract_api.rs`
(strides come back tight; the JP2 header stamps the conventional
colourspace on an unsignalled image).

## Options

`DecodeOptions { max_width, max_height, max_pixels, max_bytes, strict,
reduce, layers }` — limits are checked against the `SIZ` geometry
(after `reduce`) before any sample plane is allocated; `max_bytes`
bounds the decoder's `i32` working set (4 bytes per sample over every
component). Defaults: 65 535 × 65 535, no pixel cap, 1 GiB, lenient,
full resolution, all layers; `None` lifts a limit (`unlimited()` lifts
all). `strict` rejects trailing bytes after `EOC` and a JP2 `ihdr` that
disagrees with `SIZ`. `reduce = n` discards the `n` highest resolution
levels (dimensions `ceil(full / 2^n)`); `layers = Some(n)` decodes the
first `n` quality layers.

`EncodeOptions` — `container` (`Jp2` default / `J2k`), `kernel`
(`Lossless5x3` / `Lossy9x7 { fine_bits }` via `with_lossless` /
`with_lossy`), `mct` (`None` = on for the RGB layouts, off otherwise),
`decomposition_levels` (3), `code_block_exp` ((6, 6)), `progression`
(LRCP), `precincts`, `layers` (1), `target_bytes` / `target_psnr` (PCRD
rate control), `tile_size`, the Table A.19 style flags (`bypass`,
`terminate_all`, `reset_probabilities`, `vertically_causal`,
`predictable_termination`, `segmentation_symbols`), `sop` / `eph`,
`plt` / `tlm`, `comment`, `high_throughput` / `ht_refinement` /
`ht_mixed` (T.814), `tile_parts`, `poc`, `packed_headers`, `roi`,
`sub_sampling` and `component_overrides` (the last two for the
plane-level encoders; `encode` derives sub-sampling from the layout).

## Metadata and colour

`ColorInfo { range, primaries, transfer, matrix }` comes from the JP2
Colour Specification box: enumerated sRGB (16) → BT.709 primaries,
sRGB transfer, identity matrix, full range; greyscale (17) → the same
code points on one channel; sYCC (18) → BT.709 / sRGB / BT.601 matrix,
full range; a T.814 parameterized box (`METH = 5`) is copied verbatim;
an ICC-only box leaves the code points unspecified and puts the profile
on `Metadata.icc`. A bare codestream carries no colour information
(`ColorInfo::unspecified()`), so chroma-sub-sampled raw codestreams
decode as YCbCr with unspecified colour and `to_rgb8` applies the sYCC
(BT.601, full-range) convention. On encode the JP2 header carries the
image's `ColorInfo` as the matching enumerated colourspace, the ICC
profile as a restricted-ICC `colr`, any other description verbatim as a
parameterized box in a JPH (HT) file, or the conventional colourspace
flagged `UnkC` in a plain JP2. `Metadata.exif` / `xmp` / `gamma` are
always `None`: Exif and XMP ride vendor `uuid` boxes whose identifiers
the staged specifications do not define.

## Limits

Hostile input never panics (fuzz targets `contract`, `decode_j2k`,
`decode_jp2`, `decode_variants`, `parse_*`, `mq_decoder`,
`ht_block_decode`, `roundtrip_encode`). `probe` is total and
allocation-free; `info` reads the main header and JP2 boxes only; the
decode limits fire before the first plane is allocated. Geometry that
overflows `usize` is `Unsupported`; a configured limit is
`LimitExceeded`.

## Format specifics

### Capability

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

The committed conformance corpus follows the ISO/IEC 15444-4 C.1
abstract-test-suite axes with real black-box-encoder fixtures:
non-zero **image and tile origin offsets** (XOsiz/YOsiz + XTOsiz/YTOsiz
reference-grid anchoring, 5-3 and 9-7), **tile-parts split by layer**
(TPsot > 0 chains), **tile-parts interleaved across tiles** (the
§A.4.2 round-robin layout, with the TPsot ordering / TNsot count
rules enforced against lost or mis-assembled chains), **PLT** and
**TLM** pointer markers (both actively **cross-validated** against the
walked packets / tile-parts — a corrupted pointer is rejected, not
skipped), **MCT-off**
RGB, **signed 8- and 12-bit** and **unsigned 16-bit** depths,
all-component **reference-grid sub-sampling** (XRsiz = 2 planes pinned
against §B.2.6 PGX reference decodes), the **JP2 container** from a
real encoder, all six Table A.19 code-block style combinations, and
the ISO/IEC 15444-4 **Ed. 4** electronic insert's three **HTJ2K
MIXED-set** codestreams (the only MIXED streams in that corpus,
carried with the corpus notice) — alongside the progression-order,
precinct, quality-layer, bypass / termination, ROI, POC, PPM / PPT
and HT fixtures listed throughout this README.

What is implemented:

- **Containers** — J2K raw codestream and the JP2 ISO BMFF box wrapper
  (`jP`, `ftyp`, `jp2h` / `ihdr` / `bpcc` / `colr`, `jp2c`), with all
  three box length encodings; plus the **JPH** (HTJ2K, T.814 Annex D)
  profile of the same layout — the `'jph '` brand, the §D.2
  no-`colr`-under-`UnkC` exemption, and the §D.4 `METH` values 3 (any
  ICC profile) and 5 (H.273 parameterized colourspace). The full
  Annex I `jp2h` box surface parses: the **`pclr` Palette** box
  (§I.5.3.4 — any 1–38-bit signed / unsigned column layout, padded
  non-multiple-of-8 storage), the **`cmap` Component Mapping** box
  (§I.5.3.5, direct + palette mappings, with the pclr ⟺ cmap pairing
  and index-range rules enforced), the **`cdef` Channel Definition**
  box (§I.5.3.6 — colour / opacity / premultiplied types, colour
  associations, the duplicate-(Typ, Asoc) rule), and the **`res `
  Resolution** superbox (§I.5.3.7, `resc` / `resd` grid resolutions).
  `jp2::decode_jp2` decodes a JP2 / JPH **file** end-to-end and
  applies the channel semantics — palette expansion and `cdef`
  colour-ordering — **byte-exact against black-box reference decodes**
  of committed palettized and BGR + `cdef` fixtures; the contract
  `decode` / `info` / `probe` and the registry decoder sniff the
  12-byte JP2 signature and route files through the same path.
- **Main header** — `SOC`, `SIZ`, `COD`, `QCD`, plus the typed
  tile-part-header markers (`COD`, `COC`, `QCD`, `QCC`, `RGN`, `POC`,
  `PLT`, `PPT`, `COM`); 8- vs 16-bit component-index width is selected
  from `Csiz`. The informational `CPF` (T.814 §A.6 corresponding
  profile) and `CRG` (§A.9.1 component registration) segments are
  accepted and length-skipped — neither affects decoding.
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
  both component orders. The lanes also mix at **tile** granularity
  (the §8.5 HETEROGENEOUS shape): a multi-tile grid whose main
  header signals one lane while selected tiles restate the other
  through their first-tile-part `COD` override decodes bit-exact in
  both orientations, byte-identical through an independent black-box
  decoder.
- **Progression** — all five §B.12.1 orders (LRCP, RLCP, RPCL, PCRL,
  CPRL) and the §A.6.6 **progression order change** (`POC`) wired into
  the decode driver: a main-header or first-tile-part `POC` drives the
  §B.12.2 volume enumeration (each volume's component / resolution /
  layer sub-range in its own order, with the per-(component, resolution,
  precinct) "next unsent layer" cursor), under the §A.6.6 precedence
  `Tile-part POC > Main POC > Tile-part COD > Main COD`. Plus
  **multi-layer** / **multi-precinct** reassembly. The position-keyed
  orders project each precinct to its reference-grid corner for any
  integer `XRsiz` / `YRsiz` — a **partial first precinct** (a tile or
  image-origin edge off the precinct lattice) keys on the tile edge
  `tx0` / `ty0` itself, exactly where the §B.12.1.3–5 OR-clause fires
  (a 400-case black-box sweep across all five orders × tiling ×
  precinct / code-block shapes × layers × offsets × both kernels
  decodes 5-3 byte-exact and 9-7 within the pinned reference
  latitude); the power-of-two requirement is enforced
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
  whose high-pass bands each carry several 32×32 HT code-blocks).
  Beyond the block decoder, **whole-codestream HT depth** is pinned
  against real black-box HT codestreams: a 46-case sweep across both
  kernels, all five §B.12.1 progression orders, precinct / code-block
  shapes, multi-tile grids (including ragged edges), **non-zero SIZ
  image and tile offsets** (XOsiz / YOsiz + XTOsiz / YTOsiz
  reference-grid anchoring), **tile-part divisions** on the resolution
  and component axes (TPsot > 0 chains), main-header **TLM** pointer
  markers, and 12- / 16-bit depths decodes **byte-identical** on every
  reversible case (the irreversible cases sit within the ±1
  half-integer inter-decoder rounding latitude ISO/IEC 15444-4
  budgets); committed fixtures pin the multi-tile, offset-anchored,
  tile-part R / RC, TLM, PCRL-RGB, irreversible-tiled and 16-bit
  shapes bit-exact in CI, plus the round-416
  **precinct-unaligned-tile** shapes (15×13 tiles, custom precincts,
  image-origin offsets — an 80-case HT sweep across all five orders,
  byte-exact, with reduced-resolution decodes matching black-box
  r1 / r2 references on 240 cases across both block-coding lanes). The
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
  pinning.) The `CAP` marker's `Ccap15` bits 15-14 classify the
  stream per §A.3.2 — HTONLY / HTDECLARED / MIXED-permitted — and
  gate the style-byte reading: under an HTONLY `Ccap15` **every**
  code-block routes to the HT decoder whatever `SPcod` says (both the
  strict first-branch signalling with style bits `00` and the
  §A.3.2-NOTE `11` encoding), HTDECLARED enforces bit 7 = 0, and the
  reserved encodings reject. **MIXED-set codestreams (§8.2 / §A.4)
  decode**: a tile-component whose style byte carries bits 6 + 7
  under a MIXED-permitting CAP holds code-blocks that are
  *individually* HT or T.800 Annex D, with no per-block signalling —
  the packet reader runs both tier-2 hypotheses (the derived
  `K(T.800) = 1`, since §A.4 bars bypass and per-pass termination,
  against the §B.2 set-`T` partition), parses the one-field layouts
  that read identical bits under both without deciding, pins the
  T.800 lane where the §A.4 constraints (`Lblock > 3`, clear first
  length bit) or §B.3 refute HT on the shared bytes, and resolves the
  genuine set-`T` straddles by a depth-first HT-first hypothesis
  search re-walked on any downstream failure; blocks still unresolved
  at tier-1 are arbitrated exactly as the §A.4 NOTE prescribes (trial
  HT decode, Annex D fallback). The three `hm` MIXED codestreams of
  the ISO/IEC 15444-4 Ed. 4 electronic insert — the only MIXED
  streams in that corpus, committed as fixtures — decode end-to-end
  on the first assignment (0 / 7 / 24 divergent choices per tile, no
  backtracking); with every available opaque decoder declining the
  MIXED style byte, they are cross-validated through the corpus's own
  controlled redundancy: the two `p0_06` transcodes reconstruct their
  losslessly carried components **byte-identically across the two
  independent encodings**, component 3 lands at the ≈40 dB the
  bundle's statistics record, and the layer-progressive /
  reduced-resolution surfaces compose (monotone MSE over every layer
  prefix). The long-standing small-block / high-energy /
  non-power-of-two decode divergence is **resolved**: differential
  tracing against this crate's own independently written HT *encoder*
  isolated it to the §7.3.4 / §7.3.6 first-line-pair interleave — when
  `s_mel = 0` and `u_q1 > 2`, the second quad's single `u` bit replaces
  the *prefix step* and therefore precedes the first quad's suffix bits
  (decidable from the prefix alone per the §7.3.6 NOTE). With the fix a
  264-stream black-box sweep (odd and even dimensions to 100×80, 1–5
  decomposition levels, 4×4–64×64 code-blocks, full-range noise)
  decodes byte-identical.

### Encoder

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
- **All six Table A.19 coding styles on encode** — the §D.6
  **selective arithmetic-coding bypass** (bit 0: SP / MR passes from
  bit-plane 5 write raw bits through a §D.6 stuff-bit writer while
  cleanups stay MQ), **context reset on every pass boundary** (bit 1,
  the Table D.7 states), §D.4.2 **termination on each coding pass**
  (bit 2), §D.7 **vertically causal context formation** (bit 3),
  §D.4.2 **predictable termination** (bit 4 — the reproducible MQ
  termination procedure, and the §D.6 alternating 0/1 fill with the
  post-`0xFF` stuffed byte on raw segments), and the §D.5
  **segmentation symbol** (bit 5, the four UNIFORM-context bits of
  `0xA` closing every cleanup pass) — separately or composed, with
  the §B.10.7.2 multi-segment length sequences written by the
  generalised tier-2 writer. Mis-signalling probes pin that the
  decision-changing bits are really in the coded stream.
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
- **Structured parameters** (`EncodeOptions` +
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
  - **PCRD quality control** (`target_psnr`): the same Equation J-13
    threshold λ bisected the other way — each candidate is assembled
    exactly and decoded through this crate's own decoder, and the
    smallest stream whose *measured* PSNR (all components, input
    sample domain) still reaches the floor wins; a floor the
    quantiser cannot reach yields the full-rate stream, and rising
    floors cost monotonically more bytes.
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
    rectangle (`EncodeOptions::roi`) is traced backwards through the
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
  - **`PLT` / `TLM` pointer markers** (§A.7.3 / §A.7.1): `plt` puts
    packet-length lists in every tile-part header (Table A.36 `Iplt`
    — SOP + header + EPH + data in-stream, SOP + data under PPM / PPT
    relocation — chunked into `Zplt`-indexed segments on completed
    entries) and `tlm` a main-header `Ztlm` series announcing every
    tile-part's `Psot` (`ST = 1` up to 255 tiles, `ST = 2` beyond,
    32-bit `Ptlm`), laid out before emission. Both satisfy this
    decoder's §A.7 pointer cross-validation (which rejects any
    corrupted entry) across framings, relocations, splits, styles,
    PCRD, ROI and the HT lanes; an opaque-encoder SOP + EPH + PLT +
    TLM fixture pins the `Iplt`-spans-from-SOP convention.
  - **`COM` comment** (§A.9.2): a main-header `Rcom = 1` Latin text
    segment via `EncodeOptions::comment`.
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
- **JP2 / JPH container writer** (`jp2::write_jp2`, T.800 Annex I /
  T.814 Annex D): Signature + File Type (`'jp2 '`, or the `'jph '`
  brand with `'jp2 '` compatibility when `Rsiz` signals HTJ2K) + JP2
  Header — `ihdr` derived from the codestream's `SIZ` (`bpcc` when
  component depths diverge), enumerated / restricted-ICC `colr`
  boxes (plus the T.814 Table D.1 Any-ICC and parameterized methods,
  JPH-only), `pclr` + `cmap` palettes, `cdef` channel definitions,
  `res` capture / display grids — + the Contiguous Codestream box.
  The writer re-parses its own output, so a returned file always
  reads; `Jp2WriteOptions::for_components` supplies the conventional
  greyscale / sRGB (+ opacity `cdef`) header and `encode_jp2` /
  `encode_jp2_with` / `encode_jp2_u16` go straight from planes to a
  file. An opaque decoder reproduces every shape byte-exactly —
  including the `cdef` BGR reorder, the palette expansion, the alpha
  channels (emitted as RGBA / grey + alpha), and the JPH file
  through two independent HT decoders.
- The §G.2 / §G.3 **MCT covers any plane count ≥ 3** (components
  0–2 transform; further components — alpha — code untouched).
- The `oxideav-core` registry installs the **`Encoder` trait** impl
  alongside the decoder (`make_encoder`): one packed interleaved
  plane per frame in `Gray8` / `Rgb24` / `Bgr24` / `Rgba` / `Bgra`
  or the little-endian `Gray10Le` / `Gray12Le` / `Gray16Le` /
  `Rgb48Le` / `Rgba64Le` layouts, `bit_rate` (over `frame_rate`, or
  per frame) as a PCRD byte budget, and a `CodecOptions` bag
  (`lossless`, `fine_bits`, `psnr`, `target_bytes`, `levels`,
  `layers`, `progression`, `tile`, `ht`, `plt` / `tlm` / `sop` /
  `eph`, `comment`, `container = jp2`); the contract `encode_rgb8` /
  `encode_rgba8` / `encode` are the standalone doors to the same
  pipeline.

The crate also **encodes HTJ2K** (T.814): setting
`EncodeOptions::high_throughput` routes every code-block through the
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
state machines included). With `EncodeOptions::ht_refinement` each
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

The encoder also emits **MIXED-set codestreams**
(`EncodeOptions::ht_mixed`, T.814 §8.2 / §A.4): every code-block is
coded through both the Annex D MQ passes and the §7.3 HT cleanup pass,
and per block the HT lane is kept wherever it stays within a
throughput budget of one-eighth plus two bytes over the MQ codeword —
blocks the MQ coder compresses markedly better stay Annex D. The
stream signals `Rsiz` bit 14, a MIXED-permitting `Ccap15`
(bits 15-14 = `11`, measured MAGB), `SPcod` / `SPcoc` bits 6 + 7, the
§A.4 first-non-zero-segment headroom on the HT-lane blocks
(`Lblock > 3`, clear top length bit) and the derived single length
field on the T.800 lane. Round-trips bit-exactly through this crate's
own MIXED decoder across RCT, multi-tile grids, custom precincts, the
position-keyed progressions, sub-sampling and SOP / EPH framing; the
9-7 stream reconstructs identically to its pure-Annex-D counterpart
(the lane choice never touches a sample), and a re-signalling probe
pins that both lanes are genuinely present. Single quality layer; the
§D.6 / §D.4.2 styles, PCRD, ROI and the all-HT mode are rejected in
combination.

#### Not yet implemented

These surface a clean `Error::NotImplemented` rather than mis-decoding:

- **`PLM`** (§A.7.2, main-header packet lengths): not emitted — this
  crate's decoder treats a `PLM` as an opaque main-header segment
  rather than cross-validating it, so an emitted `PLM` would be
  unverifiable here; `PLT` + `TLM` carry the same information in
  validated form.
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
