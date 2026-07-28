---
name: hydra
description: Interrogate a plan or design question by question, with every question, answer and rejected alternative stored in a decision tree on disk (`.hydra/`) instead of in context — so the interview survives compaction, a new session, and days off. Use to resume or pick up an interview started earlier, when a design's open questions have to be tracked across sessions rather than held in context, or when the user wants to be interviewed and the decisions recorded. Invoked as /hydra:hydra.
---

# hydra — the interview protocol

```
resume            skeleton + hydrated ancestry
next              head to ask
  → lay out 2-4 options with tradeoffs, give a recommendation, ask, WAIT
cut <slug>        answer + rationale + rejected[]
sprout            new heads the answer opened
cauterise         heads the answer killed
```

One question per turn. Never two. Never answer for the user.

`hydra` is a store with invariants. It never reads question text and has no
opinion about what to ask, what matters, or what order is best — it hands back
the first *askable* head in document order. Option generation, tradeoffs, the
recommendation, and the judgement to jump elsewhere are **yours**. `hydra next`
is a default, not an instruction.

## Start of every invocation

```bash
command -v hydra || echo absent
hydra resume
```

Orient from `resume` **every time this skill fires**, including when you wake
mid-interview. Nothing outside this skill reloads the tree for you: the state is
on disk, and reading it is one command.

If `command -v hydra` fails, say so in one line and **interview in context
instead** — same loop, same discipline, tracked in your own notes. Do not stop.

`hydra resume` exits **5** when there is no tree to interview. Then:

```bash
hydra init <slug>     # new interview; points HEAD at it
hydra trees           # what already exists here
hydra use <slug>      # resume a different one
hydra resume          # and orient
```

## Exit codes are the interface

| Code | Means | Do |
| --- | --- | --- |
| 0 | ok | carry on |
| 1 | I/O, a tree file that will not parse, or a lock that would not come free | tell the user; `hydra trees` names the broken file. Not yours to fix by retrying |
| 2 | usage — bad flag, two `-` in one call, `--reject` with no `:` | fix the command |
| 3 | slug refused: no such head, or an invariant that protects the graph | read stderr, it names the slugs |
| 4 | `hydra status` only: **open heads remain** | a signal, not a failure |
| 5 | tree addressing: no `.hydra/`, no `HEAD`, no such tree — **or one that already exists** | read stderr before retrying: `init` on an existing tree is a 5, and running `init` again just gets another |

Two things that look like failures and are not:

- `hydra status` exiting **4** is the normal state of a live interview. Never
  treat it as an error, and never wrap it in a way that aborts on nonzero.
- `hydra next` printing `null` is the **done** signal. `[]` from `hydra ready`
  says the same. Both exit 0.

The commonest 3: cutting a head whose `blocked_by` heads are not answered yet.
Answer those first — or `--force`, which records nothing, so if you force it you
own it.

## Reading the tree

Two tiers, because a large tree is mostly settled branches irrelevant to the
next question:

- **`hydra resume`** — `counts`, `next` (a slug), `skeleton` (every head: slug,
  question, status, state, first line of the answer, and `prior_summary` for a
  head that was answered and is open again), and `hydrated` (full detail for
  `next` and its ancestor chain, root first). The skeleton is the map: scan it
  before you ask anything, so you do not re-ask a question already settled in a
  distant branch. That duplication is semantic and nothing in the tool can catch
  it.
- **`hydra show <slug>`** — one head, full detail, on demand. Reach for it when
  the skeleton line is not enough to ask the question well.
- **`hydra next`** — the head to ask, already hydrated (`ancestors` are its
  premises). `hydra ready` is every askable head if you want to choose a
  different one.
- **`hydra tree`** — the ASCII render, the one output for eyes rather than for
  you. Print it after a cut that reopened heads, or when the user asks where
  things stand. Not every turn: it is noise between two questions in the same
  branch.

## Asking

For the head `next` hands you:

1. Read its `ancestors` — those are the premises. An answer that contradicts a
   parent is a signal to `reopen` the parent, not to answer this head.
2. Generate **2-4 real options**. Distinct, non-strawman, each with the tradeoff
   that actually decides between them.
3. State a recommendation and why.
4. Ask. Then **stop and wait**. Do not proceed on an assumed answer.

Ending the turn on a question is correct and nothing will stop you. Answering
your own question to keep moving is the one failure mode this skill cannot
recover from: a `cut` that records your guess as the user's decision is
indistinguishable from a real one forever after.

## Recording the answer

Answers and rationales are prose and contain quotes, so put them on stdin.
`-` reads stdin, and **only one field per invocation** may be `-`:

```bash
hydra cut graph-shape --answer - \
  --rationale 'nesting keeps the resume dump legible; cross edges cost one cycle check' \
  --reject 'strict tree: cannot express cross-branch gating' \
  --reject 'pure DAG: render nondeterministic, loses where-am-I' <<'EOF'
spanning tree + blocked_by cross edges
The tree carries narrative for the cold-start dump; the cross edges carry gating.
EOF
```

- **Lead the answer with the decision.** Its first line is the skeleton summary
  every future session reads.
- `--rationale -` instead, when the rationale is the awkward one to quote and the
  answer is short.
- `--reject '<option>: <why>'`, repeatable, split on the first `:`. Fill it
  whenever an option was genuinely considered and killed — `rejected[]` is what
  stops a future session re-proposing a dead branch. Both halves must be
  non-empty or you get exit 2.

Then open what the answer opened, and kill what it killed:

```bash
hydra sprout --question 'What does a head store?' --parent graph-shape --slug head-schema
hydra sprout --question 'Append-only or mutable?' --parent storage-format --slug write-model \
  --blocked-by head-schema,lifecycle
```

- `--parent` nests for narrative; omit it for a root. `--blocked-by` is for a
  real cross-branch dependency — this head cannot be asked until that one is
  answered. Do not fake a parent to express gating.
- `--slug` is optional; omitted, hydra derives one from the question and reports
  it as `.slug`.
- Graph surgery on an existing head: `hydra link <slug> --blocked-by <slug>`,
  `hydra unlink <slug> --blocked-by <slug>`, `hydra reparent <slug> --parent
  <slug>` (`--parent ''` roots it).
- **Never gate a head on its own descendant.** The cascade walks children and
  `blocked_by` as one relation, so `link a --blocked-by b` where `b` sits under
  `a` makes the two reopen each other forever: `status` never leaves 4 and `next`
  never returns `null`, so the interview can never finish.
  Hydra refuses that edge (exit 3, naming the loop). `link --force` overrides it
  and is the one flag that can build a tree which can never reach done — do not.
  Gating on an **ancestor** is the normal, correct case: staleness already flows
  down the tree, so the edge adds no loop.

## cauterise vs reopen

They are not the same and mixing them corrupts the record.

**`cauterise`** — a *sibling's answer killed this question*. It no longer applies.
The head ends up answered with `answer.text = "cauterised"` and `cauterised_by`
set, so the record survives and the frontier clears. `--why` takes `-` like the
other prose flags; inline in quotes is fine for one clause. `--by` must name an
**answered** head, or exit 3.

```bash
hydra cauterise numbering --by graph-shape --why - <<'EOF'
slugs are stable across re-parenting; hierarchical numbers are not, and
graph-shape settled on a spanning tree whose heads get re-parented.
EOF
```

**`reopen`** — *ask it again*. The premise moved. The old answer is kept as
`prior`, so you re-present it and ask whether it still holds; the answer is
usually one word.

```bash
hydra reopen graph-shape
```

## Cascades and `.reopened`

Re-answering a head transitively reopens its descendants and everything gated by
it. Every mutation response carries `.reopened`: a slug array, **the heads to
re-present**. Work through them, showing each old answer back and asking whether
it still holds. They block `done`, so they cannot be skipped.

The old answer is on `hydra resume`'s skeleton rows as `prior_summary` (first line
only) — one call covers the whole cascade — or in full as `.prior.text` from
`hydra show <slug>`.

`--keep-subtree` on `cut` suppresses the cascade. Use it for a typo or a
rewording, never when the substance changed. To fix the *question* rather than the
answer, `hydra reword <slug> --question <text|->` leaves the answer alone.

## After a context reset

Nothing is injected back into your context — not after a compaction, not after a
`/clear`, not in a new session next week. Recovery is yours and it is one
command:

```bash
hydra resume
```

If you find yourself mid-interview with no memory of it, that is the situation
this tool exists for. Run `resume`, read the skeleton, carry on from `next`. Do
not summarise what you have lost, do not start over, and do not re-ask anything
the skeleton shows as answered.

## Finishing

The interview ends when the tree is done, not when it feels done.

```bash
hydra status        # exit 0 = done, exit 4 = open heads remain
```

Exit 4 means keep going. Nothing in the runtime will stop you wrapping up early,
so this is discipline rather than enforcement: check `status` before you conclude
anything, and if heads remain, say what is left rather than summarising as though
it were finished. Stopping because the *user* asked to stop is always fine — the
tree is on disk and the next session picks it up.

Commit `.hydra/<slug>.json`: the decisions are about the code and belong in its
history.
