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


def main():
    path = Path(sys.argv[1])
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

    for duplicated in DUPLICATED_TARGETS:
        require(
            duplicated not in raw,
            f"loom.toml duplicates verifier target {duplicated}; use .#verify --list as the registry",
        )


if __name__ == "__main__":
    main()
