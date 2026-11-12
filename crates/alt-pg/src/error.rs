use std::{fmt, io};

/// an error reported by the server in an `ErrorResponse`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerError {
    pub severity: String,
    /// the five-character SQLSTATE
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub constraint: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    /// the connection parameters cannot be used
    Config(&'static str),
    Io(io::Error),
    /// the server sent bytes that are not a valid protocol message
    Protocol(&'static str),
    Authentication(&'static str),
    Server(Box<ServerError>),
    /// the statement cannot be put on the wire
    Request(&'static str),
    /// parameter `index` cannot be sent as the type the statement expects
    Parameter {
        index: usize,
        ty: crate::Oid,
        reason: &'static str,
    },
    /// the number of parameters differs from what the statement takes
    ParameterCount {
        expected: usize,
        given: usize,
    },
    /// a column cannot be read as the requested type
    Column {
        index: usize,
        ty: crate::Oid,
        reason: String,
    },
    /// no column has this name or index
    NoColumn(String),
    /// the statement returned a different number of rows than the call allows
    RowCount,
    /// an earlier failure left the connection unusable
    Closed,
}

impl Error {
    /// the SQLSTATE of a server error
    pub fn code(&self) -> Option<&str> {
        self.as_server().map(|error| error.code.as_str())
    }

    pub fn as_server(&self) -> Option<&ServerError> {
        match self {
            Self::Server(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(what) => write!(f, "invalid connection parameters: {what}"),
            Self::Io(error) => write!(f, "connection I/O: {error}"),
            Self::Protocol(what) => write!(f, "protocol violation: {what}"),
            Self::Authentication(what) => write!(f, "authentication failed: {what}"),
            Self::Server(error) => {
                write!(f, "{}: {} ({})", error.severity, error.message, error.code)?;
                if let Some(detail) = &error.detail {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Request(what) => write!(f, "invalid request: {what}"),
            Self::Parameter { index, ty, reason } => {
                write!(f, "parameter {index} of type oid {ty}: {reason}")
            }
            Self::ParameterCount { expected, given } => {
                write!(f, "statement takes {expected} parameters, {given} given")
            }
            Self::Column { index, ty, reason } => {
                write!(f, "column {index} of type oid {ty}: {reason}")
            }
            Self::NoColumn(name) => write!(f, "no column {name}"),
            Self::RowCount => f.write_str("statement returned an unexpected number of rows"),
            Self::Closed => f.write_str("connection closed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
