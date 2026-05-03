//! Crate-local error type.
//!
//! Defined as a small std-only enum so the crate can be built with the
//! default `registry` feature off — i.e. without depending on
//! `oxideav-core` at all. When the `registry` feature is on (the default)
//! a `From<Jpeg2000Error> for oxideav_core::Error` impl is enabled in
//! [`crate::registry`] so the `Decoder`/`Encoder` trait surface still
//! interoperates cleanly.
//!
//! The variants mirror the subset of `oxideav_core::Error` that the
//! JPEG 2000 decoder/encoder pipeline actually produces.

use core::fmt;

/// Crate-local error type for the JPEG 2000 decoder/encoder pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Jpeg2000Error {
    /// Bitstream / marker / packet header was malformed.
    InvalidData(String),
    /// Bitstream was syntactically valid but uses a feature this crate
    /// does not implement yet.
    Unsupported(String),
}

impl Jpeg2000Error {
    /// Construct an [`Jpeg2000Error::InvalidData`].
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct an [`Jpeg2000Error::Unsupported`].
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl fmt::Display for Jpeg2000Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {}", s),
            Self::Unsupported(s) => write!(f, "unsupported: {}", s),
        }
    }
}

impl std::error::Error for Jpeg2000Error {}

/// Crate-local result alias used throughout the pipeline.
pub type Result<T> = core::result::Result<T, Jpeg2000Error>;
