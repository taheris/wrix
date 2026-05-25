# Unit tests for lib/util/toml.nix
{ pkgs }:

let
  inherit (pkgs) runCommandLocal;

  toTOML = import ../lib/util/toml.nix { inherit (pkgs) lib; };

  # Helper: assert TOML output matches expected string
  assertEq =
    name: actual: expected:
    if actual == expected then
      true
    else
      throw "toml-${name}: expected:\n${expected}\n\ngot:\n${actual}";

  # --- Test cases ---

  # Scalars at top level
  scalarResult = toTOML {
    title = "example";
    port = 8080;
    enabled = true;
    debug = false;
  };
  scalarExpected = ''
    debug = false
    enabled = true
    port = 8080
    title = "example"'';

  # String escaping
  escapeResult = toTOML {
    path = "C:\\Users\\test";
    msg = "say \"hello\"\nworld";
  };
  escapeExpected = ''
    msg = "say \"hello\"\nworld"
    path = "C:\\Users\\test"'';

  # Inline list of scalars
  listResult = toTOML {
    tags = [
      "a"
      "b"
      "c"
    ];
    ports = [
      80
      443
    ];
  };
  listExpected = ''
    ports = [80, 443]
    tags = ["a", "b", "c"]'';

  # Nested table
  tableResult = toTOML {
    workspace = {
      name = "dev";
      provider = "claude";
    };
  };
  tableExpected = ''

    [workspace]
    name = "dev"
    provider = "claude"'';

  # Deeply nested table
  deepResult = toTOML {
    session = {
      k8s = {
        namespace = "demo";
        cpu_limit = "2";
      };
    };
  };
  deepExpected = ''

    [session]

    [session.k8s]
    cpu_limit = "2"
    namespace = "demo"'';

  # Array of tables ([[section]])
  aotResult = toTOML {
    agent = [
      {
        name = "build";
        scope = "global";
      }
      {
        name = "test";
        scope = "global";
        max_active_sessions = 2;
      }
    ];
  };
  aotExpected = ''

    [[agent]]
    name = "build"
    scope = "global"
    [[agent]]
    max_active_sessions = 2
    name = "test"
    scope = "global"'';

  # Mixed: scalars + tables + array of tables
  mixedResult = toTOML {
    workspace = {
      name = "myproject";
      provider = "claude";
    };
    session = {
      provider = "exec:/nix/store/fake-provider";
    };
    beads = {
      provider = "bd";
    };
    agent = [
      {
        name = "build";
        scope = "global";
      }
      {
        name = "test";
        scope = "global";
      }
    ];
  };

  # Empty attrset produces empty string
  emptyResult = toTOML { };

in
{
  toml-scalars =
    assert assertEq "scalars" scalarResult scalarExpected;
    runCommandLocal "toml-scalars" { } ''
      echo "PASS: scalar values render correctly"
      mkdir $out
    '';

  toml-escaping =
    assert assertEq "escaping" escapeResult escapeExpected;
    runCommandLocal "toml-escaping" { } ''
      echo "PASS: string escaping works"
      mkdir $out
    '';

  toml-lists =
    assert assertEq "lists" listResult listExpected;
    runCommandLocal "toml-lists" { } ''
      echo "PASS: inline lists render correctly"
      mkdir $out
    '';

  toml-tables =
    assert assertEq "tables" tableResult tableExpected;
    runCommandLocal "toml-tables" { } ''
      echo "PASS: nested tables render correctly"
      mkdir $out
    '';

  toml-deep-tables =
    assert assertEq "deep-tables" deepResult deepExpected;
    runCommandLocal "toml-deep-tables" { } ''
      echo "PASS: deeply nested tables render correctly"
      mkdir $out
    '';

  toml-array-of-tables =
    assert assertEq "array-of-tables" aotResult aotExpected;
    runCommandLocal "toml-array-of-tables" { } ''
      echo "PASS: array of tables ([[section]]) renders correctly"
      mkdir $out
    '';

  toml-mixed =
    let
      # Verify it contains all expected sections
      has = s: builtins.match (".*" + s + ".*") mixedResult != null;
      hasWorkspace = has "workspace";
      hasSession = has "session";
      hasBeads = has "beads";
      hasAgent = has "agent";
      hasProvider = has "provider";
    in
    assert hasWorkspace;
    assert hasSession;
    assert hasBeads;
    assert hasAgent;
    assert hasProvider;
    runCommandLocal "toml-mixed" { } ''
      echo "PASS: mixed content (tables + array of tables) renders correctly"
      mkdir $out
    '';

  toml-empty =
    assert emptyResult == "";
    runCommandLocal "toml-empty" { } ''
      echo "PASS: empty attrset produces empty string"
      mkdir $out
    '';
}
