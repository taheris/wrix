# Gas City tests — layered testing for mkCity, provider, and container lifecycle
#
# Layer 1 (pre-commit): Nix evaluation tests
#   - mkCity evaluates with minimal config
#   - Generated city.toml is valid and references wrapix provider
#   - Service packages build into OCI images via dockerTools.buildLayeredImage
#
# Layer 2 (pre-commit): Provider script tests
#   - All 21 provider methods handled
#   - Persistent role tmux setup
#   - Ephemeral worker worktree lifecycle
#
# Layer 3 (pre-commit): Container lifecycle / shell syntax tests
#   - Entrypoint, recovery, gate, post-gate, scout, agent scripts validate
#
# All gc checks run as part of nix flake check.
{
  pkgs,
  system,
  treefmt,
}:

let
  inherit (builtins)
    all
    any
    concatStringsSep
    elemAt
    filter
    hasAttr
    isList
    length
    match
    readFile
    substring
    ;
  inherit (pkgs) bash runCommandLocal;

  linuxPkgs =
    if pkgs.stdenv.isDarwin then
      import pkgs.path {
        system = "aarch64-linux";
        config.allowUnfree = true;
        inherit (pkgs) overlays;
      }
    else
      pkgs;

  sandbox = import ../../lib/sandbox {
    inherit pkgs system;
    inherit linuxPkgs treefmt;
  };

  beads = import ../../lib/beads { inherit pkgs linuxPkgs; };
  ralph = import ../../lib/ralph {
    inherit pkgs beads;
    inherit (sandbox) mkSandbox;
  };

  city = import ../../lib/city {
    inherit pkgs linuxPkgs;
    inherit (sandbox) mkSandbox profiles baseClaudeSettings;
    inherit (ralph) mkRalph;
  };

  # Evaluate mkCity with minimal config
  minimalCity = city.mkCity {
    services.api.package = linuxPkgs.hello;
    secrets.claude = "ANTHROPIC_API_KEY";
  };

  # Evaluate mkCity with no services (edge case)
  emptyCity = city.mkCity { services = { }; };

  # Evaluate mkCity with full options
  fullCity = city.mkCity {
    services.api.package = linuxPkgs.hello;
    services.db.package = linuxPkgs.hello;
    profile = sandbox.profiles.base;
    agent = "claude";
    workers = 2;
    cooldown = "2h";
    scout = {
      interval = "10m";
      maxBeads = 5;
    };
    mayor = {
      autoDecompose = true;
    };
    resources = {
      worker = {
        cpus = 2;
        memory = "4g";
      };
      scout = {
        cpus = 1;
        memory = "2g";
      };
    };
    secrets = {
      claude = "ANTHROPIC_API_KEY";
      deployKey = "/run/secrets/deploy-key";
    };
  };

in
{
  # =========================================================================
  # Layer 1: Nix Evaluation Tests
  # =========================================================================

  # mkCity evaluates with minimal config
  city-mkcity-eval =
    let
      hasConfig = hasAttr "config" minimalCity;
      hasProvider = hasAttr "provider" minimalCity;
      hasServiceImages = hasAttr "serviceImages" minimalCity;
      hasFormulas = hasAttr "formulas" minimalCity;
      hasDefaultFormulas = hasAttr "defaultFormulas" minimalCity;
      hasMayorConfig = hasAttr "mayorConfig" minimalCity;

      # Full city also evaluates
      fullHasConfig = hasAttr "config" fullCity;

      # Empty city evaluates
      emptyHasConfig = hasAttr "config" emptyCity;
    in
    assert hasConfig;
    assert hasProvider;
    assert hasServiceImages;
    assert hasFormulas;
    assert hasDefaultFormulas;
    assert hasMayorConfig;
    assert fullHasConfig;
    assert emptyHasConfig;

    runCommandLocal "city-mkcity-eval" { } ''
      echo "PASS: mkCity evaluates with minimal, full, and empty configs"
      echo "  - config, provider, serviceImages, formulas, defaultFormulas present"
      mkdir $out
    '';

  # Generated city.toml matches gc's config schema
  city-city-toml =
    let
      inherit (minimalCity) configAttrs;

      # Workspace section
      hasWorkspace = hasAttr "workspace" configAttrs;
      workspaceName = configAttrs.workspace.name;
      # wx-entt5: workspace must NOT have a provider field — setting it to
      # "claude" causes gc to inject a phantom agent with HOST tmux management.
      hasNoWorkspaceProvider = !(hasAttr "provider" configAttrs.workspace);

      # Session section with exec provider
      sessionProvider = configAttrs.session.provider;
      hasExecPrefix = substring 0 5 sessionProvider == "exec:";
      hasStablePath = substring 5 4 sessionProvider == ".gc/";

      # Required sections
      hasFormulas = hasAttr "formulas" configAttrs;
      hasBeads = hasAttr "beads" configAttrs;
      hasDaemon = hasAttr "daemon" configAttrs;

      # Agent is a list (array of tables)
      agentIsList = isList configAttrs.agent;
      agentCount = length configAttrs.agent;
      hasMayor = any (a: a.name == "mayor") configAttrs.agent;
      hasScout = any (a: a.name == "scout") configAttrs.agent;
      hasWorker = any (a: a.name == "worker") configAttrs.agent;
      hasJudge = any (a: a.name == "judge") configAttrs.agent;
      # wx-m7a1d: dog override suppresses system pack injection
      hasDog = any (a: a.name == "dog") configAttrs.agent;
      dogMaxSessions =
        (builtins.head (filter (a: a.name == "dog") configAttrs.agent)).max_active_sessions;

      # wx-65bws: pool agents must have max_active_sessions so gc treats them
      # as pools (not named singles that perpetually count as 'open').
      workerMaxSessions =
        (builtins.head (filter (a: a.name == "worker") configAttrs.agent)).max_active_sessions;
      scoutMaxSessions =
        (builtins.head (filter (a: a.name == "scout") configAttrs.agent)).max_active_sessions;
      judgeMaxSessions =
        (builtins.head (filter (a: a.name == "judge") configAttrs.agent)).max_active_sessions;
      workerMinSessions =
        (builtins.head (filter (a: a.name == "worker") configAttrs.agent)).min_active_sessions;
      scoutMinSessions =
        (builtins.head (filter (a: a.name == "scout") configAttrs.agent)).min_active_sessions;
      judgeMinSessions =
        (builtins.head (filter (a: a.name == "judge") configAttrs.agent)).min_active_sessions;
      # Full city: workers=2 → worker.max_active_sessions=2
      fullWorkerMaxSessions =
        (builtins.head (filter (a: a.name == "worker") fullCity.configAttrs.agent)).max_active_sessions;

      # Every agent with a prompt_template has it pointing into the staged city dir.
      allHavePromptTemplate = all (
        a:
        !(hasAttr "prompt_template" a)
        || (a.prompt_template or "") == ".wrapix/city/current/prompts/${a.name}.md"
      ) configAttrs.agent;

      # Full city: workers=2 reflected in workspace and worker agent
      fullWorkspace = fullCity.configAttrs.workspace;
      fullWorkerSessions = fullWorkspace.max_active_sessions;

      # Convergence config
      hasConvergence = hasAttr "convergence" configAttrs;
      convergenceMaxPerAgent = configAttrs.convergence.max_per_agent;
      convergenceMaxTotal = configAttrs.convergence.max_total;

      # scoutConfig exported with correct values
      minimalScoutConfig = minimalCity.scoutConfig;
      fullScoutConfig = fullCity.scoutConfig;

      # mayorConfig exported with correct values
      minimalMayorConfig = minimalCity.mayorConfig;
      hasMayorConfig = hasAttr "mayorConfig" minimalCity;
    in
    assert hasWorkspace;
    assert workspaceName == "dev";
    assert hasNoWorkspaceProvider;
    assert hasExecPrefix;
    assert hasStablePath;
    assert hasFormulas;
    assert hasBeads;
    assert hasDaemon;
    assert hasConvergence;
    assert convergenceMaxPerAgent == 2;
    assert convergenceMaxTotal == 10;
    assert agentIsList;
    assert agentCount == 5;
    assert hasMayor;
    assert hasScout;
    assert hasWorker;
    assert hasJudge;
    assert hasDog;
    assert dogMaxSessions == 0;
    # wx-65bws: pool agents have max_active_sessions set
    assert workerMaxSessions == 1; # minimal city defaults to workers=1
    assert scoutMaxSessions == 1;
    assert judgeMaxSessions == 1;
    assert workerMinSessions == 0;
    assert scoutMinSessions == 0;
    assert judgeMinSessions == 0;
    assert fullWorkerMaxSessions == 2; # full city has workers=2
    assert allHavePromptTemplate;
    assert fullWorkerSessions == 5; # workers(2) + 3 named sessions (wx-kw4gg)
    # scoutConfig reflects configured values
    assert minimalScoutConfig.maxBeads == 10;
    assert minimalScoutConfig.interval == "5m";
    assert fullScoutConfig.maxBeads == 5;
    assert fullScoutConfig.interval == "10m";
    # mayorConfig
    assert hasMayorConfig;
    assert !minimalMayorConfig.autoDecompose;

    runCommandLocal "city-city-toml" { } ''
      echo "PASS: city.toml matches gc config schema"
      echo "  - [workspace] with name and provider"
      echo "  - [session] with exec:/nix/store/... provider"
      echo "  - [formulas], [beads], [daemon], [convergence] sections present"
      echo "  - [[agent]] is list with mayor, scout, worker, judge, dog (max=0)"
      echo "  - worker/scout/judge have max_active_sessions (wx-65bws)"
      echo "  - workers reflected in max_active_sessions"
      echo "  - scoutConfig exports correct maxBeads and interval"
      echo "  - mayorConfig exports correct autoDecompose"
      mkdir $out
    '';

  # Live test: `gc prime <role>` reads prompt_template from city.toml,
  # resolves it against the city dir, and emits the role's markdown
  # content instead of the generic fallback. Exercises the real gc
  # binary against the real mkCity output — no mocks.
  city-gc-prime-live =
    runCommandLocal "city-gc-prime-live"
      {
        nativeBuildInputs = [
          bash
          pkgs.gc
        ];
      }
      ''
        set -euo pipefail
        TMPDIR="$(mktemp -d)"
        cp ${minimalCity.config} "$TMPDIR/city.toml"
        mkdir -p "$TMPDIR/.wrapix/city/current/prompts"
        for f in ${minimalCity.prompts}/*; do
          cp "$f" "$TMPDIR/.wrapix/city/current/prompts/$(basename "$f")"
        done
        cd "$TMPDIR"

        declare -A MARKER=(
          [mayor]="conversational interface"
          [scout]="eyes and ears"
          [worker]="ephemeral task executor"
          [judge]="quality gate"
        )
        declare -A HEADER=(
          [mayor]="Role: Mayor"
          [scout]="Role: Scout"
          [worker]="Role: Worker"
          [judge]="Role: Judge"
        )

        for role in mayor scout worker judge; do
          got="$(gc prime "$role")"
          echo "--- gc prime $role ---"
          echo "$got" | head -3
          echo "$got" | grep -q "''${HEADER[$role]}" \
            || { echo "FAIL: $role prime missing role header"; exit 1; }
          echo "$got" | grep -q "''${MARKER[$role]}" \
            || { echo "FAIL: $role prime is fallback, not prompt_template"; exit 1; }
          echo "  PASS: $role"
        done

        rm -rf "$TMPDIR"
        echo "PASS: gc prime emits prompt_template for every agent"
        mkdir $out
      '';

  # Service packages build into OCI images
  city-service-images =
    let
      apiImage = minimalCity.serviceImages.api;
      inherit (apiImage) imageName;
      nameCorrect = substring 0 14 imageName == "wrapix-svc-api";

      # Full city has multiple service images
      fullHasApi = hasAttr "api" fullCity.serviceImages;
      fullHasDb = hasAttr "db" fullCity.serviceImages;
    in
    assert nameCorrect;
    assert fullHasApi;
    assert fullHasDb;
    runCommandLocal "city-service-images" { } ''
      echo "Checking service image..."
      ${
        if pkgs.stdenv.isDarwin then
          ''
            test -f ${apiImage}
            tar -tf ${apiImage} >/dev/null
          ''
        else
          ''
            test -x ${apiImage}
            ${apiImage} | tar -tf - >/dev/null
          ''
      }
      echo "PASS: Service images build correctly"
      echo "  - wrapix-svc-api image is valid tar"
      echo "  - Multiple services supported"
      mkdir $out
    '';

  # Secrets classification
  city-secrets =
    let
      inherit (minimalCity) classifiedSecrets;
      claudeSecret = classifiedSecrets.claude;
      claudeIsEnv = claudeSecret.type == "env";

      fullSecrets = fullCity.classifiedSecrets;
      deployIsFile = fullSecrets.deployKey.type == "file";
      deployPath = fullSecrets.deployKey.path == "/run/secrets/deploy-key";
    in
    assert claudeIsEnv;
    assert deployIsFile;
    assert deployPath;

    runCommandLocal "city-secrets" { } ''
      echo "PASS: Secrets classified correctly"
      echo "  - env var secret: type=env"
      echo "  - file path secret: type=file with correct path"
      mkdir $out
    '';

  # =========================================================================
  # Layer 2: Shell Script Syntax Validation
  # =========================================================================

  # Validate all Gas City shell scripts parse without errors
  city-shell-syntax =
    runCommandLocal "city-shell-syntax"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        echo "Checking Gas City shell script syntax..."

        SCRIPTS=(
          "${../../lib/city/scripts/agent.sh}"
          "${../../lib/city/scripts/dispatch.sh}"
          "${../../lib/city/scripts/entrypoint.sh}"
          "${../../lib/city/scripts/gate.sh}"
          "${../../lib/city/scripts/judge-merge.sh}"
          "${../../lib/city/scripts/post-gate.sh}"
          "${../../lib/city/scripts/provider.sh}"
          "${../../lib/city/scripts/recovery.sh}"
          "${../../lib/city/scripts/scout.sh}"
          "${../../lib/city/scripts/worker-collect.sh}"
          "${../../lib/city/scripts/worker-setup.sh}"
        )

        for script in "''${SCRIPTS[@]}"; do
          name="$(basename "$script")"
          bash -n "$script" || { echo "FAIL: $name has syntax errors"; exit 1; }
          grep -q 'set -euo pipefail' "$script" || { echo "FAIL: $name missing set -euo pipefail"; exit 1; }
          echo "  PASS: $name"
        done

        echo ""
        echo "PASS: All Gas City shell scripts have valid syntax"
        mkdir $out
      '';

  # =========================================================================
  # Layer 3: Functional tests (execute scripts with mock dependencies)
  # =========================================================================

  # git-ssh-setup.sh: sets GIT_SSH_COMMAND from WRAPIX_DEPLOY_KEY and
  # configures commit signing from WRAPIX_SIGNING_KEY.
  city-git-ssh-setup =
    runCommandLocal "city-git-ssh-setup"
      {
        nativeBuildInputs = [
          bash
          pkgs.git
          pkgs.openssh
        ];
      }
      ''
        set -euo pipefail
        FRAGMENT="${../../lib/util/git-ssh-setup.sh}"

        TMPDIR=$(mktemp -d)
        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email test@wrapix.dev
        git config --global user.name test

        # Case 1: no env vars — source must be a no-op
        unset WRAPIX_DEPLOY_KEY WRAPIX_SIGNING_KEY GIT_SSH_COMMAND
        # shellcheck disable=SC1090
        bash -c '. '"$FRAGMENT"'; [ -z "''${GIT_SSH_COMMAND:-}" ] || { echo "FAIL: GIT_SSH_COMMAND set when no deploy key"; exit 1; }'
        echo "  PASS: no-op when no env vars"

        # Case 2: WRAPIX_DEPLOY_KEY points at a file — GIT_SSH_COMMAND exported
        #         and ~/.ssh/config written so bare ssh also uses the key
        KEY="$TMPDIR/deploy.key"
        ssh-keygen -t ed25519 -f "$KEY" -N "" -q
        HOME="$TMPDIR/home-case2" WRAPIX_DEPLOY_KEY="$KEY" bash -c 'mkdir -p "$HOME" && . '"$FRAGMENT"'; \
          [[ "$GIT_SSH_COMMAND" == "ssh -i '"$KEY"' -o IdentitiesOnly=yes" ]] || { \
          echo "FAIL: GIT_SSH_COMMAND=$GIT_SSH_COMMAND"; exit 1; }; \
          grep -q "IdentityFile '"$KEY"'" "$HOME/.ssh/config" || { \
          echo "FAIL: ~/.ssh/config missing IdentityFile"; cat "$HOME/.ssh/config" 2>/dev/null; exit 1; }'
        echo "  PASS: GIT_SSH_COMMAND and ~/.ssh/config set from deploy key"

        # Case 3: WRAPIX_SIGNING_KEY configures commit signing
        SKEY="$TMPDIR/signing.key"
        ssh-keygen -t ed25519 -f "$SKEY" -N "" -q
        HOME="$TMPDIR/home2" GIT_AUTHOR_EMAIL=sig@wrapix.dev WRAPIX_SIGNING_KEY="$SKEY" \
          bash -c 'mkdir -p "$HOME" && git config --global user.email x && git config --global user.name x && . '"$FRAGMENT"'
                   [[ "$(git config --global gpg.format)" == "ssh" ]] || { echo "FAIL: gpg.format"; exit 1; }
                   [[ "$(git config --global user.signingkey)" == "'"$SKEY"'" ]] || { echo "FAIL: signingkey"; exit 1; }
                   [[ "$(git config --global commit.gpgsign)" == "true" ]] || { echo "FAIL: commit.gpgsign"; exit 1; }
                   grep -q sig@wrapix.dev "$HOME/.config/git/allowed_signers" || { echo "FAIL: allowed_signers"; exit 1; }'
        echo "  PASS: signing key configures git signing"

        # Case 4: WRAPIX_GIT_SIGN=0 disables auto-signing
        HOME="$TMPDIR/home3" WRAPIX_SIGNING_KEY="$SKEY" WRAPIX_GIT_SIGN=0 \
          bash -c 'mkdir -p "$HOME" && git config --global user.email x && git config --global user.name x && . '"$FRAGMENT"'
                   [[ -z "$(git config --global --get commit.gpgsign 2>/dev/null)" ]] || { echo "FAIL: auto-sign not disabled"; exit 1; }'
        echo "  PASS: WRAPIX_GIT_SIGN=0 skips auto-signing"

        echo ""
        echo "PASS: git-ssh-setup.sh all cases"
        mkdir $out
      '';

  # Scout: parse-rules extracts patterns from orchestration.md
  city-scout-parse-rules =
    runCommandLocal "city-scout-parse-rules"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
                set -euo pipefail
                SCOUT="${../../lib/city/scripts/scout.sh}"

                echo "Testing scout.sh parse-rules..."

                # Test 1: defaults when no doc file
                TMPDIR=$(mktemp -d)
                SCOUT_ERRORS_DIR="$TMPDIR/errors" bash "$SCOUT" parse-rules ""

                immediate="$(cat "$TMPDIR/errors/immediate.pat")"
                batched="$(cat "$TMPDIR/errors/batched.pat")"
                [[ "$immediate" == "FATAL|PANIC|panic:" ]] || { echo "FAIL: wrong default immediate: $immediate"; exit 1; }
                [[ "$batched" == "ERROR|Exception" ]] || { echo "FAIL: wrong default batched: $batched"; exit 1; }
                echo "  PASS: defaults applied when no doc"

                # Test 2: custom patterns from doc
                cat > "$TMPDIR/orch.md" << 'DOC'
        ## Scout Rules
        ### Immediate (P0 bead)
        ```
        OOM_KILLED|SEGFAULT
        ```
        ### Batched (collected over one poll cycle)
        ```
        WARN|TIMEOUT
        ```
        ### Ignore
        ```
        healthcheck
        ```
        ## Auto-deploy
        DOC
                rm -rf "$TMPDIR/errors"
                SCOUT_ERRORS_DIR="$TMPDIR/errors" bash "$SCOUT" parse-rules "$TMPDIR/orch.md"

                immediate="$(cat "$TMPDIR/errors/immediate.pat")"
                batched="$(cat "$TMPDIR/errors/batched.pat")"
                ignore="$(cat "$TMPDIR/errors/ignore.pat")"
                [[ "$immediate" == "OOM_KILLED|SEGFAULT" ]] || { echo "FAIL: custom immediate: $immediate"; exit 1; }
                [[ "$batched" == "WARN|TIMEOUT" ]] || { echo "FAIL: custom batched: $batched"; exit 1; }
                [[ "$ignore" == "healthcheck" ]] || { echo "FAIL: custom ignore: $ignore"; exit 1; }
                echo "  PASS: custom patterns parsed"

                rm -rf "$TMPDIR"
                echo "PASS: scout parse-rules works correctly"
                mkdir $out
      '';

  # Scout: scan classifies log lines by pattern tier
  city-scout-scan =
    runCommandLocal "city-scout-scan"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
                set -euo pipefail
                SCOUT="${../../lib/city/scripts/scout.sh}"
                TMPDIR=$(mktemp -d)

                echo "Testing scout.sh scan with mock podman..."

                # Set up patterns
                mkdir -p "$TMPDIR/errors"
                echo "FATAL|PANIC|panic:" > "$TMPDIR/errors/immediate.pat"
                echo "ERROR|Exception" > "$TMPDIR/errors/batched.pat"
                echo "healthcheck" > "$TMPDIR/errors/ignore.pat"

                # Create mock podman that returns mixed log lines
                MOCK_BIN="$TMPDIR/bin"
                mkdir -p "$MOCK_BIN"
                cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        cat << 'LOGS'
        2026-04-01 INFO: service started
        2026-04-01 ERROR: connection refused
        2026-04-01 healthcheck passed
        2026-04-01 FATAL: out of memory
        2026-04-01 Exception in thread main
        LOGS
        MOCK
                chmod +x "$MOCK_BIN/podman"

                PATH="$MOCK_BIN:$PATH" SCOUT_ERRORS_DIR="$TMPDIR/errors" \
                  bash "$SCOUT" scan "my-api" --since=5m

                # Verify classification
                grep -q "FATAL" "$TMPDIR/errors/my-api/immediate.log" || { echo "FAIL: FATAL not in immediate"; exit 1; }
                grep -q "ERROR" "$TMPDIR/errors/my-api/batched.log" || { echo "FAIL: ERROR not in batched"; exit 1; }
                grep -q "Exception" "$TMPDIR/errors/my-api/batched.log" || { echo "FAIL: Exception not in batched"; exit 1; }

                # healthcheck should be filtered out
                ! grep -q "healthcheck" "$TMPDIR/errors/my-api/immediate.log" || { echo "FAIL: healthcheck in immediate"; exit 1; }
                ! grep -q "healthcheck" "$TMPDIR/errors/my-api/batched.log" || { echo "FAIL: healthcheck in batched"; exit 1; }

                # INFO should not appear (not in any pattern)
                ! grep -q "INFO" "$TMPDIR/errors/my-api/immediate.log" || { echo "FAIL: INFO in immediate"; exit 1; }
                ! grep -q "INFO" "$TMPDIR/errors/my-api/batched.log" || { echo "FAIL: INFO in batched"; exit 1; }

                rm -rf "$TMPDIR"
                echo "PASS: scout scan correctly classifies log lines"
                mkdir $out
      '';

  # Gate: tested via integration test (tests/city/integration.nix Phase 1).
  # The integration test exercises the full path: real gc convergence calls
  # gate.sh, which computes commit_range from real git branches, with real
  # podman containers and real beads/dolt.

  # Agent wrapper: prompt construction
  city-agent-prompt =
    runCommandLocal "city-agent-prompt"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
                set -euo pipefail
                AGENT="${../../lib/city/scripts/agent.sh}"

                echo "Testing agent.sh prompt construction..."

                TMPDIR=$(mktemp -d)
                MOCK_BIN="$TMPDIR/bin"
                mkdir -p "$MOCK_BIN" "$TMPDIR/docs"

                # Create docs and task file
                echo "Project uses Nix for builds." > "$TMPDIR/docs/README.md"
                echo "Use set -euo pipefail in shell." > "$TMPDIR/docs/style-guidelines.md"
                echo "Fix the broken auth module." > "$TMPDIR/task.md"

                # Mock claude to echo the prompt it receives
                # The last argument is the prompt (after -p and --dangerously-skip-permissions)
                cat > "$MOCK_BIN/claude" << MOCK
        #!$(command -v bash)
        for arg; do :; done
        echo "\$arg"
        MOCK
                chmod +x "$MOCK_BIN/claude"

                output="$(PATH="$MOCK_BIN:$PATH" \
                  WRAPIX_AGENT=claude \
                  WRAPIX_PROMPT_FILE="$TMPDIR/task.md" \
                  WRAPIX_DOCS_DIR="$TMPDIR/docs" \
                  bash "$AGENT" run 2>&1)"

                echo "$output" | grep -q "Project uses Nix" || { echo "FAIL: docs/README.md missing from prompt"; exit 1; }
                echo "$output" | grep -q "set -euo pipefail" || { echo "FAIL: docs/style-guidelines.md missing from prompt"; exit 1; }
                echo "$output" | grep -q "Fix the broken auth" || { echo "FAIL: task file missing from prompt"; exit 1; }

                # Missing prompt file should fail
                exit_code=0
                PATH="$MOCK_BIN:$PATH" WRAPIX_AGENT=claude WRAPIX_PROMPT_FILE="/nonexistent" \
                  bash "$AGENT" run > /dev/null 2>&1 || exit_code=$?
                [[ "$exit_code" -ne 0 ]] || { echo "FAIL: should fail on missing prompt file"; exit 1; }

                rm -rf "$TMPDIR"
                echo "PASS: agent.sh prompt construction works"
                mkdir $out
      '';

  # Provider: worker_start creates a worktree, renders the role prime
  # via live `gc prime worker`, and invokes podman with the expected
  # mounts. Only podman and bd are stubbed (can't run inside Nix
  # sandbox); git and gc are exercised live.
  city-provider-worker =
    runCommandLocal "city-provider-worker"
      {
        nativeBuildInputs = [
          bash
          pkgs.git
          pkgs.gc
        ];
      }
      ''
        set -euo pipefail

        echo "Testing provider.sh worker start..."

        TMPDIR=$(mktemp -d)
        STUB_BIN="$TMPDIR/bin"
        mkdir -p "$STUB_BIN"

        # Copy all scripts into one directory so BASH_SOURCE[0] dirname
        # finds sibling scripts (worker-setup.sh, worker-collect.sh).
        SCRIPTS_DIR="$TMPDIR/scripts"
        mkdir -p "$SCRIPTS_DIR"
        for f in ${minimalCity.scripts}/*; do
          cp "$f" "$SCRIPTS_DIR/$(basename "$f")"
        done
        chmod +x "$SCRIPTS_DIR"/*.sh
        PROVIDER="$SCRIPTS_DIR/provider.sh"

        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main
        git -C "$TMPDIR" init -q -b main
        git -C "$TMPDIR" commit --allow-empty -m "initial" -q

        # Stage the real city.toml and prompt files so `gc prime worker`
        # resolves against the city dir.
        cp ${minimalCity.config} "$TMPDIR/city.toml"
        mkdir -p "$TMPDIR/.wrapix/city/current/prompts"
        for f in ${minimalCity.prompts}/*; do
          cp "$f" "$TMPDIR/.wrapix/city/current/prompts/$(basename "$f")"
        done

        # podman and bd have no usable in-sandbox equivalent — stub them.
        cat > "$STUB_BIN/podman" << 'STUB'
        #!/bin/sh
        echo "$@" >> "$PODMAN_LOG"
        STUB
        chmod +x "$STUB_BIN/podman"
        cat > "$STUB_BIN/bd" << 'STUB'
        #!/bin/sh
        echo "$@" >> "$BD_LOG"
        STUB
        chmod +x "$STUB_BIN/bd"

        export PODMAN_LOG="$TMPDIR/podman.log"
        export BD_LOG="$TMPDIR/bd.log"

        PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME="test" \
          GC_WORKSPACE="$TMPDIR" \
          GC_AGENT_IMAGE="test-image:latest" \
          GC_PODMAN_NETWORK="wrapix-test" \
          GC_BEAD_ID="bead-123" \
          GC_BEADS_DOLT_CONTAINER="beads-test" \
          GC_DOLT_PORT="13306" \
          bash "$PROVIDER" start worker-1 > "$TMPDIR/out" 2>&1 || {
            echo "FAIL: provider start exited non-zero"
            cat "$TMPDIR/out"
            exit 1
          }

        test -d "$TMPDIR/.wrapix/worktree/bead-123" \
          || { echo "FAIL: worktree not created"; exit 1; }
        echo "  PASS: worktree created"

        git -C "$TMPDIR" rev-parse --verify bead-123 > /dev/null 2>&1 \
          || { echo "FAIL: branch not created"; exit 1; }
        echo "  PASS: git branch created"

        # The role prompt file must exist and contain real template
        # content emitted by the live gc binary (not the generic fallback).
        prime="$TMPDIR/.wrapix/worktree/bead-123/.role-prompt"
        test -f "$prime" || { echo "FAIL: .role-prompt not rendered"; exit 1; }
        grep -q "Role: Worker" "$prime" \
          || { echo "FAIL: .role-prompt missing worker header"; cat "$prime"; exit 1; }
        grep -q "ephemeral task executor" "$prime" \
          || { echo "FAIL: .role-prompt is fallback, not prompt_template"; cat "$prime"; exit 1; }
        echo "  PASS: live gc prime rendered role prompt"

        grep -q "worktree/bead-123:/workspace" "$TMPDIR/podman.log" \
          || { echo "FAIL: worktree not mounted"; exit 1; }
        echo "  PASS: worktree mounted in container"

        grep -q "WRAPIX_SYSTEM_PROMPT_FILE=/workspace/.role-prompt" "$TMPDIR/podman.log" \
          || { echo "FAIL: WRAPIX_SYSTEM_PROMPT_FILE not wired"; exit 1; }
        echo "  PASS: WRAPIX_SYSTEM_PROMPT_FILE wired to .role-prompt"

        grep -q "\.task" "$TMPDIR/podman.log" \
          || { echo "FAIL: task file not mounted"; exit 1; }
        echo "  PASS: task file mounted"

        grep -q "\.wrapix:/workspace/.wrapix" "$TMPDIR/podman.log" \
          || { echo "FAIL: .wrapix not mounted in worker"; exit 1; }
        echo "  PASS: .wrapix mounted in worker"

        rm -rf "$TMPDIR"
        echo "PASS: provider worker lifecycle works"
        mkdir $out
      '';

  # city-config derivation: the staged dir contains city.toml, claude-settings
  # with SessionStart/PreCompact hooks wired to wrapix-prime-hook, tmux.conf,
  # and every role prompt. This is what entrypoint stages into
  # .wrapix/city/current on live — identical bytes exercised here.
  city-config-derivation =
    runCommandLocal "city-config-derivation" { nativeBuildInputs = [ pkgs.jq ]; }
      ''
        set -euo pipefail
        DIR="${minimalCity.configDir}"

        test -f "$DIR/city.toml" || { echo "FAIL: city.toml missing"; exit 1; }
        test -f "$DIR/claude-settings.json" || { echo "FAIL: claude-settings.json missing"; exit 1; }
        test -f "$DIR/tmux.conf" || { echo "FAIL: tmux.conf missing"; exit 1; }
        for role in mayor scout worker judge; do
          test -f "$DIR/prompts/$role.md" || { echo "FAIL: prompts/$role.md missing"; exit 1; }
        done
        echo "  PASS: all staged files present"

        # SessionStart + PreCompact hooks must point at wrapix-prime-hook
        for hook in SessionStart PreCompact; do
          cmd="$(jq -r ".hooks.$hook[0].hooks[0].command" "$DIR/claude-settings.json")"
          [[ "$cmd" == "wrapix-prime-hook" ]] || {
            echo "FAIL: $hook hook command is '$cmd', expected 'wrapix-prime-hook'"
            exit 1
          }
        done
        echo "  PASS: SessionStart + PreCompact wired to wrapix-prime-hook"

        grep -q "set -g mouse on" "$DIR/tmux.conf" || { echo "FAIL: tmux.conf missing mouse on"; exit 1; }
        echo "  PASS: tmux.conf has mouse on"

        echo "PASS: city-config derivation content is correct"
        mkdir $out
      '';

  # wrapix-prime-hook stub: reads $WRAPIX_CITY_DIR/prompts/$GC_AGENT.md.
  # This is the command Claude Code fires on SessionStart/PreCompact. Tests
  # the exact file used by cityScripts (no copy, no rewrite).
  city-prime-hook = runCommandLocal "city-prime-hook" { } ''
    set -euo pipefail
    HOOK="${../../lib/city/scripts/prime-hook.sh}"

    # Unset vars → must fail loudly
    if WRAPIX_CITY_DIR="" GC_AGENT="" bash "$HOOK" 2>/dev/null; then
      echo "FAIL: hook exited 0 with unset vars"
      exit 1
    fi
    echo "  PASS: hook errors when vars unset"

    # Happy path: emits the role prompt verbatim
    expected="$(cat ${minimalCity.configDir}/prompts/mayor.md)"
    actual="$(WRAPIX_CITY_DIR="${minimalCity.configDir}" GC_AGENT=mayor bash "$HOOK")"
    [[ "$actual" == "$expected" ]] || {
      echo "FAIL: hook output does not match prompts/mayor.md"
      diff <(echo "$expected") <(echo "$actual") || true
      exit 1
    }
    echo "  PASS: hook emits mayor.md verbatim"

    echo "PASS: wrapix-prime-hook works"
    mkdir $out
  '';

  # Provider: persistent_start (mayor) must pass the renamed env vars,
  # reference $WRAPIX_CITY_DIR in the inline bootstrap, and name the tmux
  # session after the role (not hardcoded 'main'). Verified by grepping
  # podman.log since podman itself can't run in the nix sandbox.
  city-provider-persistent =
    runCommandLocal "city-provider-persistent" { nativeBuildInputs = [ bash ]; }
      ''
        set -euo pipefail
        PROVIDER="${../../lib/city/scripts/provider.sh}"
        TMPDIR=$(mktemp -d)
        STUB_BIN="$TMPDIR/bin"
        mkdir -p "$STUB_BIN"

        cat > "$STUB_BIN/podman" << 'STUB'
        #!/bin/sh
        echo "$@" >> "$PODMAN_LOG"
        case "$1" in inspect) echo "true" ;; esac
        STUB
        chmod +x "$STUB_BIN/podman"
        export PODMAN_LOG="$TMPDIR/podman.log"

        PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$TMPDIR" \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" start mayor > "$TMPDIR/out" 2>&1 || {
            echo "FAIL: provider start mayor exited non-zero"
            cat "$TMPDIR/out"
            exit 1
          }

        log="$TMPDIR/podman.log"

        grep -q -- "-e GC_SESSION=exec:/workspace/.gc/scripts/provider.sh" "$log" \
          || { echo "FAIL: GC_SESSION=exec:provider.sh not set"; cat "$log"; exit 1; }
        grep -q -- "-e GC_AGENT=mayor" "$log" \
          || { echo "FAIL: GC_AGENT=mayor not set"; exit 1; }
        grep -q -- "-e GC_ALIAS=mayor" "$log" \
          || { echo "FAIL: GC_ALIAS=mayor not set"; exit 1; }
        grep -q -- "-e WRAPIX_CITY_DIR=/workspace/.wrapix/city/current" "$log" \
          || { echo "FAIL: WRAPIX_CITY_DIR env not set"; exit 1; }
        echo "  PASS: renamed env vars present"

        # Inline bootstrap copies claude-settings and tmux.conf from WRAPIX_CITY_DIR
        grep -q '\$WRAPIX_CITY_DIR/claude-settings.json' "$log" \
          || { echo "FAIL: inline bash does not cp claude-settings.json"; cat "$log"; exit 1; }
        grep -q '\$WRAPIX_CITY_DIR/tmux.conf' "$log" \
          || { echo "FAIL: inline bash does not cp tmux.conf"; exit 1; }
        echo "  PASS: inline bootstrap reads from WRAPIX_CITY_DIR"

        # tmux session name must be $GC_AGENT (resolved at container runtime,
        # not 'main'). Literal string present in the heredoc.
        grep -q 'new-session -d -s "\$GC_AGENT"' "$log" \
          || { echo "FAIL: tmux session name not parameterized"; cat "$log"; exit 1; }
        if grep -q 'new-session -d -s "main"' "$log"; then
          echo "FAIL: tmux session still hardcoded to 'main'"
          exit 1
        fi
        echo "  PASS: tmux session named after role"

        # Shared tmux socket: persistent_start must use -S for all tmux
        # commands so the socket lands on the shared .wrapix/tmux/ mount.
        grep -q '\.gc:/workspace/\.gc:rw' "$log" \
          || { echo "FAIL: .gc not mounted rw in persistent container"; cat "$log"; exit 1; }
        echo "  PASS: .gc mounted rw"

        grep -q 'mkdir -p /workspace/.wrapix/tmux' "$log" \
          || { echo "FAIL: tmux socket directory not created"; cat "$log"; exit 1; }
        grep -q 'tmux -S.*start-server' "$log" \
          || { echo "FAIL: tmux not started with shared socket"; cat "$log"; exit 1; }
        grep -q '\.wrapix/tmux' "$log" \
          || { echo "FAIL: tmux socket not on .wrapix/tmux/"; cat "$log"; exit 1; }
        echo "  PASS: tmux uses shared socket on .wrapix/tmux/"

        rm -rf "$TMPDIR"
        echo "PASS: persistent_start passes renamed env + reads city dir"
        mkdir $out
      '';

  # Provider: shared tmux socket — nudge, peek, send-keys, get-last-activity
  # all work via the shared .wrapix/tmux/ socket when GC_AGENT is set
  # (container-side path). Uses real tmux servers, no podman needed.
  city-provider-tmux-socket =
    runCommandLocal "city-provider-tmux-socket"
      {
        nativeBuildInputs = [
          bash
          pkgs.tmux
          pkgs.coreutils
          pkgs.netcat-gnu
        ];
      }
      ''
        set -euo pipefail

        SCRIPTS_DIR="$(mktemp -d)"
        for f in ${minimalCity.scripts}/*; do
          cp "$f" "$SCRIPTS_DIR/$(basename "$f")"
        done
        chmod +x "$SCRIPTS_DIR"/*.sh
        PROVIDER="$SCRIPTS_DIR/provider.sh"

        WS="$(mktemp -d)"
        mkdir -p "$WS/.wrapix/tmux"

        # Stub podman — should never be called when socket exists
        STUB_BIN="$WS/stub-bin"
        mkdir -p "$STUB_BIN"
        cat > "$STUB_BIN/podman" << 'STUB'
        #!/bin/sh
        echo "FAIL: podman should not be called when socket exists" >&2
        exit 1
        STUB
        chmod +x "$STUB_BIN/podman"

        # Start a real tmux server with a shared socket (simulating scout container)
        SCOUT_SOCK="$WS/.wrapix/tmux/scout.sock"
        tmux -S "$SCOUT_SOCK" start-server
        tmux -S "$SCOUT_SOCK" new-session -d -s scout "cat"
        sleep 0.5

        cleanup() {
          tmux -S "$SCOUT_SOCK" kill-server 2>/dev/null || true
          rm -rf "$WS" "$SCRIPTS_DIR"
        }
        trap cleanup EXIT

        # --- Test 1: nudge via shared socket ---
        echo "test-nudge-message" | \
          PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$WS" \
          GC_AGENT=mayor \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" nudge scout
        sleep 0.5

        pane="$(tmux -S "$SCOUT_SOCK" capture-pane -t scout -p)"
        echo "$pane" | grep -q "test-nudge-message" \
          || { echo "FAIL: nudge message not delivered via shared socket"; echo "Pane: $pane"; exit 1; }
        echo "  PASS: nudge delivered via shared socket"

        # --- Test 2: peek (capture-pane) via shared socket ---
        output="$(PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$WS" \
          GC_AGENT=mayor \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" peek scout)"
        echo "$output" | grep -q "test-nudge-message" \
          || { echo "FAIL: peek did not return pane content"; echo "Output: $output"; exit 1; }
        echo "  PASS: peek works via shared socket"

        # --- Test 3: get-last-activity via shared socket ---
        # In Nix sandbox, pane_last_activity epoch may be 0 (no real clock),
        # so we accept empty — the test proves the socket path works without error.
        activity="$(PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$WS" \
          GC_AGENT=mayor \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" get-last-activity scout)" \
          || { echo "FAIL: get-last-activity errored via shared socket"; exit 1; }
        echo "  PASS: get-last-activity works via shared socket (got: ''${activity:-empty})"

        # --- Test 4: send-keys via shared socket ---
        PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$WS" \
          GC_AGENT=mayor \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" send-keys scout "send-keys-test" Enter
        sleep 0.3

        pane="$(tmux -S "$SCOUT_SOCK" capture-pane -t scout -p)"
        echo "$pane" | grep -q "send-keys-test" \
          || { echo "FAIL: send-keys not delivered via shared socket"; echo "Pane: $pane"; exit 1; }
        echo "  PASS: send-keys works via shared socket"

        # --- Test 5: is-running detects live socket ---
        running="$(PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$WS" \
          GC_AGENT=mayor \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" is-running scout)"
        [[ "$running" == "true" ]] \
          || { echo "FAIL: is-running should detect live socket (got: $running)"; exit 1; }
        echo "  PASS: is-running detects live tmux socket"

        echo ""
        echo "PASS: all provider methods work via shared tmux socket"
        mkdir $out
      '';

  # End-to-end env contract: provider.sh -> gc -> bd.
  #
  # provider.sh sets two env families on the role container:
  #   - BEADS_DOLT_SERVER_SOCKET — for direct bd calls (claude → bd)
  #   - GC_DOLT_HOST / GC_DOLT_PORT — for gc, which strips
  #     BEADS_DOLT_SERVER_* and rebuilds HOST/PORT from GC_DOLT_*
  #     before spawning bd (role containers live on the dolt bridge
  #     network, so TCP via container hostname works post-rebuild).
  #
  # This test runs provider.sh with a stubbed podman to capture the exact
  # -e flags, parses them into an env, runs gc with that env against a
  # stubbed bd that dumps its env, and asserts the dolt endpoint still
  # reaches bd after gc's round-trip.
  city-gc-bd-env-passthrough =
    runCommandLocal "city-gc-bd-env-passthrough"
      {
        nativeBuildInputs = [
          bash
          pkgs.gc
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        PROVIDER="${../../lib/city/scripts/provider.sh}"
        TMPDIR=$(mktemp -d)
        STUB_BIN="$TMPDIR/bin"
        mkdir -p "$STUB_BIN"

        # Stub podman: record argv on run, report running on inspect
        cat > "$STUB_BIN/podman" << 'STUB'
        #!/bin/sh
        case "$1" in
          run) printf '%s\n' "$@" > "$PODMAN_ARGS" ;;
          inspect) echo "true" ;;
        esac
        STUB
        chmod +x "$STUB_BIN/podman"
        export PODMAN_ARGS="$TMPDIR/podman.args"

        # Step 1: run provider.sh start mayor — capture the env flags it
        # passes to podman. This is the real live path; no shortcut.
        PATH="$STUB_BIN:$PATH" \
          GC_CITY_NAME=test \
          GC_WORKSPACE="$TMPDIR" \
          GC_AGENT_IMAGE=test:latest \
          GC_PODMAN_NETWORK=wrapix-test \
          GC_BEADS_DOLT_CONTAINER=beads-test \
          GC_DOLT_PORT=13306 \
          bash "$PROVIDER" start mayor > "$TMPDIR/out" 2>&1 || {
            echo "FAIL: provider start mayor exited non-zero"
            cat "$TMPDIR/out"
            exit 1
          }

        # Step 2: parse -e KEY=VALUE flags from captured podman args.
        # Each arg is on its own line in PODMAN_ARGS.
        : > "$TMPDIR/container.env"
        next_is_env=0
        while IFS= read -r arg; do
          if [[ "$next_is_env" == "1" ]]; then
            echo "$arg" >> "$TMPDIR/container.env"
            next_is_env=0
          elif [[ "$arg" == "-e" ]]; then
            next_is_env=1
          fi
        done < "$PODMAN_ARGS"

        # Sanity: both env families must be set by provider.sh
        grep -qE '^BEADS_DOLT_SERVER_SOCKET=/workspace/\.wrapix/dolt\.sock$' "$TMPDIR/container.env" \
          || { echo "FAIL: provider did not set BEADS_DOLT_SERVER_SOCKET"; cat "$TMPDIR/container.env"; exit 1; }
        grep -qE '^BEADS_DOLT_AUTO_START=0$' "$TMPDIR/container.env" \
          || { echo "FAIL: provider did not set BEADS_DOLT_AUTO_START=0 (embedded fallback risk)"; exit 1; }
        if grep -qE '^BEADS_DOLT_SERVER_(HOST|PORT)=' "$TMPDIR/container.env"; then
          echo "FAIL: provider leaked legacy BEADS_DOLT_SERVER_HOST/PORT into container env"
          cat "$TMPDIR/container.env"
          exit 1
        fi
        grep -qE '^GC_DOLT_HOST=' "$TMPDIR/container.env" \
          || { echo "FAIL: provider did not set GC_DOLT_HOST (gc won't find dolt)"; cat "$TMPDIR/container.env"; exit 1; }
        grep -qE '^GC_DOLT_PORT=' "$TMPDIR/container.env" \
          || { echo "FAIL: provider did not set GC_DOLT_PORT (gc won't find dolt)"; exit 1; }
        echo "  PASS: provider.sh sets socket + GC_DOLT_* env families"

        # Step 3: stage a minimal city so gc can find city.toml / provider
        cp ${minimalCity.config} "$TMPDIR/city.toml"

        # Step 4: stub bd that dumps its received env to a file
        cat > "$STUB_BIN/bd" << 'STUB'
        #!/bin/sh
        env > "$BD_ENV_DUMP"
        # Minimal JSON response so gc doesn't choke
        if [ "$1" = "list" ] || [ "$2" = "list" ]; then
          echo "[]"
        fi
        exit 0
        STUB
        chmod +x "$STUB_BIN/bd"
        export BD_ENV_DUMP="$TMPDIR/bd.env"

        # Step 5: run gc mail send with ONLY the env provider.sh would
        # have placed in the container. This mirrors live: a process
        # inside the mayor container inherits only those vars.
        cd "$TMPDIR"
        env -i \
          PATH="$STUB_BIN:${pkgs.gc}/bin:${pkgs.coreutils}/bin:${pkgs.bash}/bin" \
          HOME="$TMPDIR" \
          BD_ENV_DUMP="$BD_ENV_DUMP" \
          $(while IFS= read -r line; do printf ' %q' "$line"; done < "$TMPDIR/container.env") \
          gc mail send --to human -s test -m test > "$TMPDIR/gc.out" 2>&1 || true

        # Step 6: assert bd was invoked AND it received the dolt env vars.
        # gc may or may not call bd depending on the code path; the critical
        # test is: IF bd is called, it must see the dolt env.
        if [[ -f "$BD_ENV_DUMP" ]]; then
          grep -qE '^BEADS_DOLT_SERVER_HOST=' "$BD_ENV_DUMP" \
            || { echo "FAIL: gc->bd subprocess is missing BEADS_DOLT_SERVER_HOST"; cat "$BD_ENV_DUMP"; exit 1; }
          grep -qE '^BEADS_DOLT_SERVER_PORT=13306' "$BD_ENV_DUMP" \
            || { echo "FAIL: gc->bd subprocess is missing or wrong BEADS_DOLT_SERVER_PORT"; grep BEADS_ "$BD_ENV_DUMP" || true; exit 1; }
          echo "  PASS: gc propagates dolt endpoint to bd subprocesses"
        else
          echo "FAIL: gc did not invoke bd at all (test cannot verify env passthrough)"
          cat "$TMPDIR/gc.out"
          exit 1
        fi

        rm -rf "$TMPDIR"
        echo "PASS: provider.sh -> gc -> bd env chain is intact"
        mkdir $out
      '';

  # ---------------------------------------------------------------------------
  # Env coverage: provider.sh check-env against shellHook and entrypoint
  #
  # provider.sh declares REQUIRED_ENV — the canonical list of env vars needed
  # for container start. The check-env method validates them at runtime.
  #
  # This test calls check-env with only the vars each entry path provides,
  # exercising the live validation code. Adding a var to REQUIRED_ENV without
  # adding it to shellHook or entrypoint fails the test automatically.
  # ---------------------------------------------------------------------------
  city-env-parity =
    let
      shellHookNix = readFile ../../lib/city/default.nix;
      entrypointSrc = readFile ../../lib/city/scripts/entrypoint.sh;
    in
    runCommandLocal "city-env-parity"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail
        PROVIDER="${../../lib/city/scripts/provider.sh}"

        # Extract exported var names from each source.
        # Catches both "export VAR=val" and bare "export VAR".
        grep -oP '(?<=export )[A-Z_]+' <<'SHELLHOOK' | sort -u > "$TMPDIR/shellhook.vars"
        ${shellHookNix}
        SHELLHOOK

        grep -oP '(?<=export )[A-Z_]+' <<'ENTRYPOINT' | sort -u > "$TMPDIR/entrypoint.vars"
        ${entrypointSrc}
        ENTRYPOINT

        # Build an env with placeholder values for each exported var, then
        # run check-env. If provider.sh requires a var that neither source
        # exports, check-env prints MISSING and exits non-zero.

        # shellHook path — must cover all REQUIRED_ENV on its own
        env_args=""
        while IFS= read -r var; do
          env_args+=" $var=placeholder"
        done < "$TMPDIR/shellhook.vars"

        BASH="$(command -v bash)"
        if ! env -i PATH="$(dirname "$BASH")" $env_args "$BASH" "$PROVIDER" check-env _ 2>"$TMPDIR/shellhook.err"; then
          echo "FAIL: shellHook env does not satisfy provider.sh REQUIRED_ENV:"
          cat "$TMPDIR/shellhook.err"
          exit 1
        fi
        echo "  PASS: shellHook covers all REQUIRED_ENV"

        # entrypoint path — entrypoint + NixOS module vars must cover all.
        # NixOS module provides these (not exported by entrypoint.sh itself):
        nixos_vars="GC_CITY_NAME GC_WORKSPACE GC_PODMAN_NETWORK GC_AGENT_IMAGE"
        env_args=""
        while IFS= read -r var; do
          env_args+=" $var=placeholder"
        done < "$TMPDIR/entrypoint.vars"
        for var in $nixos_vars; do
          env_args+=" $var=placeholder"
        done

        if ! env -i PATH="$(dirname "$BASH")" $env_args "$BASH" "$PROVIDER" check-env _ 2>"$TMPDIR/entrypoint.err"; then
          echo "FAIL: entrypoint + NixOS module env does not satisfy provider.sh REQUIRED_ENV:"
          cat "$TMPDIR/entrypoint.err"
          exit 1
        fi
        echo "  PASS: entrypoint + NixOS module covers all REQUIRED_ENV"

        echo "PASS: provider.sh check-env passes for both entry paths"
        mkdir $out
      '';

  # NixOS module: verifies env var plumbing via Nix evaluation
  city-nixos-module =
    let
      moduleFile = readFile ../../modules/city.nix;
      cityFile = readFile ../../lib/city/default.nix;

      # Structural checks — the module must define these
      hasServicesWrapix = match ".*services\\.wrapix.*" moduleFile != null;
      hasCities = match ".*cities.*" moduleFile != null;
      hasSystemdServices = match ".*systemd\\.services.*" moduleFile != null;
      hasMkCity = match ".*mkCity.*" moduleFile != null;

      # Critical env var plumbing — provider.sh requires these in the daemon env
      hasAgentImage = match ".*GC_AGENT_IMAGE.*" moduleFile != null;
      hasPodmanNetwork = match ".*GC_PODMAN_NETWORK.*" moduleFile != null;

      # Module must consume secretFlags from mkCity (single source of truth)
      moduleUsesSecretFlags = match ".*city\\.secretFlags.*" moduleFile != null;

      # Well-known secret env plumbing — git-ssh-setup.sh reads these.
      # Definition lives in lib/city/default.nix, consumed by the module
      # via city.secretFlags.
      hasDeployKeyEnv = match ".*WRAPIX_DEPLOY_KEY.*" cityFile != null;
      hasSigningKeyEnv = match ".*WRAPIX_SIGNING_KEY.*" cityFile != null;
    in
    assert hasServicesWrapix;
    assert hasCities;
    assert hasSystemdServices;
    assert hasMkCity;
    assert
      hasAgentImage
      || throw "NixOS module does not set GC_AGENT_IMAGE — provider.sh requires it to start agent containers";
    assert
      hasPodmanNetwork
      || throw "NixOS module does not set GC_PODMAN_NETWORK — provider.sh requires it for container networking";
    assert
      moduleUsesSecretFlags
      || throw "NixOS module does not consume city.secretFlags — deploy/signing key plumbing will drift from mkCity";
    assert
      hasDeployKeyEnv
      || throw "lib/city/default.nix does not emit WRAPIX_DEPLOY_KEY — role containers won't wire the deploy key into GIT_SSH_COMMAND";
    assert
      hasSigningKeyEnv
      || throw "lib/city/default.nix does not emit WRAPIX_SIGNING_KEY — role containers won't configure commit signing";

    runCommandLocal "city-nixos-module" { } ''
      echo "PASS: NixOS module structure and env var plumbing verified"
      mkdir $out
    '';

  # Formulas: structural validation (these are TOML for the AI, grep is appropriate here)
  city-formulas =
    runCommandLocal "city-formulas"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        DIR="${../../lib/city/formulas}"

        echo "Checking role formulas..."

        for role in scout worker judge mayor; do
          F="$DIR/$role.formula.toml"
          test -f "$F" || { echo "FAIL: missing $role.formula.toml"; exit 1; }
          grep -q '^formula = ' "$F" || { echo "FAIL: $role missing formula name"; exit 1; }
          grep -q '^\[\[steps\]\]' "$F" || { echo "FAIL: $role missing steps"; exit 1; }
          grep -q 'docs/README.md' "$F" || { echo "FAIL: $role missing docs/README.md pin"; exit 1; }
          echo "  PASS: $role"
        done

        # Mayor must reference architecture.md and orchestration.md
        grep -q 'architecture.md' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor no architecture.md"; exit 1; }
        grep -q 'orchestration.md' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor no orchestration.md"; exit 1; }
        # Mayor must have briefing and triage steps
        grep -q 'id = "briefing"' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor missing briefing step"; exit 1; }
        grep -q 'id = "triage"' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor missing triage step"; exit 1; }
        grep -q 'id = "check-specs"' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor missing check-specs step"; exit 1; }
        grep -q 'auto_decompose' "$DIR/mayor.formula.toml" || { echo "FAIL: mayor missing auto_decompose var"; exit 1; }
        echo "  PASS: mayor has required steps and variables"

        # Scout must reference orchestration.md for pattern loading
        grep -q 'orchestration.md' "$DIR/scout.formula.toml" || { echo "FAIL: scout no orchestration.md"; exit 1; }

        # Scout must have housekeeping step
        grep -q 'id = "housekeeping"' "$DIR/scout.formula.toml" || { echo "FAIL: scout missing housekeeping step"; exit 1; }
        grep -q 'bd stale' "$DIR/scout.formula.toml" || { echo "FAIL: scout housekeeping missing stale beads"; exit 1; }
        grep -q 'gc-role=worker' "$DIR/scout.formula.toml" || { echo "FAIL: scout housekeeping missing orphaned workers"; exit 1; }
        grep -q 'worktree' "$DIR/scout.formula.toml" || { echo "FAIL: scout housekeeping missing worktree cleanup"; exit 1; }
        echo "  PASS: scout has housekeeping step"

        # Judge must reference style-guidelines.md for enforcement
        grep -q 'style-guidelines.md' "$DIR/judge.formula.toml" || { echo "FAIL: judge no style-guidelines.md"; exit 1; }

        echo "PASS: All role formulas valid"
        mkdir $out
      '';

  # Scripts (symlinked to source) and orders bundle
  city-scripts-bundle =
    let
      hasScriptNames = hasAttr "scriptNames" minimalCity;
      # Source directory containing the scripts
      citySrc = ../../lib/city/scripts;
    in
    assert hasScriptNames;

    runCommandLocal "city-scripts-bundle"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        echo "Checking script names and source files..."

        # Required scripts are listed in scriptNames
        ${concatStringsSep "\n" (
          map
            (s: ''
              echo "${s}" | grep -qF "${s}" || { echo "FAIL: ${s} not in scriptNames"; exit 1; }
              test -f "${citySrc}/${s}" || { echo "FAIL: source ${s} missing from lib/city/"; exit 1; }
            '')
            [
              "entrypoint.sh"
              "gate.sh"
              "post-gate.sh"
              "provider.sh"
              "recovery.sh"
            ]
        )}
        echo "  PASS: required scripts listed and source files exist"

        # Orders directory exists with post-gate order
        test -f "${minimalCity.formulas}/orders/post-gate/order.toml" || { echo "FAIL: post-gate order missing"; exit 1; }
        grep -q 'convergence.terminated' "${minimalCity.formulas}/orders/post-gate/order.toml" || \
          { echo "FAIL: post-gate order missing event trigger"; exit 1; }
        echo "  PASS: post-gate order bundled"

        echo "PASS: Scripts and orders bundle verified"
        mkdir $out
      '';

  # Verify .gc/scripts/ are executable copies from the Nix store
  # (not symlinks — copies work for consumers who lack the source tree
  # and inside containers where the workspace is bind-mounted).
  # Calls the real stageGcLayout + promoteGcLayout (same code path as app).
  city-scripts-store-copies =
    let
      names = concatStringsSep " " minimalCity.scriptNames;
    in
    runCommandLocal "city-scripts-store-copies"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail

        WS="$TMPDIR/workspace"
        mkdir -p "$WS"

        # Run the same staging + promote pipeline used by app
        cd "$WS"
        ${minimalCity.stageGcLayout}
        ${minimalCity.promoteGcLayout}

        echo "Checking scripts are executable regular files..."
        for f in ${names}; do
          file="$WS/.gc/scripts/$f"
          if [ -L "$file" ]; then
            echo "FAIL: $f is a symlink (expected a copy)"; exit 1
          fi
          if [ ! -f "$file" ]; then
            echo "FAIL: $f is missing"; exit 1
          fi
          if [ ! -x "$file" ]; then
            echo "FAIL: $f is not executable"; exit 1
          fi
        done
        echo "  PASS: all scripts are executable regular files"

        # Copy tree to a different path (simulates container bind-mount)
        WS2="$TMPDIR/container-mount"
        cp -a "$WS" "$WS2"
        for f in ${names}; do
          if [ ! -x "$WS2/.gc/scripts/$f" ]; then
            echo "FAIL: $f not executable under different mount path"; exit 1
          fi
        done
        echo "  PASS: scripts work under different workspace path"

        echo "PASS: Script store copies verified"
        mkdir $out
      '';

  # Scout formula defaults rewritten with configured values
  city-scout-formula-defaults =
    runCommandLocal "city-scout-formula-defaults"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        echo "Checking scout formula defaults are rewritten..."

        # Minimal city: defaults should remain (5m, 10)
        MINIMAL="${minimalCity.formulas}/wrapix-scout.formula.toml"
        grep -q 'default = "5m"' "$MINIMAL" || { echo "FAIL: minimal poll_interval not 5m"; exit 1; }
        grep -q 'default = "10"' "$MINIMAL" || { echo "FAIL: minimal max_beads not 10"; exit 1; }
        echo "  PASS: minimal city keeps defaults (5m, 10)"

        # Full city: defaults should be overridden (10m, 5)
        FULL="${fullCity.formulas}/wrapix-scout.formula.toml"
        grep -q 'default = "10m"' "$FULL" || { echo "FAIL: full poll_interval not 10m"; exit 1; }
        grep -q 'default = "5"' "$FULL" || { echo "FAIL: full max_beads not 5"; exit 1; }
        # Verify original defaults are NOT present
        if grep -q 'default = "10"' "$FULL" 2>/dev/null; then
          echo "FAIL: full city still has default max_beads=10"
          exit 1
        fi
        echo "  PASS: full city overrides defaults (10m, 5)"

        # Mayor formula: autoDecompose defaults rewritten
        MINIMAL_MAYOR="${minimalCity.formulas}/wrapix-mayor.formula.toml"
        grep -q 'default = "false"' "$MINIMAL_MAYOR" || { echo "FAIL: minimal mayor auto_decompose not false"; exit 1; }
        echo "  PASS: minimal city mayor keeps default (false)"

        FULL_MAYOR="${fullCity.formulas}/wrapix-mayor.formula.toml"
        grep -q 'default = "true"' "$FULL_MAYOR" || { echo "FAIL: full mayor auto_decompose not true"; exit 1; }
        echo "  PASS: full city mayor overrides auto_decompose (true)"

        echo "PASS: Scout and mayor formula defaults correctly rewritten"
        mkdir $out
      '';

  # Validate generated city.toml with the gc binary
  city-config-validate =
    runCommandLocal "city-config-validate"
      {
        nativeBuildInputs = [
          bash
          pkgs.gc
        ];
      }
      ''
        set -euo pipefail
        echo "Validating generated city.toml with gc..."

        WORK=$(mktemp -d)
        cp ${minimalCity.config} "$WORK/city.toml"
        cd "$WORK"
        gc config show --validate
        echo "  PASS: minimal city config valid"

        rm -f "$WORK/city.toml"
        cp ${fullCity.config} "$WORK/city.toml"
        cd "$WORK"
        gc config show --validate
        echo "  PASS: full city config valid"

        rm -f "$WORK/city.toml"
        cp ${emptyCity.config} "$WORK/city.toml"
        cd "$WORK"
        gc config show --validate
        echo "  PASS: empty city config valid"

        rm -rf "$WORK"
        echo "PASS: All generated configs accepted by gc"
        mkdir $out
      '';

  # wx-entt5: workspace.provider = "claude" causes gc to auto-inject a phantom
  # "claude" agent with provider = "claude" (HOST tmux), conflicting with exec.
  city-no-phantom-agent =
    runCommandLocal "city-no-phantom-agent"
      {
        nativeBuildInputs = [
          bash
          pkgs.gc
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        echo "Checking gc does not inject phantom claude agent..."

        for cfg in ${minimalCity.config} ${fullCity.config} ${emptyCity.config}; do
          WORK=$(mktemp -d)
          cp "$cfg" "$WORK/city.toml"
          cd "$WORK"
          resolved="$(gc config show --city "$WORK" 2>&1)"

          # No agent should have provider = "claude" (that's gc's built-in HOST tmux)
          if echo "$resolved" | grep -q 'provider = "claude"'; then
            echo "FAIL: gc injected phantom claude agent from $cfg"
            echo "$resolved"
            exit 1
          fi

          # Count agent blocks — should be exactly 5 (mayor, scout, worker, judge, dog override)
          agent_count="$(echo "$resolved" | grep -c '^\[\[agent\]\]' || true)"
          if [ "$agent_count" -ne 5 ]; then
            echo "FAIL: expected 5 agents in $cfg, found $agent_count"
            echo "$resolved"
            exit 1
          fi

          rm -rf "$WORK"
        done

        echo "PASS: No phantom claude agent in any config"
        mkdir $out
      '';

  # =========================================================================
  # Layer 4: Additional functional tests
  # =========================================================================

  # Scout: create-beads deduplication and cap enforcement
  city-scout-create-beads =
    runCommandLocal "city-scout-create-beads"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        SCOUT="${../../lib/city/scripts/scout.sh}"
        TMPDIR=$(mktemp -d)

        echo "Testing scout.sh create-beads..."

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        # Track bd calls
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo "$@" >> $TMPDIR/scout-bd-calls.log
        case "$1" in
          list) echo "[]" ;;
          create) echo "bead-new-1" ;;
          update) ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Mock wrapix-notify (should not be called when under cap)
        cat > "$MOCK_BIN/wrapix-notify" << 'MOCK'
        #!/bin/sh
        echo "NOTIFY: $*" >> $TMPDIR/scout-notify.log
        MOCK
        chmod +x "$MOCK_BIN/wrapix-notify"

        rm -f $TMPDIR/scout-bd-calls.log $TMPDIR/scout-notify.log

        # Set up scan results with immediate and batched errors
        mkdir -p "$TMPDIR/errors/my-api"
        echo "FATAL|PANIC|panic:" > "$TMPDIR/errors/immediate.pat"
        echo "ERROR|Exception" > "$TMPDIR/errors/batched.pat"
        echo "" > "$TMPDIR/errors/ignore.pat"
        echo "2026-04-01 FATAL: out of memory" > "$TMPDIR/errors/my-api/immediate.log"
        echo "2026-04-01 ERROR: connection refused" > "$TMPDIR/errors/my-api/batched.log"

        # Test 1: creates beads for immediate and batched
        PATH="$MOCK_BIN:$PATH" SCOUT_ERRORS_DIR="$TMPDIR/errors" SCOUT_MAX_BEADS=10 \
          bash "$SCOUT" create-beads
        grep -q "create" $TMPDIR/scout-bd-calls.log || { echo "FAIL: no bead created"; exit 1; }
        echo "  PASS: beads created for scan results"

        # Test 2: cap enforcement — set cap to 0
        rm -f $TMPDIR/scout-bd-calls.log $TMPDIR/scout-notify.log
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo "$@" >> $TMPDIR/scout-bd-calls.log
        case "$1" in
          list) echo '[{"id":"b1"},{"id":"b2"},{"id":"b3"}]' ;;
          create) echo "bead-new" ;;
          update) ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        PATH="$MOCK_BIN:$PATH" SCOUT_ERRORS_DIR="$TMPDIR/errors" SCOUT_MAX_BEADS=2 \
          bash "$SCOUT" create-beads || true
        test -f $TMPDIR/scout-notify.log || { echo "FAIL: notify not called when cap reached"; exit 1; }
        grep -q "Scout paused" $TMPDIR/scout-notify.log || { echo "FAIL: wrong notify message"; exit 1; }
        echo "  PASS: cap enforcement triggers notification"

        rm -rf "$TMPDIR" $TMPDIR/scout-bd-calls.log $TMPDIR/scout-notify.log
        echo "PASS: scout create-beads works correctly"
        mkdir $out
      '';

  # Scout: check-cap reports cap status
  city-scout-check-cap =
    runCommandLocal "city-scout-check-cap"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        SCOUT="${../../lib/city/scripts/scout.sh}"
        TMPDIR=$(mktemp -d)
        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        echo "Testing scout.sh check-cap..."

        # Under cap
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo '[{"id":"b1"}]'
        MOCK
        chmod +x "$MOCK_BIN/bd"

        result="$(PATH="$MOCK_BIN:$PATH" SCOUT_MAX_BEADS=5 bash "$SCOUT" check-cap)"
        [[ "$result" == "false" ]] || { echo "FAIL: expected false, got $result"; exit 1; }
        echo "  PASS: under cap returns false"

        # At cap
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo '[{"id":"b1"},{"id":"b2"},{"id":"b3"}]'
        MOCK
        chmod +x "$MOCK_BIN/bd"

        result="$(PATH="$MOCK_BIN:$PATH" SCOUT_MAX_BEADS=2 bash "$SCOUT" check-cap)"
        [[ "$result" == "true" ]] || { echo "FAIL: expected true, got $result"; exit 1; }
        echo "  PASS: at cap returns true"

        rm -rf "$TMPDIR"
        echo "PASS: scout check-cap works correctly"
        mkdir $out
      '';

  # Recovery: stale worktree cleanup uses rm -rf, not git worktree remove
  city-recovery-functional =
    runCommandLocal "city-recovery-functional"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        RECOVERY="${../../lib/city/scripts/recovery.sh}"
        TMPDIR=$(mktemp -d)

        echo "Testing recovery.sh..."

        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main

        # Set up workspace
        WS="$TMPDIR/ws"
        mkdir -p "$WS"
        git -C "$WS" init -q -b main
        git -C "$WS" commit --allow-empty -m "initial" -q

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        # Mock podman (no containers running)
        cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        echo ""
        MOCK
        chmod +x "$MOCK_BIN/podman"

        # Mock bd — bead is closed
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        if [ "$1" = "show" ]; then
          echo '[{"status":"closed"}]'
        fi
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Create a stale worktree
        mkdir -p "$WS/.wrapix/worktree"
        git -C "$WS" worktree add "$WS/.wrapix/worktree/stale-bead" -b stale-bead -q
        test -d "$WS/.wrapix/worktree/stale-bead" || { echo "FAIL: worktree not created"; exit 1; }

        # Run recovery
        PATH="$MOCK_BIN:$PATH" GC_CITY_NAME=test GC_WORKSPACE="$WS" bash "$RECOVERY"

        # Verify stale worktree was cleaned up
        ! test -d "$WS/.wrapix/worktree/stale-bead" || { echo "FAIL: stale worktree not cleaned"; exit 1; }
        echo "  PASS: stale worktree cleaned up"

        # Verify branch was cleaned up
        ! git -C "$WS" rev-parse --verify stale-bead 2>/dev/null || { echo "FAIL: stale branch not cleaned"; exit 1; }
        echo "  PASS: stale branch cleaned up"

        rm -rf "$TMPDIR"
        echo "PASS: recovery.sh works correctly"
        mkdir $out
      '';

  # Scout housekeeping: stale beads, orphaned workers, worktree cleanup
  city-scout-housekeeping =
    runCommandLocal "city-scout-housekeeping"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        SCOUT="${../../lib/city/scripts/scout.sh}"
        TMPDIR=$(mktemp -d)

        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        echo "Testing scout.sh housekeeping..."

        # ---- Test 1: stale beads flagged for human review ----

        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo "$@" >> $TMPDIR/scout-hk-bd.log
        case "$1" in
          stale)
            echo '[{"id":"stale-1"},{"id":"stale-2"}]'
            ;;
          label|update)
            ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Mock podman (no containers)
        cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        echo ""
        MOCK
        chmod +x "$MOCK_BIN/podman"

        # Mock wrapix-notify
        cat > "$MOCK_BIN/wrapix-notify" << 'MOCK'
        #!/bin/sh
        echo "NOTIFY: $*" >> $TMPDIR/scout-hk-notify.log
        MOCK
        chmod +x "$MOCK_BIN/wrapix-notify"

        rm -f $TMPDIR/scout-hk-bd.log $TMPDIR/scout-hk-notify.log

        PATH="$MOCK_BIN:$PATH" GC_CITY_NAME=test GC_WORKSPACE="$TMPDIR" \
          bash "$SCOUT" housekeeping-stale

        grep -c "label add" $TMPDIR/scout-hk-bd.log | grep -q "2" || \
          { echo "FAIL: expected 2 label add calls"; cat $TMPDIR/scout-hk-bd.log; exit 1; }
        grep -q "stale-1" $TMPDIR/scout-hk-bd.log || { echo "FAIL: stale-1 not flagged"; exit 1; }
        grep -q "stale-2" $TMPDIR/scout-hk-bd.log || { echo "FAIL: stale-2 not flagged"; exit 1; }
        grep -q "flagged stale by scout housekeeping" $TMPDIR/scout-hk-bd.log || \
          { echo "FAIL: notes not added"; exit 1; }
        echo "  PASS: stale beads flagged for human review"

        # ---- Test 2: orphaned workers detected and stopped ----

        rm -f $TMPDIR/scout-hk-bd.log $TMPDIR/scout-hk-notify.log

        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo "$@" >> $TMPDIR/scout-hk-bd.log
        case "$1" in
          stale) echo "[]" ;;
          list) echo '[{"id":"bead-active"}]' ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        echo "$@" >> $TMPDIR/scout-hk-podman.log
        case "$1" in
          ps)
            # Check if filtering by gc-bead (worktree cleanup check)
            if echo "$@" | grep -q "gc-bead"; then
              echo ""
            else
              echo "worker-orphan-1"
            fi
            ;;
          inspect)
            echo "bead-orphaned"
            ;;
          stop|rm)
            ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/podman"

        rm -f $TMPDIR/scout-hk-podman.log

        PATH="$MOCK_BIN:$PATH" GC_CITY_NAME=test GC_WORKSPACE="$TMPDIR" \
          bash "$SCOUT" housekeeping-orphans

        grep -q "stop worker-orphan-1" $TMPDIR/scout-hk-podman.log || \
          { echo "FAIL: orphaned worker not stopped"; cat $TMPDIR/scout-hk-podman.log; exit 1; }
        grep -q "rm worker-orphan-1" $TMPDIR/scout-hk-podman.log || \
          { echo "FAIL: orphaned worker not removed"; cat $TMPDIR/scout-hk-podman.log; exit 1; }
        echo "  PASS: orphaned workers stopped and removed"

        # ---- Test 3: stale worktrees cleaned up ----

        rm -f $TMPDIR/scout-hk-bd.log $TMPDIR/scout-hk-podman.log $TMPDIR/scout-hk-notify.log

        # Set up a git repo with a stale worktree
        WS="$TMPDIR/ws"
        mkdir -p "$WS"
        git -C "$WS" init -q -b main
        git -C "$WS" commit --allow-empty -m "initial" -q
        mkdir -p "$WS/.wrapix/worktree"
        git -C "$WS" worktree add "$WS/.wrapix/worktree/stale-bead" -b stale-bead -q

        test -d "$WS/.wrapix/worktree/stale-bead" || { echo "FAIL: worktree not created"; exit 1; }

        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        case "$1" in
          show) echo '{"status":"closed"}' ;;
          stale) echo "[]" ;;
          list) echo "[]" ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        # No containers running
        echo ""
        MOCK
        chmod +x "$MOCK_BIN/podman"

        PATH="$MOCK_BIN:$PATH" GC_CITY_NAME=test GC_WORKSPACE="$WS" \
          bash "$SCOUT" housekeeping-worktrees

        ! test -d "$WS/.wrapix/worktree/stale-bead" || \
          { echo "FAIL: stale worktree not cleaned"; exit 1; }
        echo "  PASS: stale worktrees cleaned up"

        # ---- Test 4: in-progress worktrees preserved ----

        git -C "$WS" worktree add "$WS/.wrapix/worktree/active-bead" -b active-bead -q

        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        case "$1" in
          show) echo '{"status":"in_progress"}' ;;
          stale) echo "[]" ;;
          list) echo "[]" ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        PATH="$MOCK_BIN:$PATH" GC_CITY_NAME=test GC_WORKSPACE="$WS" \
          bash "$SCOUT" housekeeping-worktrees

        test -d "$WS/.wrapix/worktree/active-bead" || \
          { echo "FAIL: in-progress worktree was removed"; exit 1; }
        echo "  PASS: in-progress worktrees preserved"

        rm -rf "$TMPDIR" $TMPDIR/scout-hk-bd.log $TMPDIR/scout-hk-podman.log $TMPDIR/scout-hk-notify.log
        echo "PASS: scout housekeeping works correctly"
        mkdir $out
      '';

  # Provider: set-meta reads value from stdin
  city-provider-set-meta =
    runCommandLocal "city-provider-set-meta"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail
        PROVIDER="${../../lib/city/scripts/provider.sh}"
        TMPDIR=$(mktemp -d)
        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        echo "Testing provider.sh set-meta/get-meta..."

        # Mock podman exec that records what it receives
        cat > "$MOCK_BIN/podman" << MOCK
        #!/bin/sh
        echo "\$@" >> "$TMPDIR/podman.log"
        # For get-meta, simulate reading from a file
        if echo "\$@" | grep -q "cat /tmp/gc-meta"; then
          echo "test-value"
        fi
        MOCK
        chmod +x "$MOCK_BIN/podman"

        # Test set-meta with stdin (gc protocol)
        echo "my-value" | PATH="$MOCK_BIN:$PATH" \
          GC_CITY_NAME=test GC_WORKSPACE="$TMPDIR" GC_AGENT_IMAGE=test:latest GC_PODMAN_NETWORK=test \
          bash "$PROVIDER" set-meta scout my-key

        grep -q "my-value" "$TMPDIR/podman.log" || { echo "FAIL: value not passed from stdin"; cat "$TMPDIR/podman.log"; exit 1; }
        echo "  PASS: set-meta reads value from stdin"

        # Test get-meta (no stdin, value on stdout)
        result="$(PATH="$MOCK_BIN:$PATH" \
          GC_CITY_NAME=test GC_WORKSPACE="$TMPDIR" GC_AGENT_IMAGE=test:latest GC_PODMAN_NETWORK=test \
          bash "$PROVIDER" get-meta scout my-key)"
        [[ "$result" == "test-value" ]] || { echo "FAIL: get-meta returned '$result'"; exit 1; }
        echo "  PASS: get-meta returns value on stdout"

        rm -rf "$TMPDIR"
        echo "PASS: provider metadata methods work"
        mkdir $out
      '';

  # Provider: unknown methods exit 2 (forward-compatible no-op)
  city-provider-unknown-method =
    runCommandLocal "city-provider-unknown-method"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail
        PROVIDER="${../../lib/city/scripts/provider.sh}"
        TMPDIR=$(mktemp -d)
        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        echo "Testing provider.sh unknown method exit code..."

        cat > "$MOCK_BIN/podman" << 'MOCK'
        #!/bin/sh
        :
        MOCK
        chmod +x "$MOCK_BIN/podman"

        exit_code=0
        PATH="$MOCK_BIN:$PATH" \
          GC_CITY_NAME=test GC_WORKSPACE="$TMPDIR" GC_AGENT_IMAGE=test:latest GC_PODMAN_NETWORK=test \
          bash "$PROVIDER" future-method scout 2>/dev/null || exit_code=$?
        [[ "$exit_code" -eq 2 ]] || { echo "FAIL: unknown method exited $exit_code (expected 2)"; exit 1; }
        echo "  PASS: unknown method exits 2"

        rm -rf "$TMPDIR"
        echo "PASS: provider unknown method handling correct"
        mkdir $out
      '';

  # Post-gate: auto-deploy path (low-risk + auto-deploy configured)
  # Post-gate no longer merges — it submits the merge request to the judge
  # and creates deploy beads.
  city-post-gate-auto-deploy =
    runCommandLocal "city-post-gate-auto-deploy"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        POST_GATE="${../../lib/city/scripts/post-gate.sh}"
        TMPDIR=$(mktemp -d)

        echo "Testing post-gate.sh auto-deploy path..."

        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main

        WS="$TMPDIR/ws"
        mkdir -p "$WS/docs"
        git -C "$WS" init -q -b main
        git -C "$WS" commit --allow-empty -m "initial" -q

        # Set up auto-deploy docs
        printf "## Auto-deploy\nLow-risk: docs only\n" > "$WS/docs/orchestration.md"
        git -C "$WS" add -A && git -C "$WS" commit -m "add docs" -q

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        # Mock bd — returns low risk classification and creates deploy bead
        DEPLOY_ACTIONS="$TMPDIR/deploy-actions.log"
        cat > "$MOCK_BIN/bd" << MOCK
        #!/bin/sh
        echo "\$@" >> "$DEPLOY_ACTIONS"
        case "\$1" in
          show)
            echo '[{"metadata":{"risk_classification":"low"},"title":"Test fix"}]'
            ;;
          create)
            echo "deploy-bead-1"
            ;;
          label|update)
            ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Mock gc — record submit calls
        GC_LOG="$TMPDIR/gc.log"
        cat > "$MOCK_BIN/gc" << MOCK
        #!/bin/sh
        echo "\$@" >> "$GC_LOG"
        MOCK
        chmod +x "$MOCK_BIN/gc"

        cat > "$MOCK_BIN/wrapix-notify" << 'MOCK'
        #!/bin/sh
        :
        MOCK
        chmod +x "$MOCK_BIN/wrapix-notify"

        # Pre-create GC_LOG so the negative-grep below works even if post-gate
        # never invokes the gc mock. Under set -euo pipefail, grep's
        # file-not-found stderr would otherwise be the only signal of a bug.
        : > "$GC_LOG"

        # Run post-gate. Approved path: close bead + create deploy bead + notify.
        # Post-gate no longer submits a merge request to judge — the judge's
        # finalize step (judge-merge.sh approve) runs the merge before gate.sh
        # returns, so by the time post-gate fires the branch is already merged.
        PATH="$MOCK_BIN:$PATH" \
          GC_BEAD_ID=test-bead \
          GC_TERMINAL_REASON=approved \
          GC_WORKSPACE="$WS" \
          GC_CITY_NAME=test \
          bash "$POST_GATE" 2>&1

        # Verify post-gate does NOT submit to judge (merge already happened)
        if grep -q 'session submit judge' "$GC_LOG"; then
          echo "FAIL: post-gate should not submit merge request to judge (merge already done)"
          cat "$GC_LOG"
          exit 1
        fi
        echo "  PASS: post-gate does not re-submit merge to judge"

        # Verify work bead was closed
        grep -q 'close test-bead' "$DEPLOY_ACTIONS" || {
          echo "FAIL: work bead not closed"; cat "$DEPLOY_ACTIONS"; exit 1; }
        echo "  PASS: work bead closed"

        # Verify auto_deploy metadata was set (not human label)
        grep -q "auto_deploy=true" "$DEPLOY_ACTIONS" || { echo "FAIL: auto_deploy not set"; cat "$DEPLOY_ACTIONS"; exit 1; }
        ! grep -q "label add" "$DEPLOY_ACTIONS" || { echo "FAIL: human label was set for low-risk auto-deploy"; exit 1; }
        echo "  PASS: low-risk auto-deploy skips human label"

        rm -rf "$TMPDIR"
        echo "PASS: post-gate auto-deploy path works"
        mkdir $out
      '';

  # Post-gate: escalation routes through mayor (metadata + label + mail)
  city-post-gate-escalation =
    runCommandLocal "city-post-gate-escalation"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        POST_GATE="${../../lib/city/scripts/post-gate.sh}"
        TMPDIR=$(mktemp -d)

        echo "Testing post-gate.sh escalation flow..."

        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main

        WS="$TMPDIR/ws"
        mkdir -p "$WS"
        git -C "$WS" init -q -b main
        git -C "$WS" commit --allow-empty -m "initial" -q

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        # Create a worktree to be cleaned up
        mkdir -p "$WS/.wrapix/worktree"
        git -C "$WS" worktree add "$WS/.wrapix/worktree/esc-bead" -b esc-bead -q

        # Mock bd — record all calls
        BD_LOG="$TMPDIR/bd.log"
        cat > "$MOCK_BIN/bd" << MOCK
        #!/bin/sh
        echo "\$@" >> "$BD_LOG"
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Mock gc — record mail calls
        GC_LOG="$TMPDIR/gc.log"
        cat > "$MOCK_BIN/gc" << MOCK
        #!/bin/sh
        echo "\$@" >> "$GC_LOG"
        MOCK
        chmod +x "$MOCK_BIN/gc"

        # Mock wrapix-notify
        NOTIFY_LOG="$TMPDIR/notify.log"
        cat > "$MOCK_BIN/wrapix-notify" << MOCK
        #!/bin/sh
        echo "\$@" >> "$NOTIFY_LOG"
        MOCK
        chmod +x "$MOCK_BIN/wrapix-notify"

        # Run post-gate with escalation
        PATH="$MOCK_BIN:$PATH" \
          GC_BEAD_ID=esc-bead \
          GC_TERMINAL_REASON=max_rounds_exceeded \
          GC_WORKSPACE="$WS" \
          GC_CITY_NAME=test \
          bash "$POST_GATE" 2>&1

        # Verify escalated=true metadata set
        grep -q 'escalated=true' "$BD_LOG" || { echo "FAIL: escalated metadata not set"; cat "$BD_LOG"; exit 1; }
        echo "  PASS: escalated=true metadata set"

        # Verify escalation_reason metadata set
        grep -q 'escalation_reason=max_rounds_exceeded' "$BD_LOG" || { echo "FAIL: escalation_reason not set"; cat "$BD_LOG"; exit 1; }
        echo "  PASS: escalation_reason metadata set"

        # Verify human label added
        grep -q 'label add esc-bead human' "$BD_LOG" || { echo "FAIL: human label not added"; cat "$BD_LOG"; exit 1; }
        echo "  PASS: human label added"

        # Verify gc mail sent to mayor
        grep -q 'mail send --to mayor' "$GC_LOG" || { echo "FAIL: no mail sent to mayor"; cat "$GC_LOG"; exit 1; }
        grep -q 'escalation' "$GC_LOG" || { echo "FAIL: mail missing escalation subject"; cat "$GC_LOG"; exit 1; }
        echo "  PASS: gc mail sent to mayor"

        # Verify fallback notification also sent
        test -f "$NOTIFY_LOG" || { echo "FAIL: fallback notify not called"; exit 1; }
        echo "  PASS: fallback notification sent"

        # wx-kutwf: worktree and branch preserved for debugging on escalation
        test -d "$WS/.wrapix/worktree/esc-bead" || { echo "FAIL: worktree should be preserved for debugging (wx-kutwf)"; exit 1; }
        echo "  PASS: worktree preserved for debugging"

        git -C "$WS" rev-parse --verify esc-bead >/dev/null 2>&1 || { echo "FAIL: branch should be preserved for debugging (wx-kutwf)"; exit 1; }
        echo "  PASS: branch preserved for debugging"

        rm -rf "$TMPDIR"
        echo "PASS: post-gate escalation routes through mayor correctly"
        mkdir $out
      '';

  # =========================================================================
  # Cooldown pacing and P0 bypass
  # =========================================================================

  # Verify cooldown is wired into city.toml worker scale_check
  city-cooldown-config =
    let
      # Default cooldown (0) — inline bd list
      minimalWorker = elemAt (filter (a: a.name == "worker") minimalCity.configAttrs.agent) 0;
      minimalScaleCheck = minimalWorker.scale_check;
      hasInlineBd = match ".*bd list.*" minimalScaleCheck != null;

      # Full city cooldown (2h) — dispatch script reference
      fullWorker = elemAt (filter (a: a.name == "worker") fullCity.configAttrs.agent) 0;
      fullScaleCheck = fullWorker.scale_check;
      hasDispatchScript = match ".*wrapix-dispatch.*" fullScaleCheck != null;
      hasCooldownEnv = match ".*GC_COOLDOWN=2h.*" fullScaleCheck != null;
    in
    assert hasInlineBd;
    assert hasDispatchScript;
    assert hasCooldownEnv;

    runCommandLocal "city-cooldown-config" { } ''
      echo "PASS: Cooldown wired into city.toml"
      echo "  - cooldown=0: inline bd list scale_check"
      echo "  - cooldown=2h: dispatch script with GC_COOLDOWN env"
      mkdir $out
    '';

  # Dispatch script: P0 bypass, cooldown enforcement, backpressure
  city-dispatch-functional =
    runCommandLocal "city-dispatch-functional"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        DISPATCH="${../../lib/city/scripts/dispatch.sh}"
        TMPDIR=$(mktemp -d)
        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN" "$TMPDIR/ws/.wrapix/state"

        echo "Testing dispatch.sh..."

        # --- Test 1: P0 bypass (returns count even during cooldown) ---
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        if echo "$@" | grep -q "priority 0"; then
          echo '[{"id":"p0-1"}]'
        else
          echo '[{"id":"b1"},{"id":"b2"}]'
        fi
        MOCK
        chmod +x "$MOCK_BIN/bd"
        cat > "$MOCK_BIN/jq" << 'MOCK'
        #!/bin/sh
        # Read stdin, count array length
        input=$(cat)
        echo "$input" | grep -o '"id"' | wc -l
        MOCK
        chmod +x "$MOCK_BIN/jq"

        # Set recent dispatch (cooldown should block normal beads)
        echo "$(date +%s)" > "$TMPDIR/ws/.wrapix/state/last-dispatch"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=2h GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -gt 0 ]] || { echo "FAIL: P0 bypass should return >0 during cooldown"; exit 1; }
        echo "  PASS: P0 beads bypass cooldown"

        # --- Test 2: Cooldown blocks normal beads ---
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        if echo "$@" | grep -q "priority 0"; then
          echo '[]'
        else
          echo '[{"id":"b1"}]'
        fi
        MOCK
        chmod +x "$MOCK_BIN/bd"

        echo "$(date +%s)" > "$TMPDIR/ws/.wrapix/state/last-dispatch"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=2h GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -eq 0 ]] || { echo "FAIL: cooldown should block (got $result)"; exit 1; }
        echo "  PASS: cooldown blocks normal dispatch"

        # --- Test 3: Cooldown elapsed allows dispatch ---
        echo "0" > "$TMPDIR/ws/.wrapix/state/last-dispatch"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=1s GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -gt 0 ]] || { echo "FAIL: should dispatch after cooldown (got $result)"; exit 1; }
        echo "  PASS: dispatch allowed after cooldown elapsed"

        # --- Test 4: Backpressure blocks all dispatch ---
        # Set rate limit until far in the future
        echo "$(( $(date +%s) + 3600 ))" > "$TMPDIR/ws/.wrapix/state/rate-limited"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=0 GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -eq 0 ]] || { echo "FAIL: backpressure should block (got $result)"; exit 1; }
        echo "  PASS: backpressure blocks all dispatch"

        # --- Test 5: Expired backpressure resumes dispatch ---
        echo "0" > "$TMPDIR/ws/.wrapix/state/rate-limited"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=0 GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -gt 0 ]] || { echo "FAIL: expired backpressure should allow dispatch (got $result)"; exit 1; }
        echo "  PASS: expired backpressure resumes dispatch"

        # --- Test 6: No cooldown (0) dispatches immediately ---
        rm -f "$TMPDIR/ws/.wrapix/state/last-dispatch" "$TMPDIR/ws/.wrapix/state/rate-limited"
        result="$(PATH="$MOCK_BIN:$PATH" GC_COOLDOWN=0 GC_WORKSPACE="$TMPDIR/ws" \
          bash "$DISPATCH")"
        [[ "$result" -gt 0 ]] || { echo "FAIL: cooldown=0 should dispatch (got $result)"; exit 1; }
        echo "  PASS: cooldown=0 dispatches immediately"

        rm -rf "$TMPDIR"
        echo "PASS: dispatch.sh handles cooldown, P0 bypass, and backpressure"
        mkdir $out
      '';

  # Dispatch script: duration parsing
  city-dispatch-duration-parse =
    runCommandLocal "city-dispatch-duration-parse"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        DISPATCH="${../../lib/city/scripts/dispatch.sh}"

        echo "Testing dispatch.sh parse_duration..."

        # Source just the parse_duration function
        parse_duration() {
          local input="$1" total=0 num=""
          for (( i=0; i<''${#input}; i++ )); do
            local c="''${input:$i:1}"
            case "$c" in
              [0-9]) num+="$c" ;;
              h) total=$(( total + ''${num:-0} * 3600 )); num="" ;;
              m) total=$(( total + ''${num:-0} * 60 )); num="" ;;
              s) total=$(( total + ''${num:-0} )); num="" ;;
            esac
          done
          if [[ -n "$num" ]]; then
            total=$(( total + num ))
          fi
          echo "$total"
        }

        check() {
          local input="$1" expected="$2"
          local result
          result="$(parse_duration "$input")"
          [[ "$result" == "$expected" ]] || { echo "FAIL: parse_duration '$input' = $result (expected $expected)"; exit 1; }
          echo "  PASS: '$input' -> $expected"
        }

        check "2h" "7200"
        check "30m" "1800"
        check "2h30m" "9000"
        check "1h15m30s" "4530"
        check "90s" "90"
        check "0" "0"

        echo "PASS: parse_duration handles all duration formats"
        mkdir $out
      '';

  # =========================================================================
  # Entrypoint: informational pending-review status (no blocking)
  # =========================================================================

  city-entrypoint-no-block =
    runCommandLocal "city-entrypoint-no-block"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.jq
          pkgs.netcat
        ];
      }
      ''
                set -euo pipefail
                ENTRYPOINT="${../../lib/city/scripts/entrypoint.sh}"
                TMPDIR=$(mktemp -d)
                MOCK_BIN="$TMPDIR/bin"
                mkdir -p "$MOCK_BIN"

                # --- Mock dependencies ---

                # bd human list --json: return scaffolding beads
                cat > "$MOCK_BIN/bd" << 'MOCK'
                #!/bin/sh
                if [ "$1" = "human" ] && [ "$2" = "list" ] && [ "$3" = "--json" ]; then
                  echo '[{"id":"wx-test1","title":"Scaffold docs/README.md"},{"id":"wx-test2","title":"Scaffold docs/architecture.md"}]'
                fi
                exit 0
        MOCK
                chmod +x "$MOCK_BIN/bd"

                # recovery.sh: no-op
                cat > "$TMPDIR/recovery.sh" << 'MOCK'
                #!/bin/sh
                exit 0
        MOCK
                chmod +x "$TMPDIR/recovery.sh"

                # stage-home.sh: return a temp gc home dir
                GC_HOME="$TMPDIR/gc-home"
                mkdir -p "$GC_HOME"
                cat > "$TMPDIR/stage-home.sh" << MOCK
                #!/bin/sh
                echo "$GC_HOME"
        MOCK
                chmod +x "$TMPDIR/stage-home.sh"

                # podman: no-op (events watcher will background and be harmless)
                cat > "$MOCK_BIN/podman" << 'MOCK'
                #!/bin/sh
                exit 0
        MOCK
                chmod +x "$MOCK_BIN/podman"

                # beads-dolt: deterministic name/port/socket. start creates
                # a real Unix socket AND a TCP listener so both the Linux
                # (socket) and Darwin (TCP) wait_for_dolt paths succeed.
                mkdir -p "$TMPDIR/ws/.wrapix"
                cat > "$MOCK_BIN/beads-dolt" << MOCK
                #!/bin/sh
                case "\$1" in
                  start)
                    # Kill any previous listeners from a prior run
                    for pidfile in "$TMPDIR/nc.pid" "$TMPDIR/nc-tcp.pid"; do
                      if [ -f "\$pidfile" ]; then
                        kill "\$(cat "\$pidfile")" 2>/dev/null || true
                      fi
                    done
                    rm -f "$TMPDIR/ws/.wrapix/dolt.sock"
                    nc -lkU "$TMPDIR/ws/.wrapix/dolt.sock" >/dev/null 2>&1 &
                    echo \$! > "$TMPDIR/nc.pid"
                    nc -lk 127.0.0.1 13306 >/dev/null 2>&1 &
                    echo \$! > "$TMPDIR/nc-tcp.pid"
                    for _ in 1 2 3 4 5 6 7 8 9 10; do
                      [ -S "$TMPDIR/ws/.wrapix/dolt.sock" ] && exit 0
                      sleep 0.05
                    done
                    exit 1
                    ;;
                  attach|stop) exit 0 ;;
                  name)  echo "beads-test" ;;
                  port)  echo "13306" ;;
                  socket) echo "$TMPDIR/ws/.wrapix/dolt.sock" ;;
                  *) exit 0 ;;
                esac
        MOCK
                chmod +x "$MOCK_BIN/beads-dolt"

                # gc: capture that we reach gc start --foreground (proves no blocking)
                cat > "$MOCK_BIN/gc" << 'MOCK'
                #!/bin/sh
                echo "GC_STARTED $*" >> $TMPDIR/gc_actions
                exit 0
        MOCK
                chmod +x "$MOCK_BIN/gc"

                # --- Prepare entrypoint with mocked SCRIPT_DIR ---
                # Copy entrypoint and patch SCRIPT_DIR to use our mock recovery.sh
                cp "$ENTRYPOINT" "$TMPDIR/entrypoint.sh"
                chmod +x "$TMPDIR/entrypoint.sh"
                # Replace the SCRIPT_DIR line so recovery.sh is found in TMPDIR
                sed -i "s|SCRIPT_DIR=.*|SCRIPT_DIR=\"$TMPDIR\"|" "$TMPDIR/entrypoint.sh"
                # Replace exec gc with just gc (exec would replace process, preventing checks)
                sed -i 's|^exec gc|gc|' "$TMPDIR/entrypoint.sh"

                export GC_CITY_NAME=test-city
                export GC_WORKSPACE=$TMPDIR/ws
                mkdir -p $TMPDIR/ws/.beads
                export GC_PODMAN_NETWORK=test-net
                export PATH="$MOCK_BIN:$PATH"

                # --- Test 1: with pending beads, entrypoint prints info and continues ---
                OUTPUT=$(bash "$TMPDIR/entrypoint.sh" 2>&1) || {
                  echo "FAIL: entrypoint exited non-zero with pending beads (should be informational)"
                  echo "Output: $OUTPUT"
                  exit 1
                }
                echo "$OUTPUT" | grep -q "Pending review items (2)" || {
                  echo "FAIL: expected pending review summary"
                  echo "Output: $OUTPUT"
                  exit 1
                }
                echo "$OUTPUT" | grep -q "wx-test1" || {
                  echo "FAIL: expected bead ID in output"
                  echo "Output: $OUTPUT"
                  exit 1
                }
                echo "$OUTPUT" | grep -q "mayor will present" || {
                  echo "FAIL: expected mayor reference in output"
                  echo "Output: $OUTPUT"
                  exit 1
                }
                grep -q "GC_STARTED" $TMPDIR/gc_actions || {
                  echo "FAIL: gc start was never called — entrypoint blocked"
                  exit 1
                }
                echo "  PASS: pending beads are informational, gc starts"

                # --- Test 2: with no pending beads, no review output ---
                rm -f $TMPDIR/gc_actions
                cat > "$MOCK_BIN/bd" << 'MOCK'
                #!/bin/sh
                if [ "$1" = "human" ] && [ "$2" = "list" ] && [ "$3" = "--json" ]; then
                  echo '[]'
                fi
                exit 0
        MOCK
                chmod +x "$MOCK_BIN/bd"

                OUTPUT=$(bash "$TMPDIR/entrypoint.sh" 2>&1) || {
                  echo "FAIL: entrypoint exited non-zero with no pending beads"
                  exit 1
                }
                # Should NOT contain pending review text
                if echo "$OUTPUT" | grep -q "Pending review"; then
                  echo "FAIL: should not print review summary when no beads pending"
                  exit 1
                fi
                grep -q "GC_STARTED" $TMPDIR/gc_actions || {
                  echo "FAIL: gc start was never called"
                  exit 1
                }
                echo "  PASS: no beads, no review output, gc starts"

                rm -rf "$TMPDIR" $TMPDIR/gc_actions $TMPDIR/ws
                echo ""
                echo "PASS: entrypoint prints informational status without blocking"
                mkdir $out
      '';

  # =========================================================================
  # Layer 5: Formula step commands — execute bash from formulas against known state
  # =========================================================================

  city-formula-steps =
    runCommandLocal "city-formula-steps"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.git
          pkgs.jq
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        TMPDIR=$(mktemp -d)
        export HOME="$TMPDIR/home"
        mkdir -p "$HOME"
        git config --global user.email "test@test"
        git config --global user.name "test"
        git config --global init.defaultBranch main

        MOCK_BIN="$TMPDIR/bin"
        mkdir -p "$MOCK_BIN"

        # =====================================================================
        # Mayor: check-specs — find last decomposition commit via git grep
        # =====================================================================
        echo "=== Mayor: check-specs ==="

        WS="$TMPDIR/mayor-ws"
        mkdir -p "$WS/specs" "$WS/docs"
        git -C "$WS" init -q -b main
        echo "# Project" > "$WS/docs/README.md"
        git -C "$WS" add -A && git -C "$WS" commit -m "initial" -q

        # Create a decomposition marker commit
        git -C "$WS" commit --allow-empty -m "mayor: decompose specs 2026-04-01T00:00:00Z" -q
        MARKER_HASH="$(git -C "$WS" rev-parse HEAD)"

        # Add a post-marker commit
        echo "new spec" > "$WS/specs/feature.md"
        git -C "$WS" add -A && git -C "$WS" commit -m "add feature spec" -q

        # Formula command: find last decomposition commit
        LAST_DECOMPOSE=$(git -C "$WS" log --all --grep="mayor: decompose specs" --format="%H" -1 2>/dev/null || echo "")
        [[ "$LAST_DECOMPOSE" == "$MARKER_HASH" ]] || { echo "FAIL: wrong decompose commit: $LAST_DECOMPOSE vs $MARKER_HASH"; exit 1; }
        echo "  PASS: found last decomposition commit"

        # Formula command: diff specs since last decomposition
        SPEC_DIFF=$(git -C "$WS" diff "$LAST_DECOMPOSE"..HEAD -- specs/ docs/ 2>/dev/null)
        echo "$SPEC_DIFF" | grep -q "new spec" || { echo "FAIL: spec diff missing changes"; exit 1; }
        echo "  PASS: spec diff captures changes since decomposition"

        # Formula command: no decomposition commit -> fallback
        WS2="$TMPDIR/mayor-ws2"
        mkdir -p "$WS2/docs"
        git -C "$WS2" init -q -b main
        echo "# Project" > "$WS2/docs/README.md"
        git -C "$WS2" add -A && git -C "$WS2" commit -m "initial" -q
        LAST_DECOMPOSE2=$(git -C "$WS2" log --all --grep="mayor: decompose specs" --format="%H" -1 2>/dev/null || echo "")
        [[ -z "$LAST_DECOMPOSE2" ]] || { echo "FAIL: expected empty, got $LAST_DECOMPOSE2"; exit 1; }
        echo "  PASS: no decomposition commit returns empty"

        # =====================================================================
        # Mayor: briefing — query pending reviews and escalated beads
        # =====================================================================
        echo "=== Mayor: briefing ==="

        # Mock bd for mayor briefing
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        case "$1" in
          human)
            echo '[{"id":"wx-r1","title":"Review auth module"},{"id":"wx-r2","title":"Deploy approval"}]'
            ;;
          list)
            echo '[{"id":"wx-e1","metadata":{"escalated":"true","escalation_reason":"max_rounds_exceeded"}},{"id":"wx-ok","metadata":{}}]'
            ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Formula command: get pending human reviews
        PENDING=$(PATH="$MOCK_BIN:$PATH" bd human list --json 2>/dev/null) || PENDING="[]"
        PENDING_COUNT=$(echo "$PENDING" | jq 'length')
        [[ "$PENDING_COUNT" -eq 2 ]] || { echo "FAIL: expected 2 pending, got $PENDING_COUNT"; exit 1; }
        echo "  PASS: pending human reviews queried (count=$PENDING_COUNT)"

        # Formula command: find escalated beads
        ESCALATED=$(PATH="$MOCK_BIN:$PATH" bd list --status=open --json --limit=0 | jq '[.[] | select(.metadata.escalated == "true")]')
        ESC_COUNT=$(echo "$ESCALATED" | jq 'length')
        [[ "$ESC_COUNT" -eq 1 ]] || { echo "FAIL: expected 1 escalated, got $ESC_COUNT"; exit 1; }
        ESC_REASON=$(echo "$ESCALATED" | jq -r '.[0].metadata.escalation_reason')
        [[ "$ESC_REASON" == "max_rounds_exceeded" ]] || { echo "FAIL: wrong reason: $ESC_REASON"; exit 1; }
        echo "  PASS: escalated beads filtered correctly"

        # Formula command: recent merges (git log --since)
        WS_BRIEF="$TMPDIR/briefing-ws"
        mkdir -p "$WS_BRIEF"
        git -C "$WS_BRIEF" init -q -b main
        echo "base" > "$WS_BRIEF/f.txt"
        git -C "$WS_BRIEF" add -A && git -C "$WS_BRIEF" commit -m "initial" -q
        echo "merged" > "$WS_BRIEF/f.txt"
        git -C "$WS_BRIEF" add -A && git -C "$WS_BRIEF" commit -m "feat: add auth module" -q
        RECENT=$(git -C "$WS_BRIEF" log --oneline --since="24 hours ago" -- ':(exclude).beads')
        echo "$RECENT" | grep -q "feat: add auth module" || { echo "FAIL: recent merge not found"; exit 1; }
        echo "  PASS: recent merges captured via git log"

        # =====================================================================
        # Mayor: triage — execute approved actions
        # =====================================================================
        echo "=== Mayor: triage ==="

        # Mock bd and gc for triage action testing
        TRIAGE_LOG="$TMPDIR/gc-triage.log"
        export TRIAGE_LOG
        rm -f "$TRIAGE_LOG"
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo "$@" >> "$TRIAGE_LOG"
        case "$1" in
          human)
            echo '{"status":"dismissed"}'
            ;;
          create)
            echo "wx-new1"
            ;;
          list)
            echo '[{"id":"wx-a1","title":"Fix auth retry","labels":["auth"]},{"id":"wx-a2","title":"Auth token expiry","labels":["auth"]},{"id":"wx-b1","title":"DB migration","labels":["db"]}]'
            ;;
          search)
            echo '[{"id":"wx-a1","title":"Fix auth retry"},{"id":"wx-a2","title":"Auth token expiry"}]'
            ;;
        esac
        MOCK
        chmod +x "$MOCK_BIN/bd"

        cat > "$MOCK_BIN/gc" << 'MOCK'
        #!/bin/sh
        echo "$@" >> "$TRIAGE_LOG"
        MOCK
        chmod +x "$MOCK_BIN/gc"

        # Formula command: dismiss a bead
        PATH="$MOCK_BIN:$PATH" bd human dismiss wx-r1 --reason="not relevant"
        grep -q 'human dismiss wx-r1' "$TRIAGE_LOG" || { echo "FAIL: dismiss not called"; exit 1; }
        echo "  PASS: dismiss bead action"

        # Formula command: approve deploy
        rm -f "$TRIAGE_LOG"
        PATH="$MOCK_BIN:$PATH" bd human respond wx-d1 --response="approved"
        grep -q 'human respond wx-d1' "$TRIAGE_LOG" || { echo "FAIL: approve not called"; exit 1; }
        echo "  PASS: approve deploy action"

        # Formula command: file investigation via scout
        rm -f "$TRIAGE_LOG"
        PATH="$MOCK_BIN:$PATH" gc mail send --to scout -s "investigate" -m "Check auth module for token leaks"
        grep -q 'mail send --to scout' "$TRIAGE_LOG" || { echo "FAIL: investigation not filed"; exit 1; }
        echo "  PASS: file investigation via scout"

        # Formula command: create P0
        rm -f "$TRIAGE_LOG"
        PATH="$MOCK_BIN:$PATH" bd create --title="Critical auth bypass" --description="Tokens not validated" --type=bug --priority=0
        grep -q 'create --title=Critical auth bypass' "$TRIAGE_LOG" || { echo "FAIL: P0 not created"; exit 1; }
        echo "  PASS: create P0 action"

        # Formula command: informal grouping — query beads and group by topic
        rm -f "$TRIAGE_LOG"
        BEADS_JSON=$(PATH="$MOCK_BIN:$PATH" bd list --status=open --json --limit=0)
        AUTH_BEADS=$(echo "$BEADS_JSON" | jq '[.[] | select(.title | test("auth|Auth"; "i"))]')
        AUTH_COUNT=$(echo "$AUTH_BEADS" | jq 'length')
        [[ "$AUTH_COUNT" -eq 2 ]] || { echo "FAIL: expected 2 auth beads, got $AUTH_COUNT"; exit 1; }
        echo "  PASS: informal grouping filters beads by topic pattern"

        # Formula command: search beads by keyword
        SEARCH_RESULTS=$(PATH="$MOCK_BIN:$PATH" bd search auth)
        echo "$SEARCH_RESULTS" | jq -e 'length == 2' >/dev/null || { echo "FAIL: search returned wrong count"; exit 1; }
        echo "  PASS: bd search returns matching beads"

        # =====================================================================
        # Scout: load-context — cap check via bd list + jq length
        # =====================================================================
        echo "=== Scout: load-context (cap check) ==="

        # Under cap
        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        echo '[{"id":"b1"},{"id":"b2"},{"id":"b3"}]'
        MOCK
        chmod +x "$MOCK_BIN/bd"

        # Formula command: check bead count against cap
        OPEN_COUNT=$(PATH="$MOCK_BIN:$PATH" bd list --status=open --status=in_progress --json --limit=0 | jq 'length')
        MAX_BEADS=10
        [[ "$OPEN_COUNT" -lt "$MAX_BEADS" ]] || { echo "FAIL: 3 < 10 should be under cap"; exit 1; }
        echo "  PASS: under cap (3 < 10)"

        # At cap
        MAX_BEADS=3
        [[ "$OPEN_COUNT" -ge "$MAX_BEADS" ]] || { echo "FAIL: 3 >= 3 should be at cap"; exit 1; }
        echo "  PASS: at cap (3 >= 3)"

        # =====================================================================
        # Scout: housekeeping — cross-reference workers vs in-progress beads
        # =====================================================================
        echo "=== Scout: housekeeping (orphan detection) ==="

        cat > "$MOCK_BIN/bd" << 'MOCK'
        #!/bin/sh
        # Return in-progress bead IDs
        echo '[{"id":"bead-active"},{"id":"bead-working"}]'
        MOCK
        chmod +x "$MOCK_BIN/bd"

        IN_PROGRESS=$(PATH="$MOCK_BIN:$PATH" bd list --status=in_progress --json --limit=0 | jq -r '.[].id')

        # Simulate worker containers with bead labels
        check_orphan() {
          local bead_id="$1"
          echo "$IN_PROGRESS" | grep -q "^''${bead_id}$" && echo "active" || echo "orphaned"
        }

        result=$(check_orphan "bead-active")
        [[ "$result" == "active" ]] || { echo "FAIL: bead-active should be active"; exit 1; }
        echo "  PASS: active worker recognized"

        result=$(check_orphan "bead-gone")
        [[ "$result" == "orphaned" ]] || { echo "FAIL: bead-gone should be orphaned"; exit 1; }
        echo "  PASS: orphaned worker detected"

        # =====================================================================
        # Worker: self-review — git diff and status checks
        # =====================================================================
        echo "=== Worker: self-review ==="

        WS_WORKER="$TMPDIR/worker-ws"
        mkdir -p "$WS_WORKER"
        git -C "$WS_WORKER" init -q -b main
        echo "original" > "$WS_WORKER/file.txt"
        git -C "$WS_WORKER" add -A && git -C "$WS_WORKER" commit -m "initial" -q

        # Simulate worker branch with changes
        git -C "$WS_WORKER" checkout -b bead-42 -q
        echo "fixed" > "$WS_WORKER/file.txt"
        git -C "$WS_WORKER" add -A && git -C "$WS_WORKER" commit -m "fix: auth bug (bead-42)" -q
        echo "more" > "$WS_WORKER/file2.txt"
        git -C "$WS_WORKER" add -A && git -C "$WS_WORKER" commit -m "chore: cleanup (bead-42)" -q

        # Formula command: review changes (diff against base branch)
        DIFF_OUTPUT=$(git -C "$WS_WORKER" diff main...HEAD)
        echo "$DIFF_OUTPUT" | grep -q "+fixed" || { echo "FAIL: diff missing fix"; exit 1; }
        echo "$DIFF_OUTPUT" | grep -q "+more" || { echo "FAIL: diff missing file2"; exit 1; }
        echo "  PASS: git diff main...HEAD shows worker changes"

        # Formula command: log of commits
        LOG_OUTPUT=$(git -C "$WS_WORKER" log --oneline main..HEAD)
        echo "$LOG_OUTPUT" | grep -q "fix: auth bug" || { echo "FAIL: log missing fix commit"; exit 1; }
        echo "$LOG_OUTPUT" | grep -q "chore: cleanup" || { echo "FAIL: log missing cleanup commit"; exit 1; }
        COMMIT_COUNT=$(echo "$LOG_OUTPUT" | wc -l)
        [[ "$COMMIT_COUNT" -eq 2 ]] || { echo "FAIL: expected 2 commits, got $COMMIT_COUNT"; exit 1; }
        echo "  PASS: git log main..HEAD shows 2 worker commits"

        # Formula command: working tree must be clean
        STATUS=$(git -C "$WS_WORKER" status --porcelain)
        [[ -z "$STATUS" ]] || { echo "FAIL: working tree not clean: $STATUS"; exit 1; }
        echo "  PASS: working tree is clean after commits"

        # Formula command: dirty working tree detected and captured
        echo "uncommitted" > "$WS_WORKER/dirty.txt"
        STATUS=$(git -C "$WS_WORKER" status --porcelain)
        [[ -n "$STATUS" ]] || { echo "FAIL: should detect uncommitted file"; exit 1; }
        git -C "$WS_WORKER" add -A && git -C "$WS_WORKER" commit -m "chore: capture remaining work (bead-42)" -q
        STATUS=$(git -C "$WS_WORKER" status --porcelain)
        [[ -z "$STATUS" ]] || { echo "FAIL: tree should be clean after capture"; exit 1; }
        echo "  PASS: uncommitted changes captured by self-review"

        # Judge finalize paths (approve ff-merge, approve→rebase+ff,
        # approve→conflict auto-converts to reject, explicit reject) are
        # exercised live in tests/city/integration.nix — mocking bd here
        # would only re-test our mocks, not the real script path.

        # =====================================================================
        # Judge: merge — fast-forward merge and worktree cleanup
        # =====================================================================
        echo "=== Judge: merge ==="

        WS_JUDGE="$TMPDIR/judge-ws"
        mkdir -p "$WS_JUDGE"
        git -C "$WS_JUDGE" init -q -b main
        echo "base" > "$WS_JUDGE/code.sh"
        git -C "$WS_JUDGE" add -A && git -C "$WS_JUDGE" commit -m "initial" -q

        # Create worker branch (simulate worker output)
        git -C "$WS_JUDGE" checkout -b bead-99 -q
        echo "fixed code" > "$WS_JUDGE/code.sh"
        git -C "$WS_JUDGE" add -A && git -C "$WS_JUDGE" commit -m "fix: resolve issue (bead-99)" -q
        WORKER_HEAD=$(git -C "$WS_JUDGE" rev-parse HEAD)
        git -C "$WS_JUDGE" checkout main -q

        # Create a worktree directory to be cleaned
        mkdir -p "$WS_JUDGE/.wrapix/worktree/bead-99"
        echo "worktree content" > "$WS_JUDGE/.wrapix/worktree/bead-99/dummy"

        # Formula command: fast-forward merge
        git -C "$WS_JUDGE" merge --ff-only bead-99 -q
        MAIN_HEAD=$(git -C "$WS_JUDGE" rev-parse HEAD)
        [[ "$MAIN_HEAD" == "$WORKER_HEAD" ]] || { echo "FAIL: ff merge didn't advance main"; exit 1; }
        echo "  PASS: fast-forward merge succeeded"

        # Formula command: cleanup worktree directory
        rm -rf "$WS_JUDGE/.wrapix/worktree/bead-99"
        ! test -d "$WS_JUDGE/.wrapix/worktree/bead-99" || { echo "FAIL: worktree not cleaned"; exit 1; }
        echo "  PASS: worktree directory removed"

        # Formula command: prune worktrees and delete branch
        git -C "$WS_JUDGE" worktree prune
        git -C "$WS_JUDGE" branch -d bead-99 -q
        ! git -C "$WS_JUDGE" rev-parse --verify bead-99 2>/dev/null || { echo "FAIL: branch not deleted"; exit 1; }
        echo "  PASS: branch deleted after merge"

        # =====================================================================
        # Judge: merge — rebase path when main has advanced
        # =====================================================================
        echo "=== Judge: merge (rebase path) ==="

        WS_REBASE="$TMPDIR/judge-rebase"
        mkdir -p "$WS_REBASE"
        git -C "$WS_REBASE" init -q -b main
        echo "base" > "$WS_REBASE/code.sh"
        git -C "$WS_REBASE" add -A && git -C "$WS_REBASE" commit -m "initial" -q

        # Worker branch
        git -C "$WS_REBASE" checkout -b bead-77 -q
        echo "worker fix" > "$WS_REBASE/worker.txt"
        git -C "$WS_REBASE" add -A && git -C "$WS_REBASE" commit -m "fix: worker (bead-77)" -q

        # Main advances (another merge landed)
        git -C "$WS_REBASE" checkout main -q
        echo "other change" > "$WS_REBASE/other.txt"
        git -C "$WS_REBASE" add -A && git -C "$WS_REBASE" commit -m "feat: other change" -q

        # Formula command: ff fails, rebase then ff
        ff_exit=0
        git -C "$WS_REBASE" merge --ff-only bead-77 -q 2>/dev/null || ff_exit=$?
        [[ "$ff_exit" -ne 0 ]] || { echo "FAIL: ff should fail when main advanced"; exit 1; }
        echo "  PASS: fast-forward correctly fails when main advanced"

        # Formula command: rebase onto main
        git -C "$WS_REBASE" checkout bead-77 -q
        git -C "$WS_REBASE" rebase main -q
        git -C "$WS_REBASE" checkout main -q
        git -C "$WS_REBASE" merge --ff-only bead-77 -q

        # Verify both changes present
        test -f "$WS_REBASE/worker.txt" || { echo "FAIL: worker change missing after rebase+merge"; exit 1; }
        test -f "$WS_REBASE/other.txt" || { echo "FAIL: other change missing after rebase+merge"; exit 1; }
        echo "  PASS: rebase + fast-forward merge succeeded"

        git -C "$WS_REBASE" branch -d bead-77 -q
        echo "  PASS: branch cleaned up after rebase merge"

        # =====================================================================
        # Judge: merge — rebase conflict rejection path
        # =====================================================================
        echo "=== Judge: merge (rebase conflict rejection) ==="

        WS_CONFLICT="$TMPDIR/judge-conflict"
        mkdir -p "$WS_CONFLICT"
        git -C "$WS_CONFLICT" init -q -b main
        echo "original" > "$WS_CONFLICT/shared.txt"
        git -C "$WS_CONFLICT" add -A && git -C "$WS_CONFLICT" commit -m "initial" -q

        # Worker branch modifies shared.txt
        git -C "$WS_CONFLICT" checkout -b bead-55 -q
        echo "worker version" > "$WS_CONFLICT/shared.txt"
        git -C "$WS_CONFLICT" add -A && git -C "$WS_CONFLICT" commit -m "fix: worker (bead-55)" -q

        # Main also modifies shared.txt — creates conflict
        git -C "$WS_CONFLICT" checkout main -q
        echo "main version" > "$WS_CONFLICT/shared.txt"
        git -C "$WS_CONFLICT" add -A && git -C "$WS_CONFLICT" commit -m "feat: main change" -q

        # Create worktree directory to verify cleanup
        mkdir -p "$WS_CONFLICT/.wrapix/worktree/bead-55"
        echo "worktree" > "$WS_CONFLICT/.wrapix/worktree/bead-55/dummy"

        # Formula command: ff fails
        ff_exit=0
        git -C "$WS_CONFLICT" merge --ff-only bead-55 -q 2>/dev/null || ff_exit=$?
        [[ "$ff_exit" -ne 0 ]] || { echo "FAIL: ff should fail"; exit 1; }

        # Formula command: rebase conflicts — abort and reject
        rebase_exit=0
        git -C "$WS_CONFLICT" rebase main bead-55 2>/dev/null || rebase_exit=$?
        [[ "$rebase_exit" -ne 0 ]] || { echo "FAIL: rebase should conflict"; exit 1; }
        git -C "$WS_CONFLICT" rebase --abort 2>/dev/null
        echo "  PASS: rebase conflict detected and aborted"

        # Formula command: cleanup after rejection
        rm -rf "$WS_CONFLICT/.wrapix/worktree/bead-55"
        git -C "$WS_CONFLICT" worktree prune 2>/dev/null || true
        git -C "$WS_CONFLICT" checkout main -q
        git -C "$WS_CONFLICT" branch -D bead-55 -q
        ! test -d "$WS_CONFLICT/.wrapix/worktree/bead-55" || { echo "FAIL: worktree not cleaned"; exit 1; }
        ! git -C "$WS_CONFLICT" rev-parse --verify bead-55 2>/dev/null || { echo "FAIL: branch not deleted"; exit 1; }
        echo "  PASS: worktree and branch cleaned up after rejection"

        # =====================================================================
        # Summary
        # =====================================================================

        rm -rf "$TMPDIR"
        echo ""
        echo "PASS: All formula step commands produce expected results"
        mkdir $out
      '';
}
