{
  # Stream output visibility (what to show during claude execution)
  output = {
    # Core output types
    responses = true; # Assistant text responses (always recommended)
    tool-names = true; # Show tool names like [Bash], [Read]
    tool-inputs = true; # Show tool input parameters
    tool-results = true; # Show tool execution results
    thinking = true; # Show extended thinking content
    stats = false; # Show final stats (cost, tokens, duration)

    # Truncation limits (0 = no limit)
    max-tool-input = 200; # Max chars for tool input display
    max-tool-result = 500; # Max chars for tool result display

    # Output prefixes (customize the bracket prefixes shown in output)
    prefixes = {
      response = "[response] "; # Prefix for assistant text responses
      tool-result = "[result] "; # Prefix for tool results
      tool-error = "[error] "; # Prefix for tool errors
      thinking-start = "<thinking>\n"; # Opening tag for thinking blocks
      thinking-end = "\n</thinking>"; # Closing tag for thinking blocks
      stats-header = "\n[stats]\n"; # Header before stats output
      stats-line = "  "; # Prefix for each stats line
    };
  };

  # Beads integration
  beads = {
    priority = 2; # Default priority (0=critical, 2=medium, 4=backlog)
    default-type = "task";
  };

  prompts = {
    plan-new = "plan-new.md";
    plan-update = "plan-update.md";
    todo-new = "todo-new.md";
    todo-update = "todo-update.md";
    run = "run.md";
  };

  loop = {
    max-iterations = 3; # per molecule, bounds run ↔ check auto-iteration before escalating via ralph:clarify
    pause-on-failure = true;
    parallel = 1; # Concurrency limit (1 = sequential)
    max-retries = 2; # per bead, retry with PREVIOUS_FAILURE context
    max-reviews = 2; # Max review cycles before stopping
  };

  # Model overrides per phase (null = inherit ANTHROPIC_MODEL)
  model = {
    run = null;
    check = null;
    plan = null;
    todo = null;
    watch = null;
  };

  # Observation settings for ralph watch
  watch = {
    poll-interval = 30; # Seconds between watch poll cycles
    max-issues = 10; # Max beads created before pausing watch
    ignore-patterns = [ ]; # Optional noise filter patterns
  };

  # Hook points for ralph run (FR2)
  # Template variables: {{LABEL}}, {{ISSUE_ID}}, {{STEP_COUNT}}, {{STEP_EXIT_CODE}}
  hooks = {
    pre-loop = "prek run";
    pre-step = "bd dolt commit && bd dolt pull";
    post-step = "prek run";
    post-loop = ''
      bd dolt commit && bd dolt push
      git diff --quiet || { echo "Error: worktree is dirty; commit or stash before pushing" >&2; exit 1; }
    '';
  };

  # Hook failure handling (FR5): block | warn | skip
  hooks-on-failure = "block";

  history = {
    enabled = true;
    max-snapshots = 50;
  };

  failure-patterns = [
    {
      pattern = "error:";
      action = "log";
    } # log | pause | notify
    {
      pattern = "FAILED";
      action = "pause";
    }
    {
      pattern = "Exception";
      action = "pause";
    }
    {
      pattern = "BLOCKED:";
      action = "pause";
    }
    {
      pattern = "panic:";
      action = "pause";
    }
  ];

  exit-signals = {
    complete = "RALPH_COMPLETE";
    blocked = "RALPH_BLOCKED:";
    clarify = "RALPH_CLARIFY:";
  };
}
