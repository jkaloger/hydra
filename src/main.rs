//! The clap surface of SPEC §5.
//!
//! JSON on stdout for every command but `tree`; messages, notes and rejections
//! on stderr. Stdout stays parseable whatever the exit code — `hydra status`
//! exits nonzero while open heads remain (§5) and its counts are still the whole
//! of stdout.

use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::Context;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;

use hydra::model::Tree;
use hydra::{
    Cauterise, Counts, Cut, Sprout, Store, graph, grill, hook, query, render, slug, store,
};

/// Exit codes. §5 gives `status` a nonzero-while-open contract and §4 gives every
/// invariant rejection a nonzero exit, so a caller has to be able to tell those
/// two apart from each other and from "there is no tree here". Distinct codes are
/// cheaper than parsing stderr.
mod exit {
    /// Success. For `status`, also "no open heads remain".
    pub const OK: i32 = 0;
    /// I/O, malformed JSON, or a lock that would not come free. Retrying, or
    /// fixing something outside hydra, may work.
    pub const FAILED: i32 = 1;
    /// Usage. clap's own code for a bad command line; hydra reports a `--reject`
    /// without a `:` and two `-` placeholders in one invocation the same way.
    pub const USAGE: i32 = 2;
    /// A §4 invariant refused the write, or a named head does not exist. The
    /// message on stderr names the offending slugs.
    pub const REJECTED: i32 = 3;
    /// `hydra status` only: open heads remain. A signal, not a failure.
    pub const OPEN: i32 = 4;
    /// No `.hydra/`, no `HEAD`, no such tree, or a tree written by a newer hydra.
    /// Nothing was addressed, so nothing was attempted.
    pub const NO_TREE: i32 = 5;
}

/// `hook` opts out of the table above and always exits `OK`.
///
/// The codes above address a caller who can read a message and try again. A hook
/// has no such caller: Claude Code reads the exit code as part of the protocol —
/// 2 means *block* on several events — and shows stderr from any other nonzero
/// code to a user who never asked for hydra. Since the plugin's hooks fire in
/// every project once installed (§6), the only safe contract is one JSON object on
/// stdout and 0, whatever the payload said and whatever is or is not in the repo.
/// See `hook.rs`.
const HOOKS_ALWAYS_SUCCEED: i32 = exit::OK;

/// The `<text|->` placeholder of §5.
const STDIN: &str = "-";

#[derive(Parser)]
#[command(
    name = "hydra",
    version,
    about = "Decision-tree store for AI-led design interviews.",
    long_about = "\
Decision-tree store for AI-led design interviews. Every open question is a head;
every answer is a cut that may sprout more.

JSON on stdout for every command but `tree`, which is for eyes. Rejections and
notes go to stderr, so stdout stays parseable at any exit code.

Prose arguments take `-` to read the value from stdin: --answer, --rationale,
--question and --why. Stdin can only be read once, so one `-` per invocation.

Exit codes:
  0  ok; for `status`, no open heads remain
  1  I/O, malformed JSON, or a lock that would not come free
  2  usage
  3  a slug was refused: an invariant (§4), or a head that is not there.
     stderr names the offending slugs
  4  `status` only: open heads remain
  5  tree addressing: no .hydra/, no HEAD, no such tree, one that already
     exists, or one written by a newer hydra

`hook` is exempt: it always exits 0 and writes one JSON object, because Claude
Code reads a hook's exit code as part of its own protocol."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a tree and point HEAD at it.
    ///
    /// Reuses the nearest .hydra/ at or above the cwd and creates one in the cwd
    /// only if there is none: discovery walks up, so a nested .hydra/ would
    /// shadow the record above it rather than extend it.
    Init {
        /// Tree slug. Defaults to the name of the directory holding .hydra/.
        slug: Option<String>,
    },

    /// Move HEAD to an existing tree.
    Use {
        /// The tree to make active.
        slug: String,
    },

    /// List every tree in the store with its counts, and which one HEAD names.
    Trees,

    /// Counts for the HEAD tree. Exits 4 while open heads remain.
    Status,

    /// Open a new head.
    Sprout {
        /// The question. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        question: String,
        /// Parent head. Omit for a root.
        #[arg(long, value_name = "SLUG")]
        parent: Option<String>,
        /// Heads that gate this one, comma-separated or repeated.
        #[arg(long = "blocked-by", value_delimiter = ',', value_name = "SLUG,...")]
        blocked_by: Vec<String>,
        /// Slug to file it under. Defaults to a slug derived from the question.
        #[arg(long, value_name = "SLUG")]
        slug: Option<String>,
    },

    /// Answer a head. Re-answering reopens its descendants and everything it
    /// gates.
    Cut {
        /// The head to answer.
        slug: String,
        /// The answer, decision first. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        answer: String,
        /// Why this answer. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        rationale: Option<String>,
        /// An option considered and killed, as "<option>: <why>". Repeatable.
        #[arg(long, value_name = "OPTION: WHY", value_parser = parse_reject)]
        reject: Vec<hydra::Rejected>,
        /// Skip the cascade. For typos and rewording.
        #[arg(long)]
        keep_subtree: bool,
        /// Answer over unanswered blockers. Records nothing: if you force it, you
        /// own it.
        #[arg(long)]
        force: bool,
    },

    /// Kill a question a sibling's answer made moot. It ends up answered, with
    /// `answer.cauterised_by` set.
    #[command(visible_alias = "sear")]
    Cauterise {
        /// The head to kill.
        slug: String,
        /// The answered head whose answer killed this question.
        #[arg(long, value_name = "SLUG")]
        by: String,
        /// Why it is moot; lands in `answer.rationale`. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        why: Option<String>,
        /// Cauterise by a head that is not answered yet. Records nothing.
        #[arg(long)]
        force: bool,
    },

    /// Withdraw an answer and ask the question again. Always cascades; the old
    /// answer is kept as `prior`.
    Reopen {
        /// The answered head to put back on the frontier.
        slug: String,
    },

    /// Change a head's question, leaving its answer alone.
    Reword {
        /// The head to reword.
        slug: String,
        /// The new question. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        question: String,
    },

    /// Move a head under a different parent.
    Reparent {
        /// The head to move.
        slug: String,
        /// The new parent. Pass '' to make the head a root.
        #[arg(long, value_name = "SLUG")]
        parent: String,
    },

    /// Add a `blocked_by` edge. Idempotent.
    Link {
        /// The head to gate.
        slug: String,
        /// The head that must be answered first.
        #[arg(long = "blocked-by", value_name = "SLUG")]
        blocked_by: String,
        /// Add an edge that closes a cycle. Records nothing.
        #[arg(long)]
        force: bool,
    },

    /// Remove a `blocked_by` edge. Idempotent.
    Unlink {
        /// The gated head.
        slug: String,
        /// The edge to drop.
        #[arg(long = "blocked-by", value_name = "SLUG")]
        blocked_by: String,
    },

    /// Open heads with their dependencies satisfied, in pre-order. `[]` when
    /// there are none.
    Ready,

    /// The first ready head in pre-order — document order, not priority. `null`
    /// when nothing can be asked.
    Next,

    /// One head, fully hydrated.
    Show {
        /// The head to hydrate.
        slug: String,
    },

    /// Cold-start payload: counts, next, a skeleton of every head, and full
    /// detail for next and its ancestors.
    Resume,

    /// ASCII render of the tree. The one command whose output is for eyes.
    Tree,

    /// Take or release the session lease that arms the hooks.
    Grill {
        #[command(subcommand)]
        command: GrillCommand,
    },

    /// Answer one Claude Code hook: hook JSON on stdin, hook JSON on stdout.
    ///
    /// Not a call you make by hand. It is what the plugin's hooks.json shells to,
    /// so it always exits 0 and always writes one JSON object — `{}` when no gate
    /// matched, which is the usual answer in a repo that has no .hydra/.
    ///
    /// EVENT is one of session-start, post-tool-use, stop. Anything else, an EVENT
    /// that is missing, and more than one of them all answer `{}` and exit 0 as
    /// well; see the note on the exit table.
    Hook {
        /// The event whose payload is on stdin: session-start, post-tool-use, stop.
        #[arg(value_name = "EVENT", trailing_var_arg = true)]
        event: Vec<String>,
    },
}

#[derive(Subcommand)]
enum GrillCommand {
    /// Take the lease: record this session as the one grilling the HEAD tree.
    ///
    /// The hooks compare their payload's session_id against the lease and stay
    /// silent unless it matches, so a lease left behind by a session that died can
    /// never fire. Nothing has to clean it up.
    ///
    /// Displaces a lease held by another session: there is one lease, and taking
    /// it is how a session says the interview is now its own.
    Start {
        /// The session to record. Defaults to $CLAUDE_CODE_SESSION_ID, which is
        /// the same id the hooks will report.
        #[arg(long, value_name = "ID")]
        session_id: Option<String>,
    },

    /// Release the lease. The kill switch: the hooks go quiet immediately.
    ///
    /// Reads neither HEAD nor any tree, so it works on a store nothing else will
    /// load. Exits 0 whether or not there was a lease to release.
    Stop,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli.command) {
        Ok(code) => code,
        Err(err) => report(err),
    };
    std::process::exit(code);
}

fn run(command: Command) -> anyhow::Result<i32> {
    match command {
        Command::Init { slug } => {
            let cwd = std::env::current_dir().context("resolving the current directory")?;
            let store = adopt_or_init(&cwd)?;
            let slug = match slug {
                Some(slug) => slug,
                None => default_slug(&store),
            };
            // `create` is the only public path to a new tree, and it refuses to
            // clobber, so HEAD only moves once the file is on disk.
            let tree = store.create(&slug)?;
            store.set_head(&slug)?;
            emit(&Mutation::new("init", &tree, None, vec![]))?;
        }

        Command::Use { slug } => {
            let store = Store::discover()?;
            // Load first: pointing HEAD at a tree that does not exist would break
            // every command after this one, and `use` is where it is cheap to
            // catch.
            let tree = store.load(&slug)?;
            store.set_head(&slug)?;
            emit(&Mutation::new("use", &tree, None, vec![]))?;
        }

        Command::Trees => {
            let store = Store::discover()?;
            let head = match store.head() {
                Ok(slug) => Some(slug),
                // A store with no HEAD yet still has trees worth listing.
                Err(hydra::Error::HeadUnset) => None,
                Err(err) => return Err(err.into()),
            };
            let trees = store
                .trees()?
                .into_iter()
                .map(|slug| Listed::read(&store, slug, head.as_deref()))
                .collect();
            emit(&Trees { head, trees })?;
        }

        Command::Status => {
            let tree = head_tree()?;
            let counts = query::status(&tree);
            let done = counts.done;
            emit(&counts)?;
            // §5's makefile-friendly signal. Deliberately not routed through the
            // error path: nothing went wrong, so stderr stays empty and the
            // counts are still on stdout.
            return Ok(if done { exit::OK } else { exit::OPEN });
        }

        Command::Sprout {
            mut question,
            parent,
            blocked_by,
            slug,
        } => {
            resolve_stdin("sprout", &mut [("--question", &mut question)])?;
            // §2 asks for a *short* kebab slug, and `--question -` makes the text
            // arbitrarily long, so a multi-line question narrows the derivation to
            // its first line. Only the basis narrows — the whole text is still
            // what gets stored. A single-line question is left to
            // `graph::sprout`, whose derivation is identical for it and which is
            // the only thing that can add the `-2` suffix on a collision.
            let first_line = query::first_line(&question);
            let slug = slug.or_else(|| (first_line != question).then(|| slug::slugify(first_line)));
            let (store, head) = head_store()?;
            let blockers: Vec<&str> = blocked_by.iter().map(String::as_str).collect();
            let mutation = store.with_tree_mut(&head, |tree| {
                let slug = graph::sprout(
                    tree,
                    Sprout {
                        question: &question,
                        parent: parent.as_deref(),
                        blocked_by: &blockers,
                        slug: slug.as_deref(),
                    },
                )?;
                Ok(Mutation::new("sprout", tree, Some(slug), vec![]))
            })?;
            emit(&mutation)?;
        }

        Command::Cut {
            slug,
            mut answer,
            mut rationale,
            reject,
            keep_subtree,
            force,
        } => {
            let mut fields: Vec<(&str, &mut String)> = vec![("--answer", &mut answer)];
            if let Some(rationale) = rationale.as_mut() {
                fields.push(("--rationale", rationale));
            }
            resolve_stdin("cut", &mut fields)?;

            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                let reopened = graph::cut(
                    tree,
                    Cut {
                        slug: &slug,
                        answer: &answer,
                        rationale: rationale.as_deref(),
                        rejected: reject,
                        keep_subtree,
                        force,
                    },
                )?;
                Ok(Mutation::new("cut", tree, Some(slug.clone()), reopened))
            })?;
            emit(&mutation)?;
        }

        Command::Cauterise {
            slug,
            by,
            mut why,
            force,
        } => {
            let mut fields: Vec<(&str, &mut String)> = Vec::new();
            if let Some(why) = why.as_mut() {
                fields.push(("--why", why));
            }
            resolve_stdin("cauterise", &mut fields)?;

            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                let reopened = graph::cauterise(
                    tree,
                    Cauterise {
                        slug: &slug,
                        by: &by,
                        why: why.as_deref(),
                        force,
                    },
                )?;
                Ok(Mutation::new(
                    "cauterise",
                    tree,
                    Some(slug.clone()),
                    reopened,
                ))
            })?;
            emit(&mutation)?;
        }

        Command::Reopen { slug } => {
            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                let reopened = graph::reopen(tree, &slug)?;
                Ok(Mutation::new("reopen", tree, Some(slug.clone()), reopened))
            })?;
            emit(&mutation)?;
        }

        Command::Reword { slug, mut question } => {
            resolve_stdin("reword", &mut [("--question", &mut question)])?;
            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                graph::reword(tree, &slug, &question)?;
                Ok(Mutation::new("reword", tree, Some(slug.clone()), vec![]))
            })?;
            emit(&mutation)?;
        }

        Command::Reparent { slug, parent } => {
            let (store, head) = head_store()?;
            // §5 gives `reparent` no flag for rooting a head, and an empty string
            // is not a legal slug (§2), so it is free to mean "no parent".
            let parent = Some(parent.as_str()).filter(|parent| !parent.is_empty());
            let mutation = store.with_tree_mut(&head, |tree| {
                graph::reparent(tree, &slug, parent)?;
                Ok(Mutation::new("reparent", tree, Some(slug.clone()), vec![]))
            })?;
            emit(&mutation)?;
        }

        Command::Link {
            slug,
            blocked_by,
            force,
        } => {
            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                graph::link(tree, &slug, &blocked_by, force)?;
                Ok(Mutation::new("link", tree, Some(slug.clone()), vec![]))
            })?;
            emit(&mutation)?;
        }

        Command::Unlink { slug, blocked_by } => {
            let (store, head) = head_store()?;
            let mutation = store.with_tree_mut(&head, |tree| {
                graph::unlink(tree, &slug, &blocked_by)?;
                Ok(Mutation::new("unlink", tree, Some(slug.clone()), vec![]))
            })?;
            emit(&mutation)?;
        }

        Command::Ready => {
            let tree = head_tree()?;
            // An empty frontier is an answer, not a failure: a done tree and an
            // empty tree both legitimately have nothing ready.
            emit(&query::ready(&tree))?;
        }

        Command::Next => {
            let tree = head_tree()?;
            // `null` and exit 0, for the same reason `ready` prints `[]`. Making
            // this nonzero would invert `status`, which exits 0 exactly when
            // there is nothing left to ask.
            emit(&query::next(&tree))?;
        }

        Command::Show { slug } => {
            let tree = head_tree()?;
            emit(&query::show(&tree, &slug)?)?;
        }

        Command::Resume => {
            let tree = head_tree()?;
            emit(&query::resume(&tree))?;
        }

        Command::Tree => {
            let tree = head_tree()?;
            // Not `print!`: `std::io::_print` panics on a write failure, and §6's
            // `PostToolUse` hook pipes this. `render` already ends in a newline.
            write_stdout(&render(&tree))?;
        }

        Command::Grill { command } => return run_grill(command),

        Command::Hook { event } => return Ok(run_hook(&event)),
    }
    Ok(exit::OK)
}

/// The lease of §6. Ordinary CLI calls, unlike `hook` — the skill runs these, and
/// a skill can read a message and a code.
fn run_grill(command: GrillCommand) -> anyhow::Result<i32> {
    match command {
        GrillCommand::Start { session_id } => {
            let store = Store::discover()?;
            let head = store.head()?;
            // Loaded before the lease is written, the way `use` loads before
            // moving HEAD: a lease naming a tree that will not load is a lease no
            // hook can act on.
            let tree = store.load(&head)?;
            let session_id = resolve_session_id(session_id);
            if let Some(displaced) = grill::read(&store).filter(|held| !held.holds(&session_id)) {
                // Deliberately not phrased as a competitor. Nothing releases the
                // lease when a session ends cleanly — correct per §6, since a stale
                // lease is inert — so this fires on every interview after the first
                // in any repo ever grilled, and `start` cannot tell a leftover from
                // a live holder without the liveness check §6 rules out. "Another
                // agent is grilling this repo right now" is the wrong thing for the
                // model to read out of the usual case.
                eprintln!(
                    "hydra: replacing a lease left by session {} on '{}'",
                    displaced.session_id, displaced.tree
                );
            }
            let lease = grill::start(&store, &session_id, &head)?;
            emit(&Grilling {
                op: "grill start",
                lease,
                counts: query::status(&tree),
            })?;
        }

        GrillCommand::Stop => {
            let store = Store::discover()?;
            emit(&Released {
                op: "grill stop",
                released: grill::stop(&store)?,
            })?;
        }
    }
    Ok(exit::OK)
}

/// One hook: payload on stdin, envelope on stdout, exit 0 (see
/// `HOOKS_ALWAYS_SUCCEED`).
///
/// Every failure here collapses to the same answer as every gate that did not
/// match — `{}` — so nothing reaches `report`, nothing reaches stderr, and a
/// stdout that has stopped listening is not worth a word either.
///
/// The event arrives as a `Vec<String>` and is resolved here rather than by a clap
/// `ValueEnum`, which is the difference between an unrecognised event answering
/// `{}` and clap refusing it with exit 2 before any of this runs. On `Stop`, exit 2
/// means *show stderr to the model and continue the conversation*, so a `hooks.json`
/// that says `hydra hook Stop` — the casing of the event *key* — would otherwise
/// turn into an unconditional, un-lease-gated refusal to end the turn, in every
/// project the plugin is installed in, with clap's usage text as the reason. The
/// same goes for a newer plugin naming an event an older binary has never heard of,
/// which §6 makes ordinary skew by shipping the binary as a separate prerequisite.
///
/// Stdin is drained first either way: the writer on the other end has a payload to
/// finish sending, whatever hydra makes of the arguments.
fn run_hook(event: &[String]) -> i32 {
    let mut raw = String::new();
    let payload = io::stdin()
        .read_to_string(&mut raw)
        .ok()
        .and_then(|_| hook::Payload::parse(&raw));
    // One argument, spelled exactly as §9 spells it. Nothing is matched loosely:
    // an event hydra half-recognises is one it would answer with the wrong gate.
    let event = match event {
        [only] => hook::Event::parse(only),
        _ => None,
    };
    let response = match (event, payload) {
        // Discovery failing is the ordinary case, not an error: most repos have no
        // `.hydra/` and the plugin's hooks fire in all of them.
        (Some(event), Some(payload)) => {
            hook::respond(Store::discover().ok().as_ref(), event, &payload)
        }
        _ => hook::Response::default(),
    };
    let _ = write_stdout(&format!("{}\n", hook::to_json(&response)));
    HOOKS_ALWAYS_SUCCEED
}

/// §5 gives `grill start` no arguments, so the id comes from the environment:
/// Claude Code exports `CLAUDE_CODE_SESSION_ID` into every command it runs, and it
/// is the same id the hooks report on stdin. `--session-id` drives the lease from
/// anything else.
///
/// Refused rather than defaulted when there is neither. A lease carrying an id no
/// hook will ever send is a lease that silently does nothing, which would make
/// §6's gating look broken from the inside; saying so lets the skill degrade the
/// way it already degrades when the binary is missing.
///
/// Reported as a usage error, like every other command-line mistake hydra spots for
/// itself: the fix is on the command line or in the environment that invoked it, not
/// in the store, so this is `usage` and exit 2 rather than exit 1.
fn resolve_session_id(given: Option<String>) -> String {
    let from_env = std::env::var(grill::SESSION_ENV).unwrap_or_default();
    let session_id = given.unwrap_or(from_env).trim().to_string();
    if session_id.is_empty() {
        usage(
            "grill start",
            &format!(
                "no session to grill: ${} is unset or empty, so pass --session-id",
                grill::SESSION_ENV
            ),
        );
    }
    session_id
}

/// The response shape of every mutation. §4's last line asks each one to echo the
/// tree it wrote to, so a stale `HEAD` surfaces immediately; `counts` rides along
/// because the interview loop's next question is always "is there more to ask?",
/// and answering it here saves a round trip. `tree` and `counts.tree` are the
/// same string.
#[derive(Serialize)]
struct Mutation {
    tree: String,
    /// The verb, so a log of responses is legible without the commands.
    op: &'static str,
    /// The head written, absent for `init` and `use`, which write `HEAD` rather
    /// than a head. `sprout` reports the slug it chose, which the caller may not
    /// have supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<String>,
    /// The cascade: heads this write reopened, sorted, excluding the head named
    /// in `slug`. Always present, empty when there was none.
    reopened: Vec<String>,
    counts: Counts,
}

impl Mutation {
    fn new(op: &'static str, tree: &Tree, slug: Option<String>, reopened: Vec<String>) -> Self {
        Mutation {
            tree: tree.slug.clone(),
            op,
            slug,
            reopened,
            counts: query::status(tree),
        }
    }
}

/// `grill start`'s response: the lease as §6 stores it, flattened in, plus the
/// counts of the tree it was taken on — the skill's next question after taking the
/// lease is always "is there anything to ask?" (§6's interview protocol).
#[derive(Serialize)]
struct Grilling {
    op: &'static str,
    #[serde(flatten)]
    lease: grill::Lease,
    counts: Counts,
}

/// `grill stop`'s response. No tree echo: the kill switch reads no tree, on
/// purpose.
#[derive(Serialize)]
struct Released {
    op: &'static str,
    /// What was let go, `null` when there was no lease — or when the file was
    /// there but was not a lease, which is removed all the same.
    released: Option<grill::Lease>,
}

#[derive(Serialize)]
struct Trees {
    /// `null` when `.hydra/HEAD` is missing.
    head: Option<String>,
    trees: Vec<Listed>,
}

/// One row of `trees`. Counts are nested rather than flattened so that a tree
/// which will not load still gets a row: this is the command you reach for to
/// find out *which* file is broken, so it must not be the command that dies on
/// the answer. Exit stays 0 — the store was listed; `error` is where a caller
/// checking for corruption looks.
#[derive(Serialize)]
struct Listed {
    tree: String,
    /// Whether `HEAD` names this tree.
    current: bool,
    /// Absent when `error` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Counts>,
    /// Why this tree could not be read. Absent when it could.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Listed {
    fn read(store: &Store, slug: String, head: Option<&str>) -> Self {
        let (counts, error) = match store.load(&slug) {
            Ok(tree) => (Some(query::status(&tree)), None),
            Err(err) => (None, Some(err.to_string())),
        };
        Listed {
            current: head == Some(slug.as_str()),
            tree: slug,
            counts,
            error,
        }
    }
}

/// Every command but `init` operates on the tree `HEAD` names (§5).
fn head_tree() -> hydra::Result<Tree> {
    let store = Store::discover()?;
    store.load(&store.head()?)
}

/// The mutation half of `head_tree`: the tree is loaded inside
/// `with_tree_mut`, under the lock, so this hands back the slug rather than a
/// snapshot that would already be stale.
fn head_store() -> hydra::Result<(Store, String)> {
    let store = Store::discover()?;
    let slug = store.head()?;
    Ok((store, slug))
}

/// `Store::discover` walks up (§3), so creating `.hydra/` in the cwd when one
/// already exists above it would shadow the record instead of extending it —
/// silently, which is the failure §4 exists to prevent. Adopt the one that is
/// there, and say so, since a store outside the cwd is the surprising case.
fn adopt_or_init(cwd: &Path) -> hydra::Result<Store> {
    match Store::discover_from(cwd) {
        Ok(store) => {
            if store.dir() != cwd.join(store::DIR) {
                eprintln!("hydra: using the store at {}", store.dir().display());
            }
            Ok(store)
        }
        Err(hydra::Error::NoStore { .. }) => Store::init(cwd),
        Err(err) => Err(err),
    }
}

/// §5 leaves `init`'s default slug unspecified. The name of the directory holding
/// `.hydra/` is the only thing to hand that means anything — these decisions are
/// about this code (§3) — and taking it from the store rather than from the cwd
/// makes the default the same from anywhere in the repo, so a second bare `init`
/// is refused as a duplicate instead of quietly making a tree named after a
/// subdirectory. Falls back for a directory whose name slugifies to nothing.
fn default_slug(store: &Store) -> String {
    let derived = store
        .dir()
        .parent()
        .and_then(Path::file_name)
        .map(|name| slug::slugify(&name.to_string_lossy()))
        .unwrap_or_default();
    if derived.is_empty() {
        "hydra".to_string()
    } else {
        derived
    }
}

/// JSON on stdout (§5), serialized straight from the type. Never through
/// `store::to_json`: its `Value` round-trip is what sorts §3's stored keys, and
/// it would re-sort `Resume`, whose field order is deliberate (§7) and pinned by
/// a test in `query`.
fn emit<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let mut json = serde_json::to_string_pretty(value).context("serializing the response")?;
    json.push('\n');
    write_stdout(&json)
}

/// The only place stdout is written — `tree` included, even though it is the one
/// command whose output is not JSON — so `BrokenPipe` has exactly one place to
/// arise and `report` exactly one place to forgive it.
fn write_stdout(text: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .context("writing to stdout")
}

/// Replaces each `-` placeholder with stdin (§5).
///
/// Stdin can be read once, so two placeholders in one invocation is a usage
/// error rather than one field silently swallowing the stream and the other
/// getting nothing. `--question`, `--rationale` and `--why` get the same
/// treatment as `--answer` for the reason §5 gives for `--answer`: they are prose
/// and will contain quotes.
fn resolve_stdin(verb: &str, fields: &mut [(&str, &mut String)]) -> anyhow::Result<()> {
    let asking: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, (_, value))| value.as_str() == STDIN)
        .map(|(at, _)| at)
        .collect();
    match asking.as_slice() {
        [] => Ok(()),
        // Exactly one field, so it is assigned rather than broadcast: the arm
        // below has already left for anything more.
        &[at] => {
            let text = read_stdin()?;
            if text.is_empty() {
                usage(
                    verb,
                    &format!("{} - was given, but stdin was empty", fields[at].0),
                );
            }
            *fields[at].1 = text;
            Ok(())
        }
        many => usage(
            verb,
            &format!(
                "only one field can read stdin, and {} both ask for -",
                many.iter()
                    .map(|&at| fields[at].0)
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        ),
    }
}

fn read_stdin() -> anyhow::Result<String> {
    let mut text = String::new();
    io::stdin()
        .read_to_string(&mut text)
        .context("reading stdin")?;
    // Trailing whitespace is an artifact of the pipe rather than part of the
    // prose; everything inside is kept, since these fields are multi-line by
    // design (§7's first-line summary).
    Ok(text.trim_end().to_string())
}

/// `--reject "<option>: <why>"` (§5), split on the first `:` so a `why` may
/// contain more of them.
///
/// A value with no `:` is a usage error rather than a rejection with an empty
/// `why_not`: `rejected[]` exists to stop a future session re-proposing a killed
/// option (§2), and it cannot do that without the reason. Same for either half
/// being blank.
fn parse_reject(raw: &str) -> Result<hydra::Rejected, String> {
    let malformed = |why: &str| format!("expected \"<option>: <why>\", got {raw:?}: {why}");
    let (option, why_not) = raw
        .split_once(':')
        .ok_or_else(|| malformed("no ':' to split on"))?;
    let (option, why_not) = (option.trim(), why_not.trim());
    if option.is_empty() {
        return Err(malformed("the option is empty"));
    }
    if why_not.is_empty() {
        return Err(malformed("the reason is empty"));
    }
    Ok(hydra::Rejected {
        option: option.to_string(),
        why_not: why_not.to_string(),
    })
}

/// Reported the way clap reports a bad flag, so `exit::USAGE` covers every
/// command-line mistake rather than only the ones clap can see for itself.
///
/// Raised against the subcommand rather than the root, so the usage line names
/// the verb that was misused instead of `hydra <COMMAND>`. Exited by hand rather
/// than through `Error::exit` so the code comes from the table above instead of
/// from clap's default.
///
/// `verb` may be a path — "grill start" — for a nested subcommand.
fn usage(verb: &str, message: &str) -> ! {
    let mut root = Cli::command();
    let mut found = &mut root;
    for name in verb.split(' ') {
        found = found
            .find_subcommand_mut(name)
            .expect("a verb clap just parsed");
    }
    // `bin_name` by hand: a subcommand plucked out of the root has not been
    // through clap's build, so its usage line would read `cut …` with no `hydra`.
    let mut cmd = found.clone().bin_name(format!("hydra {verb}"));
    let err = cmd.error(ErrorKind::ValueValidation, message);
    let _ = err.print();
    std::process::exit(exit::USAGE);
}

/// §4: every rejection exits nonzero with a message naming the offending slugs.
/// The `Error` variants name them structurally, so this renders them rather than
/// reformatting them.
fn report(err: anyhow::Error) -> i32 {
    // A reader that stopped reading is not hydra failing: `hydra tree | head -1`
    // and `hydra resume | jq -e .next` both close the pipe early on purpose, and
    // every unix filter treats that as success. Silent, because the message would
    // go to a terminal that asked for less output, not more.
    if let Some(io) = err.downcast_ref::<io::Error>()
        && io.kind() == io::ErrorKind::BrokenPipe
    {
        return exit::OK;
    }
    match err.downcast_ref::<hydra::Error>() {
        // Plain `{err}`, not the alternate form: the `Io` and `Json` variants
        // already fold their source into their own message, so chaining would
        // print it twice.
        Some(err) => {
            eprintln!("hydra: {err}");
            code(err)
        }
        None => {
            eprintln!("hydra: {err:#}");
            exit::FAILED
        }
    }
}

fn code(err: &hydra::Error) -> i32 {
    use hydra::Error;
    match err {
        Error::NoStore { .. }
        | Error::HeadUnset
        | Error::UnknownTree { .. }
        | Error::TreeExists { .. }
        | Error::UnsupportedVersion { .. } => exit::NO_TREE,

        // §4's rejections, plus naming a head that is not there — the same class
        // of mistake, told apart from a missing *tree* because the fix is
        // different: check the slug, not the store.
        Error::MalformedSlug { .. }
        | Error::DuplicateSlug { .. }
        | Error::UnknownHead { .. }
        | Error::UnknownParent { .. }
        | Error::UnknownBlocker { .. }
        | Error::BlockCycle { .. }
        | Error::ParentCycle { .. }
        | Error::ReopenCycle { .. }
        | Error::BlockedCut { .. }
        | Error::IllegalTransition { .. }
        | Error::CauteriseByUnanswered { .. }
        | Error::SelfCauterise { .. } => exit::REJECTED,

        Error::Io { .. } | Error::Json { .. } | Error::LockTimeout { .. } => exit::FAILED,
    }
}
