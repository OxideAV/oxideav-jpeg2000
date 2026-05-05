//! HTJ2K (ISO/IEC 15444-15 / ITU-T T.814) encoder modules.
//!
//! Round 2 scope:
//!
//! * Forward 5/3 reversible DWT for `NL ∈ [0, 5]` decomposition
//!   levels via [`crate::encode::dwt::fdwt_53`], wired into a per-
//!   resolution / per-band / per-codeblock layout shared with the
//!   decoder's `build_subbands` helper.
//! * HT cleanup pass encoder ([`cleanup_enc::encode_cleanup`]) with
//!   full multi-significance per quad (ρ ∈ {3, 5, 6, 7, 9..15}) and
//!   the §7.3.6 Eq-4 first-line-pair both-`u_off=1` special case.
//! * CAP / Rsiz markers for HTJ2K dispatch.
//! * One tier-2 packet per resolution. Single tile, single component
//!   (Gray8), single layer, LRCP.
//!
//! Out of scope for round 2 (deferred to round 3):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}).
//! * Multi-tile, multi-layer, multi-component, MCT.
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
