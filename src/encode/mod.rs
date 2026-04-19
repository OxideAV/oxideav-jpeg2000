//! JPEG 2000 Part-1 sample encoder.
//!
//! Scope of this module:
//!
//! - **5/3 integer reversible** wavelet (Part-1 lossless default).
//! - **Single quality layer**, **single tile**, **LRCP** progression.
//! - **Default precinct** grid (one precinct per resolution) — PPx =
//!   PPy = 15 in the COD.
//! - **No mode switches** (`Cblksty = 0`): sigprop / magref / cleanup
//!   passes are all MQ-coded.
//!
//! The submodules mirror the decoder's structure:
//!
//! - [`mqc`] — MQ arithmetic encoder (47 probability states).
//! - [`dwt`] — forward 5/3 integer lifting.
//! - [`t1`]  — tier-1 EBCOT bit-plane encoder.
//! - [`tile`] — per-tile driver orchestrating forward DWT, tier-1
//!   encode of every code-block, and tier-2 packet construction.
//! - [`codestream`] — assembles SOC / SIZ / COD / QCD / SOT / SOD /
//!   EOC marker chain into a raw `.j2k` byte stream.

pub mod codestream;
pub mod dwt;
pub mod mqc;
pub mod t1;
pub mod tile;

pub use codestream::{encode_frame, extract_jp2_codestream, EncodeOptions, TransformMode};
