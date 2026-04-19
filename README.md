# oxideav-jpeg2000

Pure-Rust **JPEG 2000** (ISO/IEC 15444-1) codec crate. Today this ships
a Part-1 codestream marker parser and a decoder stub — the wavelet
transform, MQ arithmetic coder, and EBCOT tier-1 / tier-2 passes are
not implemented yet, so the decoder refuses to produce pixels. Zero C
dependencies.

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace)
framework but usable standalone.

## What works

The `codestream` module parses a raw `.j2k` codestream (the inner
compressed bytes — no `.jp2` ISOBMFF box wrapper yet) and returns:

- Image geometry from SIZ: canvas size, origin, tile size, tile grid
  origin.
- Per-component bit depth (1–38 bits), signedness, horizontal/vertical
  sub-sampling.
- Raw bytes of the COD and QCD segments (coding style and quantisation
  defaults), preserved for later decode passes.
- Every tile-part: tile index, tile-part index, tile-part count,
  declared length (`Psot`), byte offset and length of the compressed
  data after SOD. Tile-parts with `Psot = 0` ("length unknown, runs to
  the next SOT or EOC") are resolved by scanning forward.
- The offset of the EOC marker, when present.

Markers walked through without deep parsing: COC, QCC, RGN, POC, PPM,
PPT, PLM, PLT, TLM, CRG, COM. Unknown markers surface as
`Error::InvalidData`.

## What does not work yet

- No sample decode. `Decoder::receive_frame` returns
  `Error::Unsupported("jpeg2000 decode not yet implemented")`. The
  missing pieces are the 5/3 integer reversible and 9/7 irreversible
  wavelet transforms, the MQ arithmetic coder, EBCOT tier-1 bit-plane
  coding, and tier-2 packet header parsing.
- No encoder. `make_encoder` returns `Error::Unsupported`.
- No `.jp2` box wrapper (`00 00 00 0C 6A 50 20 20 0D 0A 87 0A`
  signature + JP2 Colour Specification / Metadata boxes). Feed the
  inner codestream directly to the parser.
- No Part-2 extensions (multi-component transform, arbitrary wavelet
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

### Decoder (parses, then refuses)

```rust
use oxideav_codec::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Error, Packet, TimeBase};

let mut reg = CodecRegistry::new();
oxideav_jpeg2000::register(&mut reg);

let params = CodecParameters::video(CodecId::new("jpeg2000"));
let mut dec = reg.make_decoder(&params)?;
let pkt = Packet::new(0, TimeBase::new(1, 1), std::fs::read("image.j2k")?);
dec.send_packet(&pkt)?;  // runs the marker parser, succeeds
match dec.receive_frame() {
    Err(Error::Unsupported(msg)) => eprintln!("(expected) {msg}"),
    _ => unreachable!("decode is not implemented yet"),
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Codec id

- Codec: `"jpeg2000"`.

## License

MIT — see [LICENSE](LICENSE).
