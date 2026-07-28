//! The session lease of SPEC §6: `.hydra/grill`.
//!
//! Every hook receives `session_id` on stdin and no-ops unless it matches, which
//! is what makes this a lease rather than a flag: a file left behind by a crashed
//! session can never match a new `session_id`, so stale state is inert. There is
//! deliberately no expiry, no liveness check and no staleness sweep here —
//! nothing stale can be acted on, so there is nothing to clean up.

use std::fs;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::model::now;
use crate::store::Store;
use crate::{Error, Result};

/// Where `hydra grill start` gets the `session_id` the hooks will later send it.
///
/// §5 gives the command no arguments and a shell invocation carries no hook
/// payload, so the id has to come from the environment. Claude Code exports this
/// variable into every command it runs — including the `Bash` tool call the skill
/// makes as its first act (§6) — and it holds the same id that arrives on stdin
/// as `session_id`.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// §6's shape exactly. Three fields: who is grilling, what, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub session_id: String,
    pub tree: String,
    pub started_at: Timestamp,
}

impl Lease {
    /// Whether `session_id` is the session this lease belongs to.
    ///
    /// An empty id matches nothing on either side: a payload that carries no
    /// `session_id` is not evidence that this session holds the lease, and a
    /// hand-written lease with no id must not be matched by one.
    pub fn holds(&self, session_id: &str) -> bool {
        !session_id.is_empty() && self.session_id == session_id
    }
}

/// The lease, or `None` when there is not one to act on.
///
/// A file that will not parse reads as no lease rather than as an error, for the
/// same reason staleness needs no handling: a lease that cannot be read cannot
/// match a `session_id`, so it is already inert. The callers are hooks whose
/// whole contract is to stay quiet (§6), and they have nothing to do with the
/// news either way.
pub fn read(store: &Store) -> Option<Lease> {
    let raw = fs::read_to_string(store.grill_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Take the lease for `session_id` on `tree`, displacing whatever was there.
///
/// A lease held by another session is overwritten rather than refused. There is
/// one lease slot; a live session holding it has moved on the moment this one
/// takes over, and a dead one left an inert file. Refusing would mean a crashed
/// session's leftovers block every later grill until something sweeps them up,
/// which is the crash recovery §6 exists to do without.
pub fn start(store: &Store, session_id: &str, tree: &str) -> Result<Lease> {
    // Re-running `grill start` inside one session must not lie about when the
    // grilling began.
    let started_at = match read(store) {
        Some(held) if held.holds(session_id) => held.started_at,
        _ => now(),
    };
    let lease = Lease {
        session_id: session_id.to_string(),
        tree: tree.to_string(),
        started_at,
    };

    let path = store.grill_path();
    // Serialized directly, not through `store::to_json`: that routes via `Value`
    // to sort the keys, which is a property §3 asks of the *stored document*.
    // This is gitignored session state, and §6 writes its fields in the order
    // they are declared. Atomic all the same — a hook that read a half-written
    // lease would silently decide it holds nothing.
    let mut json = serde_json::to_string_pretty(&lease).map_err(Error::json(&path))?;
    json.push('\n');
    store.write_atomic(&path, json.as_bytes())?;
    Ok(lease)
}

/// Release the lease. Returns what was released, `None` if there was nothing.
///
/// §6 makes this the kill switch, so it reads nothing it does not need: no
/// `HEAD`, no tree, no session id. A corrupt tree must not be able to stop a
/// grilling session from ending. Whose lease it is does not matter either — see
/// `start` on why there is only ever one to remove.
pub fn stop(store: &Store) -> Result<Option<Lease>> {
    let held = read(store);
    let path = store.grill_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(held),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(&path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let root = TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        (root, store)
    }

    #[test]
    fn lease_round_trips_in_the_shape_of_the_spec() {
        let (_root, store) = store();
        let written = start(&store, "abc123", "hydra-design").unwrap();
        assert_eq!(read(&store).as_ref(), Some(&written));

        let raw = fs::read_to_string(store.grill_path()).unwrap();
        assert_eq!(
            raw,
            format!(
                "{{\n  \"session_id\": \"abc123\",\n  \"tree\": \"hydra-design\",\n  \
                 \"started_at\": \"{}\"\n}}\n",
                written.started_at
            ),
            "§6's three fields, in §6's order"
        );
        assert!(store.grill_path().ends_with(".hydra/grill"));
    }

    #[test]
    fn a_lease_holds_only_for_its_own_session() {
        let lease = Lease {
            session_id: "abc123".to_string(),
            tree: "hydra-design".to_string(),
            started_at: now(),
        };
        assert!(lease.holds("abc123"));
        assert!(!lease.holds("def456"));
        assert!(
            !lease.holds(""),
            "a payload with no session_id is not evidence of anything"
        );

        let anonymous = Lease {
            session_id: String::new(),
            ..lease
        };
        assert!(
            !anonymous.holds(""),
            "and neither is a lease with no session_id"
        );
    }

    /// The whole reason §6 stores a `session_id` rather than a flag: a lease left
    /// by a session that died is inert without anything having to notice.
    #[test]
    fn a_stale_lease_needs_no_cleanup_to_be_inert() {
        let (_root, store) = store();
        start(&store, "crashed-session", "hydra-design").unwrap();
        assert!(!read(&store).unwrap().holds("a-new-session"));
    }

    #[test]
    fn an_absent_or_corrupt_lease_reads_as_no_lease() {
        let (_root, store) = store();
        assert_eq!(read(&store), None, "absent");

        for raw in ["{ not json", "{}", "[]", r#"{"session_id": 7}"#, ""] {
            fs::write(store.grill_path(), raw).unwrap();
            assert_eq!(read(&store), None, "corrupt: {raw}");
        }
    }

    /// Backdated by hand rather than by taking the lease twice: `model::now` has
    /// whole-second precision (§3), so two `start` calls a moment apart agree
    /// whatever the code between them does, and asserting on that pair would only
    /// assert that the code agrees with itself.
    #[test]
    fn restarting_the_same_session_keeps_the_original_start_time() {
        let (_root, store) = store();
        let old: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
        let backdated = Lease {
            session_id: "abc123".to_string(),
            tree: "hydra-design".to_string(),
            started_at: old,
        };
        fs::write(
            store.grill_path(),
            serde_json::to_string(&backdated).unwrap(),
        )
        .unwrap();

        let again = start(&store, "abc123", "hydra-design").unwrap();
        assert_eq!(again, backdated, "the same session, so the same lease");
        assert_eq!(read(&store).unwrap().started_at, old, "and on disk too");

        // Another session's start time is not this one's.
        assert!(start(&store, "def456", "hydra-design").unwrap().started_at > old);
    }

    #[test]
    fn a_foreign_lease_is_displaced_rather_than_refused() {
        let (_root, store) = store();
        start(&store, "crashed-session", "storage-format").unwrap();

        let taken = start(&store, "fresh-session", "hydra-design").unwrap();
        assert_eq!(taken.session_id, "fresh-session");
        assert_eq!(taken.tree, "hydra-design");
        assert_eq!(read(&store), Some(taken));
    }

    #[test]
    fn stop_reports_what_it_released_and_is_idempotent() {
        let (_root, store) = store();
        let lease = start(&store, "abc123", "hydra-design").unwrap();
        assert_eq!(stop(&store).unwrap(), Some(lease));
        assert!(!store.grill_path().exists());
        assert_eq!(stop(&store).unwrap(), None, "no lease is not a failure");
    }

    /// The kill switch has to work when everything else is broken, so it neither
    /// reads the tree nor insists the lease is its own.
    #[test]
    fn stop_removes_a_foreign_or_corrupt_lease_too() {
        let (_root, store) = store();
        start(&store, "another-session", "hydra-design").unwrap();
        assert_eq!(
            stop(&store).unwrap().map(|lease| lease.session_id),
            Some("another-session".to_string())
        );

        fs::write(store.grill_path(), "{ not json").unwrap();
        assert_eq!(
            stop(&store).unwrap(),
            None,
            "nothing that was a lease was released"
        );
        assert!(!store.grill_path().exists(), "and the file is gone anyway");
    }
}
