pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
    Protocol(ProtocolError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    BadLength,
    BadMessageType,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    DomainNotFound,
    InvalidTimestamp,
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
