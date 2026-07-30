//! Mutations on an in-memory tree, gated by the invariants of SPEC §4.
//!
//! Every operation validates the whole write before touching a single field, so
//! a rejected operation leaves the tree byte-identical and `with_tree_mut` has
//! nothing to roll back.
//!
//! Walks are hand-written scans over `Tree::heads` (§9): the parent pointer plus
//! `seq` is the single source of truth (§3), so no reverse index of children or
//! dependents may be stored. That makes `dependents` a full scan per visited
//! head — quadratic in the head count, which is fine at the few-hundred-head
//! scale §9 sizes these walks for, and cheaper than an index that can desync.

use std::collections::{BTreeSet, VecDeque};

use ulid::Ulid;

use crate::model::{self, Answer, Head, Rejected, Status, Tree};
use crate::{Error, Result, slug as slugs};

/// The stored `answer.text` of a cauterised head (§2). Cauterisation is not a
/// state; `answer.cauterised_by` is the field to test, this is the human-facing
/// text that goes with it.
pub const CAUTERISED: &str = "cauterised";

/// Which edge kind an operation was adding, for the §4.9 rejection: the reopen
/// cascade runs over both kinds, so either can close a cycle in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Parent,
    BlockedBy,
}

impl std::fmt::Display for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Edge::Parent => "parent",
            Edge::BlockedBy => "blocked_by",
        })
    }
}

#[derive(Debug, Default)]
pub struct Sprout<'a> {
    pub question: &'a str,
    /// `None` is a root (§4.1).
    pub parent: Option<&'a str>,
    pub blocked_by: &'a [&'a str],
    /// Defaults to a slug derived from `question`.
    pub slug: Option<&'a str>,
}

#[derive(Debug, Default)]
pub struct Cut<'a> {
    pub slug: &'a str,
    pub answer: &'a str,
    pub rationale: Option<&'a str>,
    pub rejected: Vec<Rejected>,
    pub keep_subtree: bool,
    /// Overrides §4.5 only.
    pub force: bool,
}

#[derive(Debug, Default)]
pub struct Cauterise<'a> {
    pub slug: &'a str,
    /// The answered head whose answer killed this question.
    pub by: &'a str,
    /// Becomes `answer.rationale`.
    pub why: Option<&'a str>,
    /// Overrides §4.7 only.
    pub force: bool,
}

/// A new open head. Returns the slug it was filed under, which the caller may
/// not have chosen.
///
/// Takes no `force`: the only forceable rejection reachable here would be §4.3,
/// and a head this new cannot be in a cycle — its slug was unused a moment ago,
/// so nothing in the tree points at it and every `blocked_by` edge it declares
/// runs strictly away from it.
pub fn sprout(tree: &mut Tree, args: Sprout<'_>) -> Result<String> {
    let slug = match args.slug {
        Some(slug) => {
            slugs::validate(slug)?;
            slug.to_string()
        }
        None => derive_slug(tree, args.question)?,
    };
    if tree.heads.contains_key(&slug) {
        return Err(Error::DuplicateSlug { slug });
    }
    if let Some(parent) = args.parent
        && !tree.heads.contains_key(parent)
    {
        return Err(Error::UnknownParent {
            slug,
            parent: parent.to_string(),
        });
    }
    let blocked_by = resolve_blockers(tree, &slug, args.blocked_by)?;

    let now = model::now();
    let head = Head {
        id: Ulid::generate(),
        slug: slug.clone(),
        question: args.question.to_string(),
        parent: args.parent.map(str::to_string),
        seq: next_seq(tree, args.parent),
        blocked_by,
        status: Status::Open,
        rev: 0,
        created_at: now,
        updated_at: now,
        answer: None,
        prior: None,
    };
    tree.heads.insert(slug.clone(), head);
    Ok(slug)
}

/// Answer a head. Returns the heads a re-answer cascaded onto (§2), sorted;
/// empty for a first answer or under `keep_subtree`.
///
/// Cutting an already-answered head is a re-answer, not the illegal
/// `answered → answered` transition of §4.6: §2 requires re-answering to work
/// ("Re-answering a head transitively reopens its descendants") and builds
/// `rev`, `prior` and the cascade around it. §4.6 constrains *state changes*, and
/// a re-answer changes no state — the revision is what `rev` and `prior` record.
pub fn cut(tree: &mut Tree, args: Cut<'_>) -> Result<Vec<String>> {
    require_head(tree, args.slug)?;
    if !args.force {
        require_blockers_answered(tree, args.slug)?;
    }
    let answer = Answer {
        text: args.answer.to_string(),
        rationale: args.rationale.map(str::to_string),
        rejected: args.rejected,
        cauterised_by: None,
    };
    Ok(answer_head(tree, args.slug, answer, args.keep_subtree))
}

/// Kill a question a sibling's answer made moot (§2). Not a state: the head ends
/// up `answered`, with `answer.text = "cauterised"` and `cauterised_by` set.
///
/// §4.5 is deliberately not enforced here. It says *cutting*, and §5 makes `cut`
/// and `cauterise` separate verbs; requiring a cauterised head's gates to be
/// answered first would make a dead subtree unkillable, since those gates are
/// usually the heads about to be cauterised themselves.
pub fn cauterise(tree: &mut Tree, args: Cauterise<'_>) -> Result<Vec<String>> {
    require_head(tree, args.slug)?;
    let by = require_head(tree, args.by)?;
    if args.slug == args.by {
        return Err(Error::SelfCauterise {
            slug: args.slug.to_string(),
        });
    }
    if !args.force && by.status != Status::Answered {
        return Err(Error::CauteriseByUnanswered {
            slug: args.slug.to_string(),
            by: args.by.to_string(),
        });
    }
    let answer = Answer {
        text: CAUTERISED.to_string(),
        rationale: args.why.map(str::to_string),
        rejected: vec![],
        cauterised_by: Some(args.by.to_string()),
    };
    Ok(answer_head(tree, args.slug, answer, false))
}

/// Withdraw an answer. Always cascades: a withdrawn premise invalidates
/// dependents exactly as a changed one does, and §5 gives `reopen` no
/// `--keep-subtree` — that flag is `cut`'s, for typos and rewording.
pub fn reopen(tree: &mut Tree, slug: &str) -> Result<Vec<String>> {
    let head = require_head(tree, slug)?;
    if head.status != Status::Answered {
        return Err(Error::IllegalTransition {
            slug: slug.to_string(),
            from: head.status,
            to: Status::Open,
        });
    }
    let head = tree.heads.get_mut(slug).expect("checked above");
    // Reopening is not cauterising (§2): `cauterised_by` lives inside the answer
    // this moves to `prior`, so the current answer never carries it.
    head.reopen();
    Ok(cascade(tree, slug))
}

pub fn reword(tree: &mut Tree, slug: &str, question: &str) -> Result<()> {
    require_head(tree, slug)?;
    tree.heads
        .get_mut(slug)
        .expect("checked above")
        .set_question(question.to_string());
    Ok(())
}

/// Move a head to a new parent, or to the root with `None`. It lands last in its
/// new sibling set: `seq` orders siblings (§3) and the old value belongs to the
/// old set, where it may well collide with a sibling of the new one.
pub fn reparent(tree: &mut Tree, slug: &str, parent: Option<&str>) -> Result<()> {
    let head = require_head(tree, slug)?;
    if let Some(parent) = parent {
        if !tree.heads.contains_key(parent) {
            return Err(Error::UnknownParent {
                slug: slug.to_string(),
                parent: parent.to_string(),
            });
        }
        // §4.4, which subsumes `parent == slug`: a head is trivially its own
        // ancestor, so the walk finds the one-hop path without a special case.
        if let Some(path) = ancestry_from(tree, slug, parent) {
            return Err(Error::ParentCycle {
                slug: slug.to_string(),
                parent: parent.to_string(),
                path,
            });
        }
        // §4.9 by the other route: adopting a head that is already `blocked_by`
        // this one closes the same union cycle, no `blocked_by` write involved.
        // Absolute rather than forceable — §5 gives `reparent` no `--force`, so
        // there is no surface to expose one through, and `link --force` remains
        // the way to build the shape deliberately.
        if let Some(path) = reopen_path(tree, parent, slug) {
            return Err(Error::ReopenCycle {
                slug: slug.to_string(),
                edge: Edge::Parent,
                other: parent.to_string(),
                path: cycle_from(slug, path),
            });
        }
    }
    if head.parent.as_deref() == parent {
        return Ok(());
    }
    let seq = next_seq(tree, parent);
    tree.heads
        .get_mut(slug)
        .expect("checked above")
        .set_parent(parent.map(str::to_string), seq);
    Ok(())
}

/// Add a `blocked_by` edge. Idempotent: re-adding an existing edge is a no-op
/// rather than a rejection, since §4 lists no invariant against it.
pub fn link(tree: &mut Tree, slug: &str, blocked_by: &str, force: bool) -> Result<()> {
    let head = require_head(tree, slug)?;
    if !tree.heads.contains_key(blocked_by) {
        return Err(Error::UnknownBlocker {
            slug: slug.to_string(),
            blocked_by: blocked_by.to_string(),
        });
    }
    if head.blocked_by.iter().any(|b| b == blocked_by) {
        return Ok(());
    }
    // §4.3. `blocked_by == slug` is a one-hop cycle and is rejected by the same
    // walk: carving it out as its own non-forceable rejection would make
    // `--force` mean something different for self-edges, and the state it
    // produces — a head open forever — is reachable anyway by forcing a
    // two-head cycle.
    if !force {
        if let Some(path) = block_path(tree, blocked_by, slug) {
            return Err(Error::BlockCycle {
                slug: slug.to_string(),
                blocked_by: blocked_by.to_string(),
                path: cycle_from(slug, path),
            });
        }
        // §4.9. Gating a head on its own descendant closes a cycle across the
        // union of both edge kinds — the parent edge runs down to the blocker and
        // the blocked_by edge runs back up — which §4.3's `blocked_by`-only walk
        // cannot see. The converse, gating a head on an *ancestor*, is the benign
        // direction and stays legal: both edges then push staleness the same way.
        if let Some(path) = reopen_path(tree, blocked_by, slug) {
            return Err(Error::ReopenCycle {
                slug: slug.to_string(),
                edge: Edge::BlockedBy,
                other: blocked_by.to_string(),
                path: cycle_from(slug, path),
            });
        }
    }
    let mut edges = head.blocked_by.clone();
    edges.push(blocked_by.to_string());
    tree.heads
        .get_mut(slug)
        .expect("checked above")
        .set_blocked_by(edges);
    Ok(())
}

/// Remove a `blocked_by` edge. Idempotent, and deliberately does not require the
/// blocker to exist: dropping an edge can strand nothing, and this is the repair
/// path for a hand-edited tree that violates §4.2.
pub fn unlink(tree: &mut Tree, slug: &str, blocked_by: &str) -> Result<()> {
    let head = require_head(tree, slug)?;
    if !head.blocked_by.iter().any(|b| b == blocked_by) {
        return Ok(());
    }
    let edges = head
        .blocked_by
        .iter()
        .filter(|b| *b != blocked_by)
        .cloned()
        .collect();
    tree.heads
        .get_mut(slug)
        .expect("checked above")
        .set_blocked_by(edges);
    Ok(())
}

fn answer_head(tree: &mut Tree, slug: &str, answer: Answer, keep_subtree: bool) -> Vec<String> {
    let head = tree.heads.get_mut(slug).expect("caller checked");
    // A `prior` means this head has been answered before, so answering it again
    // is a revision whatever its current status — reading the status alone would
    // classify a cut after a reopen as a first answer and skip the cascade,
    // leaving descendants answered on a premise that moved. A genuine first
    // answer has no dependents to cascade onto, so the wider test costs nothing.
    let reanswer = head.status == Status::Answered || head.prior.is_some();
    head.set_answer(answer);
    if reanswer && !keep_subtree {
        cascade(tree, slug)
    } else {
        vec![]
    }
}

/// Transitively reopen everything standing on `root`'s answer (§2).
///
/// One walk over the union of both edge kinds, not two. A head `blocked_by` the
/// revised head is as stale as a child of it, and the two kinds compose: the
/// child of a head that is `blocked_by` `root` is reachable only by alternating
/// them, so running a descendant walk and a `blocked_by` walk separately would
/// miss exactly those mixed chains.
///
/// `cauterised_by` is deliberately not a third edge kind: §2 defines the closure
/// as descendants ∪ `blocked_by`, and re-answering the head that killed a
/// question leaves the question dead. §2's own example has the killer as the
/// cauterised head's parent, so the tree edge already covers the case that
/// matters.
///
/// Returns the heads it reopened, sorted.
fn cascade(tree: &mut Tree, root: &str) -> Vec<String> {
    // Seeding `seen` with the root keeps a cycle from reopening the answer we
    // just wrote, and makes the walk terminate on the cycles `--force` can leave
    // behind even though §4.3 rejects them at write. It also collapses diamonds:
    // a head reachable by several paths is visited, and so reopened, once.
    let mut seen = BTreeSet::from([root.to_string()]);
    let mut queue = VecDeque::from([root.to_string()]);
    let mut reopened = Vec::new();
    while let Some(at) = queue.pop_front() {
        for slug in dependents(tree, &at) {
            if !seen.insert(slug.clone()) {
                continue;
            }
            // An already-open dependent needs no reopening but is still walked
            // through: staleness passes through it to whatever it gates.
            if let Some(head) = tree.heads.get_mut(&slug)
                && head.reopen()
            {
                reopened.push(slug.clone());
            }
            queue.push_back(slug);
        }
    }
    reopened.sort();
    reopened
}

/// Heads that stand on `slug`: its children and everything it blocks. Keyed by
/// the map key rather than `Head::slug`, which the rest of the file also treats
/// as authoritative — a hand-edited tree where the two diverge must not have
/// heads quietly fall out of the cascade.
fn dependents(tree: &Tree, slug: &str) -> Vec<String> {
    tree.heads
        .iter()
        .filter(|(_, head)| {
            head.parent.as_deref() == Some(slug) || head.blocked_by.iter().any(|b| b == slug)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

fn require_head<'t>(tree: &'t Tree, slug: &str) -> Result<&'t Head> {
    tree.heads.get(slug).ok_or_else(|| Error::UnknownHead {
        slug: slug.to_string(),
    })
}

/// §4.5. A `blocked_by` entry with no head counts as unanswered rather than
/// being ignored: only a hand edit can produce one, and answering over it would
/// bless the corruption.
fn require_blockers_answered(tree: &Tree, slug: &str) -> Result<()> {
    let blockers: Vec<String> = tree.heads[slug]
        .blocked_by
        .iter()
        .filter(|b| tree.heads.get(b.as_str()).map(|h| h.status) != Some(Status::Answered))
        .cloned()
        .collect();
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(Error::BlockedCut {
            slug: slug.to_string(),
            blockers,
        })
    }
}

fn resolve_blockers(tree: &Tree, slug: &str, blocked_by: &[&str]) -> Result<Vec<String>> {
    let mut edges = Vec::with_capacity(blocked_by.len());
    for blocker in blocked_by {
        if !tree.heads.contains_key(*blocker) {
            return Err(Error::UnknownBlocker {
                slug: slug.to_string(),
                blocked_by: (*blocker).to_string(),
            });
        }
        edges.push((*blocker).to_string());
    }
    // Canonicalised as in `Head::set_blocked_by`, which a fresh head bypasses.
    edges.sort();
    edges.dedup();
    Ok(edges)
}

fn next_seq(tree: &Tree, parent: Option<&str>) -> u32 {
    tree.heads
        .values()
        .filter(|head| head.parent.as_deref() == parent)
        .map(|head| head.seq)
        .max()
        .map_or(1, |highest| highest + 1)
}

/// A derived slug is the mechanical `slugify` of the question, suffixed to dodge
/// a collision. Hydra reads no question text (§1), so it does not shorten or
/// summarise, and a question that slugifies to nothing is rejected rather than
/// given an invented name — `--slug` is the answer for those.
fn derive_slug(tree: &Tree, question: &str) -> Result<String> {
    let base = slugs::slugify(question);
    slugs::validate(&base)?;
    if !tree.heads.contains_key(&base) {
        return Ok(base);
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !tree.heads.contains_key(candidate))
        .ok_or(Error::DuplicateSlug { slug: base })
}

/// The chain from `ancestor` down to `of`, inclusive, if `ancestor` is on `of`'s
/// ancestry (which it is trivially when they are the same head).
fn ancestry_from(tree: &Tree, ancestor: &str, of: &str) -> Option<Vec<String>> {
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    let mut at = Some(of.to_string());
    while let Some(slug) = at {
        if !seen.insert(slug.clone()) {
            // Only a hand edit can build a parent cycle; refuse to spin on it.
            return None;
        }
        path.push(slug.clone());
        if slug == ancestor {
            path.reverse();
            return Some(path);
        }
        at = tree.heads.get(&slug).and_then(|head| head.parent.clone());
    }
    None
}

/// A cycle for an error message. The walk already ends at `slug`, so prefixing it
/// closes the loop: `slug -> <refused edge> -> ... -> slug`.
fn cycle_from(slug: &str, path: Vec<String>) -> Vec<String> {
    std::iter::once(slug.to_string()).chain(path).collect()
}

/// A path of `blocked_by` edges from `from` to `to`, inclusive.
fn block_path(tree: &Tree, from: &str, to: &str) -> Option<Vec<String>> {
    walk_premises(tree, from, to, false)
}

/// The same walk with the parent pointer treated as an edge too, which makes it
/// the reverse of the relation `cascade` runs over: `X` stands on its blockers
/// *and* on its parent, because re-answering a parent reopens its children.
///
/// A path from `from` to `to` here means an edge `to → from` would close a cycle
/// in the reopen cascade (§4.9).
fn reopen_path(tree: &Tree, from: &str, to: &str) -> Option<Vec<String>> {
    walk_premises(tree, from, to, true)
}

fn walk_premises(tree: &Tree, from: &str, to: &str, via_parent: bool) -> Option<Vec<String>> {
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    walk(tree, from, to, via_parent, &mut seen, &mut path).then_some(path)
}

fn walk(
    tree: &Tree,
    at: &str,
    to: &str,
    via_parent: bool,
    seen: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> bool {
    if !seen.insert(at.to_string()) {
        return false;
    }
    path.push(at.to_string());
    if at == to {
        return true;
    }
    if let Some(head) = tree.heads.get(at) {
        let parent = head.parent.iter().filter(|_| via_parent);
        for next in head.blocked_by.iter().chain(parent) {
            if walk(tree, next, to, via_parent, seen, path) {
                return true;
            }
        }
    }
    path.pop();
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    fn tree() -> Tree {
        Tree::new("t".to_string(), "test intent".to_string())
    }

    fn add(tree: &mut Tree, slug: &str, parent: Option<&str>) {
        sprout(
            tree,
            Sprout {
                question: "q?",
                parent,
                slug: Some(slug),
                ..Sprout::default()
            },
        )
        .unwrap();
    }

    /// A chain of heads, each the child of the last, all answered.
    fn answered_chain(tree: &mut Tree, slugs: &[&str]) {
        let mut parent = None;
        for slug in slugs {
            add(tree, slug, parent);
            answer(tree, slug);
            parent = Some(slug);
        }
    }

    fn answer(tree: &mut Tree, slug: &str) {
        cut(
            tree,
            Cut {
                slug,
                answer: "because",
                ..Cut::default()
            },
        )
        .unwrap();
    }

    fn reanswer(tree: &mut Tree, slug: &str) -> Vec<String> {
        cut(
            tree,
            Cut {
                slug,
                answer: "actually, because",
                ..Cut::default()
            },
        )
        .unwrap()
    }

    /// Runs an operation that must be rejected and asserts the tree came out
    /// byte-identical, which is the property §4 needs: `with_tree_mut` saves
    /// whatever the closure left behind if it returns `Ok`.
    fn rejects<T: std::fmt::Debug>(
        tree: &mut Tree,
        op: impl FnOnce(&mut Tree) -> Result<T>,
    ) -> Error {
        let before = store::to_json(tree).unwrap();
        let err = op(tree).expect_err("should have been rejected");
        assert_eq!(
            store::to_json(tree).unwrap(),
            before,
            "a rejected operation must not mutate the tree"
        );
        err
    }

    #[test]
    fn sprout_sets_the_defaults() {
        let mut t = tree();
        add(&mut t, "root", None);
        let slug = sprout(
            &mut t,
            Sprout {
                question: "Strict tree or DAG?",
                parent: Some("root"),
                blocked_by: &["root"],
                slug: None,
            },
        )
        .unwrap();

        assert_eq!(slug, "strict-tree-or-dag");
        let head = &t.heads[&slug];
        assert_eq!(head.question, "Strict tree or DAG?");
        assert_eq!(head.parent.as_deref(), Some("root"));
        assert_eq!(head.blocked_by, vec!["root".to_string()]);
        assert_eq!(head.status, Status::Open);
        assert_eq!(head.rev, 0);
        assert_eq!(head.answer, None);
        assert_eq!(head.prior, None);
        assert_eq!(head.created_at, head.updated_at);
        assert_eq!(head.slug, slug, "the map key and the field agree");
        assert_ne!(head.id, Ulid::nil());
    }

    #[test]
    fn sprout_seq_is_per_sibling_set() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        add(&mut t, "a1", Some("a"));
        add(&mut t, "a2", Some("a"));
        add(&mut t, "b1", Some("b"));

        assert_eq!(t.heads["a"].seq, 1);
        assert_eq!(t.heads["b"].seq, 2);
        assert_eq!(t.heads["a1"].seq, 1);
        assert_eq!(t.heads["a2"].seq, 2);
        assert_eq!(t.heads["b1"].seq, 1, "a fresh sibling set restarts at 1");
    }

    #[test]
    fn reparent_re_seats_a_head_in_its_new_sibling_set() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        add(&mut t, "a1", Some("a"));
        add(&mut t, "a2", Some("a"));
        add(&mut t, "b1", Some("b"));

        reparent(&mut t, "a2", Some("b")).unwrap();
        assert_eq!(t.heads["a2"].seq, 2, "last in its new sibling set");

        add(&mut t, "a3", Some("a"));
        assert_eq!(
            t.heads["a3"].seq, 2,
            "seq is only unique within the current sibling set, so a departed head's value is free"
        );
        add(&mut t, "b2", Some("b"));
        assert_eq!(t.heads["b2"].seq, 3, "behind the head that moved in");
    }

    #[test]
    fn sprout_disambiguates_a_derived_slug() {
        let mut t = tree();
        for expected in ["how-many-heads", "how-many-heads-2", "how-many-heads-3"] {
            let slug = sprout(
                &mut t,
                Sprout {
                    question: "How many heads?",
                    ..Sprout::default()
                },
            )
            .unwrap();
            assert_eq!(slug, expected);
        }
        assert_eq!(t.heads.len(), 3);
    }

    #[test]
    fn sprout_rejects_a_question_that_slugifies_to_nothing() {
        let mut t = tree();
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "???",
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::MalformedSlug { slug } if slug.is_empty()));
    }

    #[test]
    fn sprout_rejects_a_duplicate_slug() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "different question?",
                    slug: Some("a"),
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::DuplicateSlug { slug } if slug == "a"));
    }

    #[test]
    fn sprout_rejects_a_malformed_slug() {
        let mut t = tree();
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "q?",
                    slug: Some("Not A Slug"),
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::MalformedSlug { slug } if slug == "Not A Slug"));
    }

    #[test]
    fn sprout_rejects_an_unknown_parent() {
        let mut t = tree();
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "q?",
                    parent: Some("ghost"),
                    slug: Some("a"),
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownParent { slug, parent }
            if slug == "a" && parent == "ghost"));
    }

    #[test]
    fn sprout_rejects_being_its_own_parent() {
        let mut t = tree();
        // The head does not exist yet, so self-parenting is just an unknown
        // parent; no special case needed.
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "q?",
                    parent: Some("a"),
                    slug: Some("a"),
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownParent { slug, parent }
            if slug == "a" && parent == "a"));
    }

    #[test]
    fn sprout_rejects_an_unknown_blocker() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| {
            sprout(
                t,
                Sprout {
                    question: "q?",
                    blocked_by: &["a", "ghost"],
                    slug: Some("b"),
                    ..Sprout::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownBlocker { slug, blocked_by }
            if slug == "b" && blocked_by == "ghost"));
    }

    #[test]
    fn sprout_canonicalises_its_blockers() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "a", "b", false).unwrap();
        // Nothing can point at a head that does not exist yet, so every edge a
        // sprout declares runs away from it. This is why `Sprout` has no `force`.
        sprout(
            &mut t,
            Sprout {
                question: "q?",
                blocked_by: &["a", "b", "b"],
                slug: Some("c"),
                ..Sprout::default()
            },
        )
        .unwrap();
        assert_eq!(
            t.heads["c"].blocked_by,
            vec!["a".to_string(), "b".to_string()],
            "sorted and deduplicated"
        );
    }

    #[test]
    fn cut_answers_without_cascading() {
        let mut t = tree();
        answered_chain(&mut t, &["root"]);
        add(&mut t, "child", Some("root"));

        let cascaded = cut(
            &mut t,
            Cut {
                slug: "child",
                answer: "spanning tree\nand cross edges",
                rationale: Some("legible"),
                rejected: vec![Rejected {
                    option: "pure DAG".to_string(),
                    why_not: "nondeterministic".to_string(),
                }],
                ..Cut::default()
            },
        )
        .unwrap();
        assert!(cascaded.is_empty(), "a first answer cascades onto nothing");

        let head = &t.heads["child"];
        assert_eq!(head.status, Status::Answered);
        assert_eq!(head.rev, 1);
        assert_eq!(head.prior, None);
        let answer = head.answer.as_ref().unwrap();
        assert_eq!(answer.text, "spanning tree\nand cross edges");
        assert_eq!(answer.rationale.as_deref(), Some("legible"));
        assert_eq!(answer.rejected.len(), 1);
        assert_eq!(answer.cauterised_by, None);
    }

    #[test]
    fn cut_rejects_unanswered_blockers() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        add(&mut t, "c", None);
        answer(&mut t, "c");
        link(&mut t, "a", "b", false).unwrap();
        link(&mut t, "a", "c", false).unwrap();

        let err = rejects(&mut t, |t| {
            cut(
                t,
                Cut {
                    slug: "a",
                    answer: "x",
                    ..Cut::default()
                },
            )
        });
        assert!(matches!(err, Error::BlockedCut { slug, blockers }
            if slug == "a" && blockers == vec!["b".to_string()]));

        // A `blocked_by` entry with no head — only a hand edit can make one —
        // counts as unanswered rather than being skipped, so answering over it
        // cannot bless the corruption.
        t.heads
            .get_mut("a")
            .unwrap()
            .set_blocked_by(vec!["ghost".to_string()]);
        let err = rejects(&mut t, |t| {
            cut(
                t,
                Cut {
                    slug: "a",
                    answer: "x",
                    ..Cut::default()
                },
            )
        });
        assert!(matches!(err, Error::BlockedCut { slug, blockers }
            if slug == "a" && blockers == vec!["ghost".to_string()]));
    }

    #[test]
    fn force_cuts_through_unanswered_blockers_and_records_nothing() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "a", "b", false).unwrap();

        cut(
            &mut t,
            Cut {
                slug: "a",
                answer: "x",
                force: true,
                ..Cut::default()
            },
        )
        .unwrap();

        let head = &t.heads["a"];
        let answer = head.answer.as_ref().unwrap();
        assert_eq!(answer.text, "x", "force leaves no trace in the answer");
        assert_eq!(answer.rationale, None);
        assert!(answer.rejected.is_empty());
        assert_eq!(answer.cauterised_by, None);
        assert_eq!(
            head.blocked_by,
            vec!["b".to_string()],
            "the edge it was forced past survives"
        );
        assert_eq!(t.heads["b"].status, Status::Open);
    }

    #[test]
    fn cut_rejects_an_unknown_head() {
        let mut t = tree();
        let err = rejects(&mut t, |t| {
            cut(
                t,
                Cut {
                    slug: "ghost",
                    answer: "x",
                    ..Cut::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }

    #[test]
    fn reanswer_cascades_over_both_edge_kinds() {
        let mut t = tree();
        // root ─ kid ─ grandkid          elsewhere
        //                                └ gated ─ gated-kid
        //          ▲───(blocked_by)──────┘
        //
        // `gated` and `gated-kid` sit outside root's subtree, so the only route
        // to them is the cross edge, then a tree edge. Delete the `blocked_by`
        // clause from `dependents` and this test must fail.
        answered_chain(&mut t, &["root", "kid", "grandkid"]);
        add(&mut t, "elsewhere", None);
        answer(&mut t, "elsewhere");
        add(&mut t, "gated", Some("elsewhere"));
        link(&mut t, "gated", "kid", false).unwrap();
        answer(&mut t, "gated");
        add(&mut t, "gated-kid", Some("gated"));
        answer(&mut t, "gated-kid");

        let cascaded = reanswer(&mut t, "root");
        assert_eq!(
            cascaded,
            vec!["gated", "gated-kid", "grandkid", "kid"],
            "the child of a head blocked by root is only reachable by alternating edge kinds"
        );
        assert_eq!(t.heads["root"].status, Status::Answered);
        assert_eq!(
            t.heads["elsewhere"].status,
            Status::Answered,
            "gated's parent stands on nothing root said"
        );
        for slug in cascaded {
            assert_eq!(t.heads[&slug].status, Status::Open, "{slug}");
        }
    }

    #[test]
    fn cascade_reopens_a_diamond_once() {
        let mut t = tree();
        // `join` hangs off `elsewhere`, so the two `blocked_by` edges are its
        // only routes from root and neither may reopen it twice.
        answered_chain(&mut t, &["root"]);
        add(&mut t, "elsewhere", None);
        answer(&mut t, "elsewhere");
        for slug in ["left", "right"] {
            add(&mut t, slug, Some("root"));
            answer(&mut t, slug);
        }
        add(&mut t, "join", Some("elsewhere"));
        link(&mut t, "join", "left", false).unwrap();
        link(&mut t, "join", "right", false).unwrap();
        answer(&mut t, "join");

        let cascaded = reanswer(&mut t, "root");
        assert_eq!(
            cascaded,
            vec!["join", "left", "right"],
            "join is reachable by two blocked_by paths and appears once"
        );
    }

    #[test]
    fn cascade_walks_through_an_open_head() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "middle", "leaf"]);
        reopen(&mut t, "middle").unwrap();
        answer(&mut t, "leaf");

        let cascaded = reanswer(&mut t, "root");
        assert_eq!(
            cascaded,
            vec!["leaf"],
            "an already-open head is not reported, but staleness passes through it"
        );
        assert_eq!(t.heads["leaf"].status, Status::Open);
    }

    #[test]
    fn cascade_terminates_on_a_forced_cycle() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "b", "a", false).unwrap();
        link(&mut t, "a", "b", true).unwrap();
        // Both heads block each other, so every cut from here on needs §4.5
        // forced too — which is the point: the tree can hold a cycle.
        for slug in ["b", "a"] {
            cut(
                &mut t,
                Cut {
                    slug,
                    answer: "first",
                    force: true,
                    ..Cut::default()
                },
            )
            .unwrap();
        }

        let cascaded = cut(
            &mut t,
            Cut {
                slug: "a",
                answer: "second",
                force: true,
                ..Cut::default()
            },
        )
        .unwrap();
        assert_eq!(cascaded, vec!["b".to_string()]);
        assert_eq!(
            t.heads["a"].status,
            Status::Answered,
            "the cycle must not reopen the answer that started the cascade"
        );
    }

    #[test]
    fn keep_subtree_skips_the_cascade_on_cut() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);

        let cascaded = cut(
            &mut t,
            Cut {
                slug: "root",
                answer: "typo fixed",
                keep_subtree: true,
                ..Cut::default()
            },
        )
        .unwrap();
        assert!(cascaded.is_empty());
        assert_eq!(t.heads["kid"].status, Status::Answered);
    }

    /// The counterpart to `keep_subtree_skips_the_cascade_on_cut`: `reopen` has
    /// no such escape hatch, because §5 gives it no flag.
    #[test]
    fn reopen_always_cascades() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);
        assert_eq!(reopen(&mut t, "root").unwrap(), vec!["kid"]);
        assert_eq!(t.heads["kid"].status, Status::Open);
    }

    #[test]
    fn cut_after_a_reopen_is_a_re_answer() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);
        reopen(&mut t, "root").unwrap();
        answer(&mut t, "kid");

        // The status says `open`, but the `prior` says this head has been
        // answered before, so the answer that follows is a revision and its
        // descendants cannot be left standing on the answer it replaces.
        let cascaded = cut(
            &mut t,
            Cut {
                slug: "root",
                answer: "a totally different answer",
                ..Cut::default()
            },
        )
        .unwrap();
        assert_eq!(cascaded, vec!["kid"]);
        assert_eq!(t.heads["kid"].status, Status::Open);
        assert_eq!(t.heads["root"].rev, 2);
    }

    #[test]
    fn reopen_keeps_prior() {
        let mut t = tree();
        answered_chain(&mut t, &["root"]);
        reopen(&mut t, "root").unwrap();

        let head = &t.heads["root"];
        assert_eq!(head.status, Status::Open);
        assert_eq!(head.answer, None);
        assert_eq!(
            head.prior.as_ref().map(|a| a.text.as_str()),
            Some("because"),
            "§2: a reopened head keeps its prior answer for context"
        );
        assert_eq!(head.rev, 1, "rev counts answers given, not withdrawals");
    }

    /// §2: reopened means *ask it again*, cauterised means *a sibling's answer
    /// killed this*. Reopening a cauterised head returns it to the frontier
    /// without hiding why it was killed, and without the current answer ever
    /// carrying a `cauterised_by`.
    #[test]
    fn reopen_of_a_cauterised_head_is_not_a_cauterise() {
        let mut t = tree();
        add(&mut t, "by", None);
        answer(&mut t, "by");
        add(&mut t, "dead", None);
        cauterise(
            &mut t,
            Cauterise {
                slug: "dead",
                by: "by",
                ..Cauterise::default()
            },
        )
        .unwrap();

        reopen(&mut t, "dead").unwrap();
        let head = &t.heads["dead"];
        assert_eq!(head.status, Status::Open);
        assert_eq!(head.answer, None, "no answer, so nothing cauterised");
        let prior = head.prior.as_ref().unwrap();
        assert_eq!(prior.text, CAUTERISED);
        assert_eq!(
            prior.cauterised_by.as_deref(),
            Some("by"),
            "the kill record survives in prior"
        );
        assert_eq!(head.rev, 1);
    }

    /// `graph::reopen` admits a head on its `status`, so `Head::reopen` has to
    /// agree: keying off `answer` alone would let a hand-edited half-answered
    /// head pass the transition check, run a cascade, and stay `answered`.
    #[test]
    fn reopen_normalises_a_half_answered_head() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);
        t.heads.get_mut("root").unwrap().answer = None;

        assert_eq!(reopen(&mut t, "root").unwrap(), vec!["kid"]);
        assert_eq!(t.heads["root"].status, Status::Open);
    }

    #[test]
    fn reopen_rejects_an_open_head() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| reopen(t, "a"));
        assert!(matches!(err, Error::IllegalTransition { slug, from, to }
            if slug == "a" && from == Status::Open && to == Status::Open));
    }

    #[test]
    fn prior_survives_a_cascade_reopen_and_the_re_answer_after_it() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);
        reanswer(&mut t, "root");

        assert_eq!(
            t.heads["kid"].prior.as_ref().map(|a| a.text.as_str()),
            Some("because")
        );
        reanswer(&mut t, "kid");
        let kid = &t.heads["kid"];
        assert_eq!(kid.answer.as_ref().unwrap().text, "actually, because");
        assert_eq!(
            kid.prior.as_ref().map(|a| a.text.as_str()),
            Some("because"),
            "the answer from before the reopen is still the most recent superseded one"
        );
        assert_eq!(kid.rev, 2);
    }

    #[test]
    fn cauterise_answers_with_the_marker() {
        let mut t = tree();
        add(&mut t, "storage-format", None);
        answer(&mut t, "storage-format");
        add(&mut t, "write-model", Some("storage-format"));

        cauterise(
            &mut t,
            Cauterise {
                slug: "write-model",
                by: "storage-format",
                why: Some("the format decision settles it"),
                force: false,
            },
        )
        .unwrap();

        let head = &t.heads["write-model"];
        assert_eq!(
            head.status,
            Status::Answered,
            "cauterisation is not a state (§2)"
        );
        assert_eq!(head.rev, 1);
        let answer = head.answer.as_ref().unwrap();
        assert_eq!(answer.text, CAUTERISED);
        assert_eq!(answer.cauterised_by.as_deref(), Some("storage-format"));
        assert_eq!(
            answer.rationale.as_deref(),
            Some("the format decision settles it"),
            "--why lands in rationale"
        );
    }

    #[test]
    fn cauterise_ignores_unanswered_blockers() {
        let mut t = tree();
        add(&mut t, "by", None);
        answer(&mut t, "by");
        add(&mut t, "dead", None);
        add(&mut t, "blocker", None);
        link(&mut t, "dead", "blocker", false).unwrap();

        // §4.5 says "cutting"; a dead branch's gates are usually themselves
        // about to be cauterised, so requiring them first makes it unkillable.
        cauterise(
            &mut t,
            Cauterise {
                slug: "dead",
                by: "by",
                ..Cauterise::default()
            },
        )
        .unwrap();
        assert_eq!(t.heads["dead"].status, Status::Answered);
    }

    #[test]
    fn cauterise_rejects_an_unanswered_by() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        let err = rejects(&mut t, |t| {
            cauterise(
                t,
                Cauterise {
                    slug: "a",
                    by: "b",
                    ..Cauterise::default()
                },
            )
        });
        assert!(matches!(err, Error::CauteriseByUnanswered { slug, by }
            if slug == "a" && by == "b"));
    }

    #[test]
    fn force_cauterises_by_an_unanswered_head() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        cauterise(
            &mut t,
            Cauterise {
                slug: "a",
                by: "b",
                force: true,
                ..Cauterise::default()
            },
        )
        .unwrap();
        assert_eq!(
            t.heads["a"]
                .answer
                .as_ref()
                .unwrap()
                .cauterised_by
                .as_deref(),
            Some("b")
        );
        assert_eq!(t.heads["b"].status, Status::Open, "force records nothing");
    }

    #[test]
    fn cauterise_rejects_itself_even_under_force() {
        let mut t = tree();
        add(&mut t, "a", None);
        answer(&mut t, "a");
        for force in [false, true] {
            let err = rejects(&mut t, |t| {
                cauterise(
                    t,
                    Cauterise {
                        slug: "a",
                        by: "a",
                        force,
                        ..Cauterise::default()
                    },
                )
            });
            assert!(matches!(err, Error::SelfCauterise { slug } if slug == "a"));
        }
    }

    #[test]
    fn cauterise_rejects_unknown_heads() {
        let mut t = tree();
        add(&mut t, "a", None);
        answer(&mut t, "a");
        let err = rejects(&mut t, |t| {
            cauterise(
                t,
                Cauterise {
                    slug: "ghost",
                    by: "a",
                    ..Cauterise::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));

        let err = rejects(&mut t, |t| {
            cauterise(
                t,
                Cauterise {
                    slug: "a",
                    by: "ghost",
                    ..Cauterise::default()
                },
            )
        });
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }

    #[test]
    fn cauterise_of_an_answered_head_cascades() {
        let mut t = tree();
        answered_chain(&mut t, &["root", "kid"]);
        add(&mut t, "by", None);
        answer(&mut t, "by");

        let cascaded = cauterise(
            &mut t,
            Cauterise {
                slug: "kid",
                by: "by",
                ..Cauterise::default()
            },
        )
        .unwrap();
        assert!(cascaded.is_empty(), "kid has no dependents");

        let cascaded = cauterise(
            &mut t,
            Cauterise {
                slug: "root",
                by: "by",
                ..Cauterise::default()
            },
        )
        .unwrap();
        assert_eq!(cascaded, vec!["kid"], "a re-answer, cauterised or not");
    }

    #[test]
    fn reword_leaves_the_answer_alone() {
        let mut t = tree();
        answered_chain(&mut t, &["a"]);
        reword(&mut t, "a", "a better question?").unwrap();

        let head = &t.heads["a"];
        assert_eq!(head.question, "a better question?");
        assert_eq!(head.rev, 1);
        assert_eq!(head.status, Status::Answered);
        assert_eq!(head.answer.as_ref().unwrap().text, "because");

        let err = rejects(&mut t, |t| reword(t, "ghost", "q?"));
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }

    #[test]
    fn reparent_moves_a_head_and_can_root_it() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        add(&mut t, "kid", Some("a"));

        reparent(&mut t, "kid", Some("b")).unwrap();
        assert_eq!(t.heads["kid"].parent.as_deref(), Some("b"));

        reparent(&mut t, "kid", None).unwrap();
        assert_eq!(t.heads["kid"].parent, None);
        assert_eq!(t.heads["kid"].seq, 3, "last among the roots");
    }

    #[test]
    fn reparent_to_the_same_parent_is_a_no_op() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "k1", Some("a"));
        add(&mut t, "k2", Some("a"));

        let before = store::to_json(&t).unwrap();
        reparent(&mut t, "k1", Some("a")).unwrap();
        assert_eq!(
            store::to_json(&t).unwrap(),
            before,
            "it must not re-seat a head at the end of the set it is already in"
        );
    }

    #[test]
    fn reparent_rejects_its_own_descendant_and_itself() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        add(&mut t, "c", Some("b"));

        let err = rejects(&mut t, |t| reparent(t, "a", Some("c")));
        assert!(matches!(err, Error::ParentCycle { slug, parent, path }
            if slug == "a" && parent == "c" && path == vec!["a", "b", "c"]));

        let err = rejects(&mut t, |t| reparent(t, "a", Some("a")));
        assert!(matches!(err, Error::ParentCycle { slug, path, .. }
            if slug == "a" && path == vec!["a"]));
    }

    /// §4.9 without touching `blocked_by`: "b turns out to belong under a" after
    /// "a is gated on b" is the same loop, so `reparent` has to check it too.
    #[test]
    fn reparent_rejects_adopting_a_head_that_blocks_it() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "a", "b", false).unwrap();

        let err = rejects(&mut t, |t| reparent(t, "b", Some("a")));
        assert!(matches!(err, Error::ReopenCycle { slug, edge, other, path }
            if slug == "b"
                && edge == Edge::Parent
                && other == "a"
                && path == vec!["b", "a", "b"]));
    }

    /// The converse again: a head moving *under* the head it is gated on is the
    /// benign direction, and common — `graph-shape blocked_by consumption-surface`
    /// in §3's own file shape is exactly this.
    #[test]
    fn reparent_accepts_adopting_a_head_it_is_gated_on() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "b", "a", false).unwrap();

        reparent(&mut t, "b", Some("a")).unwrap();
        assert_eq!(t.heads["b"].parent.as_deref(), Some("a"));

        answer(&mut t, "a");
        answer(&mut t, "b");
        assert_eq!(reanswer(&mut t, "a"), vec!["b"]);
    }

    #[test]
    fn reparent_rejects_an_unknown_parent() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| reparent(t, "a", Some("ghost")));
        assert!(matches!(err, Error::UnknownParent { slug, parent }
            if slug == "a" && parent == "ghost"));

        let err = rejects(&mut t, |t| reparent(t, "ghost", None));
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }

    #[test]
    fn link_rejects_a_cycle_and_names_it() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        add(&mut t, "c", None);
        link(&mut t, "b", "a", false).unwrap();
        link(&mut t, "c", "b", false).unwrap();

        let err = rejects(&mut t, |t| link(t, "a", "c", false));
        assert!(matches!(err, Error::BlockCycle { slug, blocked_by, path }
            if slug == "a" && blocked_by == "c" && path == vec!["a", "c", "b", "a"]));
    }

    #[test]
    fn force_links_a_cycle() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "b", "a", false).unwrap();
        link(&mut t, "a", "b", true).unwrap();
        assert_eq!(t.heads["a"].blocked_by, vec!["b".to_string()]);
        assert_eq!(t.heads["b"].blocked_by, vec!["a".to_string()]);
    }

    #[test]
    fn link_rejects_a_self_block_as_a_cycle() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| link(t, "a", "a", false));
        assert!(matches!(err, Error::BlockCycle { slug, path, .. }
            if slug == "a" && path == vec!["a", "a"]));

        // Forceable like any other cycle: the state it produces — a head that is
        // never ready — is reachable anyway by forcing a two-head cycle.
        link(&mut t, "a", "a", true).unwrap();
        assert_eq!(t.heads["a"].blocked_by, vec!["a".to_string()]);
    }

    /// §4.9. The reopen relation is `X → children(X) ∪ {Y : Y blocked_by X}`, so
    /// gating a head on its own descendant closes a loop across the two kinds:
    /// re-answering `a` reopens `b`, answering `b` reopens `a`, forever, and the
    /// tree can never reach done.
    #[test]
    fn link_rejects_gating_a_head_on_its_own_descendant() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));

        let err = rejects(&mut t, |t| link(t, "a", "b", false));
        assert!(matches!(err, Error::ReopenCycle { slug, edge, other, path }
            if slug == "a"
                && edge == Edge::BlockedBy
                && other == "b"
                && path == vec!["a", "b", "a"]));
    }

    #[test]
    fn link_rejects_gating_a_head_on_a_grandchild() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        add(&mut t, "c", Some("b"));

        let err = rejects(&mut t, |t| link(t, "a", "c", false));
        assert!(
            matches!(err, Error::ReopenCycle { slug, other, path, .. }
            if slug == "a" && other == "c" && path == vec!["a", "c", "b", "a"]),
            "the path walks back up the ancestry that closes the loop"
        );
    }

    #[test]
    fn force_links_a_reopen_cycle() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        link(&mut t, "a", "b", true).unwrap();
        assert_eq!(t.heads["a"].blocked_by, vec!["b".to_string()]);
    }

    /// The converse direction is the benign one and must stay legal: with `c`
    /// gated on its own ancestor, both the parent edge and the `blocked_by` edge
    /// push staleness the same way — down — so there is no loop. It is also
    /// load-bearing, since §2 derives `blocked` from `blocked_by` alone: a child
    /// is *not* blocked by its parent unless the edge says so.
    #[test]
    fn link_accepts_gating_a_head_on_its_ancestor() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        add(&mut t, "c", Some("b"));
        link(&mut t, "c", "a", false).unwrap();

        for slug in ["a", "b", "c"] {
            answer(&mut t, slug);
        }
        assert_eq!(
            reanswer(&mut t, "a"),
            vec!["b", "c"],
            "each descendant is reopened once and nothing reopens a"
        );
        assert_eq!(t.heads["a"].status, Status::Answered);
    }

    /// The whole point of the edge kind per §2 — a real decision gated on an
    /// answer in another branch — so the §4.9 walk must not be so broad as to
    /// refuse it.
    #[test]
    fn link_accepts_a_cross_branch_edge() {
        let mut t = tree();
        add(&mut t, "root", None);
        add(&mut t, "graph-shape", Some("root"));
        add(&mut t, "head-schema", Some("graph-shape"));
        add(&mut t, "storage-format", Some("root"));
        add(&mut t, "write-model", Some("storage-format"));

        link(&mut t, "write-model", "head-schema", false).unwrap();
        assert_eq!(
            t.heads["write-model"].blocked_by,
            vec!["head-schema".to_string()]
        );
    }

    /// `sprout` needs no §4.9 check for the same reason it needs no `--force`:
    /// every reopen edge into a brand-new head is inbound. Nothing can be
    /// `blocked_by` it and it has no children, so it has no outgoing edge to
    /// close a loop with, however its own `--parent` and `--blocked-by` overlap.
    #[test]
    fn sprout_cannot_cycle_the_reopen_cascade() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "kid", Some("a"));

        for (slug, blocked_by) in [("gated-on-parent", "a"), ("gated-on-sibling", "kid")] {
            sprout(
                &mut t,
                Sprout {
                    question: "q?",
                    parent: Some("a"),
                    blocked_by: &[blocked_by],
                    slug: Some(slug),
                },
            )
            .unwrap();
        }
        // In gate order: §4.5 still holds, which is itself the sign that these
        // edges point the way round that lets the tree be finished.
        for slug in ["a", "kid", "gated-on-parent", "gated-on-sibling"] {
            answer(&mut t, slug);
        }

        assert_eq!(
            reanswer(&mut t, "a"),
            vec!["gated-on-parent", "gated-on-sibling", "kid"],
            "the cascade fans out and stops"
        );
        assert_eq!(t.heads["a"].status, Status::Answered);
    }

    #[test]
    fn link_rejects_an_unknown_blocker() {
        let mut t = tree();
        add(&mut t, "a", None);
        let err = rejects(&mut t, |t| link(t, "a", "ghost", false));
        assert!(matches!(err, Error::UnknownBlocker { slug, blocked_by }
            if slug == "a" && blocked_by == "ghost"));

        let err = rejects(&mut t, |t| link(t, "ghost", "a", false));
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }

    #[test]
    fn link_is_idempotent_and_sorts_its_edges() {
        let mut t = tree();
        for slug in ["a", "m", "b"] {
            add(&mut t, slug, None);
        }
        link(&mut t, "a", "m", false).unwrap();
        link(&mut t, "a", "b", false).unwrap();
        assert_eq!(
            t.heads["a"].blocked_by,
            vec!["b".to_string(), "m".to_string()]
        );

        let before = store::to_json(&t).unwrap();
        link(&mut t, "a", "m", false).unwrap();
        assert_eq!(store::to_json(&t).unwrap(), before);
    }

    #[test]
    fn unlink_is_idempotent_and_repairs_a_dangling_edge() {
        let mut t = tree();
        add(&mut t, "a", None);
        add(&mut t, "b", None);
        link(&mut t, "a", "b", false).unwrap();

        unlink(&mut t, "a", "b").unwrap();
        assert!(t.heads["a"].blocked_by.is_empty());

        let before = store::to_json(&t).unwrap();
        unlink(&mut t, "a", "b").unwrap();
        assert_eq!(store::to_json(&t).unwrap(), before);

        // A hand-edited tree can violate §4.2; unlink is how it gets fixed, so
        // it does not require the blocker to exist.
        t.heads
            .get_mut("a")
            .unwrap()
            .set_blocked_by(vec!["ghost".to_string()]);
        unlink(&mut t, "a", "ghost").unwrap();
        assert!(t.heads["a"].blocked_by.is_empty());

        let err = rejects(&mut t, |t| unlink(t, "ghost", "a"));
        assert!(matches!(err, Error::UnknownHead { slug } if slug == "ghost"));
    }
}
