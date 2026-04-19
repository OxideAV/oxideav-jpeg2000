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
- **Inverse DWT** — 5/3 integer reversible lifting (Part-1 lossless
  default) and 9/7 irreversible float lifting (compiled in, tested at
  the 1-D unit level). Symmetric whole-sample extension at
  sub-band boundaries.
- **DC level-shift + clipping** back to the component's declared
  precision.
- **Reversible component transform (RCT)** for 3-channel streams with
  `MCT = 1` in the COD.

## What does not work yet

- **9/7 irreversible wavelet end-to-end** is implemented as 1-D /
  2-D functions but not plumbed through the top-level driver (which
  fails fast on `COD.transform = 0`). Use `opj_compress -I` to force
  5/3 for now.
- **Bit-exact pixel reconstruction** is approximate for complex
  content — the decoder shape is complete and passes coarse quality
  metrics (luma mean in `[32, 224]`, distinct luma samples > 20 on the
  baseline 64×64 fixture), but constant or near-constant inputs are
  lossless while textured inputs are not yet bit-identical to
  `opj_decompress` output. Expect ongoing refinements.
- **Multi-tile codestreams** — single-tile only for now.
- **Multi-layer (progressive quality) streams** — single layer only.
- **User-defined precinct grids**, **CPRL / PCRL / RPCL progression
  orders**, **PPT / PPM packed headers**, **region-of-interest
  (RGN)**, the **HT block coder** (Part 15).
- **The JP2 ISOBMFF box wrapper** (`.jp2` with the
  `00 00 00 0C 6A 50 20 20 0D 0A 87 0A` signature box + JP2 Colour
  Specification / Metadata boxes). Feed the inner `.j2k` codestream
  directly to the parser.
- **Part-2 extensions** (multi-component transform, arbitrary wavelet
  kernels, etc.).
- **Encoder** — `make_encoder` still returns
  `Error::Unsupported`.

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
let pkt = Packet::new(0, TimeBase::new(1, 1), std::fs::read("image.j2k")?);
dec.send_packet(&pkt)?;
let frame = dec.receive_frame()?;
if let Frame::Video(v) = frame {
    println!("decoded {}x{}, format {:?}", v.width, v.height, v.format);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Codec id

- Codec: `"jpeg2000"`.

## Generating fixtures

```bash
# Baseline 5/3 integer reversible (the default)
opj_compress -i input.ppm -o input.j2k

# Or via ffmpeg's libopenjpeg wrapper
ffmpeg -f lavfi -i "testsrc=size=64x64:rate=24:duration=0.05" \
    -pix_fmt yuv420p input.j2k
```

## License

MIT — see [LICENSE](LICENSE).
