# oxideav-jpeg2000

Pure-Rust **JPEG 2000** (ISO/IEC 15444-1) codec crate. Zero C
dependencies.

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace)
framework but usable standalone.

## What works

**Codestream parser** — the `codestream` module walks the Part-1 J2K
marker chain (SOC, SIZ, COD, QCD, COC, QCC, RGN, POC, PPM, PPT, PLM,
PLT, TLM, CRG, COM, SOT, SOD, EOC) and returns image geometry,
per-component bit depth / signedness / sub-sampling, raw COD and QCD
segments, parsed RGN (Region of Interest) segments at both main-header
and tile-part-header scope, and each tile-part's byte range.

**Sample decoder** — the `decode` module reconstructs pixels for
baseline `.j2k` codestreams. The pipeline covers:

- **MQ arithmetic decoder** — full 47-state probability table,
  `BYTEIN` / `RENORMD` / `DECODE` primitives, raw bypass mode. Ported
  from OpenJPEG `mqc.c` (BSD-2-Clause).
- **EBCOT tier-1** — significance propagation, magnitude refinement,
  and cleanup passes, driven from a clean spec-based flag
  representation. ZC / SC / MAG / RUN / UNIFORM contexts, per-band
  orientation tables (LL/HL/LH/HH).
- **Tier-2 packet headers** — tag-tree driven inclusion and
  zero-bitplane counts, comma-coded pass count, adaptive Lblock for
  segment-length encoding. LRCP and RLCP progression orders. Single
  quality layer. Default precinct (one precinct per resolution).
- **Inverse DWT** — 5/3 integer reversible lifting (Part-1 lossless)
  and **9/7 irreversible float lifting end-to-end** (wired through the
  top-level driver with `Rb = precision` stepsizes and OpenJPEG's
  `K` / `2/K` scaling convention).
- **DC level-shift + clipping** back to the component's declared
  precision.
- **Reversible component transform (RCT)** for 3-channel 5/3 streams
  and **irreversible colour transform (ICT)** for 3-channel 9/7
  streams with `MCT = 1` in the COD.
- **Region of Interest (RGN) Maxshift method** (T.800 §A.6.3 +
  Annex H). The parsed RGN segments thread a per-component shift `s`
  into `DecodeParams::roi_shifts`; the tier-1 / synthesis path
  bumps `band_numbps` by `s` on every codeblock whose
  `missing_msb < s` (i.e. those that exercise the extra ROI bit-
  planes) and divides the reconstructed magnitude by `2^s` to undo
  the encode-side upshift. Bit-exact lossless against
  `opj_compress -ROI c=<i>,U=<s>` fixtures: 8-bit Gray (U=4, U=8),
  RGB+RCT with U=4 on luma, and within ≤ 4 LSB against the
  `opj_decompress` reference for the 9/7 irreversible RGN path.

**Sample encoder** — the `encode` module emits `.j2k` codestreams
(or `.jp2` containers) for both the **5/3 integer reversible
(lossless)** and the **9/7 irreversible (lossy)** transforms. The
pipeline covers:

- **Forward 5/3 integer lifting** — 1-D / 2-D, multi-level pyramid
  with quadrant-packed output.
- **Forward 9/7 float lifting** — the same multi-level driver with
  float samples; scales the output using OpenJPEG's `BUG_WEIRD_TWO_INVK`
  convention (evens ÷ `K`, odds ÷ `2/K`) so the matching inverse
  recovers the input.
- **Per-band scalar quantiser** (§E.1.1) for the 9/7 path. Emits the
  QCD in "expounded" form (qntsty = 2) with `mu_b = 0` and
  `eps_b = precision`, yielding `stepsize_b = 1` on every sub-band.
- **Forward component transforms** — RCT (§G.1) for `Rgb24` +
  reversible, ICT (§G.2) for `Rgb24` + irreversible. The COD's
  `MCT` flag is set to 1 when applied.
- **MQ arithmetic encoder** — 47-state table, `CODEMPS` / `CODELPS` /
  `RENORME` / `BYTEOUT` / `FLUSH` primitives.
- **EBCOT tier-1 encoder** — mirror of the decoder's sigprop / magref
  / cleanup passes, including full 4-row-stripe run-length AGG coding
  in the cleanup pass.
- **Tier-2 packet construction** — inclusion + zero-bitplane tag
  trees (with threshold-sweep emission), comma-coded pass count,
  adaptive Lblock growth, bit-packed MSB-first headers with 0xFF
  stuff-bit.
- **Codestream writer** — SOC / SIZ / COD / QCD / SOT / SOD / EOC
  marker chain. Transform byte in COD reports 5/3 (1) or 9/7 (0).
- **JP2 ISOBMFF wrapper** — optional `signature` + `ftyp` + `jp2h`
  (with `ihdr` + `colr`) + `jp2c` boxes per ISO/IEC 15444-1 Annex I.
  Enabled via `EncodeOptions::jp2_wrapper`. The decoder auto-detects
  the wrapper on input, so `.jp2` buffers decode through the same
  `reg.make_decoder(...)` API.

Round-trip `encode_frame → send_packet → receive_frame` is **bit-exact
lossless** for 8-bit grayscale (and for RGB whose RCT chroma stays in
the 8-bit signed range) on the 5/3 reversible transform. The 9/7
irreversible path produces a lossy bitstream with round-trip PSNR
> 43 dB on the 64×64 gray / RGB gradient fixtures.

## What does not work yet

- **Bit-exact pixel reconstruction against OpenJPEG's decoder** is
  approximate for complex content — the shape is complete and passes
  coarse quality metrics on the 64×64 baseline / lossy fixtures, but
  heavily textured streams are not yet bit-identical to
  `opj_decompress` output. Round-trip *through our own decoder* is
  bit-exact on the 5/3 path.
- **Multi-tile codestreams** — single-tile only for now.
- **Multi-layer (progressive quality) streams** — single layer only.
- **User-defined precinct grids**, **CPRL / PCRL / RPCL progression
  orders**, **PPT / PPM packed headers**.
- **Region of Interest (RGN) on the encoder side** — the decoder
  honours `RGN` segments produced by other encoders, but our own
  encoder does not yet emit `RGN` markers.
- The **HT block coder** (Part 15, ISO/IEC 15444-15 / ITU-T T.814) is
  decoder-side functional behind the `htj2k` Cargo feature for
  single-tile single-layer LRCP codestreams. CAP marker parsing, the
  FBCOT cleanup pass (both Annex C CxtVLC tables), the SigProp /
  MagRef refinement passes, the per-codeblock bit-plane shift
  `pblk = M_b − S_blk − 1` (T.800 Eq E-1) on both the 5/3 integer
  and 9/7 float reconstruction paths, and the LRCP tier-2 walker are
  all in place. The 32×32 OpenJPH 5/3 reversible fixture is bit-exact
  against the OpenJPH-binary reference; the 32×32 9/7 lossy fixture
  decodes within MAD ≤ 8 LSB; the 64×64 5-decomposition-level 9/7
  multi-band fixture (every HF band populated) decodes at MAD ≈ 0.47
  with max-deviation 2 against the OpenJPH-binary reference. The
  pblk > 0 / pblk < 0 / `z = 1` algebraic cases on the 9/7 float
  path are unit-tested against the closed-form Eq E-1. **Decoder
  multi-tile + PPM/PPT (round 4):** the HT decoder now dispatches
  multi-tile-part codestreams (Isot grouping per T.800 §B.3) and
  splits PPM (main-header packed) / PPT (per-tile-part packed)
  packet headers from the body cursor.
  **Encoder side (round 4):** `encode::htj2k::encode_image_htj2k`
  produces a Part-15 codestream for `Gray8`, `Rgb24`, `Yuv444P`,
  `Yuv422P`, and `Yuv420P` 8-bit input with both 5/3 lossless
  (`HtTransform::Reversible53`) and 9/7 irreversible
  (`HtTransform::Irreversible97`) wavelets, `NL ∈ [0, 5]`
  decomposition levels. Multi-tile output via the new
  `EncodeOptionsHt::tile_size` knob; per-tile DWT + tier-1 + tier-2
  are independent. PPM (main-header) and PPT (per-tile-part) packed
  packet header layouts are wired via the existing classic encoder
  splitter, selectable through `EncodeOptionsHt::packet_header_placement`.
  Multi-component tier-2 packets (LRCP, one per `(resolution,
  component)`), optional forward 5/3 reversible component transform
  (RCT, T.800 §G.1) for `Rgb24` input — signalled in COD by `MCT = 1`
  — and multi-significance per quad (ρ ∈ {3, 5, 6, 7, 9..15}) plus
  the §7.3.6 Eq-4 first-line-pair both-`u_off=1` special case carry
  over from rounds 2-3. The 9/7 path uses the existing
  `encode::dwt::fdwt_97` lifting plus a per-band scalar quantiser
  with `eps_b = precision`, `mu = 0` so `stepsize_b = 1` on every
  sub-band (QCD emitted in expounded form, qntsty = 2).
  Self-roundtrip is bit-exact for 5/3 across all fixtures (single-
  tile, multi-tile, sub-sampled chroma, PPM/PPT) and within ±2 LSB
  for 9/7 on the 64×64 gradient; `ojph_expand` (binary, black-box)
  cross-decodes our 9/7 32×32 single-tile fixture within ±2 LSB,
  matching all round-1..round-3 single-tile cross-decode results.
  Compression vs raw: 64×64 8-bit Gray gradient → 4516 bytes at NL=0,
  **596 bytes at NL=2, 444 bytes at NL=3** (~90% reduction).
  **Encoder side (round 6):** SigProp + MagRef refinement passes
  (`Z_blk ∈ {2, 3}`) wired into the codestream via the new
  `EncodeOptionsHt::pass_count: HtPassCount` selector. Per-codeblock
  the encoder runs cleanup → `encode_sigprop` → `encode_magref` to
  produce `Dcup` + `Dref`; the packet header writes `num_passes` plus
  two length fields per ISO/IEC 15444-15 §B.3. `ojph_expand` cross-
  decodes our `Z_blk = 2` and `Z_blk = 3` codestreams bit-exactly on
  the 32×32 sparse fixture. Encoder gaps: rate-distortion truncation
  of refinement bits (currently emitted as zeros), multi-layer, multi-
  set HT (T.814 §B), constrained sets (T.814 §8), POC progression
  schedule.
- **RGB input beyond 8-bit unsigned chroma** — the decoder's RCT
  inverse currently clamps chroma to unsigned 8-bit before the
  inverse transform, so encoder inputs with chroma excursions outside
  [-128, 127] can overflow. 9-bit signed RCT chroma (the OpenJPEG
  convention) is not yet implemented.
- **Part-2 extensions** (multi-component transform, arbitrary wavelet
  kernels, etc.).

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-codec = "0.1"
oxideav-jpeg2000 = "0.0"
```

## Usage

### Probe a codestream for geometry

```rust
use oxideav_jpeg2000::codestream;

let bytes = std::fs::read("image.j2k")?;
let cs = codestream::parse(&bytes)?;
println!("{}x{}, {} component(s)",
    cs.siz.image_width(),
    cs.siz.image_height(),
    cs.siz.num_components());
for (i, comp) in cs.siz.components.iter().enumerate() {
    println!("  component {i}: {}-bit{} ({}x{} subsampling)",
        comp.bit_depth(),
        if comp.is_signed() { " signed" } else { "" },
        comp.xrsiz, comp.yrsiz);
}
for tp in &cs.tile_parts {
    println!("  tile {} part {}/{}: SOD @ {}, {} bytes",
        tp.tile_index, tp.tile_part_index, tp.tile_part_count,
        tp.sod_offset, tp.sod_length);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Decode to pixels

```rust
use oxideav_codec::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

let mut reg = CodecRegistry::new();
oxideav_jpeg2000::register(&mut reg);

let params = CodecParameters::video(CodecId::new("jpeg2000"));
let mut dec = reg.make_decoder(&params)?;
let pkt = Packet::new(0u32, TimeBase::new(1, 1), std::fs::read("image.j2k")?);
dec.send_packet(&pkt)?;
let frame = dec.receive_frame()?;
if let Frame::Video(v) = frame {
    println!("decoded {}x{}, format {:?}", v.width, v.height, v.format);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Encode a 5/3 lossless J2K

```rust
use oxideav_codec::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, TimeBase, VideoFrame, VideoPlane};

let vf = VideoFrame {
    format: oxideav_core::PixelFormat::Gray8,
    width: 64,
    height: 64,
    pts: None,
    time_base: TimeBase::new(1, 1),
    planes: vec![VideoPlane { stride: 64, data: vec![128u8; 64 * 64] }],
};

let mut reg = CodecRegistry::new();
oxideav_jpeg2000::register(&mut reg);
let params = CodecParameters::video(CodecId::new("jpeg2000"));
let mut enc = reg.make_encoder(&params)?;
enc.send_frame(&Frame::Video(vf))?;
let pkt = enc.receive_packet()?;
println!("emitted {} bytes of J2K", pkt.data.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Encode a 9/7 irreversible .jp2

```rust,no_run
use oxideav_core::{Frame, PixelFormat, TimeBase, VideoFrame, VideoPlane};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions, TransformMode};

// RGB input — 3 × 8-bit packed.
let vf = VideoFrame {
    format: PixelFormat::Rgb24,
    width: 64,
    height: 64,
    pts: None,
    time_base: TimeBase::new(1, 1),
    planes: vec![VideoPlane { stride: 64 * 3, data: vec![128u8; 64 * 64 * 3] }],
};

let opts = EncodeOptions {
    transform: TransformMode::Irreversible97,  // 9/7 float wavelet
    jp2_wrapper: true,                         // emit .jp2 boxes
    use_color_transform: true,                 // apply forward ICT
    ..Default::default()
};
let bytes = encode_frame(&Frame::Video(vf), &opts).expect("encode");
std::fs::write("image.jp2", &bytes)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Codec id

- Codec: `"jpeg2000"`.

## Generating fixtures

```bash
# Baseline 5/3 integer reversible (the default)
opj_compress -i input.ppm -o input.j2k

# 9/7 irreversible (lossy)
opj_compress -I -i input.pgm -o input.j2k -r 50

# Or via ffmpeg's libopenjpeg wrapper
ffmpeg -f lavfi -i "testsrc=size=64x64:rate=24:duration=0.05" \
    -pix_fmt yuv420p input.j2k
```

## License

MIT — see [LICENSE](LICENSE).
