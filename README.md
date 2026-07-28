# hydra

Decision-tree store for AI-led design interviews.

Claude interviews you about a plan. Hydra owns the state. Every open question is
a **head**; every answer is a **cut** that may sprout more. Heads, answers and
dependencies live as durable structured data, not context — the session dies,
hydra doesn't, and you pick up exactly where interrogation stopped.

**The LLM wields the sword. Hydra keeps count.** Hydra never reads question text
and has no opinion about what to ask; it refuses writes that would rot the graph
and tells you what *can* be asked next.

One JSON file per tree in `.hydra/`, repo-local and git-tracked: these decisions
are about the code and belong in its history. See [SPEC.md](SPEC.md).

## Install

```sh
cargo install --path .

# The plugin is optional: hydra works from a shell, a Makefile, or any agent.
claude --plugin-dir ./hydra-plugin      # this session only
```

For a persistent install, add a marketplace that lists `hydra-plugin/` and
`/plugin install hydra@<marketplace>`; `claude plugin validate ./hydra-plugin`
checks the manifest.

It ships one skill and nothing else — **`/hydra:hydra`**, the interview protocol;
Claude Code addresses a plugin's skills `plugin:skill` and does not collapse the
case where both names match. It shells to the `hydra` binary, so install that
first; without it the skill says so and falls back to interviewing in context.

## Example

```sh
cd my-project
hydra init my-design
hydra sprout --question 'What does this look like from outside?' --slug surface
hydra sprout --question 'How is state stored?' --parent surface --slug storage
hydra next                                     # first ready head, in pre-order
hydra cut surface --answer 'a unix CLI: JSON on stdout' \
  --rationale 'no harness assumptions' \
  --reject 'GUI: nobody will script it'
hydra cut storage --answer - <<'EOF'           # `-` reads stdin, for prose
one JSON file per tree, git-tracked
The first line is what a future session sees in the skeleton.
EOF
hydra tree                                     # for eyes
hydra status                                   # exit 0: done. 4 while heads remain
```

`hydra --help` carries the full verb surface and the exit-code table.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | ok; for `status`, no open heads remain |
| 1 | I/O, malformed JSON, or a lock that would not come free |
| 2 | usage |
| 3 | a slug was refused: an invariant, or a head that is not there |
| 4 | `status` only: open heads remain |
| 5 | tree addressing: no `.hydra/`, no `HEAD`, no such tree, or one that exists |

## Development

```sh
cargo test              # core lib: graph, invariants, cascade
cargo clippy --all-targets
scripts/smoke.sh        # the CLI's output shapes and the plugin's files
```
