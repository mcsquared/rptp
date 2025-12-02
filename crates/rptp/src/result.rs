use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
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

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    HeaderTooShort {
        found: usize,
    },
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    PayloadTooShort {
        field: &'static str,
        expected: usize,
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

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    UnsupportedPtpVersion(u8),
    DomainNotFound(u8),
    InvalidTimestamp { nanos: u32 },
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
