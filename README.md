<h1 align="center">
  🐙 / 🐉
  <br>hydra
</h1>
<p align="center">
    Decision trees for AI-led design interviews.
</p>
<p align="center">
  <a href="#license"><img alt="license: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg"></a>
</p>

## Synopsis

```
hydra init [slug]
hydra use <slug>
hydra trees | status | ready | next | resume | tree
hydra sprout --question <text|-> [--parent <slug>] [--blocked-by <slug,...>] [--slug <slug>]
hydra cut <slug> --answer <text|-> [--rationale <text|->] [--reject '<option>: <why>']...
            [--keep-subtree] [--force]
hydra cauterise <slug> --by <slug> [--why <text|->] [--force]
hydra reopen <slug>
hydra reword <slug> --question <text|->
hydra reparent <slug> --parent <slug>
hydra link <slug> --blocked-by <slug> [--force]
hydra unlink <slug> --blocked-by <slug>
hydra show <slug>
```

## Description

An interviewer asks one question at a time about a plan. Each open question is
a **head**; answering it is a **cut**, which may sprout more heads. Hydra stores
the heads, the answers and the dependencies between them as structured data on
disk, so the interview survives the process that was conducting it.

Hydra holds no opinion about what to ask. It never reads question text. It
refuses writes that would corrupt the graph (see _Invariants_) and reports which
heads can be asked next.

Every command writes JSON to stdout except `tree`, which is formatted for
reading. Rejections and diagnostics go to stderr, so stdout stays parseable at
any exit status. The on-disk format is `jq`-native by intent: much of the read
surface is a one-liner against the tree file rather than a subcommand.

Prose options take `-` to read the value from stdin: `--question`, `--answer`,
`--rationale`, `--why`. Stdin is read once, so at most one `-` per invocation.

The first line of an answer is what a later session sees in the `resume`
skeleton; put the decision there and the reasoning after it.

## Commands

### Tree management

| Command       | Effect                                                                                             |
| ------------- | -------------------------------------------------------------------------------------------------- |
| `init [slug]` | Create a tree and point `HEAD` at it. Slug defaults to the name of the directory holding `.hydra/` |
| `use <slug>`  | Move `HEAD` to an existing tree                                                                    |
| `trees`       | Every tree in the store with its counts, and which one `HEAD` names                                |
| `status`      | Counts for the `HEAD` tree. Exits 4 while open heads remain                                        |

`init` reuses the nearest `.hydra/` at or above the cwd, and creates one in the
cwd only if there is none. Discovery walks up, so a nested `.hydra/` shadows the
store above it instead of extending it.

### Mutation

| Command                    | Effect                                                                                                  |
| -------------------------- | ------------------------------------------------------------------------------------------------------- |
| `sprout`                   | Open a new head. Omit `--parent` for a root; `--slug` defaults to a slug derived from the question      |
| `cut`                      | Answer a head. Reopens its descendants and everything it gates                                          |
| `cauterise` (alias `sear`) | Kill a question a sibling's answer made moot. The head ends up answered with `answer.cauterised_by` set |
| `reopen`                   | Withdraw an answer and ask again. Always cascades; the old answer is kept as `prior`                    |
| `reword`                   | Change a head's question, leaving its answer alone                                                      |
| `reparent`                 | Move a head under a different parent. `--parent ''` makes it a root                                     |
| `link` / `unlink`          | Add or remove a `blocked_by` edge. Idempotent                                                           |

`cut --keep-subtree` skips the cascade — for typos and rewording, not for
decisions. `--force` on `cut`, `cauterise` and `link` overrides an invariant and
records nothing.

### Query

| Command       | Output                                                                                                 |
| ------------- | ------------------------------------------------------------------------------------------------------ |
| `ready`       | Open heads whose dependencies are answered, in pre-order. `[]` when there are none                     |
| `next`        | The first ready head. `null` when nothing can be asked                                                 |
| `show <slug>` | One head, fully hydrated                                                                               |
| `resume`      | Cold-start payload: counts, `next`, a skeleton of every head, full detail for `next` and its ancestors |
| `tree`        | ASCII render. The one command whose output is for eyes                                                 |

Pre-order is a depth-first walk with siblings ascending by `seq` — document
order, not priority.

`hydra --help` and `hydra help <command>` carry the same surface.

## Files

```
.hydra/
├── HEAD               active tree slug
├── <slug>.json        one tree
└── <slug>.lock        advisory lock, held for the length of a write
```

Repo-local and git-tracked: these decisions are about the code and belong in its
history. Trees are mutable documents, not event logs — sorted keys,
pretty-printed, one field per line, written by temp file and atomic rename, so
diffs stay minimal and reviewable. Git is the event log; `prior` and `rejected[]`
carry what a single overwrite would otherwise lose.

## Exit status

| Code | Meaning                                                                                                          |
| ---- | ---------------------------------------------------------------------------------------------------------------- |
| 0    | ok; for `status`, no open heads remain                                                                           |
| 1    | I/O, malformed JSON, or a lock that would not come free                                                          |
| 2    | usage                                                                                                            |
| 3    | a slug was refused: an invariant, or a head that is not there. stderr names the offending slugs                  |
| 4    | `status` only: open heads remain                                                                                 |
| 5    | tree addressing: no `.hydra/`, no `HEAD`, no such tree, one that already exists, or one written by a newer hydra |

## Invariants

Refused at write, exit 3:

1. `parent` references a head that doesn't exist (`null` = root).
2. `blocked_by` references a head that doesn't exist.
3. A `blocked_by` edge that would create a cycle.
4. A `reparent` that would make a head its own ancestor.
5. Cutting a head whose `blocked_by` set isn't fully answered.
6. An illegal transition. Only `open → answered` and `answered → open`.
7. `cauterise --by` pointing at an unanswered head.
8. A duplicate or malformed slug.
9. An edge — `blocked_by` or `parent` — that would create a cycle in the cascade
   relation, which walks children and `blocked_by` as one. Gating a head on an
   ancestor is legal; gating on a descendant makes the tree unable to reach done.

`--force` covers 3, 5, 7 and 9 only. [SPEC.md](SPEC.md) §4 has the reasoning.

## Examples

```sh
cd my-project
hydra init my-design
hydra sprout --question 'What does this look like from outside?' --slug surface
hydra sprout --question 'How is state stored?' --parent surface --slug storage
hydra next
hydra cut surface --answer 'a unix CLI: JSON on stdout' \
  --rationale 'no harness assumptions' \
  --reject 'GUI: nobody will script it'
hydra cut storage --answer - <<'EOF'
one JSON file per tree, git-tracked
The first line is what a future session sees in the skeleton.
EOF
hydra tree
hydra status
```

Read straight off disk instead of shelling a subcommand:

```sh
jq -r '.heads | to_entries[] | select(.value.status == "open") | .key' .hydra/my-design.json
```

## Install

```sh
cargo install --path .
```

The Claude Code plugin is optional; hydra works from a shell, a Makefile, or any
agent.

```sh
claude --plugin-dir ./claude-plugin      # this session only
```

For a persistent install, add a marketplace listing `claude-plugin/`, then
`/plugin install hydra@<marketplace>`. `claude plugin validate ./claude-plugin`
checks the manifest.

The plugin ships one skill, `/hydra:hydra`, the interview protocol — Claude Code
addresses a plugin's skills as `plugin:skill` and does not collapse the case
where both names match. It shells to the `hydra` binary, so install that first;
without it the skill says so and falls back to interviewing in context.

## Development

```sh
cargo test              # core lib: graph, invariants, cascade
cargo clippy --all-targets
scripts/smoke.sh        # the CLI's output shapes and the plugin's files
```

## See also

[SPEC.md](SPEC.md) — model, storage format, invariants, plugin protocol.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
