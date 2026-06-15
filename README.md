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
  cleanup with the run-length / UNIFORM shortcut), plus the §D.5
  segmentation symbol.
- **Reassembly** — per-coefficient `Nb(u, v)` magnitude-bit tracking for
  rate-truncated streams, dequantisation, the 5-3 and 9-7 inverse DWT,
  and the inverse multi-component transform.
- **Progression** — all five §B.12.1 orders (LRCP, RLCP, RPCL, PCRL,
  CPRL), §B.12.2 POC volume iteration, and **multi-layer** /
  **multi-precinct** reassembly.

### Not yet implemented

These surface a clean `Error::NotImplemented` rather than mis-decoding:

- `COC` / `QCC` per-component overrides and tile-part `COD` / `QCD`
  overrides.
- `RGN` region-of-interest, `POC` order changes mid-decode, and
  `PPM` / `PPT` packed-header markers.
- The Table A.19 segmentation-changing style bits (bypass / reset /
  termall).
- Position-keyed orders under non-power-of-two sub-sampling.
- High-Throughput JPEG 2000 (HTJ2K) block coding.

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
