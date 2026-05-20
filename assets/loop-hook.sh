#!/bin/bash
#
# loop-hook.sh — Grove Ralph-loop Stop hook.
#
# Adapted from the official Anthropic ralph-loop plugin's hooks/stop-hook.sh.
# Behavior matches our freqtrade_tecolote port:
#
#   * State file is $GROVE_AGENT_DIR/loop.md (not .claude/ralph-loop.local.md).
#     If GROVE_AGENT_DIR is unset or $GROVE_AGENT_DIR/loop.md is absent, the hook
#     exits silently — so the main workspace, integration worktrees, and any
#     non-agentic session are unaffected. GROVE_AGENT_DIR is exported by
#     `grove spawn` when launching the tmux/claude session.
#
#   * Honors an `active:` flag in loop.md frontmatter — `active: false` parks a
#     loop (e.g. roadblocked) without removing loop.md.
#
#   * On completion (max iterations reached or <promise> matched), the hook flips
#     `active: true` -> `active: false` instead of deleting loop.md, so the
#     iteration count and history survive for inspection.
#
# Hook contract (Claude Code):
#   stdin: JSON with .session_id, .transcript_path
#   stdout (when blocking): {"decision":"block","reason":..., "systemMessage":...}
#   exit 0 with no body = allow the stop.

set -euo pipefail

# ---- state-file resolution ------------------------------------------------

if [[ -z "${GROVE_AGENT_DIR:-}" ]]; then
  exit 0
fi
GROVE_STATE_FILE="$GROVE_AGENT_DIR/loop.md"
if [[ ! -f "$GROVE_STATE_FILE" ]]; then
  exit 0
fi

# Read hook input from stdin.
HOOK_INPUT=$(cat)

flip_inactive() {
  local tmp
  tmp="${GROVE_STATE_FILE}.tmp.$$"
  sed 's/^active: *true$/active: false/' "$GROVE_STATE_FILE" > "$tmp" && mv "$tmp" "$GROVE_STATE_FILE"
}

# ---- frontmatter parsing --------------------------------------------------

FRONTMATTER=$(sed -n '/^---$/,/^---$/{ /^---$/d; p; }' "$GROVE_STATE_FILE")
ACTIVE=$(echo "$FRONTMATTER"          | grep '^active:'             | sed 's/active: *//'             | tr -d '"' || true)
ITERATION=$(echo "$FRONTMATTER"       | grep '^iteration:'          | sed 's/iteration: *//'          || true)
MAX_ITERATIONS=$(echo "$FRONTMATTER"  | grep '^max_iterations:'     | sed 's/max_iterations: *//'     || true)
COMPLETION_PROMISE=$(echo "$FRONTMATTER" | grep '^completion_promise:' | sed 's/completion_promise: *//' | sed 's/^"\(.*\)"$/\1/' || true)
STATE_SESSION=$(echo "$FRONTMATTER"   | grep '^session_id:'         | sed 's/session_id: *//'         | tr -d '"' || true)
HOOK_SESSION=$(echo "$HOOK_INPUT" | jq -r '.session_id // ""')

# Honor active:false — loop is parked, do nothing.
if [[ "$ACTIVE" != "true" ]]; then
  exit 0
fi

# Session isolation: a different session must not drive this loop.
if [[ -n "$STATE_SESSION" ]] && [[ "$STATE_SESSION" != "$HOOK_SESSION" ]]; then
  exit 0
fi

# Validate numeric fields.
if [[ ! "$ITERATION" =~ ^[0-9]+$ ]]; then
  echo "⚠️  Grove loop: 'iteration' is not numeric (got: '$ITERATION') in $GROVE_STATE_FILE — stopping." >&2
  flip_inactive
  exit 0
fi
if [[ ! "$MAX_ITERATIONS" =~ ^[0-9]+$ ]]; then
  echo "⚠️  Grove loop: 'max_iterations' is not numeric (got: '$MAX_ITERATIONS') in $GROVE_STATE_FILE — stopping." >&2
  flip_inactive
  exit 0
fi

# ---- completion: max iterations -------------------------------------------

if [[ $MAX_ITERATIONS -gt 0 ]] && [[ $ITERATION -ge $MAX_ITERATIONS ]]; then
  echo "🛑 Grove loop: max iterations ($MAX_ITERATIONS) reached for ${GROVE_AGENT_DIR##*/}; parking (active:false)." >&2
  flip_inactive
  exit 0
fi

# ---- completion: <promise> tag in last assistant text ---------------------

TRANSCRIPT_PATH=$(echo "$HOOK_INPUT" | jq -r '.transcript_path')
if [[ ! -f "$TRANSCRIPT_PATH" ]]; then
  echo "⚠️  Grove loop: transcript not found at $TRANSCRIPT_PATH — stopping." >&2
  flip_inactive
  exit 0
fi
if ! grep -q '"role":"assistant"' "$TRANSCRIPT_PATH"; then
  echo "⚠️  Grove loop: no assistant messages in transcript — stopping." >&2
  flip_inactive
  exit 0
fi

LAST_LINES=$(grep '"role":"assistant"' "$TRANSCRIPT_PATH" | tail -n 100)
set +e
LAST_OUTPUT=$(echo "$LAST_LINES" | jq -rs '
  map(.message.content[]? | select(.type == "text") | .text) | last // ""
' 2>&1)
JQ_EXIT=$?
set -e
if [[ $JQ_EXIT -ne 0 ]]; then
  echo "⚠️  Grove loop: failed to parse transcript ($LAST_OUTPUT) — stopping." >&2
  flip_inactive
  exit 0
fi

if [[ "$COMPLETION_PROMISE" != "null" ]] && [[ -n "$COMPLETION_PROMISE" ]]; then
  PROMISE_TEXT=$(echo "$LAST_OUTPUT" | perl -0777 -pe \
    's/.*?<promise>(.*?)<\/promise>.*/$1/s; s/^\s+|\s+$//g; s/\s+/ /g' 2>/dev/null || echo "")
  if [[ -n "$PROMISE_TEXT" ]] && [[ "$PROMISE_TEXT" = "$COMPLETION_PROMISE" ]]; then
    echo "✅ Grove loop: <promise>$COMPLETION_PROMISE</promise> matched — parking (active:false)." >&2
    flip_inactive
    exit 0
  fi
fi

# ---- continue: bump iteration and re-feed the prompt ----------------------

NEXT_ITERATION=$((ITERATION + 1))

PROMPT_TEXT=$(awk '/^---$/{i++; next} i>=2' "$GROVE_STATE_FILE")
if [[ -z "$PROMPT_TEXT" ]]; then
  echo "⚠️  Grove loop: empty prompt body in $GROVE_STATE_FILE — stopping." >&2
  flip_inactive
  exit 0
fi

TMP="${GROVE_STATE_FILE}.tmp.$$"
sed "s/^iteration: .*/iteration: $NEXT_ITERATION/" "$GROVE_STATE_FILE" > "$TMP"
mv "$TMP" "$GROVE_STATE_FILE"

if [[ "$COMPLETION_PROMISE" != "null" ]] && [[ -n "$COMPLETION_PROMISE" ]]; then
  SYSTEM_MSG="🔄 Grove iteration $NEXT_ITERATION (${GROVE_AGENT_DIR##*/}) | To stop: emit <promise>$COMPLETION_PROMISE</promise> — only when genuinely true."
else
  SYSTEM_MSG="🔄 Grove iteration $NEXT_ITERATION (${GROVE_AGENT_DIR##*/}) | No completion_promise set — runs until max_iterations or active:false."
fi

jq -n \
  --arg prompt "$PROMPT_TEXT" \
  --arg msg "$SYSTEM_MSG" \
  '{ "decision": "block", "reason": $prompt, "systemMessage": $msg }'

exit 0
