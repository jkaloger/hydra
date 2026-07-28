pub mod model;
pub mod slug;
pub mod store;

pub use model::{Answer, Head, Rejected, Status, Tree};
pub use store::Store;

use std::io;
use std::path::{Path, PathBuf};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One variant per failure the core lib can report. SPEC §4's invariant
/// rejections join this enum in I2.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path}: malformed JSON: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("no .hydra directory in {start} or any parent")]
    NoStore { start: PathBuf },

    #[error("no active tree: .hydra/HEAD is missing")]
    HeadUnset,

    #[error("unknown tree '{slug}'")]
    UnknownTree { slug: String },

    #[error("tree '{slug}' already exists")]
    TreeExists { slug: String },

    #[error("{path}: tree format version {found}, expected {expected}")]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        expected: u32,
    },

    #[error("malformed slug '{slug}': expected ^[a-z0-9][a-z0-9-]*$")]
    MalformedSlug { slug: String },

    #[error("duplicate slug '{slug}'")]
    DuplicateSlug { slug: String },

    #[error("timed out waiting for the lock on {path}")]
    LockTimeout { path: PathBuf },
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>) -> impl FnOnce(io::Error) -> Error {
        let path = path.as_ref().to_path_buf();
        move |source| Error::Io { path, source }
    }

    pub(crate) fn json(path: impl AsRef<Path>) -> impl FnOnce(serde_json::Error) -> Error {
        let path = path.as_ref().to_path_buf();
        move |source| Error::Json { path, source }
    }
}
