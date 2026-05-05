//! HTJ2K (ISO/IEC 15444-15 / ITU-T T.814) encoder modules.
//!
//! Round 4 scope (delta over round 3):
//!
//! * **9/7 irreversible transform** path. New
//!   [`tile_enc::HtTransform::Irreversible97`] selector drives the
//!   forward 9/7 lifting + per-band scalar quantiser, with the QCD
//!   emitted in expounded form (qntsty = 2).
//! * **Multi-tile codestream output** via the new
//!   [`tile_enc::EncodeOptionsHt::tile_size`] knob. Per-tile DWT +
//!   tier-1 + tier-2 are independent, exactly as Part-1 prescribes.
//!   The HT decoder dispatches multi-tile-part codestreams now too.
//! * **Sub-sampled chroma input** for `Yuv420P` / `Yuv422P` pixel
//!   formats. SIZ encodes per-component `(XRsiz, YRsiz)`; cleanup +
//!   tier-2 walk each component at its own sub-sampled extent.
//! * **PPM / PPT packed packet headers** via
//!   [`tile_enc::EncodeOptionsHt::packet_header_placement`]. Reuses the
//!   classic encoder's `split_packet_headers` helper to extract per-
//!   tile-part header bytes from the inline body.
//!
//! Carried over from round 3:
//!
//! * Multi-component encode for `Gray8`, `Rgb24`, `Yuv444P` input.
//! * Optional forward 5/3 reversible component transform (RCT, T.800
//!   §G.1) for RGB input via [`tile_enc::EncodeOptionsHt::use_color_transform`];
//!   signalled in COD by `MCT = 1`.
//! * Forward 5/3 reversible DWT for `NL ∈ [0, 5]` decomposition levels
//!   via [`crate::encode::dwt::fdwt_53`].
//! * HT cleanup pass encoder ([`cleanup_enc::encode_cleanup`]) with
//!   full multi-significance per quad and the §7.3.6 Eq-4 first-line-
//!   pair both-`u_off=1` special case.
//! * CAP / Rsiz markers for HTJ2K dispatch.
//! * Single quality layer.
//!
//! Out of scope (round 5+):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}). The decoder
//!   side already supports them; the encoder still emits cleanup-only
//!   (Z_blk = 1).
//! * Multi-layer (single quality layer per code-block).
//! * Constrained sets (T.814 §8) and multi-set HT (T.814 Annex B).

#![cfg(feature = "htj2k")]

pub mod cleanup_enc;
pub mod cxt_vlc_enc;
pub mod magref_enc;
pub mod mel_enc;
pub mod sigprop_enc;
pub mod streams_enc;
pub mod tile_enc;
pub mod uvlc_enc;

pub use tile_enc::{encode_image_htj2k, EncodeOptionsHt, HtPacketHeaderPlacement, HtTransform};
