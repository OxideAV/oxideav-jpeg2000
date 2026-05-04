//! JPEG 2000 Part-1 sample encoder.
//!
//! Scope of this module:
//!
//! - **5/3 integer reversible** and **9/7 irreversible float** wavelets.
//! - **Single quality layer**, **single tile**, **LRCP** progression.
//! - **Default precinct** grid (one precinct per resolution) — PPx =
//!   PPy = 15 in the COD.
//! - **No mode switches** (`Cblksty = 0`): sigprop / magref / cleanup
//!   passes are all MQ-coded.
//! - **Gray8** and **Rgb24** input with optional forward RCT / ICT
//!   component transform.
//! - Optional **JP2 ISOBMFF box wrapper** (`.jp2`) around the raw
//!   `.j2k` codestream.
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
#[cfg(feature = "htj2k")]
pub mod htj2k;
pub mod mqc;
pub mod t1;
pub mod tile;

pub use codestream::{
    encode_image, extract_jp2_codestream, EncodeOptions, PacketHeaderPlacement, ProgressionOrder,
    TransformMode,
};

#[cfg(feature = "htj2k")]
pub use htj2k::{encode_image_htj2k, EncodeOptionsHt};

#[cfg(feature = "registry")]
pub use codestream::encode_frame;
