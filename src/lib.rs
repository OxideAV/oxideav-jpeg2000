//! # oxideav-jpeg2000
//!
//! **Status:** orphan-rebuild scaffold (post 2026-05-20 audit).
//!
//! The prior implementation was retired under the workspace clean-room
//! policy. The crate will be re-implemented from scratch against
//! ITU-T T.800 (ISO/IEC 15444-1) and ISO/IEC 15444-15 (HTJ2K) in a
//! future clean-room round, using only material under `docs/` and
//! black-box validator binaries.
//!
//! Every public API currently returns [`Error::NotImplemented`].

#![warn(missing_debug_implementations)]

#[cfg(feature = "registry")]
use oxideav_core::RuntimeContext;

/// Crate-local error type. Until the clean-room rebuild lands every
/// public API path returns [`Error::NotImplemented`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The crate has been reset to a scaffold pending clean-room
    /// rebuild; no decoder or encoder functionality is wired up yet.
    NotImplemented,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "oxideav-jpeg2000: orphan-rebuild scaffold — no decoder/encoder wired up"
        )
    }
}

impl std::error::Error for Error {}

/// Decode a JPEG 2000 codestream or JP2 file.
///
/// Returns [`Error::NotImplemented`] until the clean-room rebuild
/// lands.
pub fn decode_jpeg2000(_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    Err(Error::NotImplemented)
}

/// Encode RGB / grey data into a JPEG 2000 codestream or JP2 file.
///
/// Returns [`Error::NotImplemented`] until the clean-room rebuild
/// lands.
pub fn encode_jpeg2000(_pixels: &[u8], _width: u32, _height: u32) -> Result<Vec<u8>, Error> {
    Err(Error::NotImplemented)
}

/// No-op codec registration — the orphan-rebuild scaffold registers
/// nothing into the runtime context.
#[cfg(feature = "registry")]
pub fn register(_ctx: &mut RuntimeContext) {}

#[cfg(feature = "registry")]
oxideav_core::register!("jpeg2000", register);
