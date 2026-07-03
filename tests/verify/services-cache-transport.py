import sys
from pathlib import Path


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition, message):
    if not condition:
        fail(message)


def section(text, start, end, label):
    try:
        start_index = text.index(start)
    except ValueError:
        fail(f"{label}: missing start marker {start!r}")
    try:
        end_index = text.index(end, start_index + len(start))
    except ValueError:
        fail(f"{label}: missing end marker {end!r}")
    return text[start_index:end_index]


def require_contains(label, text, needle):
    require(needle in text, f"{label}: missing {needle!r}")


def require_absent(label, text, needle):
    require(needle not in text, f"{label}: unexpected {needle!r}")


def main():
    root = Path(sys.argv[1])
    launcher = (root / "crates/wrix-sandbox/src/command/launch.rs").read_text()
    lifecycle = (root / "crates/wrix-service/src/lifecycle/mod.rs").read_text()
    service_image = (root / "lib/services/image.nix").read_text()

    cache_load = section(
        launcher,
        "    fn load_project_cache",
        "    fn load_dolt",
        "project-cache launcher path",
    )
    service_run = section(
        lifecycle,
        "    fn ensure_running",
        "    fn status",
        "service container run path",
    )
    container_command = section(
        lifecycle,
        "fn container_command",
        "fn expected_dolt_transport_label",
        "service container command",
    )
    image_contents = section(
        service_image,
        "  contents = [",
        "  ];",
        "service image contents",
    )

    require_contains(
        "project-cache launcher path",
        cache_load,
        'format!("http://{sandbox_host}:{port}")',
    )
    require_contains(
        "project-cache launcher path",
        cache_load,
        "extra-substituters = {url}",
    )
    require_contains(
        "project-cache launcher path",
        cache_load,
        "extra-trusted-public-keys = {public_key}",
    )
    require_contains(
        "project-cache launcher path",
        cache_load,
        "builders-use-substitutes = true",
    )
    require_contains(
        "service container run path",
        service_run,
        'format!("127.0.0.1:{port}:8080")',
    )
    require_contains(
        "service container run path",
        service_run,
        'format!("{}:/cache:ro", plan.paths().cache_root().display())',
    )
    require_contains(
        "service container command",
        container_command,
        "wrix-cache-serve /cache",
    )
    require_contains("service image contents", image_contents, "cacheServe")

    checked_regions = {
        "project-cache launcher path": cache_load,
        "service container run path": service_run,
        "service container command": container_command,
        "service image contents": image_contents,
    }
    forbidden = [
        "file://",
        "unix://",
        "nix-daemon",
        "/nix/var/nix/daemon-socket",
        ":/nix/store",
        "harmonia",
        "nix-serve",
    ]
    for label, text in checked_regions.items():
        for needle in forbidden:
            require_absent(label, text, needle)


if __name__ == "__main__":
    main()
