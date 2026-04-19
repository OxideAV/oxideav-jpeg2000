//! JPEG 2000 Part-1 sample decoder.
//!
//! The module is split into layers that mirror the ISO/IEC 15444-1
//! decoder reference (§D, §E, §F, §G):
//!
//! - [`mqc`] — MQ arithmetic decoder (47 probability states,
//!   BYTEIN/RENORMD/DECODE primitives). Ported from OpenJPEG `mqc.c`.
//! - [`bio`] — bit I/O reader used by tier-2 packet headers.
//! - [`tagtree`] — hierarchical value compressor used for inclusion +
//!   zero-bitplane tags in packet headers.
//! - [`t1`] — tier-1 EBCOT bit-plane decoder (significance propagation,
//!   magnitude refinement, cleanup).
//! - [`dwt`] — inverse 5/3 integer reversible and 9/7 irreversible
//!   lifting, 1-D and 2-D.
//! - [`tile`] — tile-level driver: COD/QCD parsing, tier-2 progression
//!   sweep, tier-1 decode for every participating code-block, IDWT per
//!   resolution level, DC level-shift, component transform.
//! - [`frame`] — public API turning a parsed [`crate::Codestream`]
//!   into a decoded `oxideav_core::Frame`.

pub mod bio;
pub mod dwt;
pub mod frame;
pub mod mqc;
pub mod t1;
pub mod tagtree;
pub mod tile;
