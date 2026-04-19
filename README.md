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
segments, and each tile-part's byte range.

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

**Sample encoder** — the `encode` module emits baseline `.j2k`
codestreams for **5/3 integer reversible (lossless)** input. The
pipeline covers:

- **Forward 5/3 integer lifting** — 1-D / 2-D, multi-level pyramid
  with quadrant-packed output.
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
  marker chain.

Round-trip `encode_frame → send_packet → receive_frame` is **bit-exact
lossless** for 8-bit grayscale input on the 5/3 reversible transform.

## What does not work yet

- **9/7 irreversible encoder** — 9/7 decode is wired but the encoder
  writes 5/3 only. Use `opj_compress -I` if you need a 9/7 bitstream.
- **Bit-exact pixel reconstruction against OpenJPEG's decoder** is
  approximate for complex content — the shape is complete and passes
  coarse quality metrics on the 64×64 baseline / lossy fixtures, but
  heavily textured streams are not yet bit-identical to
  `opj_decompress` output. Round-trip *through our own decoder* is
  bit-exact on the 5/3 path.
- **Multi-tile codestreams** — single-tile only for now.
- **Multi-layer (progressive quality) streams** — single layer only.
- **User-defined precinct grids**, **CPRL / PCRL / RPCL progression
  orders**, **PPT / PPM packed headers**, **region-of-interest
  (RGN)**, the **HT block coder** (Part 15).
- **Encoder pixel formats beyond `Gray8`** — RGB / YUV support is
  blocked on a forward RCT / ICT implementation and the corresponding
  SIZ + COD emission changes.
- **The JP2 ISOBMFF box wrapper** (`.jp2` with the
  `00 00 00 0C 6A 50 20 20 0D 0A 87 0A` signature box + JP2 Colour
  Specification / Metadata boxes). Feed the inner `.j2k` codestream
  directly to the parser.
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
