//! The `.hydra/` directory: discovery, `HEAD`, and tree read/write (SPEC §3).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};
use tempfile::NamedTempFile;

use crate::model::{self, Tree};
use crate::{Error, Result, slug};

pub const DIR: &str = ".hydra";
pub const HEAD: &str = "HEAD";

/// §9 calls for a blocking lock with a short timeout: contention is a second
/// agent in the same repo, not a thundering herd.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(20);

/// Serialized form of a tree: pretty-printed, sorted keys, trailing newline.
///
/// Going via `Value` is what sorts the keys — `serde_json::Map` is a `BTreeMap`,
/// whereas serializing the struct directly would emit fields in declaration
/// order (§3, §9).
pub fn to_json(tree: &Tree) -> Result<String> {
    let name = format!("{}.json", tree.slug);
    let value = serde_json::to_value(tree).map_err(Error::json(&name))?;
    let mut json = serde_json::to_string_pretty(&value).map_err(Error::json(&name))?;
    json.push('\n');
    Ok(json)
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Walk up from `start` looking for a `.hydra/` directory.
    pub fn discover_from(start: &Path) -> Result<Store> {
        for ancestor in start.ancestors() {
            let dir = ancestor.join(DIR);
            if dir.is_dir() {
                return Ok(Store { dir });
            }
        }
        Err(Error::NoStore {
            start: start.to_path_buf(),
        })
    }

    pub fn discover() -> Result<Store> {
        let cwd = std::env::current_dir().map_err(Error::io("."))?;
        Store::discover_from(&cwd)
    }

    /// Create `<parent>/.hydra/`, or adopt it if it already exists.
    pub fn init(parent: &Path) -> Result<Store> {
        let dir = parent.join(DIR);
        fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn tree_path(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.json"))
    }

    pub fn head_path(&self) -> PathBuf {
        self.dir.join(HEAD)
    }

    /// The sidecar the read-modify-write lock is taken on. Deliberately *not*
    /// the tree file: `save` replaces that by rename, and an flock is held on
    /// the inode, so a lock taken on the pre-rename inode excludes nobody once
    /// the rename lands. Session state, so it is gitignored.
    pub fn lock_path(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.lock"))
    }

    /// The active tree slug.
    pub fn head(&self) -> Result<String> {
        let path = self.head_path();
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::HeadUnset),
            Err(e) => return Err(Error::io(&path)(e)),
        };
        let slug = raw.trim();
        slug::validate(slug)?;
        Ok(slug.to_string())
    }

    pub fn set_head(&self, slug: &str) -> Result<()> {
        slug::validate(slug)?;
        self.write_atomic(&self.head_path(), format!("{slug}\n").as_bytes())
    }

    pub fn load(&self, slug: &str) -> Result<Tree> {
        slug::validate(slug)?;
        self.read_tree(&self.tree_path(slug), slug)
    }

    /// Crate-private: writing a tree outside `with_tree_mut` or `create` is an
    /// unlocked read-modify-write waiting to happen (§9).
    pub(crate) fn save(&self, tree: &Tree) -> Result<()> {
        slug::validate(&tree.slug)?;
        let json = to_json(tree)?;
        self.write_atomic(&self.tree_path(&tree.slug), json.as_bytes())
    }

    /// The only way to bring a tree into existence. Locked, so two concurrent
    /// `hydra init` runs cannot both believe they won.
    pub fn create(&self, slug: &str) -> Result<Tree> {
        slug::validate(slug)?;
        let _lock = lock_exclusive(&self.lock_path(slug), LOCK_TIMEOUT)?;
        if self.tree_path(slug).exists() {
            return Err(Error::TreeExists {
                slug: slug.to_string(),
            });
        }
        let tree = Tree::new(slug.to_string());
        self.save(&tree)?;
        Ok(tree)
    }

    /// Load, mutate, save, all under an exclusive lock (§9). This is the only
    /// public write path for an existing tree — `save` is crate-private
    /// precisely so that `load` → mutate → `save` is not reachable from
    /// outside, because that sequence is unlocked.
    pub fn with_tree_mut<T>(
        &self,
        slug: &str,
        f: impl FnOnce(&mut Tree) -> Result<T>,
    ) -> Result<T> {
        slug::validate(slug)?;
        let _lock = lock_exclusive(&self.lock_path(slug), LOCK_TIMEOUT)?;
        // Read under the lock, never before it: the tree file we are about to
        // read may be replaced by another agent's `save` right up to the moment
        // the lock is granted.
        let mut tree = self.read_tree(&self.tree_path(slug), slug)?;
        let out = f(&mut tree)?;
        self.save(&tree)?;
        Ok(out)
    }

    fn read_tree(&self, path: &Path, slug: &str) -> Result<Tree> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::UnknownTree {
                    slug: slug.to_string(),
                });
            }
            Err(e) => return Err(Error::io(path)(e)),
        };
        let tree: Tree = serde_json::from_str(&raw).map_err(Error::json(path))?;
        // A tree written by a future hydra must not be silently rewritten in
        // this version's shape: the file is git-tracked and hand-editable.
        if tree.version != model::VERSION {
            return Err(Error::UnsupportedVersion {
                path: path.to_path_buf(),
                found: tree.version,
                expected: model::VERSION,
            });
        }
        Ok(tree)
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        // The temp file must live in the target's directory: `persist` is a
        // rename, and a rename across filesystems is not atomic (§9).
        let mut tmp = NamedTempFile::new_in(&self.dir).map_err(Error::io(&self.dir))?;
        tmp.write_all(bytes).map_err(Error::io(tmp.path()))?;
        tmp.as_file().sync_all().map_err(Error::io(tmp.path()))?;
        tmp.persist(path).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e.error,
        })?;
        Ok(())
    }
}

/// Held for the read-modify-write span; releases on drop.
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock_exclusive(path: &Path, timeout: Duration) -> Result<Lock> {
    // `create` without `truncate`: the sidecar's contents are never read, but
    // its inode has to survive, since that is what the flock lives on.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Error::io(path))?;

    let deadline = Instant::now() + timeout;
    loop {
        // Fully qualified: `File` has inherent `try_lock`/`unlock` of its own
        // since Rust 1.89, and §9 asks for fs4.
        match FileExt::try_lock(&file) {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_POLL)
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Error::LockTimeout {
                    path: path.to_path_buf(),
                });
            }
            Err(TryLockError::Error(e)) => return Err(Error::io(path)(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Head, Status};
    use std::sync::mpsc;
    use std::thread;
    use tempfile::TempDir;
    use ulid::Ulid;

    fn store() -> (TempDir, Store) {
        let root = TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        (root, store)
    }

    fn tree(slug: &str) -> Tree {
        Tree::new(slug.to_string())
    }

    fn head(slug: &str, seq: u32) -> Head {
        Head {
            id: Ulid::generate(),
            slug: slug.to_string(),
            question: format!("question for {slug}?"),
            parent: None,
            seq,
            blocked_by: vec![],
            status: Status::Open,
            rev: 0,
            created_at: crate::model::now(),
            updated_at: crate::model::now(),
            answer: None,
            prior: None,
        }
    }

    #[test]
    fn discovery_walks_up() {
        let (root, store) = store();
        let nested = root.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            Store::discover_from(&nested).unwrap().dir(),
            store.dir(),
            "should find .hydra from a nested cwd"
        );
    }

    #[test]
    fn missing_store_is_a_typed_error() {
        let empty = TempDir::new().unwrap();
        match Store::discover_from(empty.path()) {
            Err(Error::NoStore { start }) => assert_eq!(start, empty.path()),
            other => panic!("expected NoStore, got {other:?}"),
        }
    }

    #[test]
    fn head_round_trips() {
        let (_root, store) = store();
        assert!(matches!(store.head(), Err(Error::HeadUnset)));
        store.set_head("hydra-design").unwrap();
        assert_eq!(store.head().unwrap(), "hydra-design");
        store.set_head("other-tree").unwrap();
        assert_eq!(store.head().unwrap(), "other-tree");
    }

    #[test]
    fn head_rejects_malformed_slug() {
        let (_root, store) = store();
        assert!(matches!(
            store.set_head("Not A Slug"),
            Err(Error::MalformedSlug { .. })
        ));
        fs::write(store.head_path(), "Not A Slug\n").unwrap();
        assert!(matches!(store.head(), Err(Error::MalformedSlug { .. })));
    }

    #[test]
    fn tree_round_trips() {
        let (_root, store) = store();
        let mut t = tree("hydra-design");
        t.heads
            .insert("graph-shape".to_string(), head("graph-shape", 1));
        store.save(&t).unwrap();
        assert_eq!(store.load("hydra-design").unwrap(), t);
    }

    #[test]
    fn saved_file_is_complete_and_leaves_no_temp_files() {
        let (_root, store) = store();
        let mut t = store.create("hydra-design").unwrap();
        for (i, slug) in ["graph-shape", "storage-format"].iter().enumerate() {
            t.heads.insert(slug.to_string(), head(slug, i as u32 + 1));
        }
        store.save(&t).unwrap();

        let mut entries: Vec<String> = fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                "hydra-design.json".to_string(),
                "hydra-design.lock".to_string()
            ],
            "the tree and its lock sidecar, and no temp file"
        );

        let raw = fs::read_to_string(store.tree_path("hydra-design")).unwrap();
        assert_eq!(raw, to_json(&t).unwrap());
        assert!(raw.ends_with("}\n"));
        assert!(raw.contains("\n  \"heads\": {"), "should be pretty-printed");
    }

    #[test]
    fn create_refuses_to_clobber() {
        let (_root, store) = store();
        store.create("hydra-design").unwrap();
        assert!(matches!(
            store.create("hydra-design"),
            Err(Error::TreeExists { slug }) if slug == "hydra-design"
        ));
    }

    #[test]
    fn future_version_is_a_typed_error() {
        let (_root, store) = store();
        let mut value: serde_json::Value =
            serde_json::from_str(&to_json(&tree("hydra-design")).unwrap()).unwrap();
        value["version"] = serde_json::json!(2);
        fs::write(
            store.tree_path("hydra-design"),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();

        match store.load("hydra-design") {
            Err(Error::UnsupportedVersion {
                found, expected, ..
            }) => {
                assert_eq!((found, expected), (2, crate::model::VERSION));
            }
            other => panic!("expected UnsupportedVersion, got {:?}", other.map(|_| ())),
        }
        assert!(store.with_tree_mut("hydra-design", |_| Ok(())).is_err());
        let raw = fs::read_to_string(store.tree_path("hydra-design")).unwrap();
        assert!(
            raw.contains(r#""version":2"#),
            "a v2 tree must not be rewritten in v1 shape: {raw}"
        );
    }

    #[test]
    fn load_of_unknown_tree_is_a_typed_error() {
        let (_root, store) = store();
        assert!(matches!(
            store.load("nope"),
            Err(Error::UnknownTree { slug }) if slug == "nope"
        ));
        assert!(matches!(
            store.with_tree_mut("nope", |_| Ok(())),
            Err(Error::UnknownTree { .. })
        ));
    }

    #[test]
    fn malformed_json_is_a_typed_error() {
        let (_root, store) = store();
        fs::write(store.tree_path("broken"), "{ not json").unwrap();
        assert!(matches!(store.load("broken"), Err(Error::Json { .. })));
    }

    #[test]
    fn with_tree_mut_persists_and_releases_the_lock() {
        let (_root, store) = store();
        store.create("hydra-design").unwrap();

        for slug in ["graph-shape", "storage-format"] {
            store
                .with_tree_mut("hydra-design", |t| {
                    let seq = t.heads.len() as u32 + 1;
                    t.heads.insert(slug.to_string(), head(slug, seq));
                    Ok(())
                })
                .unwrap();
        }

        let loaded = store.load("hydra-design").unwrap();
        assert_eq!(loaded.heads.len(), 2);
        assert_eq!(loaded.heads["storage-format"].seq, 2);
    }

    #[test]
    fn with_tree_mut_does_not_save_when_the_closure_fails() {
        let (_root, store) = store();
        store.create("hydra-design").unwrap();
        let err = store.with_tree_mut("hydra-design", |t| {
            t.heads
                .insert("graph-shape".to_string(), head("graph-shape", 1));
            Err::<(), _>(Error::DuplicateSlug {
                slug: "graph-shape".to_string(),
            })
        });
        assert!(matches!(err, Err(Error::DuplicateSlug { .. })));
        assert!(store.load("hydra-design").unwrap().heads.is_empty());
    }

    #[test]
    fn contended_lock_times_out() {
        let (_root, store) = store();
        store.create("hydra-design").unwrap();
        let path = store.lock_path("hydra-design");

        let held = lock_exclusive(&path, LOCK_TIMEOUT).unwrap();
        match lock_exclusive(&path, Duration::from_millis(50)) {
            Err(Error::LockTimeout { path: p }) => assert_eq!(p, path),
            other => panic!("expected LockTimeout, got {:?}", other.map(|_| ())),
        }
        drop(held);
        lock_exclusive(&path, Duration::from_millis(50)).unwrap();
    }

    /// The lost-update shape §9 exists to prevent: agent A issues two
    /// operations back to back (`cut` then `sprout`) while agent B is mid
    /// mutation. Locking the tree file instead of the sidecar passes A's second
    /// operation straight through — B holds a lock on the inode A's first save
    /// renamed away — and B's write then clobbers it.
    #[test]
    fn concurrent_with_tree_mut_serialises() {
        let (_root, store) = store();
        store.create("hydra-design").unwrap();

        let (a_holding, a_has_lock) = mpsc::channel();
        let (b_holding, b_has_lock) = mpsc::channel();
        let b = {
            let store = store.clone();
            thread::spawn(move || {
                a_has_lock.recv().unwrap();
                store
                    .with_tree_mut("hydra-design", |t| {
                        b_holding.send(()).unwrap();
                        assert!(
                            t.heads.contains_key("a-first"),
                            "B waited for the lock, so it must see A's first write"
                        );
                        thread::sleep(Duration::from_millis(300));
                        t.heads.insert("b-head".to_string(), head("b-head", 3));
                        Ok(())
                    })
                    .unwrap();
            })
        };

        store
            .with_tree_mut("hydra-design", |t| {
                a_holding.send(()).unwrap();
                // Long enough for B to reach the lock and block on it.
                thread::sleep(Duration::from_millis(200));
                t.heads.insert("a-first".to_string(), head("a-first", 1));
                Ok(())
            })
            .unwrap();

        b_has_lock.recv().unwrap();
        let started = Instant::now();
        store
            .with_tree_mut("hydra-design", |t| {
                t.heads.insert("a-second".to_string(), head("a-second", 2));
                Ok(())
            })
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "A's second operation should have blocked until B released"
        );

        b.join().unwrap();
        let heads = store.load("hydra-design").unwrap().heads;
        assert_eq!(
            heads.keys().collect::<Vec<_>>(),
            vec!["a-first", "a-second", "b-head"],
            "no write may be lost"
        );
    }
}
