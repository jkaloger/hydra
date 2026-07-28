pub mod graph;
pub mod model;
pub mod slug;
pub mod store;

pub use graph::{CAUTERISED, Cauterise, Cut, Sprout};
pub use model::{Answer, Head, Rejected, Status, Tree};
pub use store::Store;

use std::io;
use std::path::{Path, PathBuf};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One variant per failure the core lib can report, including one per SPEC §4
/// rejection. Offending slugs are named as fields rather than baked into a
/// message so the CLI can render them however it likes.
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

    #[error("no head '{slug}' in this tree")]
    UnknownHead { slug: String },

    /// §4.1. `parent: None` is the root and is never rejected.
    #[error("head '{slug}': parent '{parent}' does not exist")]
    UnknownParent { slug: String, parent: String },

    /// §4.2.
    #[error("head '{slug}': blocked_by '{blocked_by}' does not exist")]
    UnknownBlocker { slug: String, blocked_by: String },

    /// §4.3. Forceable.
    #[error("head '{slug}' blocked_by '{blocked_by}' would close a cycle: {}", path.join(" -> "))]
    BlockCycle {
        slug: String,
        blocked_by: String,
        /// The cycle the edge would close, from `slug` back round to `slug`.
        path: Vec<String>,
    },

    /// §4.4.
    #[error("head '{slug}' under '{parent}' would be its own ancestor: {}", path.join(" > "))]
    ParentCycle {
        slug: String,
        parent: String,
        /// The ancestry that would close the loop, from `slug` down to `parent`.
        path: Vec<String>,
    },

    /// §4.5. Forceable.
    #[error("head '{slug}' is blocked by unanswered {}", blockers.join(", "))]
    BlockedCut { slug: String, blockers: Vec<String> },

    /// §4.6.
    #[error("head '{slug}': illegal transition {from} -> {to}")]
    IllegalTransition {
        slug: String,
        from: Status,
        to: Status,
    },

    /// §4.7. Forceable.
    #[error("head '{slug}': cauterising head '{by}' is unanswered")]
    CauteriseByUnanswered { slug: String, by: String },

    /// §2 defines `cauterised_by` as the *sibling* answer that killed the
    /// question, so a head cannot be its own killer. Not forceable: `--force` is
    /// for "the dependency is unanswered and I know it doesn't matter", not for
    /// writing a `cauterised_by` no consumer can read.
    #[error("head '{slug}' cannot cauterise itself")]
    SelfCauterise { slug: String },
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
