import re
import subprocess
import sys
import tomllib
from pathlib import Path


EXPECTED = {
    "match": r"^verify:(.+)$",
    "command": "nix run .#verify -- {targets}",
    "target": "{capture_1}",
    "join": " ",
    "parse": "json-lines",
    "cwd": ".",
}
DUPLICATED_TARGETS = (
    "verify:cli.package-surface",
    "verify:cli.shared-verifier-app",
    "verify:cli.verify-runner-batching",
)
SOURCE_ANNOTATION = re.compile(r"\[(?:test|judge)\??\]\(([^)]+)\)")
VERIFY_ANNOTATION = re.compile(r"\[(?:check|system)\]\((verify:[^)]+)\)")
TEST_RUNNER_COMMAND = "bash tests/loom-nextest-file-targets.sh '{paths}'"


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition, message):
    if not condition:
        fail(message)


def runner_entry(runner, tier):
    try:
        return runner[tier]["verify"]
    except KeyError:
        fail(f"missing [runner.{tier}.verify]")


def file_selector_error(root, spec, target):
    path_text = re.split(r"#|::", target, maxsplit=1)[0]
    if "/" not in path_text:
        return None
    if path_text.startswith("crates/"):
        return f"{spec.relative_to(root)} target {target!r} is workspace-relative; file targets must be spec-relative"
    resolved = spec.parent / path_text
    if not resolved.is_file():
        return f"{spec.relative_to(root)} target {target!r} resolves to missing file {resolved}"
    return None


def test_file_selectors(root):
    synthetic_spec = root / "specs/synthetic.md"
    require(
        file_selector_error(
            root,
            synthetic_spec,
            "crates/example/tests/sample.rs::test_name",
        )
        is not None,
        "workspace-relative synthetic file selector was not rejected",
    )
    for spec in sorted((root / "specs").glob("*.md")):
        for match in SOURCE_ANNOTATION.finditer(spec.read_text()):
            target = match.group(1)
            error = file_selector_error(root, spec, target)
            require(error is None, error)


def test_file_selector_adapter(root):
    adapter = root / "tests/loom-nextest-file-targets.sh"
    completed = subprocess.run(
        [
            str(adapter),
            "--print-filter",
            "../crates/example/tests/sample.rs::file_test | module::unit_test",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    require(
        completed.stdout.strip() == "test(file_test) + test(module::unit_test)",
        f"file selector adapter emitted unexpected filter {completed.stdout.strip()!r}",
    )


def logical_annotation_error(spec, target, inventory):
    if target in inventory:
        return None
    return f"{spec} annotation target {target!r} is absent from .#verify --list"


def test_logical_annotation_targets(root, inventory):
    require(
        logical_annotation_error(
            Path("specs/synthetic.md"), "verify:missing.target", inventory
        )
        is not None,
        "synthetic missing verifier target was not rejected",
    )
    for spec in sorted((root / "specs").glob("*.md")):
        for match in VERIFY_ANNOTATION.finditer(spec.read_text()):
            target = match.group(1)
            error = logical_annotation_error(spec.relative_to(root), target, inventory)
            require(error is None, error)


def test_pending_logical_annotation_targets_are_ignored():
    require(
        VERIFY_ANNOTATION.search("[check?](verify:missing.target)") is None,
        "pending logical verifier target was treated as non-pending",
    )


def main():
    path = Path(sys.argv[1])
    root = path.parent
    inventory = {line.strip() for line in sys.stdin if line.strip()}
    require(inventory, ".#verify --list inventory is empty")
    raw = path.read_text()
    config = tomllib.loads(raw)
    runner = config.get("runner", {})

    for tier in ("check", "system"):
        entry = runner_entry(runner, tier)
        for key, value in EXPECTED.items():
            require(
                entry.get(key) == value,
                f"[runner.{tier}.verify] {key} is {entry.get(key)!r}, expected {value!r}",
            )

    require(
        runner.get("test", {}).get("command") == TEST_RUNNER_COMMAND,
        f"[runner.test] command must be {TEST_RUNNER_COMMAND!r}",
    )
    test_file_selectors(root)
    test_file_selector_adapter(root)
    test_logical_annotation_targets(root, inventory)
    test_pending_logical_annotation_targets_are_ignored()

    for duplicated in DUPLICATED_TARGETS:
        require(
            duplicated not in raw,
            f"loom.toml duplicates verifier target {duplicated}; use .#verify --list as the registry",
        )


if __name__ == "__main__":
    main()
