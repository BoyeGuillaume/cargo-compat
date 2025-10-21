//! Unified error type used across the application.
//! Variants are designed to provide actionable context for end users and developers.
use std::borrow::Cow;

use thiserror::Error;

#[derive(Debug, Error)]
/// All possible errors returned by rust-version-searcher.
pub enum Error {
    /// The provided version string could not be parsed as a valid semver.
    #[error("The provided version has an invalid syntax: {0}")]
    InvalidVersionSyntax(#[from] semver::Error),

    /// Network or protocol error while communicating with crates.io.
    #[error("An error occurred while communicating with the crates.io API: {0}")]
    CratesIoApiError(#[from] crates_io_api::Error),

    /// Underlying I/O error from the filesystem or a subprocess.
    #[error("An I/O error occurred: {0}")]
    AnyIoError(#[from] std::io::Error),

    /// A filesystem operation failed at a specific path.
    #[error("File system error: {path}: {error}")]
    FileSystemError {
        path: String,
        error: std::io::ErrorKind,
    },

    /// The Cargo.toml file could not be deserialized from TOML.
    #[error("Failed to parse cargo manifest at {path}: {error}")]
    CargoManifestParseError {
        path: String,
        error: toml::de::Error,
    },

    /// The Cargo.lock file could not be deserialized from TOML.
    #[error("Failed to parse Cargo lock file at {path}: {error}")]
    CargoLockParseError {
        path: String,
        error: toml::de::Error,
    },

    /// A generic error with a human-readable message.
    #[error("{0}")]
    Other(Cow<'static, str>),

    /// The project contains a git dependency which is not supported by this tool.
    #[error("Git packages are not supported: {0}")]
    GitPackageNotSupported(String),
}

impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Error::Other(Cow::Borrowed(value))
    }
}
