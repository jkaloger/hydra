#!/usr/bin/env bash
#
# Manual smoke test for the `hydra` CLI (SPEC §8: the CLI's output shapes are not
# unit tested, so this is their only coverage). Drives the whole verb surface of
# §5 against a scratch store in a temp dir, checking the shapes with `jq` and the
# exit codes against the table in `hydra --help`.
#
# Adversarial on purpose: every §4 rejection reachable from the CLI, the three
# `--force` overrides, the `-` stdin path and its misuse, and the nonzero
# `status`. Prints each command it runs and stops at the first surprise.
#
#   scripts/smoke.sh

set -euo pipefail

# Assigned then made readonly, never both at once: the `readonly` builtin reports
# its own success, which would mask a failing command substitution from `set -e`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; readonly ROOT
readonly BIN="$ROOT/target/debug/hydra"

command -v jq >/dev/null || { echo "smoke: jq is required" >&2; exit 1; }

echo "== cargo build"
cargo build --manifest-path "$ROOT/Cargo.toml"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/hydra-smoke.XXXXXX")"; readonly WORK
trap 'rm -rf "$WORK"' EXIT
readonly OUT="$WORK/stdout"
readonly ERR="$WORK/stderr"

# The last command `run` executed, for the failure message.
STEP=""

# Stops at the first surprise: everything after a wrong exit code or a wrong
# shape is running against a tree in a state this script did not intend, so the
# failures after it would be noise.
fail() {
  echo "  FAIL: $*" >&2
  echo "  --- stdout" >&2; sed 's/^/  | /' "$OUT" >&2
  echo "  --- stderr" >&2; sed 's/^/  | /' "$ERR" >&2
  exit 1
}

# run <expected-exit> <args...>; set IN beforehand to feed stdin.
run() {
  local want="$1"; shift
  local code=0
  if [ -n "${IN:-}" ]; then
    echo "+ printf %s \"\$IN\" | hydra $*"
    printf '%s' "$IN" | "$BIN" "$@" >"$OUT" 2>"$ERR" || code=$?
  else
    echo "+ hydra $*"
    "$BIN" "$@" >"$OUT" 2>"$ERR" </dev/null || code=$?
  fi
  IN=""
  STEP="hydra $*"
  [ "$code" -eq "$want" ] || fail "expected exit $want, got $code"
}

# Every jq check is `-e`, so a false or null result is a failure too.
jqok() {
  jq -e "$1" "$OUT" >/dev/null || fail "jq check failed on '$STEP': $1"
}

jqv() { jq -r "$1" "$OUT"; }

same() {
  [ "$1" = "$2" ] || fail "expected '$2', got '$1' ($3)"
}

err_has() {
  grep -qF -- "$1" "$ERR" || fail "stderr should name '$1' for '$STEP'"
}

stdout_empty() {
  [ ! -s "$OUT" ] || fail "stdout must stay clean when '$STEP' is refused"
}

stderr_empty() {
  [ ! -s "$ERR" ] || fail "stderr should be empty for '$STEP'"
}

# A rejection must leave the tree byte-identical (§4).
fingerprint() { cksum "$1" | awk '{print $1, $2}'; }

echo
echo "== --help is what the skill reads (§6), so it carries the contract"
run 0 --help
grep -q "alias: sear" "$OUT" || fail "--help should advertise the sear alias"
grep -q "4  \`status\` only: open heads remain" "$OUT" \
  || fail "--help should carry the exit-4 row"
grep -q "one \`-\` per invocation" "$OUT" || fail "--help should state the stdin rule"
grep -q "3  a slug was refused" "$OUT" || fail "--help should not call a read a write"

echo
echo "== no store, no HEAD (exit 5)"
mkdir -p "$WORK/bare"
cd "$WORK/bare"
run 5 status
stdout_empty
err_has "no .hydra directory"
mkdir .hydra
run 5 status
err_has "HEAD is missing"
# `trees` is the one read that tolerates a missing HEAD: it lists the store, and
# a store with nothing in it yet is not an error.
run 0 trees
jqok '.head == null and .trees == []'
rm -rf .hydra

echo
echo "== init"
mkdir -p "$WORK/smoke-repo"
cd "$WORK/smoke-repo"
run 0 init --intent 'Smoke the CLI end to end.'
jqok '.op == "init" and .tree == "smoke-repo"'
jqok '.counts.done == true and .counts.open == 0'
jqok 'has("slug") == false'
same "$(cat .hydra/HEAD)" "smoke-repo" "init points HEAD at the tree it made"
[ -f .hydra/smoke-repo.json ] || fail "init should have written the tree file"

run 0 init hydra-design --intent 'Design hydra itself: storage, invariants, surface.'
jqok '.tree == "hydra-design"'
same "$(cat .hydra/HEAD)" "hydra-design" "init moves HEAD"

run 5 init hydra-design --intent 'Design hydra itself: storage, invariants, surface.'
stdout_empty
err_has "already exists"

echo "-- --intent is required, and blank does not satisfy it"
run 2 init no-intent
err_has "--intent"
run 2 init blank-intent --intent '   '
err_has "blank"
[ ! -f .hydra/blank-intent.json ] || fail "a refused init should not have made a tree"

echo
echo "== init from a subdirectory adopts the store above it"
mkdir -p sub/deep
(cd sub/deep && "$BIN" init nested --intent 'Exercise store adoption from a subdirectory.' >"$OUT" 2>"$ERR") || fail "nested init should succeed"
STEP="hydra init nested (in sub/deep)"
err_has "using the store at"
[ ! -d sub/deep/.hydra ] || fail "a nested .hydra/ would shadow the store above it"
jqok '.tree == "nested"'

echo "-- the default slug comes from the store's directory, not the cwd, so a"
echo "   bare init from a subdirectory is a duplicate rather than a new tree"
code=0
(cd sub/deep && "$BIN" init --intent 'Exercise the default slug.' >"$OUT" 2>"$ERR") || code=$?
STEP="hydra init (in sub/deep)"
[ "$code" -eq 5 ] || fail "expected exit 5, got $code"
err_has "tree 'smoke-repo' already exists"

echo
echo "== use / trees"
run 5 use ghost-tree
stdout_empty
err_has "unknown tree 'ghost-tree'"
# A malformed slug never reaches the store, so it is a refused slug (3), not a
# missing tree (5).
run 3 use "Not A Slug"
err_has "malformed slug"
same "$(cat .hydra/HEAD)" "nested" "a refused use must not move HEAD"

run 0 use hydra-design
jqok '.op == "use" and .tree == "hydra-design"'

run 0 trees
jqok '.head == "hydra-design"'
jqok '(.trees | length) == 3'
jqok '[.trees[] | select(.current)] | length == 1'
jqok '[.trees[] | select(.current) | .tree] == ["hydra-design"]'
jqok '[.trees[].tree] == ["hydra-design", "nested", "smoke-repo"]'

echo
echo "== empty tree: ready is [], next is null, resume is done (all exit 0)"
run 0 use nested
run 0 ready
jqok '. == []'
run 0 next
jqok '. == null'
run 0 resume
jqok '.counts.done == true and .next == null and .skeleton == [] and .hydrated == []'
jqok '.intent == "Exercise store adoption from a subdirectory."'
run 0 status
jqok '.done == true'
stderr_empty
run 0 use hydra-design

echo
echo "== sprout"
run 0 sprout --question "What does hydra look like from outside?" --slug consumption-surface
jqok '.op == "sprout" and .slug == "consumption-surface" and .tree == "hydra-design"'
jqok '.reopened == [] and .counts.ready == 1'
run 0 sprout --question "Strict tree, tree + dep edges, or pure DAG?" --parent consumption-surface --slug graph-shape
run 0 sprout --question "What does a head store?" --parent graph-shape --slug head-schema
run 0 sprout --question "Which states does a head have?" --parent graph-shape --slug lifecycle
run 0 sprout --question "How is a tree stored?" --parent consumption-surface --slug storage-format

echo "-- --blocked-by is comma-separated on sprout (§5's <slug>,...)"
run 0 sprout --question "Append-only or mutable?" --parent storage-format --slug write-model \
  --blocked-by head-schema,lifecycle
run 0 show write-model
jqok '.blocked_by == ["head-schema", "lifecycle"] and .state == "blocked"'
jqok '.open_blockers == ["head-schema", "lifecycle"]'

echo "-- ...and repeatable, which is the other half of clap's value_delimiter"
run 0 sprout --question "Repeated flags?" --slug repeated-flags \
  --blocked-by head-schema --blocked-by lifecycle
run 0 show repeated-flags
jqok '.blocked_by == ["head-schema", "lifecycle"]'

echo "-- no --slug means the slug is derived from the question"
run 0 sprout --question "What shape is the resume payload?" --parent storage-format
same "$(jqv .slug)" "what-shape-is-the-resume-payload" "derived slug"
readonly RESUME_SHAPE="what-shape-is-the-resume-payload"

echo
echo "== sprout rejections leave the tree alone (exit 3)"
readonly TREE_FILE="$WORK/smoke-repo/.hydra/hydra-design.json"
before="$(fingerprint "$TREE_FILE")"
run 3 sprout --question "orphan?" --parent ghost
stdout_empty
err_has "ghost"
run 3 sprout --question "stranded?" --blocked-by lifecycle,ghost
err_has "ghost"
run 3 sprout --question "a duplicate?" --slug graph-shape
err_has "duplicate slug 'graph-shape'"
run 3 sprout --question "malformed?" --slug "Not A Slug"
err_has "malformed slug"
run 3 sprout --question "???"
err_has "malformed slug"
same "$(fingerprint "$TREE_FILE")" "$before" "a rejected sprout must not touch the file"

echo
echo "== cut: --answer - reads stdin, --reject is repeatable"
IN="CLI unix tool
JSON on stdout, no lazyspec coupling, no harness assumptions.
"
run 0 cut consumption-surface --answer - \
  --rationale "the plugin is an adapter and is optional" \
  --reject "MCP server: not designed yet, and the shim stays possible later" \
  --reject "TUI: non-goal: hydra is a store, the LLM is the interface"
jqok '.op == "cut" and .slug == "consumption-surface" and .reopened == []'
jqok '.counts.answered == 1'

run 0 show consumption-surface
jqok '.answer.text == "CLI unix tool\nJSON on stdout, no lazyspec coupling, no harness assumptions."'
jqok '.answer.rationale == "the plugin is an adapter and is optional"'
jqok '(.answer.rejected | length) == 2'
jqok '.answer.rejected[0] == {"option": "MCP server", "why_not": "not designed yet, and the shim stays possible later"}'
echo "-- --reject splits on the first ':' only"
jqok '.answer.rejected[1].option == "TUI"'
jqok '.answer.rejected[1].why_not == "non-goal: hydra is a store, the LLM is the interface"'
jqok '.state == "answered" and .status == "answered" and .rev == 1'

echo
echo "== cut: stdin misuse is a usage error (exit 2)"
IN="prose"
run 2 cut graph-shape --answer - --rationale -
stdout_empty
err_has "only one field can read stdin"
IN=""
run 2 cut graph-shape --answer -
err_has "stdin was empty"
run 2 cut graph-shape --answer "x" --reject "no colon here"
err_has "no ':' to split on"
run 2 cut graph-shape --answer "x" --reject ": empty option"
err_has "the option is empty"
run 2 cut graph-shape --answer "x" --reject "empty reason:"
err_has "the reason is empty"

echo "-- --rationale - on its own is the same path as --answer -"
IN="prose from stdin, on the rationale this time"
run 0 cut lifecycle --answer "two, open and answered" --rationale -
run 0 show lifecycle
jqok '.answer.rationale == "prose from stdin, on the rationale this time"'
run 0 reopen lifecycle

echo
echo "== §4.5: cutting a head with unanswered blockers (exit 3), and --force"
run 0 cut graph-shape --answer "spanning tree + blocked_by cross edges"
before="$(fingerprint "$TREE_FILE")"
run 3 cut write-model --answer "mutable document"
stdout_empty
err_has "write-model"
err_has "head-schema"
same "$(fingerprint "$TREE_FILE")" "$before" "a rejected cut must not touch the file"

run 0 cut write-model --answer "mutable document, git is the history" --force
jqok '.op == "cut" and .reopened == []'
run 0 show write-model
echo "-- §4: --force records nothing, so the edge it was forced past is the only trace"
jqok '.state == "answered" and .open_blockers == ["head-schema", "lifecycle"]'
jqok '.answer.rationale == null and .answer.rejected == []'

echo
echo "== flags §5 does not list are not silently accepted (exit 2)"
echo "-- --keep-subtree is cut's alone; reopen always cascades"
run 2 reopen graph-shape --keep-subtree
stdout_empty
echo "-- --force is for §4.3, §4.5 and §4.7 only: link, cut and cauterise"
run 2 sprout --question "forced?" --force
run 2 reparent lifecycle --parent consumption-surface --force
run 2 unlink write-model --blocked-by lifecycle --force
run 2 reword lifecycle --question "forced?" --force
run 2 show lifecycle --force
run 2 status --force

echo
echo "== cauterise (alias: sear)"
run 3 cauterise "$RESUME_SHAPE" --by storage-format
stdout_empty
err_has "storage-format"
err_has "unanswered"
run 3 cauterise storage-format --by storage-format
err_has "cannot cauterise itself"
run 3 cauterise ghost --by storage-format
err_has "no head 'ghost'"

run 0 cut storage-format --answer "one JSON file per tree, repo-local, git-tracked"
IN="the resume shape falls out of the storage decision"
run 0 sear "$RESUME_SHAPE" --by storage-format --why -
jqok '.op == "cauterise" and .slug == "what-shape-is-the-resume-payload"'
run 0 show "$RESUME_SHAPE"
jqok '.status == "answered" and .state == "cauterised"'
jqok '.answer.text == "cauterised" and .answer.cauterised_by == "storage-format"'
jqok '.answer.rationale == "the resume shape falls out of the storage decision"'

echo "-- §4.7 forced: cauterise by a head that is not answered"
run 0 sprout --question "Do we ship an MCP shim?" --slug mcp-shim --parent consumption-surface
run 0 cauterise mcp-shim --by lifecycle --force
jqok '.op == "cauterise"'
run 0 show mcp-shim
jqok '.answer.cauterised_by == "lifecycle"'
run 0 show lifecycle
jqok '.status == "open"' # force records nothing

echo
echo "== link / unlink, and §4.3's cycle (exit 3, forceable)"
run 3 link head-schema --blocked-by head-schema
err_has "cycle"
run 0 link lifecycle --blocked-by head-schema
run 3 link head-schema --blocked-by lifecycle
stdout_empty
err_has "head-schema"
err_has "lifecycle"
run 0 link head-schema --blocked-by lifecycle --force
run 0 show head-schema
jqok '.blocked_by == ["lifecycle"]'
run 0 unlink head-schema --blocked-by lifecycle
run 0 unlink head-schema --blocked-by lifecycle # idempotent
run 0 show head-schema
jqok '.blocked_by == []'
run 3 link lifecycle --blocked-by ghost
err_has "ghost"

echo
echo "== §4.9: an edge whose reopen cascade would cycle (exit 3, forceable on link)"
# In the empty `nested` tree, so the frontier of `hydra-design` is left alone: the
# only way to show this one is to drive a tree to done twice.
run 0 use nested
run 0 sprout --question "parent?" --slug wedge-a
run 0 sprout --question "child?" --parent wedge-a --slug wedge-b
run 0 sprout --question "grandchild?" --parent wedge-b --slug wedge-c
run 0 sprout --question "elsewhere?" --slug wedge-d

echo "-- gating a head on its own child: cutting either would reopen the other"
run 3 link wedge-a --blocked-by wedge-b
stdout_empty
err_has "wedge-a -> wedge-b -> wedge-a"
run 0 show wedge-a
jqok '.blocked_by == []'

echo "-- and on a grandchild: the walk climbs the ancestry to find the loop"
run 3 link wedge-a --blocked-by wedge-c
stdout_empty
err_has "wedge-a -> wedge-c -> wedge-b -> wedge-a"

echo "-- the converse direction is the benign one, and is the point of §2's cross edge"
run 0 link wedge-c --blocked-by wedge-a
run 0 link wedge-d --blocked-by wedge-a

echo "-- reparent closes the same loop with no blocked_by write at all"
run 3 reparent wedge-a --parent wedge-d
stdout_empty
err_has "wedge-a -> wedge-d -> wedge-a"
run 0 show wedge-a
jqok '.parent == null'

echo "-- a first answer never cascades, so the frontier drains either way"
for _ in $(seq 1 8); do
  slug="$("$BIN" next | jq -r '.slug // empty')"
  [ -n "$slug" ] || break
  run 0 cut "$slug" --answer "settled: $slug"
done
run 0 status
jqok '.done == true'

echo "-- re-answering the root cascades down and stops: this is what §4.9 protects"
run 0 cut wedge-a --answer "revised"
jqok '.reopened == ["wedge-b", "wedge-c", "wedge-d"]'
for _ in $(seq 1 12); do
  slug="$("$BIN" next | jq -r '.slug // empty')"
  [ -n "$slug" ] || break
  run 0 cut "$slug" --answer "re-settled: $slug"
done
run 0 status
jqok '.done == true and .open == 0'

echo "-- forced, the edge lands and records nothing: if you force it, you own it"
run 0 link wedge-a --blocked-by wedge-b --force
run 0 show wedge-a
jqok '.blocked_by == ["wedge-b"]'
echo "   and now the same loop ping-pongs: 12 cuts, still not done"
run 0 cut wedge-b --answer "revised again"
for _ in $(seq 1 12); do
  slug="$("$BIN" next | jq -r '.slug // empty')"
  [ -n "$slug" ] || break
  run 0 cut "$slug" --answer "re-settled: $slug"
done
run 4 status
jqok '.done == false'

echo "-- unlink is the way back out, and then it finishes"
run 0 unlink wedge-a --blocked-by wedge-b
for _ in $(seq 1 12); do
  slug="$("$BIN" next | jq -r '.slug // empty')"
  [ -n "$slug" ] || break
  run 0 cut "$slug" --answer "re-settled: $slug"
done
run 0 status
jqok '.done == true'
run 0 use hydra-design

echo
echo "== tree is for eyes, not for jq (all four glyphs are on the board here)"
echo "+ hydra tree"
"$BIN" tree >"$OUT" 2>"$ERR"
STEP="hydra tree"
stderr_empty
if jq -e . "$OUT" >/dev/null 2>&1; then fail "tree should not be JSON"; fi
grep -q "hydra-design  (" "$OUT" || fail "tree should head with the tree name and counts"
grep -q "← next" "$OUT" || fail "tree should mark next"
grep -qE "⊘ what-shape-is-the-resume-payload +cauterised by storage-format" "$OUT" \
  || fail "tree should show the cauterised head and its killer"
grep -q "◌ " "$OUT" || fail "tree should show a blocked head"
grep -q "└── " "$OUT" || fail "tree should draw connectors"
if grep -q $'\x1b' "$OUT"; then fail "tree into a file should carry no ANSI"; fi
sed 's/^/  /' "$OUT"

echo
echo "== reparent, including rooting a head with --parent ''"
run 3 reparent consumption-surface --parent graph-shape
stdout_empty
err_has "own ancestor"
run 3 reparent graph-shape --parent graph-shape
err_has "own ancestor"
run 3 reparent lifecycle --parent ghost
err_has "parent 'ghost' does not exist"
run 0 reparent lifecycle --parent ""
run 0 show lifecycle
jqok '.parent == null and .ancestors == []'
run 0 reparent lifecycle --parent graph-shape
run 0 show lifecycle
jqok '.parent == "graph-shape" and .ancestors == ["consumption-surface", "graph-shape"]'

echo
echo "== reword leaves the answer alone"
IN="Which states does a head have, and what is derived?"
run 0 reword lifecycle --question -
jqok '.op == "reword"'
run 0 reword graph-shape --question "Strict tree, tree + cross edges, or pure DAG?"
run 0 show graph-shape
jqok '.question == "Strict tree, tree + cross edges, or pure DAG?"'
jqok '.answer.text == "spanning tree + blocked_by cross edges" and .rev == 1'
run 3 reword ghost --question "no such head?"
err_has "no head 'ghost'"

echo
echo "== reopen: always cascades; an open head cannot be reopened (exit 3)"
run 0 cut head-schema --answer "answer{text, rationale, rejected, cauterised_by}"
run 3 reopen lifecycle
stdout_empty
err_has "illegal transition open -> open"
run 0 reopen graph-shape
jqok '.op == "reopen" and .slug == "graph-shape"'
echo "-- the cascade reaches head-schema and, through its cross edge, write-model"
jqok '.reopened == ["head-schema", "write-model"]'
echo "-- lifecycle is open already, so it is walked through but not reported"
jqok '(.reopened | index("lifecycle")) == null'
jqok '(.reopened | index("graph-shape")) == null'
run 0 cut graph-shape --answer "spanning tree + blocked_by cross edges"

echo
echo "== re-answering cascades; --keep-subtree does not"
run 0 cut head-schema --answer "answer{text, rationale, rejected, cauterised_by}"
run 0 cut consumption-surface --answer "CLI unix tool, plus an optional plugin"
jqok '(.reopened | length) > 1'
jqok '.reopened | index("graph-shape") != null and index("head-schema") != null'
run 0 cut consumption-surface --answer "CLI unix tool, plus an optional plugin." --keep-subtree
jqok '.reopened == []'
run 0 show head-schema
jqok '.status == "open"'
echo "-- §2: a reopened head keeps its prior answer for context"
jqok '.answer == null and .prior.text == "answer{text, rationale, rejected, cauterised_by}"'
run 0 resume
jqok '[.skeleton[] | select(.slug == "head-schema")][0].prior_summary == "answer{text, rationale, rejected, cauterised_by}"'

echo
echo "== queries"
run 0 ready
jqok 'type == "array" and length > 0'
jqok 'all(.[]; .state == "ready")'
jqok 'all(.[]; has("summary") == false)'
FIRST_READY="$(jqv '.[0].slug')"; readonly FIRST_READY
run 0 next
jqok '.state == "ready"'
same "$(jqv .slug)" "$FIRST_READY" "next is the first ready head in pre-order"

run 3 show ghost
stdout_empty
err_has "no head 'ghost'"

run 0 resume
echo "-- §7's field order is deliberate; a Value round-trip would sort it"
same "$(jqv 'keys_unsorted | join(",")')" "intent,counts,next,skeleton,hydrated" "resume field order"
jqok '.intent == "Design hydra itself: storage, invariants, surface."'
jqok '.counts.tree == "hydra-design"'
jqok '(.skeleton | length) == 9'
echo "-- depth first, siblings by seq (§3) — not the slug order the file is keyed by"
jqok '[.skeleton[].slug] == ["consumption-surface", "graph-shape", "head-schema", "lifecycle", "storage-format", "write-model", "what-shape-is-the-resume-payload", "mcp-shim", "repeated-flags"]'
jqok '.hydrated[-1].slug == .next'
jqok '[.hydrated[].slug] == ([.hydrated[-1].ancestors[]] + [.next])'

echo
echo "== status exits 4 while open heads remain, and stdout stays parseable"
run 4 status
stderr_empty
jqok '.done == false and .open > 0'
jqok '.answered + .open == 9'
jqok '.ready + .blocked == .open'

echo
echo "== every mutation echoes the tree it wrote to (§4)"
run 0 reword lifecycle --question "Which states does a head have?"
same "$(jqv .tree)" "$(cat "$WORK/smoke-repo/.hydra/HEAD")" "mutation .tree vs HEAD"
same "$(jqv .counts.tree)" "$(jqv .tree)" ".counts.tree agrees with .tree"

echo
echo "== completion candidates come from the store (§5)"
# The shell's own protocol: `COMPLETE=<shell> hydra -- hydra <words>` with the
# index of the word under the cursor in the environment. An empty last word is
# the `<TAB>` pressed after a space.
comp() {
  local index="$1"; shift
  echo "+ COMPLETE=bash _CLAP_COMPLETE_INDEX=$index hydra -- $*"
  STEP="completion of: $*"
  COMPLETE=bash _CLAP_COMPLETE_INDEX="$index" _CLAP_IFS=$'\n' \
    "$BIN" -- "$@" >"$OUT" 2>"$ERR" </dev/null || fail "the completer exited nonzero"
  stderr_empty
}

comp_has() {
  grep -qxF -- "$1" "$OUT" || fail "candidates for '$STEP' should include '$1'"
}

comp_lacks() {
  if grep -qxF -- "$1" "$OUT"; then fail "candidates for '$STEP' should not include '$1'"; fi
}

# Read off the tree rather than hard-coded: the sections above have been
# cutting and reopening, and which slug is in which state is their business.
answered="$("$BIN" resume | jq -r '[.skeleton[] | select(.state == "answered")][0].slug')"
open="$("$BIN" next | jq -r .slug)"
echo "-- answered: $answered · open: $open"

comp 2 hydra use ''
comp_has "hydra-design"
comp_has "smoke-repo"
comp_lacks "$open"

comp 2 hydra cut ''
comp_has "$open"
echo "-- an answered head is offered too: re-answering is a cut, not a rejection"
comp_has "$answered"

echo "-- reopen and cauterise --by take answered heads only (§4.6, §4.7)"
comp 2 hydra reopen ''
comp_has "$answered"
comp_lacks "$open"
comp 4 hydra cauterise "$open" --by ''
comp_has "$answered"
comp_lacks "$open"

echo "-- a prefix narrows the set the shell would have narrowed anyway"
comp 2 "hydra" "cut" "${answered:0:3}"
comp_has "$answered"

echo "-- the element under the cursor completes inside a comma-separated list"
comp 3 hydra sprout --blocked-by "$answered,"
comp_has "$answered,$open"

echo "-- zsh gets the state glyph as the description"
STEP="zsh descriptions"
COMPLETE=zsh _CLAP_COMPLETE_INDEX=2 _CLAP_IFS=$'\n' \
  "$BIN" -- hydra cut '' >"$OUT" 2>"$ERR" </dev/null || fail "the completer exited nonzero"
grep -qF -- "$answered:● " "$OUT" || fail "an answered candidate should be described '● <question>'"

echo "-- outside a store there is nothing to name, and nothing to say about it"
cd "$WORK/bare"
comp 2 hydra cut ''
comp_lacks "$answered"
comp_has "--answer"
cd "$WORK/smoke-repo"

echo
echo "== drive the frontier to done the way a skill would"
for _ in $(seq 1 20); do
  slug="$("$BIN" next | jq -r '.slug // empty')"
  [ -n "$slug" ] || break
  # --keep-subtree: every head here has been answered before, so a plain
  # re-answer would reopen the ones already settled and the loop would not end.
  run 0 cut "$slug" --answer "settled: $slug" --keep-subtree
done
run 0 status
jqok '.done == true and .open == 0'
stderr_empty
run 0 next
jqok '. == null'
run 0 ready
jqok '. == []'
run 0 trees
jqok '[.trees[] | select(.tree == "hydra-design") | .counts.done] == [true]'
jqok 'all(.trees[]; has("error") == false)'

echo
echo "== claude-plugin: the two files of §6"
# The plugin is data, not code: a manifest and a skill, no hooks and no scripts.
# So what is checkable from here is that both files are there, parse, and carry
# the fields Claude Code addresses them by.
readonly PLUGIN="$ROOT/claude-plugin"
readonly MANIFEST="$PLUGIN/.claude-plugin/plugin.json"
readonly SKILL="$PLUGIN/skills/hydra/SKILL.md"

echo "-- §6's two files and nothing else: a third would mean hooks or scripts crept back"
same "$(cd "$PLUGIN" && find . -type f | sort | tr '\n' ' ')" \
  "./.claude-plugin/plugin.json ./skills/hydra/SKILL.md " \
  "the plugin's file list"

echo "+ jq . plugin.json"
jq -e '.name == "hydra"' "$MANIFEST" >/dev/null || fail "plugin.json should parse and name itself"
# No manifest field declares a binary as a prerequisite, so the description is
# where a user finds out they need one.
jq -e '.description | test("hydra` binary")' "$MANIFEST" >/dev/null \
  || fail "plugin.json should name the binary prerequisite"

echo "+ head -3 skills/hydra/SKILL.md"
grep -qx 'name: hydra' "$SKILL" || fail "the skill must be named hydra (§6)"
grep -q '^description: ' "$SKILL" || fail "the skill needs a description to trigger on"
# `/hydra:hydra`, both halves: Claude Code addresses a plugin's skill
# `plugin:skill` and does not collapse the case where the names match. The skill's
# own description is now the only place a user is told what to type.
grep -q '/hydra:hydra' "$SKILL" || fail "the skill should name itself /hydra:hydra"

echo
echo "== the marketplace that makes the plugin installable"
# Claude Code only looks for a marketplace at the repo root, and a relative
# `source` resolves against that root rather than against `.claude-plugin/`.
readonly MARKETPLACE="$ROOT/.claude-plugin/marketplace.json"

echo "+ jq . marketplace.json"
jq -e '.name == "hydra"' "$MARKETPLACE" >/dev/null \
  || fail "marketplace.json should parse and name itself"
jq -e '.plugins == [.plugins[0]] and .plugins[0].name == "hydra"' "$MARKETPLACE" >/dev/null \
  || fail "the marketplace should list the one plugin, named for /plugin install hydra@hydra"
# The path is the whole point of the file: get it wrong and the install 404s
# with the manifest still validating.
same "$(jq -r '.plugins[0].source' "$MARKETPLACE")" "./claude-plugin" \
  "the marketplace's plugin source"
test -f "$ROOT/$(jq -r '.plugins[0].source' "$MARKETPLACE")/.claude-plugin/plugin.json" \
  || fail "the source path should resolve from the repo root to a real manifest"

echo "-- .claude/skills/hydra: the dogfood symlink, which a rename silently breaks"
test -e "$ROOT/.claude/skills/hydra" \
  || fail ".claude/skills/hydra is dangling; repoint it at claude-plugin/skills/hydra"

echo
echo "== the stored file is still the shape §3 asks for"
# No pipes into `grep -q`: it exits at the first match, and under `pipefail` the
# SIGPIPE that gives the upstream would turn the pipeline into 141.
# `created_at` is the first key of a sorted top level (§3); in declaration order
# it would be third, behind `version` and `slug`.
grep -q '"created_at"' <<<"$(sed -n 2p "$TREE_FILE")" || fail "sorted keys, pretty-printed"
# `od` reads to EOF, so only the `grep -q` end of this needed unpicking.
grep -q '}  \\n' <<<"$(tail -c 2 "$TREE_FILE" | od -c)" || fail "file should end in }\\n"
jq -e '.version == 1 and (.heads | length) == 9' "$TREE_FILE" >/dev/null \
  || fail "the tree file should still parse"

echo
echo "== exit 1: a tree file this hydra cannot read"
# Last, because it breaks the tree on purpose. `trees` is the command you reach
# for when this happens, so it must survive what the others die on.
printf '{ not json' > "$TREE_FILE"
run 1 status
stdout_empty
err_has "hydra-design.json"
err_has "malformed JSON"
run 1 resume
run 1 tree
run 0 trees
jqok '[.trees[] | select(.tree == "hydra-design") | .error] | length == 1'
jqok '[.trees[] | select(.tree == "hydra-design") | has("counts")] == [false]'
echo "-- the other trees are still listed, with their counts"
jqok '[.trees[] | select(.error == null) | .tree] == ["nested", "smoke-repo"]'
jqok '[.trees[] | select(.tree == "smoke-repo") | .counts.done] == [true]'

echo
echo "smoke: OK"
