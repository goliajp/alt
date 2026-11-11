use std::fmt;

/// An alt-json error that serde implementations can produce.
#[derive(Debug)]
pub(crate) struct Error(pub(crate) alt_json::Error);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Error(alt_json::Error::custom(message))
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Error(alt_json::Error::custom(message))
    }
}

impl From<alt_json::Error> for Error {
    fn from(error: alt_json::Error) -> Self {
        Error(error)
    }
}

impl From<Error> for alt_json::Error {
    fn from(error: Error) -> Self {
        error.0
    }
}
