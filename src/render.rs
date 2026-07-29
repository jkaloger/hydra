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

/// §5's connectors, four columns each as `tree(1)` draws them. The header line is
/// the root every depth-0 head hangs off, so every row carries a connector.
const TEE: &str = "├── ";
const ELBOW: &str = "└── ";
/// What a connector becomes on the lines below it: a bar while the sibling set is
/// still open, blank once the elbow has closed it.
const BAR: &str = "│   ";
const GAP: &str = "    ";

/// Blank columns between the longest `<prefix><glyph> <slug>` label and the
/// summary column, measured off §5's example.
const GUTTER: usize = 3;

/// Whether to dress the render in ANSI. The caller decides — a pipe gets
/// `Plain` — so nothing in here reads the environment or asks about the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Plain,
    Ansi,
}

impl Colour {
    fn paint(self, sgr: &str, text: &str) -> String {
        match self {
            Colour::Plain => text.to_string(),
            Colour::Ansi => format!("\x1b[{sgr}m{text}\x1b[0m"),
        }
    }
}

/// The glyph carries the state twice, as shape and as colour, so the legend is
/// only needed once.
fn state_sgr(state: State) -> &'static str {
    match state {
        State::Answered => "32",
        State::Ready => "1;36",
        State::Blocked => "33",
        // Settled without being decided (§2), so it is dimmed as well as red:
        // there is nothing here to come back to.
        State::Cauterised => "2;31",
    }
}

/// Connectors and summaries are context; the glyphs and slugs are the content.
const STRUCTURE: &str = "2";
const MARKER: &str = "1";

struct Row {
    prefix: String,
    state: State,
    slug: String,
    summary: String,
    next: bool,
    /// Character count of `<prefix><glyph> <slug>`.
    ///
    /// Characters, not bytes. The column §5 draws is a column of characters, and
    /// byte lengths do not agree with it here: the connectors are three bytes per
    /// drawing character and the glyph is three more. Those offsets would cancel
    /// if every row carried the same number of them, but rows sit at different
    /// depths, so a byte-measured column pads shallow rows out past deep ones.
    /// A hand-edited multi-byte slug breaks the symmetry the same way, which is
    /// what `a_multibyte_slug_does_not_shift_the_column` holds.
    width: usize,
}

pub fn render(tree: &Tree, colour: Colour) -> String {
    let counts = query::status(tree);
    let next = query::next_slug(tree);
    let visits = query::preorder(tree);
    let rows: Vec<Row> = prefixes(&visits)
        .into_iter()
        .zip(&visits)
        .map(|(prefix, visit)| {
            let state = query::state(tree, visit.head);
            let is_next = next == Some(visit.slug);
            Row {
                width: prefix.chars().count() + 2 + visit.slug.chars().count(),
                prefix,
                state,
                slug: visit.slug.to_string(),
                // The marker owns the column outright rather than being appended
                // to a summary: `next` is always a ready head (§5) and a ready
                // head has no answer to summarise, so there is never anything to
                // append to. §5's example shows exactly that — `○ lifecycle
                // ← next`.
                summary: if is_next {
                    NEXT.to_string()
                } else {
                    summary(tree, visit, state)
                },
                next: is_next,
            }
        })
        .collect();

    let column = rows
        .iter()
        .map(|row| row.width)
        .max()
        .map_or(0, |longest| longest + GUTTER);

    let mut out = format!(
        "{}  ({} answered, {} open)\n",
        tree.slug, counts.answered, counts.open
    );
    for row in rows {
        out.push_str(&colour.paint(STRUCTURE, &row.prefix));
        out.push_str(&colour.paint(state_sgr(row.state), &glyph(row.state).to_string()));
        out.push(' ');
        out.push_str(&row.slug);
        // A ready head that is not `next` has nothing in the summary column, and
        // the padding leading up to it is not worth committing to a file.
        if !row.summary.is_empty() {
            let sgr = if row.next { MARKER } else { STRUCTURE };
            out.push_str(&" ".repeat(column - row.width));
            out.push_str(&colour.paint(sgr, &row.summary));
        }
        out.push('\n');
    }
    out
}

/// The connector column for each visit, in the order they were walked.
///
/// A row's own connector says whether it closes its sibling set; the connectors
/// above it are replayed as bars or blanks, which is what makes a subtree read as
/// one block. Depth-0 heads are siblings of each other under the header line, so
/// they are drawn like any other sibling set — including the entry points
/// `preorder` invents for a hand-edited parent cycle, which arrive at depth 0.
fn prefixes(visits: &[Visit<'_>]) -> Vec<String> {
    let last = closes_its_sibling_set(visits);
    // Last-ness of each row on the path from depth 0 down to the current row.
    let mut path: Vec<bool> = Vec::new();
    visits
        .iter()
        .zip(&last)
        .map(|(visit, closes)| {
            path.truncate(visit.depth);
            let mut prefix = String::with_capacity(GAP.len() * (visit.depth + 1));
            for ancestor_closes in &path {
                prefix.push_str(if *ancestor_closes { GAP } else { BAR });
            }
            prefix.push_str(if *closes { ELBOW } else { TEE });
            path.push(*closes);
            prefix
        })
        .collect()
}

/// Whether each visit is the last of its siblings, walked backwards: a row closes
/// its set when nothing after it sits at the same depth without first rising above
/// it. Truncating at each row is what confines a subtree's depths to that subtree.
fn closes_its_sibling_set(visits: &[Visit<'_>]) -> Vec<bool> {
    let mut closes = vec![false; visits.len()];
    let mut pending: Vec<bool> = Vec::new();
    for (at, visit) in visits.iter().enumerate().rev() {
        if pending.len() <= visit.depth {
            pending.resize(visit.depth + 1, false);
        }
        closes[at] = !pending[visit.depth];
        pending[visit.depth] = true;
        pending.truncate(visit.depth + 1);
    }
    closes
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

    /// The render every test but the colour ones reads, since §5's layout is a
    /// property of the plain text and the ANSI is a coat of paint over it.
    fn plain(tree: &Tree) -> String {
        render(tree, Colour::Plain)
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
└── ● consumption-surface    CLI unix tool
    ├── ● graph-shape        spanning tree + blocked_by
    │   ├── ● head-schema    answer{text, rationale, rejected}
    │   └── ○ lifecycle      ← next
    └── ● storage-format     mutable JSON, git = history
        ├── ⊘ write-model    cauterised by storage-format
        └── ◌ resume-shape   blocked by lifecycle
";
        assert_eq!(plain(&spec_example()), expected);
    }

    /// The character column each summary starts in. Character, not byte: a byte
    /// offset into a line carrying connectors, `●` and CJK text is not a column
    /// at all.
    fn summary_columns(render: &str) -> Vec<usize> {
        render
            .lines()
            .skip(1)
            .map(|line| {
                // <prefix><glyph><space><slug><gutter><summary>
                let chars: Vec<char> = line.chars().collect();
                let glyph = chars.iter().position(|c| "●○◌⊘".contains(*c)).unwrap();
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
        let columns = summary_columns(&plain(&spec_example()));
        assert_eq!(
            columns,
            vec![29; 7],
            "the longest label is `        └── ◌ resume-shape` plus the gutter"
        );
    }

    /// The one input that tells character counts apart from byte lengths in the
    /// slug itself. §2's slug format is ASCII, but `store` validates only the
    /// tree slug, never the head keys — so a hand-edited file can carry one,
    /// which is the same class of input `preorder` already refuses to be broken
    /// by. A byte-measured column would pad the ASCII row out by the four bytes
    /// `café-über` spends on two characters.
    #[test]
    fn a_multibyte_slug_does_not_shift_the_column() {
        let mut t = Tree::new("t".to_string());
        cut(&mut t, "ascii", None, "one");
        cut(&mut t, "placeholder", None, "two");
        let mut head = t.heads.remove("placeholder").unwrap();
        head.slug = "café-über".to_string();
        t.heads.insert(head.slug.clone(), head);

        let out = plain(&t);
        assert_eq!(
            out,
            "\
t  (2 answered, 0 open)
├── ● ascii       one
└── ● café-über   two
"
        );
        assert_eq!(
            summary_columns(&out),
            vec![18; 2],
            "15 characters of longest label plus the gutter, not 19 bytes of it"
        );
    }

    /// Multi-byte content in the summary column must not shift the column, and
    /// the column itself is a character count — a slug is ASCII by §2, but the
    /// connectors and glyph in front of it and the answers behind it are not.
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

        let out = plain(&t);
        assert_eq!(
            out,
            "\
hydra-design  (2 answered, 1 open)
├── ● one                   日本語 で 答え
├── ● a-considerably-long   café ← naïve — em dash
└── ○ three                 ← next
"
        );
        assert_eq!(summary_columns(&out), vec![28; 3]);
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
            plain(&t),
            "\
t  (2 answered, 2 open)
├── ● answered     yes
├── ○ ready        ← next
├── ◌ blocked      blocked by ready
└── ⊘ cauterised   cauterised by answered
"
        );
    }

    /// Nesting is drawn, not indented: the bars and blanks above a row are what
    /// say which sibling sets are still open above it.
    #[test]
    fn connectors_close_each_sibling_set_and_carry_the_bars_below_it() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "a", None);
        add(&mut t, "a1", Some("a"));
        add(&mut t, "deep", Some("a1"));
        add(&mut t, "a2", Some("a"));
        add(&mut t, "b", None);
        add(&mut t, "b1", Some("b"));

        assert_eq!(
            plain(&t),
            "\
t  (0 answered, 6 open)
├── ○ a              ← next
│   ├── ○ a1
│   │   └── ○ deep
│   └── ○ a2
└── ○ b
    └── ○ b1
"
        );
    }

    #[test]
    fn the_next_marker_moves_with_next() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "first", None);
        add(&mut t, "second", None);

        let out = plain(&t);
        assert!(out.contains("├── ○ first    ← next"), "{out}");
        assert!(
            out.ends_with("└── ○ second\n"),
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
        let out = plain(&t);
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
            plain(&t).contains("◌ waiting   blocked by gate-a, gate-b, ghost"),
            "{}",
            plain(&t)
        );
    }

    #[test]
    fn an_empty_tree_renders_the_header_alone() {
        assert_eq!(
            plain(&Tree::new("fresh".to_string())),
            "fresh  (0 answered, 0 open)\n"
        );
    }

    /// A hand-edited parent cycle is unreachable from any root, so its members
    /// are entered after the genuine roots rather than dropped — and the entry
    /// point is drawn as the sibling of those roots that it is walked as.
    #[test]
    fn a_parent_cycle_renders_every_head_once() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "a", None);
        add(&mut t, "b", Some("a"));
        add(&mut t, "loose", None);
        t.heads.get_mut("a").unwrap().parent = Some("b".to_string());

        assert_eq!(
            plain(&t),
            "\
t  (0 answered, 3 open)
├── ○ loose   ← next
└── ○ a
    └── ○ b
"
        );
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The escapes are zero characters wide to the terminal and non-zero to
    /// `chars().count()`, so the column has to be measured on the plain text.
    /// Stripping the paint back off must land exactly on the plain render.
    #[test]
    fn colour_does_not_move_the_column() {
        let tree = spec_example();
        assert_eq!(strip_ansi(&render(&tree, Colour::Ansi)), plain(&tree));
    }

    #[test]
    fn a_plain_render_carries_no_escapes() {
        assert!(!plain(&spec_example()).contains('\x1b'));
    }

    #[test]
    fn ansi_paints_the_state_on_the_glyph_and_dims_the_structure() {
        let out = render(&spec_example(), Colour::Ansi);
        assert!(
            out.contains("\u{1b}[2m    │   └── \u{1b}[0m\u{1b}[1;36m○\u{1b}[0m lifecycle"),
            "connectors dim, a ready glyph bold cyan, the slug left alone: {out}"
        );
        assert!(
            out.contains("\u{1b}[1m← next\u{1b}[0m"),
            "the marker is the one summary that is not context: {out}"
        );
        assert!(
            out.contains("\u{1b}[32m●\u{1b}[0m consumption-surface    \u{1b}[2mCLI unix tool"),
            "{out}"
        );
        assert!(out.contains("\u{1b}[33m◌\u{1b}[0m resume-shape"), "{out}");
        assert!(out.contains("\u{1b}[2;31m⊘\u{1b}[0m write-model"), "{out}");
    }

    /// A ready head that is not `next` ends at its slug, painted or not — no
    /// padding, and no escape sequence opened for a summary that is not there.
    #[test]
    fn a_row_with_no_summary_ends_at_its_slug() {
        let mut t = Tree::new("t".to_string());
        add(&mut t, "first", None);
        add(&mut t, "second", None);

        assert!(render(&t, Colour::Ansi).ends_with("\u{1b}[0m second\n"));
    }
}
