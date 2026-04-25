# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.4](https://github.com/OxideAV/oxideav-jpeg2000/compare/v0.0.3...v0.0.4) - 2026-04-25

### Fixed

- mark test fixtures as binary so Windows CI doesn't CRLF them

### Other

- drop oxideav-codec/oxideav-container shims, import from oxideav-core
- round 9 — tighten HH-interop regression tests
- un-ignore round-9-closed OPJ interop / ffmpeg / multi-tile tests
- round 9 — fix ZC context for HH sub-band (spec Table D.1)
- round 8 — black-box probe rules out HH lifting as root cause
- round 7 — swap FDWT/IDWT axis order to spec-conformant VER-then-HOR / HOR-then-VER
- round-6 MQ trace harness + LL/HL/LH bit-exact OPJ interop
- add T1 sub-band diff harness for OPJ interop debugging
- add 16x16 1-level 5/3 round-trip test + tighten opj ignore notes
- mark sigprop-tested samples as pi-tested even on bit=0
- swap MQ state-table nlps/nmps transitions
- add opj_compress interop diagnostics + passing const fixture
- multi-tile decode (T.800 §B.3)
- README + crate docs — document 9/7 encoder + JP2 wrapper
- add JP2 ISOBMFF wrapper (encode + transparent decode)
- add 9/7 irreversible encoder + RGB / forward RCT / ICT
- add forward 9/7 irreversible DWT
- add 5/3 reversible lossless encoder
- wire 9/7 irreversible wavelet through decoder
- Merge remote-tracking branch 'origin/master' into wt/complete
- add Part-1 sample decoder (MQ + EBCOT + 5/3 IDWT + tier-2)
