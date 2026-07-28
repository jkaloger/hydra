//! Read-only derivations: SPEC §2's derived state, §3's pre-order walk, §5's
//! queries and §7's resume payload.
//!
//! Nothing here is stored. §2 keeps two states and §3 keeps the parent pointer
//! plus `seq` as the single source of truth, so every derivation below is a
//! hand-written walk over `Tree::heads` computed fresh (§9). A walk may build a
//! transient child index; it may not leave one behind.
//!
//! The output types are serde `Serialize` and are the shapes the CLI prints.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::{Head, Status, Tree};
use crate::{Error, Result};

/// §2's derivations collapsed into the four things a head can look like, which
/// is also §5's glyph set. Cauterisation is not a state (§2) — this is a view,
/// and `Status` remains the only thing on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Answered,
    Cauterised,
    Ready,
    Blocked,
}

/// One stop on the pre-order walk.
#[derive(Debug, Clone, Copy)]
pub struct Visit<'t> {
    /// The map key, which the lib treats as the head's identity (see
    /// `graph::dependents`).
    pub slug: &'t str,
    pub head: &'t Head,
    pub depth: usize,
}

/// §7's skeleton row. `summary` and `prior_summary` are omitted rather than
/// nulled when absent: §3's stable-key rule is about the stored document, and
/// here two null fields on each of 200 heads is a measurable share of the
/// budget §7 exists to defend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skeleton {
    pub slug: String,
    pub question: String,
    /// The stored two-valued state (§2), unwidened: a `status` that could say
    /// `cauterised` would teach a consumer that §2 stores three states.
    pub status: Status,
    /// The derived view of the same head. One word per row against §7's budget,
    /// and it is what makes the skeleton answer "can that one be asked?" — on a
    /// cold start this payload is all the model has, so without it a request to
    /// jump to a named head is a guess or a `show`.
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// First line of `prior.text`, for a head that has been answered before and
    /// is open again. §2 has the LLM re-present the old answer and ask whether
    /// it still holds, which it cannot do from a skeleton that says only
    /// "open" — and a cascade can reopen a large subtree at once, so without
    /// this the skeleton goes blank over exactly the region §7 is meant to keep
    /// legible. Kept as its own field so `summary` never implies settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_summary: Option<String>,
}

/// One head, fully hydrated (§5) — the stored head verbatim plus the
/// derivations a consumer would otherwise have to recompute. Flattened rather
/// than projected field by field so `show` cannot fall behind the model.
#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub head: Head,
    pub state: State,
    /// The `blocked_by` entries not yet answered. Not confined to a `blocked`
    /// head: a head force-cut past §4.5 is answered and still lists the edges it
    /// was forced past. §4 says `--force` records nothing, so this is the only
    /// trace of it anywhere — worth surfacing rather than filtering out.
    pub open_blockers: Vec<String>,
    /// Ancestry root first, excluding this head. §7's premises.
    pub ancestors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Echoed so a stale `HEAD` shows up in a read as well as in a write (§4).
    pub tree: String,
    pub answered: usize,
    pub open: usize,
    pub ready: usize,
    pub blocked: usize,
    /// A subset of `answered`, not a state of its own (§2).
    pub cauterised: usize,
    /// §2's tree-level `done`: zero open heads. Vacuously true of an empty tree,
    /// which is the literal derivation and the right answer — a tree with no
    /// questions has nothing outstanding.
    pub done: bool,
}

/// Field order is load-bearing and is pinned by `resume_field_order_is_pinned`.
/// Serialized directly, the fields come out in declaration order, which puts the
/// cheap orienting ones first and `hydrated` last so a truncated dump still
/// reads. Going via `serde_json::Value` — as `store::to_json` does for §3's
/// sorted keys — re-sorts them to `counts, hydrated, next, skeleton` and throws
/// that away, so the CLI must serialize this type straight.
#[derive(Debug, Clone, Serialize)]
pub struct Resume {
    pub counts: Counts,
    /// `None` when the tree is done or empty, or when every open head is
    /// blocked.
    ///
    /// That third case means the file is corrupt. §4.3 keeps `blocked_by` a DAG,
    /// and a minimal open head in a DAG has only answered blockers and is
    /// therefore ready — so it takes a forced cycle or a hand-edited dangling
    /// edge to get here. Which is why `next` returns `None` rather than falling
    /// back to a blocked head: `counts.done` is then `false` and the two
    /// together say "something is wrong" instead of handing over a question
    /// whose premises are not in.
    pub next: Option<String>,
    /// Every head, in pre-order.
    pub skeleton: Vec<Skeleton>,
    /// `next` and its ancestors, root first, `next` last. Empty when there is
    /// no `next`: §7's second tier exists to furnish the premises of the
    /// question about to be asked, and a done tree has none to ask.
    pub hydrated: Vec<Detail>,
}

/// Whether one `blocked_by` entry is still holding its head back.
///
/// An entry naming a head that does not exist counts as blocking rather than
/// being skipped, matching `graph::require_blockers_answered`. Only a hand edit
/// can produce one, and reading it as satisfied would advance the frontier over
/// the corruption instead of stalling on it.
fn unmet(tree: &Tree, blocker: &str) -> bool {
    tree.heads.get(blocker).map(|head| head.status) != Some(Status::Answered)
}

/// The `blocked_by` entries keeping `head` off the frontier, in stored order.
pub fn open_blockers(tree: &Tree, head: &Head) -> Vec<String> {
    head.blocked_by
        .iter()
        .filter(|blocker| unmet(tree, blocker))
        .cloned()
        .collect()
}

pub fn is_blocked(tree: &Tree, head: &Head) -> bool {
    head.blocked_by.iter().any(|blocker| unmet(tree, blocker))
}

pub fn is_ready(tree: &Tree, head: &Head) -> bool {
    head.status == Status::Open && !is_blocked(tree, head)
}

/// §2: a head killed by a sibling's answer, identified by the field rather than
/// by matching `answer.text` against `graph::CAUTERISED` — the text is the
/// human-facing half and a real answer is free to say the same word.
pub fn is_cauterised(head: &Head) -> bool {
    head.answer
        .as_ref()
        .is_some_and(|answer| answer.cauterised_by.is_some())
}

pub fn is_done(tree: &Tree) -> bool {
    !tree.heads.values().any(|head| head.status == Status::Open)
}

pub fn state(tree: &Tree, head: &Head) -> State {
    match head.status {
        Status::Answered if is_cauterised(head) => State::Cauterised,
        Status::Answered => State::Answered,
        Status::Open if is_blocked(tree, head) => State::Blocked,
        Status::Open => State::Ready,
    }
}

/// §7's one-line summary. Derived, never stored: it costs nothing and trains
/// the habit of leading an answer with the decision.
pub fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

type Kids<'t> = BTreeMap<Option<&'t str>, Vec<(&'t str, &'t Head)>>;

/// Depth-first, siblings ascending by `seq` (§3). Every head appears exactly
/// once, whatever the file says.
///
/// Siblings are ordered by `(seq, slug)`. `seq` is unique per sibling set for
/// anything hydra writes, but a hand edit can tie it, and a walk that reorders
/// itself between two runs of `hydra tree` is worse than one that picks an
/// arbitrary-but-fixed tiebreak.
///
/// The `slug` half of that key is currently documentation rather than behaviour:
/// `Tree::heads` is a `BTreeMap` keyed by slug, so the vec below is already in
/// ascending-slug order before it is sorted, and `sort_by` is stable. Spelling
/// the tiebreak out anyway makes the ordering a property of this function rather
/// than of the container it happens to read from.
///
/// Two things a hand-edited file can do that the walk has to survive, since §4
/// exists to stop silent corruption and dropping a head from `tree` or `resume`
/// output *is* silent corruption:
///
/// - A `parent` naming a head that does not exist. Such a head is walked as a
///   root: it has no ancestry to nest under, and the alternative is omitting it.
/// - A `parent` cycle. The cycle's members are reachable from no root, so after
///   the walk from the roots drains, any head still unvisited is used as an
///   extra entry point, in slug order for determinism. The `seen` set is what
///   makes the walk terminate rather than spin round the cycle.
pub fn preorder(tree: &Tree) -> Vec<Visit<'_>> {
    let mut children: Kids<'_> = BTreeMap::new();
    for (slug, head) in &tree.heads {
        let parent = head
            .parent
            .as_deref()
            .filter(|parent| tree.heads.contains_key(*parent));
        children
            .entry(parent)
            .or_default()
            .push((slug.as_str(), head));
    }
    for kids in children.values_mut() {
        kids.sort_by(|(a_slug, a), (b_slug, b)| (a.seq, *a_slug).cmp(&(b.seq, *b_slug)));
    }

    let mut out = Vec::with_capacity(tree.heads.len());
    let mut seen = BTreeSet::new();
    let roots: Vec<(&str, &Head)> = children.get(&None).cloned().unwrap_or_default();
    for root in roots {
        walk(root, &children, &mut seen, &mut out);
    }
    for (slug, head) in &tree.heads {
        if !seen.contains(slug.as_str()) {
            walk((slug.as_str(), head), &children, &mut seen, &mut out);
        }
    }
    out
}

fn walk<'t>(
    root: (&'t str, &'t Head),
    children: &Kids<'t>,
    seen: &mut BTreeSet<&'t str>,
    out: &mut Vec<Visit<'t>>,
) {
    let mut stack = vec![(root.0, root.1, 0usize)];
    while let Some((slug, head, depth)) = stack.pop() {
        if !seen.insert(slug) {
            continue;
        }
        out.push(Visit { slug, head, depth });
        if let Some(kids) = children.get(&Some(slug)) {
            // Reversed, because a stack hands the last push back first.
            for (kid, kid_head) in kids.iter().rev() {
                stack.push((kid, kid_head, depth + 1));
            }
        }
    }
}

/// `slug`'s ancestry, root first, excluding `slug` itself.
///
/// Stops at a parent that does not exist and at a repeat: a hand-edited parent
/// cycle has no root to reach, and the chain returned is then whatever ancestry
/// is genuinely above the head. Same line `preorder` takes.
pub fn ancestors(tree: &Tree, slug: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = BTreeSet::from([slug.to_string()]);
    let mut at = tree.heads.get(slug).and_then(|head| head.parent.clone());
    while let Some(parent) = at {
        let Some(head) = tree.heads.get(&parent) else {
            break;
        };
        if !seen.insert(parent.clone()) {
            break;
        }
        at = head.parent.clone();
        chain.push(parent);
    }
    chain.reverse();
    chain
}

pub fn skeleton(tree: &Tree) -> Vec<Skeleton> {
    preorder(tree)
        .into_iter()
        .map(|visit| Skeleton {
            slug: visit.slug.to_string(),
            question: visit.head.question.clone(),
            status: visit.head.status,
            state: state(tree, visit.head),
            summary: visit
                .head
                .answer
                .as_ref()
                .map(|answer| first_line(&answer.text).to_string()),
            prior_summary: match (&visit.head.answer, &visit.head.prior) {
                (None, Some(prior)) => Some(first_line(&prior.text).to_string()),
                _ => None,
            },
        })
        .collect()
}

/// All ready heads in pre-order (§5). Skeleton rows: a ready head has no answer
/// to show, and `show` is the way to ask for more.
pub fn ready(tree: &Tree) -> Vec<Skeleton> {
    let ready: BTreeSet<&str> = tree
        .heads
        .iter()
        .filter(|(_, head)| is_ready(tree, head))
        .map(|(slug, _)| slug.as_str())
        .collect();
    skeleton(tree)
        .into_iter()
        .filter(|row| ready.contains(row.slug.as_str()))
        .collect()
}

/// §5: the *first ready head in pre-order*. Document order, not priority —
/// hydra says what can be asked, never what should.
pub fn next_slug(tree: &Tree) -> Option<&str> {
    preorder(tree)
        .into_iter()
        .find(|visit| is_ready(tree, visit.head))
        .map(|visit| visit.slug)
}

pub fn next(tree: &Tree) -> Option<Detail> {
    let slug = next_slug(tree)?.to_string();
    Some(detail(tree, &slug, &tree.heads[&slug]))
}

pub fn show(tree: &Tree, slug: &str) -> Result<Detail> {
    let head = tree.heads.get(slug).ok_or_else(|| Error::UnknownHead {
        slug: slug.to_string(),
    })?;
    Ok(detail(tree, slug, head))
}

pub fn status(tree: &Tree) -> Counts {
    let (mut ready, mut blocked, mut cauterised, mut answered) = (0, 0, 0, 0);
    for head in tree.heads.values() {
        match state(tree, head) {
            State::Answered => answered += 1,
            // §2: cauterised is answered, counted twice on purpose so
            // `answered + open` still totals the tree.
            State::Cauterised => {
                answered += 1;
                cauterised += 1;
            }
            State::Ready => ready += 1,
            State::Blocked => blocked += 1,
        }
    }
    Counts {
        tree: tree.slug.clone(),
        answered,
        open: ready + blocked,
        ready,
        blocked,
        cauterised,
        done: ready + blocked == 0,
    }
}

/// §7's two tiers in one payload.
pub fn resume(tree: &Tree) -> Resume {
    let next = next_slug(tree).map(str::to_string);
    let hydrated = match &next {
        Some(slug) => ancestors(tree, slug)
            .into_iter()
            .chain([slug.clone()])
            .map(|slug| detail(tree, &slug, &tree.heads[&slug]))
            .collect(),
        None => vec![],
    };
    Resume {
        counts: status(tree),
        next,
        skeleton: skeleton(tree),
        hydrated,
    }
}

fn detail(tree: &Tree, slug: &str, head: &Head) -> Detail {
    Detail {
        head: head.clone(),
        state: state(tree, head),
        open_blockers: open_blockers(tree, head),
        ancestors: ancestors(tree, slug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{self, CAUTERISED, Cauterise, Cut, Sprout};

    fn tree() -> Tree {
        Tree::new("t".to_string())
    }

    fn add(tree: &mut Tree, slug: &str, parent: Option<&str>) {
        graph::sprout(
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

    fn answer(tree: &mut Tree, slug: &str, text: &str) {
        graph::cut(
            tree,
            Cut {
                slug,
                answer: text,
                force: true,
                ..Cut::default()
            },
        )
        .unwrap();
    }

    fn slugs(tree: &Tree) -> Vec<&str> {
        preorder(tree).into_iter().map(|v| v.slug).collect()
    }

    fn depths(tree: &Tree) -> Vec<(&str, usize)> {
        preorder(tree)
            .into_iter()
            .map(|v| (v.slug, v.depth))
            .collect()
    }

    /// root ─ a ─ a1, a2 · b ─ b1, all open.
    fn fanned() -> Tree {
        let mut t = tree();
        add(&mut t, "root", None);
        add(&mut t, "a", Some("root"));
        add(&mut t, "a1", Some("a"));
        add(&mut t, "a2", Some("a"));
        add(&mut t, "b", Some("root"));
        add(&mut t, "b1", Some("b"));
        t
    }

    #[test]
    fn preorder_is_depth_first_by_seq() {
        let t = fanned();
        assert_eq!(slugs(&t), vec!["root", "a", "a1", "a2", "b", "b1"]);
        assert_eq!(
            depths(&t),
            vec![
                ("root", 0),
                ("a", 1),
                ("a1", 2),
                ("a2", 2),
                ("b", 1),
                ("b1", 2)
            ]
        );
    }

    #[test]
    fn preorder_orders_roots_like_any_sibling_set() {
        let mut t = tree();
        add(&mut t, "second", None);
        add(&mut t, "first", None);
        add(&mut t, "kid", Some("second"));
        assert_eq!(
            slugs(&t),
            vec!["second", "kid", "first"],
            "roots go by seq, not by slug"
        );
    }

    /// §8 calls out pre-order stability. A slug's position is a function of the
    /// tree, not of when it was inserted.
    #[test]
    fn preorder_is_stable_under_insertion() {
        let mut t = fanned();
        let before: Vec<String> = slugs(&t).into_iter().map(str::to_string).collect();
        add(&mut t, "a3", Some("a"));
        add(&mut t, "deep", Some("a1"));
        let after = slugs(&t);

        let kept: Vec<&str> = after
            .iter()
            .copied()
            .filter(|s| before.iter().any(|b| b == s))
            .collect();
        assert_eq!(kept, before, "an insertion must not reorder existing heads");
        assert_eq!(
            after,
            vec!["root", "a", "a1", "deep", "a2", "a3", "b", "b1"]
        );
    }

    #[test]
    fn preorder_follows_a_reparent() {
        let mut t = fanned();
        graph::reparent(&mut t, "a", Some("b")).unwrap();
        assert_eq!(
            slugs(&t),
            vec!["root", "b", "b1", "a", "a1", "a2"],
            "the subtree moves whole, and lands last in its new sibling set"
        );

        graph::reparent(&mut t, "a1", None).unwrap();
        assert_eq!(slugs(&t), vec!["root", "b", "b1", "a", "a2", "a1"]);
        assert_eq!(depths(&t).last(), Some(&("a1", 0)), "a new root");
    }

    /// A tied `seq` — only a hand edit can make one — must still produce a
    /// defined walk. It does not prove the `slug` half of the sort key is doing
    /// the work: `Tree::heads` is slug-keyed and `sort_by` is stable, so ties
    /// fall into slug order whether the key mentions `slug` or not. See the note
    /// on `preorder`.
    #[test]
    fn preorder_orders_a_tied_seq_by_slug() {
        let mut t = fanned();
        add(&mut t, "a0", Some("a"));
        assert_eq!(slugs(&t), vec!["root", "a", "a1", "a2", "a0", "b", "b1"]);

        for slug in ["a0", "a1", "a2"] {
            t.heads.get_mut(slug).unwrap().seq = 1;
        }
        assert_eq!(slugs(&t), vec!["root", "a", "a0", "a1", "a2", "b", "b1"]);
    }

    #[test]
    fn preorder_walks_a_dangling_parent_as_a_root() {
        let mut t = fanned();
        t.heads.get_mut("a").unwrap().parent = Some("ghost".to_string());

        // 'a' keeps the `seq` it had as root's child, so it ties with root and
        // the slug tiebreak puts it first. Order is arbitrary here; being
        // present and deterministic is the property that matters.
        assert_eq!(slugs(&t), vec!["a", "a1", "a2", "root", "b", "b1"]);
        assert_eq!(
            depths(&t).iter().find(|(s, _)| *s == "a"),
            Some(&("a", 0)),
            "no ancestry to nest under"
        );
        assert_eq!(
            ancestors(&t, "a1"),
            vec!["a".to_string()],
            "the chain stops where the file stops making sense"
        );
    }

    #[test]
    fn preorder_terminates_on_a_parent_cycle_and_drops_nothing() {
        let mut t = fanned();
        // root → a → a1 → root: only a hand edit gets here, and the walk must
        // neither spin nor omit the heads it can no longer reach from a root.
        t.heads.get_mut("root").unwrap().parent = Some("a1".to_string());

        let walked = slugs(&t);
        assert_eq!(walked.len(), t.heads.len());
        assert_eq!(
            walked,
            vec!["a", "a1", "root", "b", "b1", "a2"],
            "'a' is the first unvisited head in slug order, so the cycle enters there"
        );
        assert_eq!(
            ancestors(&t, "root"),
            vec!["a".to_string(), "a1".to_string()]
        );
    }

    #[test]
    fn blocked_heads_are_excluded_from_ready() {
        let mut t = fanned();
        graph::link(&mut t, "b", "a", false).unwrap();

        assert_eq!(state(&t, &t.heads["b"]), State::Blocked);
        assert_eq!(open_blockers(&t, &t.heads["b"]), vec!["a".to_string()]);
        let ready: Vec<String> = ready(&t).into_iter().map(|r| r.slug).collect();
        assert_eq!(ready, vec!["root", "a", "a1", "a2", "b1"]);

        answer(&mut t, "a", "settled");
        assert_eq!(state(&t, &t.heads["b"]), State::Ready);
    }

    #[test]
    fn a_cauterised_blocker_unblocks_because_cauterised_is_answered() {
        let mut t = fanned();
        add(&mut t, "killer", None);
        answer(&mut t, "killer", "the format decision settles it");
        graph::link(&mut t, "b", "a", false).unwrap();
        graph::cauterise(
            &mut t,
            Cauterise {
                slug: "a",
                by: "killer",
                ..Cauterise::default()
            },
        )
        .unwrap();

        assert_eq!(state(&t, &t.heads["a"]), State::Cauterised);
        assert!(
            is_ready(&t, &t.heads["b"]),
            "cauterisation is not a state (§2): the blocker is answered"
        );
        assert!(open_blockers(&t, &t.heads["b"]).is_empty());
    }

    #[test]
    fn a_missing_blocker_blocks() {
        let mut t = fanned();
        t.heads
            .get_mut("b")
            .unwrap()
            .set_blocked_by(vec!["ghost".to_string()]);

        assert_eq!(state(&t, &t.heads["b"]), State::Blocked);
        assert_eq!(open_blockers(&t, &t.heads["b"]), vec!["ghost".to_string()]);
        assert!(!ready(&t).iter().any(|r| r.slug == "b"));
    }

    #[test]
    fn a_real_answer_saying_cauterised_is_not_cauterised() {
        let mut t = fanned();
        answer(&mut t, "a", CAUTERISED);
        assert_eq!(
            state(&t, &t.heads["a"]),
            State::Answered,
            "the field is what identifies a cauterised head, not the text"
        );
    }

    #[test]
    fn next_is_the_first_ready_head_in_preorder() {
        let mut t = fanned();
        answer(&mut t, "root", "CLI unix tool");
        answer(&mut t, "a", "spanning tree");
        graph::link(&mut t, "a1", "b1", false).unwrap();

        assert_eq!(
            next_slug(&t),
            Some("a2"),
            "a1 is blocked, so the walk carries on to its sibling"
        );
        let next = next(&t).unwrap();
        assert_eq!(next.head.slug, "a2");
        assert_eq!(next.state, State::Ready);
        assert_eq!(next.ancestors, vec!["root".to_string(), "a".to_string()]);
    }

    #[test]
    fn changing_seq_changes_next() {
        let mut t = fanned();
        answer(&mut t, "root", "x");
        answer(&mut t, "a", "x");
        assert_eq!(next_slug(&t), Some("a1"));

        let (a1, a2) = (t.heads["a1"].seq, t.heads["a2"].seq);
        t.heads.get_mut("a1").unwrap().seq = a2;
        t.heads.get_mut("a2").unwrap().seq = a1;
        assert_eq!(
            next_slug(&t),
            Some("a2"),
            "document order, not priority (§5)"
        );
    }

    #[test]
    fn next_is_none_when_nothing_can_be_asked() {
        assert_eq!(next_slug(&tree()), None, "empty tree");

        let mut t = fanned();
        for slug in ["a1", "a2", "b1", "a", "b", "root"] {
            answer(&mut t, slug, "x");
        }
        assert!(is_done(&t));
        assert_eq!(next_slug(&t), None);

        // Every open head blocked, which §4.3's DAG makes unreachable without
        // corruption — a forced cycle here, a hand-edited dangling edge below.
        // Either way `next` says "nothing" rather than falling back to a blocked
        // head, and `done` stays false so the pair reads as "something is wrong".
        graph::reopen(&mut t, "a1").unwrap();
        graph::link(&mut t, "a1", "a1", true).unwrap();
        assert!(!is_done(&t));
        assert_eq!(next_slug(&t), None);

        let mut t = fanned();
        for slug in ["a1", "a2", "b1", "a", "b", "root"] {
            answer(&mut t, slug, "x");
        }
        graph::reopen(&mut t, "b1").unwrap();
        t.heads
            .get_mut("b1")
            .unwrap()
            .set_blocked_by(vec!["ghost".to_string()]);
        assert!(!is_done(&t));
        assert_eq!(next_slug(&t), None);
        assert!(resume(&t).hydrated.is_empty());
    }

    #[test]
    fn skeleton_summary_is_the_first_line_only() {
        let mut t = fanned();
        answer(
            &mut t,
            "root",
            "spanning tree + blocked_by cross edges\nnesting carries the narrative\nand more",
        );
        let rows = skeleton(&t);
        let root = &rows[0];
        assert_eq!(root.slug, "root");
        assert_eq!(
            root.summary.as_deref(),
            Some("spanning tree + blocked_by cross edges")
        );
        assert_eq!(root.status, Status::Answered);
        assert_eq!(root.prior_summary, None);

        assert_eq!(rows[1].slug, "a");
        assert_eq!(
            rows[1].summary, None,
            "an open head has no answer to line up"
        );
        assert_eq!(rows.len(), t.heads.len(), "every head, in pre-order");
        assert_eq!(
            rows.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
            slugs(&t)
        );
    }

    #[test]
    fn skeleton_falls_back_to_prior_for_a_reopened_head() {
        let mut t = fanned();
        answer(&mut t, "root", "CLI unix tool");
        answer(&mut t, "a", "spanning tree\nwith cross edges");
        answer(&mut t, "root", "an MCP shim as well");

        let rows = skeleton(&t);
        let a = rows.iter().find(|r| r.slug == "a").unwrap();
        assert_eq!(a.status, Status::Open, "cascade-reopened");
        assert_eq!(a.summary, None, "nothing is settled here any more");
        assert_eq!(
            a.prior_summary.as_deref(),
            Some("spanning tree"),
            "§2: the old answer is what the LLM re-presents"
        );

        // An open head that was never answered carries neither.
        let a1 = rows.iter().find(|r| r.slug == "a1").unwrap();
        assert_eq!(
            (a1.summary.as_deref(), a1.prior_summary.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn skeleton_omits_absent_summaries_from_the_json() {
        let t = fanned();
        let json = serde_json::to_string(&skeleton(&t)[0]).unwrap();
        assert_eq!(
            json,
            r#"{"slug":"root","question":"q?","status":"open","state":"ready"}"#
        );
    }

    /// The reason `state` earns its row: after a `compact` reload (§6) the
    /// skeleton is all the model has, and `status` alone cannot say whether a
    /// named head is available to ask.
    #[test]
    fn skeleton_state_separates_ready_from_blocked_and_cauterised() {
        let mut t = fanned();
        add(&mut t, "killer", None);
        answer(&mut t, "killer", "settled");
        graph::link(&mut t, "b", "a", false).unwrap();
        graph::cauterise(
            &mut t,
            Cauterise {
                slug: "b1",
                by: "killer",
                ..Cauterise::default()
            },
        )
        .unwrap();

        let rows: BTreeMap<String, (Status, State)> = skeleton(&t)
            .into_iter()
            .map(|row| (row.slug, (row.status, row.state)))
            .collect();
        assert_eq!(rows["a"], (Status::Open, State::Ready));
        assert_eq!(
            rows["b"],
            (Status::Open, State::Blocked),
            "both open, and only `state` tells them apart"
        );
        assert_eq!(rows["b1"], (Status::Answered, State::Cauterised));
        assert_eq!(rows["killer"], (Status::Answered, State::Answered));
    }

    #[test]
    fn hydrated_is_next_plus_its_ancestors_root_first() {
        let mut t = fanned();
        answer(&mut t, "root", "CLI unix tool");
        answer(&mut t, "a", "spanning tree");

        let payload = resume(&t);
        assert_eq!(payload.next.as_deref(), Some("a1"));
        assert_eq!(
            payload
                .hydrated
                .iter()
                .map(|d| d.head.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "a", "a1"],
            "premises first, the question last"
        );
        assert_eq!(payload.hydrated[0].state, State::Answered);
        assert_eq!(
            payload.hydrated[0].head.answer.as_ref().unwrap().text,
            "CLI unix tool",
            "full detail, not a summary"
        );
        assert_eq!(payload.hydrated[2].ancestors, vec!["root", "a"]);
        assert_eq!(payload.skeleton.len(), t.heads.len());
    }

    #[test]
    fn resume_of_a_done_tree_is_skeleton_only() {
        let mut t = fanned();
        for slug in ["a1", "a2", "b1", "a", "b", "root"] {
            answer(&mut t, slug, "x");
        }
        let payload = resume(&t);
        assert_eq!(payload.next, None);
        assert!(
            payload.hydrated.is_empty(),
            "no question to ask, so no premises to lay out"
        );
        assert_eq!(payload.skeleton.len(), 6, "the record is still complete");
        assert!(payload.counts.done);
        assert_eq!(payload.counts.open, 0);
    }

    /// I4 prints this type. Reusing `store::to_json`, which routes through
    /// `Value` to get §3's sorted keys, would silently reorder it — so the order
    /// is asserted here rather than left as a comment.
    #[test]
    fn resume_field_order_is_pinned() {
        let json = serde_json::to_string(&resume(&fanned())).unwrap();
        assert!(json.starts_with(r#"{"counts":{"tree":"t","#), "{json}");

        let mut at = 0;
        for key in ["counts", "next", "skeleton", "hydrated"] {
            let found = json
                .find(&format!("\"{key}\":"))
                .unwrap_or_else(|| panic!("no {key} in {json}"));
            assert!(found >= at, "{key} is out of order in {json}");
            at = found;
        }

        // The shape `store::to_json` would have produced instead, so the reason
        // this test exists is legible from the test.
        let sorted = serde_json::to_string(&serde_json::to_value(resume(&fanned())).unwrap());
        assert!(
            sorted.unwrap().starts_with(r#"{"counts":{"answered":"#),
            "a Value round-trip sorts the keys and loses the order above"
        );
    }

    #[test]
    fn resume_of_an_empty_tree_is_empty_and_done() {
        let payload = resume(&tree());
        assert_eq!(payload.next, None);
        assert!(payload.skeleton.is_empty());
        assert!(payload.hydrated.is_empty());
        assert!(
            payload.counts.done,
            "zero open heads is §2's derivation, vacuous or not"
        );
        assert_eq!(payload.counts.answered, 0);
    }

    #[test]
    fn counts_split_open_and_answered() {
        let mut t = fanned();
        add(&mut t, "killer", None);
        answer(&mut t, "killer", "settled");
        graph::cauterise(
            &mut t,
            Cauterise {
                slug: "a2",
                by: "killer",
                ..Cauterise::default()
            },
        )
        .unwrap();
        graph::link(&mut t, "b1", "a1", false).unwrap();

        let counts = status(&t);
        assert_eq!(counts.tree, "t");
        assert_eq!(counts.answered, 2, "killer and the cauterised a2");
        assert_eq!(counts.cauterised, 1, "a subset of answered, not a state");
        assert_eq!(counts.open, 5);
        assert_eq!(counts.blocked, 1);
        assert_eq!(counts.ready, 4);
        assert!(!counts.done);
        assert_eq!(counts.answered + counts.open, t.heads.len());
    }

    #[test]
    fn show_hydrates_one_head() {
        let mut t = fanned();
        answer(&mut t, "root", "CLI unix tool\nand nothing else");
        graph::link(&mut t, "a1", "b1", false).unwrap();

        let detail = show(&t, "a1").unwrap();
        assert_eq!(detail.state, State::Blocked);
        assert_eq!(detail.open_blockers, vec!["b1".to_string()]);
        assert_eq!(detail.ancestors, vec!["root".to_string(), "a".to_string()]);

        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["slug"], "a1", "the stored head is flattened in");
        assert_eq!(json["seq"], 1);
        assert!(json["prior"].is_null());

        assert!(matches!(
            show(&t, "ghost"),
            Err(Error::UnknownHead { slug }) if slug == "ghost"
        ));
    }

    /// §4 says `--force` records nothing, so an answered head still listing an
    /// open blocker is the only surviving trace of a cut forced past §4.5.
    /// `open_blockers` is therefore not confined to a `blocked` head.
    #[test]
    fn an_answered_head_keeps_the_blockers_it_was_forced_past() {
        let mut t = fanned();
        graph::link(&mut t, "a1", "b1", false).unwrap();
        graph::cut(
            &mut t,
            Cut {
                slug: "a1",
                answer: "decided anyway",
                force: true,
                ..Cut::default()
            },
        )
        .unwrap();

        let detail = show(&t, "a1").unwrap();
        assert_eq!(detail.state, State::Answered);
        assert_eq!(detail.open_blockers, vec!["b1".to_string()]);

        // §2 derives `blocked` from the edges alone, so it stays true here; it is
        // `ready`'s `open &&` that keeps an answered head off the frontier, and
        // `state` resolves answeredness before it looks at blockers.
        assert!(is_blocked(&t, &t.heads["a1"]));
        assert!(!is_ready(&t, &t.heads["a1"]));
        assert_eq!(status(&t).blocked, 0, "blocked counts open heads only");
    }
}
