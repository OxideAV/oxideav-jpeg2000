//! HTJ2K (ISO/IEC 15444-15 / ITU-T T.814) encoder modules.
//!
//! Round 6 scope (delta over round 5):
//!
//! * **HT SigProp + MagRef encoder passes** wired into the codestream.
//!   New [`tile_enc::EncodeOptionsHt::pass_count`] selector picks
//!   `Cleanup` (`Z_blk = 1`, the historical default), `CleanupSigprop`
//!   (`Z_blk = 2`), or `CleanupSigpropMagref` (`Z_blk = 3`). Per code-
//!   block the encoder runs [`cleanup_enc::encode_cleanup`] to produce
//!   `Dcup`, then derives a `CleanupOutput`-equivalent state from the
//!   sample magnitudes and runs [`sigprop_enc::encode_sigprop`] +
//!   [`magref_enc::encode_magref`] (with all-zero refinement bits, since
//!   the cleanup pass already communicates the full sample magnitude in
//!   FBCOT round 1) to obtain `Dref`. The packet header writes
//!   `num_passes ∈ {1, 2, 3}`, two length fields when `num_passes ≥ 2`
//!   per ISO/IEC 15444-15 §B.3 (lblock + ⌊log2(passes_added)⌋), and
//!   appends `Dref` after `Dcup` in the packet body. The decoder side
//!   already round-trips `Z_blk ∈ {2, 3}`.
//!
//! Carried over from round 5:
//!
//! * **9/7 irreversible transform** path. The
//!   [`tile_enc::HtTransform::Irreversible97`] selector drives the
//!   forward 9/7 lifting + per-band scalar quantiser, with the QCD
//!   emitted in expounded form (qntsty = 2).
//! * **Multi-tile codestream output** via
//!   [`tile_enc::EncodeOptionsHt::tile_size`].
//! * **Sub-sampled chroma input** for `Yuv420P` / `Yuv422P` pixel
//!   formats. SIZ encodes per-component `(XRsiz, YRsiz)`; cleanup +
//!   tier-2 walk each component at its own sub-sampled extent.
//! * **PPM / PPT packed packet headers** via
//!   [`tile_enc::EncodeOptionsHt::packet_header_placement`].
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
//! Out of scope (round 7+):
//!
//! * Multi-layer (single quality layer per code-block).
//! * Rate-distortion truncation: refinement-bit selection is currently
//!   "all zero" (placeholder-equivalent w.r.t. magnitudes; the decoder
//!   recovers the same `mag[n]` as cleanup-only).
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

pub use tile_enc::{
    encode_image_htj2k, EncodeOptionsHt, HtPacketHeaderPlacement, HtPassCount, HtTransform,
};
