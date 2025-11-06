pub type Result<T> = core::result::Result<T, Error>;

pub enum Error {
    Parse(ParseError),
    Protocol(ProtocolError),
}

pub enum ParseError {
    BadLength,
}

pub enum ProtocolError {
    DomainNotFound,
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
