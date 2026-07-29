# hydra — SPEC

Decision-tree store for AI-led design interviews.

Claude interviews you about a plan. Hydra owns the state. Every open question is a head; every answer is a cut that may sprout more. Heads, answers and dependencies live as durable structured data, not context. Session dies, hydra doesn't — pick up exactly where interrogation stopped.

LLM wields the sword. Hydra keeps count.

---

## 1. Shape

Three artifacts, deliberately separable:

| Artifact         | What                                    | Knows about              |
| ---------------- | --------------------------------------- | ------------------------ |
| `hydra` core lib | Rust. Graph model, invariants, storage. | Nothing external         |
| `hydra` CLI      | Thin clap wrapper. JSON on stdout.      | Core lib                 |
| hydra plugin     | Claude Code plugin: one skill.          | The CLI, by shelling out |

The CLI is a unix tool. No lazyspec coupling, no export templates, no harness assumptions. The plugin is an adapter and is optional — hydra works from a shell, from a Makefile, from any agent. An MCP shim over the core lib stays possible later; nothing here forecloses it.

### Non-goals

- Hydra never reads question text. It has no opinion about what to ask, what matters, or what order is _best_.
- No export renderer. The JSON is the artifact; consumers render it.
- No event log. The tree is a mutable document, git is the history.
- No TUI.

---

## 2. Model

### Heads

A head is one open question. Heads form a **spanning tree** (`parent`) with **cross edges** (`blocked_by`) layered over it.

The tree carries narrative: nesting is what makes a cold-start dump legible as an outline. The cross edges carry gating: real decisions depend on answers in other branches, and forcing those into the tree would mean fake re-parenting.

```
consumption-surface          ← root
├─ graph-shape
│  ├─ head-schema
│  └─ lifecycle
└─ storage-format
   └─ write-model            blocked_by: [head-schema]
```

### States

Two. `open`, `answered`. Nothing else is stored.

| Property      | Derivation                      |
| ------------- | ------------------------------- |
| `blocked`     | any `blocked_by` head is `open` |
| `ready`       | `open` and not `blocked`        |
| `done` (tree) | zero `open` heads               |

Cauterisation is not a state. A head killed by a sibling's answer is _answered_ with `answer.text = "cauterised"` and `answer.cauterised_by` set. Question resolved: it doesn't apply. Keeps the record, clears the frontier, adds no lifecycle.

### Answers

Stored:

```json
{
  "text": "spanning tree + blocked_by cross edges",
  "rationale": "nesting keeps the resume dump legible; cross edges cost one cycle check",
  "rejected": [
    { "option": "strict tree", "why_not": "can't express cross-branch gating" },
    {
      "option": "pure DAG",
      "why_not": "render nondeterministic, loses 'where am I'"
    }
  ],
  "cauterised_by": null
}
```

**Options are not persisted before an answer exists.** An open head gets its option set regenerated from scratch when it's re-presented, so storing a menu is dead weight. The choice and the killed alternatives are the only lossy part — store exactly that.

`rejected[]` is optional. Fill it when a branch was genuinely considered and killed; that's what stops a future session re-proposing it.

### Identity

ULID internally, short kebab slug as the handle. Slugs are what the LLM types and what appears in output. Stable across re-parenting and insertion, unlike hierarchical `D4.2` numbering.

Slug format: `^[a-z0-9][a-z0-9-]*$`, unique within a tree.

### Revision and cascade

Each head carries `rev`, bumped when its answer changes.

Re-answering a head **transitively reopens** its descendants and everything `blocked_by` it. Each reopened head retains its `prior` answer for context, so re-answering is usually one word — the LLM re-presents the old answer and asks whether it still holds.

Staleness lives in the frontier, not in a report. A flag saying "please review these" is the kind of thing a session three days later skims past, and then hydra hands back a coherent-looking tree standing on a premise that moved. An `open` head can't be skipped: it blocks the done condition, policed by machinery that already exists.

`--keep-subtree` skips the cascade. For typos and rewording.

Reopening is not cauterising. Cauterised means _a sibling's answer killed this question_. Reopened means _ask it again_.

---

## 3. Storage

One JSON file per tree, repo-local, git-tracked.

```
.hydra/
├── HEAD               active tree slug
└── <slug>.json
```

Repo-local because these decisions are _about the code_ and belong in its history — reviewable in a PR, greppable, survives the laptop.

Mutable document, not an event log. Sorted keys, pretty-printed, one field per line, written by temp-file + atomic rename. `jq`-native by design: a good share of the read surface should be a one-liner rather than a subcommand.

Git is the event log. Building an append-only log inside a git-tracked file is building a worse git. Cost accepted: an answer overwritten twice between commits leaves no trace of the middle. `prior` and `rejected[]` carry the substance.

### File shape

```json
{
  "version": 1,
  "slug": "hydra-design",
  "created_at": "2026-07-28T04:11:02Z",
  "heads": {
    "graph-shape": {
      "id": "01J8XQ2K7T4V9WZ3N5M6P8R0AB",
      "slug": "graph-shape",
      "question": "Strict tree, tree + dep edges, or pure DAG?",
      "parent": "consumption-surface",
      "seq": 2,
      "blocked_by": [],
      "status": "answered",
      "rev": 1,
      "created_at": "2026-07-28T04:12:00Z",
      "updated_at": "2026-07-28T04:19:31Z",
      "answer": {
        "text": "...",
        "rationale": "...",
        "rejected": [],
        "cauterised_by": null
      },
      "prior": null
    }
  }
}
```

Sibling order is `seq`, an integer. Pre-order walk = depth-first, siblings ascending by `seq`. No `children` array — parent pointer plus `seq` is the single source of truth, so there is no ordering to desync.

`prior` holds the single most recent superseded answer. Deeper history is git's job.

---

## 4. Invariants

Hydra is a store with invariants, not a policy engine. It never reads question text. It does refuse writes that would rot the graph, because corruption here is silent and durable — the exact failure the tool exists to prevent.

Rejected at write:

1. `parent` references a head that doesn't exist (`null` = root).
2. `blocked_by` references a head that doesn't exist.
3. A `blocked_by` edge that would create a cycle.
4. A `reparent` that would make a head its own ancestor.
5. Cutting a head whose `blocked_by` set isn't fully answered.
6. Illegal transition. Only `open → answered` (cut, cauterise) and `answered → open` (cascade, explicit reopen).
7. `cauterise --by` pointing at an unanswered head.
8. Duplicate or malformed slug.
9. An edge — `blocked_by` or `parent` — that would create a cycle in the cascade relation.

9 is subtler than 3 and was found the hard way. The cascade walks `children ∪ blocked_by` as one relation (§2), so gating an ancestor on its own descendant closes a cycle across the union even though `blocked_by` alone stays acyclic: re-answering either head reopens the other, forever. `status` never leaves nonzero and `next` never returns null. A tree that can never reach done is the silent durable corruption this section exists to refuse.

Only one direction is a cycle. A `blocked_by` edge contributes `blocker → dependent`, the reverse of the dependency arrow, while a parent pointer contributes `parent → child`. So gating a head on its *ancestor* pushes staleness downward exactly as the tree does and stays legal — §3's own file shape does it. Gating on a descendant closes the loop. `reparent` reaches the same shape with no `blocked_by` write at all, by adopting a head that already blocks the new parent, so it is checked there too; §4.4 doesn't catch that, since the new parent needn't be a descendant. `sprout` needs no check: every cascade edge touching a brand-new head points inward, and a head that didn't exist a moment ago has neither children nor dependents.

Every rejection exits nonzero with a message naming the offending slugs. `--force` exists for 3, 5, 7 and 9 only, and records nothing — if you force it, you own it. On 9 it covers `link` alone: §5 gives `reparent` no `--force`, and `link --force` is already the deliberate path to the shape.

Every mutation echoes the tree it wrote to, so a stale `HEAD` surfaces immediately.

---

## 5. Commands

JSON on stdout for everything except `tree`, which is for eyes.

### Tree management

```
hydra init [<slug>]                      create tree, point HEAD at it
hydra use <slug>                         move HEAD
hydra trees                              list trees, open counts
hydra status                             counts; exit nonzero while open heads remain
```

### Mutation

```
hydra sprout --question <text>
             [--parent <slug>] [--blocked-by <slug>,...] [--slug <slug>]
hydra cut <slug> --answer <text|->
             [--rationale <text>] [--reject "<option>: <why>"]... [--keep-subtree]
hydra cauterise <slug> --by <slug> [--why <text>]        # alias: sear
hydra reopen <slug>
hydra reword <slug> --question <text>
hydra reparent <slug> --parent <slug>
hydra link <slug> --blocked-by <slug>
hydra unlink <slug> --blocked-by <slug>
```

`--answer -` reads stdin. Answers and rationales are prose and will contain quotes; stdin is the sane path for anything long.

### Query

```
hydra ready                              open heads with deps satisfied
hydra next                               first ready head in tree pre-order
hydra show <slug>                        one head, fully hydrated
hydra resume                             cold-start payload (§7)
hydra tree                               ASCII render
```

`next` is documented as _first ready head in pre-order_ — document order, not priority. Hydra says what **can** be asked, never what **should**. Pre-order is deterministic, so whoever resumes walks the tree the same way.

### `hydra tree`

Compact enough to print every turn. Marks the current head.

```
hydra-design  (14 answered, 6 open)
└── ● consumption-surface    CLI unix tool
    ├── ● graph-shape        spanning tree + blocked_by
    │   ├── ● head-schema    answer{text, rationale, rejected}
    │   └── ○ lifecycle      ← next
    └── ● storage-format     mutable JSON, git = history
        ├── ⊘ write-model    cauterised by storage-format
        └── ◌ resume-shape   blocked by lifecycle
```

`●` answered · `○` ready · `◌` blocked · `⊘` cauterised · `←` next

Connectors as `tree(1)` draws them, with the header line as the root every top-level head hangs off — so nesting is drawn rather than implied by indent, and a subtree reads as one block at any depth. Summaries share one character column, measured on characters rather than bytes.

Colour when stdout is a terminal, and only then: glyph in its state's colour (answered green, ready bold cyan, blocked yellow, cauterised dim red), connectors and summaries dimmed, `← next` bold. `NO_COLOR` disables it. The escapes are zero-width to the terminal and non-zero to a string length, so the column is measured on the plain text and the paint applied after.

---

## 6. Plugin

Distributed as a Claude Code plugin. Not a `hydra skills install` subcommand — the CLI stays harness-agnostic.

```
hydra-plugin/
├── .claude-plugin/plugin.json
└── skills/hydra/SKILL.md
```

One skill and nothing else. No hooks: a plugin's hooks fire in every project it is installed in, and buying enforcement at that price is a bad trade — the failure modes land in sessions that never asked for hydra, and the enforcement they buy is a turn the model cannot end, which reads as the tool being broken.

The plugin declares the `hydra` binary as a prerequisite. The skill checks `command -v hydra` and degrades to in-context interviewing if it's absent rather than dying.

### Skill

Named `hydra`, invoked as `/hydra:hydra` — Claude Code namespaces plugin skills `plugin:skill`, and the degenerate case where the two names match is not collapsed. Owns the interview protocol:

```
resume            skeleton + hydrated ancestry
next              head to ask
  → lay out 2-4 options with tradeoffs, give a recommendation, ask, wait
cut <slug>        answer + rationale + rejected[]
sprout            new heads the answer opened
cauterise         heads the answer killed
```

### Relentlessness

Hydra is relentless in the *store*, not in the runtime. There is nothing to seize a stop decision or to inject a reload; the pressure is that the state outlives the context.

- An open head cannot be skipped: it blocks `done`, and `hydra status` exits 4 while any remain (§5).
- Losing the thread costs one command. `hydra resume` (§7) rebuilds the interview from disk, so a compaction, a `/clear` or a new session next week all recover the same way.
- The skill's discipline covers the rest: re-orient with `resume` on waking, and don't wrap up on a tree that is not done.

A model that drifts into summarising mid-interview loses nothing but the turn. That is the trade against a runtime that could not drift and could not be got out of either.

---

## 7. Resume

Two tiers, because a 200-head tree with rationale, rejected and prior is easily 20k tokens and 90% of it is settled branches irrelevant to the next question.

**Skeleton** — every head: slug, question, status, and the _first line_ of `answer.text`. About 15 tokens each; 200 heads ≈ 3k. Enough global awareness to stop the LLM re-asking something settled in a distant branch, which `ready` cannot catch because that duplication is semantic, not structural.

**Hydrated** — full detail for `next` and its ancestor chain. Those are its premises; it can't be asked well without them.

**On demand** — `hydra show <slug>` for anything else.

The one-line summary is the first line of `answer.text`, not a separate field. Costs nothing and trains a good habit: lead the answer with the decision, elaborate after. The skeleton is a map, not the territory.

---

## 8. Testing

Unit tests on the core lib: graph operations, the invariant set in §4, cascade closure, pre-order stability.

The CLI and its output shapes are not covered. Known consequence: renaming a JSON key won't fail a test, and the skill is the only thing that notices.

---

## 9. Dependencies

Versions are resolved at `cargo add` time, not pinned here.

### Core lib

| Crate | Why | Notes |
| --- | --- | --- |
| `serde` (derive) | Model ↔ JSON | |
| `serde_json` | The storage format | `Map` is a `BTreeMap` by default, so **sorted keys come free** — do *not* enable `preserve_order`, it swaps in `IndexMap` and breaks the stable-diff property from §3 |
| `ulid` | Head identity (§2) | Has a `serde` feature. Rules out `uuid` |
| `jiff` | RFC 3339 timestamps | Modern, sane API, good serde support. `time` is the conservative alternative; `chrono` is not worth its surface |
| `thiserror` | Typed error enum, one variant per invariant in §4 | Lets rejections name offending slugs structurally rather than as formatted strings |
| `tempfile` | Atomic write via `NamedTempFile::persist` | Must be created in the same directory as the target or `persist` crosses a filesystem boundary and stops being atomic |
| `fs4` | Advisory lock on the tree file | See concurrency note below |

### CLI

| Crate | Why |
| --- | --- |
| `clap` (derive) | Verb surface in §5 |
| `anyhow` | Top-level error reporting; core lib keeps `thiserror` |

### Dev

Std `#[test]` only, per §8. No `insta`, no `proptest`, no `assert_cmd` — those were considered and rejected with the testing scope.

### Deliberately absent

- **`regex`** — slug validation (`^[a-z0-9][a-z0-9-]*$`) is a dozen lines of `char` checks. Not worth the compile time.
- **`uuid`** — `ulid` covers identity and sorts lexicographically by time.
- **`petgraph`** — the graph is a `HashMap<Slug, Head>` with parent pointers. Cycle detection and transitive closure over a few hundred nodes are short hand-written walks; a graph library would cost an index to keep in sync with the JSON, which is exactly the desync §3 avoids.
- **`indexmap`** — see the `serde_json` note.
- **`jq`** — nothing hydra ships parses JSON in shell. The plugin is data files (§6) and the smoke script is a developer tool, so `jq` is a test-time convenience, never a runtime dependency.

### Concurrency

Not previously covered. Two agents in one repo can interleave a read-modify-write and silently lose an answer. `fs4` advisory-locks the tree file for the read-modify-write span; contention is near zero in practice, so a blocking lock with a short timeout is enough.

---

## 10. Assumptions and open questions

Taken as recommended, not explicitly ratified:

- ULID + slug identity (§2) over hierarchical numbering.
- First line of `answer.text` as the skeleton summary (§7).
- `prior` holds one superseded answer, not a stack (§3).

Open:

- `cauterise`/`cauterised` vs shorter `sear`/`seared` as the stored value. Currently British-spelt long form, `sear` as command alias.
- MCP shim over the core lib — deferred, not designed.
- Delivery slicing — deliberately out of scope for this document.
