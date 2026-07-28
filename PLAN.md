# hydra — delivery plan

Slicing of [SPEC.md](SPEC.md). Spec §10 leaves delivery slicing out of scope; this is it.

## Layout

Single crate, `lib.rs` + `main.rs`. The lib is the core (§1), the bin is the clap wrapper. The plugin (§6) ships as a sibling directory of data files.

```
src/
├── lib.rs            re-exports, error enum
├── slug.rs           validation (§2)
├── model.rs          Head, Answer, Rejected, Tree (§3 file shape)
├── store.rs          .hydra/ layout, atomic write, fs4 lock (§3)
├── graph.rs          mutations + invariants (§4)
├── query.rs          derived state, pre-order, ready/next/resume (§5, §7)
├── render.rs         ASCII tree (§5)
├── hook.rs           Claude Code hook payloads + gating (§6, §9)
├── grill.rs          session lease (§6)
└── main.rs           clap surface (§5)
hydra-plugin/
```

## Iterations

Sequential; each is built and reviewed before the next starts.

### I1 — model and storage

Cargo features (`serde/derive`, `clap/derive`, `ulid/serde`). Slug validation. `Head`, `Answer`, `Rejected`, `Tree` with serde shapes matching §3 exactly. `.hydra/` discovery (walk up for the directory), `HEAD` read/write, tree load/save via `NamedTempFile::persist` in the target dir, `fs4` advisory lock over the read-modify-write span. Pretty-printed, sorted keys, no `preserve_order`.

Tests: slug accept/reject table, round-trip serde against a literal JSON fixture matching §3, atomic write leaves no temp file, missing `.hydra` is a typed error.

### I2 — graph and invariants

`sprout`, `cut`, `cauterise`, `reopen`, `reword`, `reparent`, `link`, `unlink`. `rev` bump, `prior` capture, transitive cascade reopen (descendants ∪ `blocked_by` closure), `--keep-subtree`. `thiserror` enum, one variant per §4 rejection, each naming offending slugs. `force` gated to rejections 3, 5, 7 only.

Tests: every §4 rejection, cascade closure (including diamond and cross-branch), `--keep-subtree`, cauterise sets `cauterised_by` and `text = "cauterised"`, reopen ≠ cauterise, `seq` assignment on sprout.

### I3 — queries and render

Derived `blocked`/`ready`/`done`. Pre-order walk (depth-first, siblings by `seq`). `ready`, `next`, `show`, `resume` (skeleton = slug/question/status/first line of `answer.text`; hydrated = `next` plus ancestor chain), `status` counts. ASCII render with the §5 glyph set and `← next` marker.

Tests: pre-order stability under re-parenting and insertion, `next` picks first ready in pre-order, blocked heads excluded from `ready`, skeleton summary is first line only, render glyph per state.

### I4 — CLI

clap derive over §5. JSON on stdout for everything but `tree`. `--answer -` reads stdin. Every mutation echoes the tree it wrote. Rejections exit nonzero with the offending slugs. `status` exits nonzero while open heads remain. `anyhow` at the top level.

Not unit tested (§8). Verified by a manual smoke script over the full verb surface.

### I5 — session and hooks

`grill start`/`stop` writing `.hydra/grill` (§6 shape), gitignored. `hydra hook <event>` for `session-start`, `post-tool-use`, `stop`: parse payload on stdin, gate per the §6 table, emit the `hookSpecificOutput` envelope.

Tests: lease match/mismatch/absent for each event, `session-start` source split (`startup|resume` one-liner vs `compact|clear` full resume), `post-tool-use` command-string gate, `stop` blocks only under a live lease, unparseable payload no-ops rather than dying.

### I6 — plugin

`hydra-plugin/` with `.claude-plugin/plugin.json`, `skills/hydra/SKILL.md` (interview protocol per §6, `grill start` first, degrades if `command -v hydra` fails), `hooks/hooks.json` shelling to `hydra hook <event>`. Zero scripts.

## Out of scope

Per §1 and §8: no export renderer, no event log, no TUI, no MCP shim, no CLI output-shape tests.
