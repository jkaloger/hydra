<h1 align="center">
  🐙 / 🐉
  <br>hydra
</h1>
<p align="center">
    Decision trees for LLM-led design interviews.
</p>

<img alt="Terminal displaying a breakdown of a design interview with questions and answers listed" src="https://github.com/user-attachments/assets/ccc0b5a7-0672-46bc-b8a0-f77bc3949448" />

## Synopsis

```
hydra init [slug] --intent <text|->
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

Hydra maintains a tree of the questions asked during a design interview, in
which one question is put at a time. An unanswered question is a **head**.
Answering a head is a **cut**. A cut may sprout further heads, and reopens the
heads that depend on the one answered.

Heads, answers, and the edges between them are stored as JSON under `.hydra/`.
Each invocation reads that store and exits; no state is held between
invocations.

Hydra does not read the text of a question and does not select which question is
put next. It refuses any write that would violate an invariant (see
_Invariants_) and reports which heads have no unanswered dependencies.

Every command writes JSON to stdout, except `tree`, whose output is formatted
for reading. Rejections and diagnostics are written to stderr. Stdout is
parseable at every exit status.

The options `--question`, `--answer`, `--intent`, `--rationale` and `--why`
accept `-` as a value, in which case the value is read from stdin. Stdin is read
once, so at most one such option may be given per invocation.

A tree carries an **intent**: prose stating what the interview is for, given at
`init` and stored alongside the slug. It is the first field written by `resume`.
Hydra does not read it. A tree written by a version of hydra predating the field
has an empty intent.

Only the first line of an answer appears in the skeleton written by `resume`.
The remainder is reported by `show`, and by `resume` for `next` and its
ancestors.

## Commands

### Tree management

| Command       | Effect                                                                                             |
| ------------- | -------------------------------------------------------------------------------------------------- |
| `init [slug]` | Create a tree and point `HEAD` at it. Slug defaults to the name of the directory holding `.hydra/`. `--intent` is required and is rejected if blank |
| `use <slug>`  | Move `HEAD` to an existing tree                                                                    |
| `trees`       | Every tree in the store with its counts, and which one `HEAD` names                                |
| `status`      | Counts for the `HEAD` tree. Exits 4 while open heads remain                                        |

`init` reuses the nearest `.hydra/` at or above the cwd and creates one in the
cwd only if there is none. Discovery walks upward, so a nested `.hydra/` shadows
the store above it rather than extending it.

### Mutation

| Command                    | Effect                                                                                                  |
| -------------------------- | ------------------------------------------------------------------------------------------------------- |
| `sprout`                   | Open a new head. Omit `--parent` for a root; `--slug` defaults to a slug derived from the question      |
| `cut`                      | Answer a head. Reopens its descendants and everything it gates                                          |
| `cauterise` (alias `sear`) | Answer a head that another head's answer has made unnecessary, setting `answer.cauterised_by`           |
| `reopen`                   | Discard an answer and reopen the head. Always cascades. The discarded answer is retained as `prior`      |
| `reword`                   | Replace a head's question, leaving its answer unchanged                                                 |
| `reparent`                 | Move a head under a different parent. `--parent ''` makes it a root                                     |
| `link` / `unlink`          | Add or remove a `blocked_by` edge. Idempotent                                                           |

`cut --keep-subtree` suppresses the cascade, leaving descendants and gated heads
as they are. `--force` is accepted by `cut`, `cauterise` and `link`; it overrides
the invariant that would otherwise reject the write, and is not recorded in the
tree.

### Query

| Command       | Output                                                                                                 |
| ------------- | ------------------------------------------------------------------------------------------------------ |
| `ready`       | Open heads whose dependencies are answered, in pre-order. `[]` when there are none                     |
| `next`        | The first ready head. `null` when nothing can be asked                                                 |
| `show <slug>` | One head, with every field resolved                                                                    |
| `resume`      | The intent, counts, `next`, a skeleton of every head, and full detail for `next` and its ancestors     |
| `tree`        | ASCII rendering, formatted for reading rather than parsing                                             |

Pre-order is a depth-first walk in which siblings are visited in ascending `seq`
order. `seq` is assigned when a head is sprouted and does not encode priority.

`hydra --help` and `hydra help <command>` describe the same surface.

## Files

```
.hydra/
├── HEAD               active tree slug
├── <slug>.json        one tree
└── <slug>.lock        advisory lock, held for the length of a write
```

The store is created within the repository and is intended to be committed. A
tree file is a mutable document rather than an event log: keys are sorted, the
output is pretty-printed one field per line, and each write is made to a
temporary file and renamed into place. `prior` and `rejected[]` retain the
answers and options that an overwrite would otherwise discard.

## Exit status

| Code | Meaning                                                                                                          |
| ---- | ---------------------------------------------------------------------------------------------------------------- |
| 0    | Success. For `status`, no open heads remain                                                                      |
| 1    | I/O error, malformed JSON, or a lock that could not be acquired                                                  |
| 2    | Usage error                                                                                                      |
| 3    | A slug was refused, either by an invariant or because no such head exists. The offending slugs are named on stderr |
| 4    | `status` only: open heads remain                                                                                  |
| 5    | Tree addressing: no `.hydra/`, no `HEAD`, no such tree, a tree that already exists, or one written by a newer hydra |

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
9. An edge, `blocked_by` or `parent`, that would create a cycle in the cascade
   relation, which traverses children and `blocked_by` alike. Gating a head on an
   ancestor is permitted. Gating a head on a descendant leaves the tree unable to
   reach a state in which no heads are open.

`--force` applies to 3, 5, 7 and 9 only. [SPEC.md](SPEC.md) §4 gives the
reasoning.

## Examples

```sh
cd my-project
hydra init my-design --intent 'Settle the storage format and CLI surface before any of it is built.'
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

The tree file can also be read directly:

```sh
jq -r '.heads | to_entries[] | select(.value.status == "open") | .key' .hydra/my-design.json
```

## Install

```sh
cargo install --path .
```

The Claude Code plugin is optional. Hydra is invoked from a shell, a Makefile, or
an agent.

```sh
claude --plugin-dir ./claude-plugin      # this session only
```

The repository is also a plugin marketplace. A persistent install takes two
commands:

```
/plugin marketplace add jkaloger/hydra
/plugin install hydra@hydra
```

`claude plugin validate ./claude-plugin --strict` checks the manifest.
`claude plugin validate . --strict` checks the marketplace that lists it.

The plugin provides one skill, `/hydra:hydra`, which contains the interview
protocol. Claude Code addresses a plugin's skills as `plugin:skill`, and does not
collapse the case in which both names are the same. The skill invokes the `hydra`
binary. If the binary is absent, the skill reports this and conducts the
interview in context.

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
