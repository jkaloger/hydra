#!/usr/bin/env bash
#
# Manual smoke test for the `hydra` CLI (SPEC §8: the CLI's output shapes are not
# unit tested, so this is their only coverage). Drives the whole verb surface of
# §5 against a scratch store in a temp dir, checking the shapes with `jq` and the
# exit codes against the table in `hydra --help`.
#
# Adversarial on purpose: every §4 rejection reachable from the CLI, the three
# `--force` overrides, the `-` stdin path and its misuse, the nonzero `status`,
# and — for the §6 verbs — every gate a hook has to refuse as well as the ones it
# fires on. Prints each command it runs and stops at the first surprise.
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

# The contract every `hydra hook` invocation owes Claude Code, whatever it was
# handed: exit 0 (checked by the `run 0` that precedes this), one JSON object on
# stdout, and not a word on stderr. A hook that breaks any of the three breaks
# unrelated sessions in unrelated repos (§6).
hook_wellformed() {
  stderr_empty
  jq -e 'type == "object"' "$OUT" >/dev/null \
    || fail "'$STEP' must write one JSON object"
}

# `{}`: the gate did not match, and the hook has nothing to say.
noop() { jqok '. == {}'; }

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
grep -q "\`hook\` is exempt" "$OUT" || fail "--help should say hooks always exit 0"

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
run 0 init
jqok '.op == "init" and .tree == "smoke-repo"'
jqok '.counts.done == true and .counts.open == 0'
jqok 'has("slug") == false'
same "$(cat .hydra/HEAD)" "smoke-repo" "init points HEAD at the tree it made"
[ -f .hydra/smoke-repo.json ] || fail "init should have written the tree file"

run 0 init hydra-design
jqok '.tree == "hydra-design"'
same "$(cat .hydra/HEAD)" "hydra-design" "init moves HEAD"

run 5 init hydra-design
stdout_empty
err_has "already exists"

echo
echo "== init from a subdirectory adopts the store above it"
mkdir -p sub/deep
(cd sub/deep && "$BIN" init nested >"$OUT" 2>"$ERR") || fail "nested init should succeed"
STEP="hydra init nested (in sub/deep)"
err_has "using the store at"
[ ! -d sub/deep/.hydra ] || fail "a nested .hydra/ would shadow the store above it"
jqok '.tree == "nested"'

echo "-- the default slug comes from the store's directory, not the cwd, so a"
echo "   bare init from a subdirectory is a duplicate rather than a new tree"
code=0
(cd sub/deep && "$BIN" init >"$OUT" 2>"$ERR") || code=$?
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
grep -q "⊘ what-shape-is-the-resume-payload   cauterised by storage-format" "$OUT" \
  || fail "tree should show the cauterised head and its killer"
grep -q "◌ " "$OUT" || fail "tree should show a blocked head"
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
same "$(jqv 'keys_unsorted | join(",")')" "counts,next,skeleton,hydrated" "resume field order"
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
echo "== grill: the session lease of §6"
readonly SESSION="smoke-session-1"
run 0 grill start --session-id "$SESSION"
jqok '.op == "grill start" and .session_id == "smoke-session-1"'
jqok '.tree == "hydra-design" and .counts.tree == "hydra-design"'
jqok 'has("started_at") and .counts.open > 0'
readonly LEASE="$WORK/smoke-repo/.hydra/grill"
[ -f "$LEASE" ] || fail "grill start should have written .hydra/grill"
jq -e '. == {session_id: "smoke-session-1", tree: "hydra-design", started_at: .started_at}' \
  "$LEASE" >/dev/null || fail "the lease should carry exactly §6's three fields"
echo "-- the lease is not a tree, so it must not turn up in \`trees\`"
run 0 trees
jqok '[.trees[].tree] == ["hydra-design", "nested", "smoke-repo"]'

echo "-- restarting the same session must not lie about when the grilling began"
# Backdated by hand. Timestamps are whole seconds (§3), so comparing two `grill
# start` calls a moment apart would agree whatever the code between them did.
jq '.started_at = "2020-01-01T00:00:00Z"' "$LEASE" >"$WORK/lease"
mv "$WORK/lease" "$LEASE"
run 0 grill start --session-id "$SESSION"
same "$(jqv .started_at)" "2020-01-01T00:00:00Z" "grill start is idempotent in a session"
same "$(jq -r .started_at "$LEASE")" "2020-01-01T00:00:00Z" "and the lease agrees"

echo "-- \$CLAUDE_CODE_SESSION_ID is where a real session's id comes from"
code=0
CLAUDE_CODE_SESSION_ID=from-the-environment "$BIN" grill start >"$OUT" 2>"$ERR" </dev/null \
  || code=$?
STEP="CLAUDE_CODE_SESSION_ID=... hydra grill start"
[ "$code" -eq 0 ] || fail "expected exit 0, got $code"
jqok '.session_id == "from-the-environment"'
echo "-- ...and the lease it displaced is named, without implying a live competitor:"
echo "   nothing releases a lease on a clean exit, so this is the usual case"
err_has "replacing a lease left by session smoke-session-1"

echo "-- with neither, a lease no hook could ever match is refused rather than written"
echo "   (exit 2: the fix is on the command line or in the environment, not the store)"
code=0
(unset CLAUDE_CODE_SESSION_ID; "$BIN" grill start >"$OUT" 2>"$ERR" </dev/null) || code=$?
STEP="hydra grill start (no session id)"
[ "$code" -eq 2 ] || fail "expected exit 2, got $code"
stdout_empty
err_has "CLAUDE_CODE_SESSION_ID"
err_has "hydra grill start"
same "$(jq -r .session_id "$LEASE")" "from-the-environment" \
  "a refused start must leave the lease it found alone"

run 0 grill start --session-id "$SESSION"

echo
echo "== hook session-start: one verb, two of §6's rows, split on \`source\`"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"startup\"}"
run 0 hook session-start
hook_wellformed
jqok '.systemMessage | test("^hydra: [0-9]+ open heads on .hydra-design. . /hydra:hydra to resume$")'
jqok 'has("hookSpecificOutput") == false and has("decision") == false'
echo "-- resume is the same row as startup"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"resume\"}"
run 0 hook session-start
jqok 'has("systemMessage")'

echo "-- compact and clear reload the whole resume payload into additionalContext"
for source in compact clear; do
  IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"$source\"}"
  run 0 hook session-start
  hook_wellformed
  jqok '.hookSpecificOutput.hookEventName == "SessionStart"'
  jqok 'has("systemMessage") == false'
  jqok ".hookSpecificOutput.additionalContext | contains(\"($source)\")"
  # additionalContext is text, so the payload inside it has to survive a round
  # trip through `fromjson` to be worth anything to the model.
  jqok '(.hookSpecificOutput.additionalContext | sub("^[^{]*"; "") | fromjson) as $r
        | $r.counts.tree == "hydra-design" and ($r.skeleton | length) == 9
        and $r.hydrated[-1].slug == $r.next'
done

echo "-- the lease is the gate: another session's payload gets nothing"
IN='{"session_id":"another-session","hook_event_name":"SessionStart","source":"compact"}'
run 0 hook session-start
hook_wellformed
noop
echo "-- nor does a payload with no session_id at all"
IN='{"hook_event_name":"SessionStart","source":"compact"}'
run 0 hook session-start
noop
echo "-- nor a source §6's table has no row for"
for source in fork "" STARTUP; do
  IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"$source\"}"
  run 0 hook session-start
  noop
done

echo
echo "== hook post-tool-use: the tree, to the user, after a \`hydra \` command"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"hydra cut lifecycle --answer x\"}}"
run 0 hook post-tool-use
hook_wellformed
jqok '.systemMessage | startswith("hydra-design  (")'
jqok '.systemMessage | contains("← next")'
jqok 'has("decision") == false and has("hookSpecificOutput") == false'
echo "-- it is the same render \`hydra tree\` prints"
"$BIN" tree > "$WORK/render"
same "$(jqv .systemMessage)" "$(cat "$WORK/render")" "systemMessage vs hydra tree"

echo "-- gated on \`hydra\` as a whole word, so every way of naming the binary counts"
for command in "/usr/local/bin/hydra next" '\"$HOME/bin/hydra\" next' \
  "\$(which hydra) ready" "git-hydra sync && hydra next" "hydra"; do
  IN="{\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"$command\"}}"
  run 0 hook post-tool-use
  jqok 'has("systemMessage")'
done
echo "-- ...and a longer name is a different thing, as is reading the store"
for command in "myhydra tree" "./nothydra ready" "foo.hydra tree" "hydraulics --help" \
  "cat .hydra/HEAD" "ls hydra-plugin/hooks" "cargo build" ""; do
  IN="{\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"$command\"}}"
  run 0 hook post-tool-use
  noop
done
echo "-- and on the tool: another tool's input is not this row's subject"
IN='{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"command":"hydra tree"}}'
run 0 hook post-tool-use
noop
echo "-- but on no lease (§6): only a grilling session runs \`hydra cut\`"
IN='{"session_id":"another-session","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"hydra next"}}'
run 0 hook post-tool-use
jqok 'has("systemMessage")'

echo
echo "== hook stop: §6's enforcement"
NEXT_SLUG="$("$BIN" next | jq -r .slug)"; readonly NEXT_SLUG
[ "$NEXT_SLUG" != "null" ] || fail "there should be a next head to block on"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\",\"stop_hook_active\":false}"
run 0 hook stop
hook_wellformed
jqok '.decision == "block"'
jqok '.reason | contains("the interview is not finished")'
jqok '.reason | contains("hydra grill stop")'
jqok 'has("hookSpecificOutput") == false and has("systemMessage") == false'
echo "-- and the reason carries \`hydra next\`, parseable"
same "$(jqv '.reason | sub("^[^{]*"; "") | fromjson | .slug')" "$NEXT_SLUG" \
  "the head the block hands over"

echo "-- but not when HEAD has moved off the leased tree: the head it would hand"
echo "   over is one \`hydra next\` and \`hydra cut\` could not see"
run 0 use nested
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
run 0 hook stop
noop
echo "-- while the compact reload still reads the tree the lease names"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"compact\"}"
run 0 hook session-start
jqok '(.hookSpecificOutput.additionalContext | sub("^[^{]*"; "") | fromjson | .counts.tree) == "hydra-design"'
run 0 use hydra-design
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
run 0 hook stop
jqok '.decision == "block"'

echo "-- at most one block per turn: stop_hook_active is Claude Code's own flag"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\",\"stop_hook_active\":true}"
run 0 hook stop
noop
echo "-- only inside a live lease"
IN='{"session_id":"another-session","hook_event_name":"Stop"}'
run 0 hook stop
noop
echo "-- and a mis-wired hooks.json must never deny somebody else's tool call"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\"}"
run 0 hook stop
noop

echo
echo "== an event name hydra does not know is \`{}\` and exit 0, never a usage error"
# The one thing no unit test can cover: an argument clap would reject never reaches
# hydra's code at all. And on Stop, exit 2 means *show stderr to the model and
# continue the conversation* — so a hooks.json naming the event wrong, or a newer
# plugin against an older binary, would otherwise become an un-lease-gated refusal
# to end the turn in every project, with clap's usage text as the reason.
for event in Stop STOP SessionStart session_start sessionstart pre-compact \
  "stop extra" "post-tool-use --json"; do
  IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
  # Deliberately unquoted: "stop extra" has to arrive as two arguments.
  # shellcheck disable=SC2086
  run 0 hook $event
  hook_wellformed
  noop
done
echo "-- and so is no event at all"
run 0 hook
hook_wellformed
noop
echo "-- the three §9 spells are the three that work"
for event in session-start post-tool-use stop; do
  IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
  run 0 hook "$event"
  hook_wellformed
done
jqok '.decision == "block"'

echo
echo "== a hook is handed garbage in every project the plugin is installed in"
for payload in '   ' 'not json at all' '[]' '42' 'null' '{' '{"session_id":7}' '{}'; do
  for event in session-start post-tool-use stop; do
    IN="$payload"
    run 0 hook "$event"
    hook_wellformed
    noop
  done
done
echo "-- including no stdin at all"
for event in session-start post-tool-use stop; do
  run 0 hook "$event"
  hook_wellformed
  noop
done

echo
echo "== a hook in a repo that is not a hydra repo — the common case"
cd "$WORK/bare"
[ ! -d .hydra ] || fail "this directory is meant to have no store"
for event in session-start post-tool-use stop; do
  IN="{\"session_id\":\"$SESSION\",\"source\":\"startup\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"hydra tree\"}}"
  run 0 hook "$event"
  hook_wellformed
  noop
done
cd "$WORK/smoke-repo"

echo
echo "== grill stop is the kill switch, and idempotent"
run 0 grill stop
jqok '.op == "grill stop" and .released.session_id == "smoke-session-1"'
jqok '.released.tree == "hydra-design"'
[ ! -f "$LEASE" ] || fail "the lease should be gone"
run 0 grill stop
jqok '.released == null'
echo "-- with the lease released the Stop hook lets the turn end"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
run 0 hook stop
noop

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
echo "== a done tree has nothing to announce and nothing to be relentless about"
run 0 grill start --session-id "$SESSION"
jqok '.counts.done == true'
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"startup\"}"
run 0 hook session-start
noop
echo "-- blocking with no head to hand over would wall the session in"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"Stop\"}"
run 0 hook stop
noop
echo "-- but §6 gates the compact reload on the lease alone, so the record still lands"
IN="{\"session_id\":\"$SESSION\",\"hook_event_name\":\"SessionStart\",\"source\":\"compact\"}"
run 0 hook session-start
jqok '(.hookSpecificOutput.additionalContext | sub("^[^{]*"; "") | fromjson | .counts.done) == true'

echo
echo "== hydra-plugin: the wiring of §6, parsed out of hooks.json and executed"
# The plugin ships zero scripts (§6), so its `hooks.json` command strings are the
# whole contract between the plugin and this binary. That makes them the one part
# of the plugin testable from here: parse each one out with `jq` and run it. A
# renamed verb still exits 0 and still prints one JSON object — `hook` is built
# that way on purpose — so what fails here is the *gate*, which stops firing.
readonly PLUGIN="$ROOT/hydra-plugin"
readonly MANIFEST="$PLUGIN/.claude-plugin/plugin.json"
readonly HOOKS="$PLUGIN/hooks/hooks.json"
readonly SKILL="$PLUGIN/skills/hydra/SKILL.md"

echo "-- §6's three files and nothing else"
same "$(cd "$PLUGIN" && find . -type f | sort | tr '\n' ' ')" \
  "./.claude-plugin/plugin.json ./hooks/hooks.json ./skills/hydra/SKILL.md " \
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

echo "+ jq . hooks/hooks.json"
# Claude Code parses this file as `{description?, hooks}` and takes `.hooks`, so
# an unwrapped event map is discarded silently and the plugin does nothing at all.
jq -e 'has("hooks") and (.hooks | type == "object")' "$HOOKS" >/dev/null \
  || fail "hooks.json needs the plugin wrapper: {\"hooks\": {...}}"
jq -e '[.hooks | keys[]] == ["PostToolUse", "SessionStart", "Stop"]' "$HOOKS" >/dev/null \
  || fail "hooks.json should wire exactly §6's three events"
jq -e '[.hooks[][]] | length == 3' "$HOOKS" >/dev/null \
  || fail "one matcher row per event"
jq -e 'all(.hooks[][].hooks[]; .type == "command" and (.command | startswith("hydra hook ")))' \
  "$HOOKS" >/dev/null || fail "every hook shells straight to \`hydra hook <event>\` (§9)"
# Below the default and a hook that has work to do gets killed mid-answer.
jq -e 'all(.hooks[][].hooks[]; has("timeout") == false)' "$HOOKS" >/dev/null \
  || fail "no timeout: the default is the contract"
echo "-- one SessionStart row covers both of §6's rows: \`hydra hook session-start\`"
echo "   branches on the payload's \`source\` itself, in tested Rust"
jq -e '.hooks.SessionStart[0].matcher == "startup|resume|compact|clear"' "$HOOKS" >/dev/null \
  || fail "the SessionStart matcher should carry all four sources §6 names"
echo "-- but never \`fork\`: a fork gets a new session_id, so the lease cannot match"
jq -e '.hooks.SessionStart[0].matcher | contains("fork") == false' "$HOOKS" >/dev/null \
  || fail "fork is a source hydra no-ops on"
jq -e '.hooks.PostToolUse[0].matcher == "Bash"' "$HOOKS" >/dev/null \
  || fail "§6 gives PostToolUse a Bash matcher"
jq -e '.hooks.Stop[0] | has("matcher") == false' "$HOOKS" >/dev/null \
  || fail "§6's Stop row has no matcher"

echo "-- and now run each command string as Claude Code would: a shell, a payload"
echo "   on stdin, \`hydra\` resolved off PATH"
mkdir -p "$WORK/bin" "$WORK/plugin-repo"
ln -sf "$BIN" "$WORK/bin/hydra"
readonly PLUGIN_SESSION="plugin-session-1"
(
  cd "$WORK/plugin-repo"
  "$BIN" init >/dev/null
  "$BIN" sprout --question "Does the wiring hold?" --slug wiring >/dev/null
  "$BIN" sprout --question "Is it still wired?" --parent wiring --slug still-wired >/dev/null
  "$BIN" cut wiring --answer "yes, and this is the answer the render shows" >/dev/null
  "$BIN" grill start --session-id "$PLUGIN_SESSION" >/dev/null
) || fail "could not build the scratch store for the plugin's hooks"

# One payload per event, carrying everything §6's gate for that row reads.
hook_payload() {
  case "$1" in
    SessionStart)
      printf '{"session_id":"%s","hook_event_name":"SessionStart","source":"startup"}' \
        "$PLUGIN_SESSION" ;;
    PostToolUse)
      printf '{"session_id":"%s","hook_event_name":"PostToolUse","tool_name":"Bash",' \
        "$PLUGIN_SESSION"
      printf '"tool_input":{"command":"hydra cut wiring --answer x"}}' ;;
    Stop)
      printf '{"session_id":"%s","hook_event_name":"Stop","stop_hook_active":false}' \
        "$PLUGIN_SESSION" ;;
    *) fail "hooks.json declares an event this script has no payload for: $1" ;;
  esac
}

while IFS=$'\t' read -r event command; do
  echo "+ [$event] $command"
  STEP="hooks.json [$event] $command"
  code=0
  hook_payload "$event" | (
    cd "$WORK/plugin-repo"
    export PATH="$WORK/bin:$PATH"
    eval "$command"
  ) >"$OUT" 2>"$ERR" || code=$?
  [ "$code" -eq 0 ] || fail "a hook must exit 0, got $code"
  hook_wellformed
  # The gate. This is what a renamed verb breaks: `hydra hook <unknown>` is a
  # well-formed `{}` and exit 0, so only the gate not firing gives it away.
  case "$event" in
    # `/hydra:hydra`, both halves: Claude Code addresses a plugin's skill
    # `plugin:skill` and does not collapse the case where the names match, and this
    # line is the only place a user is told what to type.
    SessionStart) jqok '.systemMessage | test("^hydra: 1 open head on .plugin-repo. . /hydra:hydra to resume$")' ;;
    PostToolUse) jqok '.systemMessage | startswith("plugin-repo  (1 answered, 1 open)")' ;;
    Stop) jqok '.decision == "block" and (.reason | contains("still-wired"))' ;;
  esac
done < <(jq -r '.hooks | to_entries[] as $e | $e.value[].hooks[]
                | [$e.key, .command] | @tsv' "$HOOKS")

# The `2>/dev/null || true` on each command string, and the only place it is
# written down: §6 ships the binary as a *separate* prerequisite, so a plugin
# installed without it would report `command not found` in every project the user
# opens. Worse on `Stop`, where Claude Code reads exit 2 as *block* whatever is on
# stdout — so a missing binary would become a shell error refusing to end the
# turn. `hydra hook` exits 0 and writes nothing to stderr by design (see
# `HOOKS_ALWAYS_SUCCEED`), so the guard costs an installed hydra nothing.
echo "-- the guard on each command string is for the missing binary, not for a"
echo "   failing one: a plugin installed without it must stay silent rather than"
echo "   report it everywhere, and on Stop a nonzero exit is a refusal to stop"
mkdir -p "$WORK/empty-bin"
# Read out before PATH goes away: `jq` would not be findable afterwards either.
COMMANDS=()
while IFS= read -r command; do
  COMMANDS+=("$command")
done < <(jq -r '.hooks[][].hooks[].command' "$HOOKS")
for command in "${COMMANDS[@]}"; do
  STEP="hooks.json with no hydra on PATH: $command"
  echo "+ PATH=(empty) $command"
  code=0
  (
    cd "$WORK/plugin-repo"
    # Emptying the search path is the whole point: this is the repo where hydra
    # was never installed. Scoped to the subshell.
    # shellcheck disable=SC2123
    PATH="$WORK/empty-bin"
    printf '{}' | eval "$command"
  ) >"$OUT" 2>"$ERR" || code=$?
  [ "$code" -eq 0 ] || fail "expected exit 0 with no binary to run, got $code"
  stderr_empty
  [ ! -s "$OUT" ] || fail "nothing to say, so nothing on stdout"
done

(cd "$WORK/plugin-repo" && "$BIN" grill stop >/dev/null)

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
echo "-- a hook must no-op on a corrupt tree, not report it: exit 0, {} and no stderr"
# One payload that satisfies every gate but the tree, and no `hook_event_name`, so
# each verb reaches its own gate rather than being turned away for the wrong reason.
for source in startup compact; do
  for event in session-start post-tool-use stop; do
    IN="{\"session_id\":\"$SESSION\",\"source\":\"$source\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"hydra tree\"}}"
    run 0 hook "$event"
    hook_wellformed
    noop
  done
done

echo "-- grill start refuses a tree it cannot read, so the lease never names one"
run 1 grill start --session-id another-session
stdout_empty
err_has "malformed JSON"
same "$(jq -r .session_id "$LEASE")" "$SESSION" "a refused start must not take the lease"

echo "-- but grill stop is the kill switch, so it reads no tree and always works"
run 0 grill stop
jqok '.released.session_id == "smoke-session-1"'
[ ! -f "$LEASE" ] || fail "the kill switch must work on a store nothing else will load"

echo
echo "smoke: OK"
