//! HT MagRef pass decoder (§7.5 of ISO/IEC 15444-15:2019,
//! FDIS page 22).
//!
//! The MagRef pass refines magnitudes for samples that already had
//! σ_n = 1 after the cleanup pass. Per §7.5, the spec procedure
//! `decodeMagRefValue` is "if σ_n != 0 then z_n=1 and r_n=importMagRefBit".
//! There is no neighbourhood logic, no sign step, and no MEL
//! interaction.

use super::cleanup::CleanupOutput;
use super::sigprop::SigPropOutput;
use super::streams::MagRefReader;
use crate::error::Result;

/// Run the MagRef pass on top of cleanup + SigProp output. `dref` is
/// the *same* HT refinement segment as feeds the SigProp reader; the
/// MagRef reader walks it from the opposite end.
pub fn decode_magref(
    cleanup: &CleanupOutput,
    sigprop: &SigPropOutput,
    dref: &[u8],
) -> Result<SigPropOutput> {
    let mut z = sigprop.z.clone();
    let mut r = sigprop.r.clone();
    let sign = sigprop.sign.clone();
    let mut reader = MagRefReader::new(dref);

    for n in 0..cleanup.sig.len() {
        if cleanup.sig[n] != 0 {
            z[n] = 1;
            r[n] = reader.import_bit()?;
        }
    }
    Ok(SigPropOutput { z, r, sign })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::cleanup::decode_cleanup;
    use crate::decode::htj2k::sigprop::decode_sigprop;

    #[test]
    fn placeholder_magref_leaves_state_zero() {
        let dcup = vec![0x80u8, 0x03, 0x00];
        let cleanup = decode_cleanup(2, 2, &dcup).unwrap();
        let sp = decode_sigprop(&cleanup, &[]).unwrap();
        let mr = decode_magref(&cleanup, &sp, &[]).unwrap();
        assert_eq!(mr.z, vec![0; 4]);
        assert_eq!(mr.r, vec![0; 4]);
    }
}
