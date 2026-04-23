#!/usr/bin/env bash
set -euo pipefail

RALPH_DIR="${RALPH_DIR:-.wrapix/ralph}"
COMMAND="${1:-help}"
if [ "$#" -gt 0 ]; then
  shift
fi

case "$COMMAND" in
  check)  exec ralph-check  "$@" ;;
  init)   exec ralph-init   "$@" ;;
  logs)   exec ralph-logs   "$@" ;;
  msg)    exec ralph-msg    "$@" ;;
  plan)   exec ralph-plan   "$@" ;;
  run)    exec ralph-run    "$@" ;;
  spec)   exec ralph-spec   "$@" ;;
  status) exec ralph-status "$@" ;;
  sync)   exec ralph-sync   "$@" ;;
  todo)   exec ralph-todo   "$@" ;;
  tune)   exec ralph-tune   "$@" ;;
  use)    exec ralph-use    "$@" ;;
  watch)  exec ralph-watch  "$@" ;;

  edit)
      # Get current label from state/current, then spec_path from state/<label>.json
      CURRENT_POINTER="$RALPH_DIR/state/current"
      if [ ! -f "$CURRENT_POINTER" ]; then
          echo "No active feature. Run 'ralph plan <label>' first."
          exit 1
      fi
      LABEL=$(<"$CURRENT_POINTER")
      LABEL="${LABEL#"${LABEL%%[![:space:]]*}"}"
      LABEL="${LABEL%"${LABEL##*[![:space:]]}"}"
      if [ -z "$LABEL" ]; then
          echo "No label in state/current. Run 'ralph plan <label>' first."
          exit 1
      fi

      STATE_FILE="$RALPH_DIR/state/${LABEL}.json"
      if [ ! -f "$STATE_FILE" ]; then
          echo "No state file for '$LABEL'. Run 'ralph plan $LABEL' first."
          exit 1
      fi
      SPEC_FILE=$(jq -r '.spec_path // empty' "$STATE_FILE")
      if [ -z "$SPEC_FILE" ] || [ ! -f "$SPEC_FILE" ]; then
          echo "Spec file not found: ${SPEC_FILE:-<not set>}"
          echo "Run 'ralph plan $LABEL' to create it."
          exit 1
      fi
      exec "${EDITOR:-vi}" "$SPEC_FILE"
      ;;

  help|--help|-h)
    echo "Usage: ralph <command> [args]"
    echo ""
    echo "Spec-Driven Workflow Commands:"
    echo "  plan            Spec interview and creation:"
    echo "    -n <label>      New spec in specs/"
    echo "    -h <label>      New hidden spec in state/"
    echo "    -u <spec>       Update existing spec (-uh for hidden)"
    echo "  todo            Convert spec to beads issues"
    echo "  run [feature]   Execute work items for a feature"
    echo "    --once/-1       Execute single issue then exit"
    echo "    -c/--check      Auto-review after molecule completes"
    echo "    -p/--parallel N Concurrent workers (default: 1)"
    echo "  spec            Query spec annotations"
    echo "    --verbose       Show per-criterion detail"
    echo "    --verify        Run [verify] shell tests"
    echo "    --judge         Run [judge] LLM evaluations"
    echo "    --all           Run both verify and judge"
    echo "  status          Show current workflow state"
    echo "  use <name>      Switch active workflow"
    echo ""
    echo "Template Commands:"
    echo "  check           Post-loop review (default) or template validation (-t)"
    echo "    -s, --spec <label>  Review a named spec (default: active spec)"
    echo "    -t, --templates     Validate templates (no Claude invocation)"
    echo "  sync            Update local templates from packaged (backs up customizations)"
    echo "    --diff [name]   Show local template changes vs packaged"
    echo "  tune            AI-assisted template editing (interactive or via diff)"
    echo ""
    echo "Observation Commands:"
    echo "  watch           Monitor running services and create beads for issues"
    echo "    -s <label>      Spec label to monitor (required)"
    echo "    --panes <p>     Tmux panes to observe"
    echo ""
    echo "Communication Commands:"
    echo "  msg             Respond to agent questions (async human communication)"
    echo "    -s <label>      Filter by spec"
    echo "    -i <id>         Target specific question"
    echo "    -i <id> \"ans\"   Reply to a question"
    echo "    -i <id> -d      Dismiss without answering"
    echo ""
    echo "Utility Commands:"
    echo "  init            Bootstrap a new wrapix project in cwd (host-side)"
    echo "  logs [N]        View recent work logs (default 5)"
    echo "  edit            Edit current spec file"
    echo ""
    echo "Environment:"
    echo "  RALPH_DIR    Working directory (default: .wrapix/ralph)"
    echo "  RALPH_DEBUG  Enable debug output (set to 1)"
    ;;

  *)
    echo "Unknown command: $COMMAND"
    echo "Run 'ralph help' for usage"
    exit 1
    ;;
esac
