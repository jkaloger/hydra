---
name: hydra
description: Interview the user relentlessly about a plan or design. Use when the user wants to plan/design a system, break down a problem, asks an open question, or wants to continue from a previous interview.
---

# hydra — the interview protocol

```
resume            skeleton + hydrated ancestry
sprout ×N         the whole known decomposition, once, before asking anything
next              head to ask
  → tree, then 2-4 options with tradeoffs, a recommendation, the question, WAIT
cut <slug>        answer + rationale + rejected[]
sprout            heads the answer genuinely opened
cauterise         heads the answer killed
```

One question per turn. Never two. Never answer for the user.

`hydra` is a store with invariants. It never reads question text and has no
opinion about what to ask, what matters, or what order is best — it hands back
the first _askable_ head in document order. Option generation, tradeoffs, the
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

After `init` the tree is empty: lay it out before you ask anything. See **Laying
the tree out**.

## Exit codes are the interface

`hydra --help` lists all six. Four need judgement rather than a lookup:

- **4** — `status` only, open heads remain. The normal state of a live interview.
  Never treat it as an error, and never wrap it in a way that aborts on nonzero.
- **3** — a slug was refused; stderr names it. Commonest cause: cutting a head
  whose `blocked_by` heads are not answered yet. Answer those first.
- **5** — tree addressing, **including a tree that already exists**. Read stderr
  before retrying: `init` on an existing tree is a 5, and running it again just
  gets another.
- **1** — I/O, a file that will not parse, a lock that would not come free. Tell
  the user; `hydra trees` names the broken file. Not yours to fix by retrying.

`hydra next` printing `null` is the **done** signal, as is `[]` from `hydra
ready`. Both exit 0.

## Reading the tree

Two tiers, because a large tree is mostly settled branches irrelevant to the
next question:

- **`hydra resume`** — everything you need cold, `next` plus a skeleton of every
  head. **Scan the skeleton before you ask anything**, so you do not re-ask a
  question already settled in a distant branch. That duplication is semantic and
  nothing in the tool can catch it.
- **`hydra show <slug>`** — one head, full detail. Reach for it when the skeleton
  line is not enough to ask the question well.
- **`hydra next`** — the head to ask, hydrated; its `ancestors` are its premises.
  `hydra ready` is every askable head if you want to choose a different one.
- **`hydra tree`** — the ASCII render, the one output for eyes rather than for
  you. It opens every question turn; see **Asking**.

## Laying the tree out

A tree fresh from `hydra init` has no heads. Before you ask anything, sprout the
whole decomposition you can already see in the brief: every question the design
has to answer, at every depth, with its parents and its `blocked_by` edges. One
`sprout` per head, issued as a batch. Then `hydra resume` and start asking.

Do this even when the first question is obvious. A tree grown one head at a time
never shows the user what the interview covers, so gaps and scope creep both stay
invisible until the end; and `next` walks document order, so a tree laid out up
front asks in a coherent sequence rather than the order things occurred to you.

A real brief is a dozen heads or more. If your layout has three, you have
restated the brief's headings instead of decomposing it.

Sprout later only for a question that **did not exist** until the user picked
that option. Finding yourself sprouting a head you could have foreseen means the
layout was too thin, not the answer unusually generative.

## Asking

Every question turn has the same four parts, in this order:

1. **The tree** — `hydra tree`, verbatim, in a fenced block. Every turn, no
   exceptions. It is how the user sees where they are, and it costs a few lines.
2. **2-4 real options** — distinct, non-strawman, each with the tradeoff that
   actually decides between them.
3. **A recommendation**, and why.
4. **The question.** Then **stop and wait**. Do not proceed on an assumed answer.

Read the head's `ancestors` before you write any of it — those are its premises.
An answer that contradicts a parent is a signal to `reopen` the parent, not to
answer this head.

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
- **Fill `--reject` whenever an option was genuinely considered and killed.**
  `rejected[]` is what stops a future session re-proposing a dead branch.

Then open what the answer opened, and kill what it killed:

```bash
hydra sprout --question 'What does a head store?' --parent graph-shape --slug head-schema
hydra sprout --question 'Append-only or mutable?' --parent storage-format --slug write-model \
  --blocked-by head-schema,lifecycle
```

- `--parent` nests for narrative; omit it for a root. `--blocked-by` is for a
  real cross-branch dependency — this head cannot be asked until that one is
  answered. Do not fake a parent to express gating.
- Graph surgery on an existing head: `link`, `unlink`, `reparent` (`--parent ''`
  roots it).
- **Never gate a head on its own descendant.** The cascade walks children and
  `blocked_by` as one relation, so `link a --blocked-by b` where `b` sits under
  `a` makes the two reopen each other forever — the interview can never reach
  done. Hydra refuses the edge (exit 3, naming the loop); `link --force`
  overrides it, and is the one flag that can build an unfinishable tree. Do not.
  Gating on an **ancestor** is the normal, correct case: staleness already flows
  down the tree, so the edge adds no loop.

## cauterise vs reopen

They are not the same and mixing them corrupts the record.

**`cauterise`** — a _sibling's answer killed this question_. It no longer applies.
The head ends up answered, with `cauterised_by` set, so the record survives and
the frontier clears.

```bash
hydra cauterise numbering --by graph-shape --why - <<'EOF'
slugs are stable across re-parenting; hierarchical numbers are not, and
graph-shape settled on a spanning tree whose heads get re-parented.
EOF
```

**`reopen`** — _ask it again_. The premise moved. The old answer is kept as
`prior`, so you re-present it and ask whether it still holds; the answer is
usually one word.

```bash
hydra reopen graph-shape
```

Either way the change cascades: re-answering a head transitively reopens its
descendants and everything gated by it. Every mutation response carries
`.reopened`, **the heads to re-present**. Work through them, showing each old
answer back and asking whether it still holds — `resume`'s skeleton carries them
as `prior_summary`, so one call covers the whole cascade. They block `done`, so
they cannot be skipped.

`cut --keep-subtree` suppresses the cascade: for a typo or a rewording, never when
the substance changed. To fix the _question_ rather than the answer, `hydra reword`
leaves the answer alone.

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
it were finished. Stopping because the _user_ asked to stop is always fine — the
tree is on disk and the next session picks it up.

Commit `.hydra/<slug>.json`: the decisions are about the code and belong in its
history.
