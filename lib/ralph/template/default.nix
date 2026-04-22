# Nix-native template definitions with static validation
#
# Templates are validated at Nix evaluation time:
# - All referenced partials must exist
# - All required variables must be provided during render
# - Partial markers {{> partial-name}} are resolved at render time
#
# Variable metadata (single source of truth):
# - Each variable has: source, required, default (optional), description
# - Shell scripts can read this via: nix eval --json .#ralph.variableDefinitions
{ lib }:

let
  inherit (builtins)
    all
    attrNames
    elem
    filter
    hasAttr
    listToAttrs
    map
    match
    readFile
    replaceStrings
    split
    stringLength
    toJSON
    ;

  inherit (lib)
    assertMsg
    concatStringsSep
    filterAttrs
    foldl'
    ;

  # ==========================================================================
  # Variable Definitions (Single Source of Truth)
  # ==========================================================================
  #
  # Each variable has:
  #   source: Where the value comes from
  #     - "args"     : CLI argument
  #     - "state"    : From current.json state file
  #     - "computed" : Derived from other values
  #     - "file"     : Content read from a file path
  #     - "beads"    : From beads issue data
  #     - "config"   : From ralph config
  #   required: Whether the variable must have a value for render
  #   default: Optional default value if not provided
  #   description: Human-readable description
  #   derivedFrom: For computed variables, what they're derived from
  #   filePath: For file variables, expression for the source path
  #
  variableDefinitions = {
    # --- Arguments (from CLI) ---
    LABEL = {
      source = "args";
      required = true;
      description = "Feature label (e.g., 'my-feature')";
    };

    # --- State (from current.json) ---
    MOLECULE_ID = {
      source = "state";
      required = false;
      description = "Molecule/epic ID from current.json";
    };

    # --- Computed (derived from other values) ---
    SPEC_PATH = {
      source = "computed";
      required = true;
      derivedFrom = "LABEL";
      description = "Path to spec file: specs/{LABEL}.md";
    };

    CURRENT_FILE = {
      source = "computed";
      required = false;
      description = "Path to current.json state file";
    };

    MOLECULE_PROGRESS = {
      source = "computed";
      required = false;
      derivedFrom = "MOLECULE_ID";
      description = "Progress string like '50% (5/10)' from beads status";
    };

    # --- File content (read from paths) ---
    SPEC_CONTENT = {
      source = "file";
      required = false;
      filePath = "SPEC_PATH";
      description = "Full content of the spec file";
    };

    EXISTING_SPEC = {
      source = "file";
      required = false;
      filePath = "SPEC_PATH";
      description = "Existing spec content (alias for SPEC_CONTENT in update mode)";
    };

    SPEC_DIFF = {
      source = "computed";
      required = false;
      description = "Output of git diff base_commit..HEAD for spec file (tier 1)";
    };

    EXISTING_TASKS = {
      source = "computed";
      required = false;
      description = "Formatted list of existing molecule tasks with status (tier 2/3)";
    };

    PINNED_CONTEXT = {
      source = "file";
      required = false;
      filePath = "config.pinnedContext";
      default = "";
      description = "Content from pinned context file (default docs/README.md)";
    };

    # --- Beads data (from issue) ---
    ISSUE_ID = {
      source = "beads";
      required = false;
      description = "Current beads issue ID";
    };

    TITLE = {
      source = "beads";
      required = false;
      description = "Issue title from beads";
    };

    DESCRIPTION = {
      source = "beads";
      required = false;
      description = "Issue description from beads";
    };

    # --- Companions (from state JSON) ---
    COMPANIONS = {
      source = "computed";
      required = false;
      default = "";
      description = "Rendered companion manifests from read_manifests";
    };

    # --- Config (from ralph config) ---
    EXIT_SIGNALS = {
      source = "config";
      required = false;
      default = "";
      description = "Exit signal definitions for templates";
    };

    README_INSTRUCTIONS = {
      source = "config";
      required = false;
      default = "";
      description = "Conditional README update instructions";
    };

    IMPLEMENTATION_NOTES = {
      source = "state";
      required = false;
      default = "";
      description = "Implementation hints from state file, formatted as markdown bullet list";
    };

    # --- Orchestration variables ---
    BEADS_SUMMARY = {
      source = "computed";
      required = false;
      description = "Titles and status of molecule's beads (for reviewer context)";
    };

    BASE_COMMIT = {
      source = "state";
      required = false;
      description = "SHA of the commit before implementation started";
    };

    PREVIOUS_FAILURE = {
      source = "computed";
      required = false;
      default = "";
      description = "Error output from a previous failed attempt, injected on retry";
    };
  };

  # Get list of all variable names
  allVariableNames = attrNames variableDefinitions;

  # Filter variables by source type
  variablesBySource =
    source: attrNames (filterAttrs (_name: def: def.source == source) variableDefinitions);

  # Get required variables (those with required = true)
  requiredVariables = attrNames (filterAttrs (_name: def: def.required or false) variableDefinitions);

  # ==========================================================================
  # Template Functions
  # ==========================================================================

  # Extract {{VAR}} variable names from content (excludes {{> partial}})
  extractVars =
    content:
    let
      parts = split "[{][{]([A-Z_]+)[}][}]" content;
      matches = filter (p: builtins.isList p) parts;
    in
    map builtins.head matches;

  # Extract partial names from content ({{> partial-name}})
  # Returns list of partial names referenced in the template
  extractPartialRefs =
    content:
    let
      # Split on {{> partial-name}} pattern
      # Nix regex uses POSIX extended regex, need to escape { and }
      parts = split "[{][{]> ([a-z-]+)[}][}]" content;
      # Filter to get only the matched groups (lists with the partial name)
      matches = filter (p: builtins.isList p) parts;
    in
    map builtins.head matches;

  # Resolve a single partial marker in content
  resolvePartial =
    partials: content: name:
    let
      partialContent = partials.${name} or (throw "Partial not found: ${name}");
      marker = "{{> ${name}}}";
    in
    replaceStrings [ marker ] [ partialContent ] content;

  # Resolve all partial markers in content
  resolvePartials =
    partials: content:
    let
      refs = extractPartialRefs content;
    in
    foldl' (resolvePartial partials) content refs;

  # Load partials from a directory as an attrset
  # Takes a list of partial file paths and returns { name = content; }
  loadPartials =
    partialPaths:
    listToAttrs (
      map (path: {
        # Extract name from path: ./partial/context-pinning.md -> context-pinning
        # Also handles Nix store paths: /nix/store/<hash>-context-pinning.md -> context-pinning
        name =
          let
            filename = baseNameOf path;
            # Remove .md extension
            # Nix store hashes are exactly 32 chars of base32 (a-z0-9) followed by hyphen
            # So we match: optional 32-char hash prefix, then the actual name
            nameMatch = match "([a-z0-9]{32}-)?(.+)\\.md" filename;
          in
          if nameMatch != null then
            # Get the second capture group (the actual name without hash)
            builtins.elemAt nameMatch 1
          else
            filename;
        value = readFile path;
      }) partialPaths
    );

  # Create a template with validation
  #
  # Arguments:
  #   body: Path to the template body file
  #   partials: List of paths to partial files (optional)
  #   variables: List of variable names required for rendering
  #
  # Returns an attrset with:
  #   content: Raw template content
  #   variables: List of required variables
  #   partials: Loaded partial contents
  #   render: Function to render template with variables
  #   validate: Function to check if variables are valid
  mkTemplate =
    {
      body,
      partials ? [ ],
      variables,
    }:
    let
      bodyContent = readFile body;
      loadedPartials = loadPartials partials;

      # Validate that all referenced partials exist
    in
    {
      inherit variables;
      content = bodyContent;
      partials = loadedPartials;

      # Validate that all required variables are present
      # Returns { valid: bool; missing: [string]; }
      validate =
        vars:
        let
          missing = filter (v: !(hasAttr v vars)) variables;
        in
        {
          valid = missing == [ ];
          inherit missing;
        };

      # Render template with provided variables
      # Throws if any required variables are missing
      render =
        vars:
        let

          # First resolve partials
          withPartials = resolvePartials loadedPartials bodyContent;

          # Then substitute variables
          varMarkers = map (v: "{{${v}}}") variables;
          varValues = map (v: vars.${v}) variables;
        in
        replaceStrings varMarkers varValues withPartials;
    };

  # Partial files
  partialDir = ./partial;
  partialFiles = [
    (partialDir + "/companions-context.md")
    (partialDir + "/context-pinning.md")
    (partialDir + "/exit-signals.md")
    (partialDir + "/interview-modes.md")
    (partialDir + "/spec-header.md")
  ];

  # Template definitions (in let block so validateTemplates can reference them)
  templates = {
    plan-new = mkTemplate {
      body = ./plan-new.md;
      partials = partialFiles;
      variables = [
        "PINNED_CONTEXT"
        "LABEL"
        "SPEC_PATH"
        "EXIT_SIGNALS"
        "README_INSTRUCTIONS"
      ];
    };

    plan-update = mkTemplate {
      body = ./plan-update.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "LABEL"
        "SPEC_PATH"
        "EXISTING_SPEC"
        "EXIT_SIGNALS"
      ];
    };

    todo-new = mkTemplate {
      body = ./todo-new.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "LABEL"
        "SPEC_PATH"
        "SPEC_CONTENT"
        "CURRENT_FILE"
        "EXIT_SIGNALS"
        "README_INSTRUCTIONS"
        "IMPLEMENTATION_NOTES"
      ];
    };

    todo-update = mkTemplate {
      body = ./todo-update.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "LABEL"
        "SPEC_PATH"
        "EXISTING_SPEC"
        "MOLECULE_ID"
        "MOLECULE_PROGRESS"
        "SPEC_DIFF"
        "EXISTING_TASKS"
        "EXIT_SIGNALS"
        "README_INSTRUCTIONS"
        "IMPLEMENTATION_NOTES"
      ];
    };

    run = mkTemplate {
      body = ./run.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "SPEC_PATH"
        "LABEL"
        "MOLECULE_ID"
        "ISSUE_ID"
        "TITLE"
        "DESCRIPTION"
        "EXIT_SIGNALS"
        "PREVIOUS_FAILURE"
      ];
    };

    check = mkTemplate {
      body = ./check.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "SPEC_PATH"
        "LABEL"
        "BEADS_SUMMARY"
        "BASE_COMMIT"
        "MOLECULE_ID"
        "EXIT_SIGNALS"
      ];
    };

    watch = mkTemplate {
      body = ./watch.md;
      partials = partialFiles;
      variables = [
        "COMPANIONS"
        "PINNED_CONTEXT"
        "SPEC_PATH"
        "LABEL"
        "MOLECULE_ID"
        "EXIT_SIGNALS"
      ];
    };
  };

  # Validate all templates (for use in flake check)
  # Returns true if all templates are valid, throws otherwise
  # Checks: partials exist, no undeclared variables in body or referenced partials
  validateTemplates =
    let
      templateNames = attrNames templates;
      checkTemplate =
        name:
        let
          t = templates.${name};
          # Force evaluation of the template (triggers partial validation)
          forceContent = t.content;
          # Check for undeclared variables in body and referenced partials
          bodyVars = extractVars t.content;
          referencedPartials = extractPartialRefs t.content;
          referencedPartialContents = map (ref: t.partials.${ref}) referencedPartials;
          partialVars = builtins.concatLists (map extractVars referencedPartialContents);
          allUsed = bodyVars ++ partialVars;
          undeclared = filter (v: !(elem v t.variables)) allUsed;
        in
        assert assertMsg (forceContent != null) "Template ${name} content is null";
        assert assertMsg (
          undeclared == [ ]
        ) "Template ${name} uses undeclared variables: ${concatStringsSep ", " undeclared}";
        true;
    in
    all checkTemplate templateNames;

  # Create a flake check derivation that validates templates
  # This runs as part of 'nix flake check' to catch template errors at build time
  #
  # Arguments:
  #   pkgs: nixpkgs set (for runCommandLocal)
  #
  # Validates:
  #   - All partials referenced in templates exist
  #   - All templates can be loaded without errors
  #   - Dry-run render with dummy values to catch placeholder typos
  mkTemplatesCheck =
    pkgs:
    pkgs.runCommandLocal "ralph-templates-check" { } ''
      set -e
      echo "Validating ralph templates..."

      # Force evaluation of validateTemplates (triggers all validation)
      ${
        if validateTemplates then
          ''
            echo "✓ All templates loaded successfully"
          ''
        else
          ''
            echo "✗ Template validation failed"
            exit 1
          ''
      }

      # Test dry-run rendering with dummy values for each template
      ${concatStringsSep "\n" (
        map (name: ''
          echo "  Checking ${name}..."
          ${
            let
              t = templates.${name};
              # Create dummy values for all variables
              dummyVars = listToAttrs (
                map (v: {
                  name = v;
                  value = "DUMMY_${v}";
                }) t.variables
              );
              # Force render to catch any placeholder typos
              rendered = t.render dummyVars;
              # Check that rendered content is non-empty
              isValid = stringLength rendered > 0;
            in
            if isValid then
              "echo '    ✓ ${name} renders correctly'"
            else
              ''
                echo '    ✗ ${name} failed to render'
                exit 1
              ''
          }
        '') (attrNames templates)
      )}

      echo ""
      echo "All ralph templates validated successfully"
      mkdir $out
    '';

in
{
  inherit
    mkTemplate
    loadPartials
    extractPartialRefs
    resolvePartials
    templates
    validateTemplates
    mkTemplatesCheck
    # Variable definitions (single source of truth)
    variableDefinitions
    allVariableNames
    variablesBySource
    requiredVariables
    ;

  # JSON export for shell scripts
  # Usage: nix eval --json .#ralph.variablesJson
  variablesJson = toJSON variableDefinitions;
}
