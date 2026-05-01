//! High-Throughput JPEG 2000 (FBCOT) block decoder.
//!
//! Implements the core entropy-decoding part of ISO/IEC 15444-15:2019
//! (= ITU-T T.814), Annex B (HT data organisation) and §7 (HT block
//! decoding algorithm). The companion CxtVLC tables of Annex C are
//! transcribed verbatim into [`cxt_vlc_tables`].
//!
//! Scope of round 2 (this module):
//!
//! * Three-pass decode per §7.3, §7.4, §7.5 — HT cleanup, HT SigProp,
//!   HT MagRef.
//! * Dual-substream split per §7.1 (MagSgn + MEL + VLC inside the
//!   cleanup segment; SigProp + MagRef inside the refinement segment).
//! * Placeholder-pass handling per Annex B.1 / B.3 — a coding pass
//!   present in the Z_blk count but with zero bytes in the codestream
//!   produces no state changes.
//! * Per-codeblock public entry point [`decode_codeblock`] taking the
//!   block dimensions and the raw cleanup / refinement segments.
//!
//! Out of scope (deferred to round 3): tier-2 packet header parsing
//! that turns a packet body into HT-set boundaries; Annex F encoder;
//! the constrained-codestream sets defined in §8.

mod cleanup;
mod cxt_vlc;
mod cxt_vlc_tables;
mod magref;
mod mel;
mod sigprop;
mod streams;
mod uvlc;

pub use cleanup::CleanupOutput;
pub use sigprop::SigPropOutput;

use oxideav_core::{Error, Result};

/// Number of FBCOT passes encoded for one HT code-block (per Annex B
/// of ISO/IEC 15444-15). Matches the spec symbol `Z_blk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZBlk {
    /// Skipped block — no HT segments are available, all sample
    /// outputs are 0 (§7.1.1).
    Zero,
    /// Cleanup pass only.
    One,
    /// Cleanup + SigProp.
    Two,
    /// Cleanup + SigProp + MagRef.
    Three,
}

/// Final FBCOT decoder output for a single code-block.
#[derive(Debug, Clone)]
pub struct CodeblockOutput {
    pub width: u32,
    pub height: u32,
    /// Magnitude `μ_n` per sample, in quad-scan order.
    pub mag: Vec<u64>,
    /// Sign bit `s_n` per sample (1 = negative).
    pub sign: Vec<u8>,
    /// SigProp/MagRef refinement bit `r_n` per sample. Always 0 when
    /// `Z_blk` <= 1.
    pub refinement: Vec<u8>,
    /// SigProp/MagRef refinement indicator `z_n` per sample.
    pub z: Vec<u8>,
}

/// Decode one HT code-block.
///
/// * `width`, `height` — sample dimensions of the code-block.
/// * `dcup` — HT cleanup segment bytes (`Lcup` bytes). Must be
///   non-empty when `zblk != Zero`.
/// * `dref` — HT refinement segment bytes (`Lref` bytes). May be
///   empty when `zblk` is `One` or when the SigProp/MagRef passes are
///   placeholders.
pub fn decode_codeblock(
    width: u32,
    height: u32,
    zblk: ZBlk,
    dcup: &[u8],
    dref: &[u8],
) -> Result<CodeblockOutput> {
    let nsamples = (width as usize).div_ceil(2) * (height as usize).div_ceil(2) * 4;
    if zblk == ZBlk::Zero {
        // §7.1.1: all sample output values shall be 0.
        return Ok(CodeblockOutput {
            width,
            height,
            mag: vec![0u64; nsamples],
            sign: vec![0u8; nsamples],
            refinement: vec![0u8; nsamples],
            z: vec![0u8; nsamples],
        });
    }

    if dcup.is_empty() {
        return Err(Error::invalid(
            "HTJ2K: cleanup segment cannot be empty when Z_blk > 0",
        ));
    }
    let cleanup = cleanup::decode_cleanup(width, height, dcup)?;

    let (sigprop_out, magref_out) = match zblk {
        ZBlk::Zero => unreachable!(),
        ZBlk::One => (None, None),
        ZBlk::Two => {
            let sp = sigprop::decode_sigprop(&cleanup, dref)?;
            (Some(sp), None)
        }
        ZBlk::Three => {
            let sp = sigprop::decode_sigprop(&cleanup, dref)?;
            let mr = magref::decode_magref(&cleanup, &sp, dref)?;
            (Some(sp), Some(mr))
        }
    };

    let (refinement, z, sign) = if let Some(mr) = magref_out {
        (mr.r, mr.z, mr.sign)
    } else if let Some(sp) = sigprop_out {
        (sp.r, sp.z, sp.sign)
    } else {
        (
            vec![0u8; nsamples],
            vec![0u8; nsamples],
            cleanup.sign.clone(),
        )
    };

    Ok(CodeblockOutput {
        width,
        height,
        mag: cleanup.mag,
        sign,
        refinement,
        z,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_zblk_returns_all_zero() {
        let out = decode_codeblock(4, 4, ZBlk::Zero, &[], &[]).unwrap();
        // 4x4 = 4 quads = 16 samples
        assert_eq!(out.mag, vec![0; 16]);
        assert_eq!(out.sign, vec![0; 16]);
        assert_eq!(out.refinement, vec![0; 16]);
        assert_eq!(out.z, vec![0; 16]);
    }

    #[test]
    fn placeholder_passes_decode_to_cleanup_state() {
        // Cleanup-only AZC: the small 2x2 block with a MEL byte that
        // emits 0 for the first quad, then placeholder SigProp+MagRef.
        let dcup = vec![0x80u8, 0x03, 0x00];
        let out = decode_codeblock(2, 2, ZBlk::Three, &dcup, &[]).unwrap();
        assert_eq!(out.mag, vec![0u64; 4]);
        assert_eq!(out.refinement, vec![0u8; 4]);
        assert_eq!(out.z, vec![0u8; 4]);
    }

    #[test]
    fn one_pass_only_skips_refinement_streams() {
        let dcup = vec![0x80u8, 0x03, 0x00];
        let out = decode_codeblock(2, 2, ZBlk::One, &dcup, &[]).unwrap();
        assert_eq!(out.mag, vec![0u64; 4]);
    }
}
