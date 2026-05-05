//! HTJ2K (ISO/IEC 15444-15 / ITU-T T.814) encoder modules.
//!
//! Round 3 scope (delta over round 2):
//!
//! * Multi-component encode for `Gray8`, `Rgb24`, and `Yuv444P` input
//!   pixel formats. SIZ writes `Csiz = N` with the matching per-
//!   component sub-sampling fields; the tier-2 packet emit loop walks
//!   `(resolution, component)` in LRCP order.
//! * Optional forward 5/3 reversible component transform (RCT, T.800
//!   §G.1) for RGB input via [`tile_enc::EncodeOptionsHt::use_color_transform`];
//!   signalled in COD by `MCT = 1`. The decoder already inverts the
//!   RCT for HTJ2K 5/3 + `MCT = 1`.
//!
//! Carried over from round 2:
//!
//! * Forward 5/3 reversible DWT for `NL ∈ [0, 5]` decomposition
//!   levels via [`crate::encode::dwt::fdwt_53`], wired into a per-
//!   resolution / per-band / per-codeblock layout shared with the
//!   decoder's `build_subbands` helper.
//! * HT cleanup pass encoder ([`cleanup_enc::encode_cleanup`]) with
//!   full multi-significance per quad (ρ ∈ {3, 5, 6, 7, 9..15}) and
//!   the §7.3.6 Eq-4 first-line-pair both-`u_off=1` special case.
//! * CAP / Rsiz markers for HTJ2K dispatch.
//! * Single tile, single layer.
//!
//! Out of scope (round 4+):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}).
//! * Multi-tile (the HTJ2K decoder rejects multi-tile-part codestreams
//!   today; this encoder matches that limit).
//! * Sub-sampled chroma (4:2:2 / 4:2:0) — both sides need per-component
//!   sub-band layouts at the sub-sampled extent.
//! * PPM/PPT packet headers (§A.7.4 / §A.7.5).
//! * Constrained sets (T.814 §8) and multi-set HT (T.814 Annex B).

#![cfg(feature = "htj2k")]

pub mod cleanup_enc;
pub mod cxt_vlc_enc;
pub mod mel_enc;
pub mod streams_enc;
pub mod tile_enc;
pub mod uvlc_enc;

pub use tile_enc::{encode_image_htj2k, EncodeOptionsHt};
