//! Decode-side limits, strictness and the JPEG 2000 scalability
//! extras ([`DecodeOptions`]) of the standalone API. The encode-side
//! knobs are [`crate::EncodeOptions`] in [`mod@crate::encode`].

use crate::error::{Error, Jpeg2000Error};

/// Limits, strictness and selection for [`crate::decode_with`].
///
/// Every limit is checked against the `SIZ` geometry (after the
/// `reduce` scaling) **before** any sample plane is allocated, so a
/// hostile header fails with [`Jpeg2000Error::LimitExceeded`] instead
/// of committing memory. `max_bytes` bounds the decoder's working set:
/// every codestream component's `i32` sample plane (4 bytes per
/// sample) — at least the size of the returned image. The defaults:
/// `max_width` / `max_height` of [`DecodeOptions::DEFAULT_MAX_DIMENSION`]
/// (65 535), no pixel-count limit, [`DecodeOptions::DEFAULT_MAX_BYTES`]
/// (1 GiB), `strict = false`, full resolution, every layer.
///
/// `strict` rejects what the lenient decoder tolerates: bytes after the
/// `EOC` marker of a raw codestream, and a JP2 Image Header box
/// (`ihdr`) whose geometry / component count disagrees with the
/// codestream's `SIZ` (T.800 §I.5.3.1 requires them equal; the lenient
/// path trusts `SIZ`).
///
/// `reduce` discards the highest `reduce` resolution levels of every
/// component (ISO/IEC 15444-4 §B.2.3; each dimension becomes
/// `ceil(full / 2^reduce)`); `layers` decodes only the first `n`
/// quality layers (`None` = all). Both are the [`crate::decode_j2k_reduced`]
/// / [`crate::decode_j2k_layers`] depth surfaces on the contract path.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Reject images wider than this (after `reduce`).
    pub max_width: Option<u32>,
    /// Reject images taller than this (after `reduce`).
    pub max_height: Option<u32>,
    /// Reject images with more than this many pixels (`width × height`
    /// on component 0's grid).
    pub max_pixels: Option<u64>,
    /// Reject images whose decoder working set would exceed this many
    /// bytes (sum over components of `w × h × 4`).
    pub max_bytes: Option<u64>,
    /// Reject the recoverable irregularities listed in the type docs.
    pub strict: bool,
    /// Resolution levels to discard (`0` = full resolution).
    pub reduce: u8,
    /// Quality layers to decode (`None` = all; `Some(0)` is rejected).
    pub layers: Option<u16>,
}

impl DecodeOptions {
    /// Default [`Self::max_width`] / [`Self::max_height`]: 65 535.
    pub const DEFAULT_MAX_DIMENSION: u32 = 65_535;
    /// Default [`Self::max_bytes`]: 1 GiB of decoder working set.
    pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;

    /// The defaults (see the type docs).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the width limit.
    pub fn with_max_width(mut self, max_width: impl Into<Option<u32>>) -> Self {
        self.max_width = max_width.into();
        self
    }

    /// Set (or clear) the height limit.
    pub fn with_max_height(mut self, max_height: impl Into<Option<u32>>) -> Self {
        self.max_height = max_height.into();
        self
    }

    /// Set (or clear) the pixel-count limit.
    pub fn with_max_pixels(mut self, max_pixels: impl Into<Option<u64>>) -> Self {
        self.max_pixels = max_pixels.into();
        self
    }

    /// Set (or clear) the working-set byte limit.
    pub fn with_max_bytes(mut self, max_bytes: impl Into<Option<u64>>) -> Self {
        self.max_bytes = max_bytes.into();
        self
    }

    /// Set strict mode (see the type docs).
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Discard the highest `reduce` resolution levels.
    pub fn with_reduce(mut self, reduce: u8) -> Self {
        self.reduce = reduce;
        self
    }

    /// Decode only the first `layers` quality layers (`None` = all).
    pub fn with_layers(mut self, layers: impl Into<Option<u16>>) -> Self {
        self.layers = layers.into();
        self
    }

    /// Every limit cleared (`None`): unlimited decoding.
    pub fn unlimited(mut self) -> Self {
        self.max_width = None;
        self.max_height = None;
        self.max_pixels = None;
        self.max_bytes = None;
        self
    }

    /// Check a `width × height` picture whose decoder working set is
    /// `bytes` against the limits.
    pub(crate) fn check(&self, width: u32, height: u32, bytes: u64) -> Result<(), Error> {
        if let Some(m) = self.max_width {
            if width > m {
                return Err(Jpeg2000Error::limit(format!(
                    "width {width} exceeds max_width {m}"
                )));
            }
        }
        if let Some(m) = self.max_height {
            if height > m {
                return Err(Jpeg2000Error::limit(format!(
                    "height {height} exceeds max_height {m}"
                )));
            }
        }
        if let Some(m) = self.max_pixels {
            let pixels = u64::from(width) * u64::from(height);
            if pixels > m {
                return Err(Jpeg2000Error::limit(format!(
                    "{pixels} pixels exceed max_pixels {m}"
                )));
            }
        }
        if let Some(m) = self.max_bytes {
            if bytes > m {
                return Err(Jpeg2000Error::limit(format!(
                    "decoder working set of {bytes} bytes exceeds max_bytes {m}"
                )));
            }
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_width: Some(Self::DEFAULT_MAX_DIMENSION),
            max_height: Some(Self::DEFAULT_MAX_DIMENSION),
            max_pixels: None,
            max_bytes: Some(Self::DEFAULT_MAX_BYTES),
            strict: false,
            reduce: 0,
            layers: None,
        }
    }
}
