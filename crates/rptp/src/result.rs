//! Common `Result` and error types for `rptp`.
//!
//! The crate distinguishes between two broad categories of failure:
//! - [`ParseError`]: the input bytes are incomplete or structurally inconsistent (e.g. truncated
//!   header, declared length mismatch, missing payload fields).
//! - [`ProtocolError`]: the bytes are well-formed enough to parse, but represent an unsupported
//!   or invalid protocol value (e.g. unsupported PTP version, unknown message type nibble, invalid
//!   timestamp encoding) or an integration-level mismatch (e.g. an incoming domain number is not
//!   configured locally).
//!
//! Most parsing paths in [`crate::wire`] and [`crate::message`] return these errors. Higher-level
//! dispatch (e.g. domain lookup in [`crate::port`]) may also surface them.

use core::fmt;

/// Crate-wide `Result` type using [`Error`] as the error variant.
pub type Result<T> = core::result::Result<T, Error>;

/// Top-level error type for fallible `rptp` operations.
///
/// This enum primarily exists to provide a single error type for the public API while still
/// preserving the distinction between parse failures and protocol-level rejections.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The input bytes could not be parsed into a valid PTP message/field.
    Parse(ParseError),
    /// The input was parsed but rejected as unsupported or invalid.
    Protocol(ProtocolError),
}

impl From<ParseError> for Error {
    fn from(err: ParseError) -> Self {
        Error::Parse(err)
    }
}

impl From<ProtocolError> for Error {
    fn from(err: ProtocolError) -> Self {
        Error::Protocol(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "parse error: {}", e),
            Error::Protocol(e) => write!(f, "protocol error: {}", e),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Errors caused by malformed or incomplete input buffers.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not enough bytes were provided to even read the common PTP header.
    HeaderTooShort {
        /// The number of bytes available.
        found: usize,
    },
    /// The header's declared message length does not match the received buffer length.
    LengthMismatch {
        /// The length declared in the message header.
        declared: usize,
        /// The number of bytes actually available.
        actual: usize,
    },
    /// The payload did not contain enough bytes to read a required field.
    PayloadTooShort {
        /// A human-readable field name (used for debugging and error messages).
        field: &'static str,
        /// The required size for the field.
        expected: usize,
        /// The number of bytes available in the payload slice.
        found: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::HeaderTooShort { found } => {
                write!(f, "message too short for PTP header: found {found} bytes")
            }
            ParseError::LengthMismatch { declared, actual } => write!(
                f,
                "declared PTP length {declared} does not match actual {actual}"
            ),
            ParseError::PayloadTooShort {
                field,
                expected,
                found,
            } => write!(
                f,
                "payload too short for field `{field}`: expected {expected} bytes, found {found}"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParseError {}

/// Errors caused by unsupported or semantically invalid protocol values.
#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// The PTP version field in the header is not supported by this crate.
    UnsupportedPtpVersion(u8),
    /// The incoming message's domain number does not match any configured local domain.
    ///
    /// This is returned by domain routing (e.g. a `PortMap`) when it cannot find a port for the
    /// incoming domain.
    DomainNotFound(u8),
    /// A wire timestamp field uses an invalid nanosecond value.
    ///
    /// The PTP timestamp format encodes nanoseconds as an unsigned integer and constrains it to
    /// `[0, 1_000_000_000)`. Values outside that range are rejected.
    InvalidTimestamp { nanos: u32 },
    /// The message type nibble in the header does not map to a known message class.
    UnknownMessageType(u8),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::UnsupportedPtpVersion(v) => {
                write!(f, "unsupported PTP version {v}")
            }
            ProtocolError::DomainNotFound(domain) => {
                write!(f, "PTP domain {domain} not found")
            }
            ProtocolError::InvalidTimestamp { nanos } => {
                write!(f, "invalid timestamp nanoseconds {nanos}")
            }
            ProtocolError::UnknownMessageType(t) => {
                write!(f, "unknown message type nibble 0x{t:02x}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ProtocolError {}
