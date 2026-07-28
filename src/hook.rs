//! Claude Code's hook protocol: SPEC §6's hook table and §9's `hydra hook
//! <event>`.
//!
//! The one place hydra speaks someone else's JSON. §9 puts the whole point of
//! doing it in Rust rather than in `jq` and bash on the gating being testable, so
//! everything here is a pure function of a payload and a store, and the envelope
//! shapes are pinned against literal JSON below.
//!
//! Robustness is the point of the module, not a polish pass on it. Plugin hooks
//! fire in every project once the plugin is installed (§6: "gating is
//! load-bearing"), and a hook that errors, hangs or emits garbage breaks
//! unrelated sessions in unrelated repos. So nothing here returns an error:
//! unparseable stdin, no `.hydra/` at all — the common case, since most repos are
//! not hydra repos — a lease that does not match, a corrupt tree, all of them end
//! in `Response::default()`, which serializes to `{}`. Claude Code reads that as a
//! hook with nothing to say.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Tree;
use crate::render::render;
use crate::store::Store;
use crate::{grill, query};

/// §6's three events, which are also the `hookEventName` each one must echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SessionStart,
    PostToolUse,
    Stop,
}

impl Event {
    /// The `hydra hook <event>` spelling of §9, exactly. `None` for anything else,
    /// which the CLI answers with `{}` rather than a usage error — see `run_hook`.
    ///
    /// Deliberately not case-folded or otherwise forgiving: an event hydra half
    /// recognises is one it would answer with the wrong gate, and `Stop` is both a
    /// plausible mis-spelling of `stop` and the name of a different thing (the
    /// `hooks.json` event key).
    pub fn parse(verb: &str) -> Option<Event> {
        match verb {
            "session-start" => Some(Event::SessionStart),
            "post-tool-use" => Some(Event::PostToolUse),
            "stop" => Some(Event::Stop),
            _ => None,
        }
    }

    /// The `hookEventName` Claude Code checks the response against.
    pub fn name(self) -> &'static str {
        match self {
            Event::SessionStart => "SessionStart",
            Event::PostToolUse => "PostToolUse",
            Event::Stop => "Stop",
        }
    }
}

/// The `Bash` matcher of §6's `PostToolUse` row.
const BASH: &str = "Bash";

/// The fields of the hook payload that §6's gates read, flattened.
///
/// Every one is absent-tolerant, because a payload from an event or a Claude Code
/// version this does not know about must read as "no gate matched" rather than as
/// a parse failure. Empty means absent throughout, and empty never matches
/// anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payload {
    pub session_id: String,
    /// Which event Claude Code thinks it is running.
    pub hook_event_name: String,
    /// `SessionStart`: `startup`, `resume`, `clear`, `compact` — or something a
    /// later Claude Code adds, which is why it stays a string.
    pub source: String,
    pub tool_name: String,
    /// `tool_input.command`, the only part of `tool_input` §6 gates on.
    pub command: String,
    /// Set by Claude Code on the `Stop` that follows a stop a hook already
    /// blocked. §6's "at most one block per turn" is this flag: the hook is a
    /// fresh process every time and `Stop` fires again after a block, so the
    /// count cannot live in hydra.
    pub stop_hook_active: bool,
}

impl Payload {
    /// `None` when stdin was not a JSON object — which is a clean no-op, not an
    /// error (see the module note).
    ///
    /// Fields are picked out of a `Value` rather than deserialized into a struct
    /// so that one field of an unexpected type cannot fail the whole parse. A
    /// numeric `session_id` from some future Claude Code should cost this hook the
    /// gate it could not read, not every gate it could.
    pub fn parse(raw: &str) -> Option<Payload> {
        let value: Value = serde_json::from_str(raw).ok()?;
        let object = value.as_object()?;
        let text = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Some(Payload {
            session_id: text("session_id"),
            hook_event_name: text("hook_event_name"),
            source: text("source"),
            tool_name: text("tool_name"),
            command: object
                .get("tool_input")
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stop_hook_active: object
                .get("stop_hook_active")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Claude Code's hook output envelope, narrowed to the three fields §6 uses.
///
/// `camelCase` is Claude Code's, not hydra's — the payload on the way in is
/// `snake_case` and the response on the way out is not. Every field is skipped
/// when absent, so the default serializes to `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// `"block"`, and only ever from the `Stop` gate. Top-level rather than inside
    /// `hookSpecificOutput`: `decision` is not event-scoped in Claude Code's
    /// schema, and `reason` is its companion — the text the model is handed when
    /// the stop is refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Shown to the user rather than to the model. §6 uses it for the two rows
    /// addressed to a human: the open-heads one-liner and the `hydra tree` render.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<Specific>,
}

/// The event-scoped half of the envelope. `hookEventName` is mandatory and is
/// checked by Claude Code against the event it invoked, so it comes from
/// `Event::name` rather than from a literal at each use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Specific {
    pub hook_event_name: String,
    pub additional_context: String,
}

/// The bytes for stdout, with no way to fail: a hook that cannot serialize its own
/// response still owes Claude Code well-formed silence.
pub fn to_json(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| "{}".to_string())
}

/// §6's table. `store` is `None` when there is no `.hydra/` above the cwd.
pub fn respond(store: Option<&Store>, event: Event, payload: &Payload) -> Response {
    // A `hooks.json` that wires the wrong verb to an event is a plugin bug, and
    // this must not turn it into a decision on someone else's tool call:
    // `decision: "block"` is not event-scoped, so a `Stop` response emitted on a
    // `PreToolUse` would deny it. Absent is not a mismatch — a payload driven by
    // hand need not carry the field.
    if !payload.hook_event_name.is_empty() && payload.hook_event_name != event.name() {
        return Response::default();
    }
    match event {
        Event::SessionStart => session_start(store, payload),
        Event::PostToolUse => post_tool_use(store, payload),
        Event::Stop => stop(store, payload),
    }
}

/// Both `SessionStart` rows of §6, told apart by `source` — one invocation, two
/// gates.
fn session_start(store: Option<&Store>, payload: &Payload) -> Response {
    match payload.source.as_str() {
        // A session that has just started has no lease of its own yet, and one
        // left by a crashed session cannot match its `session_id`, so this row is
        // gated on the tree instead: §6 asks whether the HEAD tree has open heads.
        "startup" | "resume" => match head_tree(store) {
            Some(tree) => open_heads_notice(&tree),
            None => Response::default(),
        },

        // §6: "The compact gate matters most." Context death by compaction is far
        // more common than session death, and the `session_id` survives it — so
        // the lease is exactly what says this reload belongs to a grilling
        // session, and what keeps a `/clear` in an unrelated repo silent.
        "compact" | "clear" => match leased_tree(store, payload) {
            Some(tree) => reload(&tree, &payload.source),
            None => Response::default(),
        },

        // A source §6's table has no row for: `fork`, or whatever a later Claude
        // Code adds. Silence is the only safe reading of an event this does not
        // understand.
        _ => Response::default(),
    }
}

/// §6: `hydra: 6 open heads on 'hydra-design' — /hydra to resume`.
///
/// A `systemMessage` and not `additionalContext`: the line ends by telling
/// somebody to type `/hydra`, and §6 spells `additionalContext` out on the row
/// where it does mean the model.
fn open_heads_notice(tree: &Tree) -> Response {
    let counts = query::status(tree);
    if counts.open == 0 {
        return Response::default();
    }
    Response {
        system_message: Some(format!(
            "hydra: {} open head{} on '{}' — /hydra to resume",
            counts.open,
            plural(counts.open),
            tree.slug
        )),
        ..Response::default()
    }
}

/// §6: the full `hydra resume` payload into `additionalContext`.
///
/// Prefixed with one line of framing. §7's payload is the whole point of the row,
/// but after a compaction it is arriving at a model that does not know it forgot
/// anything, and raw JSON with no framing is the one case where that matters.
fn reload(tree: &Tree, source: &str) -> Response {
    let Ok(resume) = serde_json::to_string_pretty(&query::resume(tree)) else {
        return Response::default();
    };
    Response {
        hook_specific_output: Some(Specific {
            hook_event_name: Event::SessionStart.name().to_string(),
            additional_context: format!(
                "hydra: context was reset ({source}) during an interview on '{}'. \
                 Below is `hydra resume`: the skeleton of every head, and full \
                 detail for `next` and its ancestors. Carry on from `next`.\n\n\
                 {resume}",
                tree.slug
            ),
        }),
        ..Response::default()
    }
}

/// §6: `systemMessage` renders `hydra tree` to the user after a `hydra ` command.
///
/// No lease — §6: "only a grilling session runs `hydra cut`", so the command
/// string is the gate. The `HEAD` tree, for the same reason: whatever the command
/// touched, it touched through `HEAD`.
fn post_tool_use(store: Option<&Store>, payload: &Payload) -> Response {
    // §6 gives the row a `Bash` matcher and gates on the command, so both are
    // checked: another tool with a `command` in its input is not the row's
    // subject, and gating in the hook rather than trusting the matcher is what
    // makes this safe to install everywhere.
    if payload.tool_name != BASH || !runs_hydra(&payload.command) {
        return Response::default();
    }
    match head_tree(store) {
        Some(tree) => Response {
            // The render ends in a newline; a displayed message should not.
            system_message: Some(render(&tree).trim_end().to_string()),
            ..Response::default()
        },
        None => Response::default(),
    }
}

/// §6's gate: the command contains `hydra `.
///
/// Read as the whole word `hydra`, bounded at both ends, rather than as the literal
/// six characters §6 writes. The trailing space in §6 is doing word-boundary work,
/// and doing that work properly is what catches the invocation forms a plain
/// substring misses — `"$HOME/bin/hydra" cut x` and `$(which hydra) next` contain
/// no `hydra ` at all, and skipping the render there is the false-negative
/// direction that actually costs the user something. It also rules out
/// `hydra-plugin/hooks.json` and `cat .hydra/HEAD`, which the substring would have
/// let through only by accident of punctuation.
///
/// Every occurrence is tested, not the first: `git-hydra sync && hydra cut a` is
/// one command with a name-attached false lead in front of a real invocation.
///
/// Hand-rolled rather than a regex, for the reason §9 gives for slug validation.
/// Consequence taken knowingly: a bare `hydra`, which prints help and changes
/// nothing, now matches, as does `echo hydra is a store`. A false positive costs
/// one `hydra tree` render in a repo that has a tree and nothing at all in a repo
/// that does not.
fn runs_hydra(command: &str) -> bool {
    command.match_indices("hydra").any(|(at, name)| {
        let before = command[..at].chars().next_back();
        let after = command[at + name.len()..].chars().next();
        !before.is_some_and(part_of_a_longer_name) && !after.is_some_and(part_of_a_longer_name)
    })
}

/// A character that makes `hydra` part of some other name: `myhydra`, `git-hydra`,
/// `foo.hydra`, `hydra-plugin`, `.hydra/HEAD`.
fn part_of_a_longer_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// §6: `decision: "block"` plus `hydra next`, under a live lease.
fn stop(store: Option<&Store>, payload: &Payload) -> Response {
    // §6's "at most one block per turn". Claude Code sets this on the `Stop` that
    // follows a stop a hook blocked, and it is the only turn-scoped memory
    // available to a process that starts again for every event.
    if payload.stop_hook_active {
        return Response::default();
    }
    let Some((store, lease)) = leased(store, payload) else {
        return Response::default();
    };
    // The one row that has to agree with `HEAD` as well as with the lease. Blocking
    // hands the model a head to go and answer, and the `hydra next` and `hydra cut`
    // it reaches for resolve through `HEAD` — so if something moved `HEAD` mid
    // interview (a second agent in the same repo, per §9, or a human shell running
    // `use` or `init`) a block from the lease's tree names a head those commands
    // cannot see. The model would have nothing to act on, stop, be blocked again,
    // and go round to Claude Code's block cap. Saying nothing is the safe reading:
    // the interview's tree is not the active one, and `hydra use` puts it back.
    if store.head().ok().as_deref() != Some(lease.tree.as_str()) {
        return Response::default();
    }
    let Some(tree) = store.load(&lease.tree).ok() else {
        return Response::default();
    };
    // §6 gates this row on the lease alone, but a block with no question to hand
    // over would wall the session in: `next` is `None` for a done tree, and also
    // for a corrupt one where every open head is blocked (see `query::next`).
    // Either way there is nothing to be relentless about, and `hydra grill stop`
    // should not be the only way out.
    let Some(next) = query::next(&tree) else {
        return Response::default();
    };
    let Ok(next) = serde_json::to_string_pretty(&next) else {
        return Response::default();
    };
    let counts = query::status(&tree);
    Response {
        decision: Some("block".to_string()),
        reason: Some(format!(
            "hydra: {} open head{} on '{}' — the interview is not finished. Ask the \
             head below rather than summarising or wrapping up. `hydra grill stop` \
             ends the interview.\n\n{next}",
            counts.open,
            plural(counts.open),
            tree.slug
        )),
        ..Response::default()
    }
}

/// The tree `HEAD` names, or `None` if anything at all is in the way.
fn head_tree(store: Option<&Store>) -> Option<Tree> {
    let store = store?;
    store.load(&store.head().ok()?).ok()
}

/// The lease, when this payload's session is the one holding it.
fn leased<'s>(store: Option<&'s Store>, payload: &Payload) -> Option<(&'s Store, grill::Lease)> {
    let store = store?;
    let lease = grill::read(store)?;
    lease.holds(&payload.session_id).then_some((store, lease))
}

/// The tree named by a lease this session holds.
///
/// The lease's tree, not `HEAD`'s: §6 records `tree` in the lease precisely so
/// that a reload knows what was being grilled, and `HEAD` may have moved since.
/// Right for a reload, which only reads; `stop` needs more than this, because it
/// hands the model a head to act on through `HEAD`.
fn leased_tree(store: Option<&Store>, payload: &Payload) -> Option<Tree> {
    let (store, lease) = leased(store, payload)?;
    store.load(&lease.tree).ok()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{self, Cut, Sprout};
    use std::fs;
    use tempfile::TempDir;

    const SESSION: &str = "abc123";

    /// A store with `hydra-design` under `HEAD`: two heads, one answered.
    fn store() -> (TempDir, Store) {
        let root = TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        store.create("hydra-design").unwrap();
        store.set_head("hydra-design").unwrap();
        store
            .with_tree_mut("hydra-design", |tree| {
                add(tree, "consumption-surface", None);
                add(tree, "graph-shape", Some("consumption-surface"));
                answer(tree, "consumption-surface", "CLI unix tool");
                Ok(())
            })
            .unwrap();
        (root, store)
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

    fn take_lease(store: &Store, session_id: &str) {
        grill::start(store, session_id, "hydra-design").unwrap();
    }

    fn payload(event: Event) -> Payload {
        Payload {
            session_id: SESSION.to_string(),
            hook_event_name: event.name().to_string(),
            ..Payload::default()
        }
    }

    fn session_start(source: &str) -> Payload {
        Payload {
            source: source.to_string(),
            ..payload(Event::SessionStart)
        }
    }

    fn bash(command: &str) -> Payload {
        Payload {
            tool_name: BASH.to_string(),
            command: command.to_string(),
            ..payload(Event::PostToolUse)
        }
    }

    /// Every assertion goes through the serialized form: a renamed key is exactly
    /// what these tests exist to fail on, and the struct field would not notice.
    fn json(store: &Store, event: Event, payload: &Payload) -> String {
        to_json(&respond(Some(store), event, payload))
    }

    const NOTHING: &str = "{}";

    /// The CLI answers an unrecognised event with `{}`, so this is where the set of
    /// recognised ones is pinned. Half of the pair — that clap does not reject the
    /// string first — can only be checked from outside the binary; `scripts/smoke.sh`
    /// does that.
    #[test]
    fn only_the_exact_event_spellings_are_recognised() {
        assert_eq!(Event::parse("session-start"), Some(Event::SessionStart));
        assert_eq!(Event::parse("post-tool-use"), Some(Event::PostToolUse));
        assert_eq!(Event::parse("stop"), Some(Event::Stop));
        for verb in [
            "Stop",
            "STOP",
            "SessionStart",
            "session_start",
            "sessionstart",
            "pre-compact",
            "stop ",
            " stop",
            "stop extra",
            "",
        ] {
            assert_eq!(Event::parse(verb), None, "{verb:?}");
        }
        assert_eq!(
            Event::parse("stop").unwrap().name(),
            "Stop",
            "the verb hydra is called by and the name Claude Code checks are not the same string"
        );
    }

    #[test]
    fn a_payload_that_is_not_a_json_object_is_no_payload() {
        for raw in ["", "   ", "{ not json", "[]", "42", r#""a string""#, "null"] {
            assert_eq!(Payload::parse(raw), None, "{raw:?}");
        }
        assert_eq!(
            Payload::parse("{}"),
            Some(Payload::default()),
            "an empty object is a payload with every gate absent"
        );
    }

    #[test]
    fn a_payload_keeps_the_fields_it_can_read_and_ignores_the_rest() {
        let parsed = Payload::parse(
            r#"{
              "session_id": "abc123",
              "transcript_path": "/tmp/t.jsonl",
              "cwd": "/repo",
              "hook_event_name": "PostToolUse",
              "tool_name": "Bash",
              "tool_input": {"command": "hydra cut graph-shape --answer x", "timeout": 5},
              "tool_response": {"stdout": "{}"},
              "permission_mode": "default"
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.session_id, "abc123");
        assert_eq!(parsed.hook_event_name, "PostToolUse");
        assert_eq!(parsed.tool_name, "Bash");
        assert_eq!(parsed.command, "hydra cut graph-shape --answer x");
        assert!(!parsed.stop_hook_active);

        // One field of the wrong type must cost only that field: a hook that
        // stopped gating because `session_id` became a number would start
        // blocking stops in every repo.
        let odd = Payload::parse(
            r#"{"session_id": 7, "hook_event_name": "Stop", "stop_hook_active": true,
                "tool_input": null, "source": ["compact"]}"#,
        )
        .unwrap();
        assert_eq!(odd.session_id, "");
        assert_eq!(odd.hook_event_name, "Stop");
        assert_eq!(odd.source, "");
        assert_eq!(odd.command, "");
        assert!(odd.stop_hook_active);
    }

    #[test]
    fn the_verb_must_agree_with_the_event_in_the_payload() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        let stop = Payload {
            hook_event_name: "PreToolUse".to_string(),
            ..payload(Event::Stop)
        };
        assert_eq!(
            json(&store, Event::Stop, &stop),
            NOTHING,
            "a mis-wired hooks.json must not deny somebody's tool call"
        );

        let unnamed = Payload {
            hook_event_name: String::new(),
            ..payload(Event::Stop)
        };
        assert!(
            json(&store, Event::Stop, &unnamed).contains("block"),
            "absent is not a mismatch"
        );
    }

    #[test]
    fn session_start_announces_open_heads_on_startup_and_resume() {
        let (_root, store) = store();
        let expected =
            r#"{"systemMessage":"hydra: 1 open head on 'hydra-design' — /hydra to resume"}"#;
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("startup")),
            expected
        );
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("resume")),
            expected
        );

        store
            .with_tree_mut("hydra-design", |tree| {
                add(tree, "storage-format", None);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("startup")),
            r#"{"systemMessage":"hydra: 2 open heads on 'hydra-design' — /hydra to resume"}"#,
            "§6's example is plural; one head is not"
        );
    }

    /// The gate is the tree, not the lease: a session that has just started has no
    /// lease, and the notice still has to arrive.
    #[test]
    fn session_start_needs_no_lease_for_the_one_liner() {
        let (_root, store) = store();
        take_lease(&store, "some-other-session");
        let anonymous = Payload {
            session_id: String::new(),
            ..session_start("startup")
        };
        assert!(json(&store, Event::SessionStart, &anonymous).contains("1 open head"));
    }

    #[test]
    fn session_start_says_nothing_about_a_done_tree() {
        let (_root, store) = store();
        store
            .with_tree_mut("hydra-design", |tree| {
                answer(tree, "graph-shape", "spanning tree + blocked_by");
                Ok(())
            })
            .unwrap();
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("startup")),
            NOTHING
        );
    }

    #[test]
    fn session_start_reloads_resume_after_a_compact_or_clear() {
        let (_root, store) = store();
        take_lease(&store, SESSION);

        for source in ["compact", "clear"] {
            let raw = json(&store, Event::SessionStart, &session_start(source));
            let response: Response = serde_json::from_str(&raw).unwrap();
            let specific = response.hook_specific_output.as_ref().unwrap();
            assert_eq!(specific.hook_event_name, "SessionStart");
            assert_eq!(
                (
                    &response.decision,
                    &response.reason,
                    &response.system_message
                ),
                (&None, &None, &None),
                "the model is the audience here, not the user"
            );

            let resume =
                serde_json::to_string_pretty(&query::resume(&store.load("hydra-design").unwrap()))
                    .unwrap();
            assert!(
                specific.additional_context.ends_with(&resume),
                "§6: the full resume payload, framed by one line\n{}",
                specific.additional_context
            );
            assert!(specific.additional_context.starts_with(&format!(
                "hydra: context was reset ({source}) during an interview on 'hydra-design'."
            )));

            // The envelope keys, spelled out, so renaming one fails here.
            assert_eq!(
                raw,
                format!(
                    r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{}}}}}"#,
                    serde_json::to_string(&specific.additional_context).unwrap()
                )
            );
        }
    }

    #[test]
    fn session_start_reloads_the_tree_the_lease_names_not_head() {
        let (_root, store) = store();
        store.create("storage-format").unwrap();
        take_lease(&store, SESSION);
        store.set_head("storage-format").unwrap();

        let raw = json(&store, Event::SessionStart, &session_start("compact"));
        assert!(
            raw.contains("consumption-surface"),
            "§6 records `tree` in the lease so a reload survives HEAD moving\n{raw}"
        );
    }

    #[test]
    fn the_compact_reload_needs_a_lease_that_matches() {
        let (_root, store) = store();
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("compact")),
            NOTHING,
            "no lease"
        );

        take_lease(&store, "another-session");
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("compact")),
            NOTHING,
            "a lease another session holds — the stale case, inert without cleanup"
        );

        let anonymous = Payload {
            session_id: String::new(),
            ..session_start("compact")
        };
        assert_eq!(
            json(&store, Event::SessionStart, &anonymous),
            NOTHING,
            "no session_id on the payload"
        );

        take_lease(&store, SESSION);
        assert_ne!(
            json(&store, Event::SessionStart, &session_start("compact")),
            NOTHING
        );
    }

    #[test]
    fn an_unknown_session_start_source_says_nothing() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        // `fork` is a source Claude Code sends and §6's table has no row for.
        for source in ["fork", "", "STARTUP", "compaction", "resumed"] {
            assert_eq!(
                json(&store, Event::SessionStart, &session_start(source)),
                NOTHING,
                "source {source:?}"
            );
        }
    }

    #[test]
    fn post_tool_use_renders_the_tree_to_the_user() {
        let (_root, store) = store();
        let raw = json(
            &store,
            Event::PostToolUse,
            &bash("hydra cut consumption-surface --answer 'CLI unix tool'"),
        );
        let response: Response = serde_json::from_str(&raw).unwrap();
        let rendered = response.system_message.as_deref().unwrap();
        assert_eq!(
            rendered,
            render(&store.load("hydra-design").unwrap()).trim_end()
        );
        assert!(rendered.starts_with("hydra-design  (1 answered, 1 open)"));
        assert!(!rendered.ends_with('\n'), "a message, not a file");
        assert_eq!(
            raw,
            format!(
                "{{\"systemMessage\":{}}}",
                serde_json::to_string(rendered).unwrap()
            ),
            "systemMessage alone: no lease, no decision, nothing for the model"
        );
    }

    #[test]
    fn post_tool_use_gates_on_the_command_string() {
        let (_root, store) = store();
        let fires = |command: &str| json(&store, Event::PostToolUse, &bash(command)) != NOTHING;

        assert!(fires("hydra cut graph-shape --answer x"));
        assert!(fires("cd /repo && hydra sprout --question 'why?'"));
        assert!(
            fires("/usr/local/bin/hydra next"),
            "an absolute path is still hydra"
        );
        assert!(fires("~/.cargo/bin/hydra next"));
        assert!(fires("HYDRA_DIR=x hydra tree"));

        // A quoted binary path contains no `hydra ` substring at all, and this is
        // the false-negative direction that costs the user a render they wanted.
        assert!(fires(r#""$HOME/bin/hydra" cut a --answer x"#));
        assert!(fires("'/opt/my tools/hydra' next"));
        assert!(fires("$(which hydra) ready"));
        assert!(fires("hydra"), "help, but still hydra: see `runs_hydra`");

        // Every occurrence is tested, not just the first: the name-attached lead
        // here would hide the real invocation behind it.
        assert!(fires("git-hydra sync && hydra cut a --answer x"));

        assert!(
            !fires("myhydra cut graph-shape"),
            "a longer name is a different tool"
        );
        assert!(!fires("git-hydra status"));
        assert!(!fires("./nothydra ready"));
        assert!(!fires("foo.hydra tree"));
        assert!(!fires("hydraulics --help"));
        assert!(
            !fires("cat .hydra/HEAD"),
            "reading the store is not running hydra"
        );
        assert!(!fires("ls hydra-plugin/hooks"));
        assert!(!fires("cargo build"));
        assert!(!fires(""));
    }

    /// The row has a `Bash` matcher and no lease, so `tool_name` is the only thing
    /// keeping another tool's `command` field out of it.
    #[test]
    fn post_tool_use_only_looks_at_bash() {
        let (_root, store) = store();
        let other = Payload {
            tool_name: "Write".to_string(),
            ..bash("hydra cut graph-shape --answer x")
        };
        assert_eq!(json(&store, Event::PostToolUse, &other), NOTHING);

        let unnamed = Payload {
            tool_name: String::new(),
            ..bash("hydra cut graph-shape --answer x")
        };
        assert_eq!(json(&store, Event::PostToolUse, &unnamed), NOTHING);
    }

    #[test]
    fn post_tool_use_needs_no_lease() {
        let (_root, store) = store();
        take_lease(&store, "another-session");
        let anonymous = Payload {
            session_id: String::new(),
            ..bash("hydra tree")
        };
        assert_ne!(json(&store, Event::PostToolUse, &anonymous), NOTHING);
    }

    #[test]
    fn stop_blocks_under_a_live_lease_and_hands_over_next() {
        let (_root, store) = store();
        take_lease(&store, SESSION);

        let raw = json(&store, Event::Stop, &payload(Event::Stop));
        let response: Response = serde_json::from_str(&raw).unwrap();
        assert_eq!(response.decision.as_deref(), Some("block"));
        assert_eq!(
            (&response.system_message, &response.hook_specific_output),
            (&None, &None),
            "§6 puts the injection in the block's reason"
        );

        let reason = response.reason.as_deref().unwrap();
        let next = serde_json::to_string_pretty(&query::next(&store.load("hydra-design").unwrap()))
            .unwrap();
        assert!(reason.ends_with(&next), "`hydra next`, injected\n{reason}");
        assert!(
            reason.starts_with(
                "hydra: 1 open head on 'hydra-design' — the interview is not finished."
            )
        );
        assert!(reason.contains("hydra grill stop"), "name the kill switch");
        assert!(reason.contains(r#""slug": "graph-shape""#));

        assert_eq!(
            raw,
            format!(
                r#"{{"decision":"block","reason":{}}}"#,
                serde_json::to_string(reason).unwrap()
            ),
            "top-level decision and reason, and nothing else"
        );
    }

    /// The block hands the model a head to answer, and the `hydra next`/`hydra cut`
    /// it reaches for go through `HEAD`. If `HEAD` has moved off the lease's tree,
    /// blocking would name a head those commands cannot see.
    #[test]
    fn stop_does_not_block_when_head_has_moved_off_the_leased_tree() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        store.create("storage-format").unwrap();
        store.set_head("storage-format").unwrap();

        assert_eq!(json(&store, Event::Stop, &payload(Event::Stop)), NOTHING);
        assert_ne!(
            json(&store, Event::SessionStart, &session_start("compact")),
            NOTHING,
            "the reload only reads, so it keeps the tree §6 put in the lease"
        );

        store.set_head("hydra-design").unwrap();
        assert_ne!(
            json(&store, Event::Stop, &payload(Event::Stop)),
            NOTHING,
            "`hydra use` puts it back"
        );
    }

    #[test]
    fn stop_blocks_at_most_once_per_turn() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        let again = Payload {
            stop_hook_active: true,
            ..payload(Event::Stop)
        };
        assert_eq!(
            json(&store, Event::Stop, &again),
            NOTHING,
            "Claude Code sets stop_hook_active on the Stop after a blocked one"
        );
    }

    #[test]
    fn stop_only_fires_inside_a_live_lease() {
        let (_root, store) = store();
        let stop = payload(Event::Stop);
        assert_eq!(json(&store, Event::Stop, &stop), NOTHING, "no lease");

        take_lease(&store, "another-session");
        assert_eq!(
            json(&store, Event::Stop, &stop),
            NOTHING,
            "§6: a session doing unrelated work in the same repo is never grilled"
        );

        take_lease(&store, SESSION);
        assert_ne!(json(&store, Event::Stop, &stop), NOTHING);

        // The kill switch of §6.
        grill::stop(&store).unwrap();
        assert_eq!(json(&store, Event::Stop, &stop), NOTHING);
    }

    /// §6 gates the row on the lease alone, but a block with nothing to ask would
    /// leave `hydra grill stop` as the only way to end the session.
    #[test]
    fn stop_does_not_block_when_there_is_nothing_left_to_ask() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        store
            .with_tree_mut("hydra-design", |tree| {
                answer(tree, "graph-shape", "spanning tree + blocked_by");
                Ok(())
            })
            .unwrap();
        assert_eq!(json(&store, Event::Stop, &payload(Event::Stop)), NOTHING);

        // The other way `next` comes back empty: every open head blocked, which
        // takes a hand-edited or forced edge (see `query::next`).
        store
            .with_tree_mut("hydra-design", |tree| {
                graph::reopen(tree, "graph-shape")?;
                graph::link(tree, "graph-shape", "graph-shape", true)
            })
            .unwrap();
        assert!(!query::status(&store.load("hydra-design").unwrap()).done);
        assert_eq!(json(&store, Event::Stop, &payload(Event::Stop)), NOTHING);
    }

    /// The common case: most repos are not hydra repos, and the plugin's hooks
    /// fire in all of them.
    #[test]
    fn no_store_at_all_is_a_clean_no_op() {
        for event in [Event::SessionStart, Event::PostToolUse, Event::Stop] {
            for payload in [
                session_start("startup"),
                session_start("compact"),
                bash("hydra tree"),
                payload(Event::Stop),
            ] {
                let payload = Payload {
                    hook_event_name: String::new(),
                    ..payload
                };
                assert_eq!(to_json(&respond(None, event, &payload)), NOTHING);
            }
        }
    }

    #[test]
    fn a_corrupt_tree_is_a_no_op_rather_than_an_error() {
        let (_root, store) = store();
        take_lease(&store, SESSION);
        fs::write(store.tree_path("hydra-design"), "{ not json").unwrap();

        for payload in [
            session_start("startup"),
            session_start("compact"),
            bash("hydra tree"),
            payload(Event::Stop),
        ] {
            let event = match payload.hook_event_name.as_str() {
                "PostToolUse" => Event::PostToolUse,
                "Stop" => Event::Stop,
                _ => Event::SessionStart,
            };
            assert_eq!(json(&store, event, &payload), NOTHING, "{payload:?}");
        }
    }

    #[test]
    fn an_empty_store_is_a_no_op() {
        let root = TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        // No HEAD, so no tree to read, and no lease either.
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("startup")),
            NOTHING
        );
        assert_eq!(
            json(&store, Event::PostToolUse, &bash("hydra tree")),
            NOTHING
        );
        assert_eq!(json(&store, Event::Stop, &payload(Event::Stop)), NOTHING);

        // A lease naming a tree that is not there.
        take_lease(&store, SESSION);
        assert_eq!(
            json(&store, Event::SessionStart, &session_start("compact")),
            NOTHING
        );
        assert_eq!(json(&store, Event::Stop, &payload(Event::Stop)), NOTHING);
    }

    #[test]
    fn the_default_response_is_an_empty_object() {
        assert_eq!(to_json(&Response::default()), NOTHING);
    }
}
