//! `hydra tree` (SPEC §5): the one command whose output is for eyes rather than
//! for `jq`, laid out exactly as §5's example.

use crate::model::Tree;
use crate::query::{self, State, Visit};

/// §5's legend: `●` answered · `○` ready · `◌` blocked · `⊘` cauterised.
fn glyph(state: State) -> char {
    match state {
        State::Answered => '●',
        State::Ready => '○',
        State::Blocked => '◌',
        State::Cauterised => '⊘',
    }
}

const NEXT: &str = "← next";
const INDENT: usize = 2;
/// Blank columns between the longest `<indent><glyph> <slug>` label and the
/// summary column, measured off §5's example.
const GUTTER: usize = 3;

pub fn render(tree: &Tree) -> String {
    let counts = query::status(tree);
    let next = query::next_slug(tree);
    let rows: Vec<(String, String)> = query::preorder(tree)
        .into_iter()
        .map(|visit| {
            let state = query::state(tree, visit.head);
            let label = format!(
                "{}{} {}",
                " ".repeat(INDENT * visit.depth),
                glyph(state),
                visit.slug
            );
            // The marker owns the column outright rather than being appended to
            // a summary: `next` is always a ready head (§5) and a ready head has
            // no answer to summarise, so there is never anything to append to.
            // §5's example shows exactly that — `○ lifecycle  ← next`.
            let summary = if next == Some(visit.slug) {
                NEXT.to_string()
            } else {
                summary(tree, &visit, state)
            };
            (label, summary)
        })
        .collect();

    // Character counts, not `len()`. The column §5 draws is a column of
    // characters. Byte lengths would agree for every tree hydra itself writes —
    // each label is one three-byte glyph plus an ASCII slug (§2), a constant
    // offset that cancels — but nothing validates head keys on load, and a
    // hand-edited multi-byte slug breaks the symmetry and over-pads every other
    // line. `a_multibyte_slug_does_not_shift_the_column` is what holds this.
    let column = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .map_or(0, |longest| longest + GUTTER);

    let mut out = format!(
        "{}  ({} answered, {} open)\n",
        tree.slug, counts.answered, counts.open
    );
    if rows.is_empty() {
        return out;
    }
    out.push('\n');
    for (label, summary) in rows {
        let pad = " ".repeat(column - label.chars().count());
        let line = format!("{label}{pad}{summary}");
        // A ready head that is not `next` has nothing in the summary column, and
        // the padding leading up to it is not worth committing to a file.
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The summary column. §5 puts the reason a head is settled or stalled where an
/// answer summary would go, so `cauterised by` and `blocked by` are alternatives
/// to the summary rather than additions to it.
fn summary(tree: &Tree, visit: &Visit<'_>, state: State) -> String {
    let answer = visit.head.answer.as_ref();
    match state {
        // The `unwrap_or` is unreachable: `query::state` returns `Cauterised`
        // only for an answer whose `cauterised_by` is set, and it is handed the
        // same head. It stays because the correlation is invisible to the type
        // system and `render` must not panic on a file someone hand-edited —
        // §4's line is that corruption is reported, not fatal.
        State::Cauterised => {
            let by = answer
                .and_then(|a| a.cauterised_by.as_deref())
                .unwrap_or("");
            format!("cauterised by {by}")
        }
        // A hand-edited `{status: "answered", answer: null}` head has no summary
        // to show; the glyph already says it is settled.
        State::Answered => answer.map_or(String::new(), |a| query::first_line(&a.text).to_string()),
        State::Blocked => format!(
            "blocked by {}",
            query::open_blockers(tree, visit.head).join(", ")
        ),
        State::Ready => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{self, Cauterise, Cut, Sprout};

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

    fn cut(tree: &mut Tree, slug: &str, parent: Option<&str>, answer: &str) {
        add(tree, slug, parent);
        graph::cut(
            tree,
            Cut {
                slug,
                answer,
                force: true,
                ..Cut::default()
            },
        )
        .unwrap();
    }

    /// The tree §5's example block renders. The header counts there belong to a
    /// larger tree the example is a window onto, so only the body is reproduced
    /// byte for byte.
    fn spec_example() -> Tree {
        let mut t = Tree::new("hydra-design".to_string());
        cut(&mut t, "consumption-surface", None, "CLI unix tool");
        cut(
            &mut t,
            "graph-shape",
            Some("consumption-surface"),
            "spanning tree + blocked_by",
        );
        cut(
            &mut t,
            "head-schema",
            Some("graph-shape"),
            "answer{text, rationale, rejected}",
        );
        add(&mut t, "lifecycle", Some("graph-shape"));
        cut(
            &mut t,
            "storage-format",
            Some("consumption-surface"),
            "mutable JSON, git = history",
        );
        add(&mut t, "write-model", Some("storage-format"));
        graph::cauterise(
            &mut t,
            Cauterise {
                slug: "write-model",
                by: "storage-format",
                ..Cauterise::default()
            },
        )
        .unwrap();
        add(&mut t, "resume-shape", Some("storage-format"));
        graph::link(&mut t, "resume-shape", "lifecycle", false).unwrap();
        t
    }

    #[test]
    fn matches_the_spec_example() {
        let expected = "\
hydra-design  (5 answered, 2 open)

● consumption-surface   CLI unix tool
  ● graph-shape         spanning tree + blocked_by
    ● head-schema       answer{text, rationale, rejected}
    ○ lifecycle         ← next
  ● storage-format      mutable JSON, git = history
    ⊘ write-model       cauterised by storage-format
    ◌ resume-shape      blocked by lifecycle
";
        assert_eq!(render(&spec_example()), expected);
    }

    /// The character column each summary starts in. Character, not byte: a byte
    /// offset into a line carrying `●` and CJK text is not a column at all.
    fn summary_columns(render: &str) -> Vec<usize> {
        render
            .lines()
            .skip(2)
            .map(|line| {
                // <indent><glyph><space><slug><gutter><summary>
                let chars: Vec<char> = line.chars().collect();
                let glyph = chars.iter().position(|c| *c != ' ').unwrap();
                let mut at = glyph + 2;
                while chars.get(at).is_some_and(|c| *c != ' ') {
                    at += 1;
                }
                while chars.get(at) == Some(&' ') {
                    at += 1;
                }
                at
            })
            .collect()
    }

    #[test]
    fn every_summary_starts_in_the_same_column() {
        let columns = summary_columns(&render(&spec_example()));
        assert_eq!(columns, vec![24; 7], "§5's example puts the column at 24");
    }

    /// The one input that tells character counts apart from byte lengths.
    ///
    /// Everywhere else in the render each label is one three-byte glyph plus an
    /// ASCII slug, so byte arithmetic is off by a constant two per line and the
    /// column still lands where §5 puts it. A multi-byte *slug* breaks that
    /// symmetry: it is longer in bytes than in characters, so a byte-measured
    /// `column` over-pads every other line. §2's slug format is ASCII, but
    /// `store` validates only the tree slug, never the head keys — so a
    /// hand-edited file can carry one, which is the same class of input
    /// `preorder` already refuses to be broken by.
    #[test]
    fn a_multibyte_slug_does_not_shift_the_column() {
        let mut t = Tree::new("t".to_string());
        cut(&mut t, "ascii", None, "one");
        cut(&mut t, "placeholder", None, "two");
        let mut head = t.heads.remove("placeholder").unwrap();
        head.slug = "café-über".to_string();
        t.heads.insert(head.slug.clone(), head);

        let out = render(&t);
        assert_eq!(
            out,
            "\
t  (2 answered, 0 open)

● ascii       one
● café-über   two
"
        );
        assert_eq!(
            summary_columns(&out),
            vec![14; 2],
            "11 characters of longest label plus the gutter, not 15 bytes of it"
        );
    }

    /// Multi-byte content in the summary column must not shift the column, and
    /// the column itself is a character count — a slug is ASCII by §2, but the
    /// glyph in front of it and the answers behind it are not.
    #[test]
    fn multibyte_content_does_not_disturb_the_column() {
        let mut t = Tree::new("hydra-design".to_string());
        cut(&mut t, "one", None, "日本語 で 答え\nsecond line dropped");
        cut(
            &mut t,
            "a-considerably-long",
            None,
            "café ← naïve — em dash",
        );
        add(&mut t, "three", None);

        let out = render(&t);
        assert_eq!(
            out,
            "\
hydra-design  (2 answered, 1 open)

● one                   日本語 で 答え
● a-considerably-long   café ← naïve — em dash
○ three                 ← next
"
        );
        assert_eq!(summary_columns(&out), vec![24; 3]);
    }

    #[test]
    fn glyphs_follow_the_derived_state() {
        let mut t = Tree::new("t".to_string());
        cut(&mut t, "answered", None, "yes");
        add(&mut t, "ready", None);
        add(&mut t, "blocked", None);
        graph::link(&mut t, "blocked", "ready", false).unwrap();
        add(&mut t, "cauterised", None);
        graph::cauterise(
            &mut t,
            Cauterise {
                slug: "cauterised",
                by: "answered",
                ..Cauterise::default()
            },
        )
        .unwrap();

        assert_eq!(
            render(&t),
            "\
t  (2 answered, 2 open)

● answered     yes
○ ready        ← next
◌ blocked      blocked by ready
⊘ cauterised   cauterised by answered
"
        );
    }

    #[test]
    fn the_next_marker_moves_with_next() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "first", None);
        add(&mut t, "second", None);

        let out = render(&t);
        assert!(out.contains("○ first    ← next"), "{out}");
        assert!(
            out.ends_with("○ second\n"),
            "not next, so nothing at all: {out}"
        );

        graph::cut(
            &mut t,
            Cut {
                slug: "first",
                answer: "done",
                ..Cut::default()
            },
        )
        .unwrap();
        let out = render(&t);
        assert!(out.contains("● first    done"), "{out}");
        assert!(out.contains("○ second   ← next"), "{out}");
    }

    #[test]
    fn a_blocked_head_names_every_open_blocker() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "gate-a", None);
        add(&mut t, "gate-b", None);
        add(&mut t, "waiting", None);
        graph::link(&mut t, "waiting", "gate-b", false).unwrap();
        graph::link(&mut t, "waiting", "gate-a", false).unwrap();
        // Only a hand edit can strand a `blocked_by`, and §4's line is that a
        // missing blocker blocks — the render has to say so rather than hide it.
        t.heads
            .get_mut("waiting")
            .unwrap()
            .blocked_by
            .push("ghost".to_string());

        assert!(
            render(&t).contains("◌ waiting   blocked by gate-a, gate-b, ghost"),
            "{}",
            render(&t)
        );
    }

    #[test]
    fn an_empty_tree_renders_the_header_alone() {
        assert_eq!(
            render(&Tree::new("fresh".to_string())),
            "fresh  (0 answered, 0 open)\n"
        );
    }

    /// A hand-edited parent cycle is unreachable from any root, so its members
    /// are entered after the genuine roots rather than dropped.
    #[test]
    fn a_parent_cycle_renders_every_head_once() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        add(&mut t, "loose", None);
        t.heads.get_mut("a").unwrap().parent = Some("b".to_string());

        assert_eq!(
            render(&t),
            "\
t  (0 answered, 3 open)

○ loose   ← next
○ a
  ○ b
"
        );
    }
}
