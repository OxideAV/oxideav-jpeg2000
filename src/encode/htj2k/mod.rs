//! HTJ2K (ISO/IEC 15444-15 / ITU-T T.814) encoder modules.
//!
//! Round 1 scope:
//!
//! * Forward 5/3 reversible DWT (reused from the Part-1 encoder via
//!   [`crate::encode::dwt`]).
//! * HT cleanup pass encoder ([`cleanup_enc::encode_cleanup`]) — emits
//!   the dual MagSgn / MEL / VLC sub-streams per T.814 §7.1 and §7.3.
//!   Single significance per quad, one HT cleanup pass per code-block
//!   (Z_blk = 1).
//! * CAP / Rsiz markers for HTJ2K dispatch (CPF deferred to round 2).
//! * Single tile, single component, single layer, LRCP, NL = 0
//!   (identity DWT) for the round-1 minimum-viable encoder.
//!
//! Out of scope for round 1 (deferred to round 2):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}).
//! * Multi-significant-sample quads (encoder rejects them with
//!   `Error::Unsupported`).
//! * First-line-pair both-significant pair (§7.3.6 Eq 4 special case).
//! * Multi-tile, multi-layer, multi-component, MCT.
//! * Constrained sets (T.814 §8) and multi-set HT (T.814 Annex B).

#![cfg(feature = "htj2k")]

pub mod cleanup_enc;
pub mod cxt_vlc_enc;
pub mod mel_enc;
pub mod streams_enc;
pub mod tile_enc;
pub mod uvlc_enc;

pub use tile_enc::{encode_image_htj2k, EncodeOptionsHt};
