use std::borrow::Cow;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("The provided version has an invalid syntax: {0}")]
    InvalidVersionSyntax(#[from] semver::Error),

    #[error("An error occurred while communicating with the crates.io API: {0}")]
    CratesIoApiError(#[from] crates_io_api::Error),

    #[error("An I/O error occurred: {0}")]
    AnyIoError(#[from] std::io::Error),

    #[error("File system error: {path}: {error}")]
    FileSystemError {
        path: String,
        error: std::io::ErrorKind,
    },

    #[error("Failed to parse cargo manifest at {path}: {error}")]
    CargoManifestParseError {
        path: String,
        error: toml::de::Error,
    },

    #[error("Failed to parse Cargo lock file at {path}: {error}")]
    CargoLockParseError {
        path: String,
        error: toml::de::Error,
    },

    #[error("{0}")]
    Other(Cow<'static, str>),

    #[error("Git packages are not supported: {0}")]
    GitPackageNotSupported(String),
}

impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Error::Other(Cow::Borrowed(value))
    }
}
