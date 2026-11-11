//! What an operator command reports when it fails.

#[derive(Debug)]
pub enum Error {
    Config(&'static str),
    File,
    Database,
    Store,
    /// the database server cannot hold this schema, whatever the instance is
    Unsupported(&'static str),
    Unknown,
    RestoreStage(&'static str),
    /// the HTTP service refused to start or ended on an error; nothing was written
    Serve(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(v) => write!(f, "invalid operator configuration: {v}"),
            Self::File => f.write_str("operator file is unavailable or could not be persisted"),
            Self::Database => f.write_str("database connection is unavailable"),
            Self::Store => f.write_str("instance is not ready or operator action was refused"),
            Self::Unsupported(v) => write!(f, "unsupported database server: {v}"),
            Self::Unknown => f.write_str(
                "operator action outcome is unknown; inspect the same instance before retrying",
            ),
            Self::RestoreStage(v) => write!(
                f,
                "restoration incomplete at {v}; inspect the isolated destination before retrying"
            ),
            Self::Serve(v) => write!(f, "service stopped: {v}"),
        }
    }
}

impl std::error::Error for Error {}
impl From<alt_server_store::Error> for Error {
    fn from(error: alt_server_store::Error) -> Self {
        match error {
            alt_server_store::Error::OutcomeUnknown => Self::Unknown,
            // the operator has to change the server, so this refusal is named
            alt_server_store::Error::Precondition(alt_server_store::LZ4_REQUIRED) => {
                Self::Unsupported(alt_server_store::LZ4_REQUIRED)
            }
            _ => Self::Store,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_without_lz4_is_named_and_other_refusals_are_not() {
        let named = Error::from(alt_server_store::Error::Precondition(
            alt_server_store::LZ4_REQUIRED,
        ));
        assert_eq!(
            named.to_string(),
            format!(
                "unsupported database server: {}",
                alt_server_store::LZ4_REQUIRED
            )
        );
        let other = Error::from(alt_server_store::Error::Precondition("request not ready"));
        assert!(matches!(other, Error::Store));
    }
}
