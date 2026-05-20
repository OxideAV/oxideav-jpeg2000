# oxideav-jpeg2000

Pure-Rust JPEG 2000 (J2K + JP2) and High-Throughput JPEG 2000 (HTJ2K)
codec.

## Status — 2026-05-20

**Orphan-rebuild scaffold.** The crate's prior implementation was
retired under the workspace clean-room policy: provenance for one or
more module-level docstrings could not be defended against the
"no external library source as reference" rule that governs every
crate in this workspace.

Per workspace policy, the only acceptable response is a full
clean-room re-implementation against the standards documents and
black-box validator binaries. That work has not yet been scheduled.

Every public entry point currently returns `Error::NotImplemented`.

## Planned clean-room sources

The clean-room rebuild will consult only:

* ITU-T Rec. T.800 | ISO/IEC 15444-1 — JPEG 2000 image coding system —
  Part 1: Core coding system.
* ITU-T Rec. T.801 | ISO/IEC 15444-2 — Part 2: Extensions.
* ISO/IEC 15444-15 — High-Throughput JPEG 2000 (HTJ2K).
* ITU-T Rec. T.814 | ISO/IEC 15444-15 supporting material as published.
* Black-box invocations of `opj_compress` / `opj_decompress` /
  `ojph_compress` / `ojph_expand` (the binaries — not their source) as
  opaque validators.

No external library source — OpenJPEG, OpenJPH, Kakadu, etc. — is
permitted as a reference under the workspace clean-room policy.

## License

MIT. See `LICENSE`.
