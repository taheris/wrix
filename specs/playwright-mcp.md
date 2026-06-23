# playwright-mcp

MCP server providing browser automation for AI-assisted frontend development and testing within wrix sandboxes.

## Problem Statement

AI agents building web frontends cannot see what they've built. They edit HTML/CSS/JS but have no way to verify the result visually — detecting misalignment, overflow, broken layouts, or rendering bugs requires actually looking at the page. The single-command Bash tool can run a dev server but cannot interact with a browser.

## Architecture

Wraps Microsoft's `@playwright/mcp` server to provide browser automation inside wrix sandboxes. The agent can navigate pages, take screenshots (returned as base64 PNG for direct visual interpretation), fill forms, click elements, and inspect accessibility trees — enabling a tight edit-code-check-browser iteration loop.

The server is registered in the main Claude session via `mkSandbox`'s `mcp` parameter (see `sandbox.md`); MCP servers compose orthogonally with workspace profiles. Container construction, isolation, and trust boundary belong to `sandbox.md`; this spec owns the Playwright server's wiring on top.

Load-bearing decisions:

- Wraps `@playwright/mcp` rather than reimplementing browser automation
- Uses `pkgs.playwright-mcp` from nixpkgs for offline operation — no `npx` at runtime
- Uses `pkgs.playwright-driver.browsers` for Chromium (Playwright-specific build at the exact expected revision; avoids upstream version skew)
- Chromium's internal sandbox is disabled (`--no-sandbox`) because the wrix container is the trust boundary (see *Chromium Sandbox Disabled* below)

## MCP Tools

Wrix does not define or freeze a Playwright tool whitelist. It starts the bundled `@playwright/mcp` server and exposes whatever tool set that package reports through `tools/list`. Categories for the primary use cases, using representative current tool names asserted by the smoke verifier:

| Category | Tools | Purpose |
|----------|-------|---------|
| Navigation | `browser_navigate`, `browser_navigate_back` | Page navigation |
| Interaction | `browser_click`, `browser_fill_form`, `browser_select_option`, `browser_hover` | User input simulation |
| Vision and content | `browser_take_screenshot`, `browser_snapshot` | Visual capture and accessibility tree inspection |
| Diagnostics | `browser_network_requests`, `browser_console_messages` | Request and console inspection |
| Browser session | `browser_tabs` | Browser tab state management |

## Configuration

Enabled per sandbox via `mkSandbox`'s `mcp` parameter:

```nix
mkSandbox {
  profile = profiles.rust;
  mcp.playwright = {
    viewport = { width = 1920; height = 1080; };  # optional
  };
}
```

User options:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `headless` | bool | `true` | Run browser in headless mode (headful requires X forwarding, unsupported in v1) |
| `viewport` | `{ width, height }` | `{ width = 1280; height = 720; }` | Default browser viewport size |
| `config` | attrset | `{ }` | Passthrough to `@playwright/mcp` JSON config (serialized to a temp file and passed via `--config`) |

Always set by the Nix expression (not user-facing, cannot be overridden):

- `--executable-path` — derived from `pkgs.playwright-driver.browsers`
- `--no-sandbox` — Chromium's internal sandbox is disabled (see below)
- `--disable-dev-shm-usage` — avoids `/dev/shm` size limits in rootless containers
- `--disable-gpu` — avoids GPU initialization failures in headless environments
- `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` — prevent runtime downloads
- `PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true` — bypass NixOS host validation

When the user supplies additional `launchOptions.args` via `config`, those args are appended to the automatic flags; the automatic flags themselves are non-overridable.

## Chromium Sandbox Disabled

The outer wrix container is the trust boundary — see `sandbox.md` and `specs/security.md` for the full posture. Chromium's own multi-layer sandbox (user namespaces, seccomp-bpf, setuid helper) is both redundant and non-functional inside the container:

- **Linux (rootless Podman)**: the kernel restricts nested user namespace creation. Chromium's sandbox cannot initialize without `CAP_SYS_ADMIN` or relaxed seccomp policy — granting either would weaken the container's own isolation.
- **macOS / Linux with `WRIX_MICROVM=1`**: each container runs in its own microVM. The VM boundary is strictly stronger than Chromium's process-level sandbox.

Passing `--no-sandbox` removes a redundant inner layer that cannot work, not protection that was previously in place.

## Platform Support

The container image is Linux (aarch64 or x86_64), so `pkgs.playwright-driver.browsers` always resolves to a Linux Chromium build regardless of host platform. All automatic Chromium flags apply identically across platforms; the differences (Podman vs Apple `container` CLI, rootless container vs microVM, `/dev/shm` sizing) belong to `sandbox.md`. `--disable-dev-shm-usage` is essential on Linux rootless containers (where `/dev/shm` is typically 64MB) and harmless under macOS / krun microVMs.

## Success Criteria

- MCP server starts, responds to `initialize` and `tools/list` with representative bundled tools, and runs fully offline (no network downloads at startup)
  [system](bash tests/mcp/playwright/smoke-test.sh)
- Screenshot returns base64 PNG when navigating to a local HTTP server
  [system](bash tests/mcp/playwright/screenshot-test.sh)
- The image built with `mcp.playwright = {}` contains the chromium binary in its store closure
  [system](bash tests/mcp/playwright/build-test.sh)
- Chromium executable path is derived from `pkgs.playwright-driver.browsers`, not from a hard-coded path or `npx`
  [system](bash tests/mcp/playwright/smoke-test.sh test_chromium_executable_path_derives_from_playwright_browsers)
- The automatic Chromium flags `--no-sandbox`, `--disable-dev-shm-usage`, and `--disable-gpu` are always passed through `launchOptions.args`
  [system](bash tests/mcp/playwright/smoke-test.sh test_mandatory_flags_are_non_overridable)
- The `headless`, `viewport`, and `config` user options reach the MCP server's serialized config file
  [system](bash tests/mcp/playwright/smoke-test.sh test_user_options_reach_serialized_config)
- The server definition exposes the MCP registry triple (`name`, `packages`, `mkServerConfig`) so `mkSandbox` can compose it like any other server
  [system](bash tests/mcp/playwright/smoke-test.sh test_registry_triple_shape)

## Requirements

### Functional

1. **MCP tool surface** — every tool the bundled `@playwright/mcp` exposes is registered; the spec does not maintain its own tool whitelist. The category table above is illustrative, not exhaustive, and the smoke verifier checks representative tools returned by the live server rather than a fixed upstream count.
2. **Offline operation** — `pkgs.playwright-mcp` and `pkgs.playwright-driver.browsers` bake the server and Chromium into the image. No `npx` or browser download at runtime.
3. **MCP opt-in via sandbox** — enabled per sandbox via `mcp.playwright = { … }`; composes with the workspace profile and other MCP servers without a `-playwright` profile variant.
4. **Configuration passthrough** — `headless`, `viewport`, and `config` options reach `@playwright/mcp`'s serialized JSON config.
5. **Non-overridable flags** — `--no-sandbox`, `--disable-dev-shm-usage`, `--disable-gpu` are always set on `browser.launchOptions.args`. User-supplied `launchOptions.args` are appended, not substituted.

### Non-Functional

1. **Image size cost** — enabling `mcp.playwright` adds ~400MB to the image (Chromium ~350MB, Node.js + MCP server ~50MB). Sandboxes without `mcp.playwright` are unaffected.
2. **Reproducibility** — Chromium revision pinned via `pkgs.playwright-driver.browsers`; no runtime downloads.
3. **Headless only in v1** — headful mode requires X forwarding and is not implemented.

## Out of Scope

- **Performance metrics** — not in the `@playwright/mcp` tool set; would require custom tooling.
- **Custom browser support (Firefox, WebKit)** — Chromium only for v1; `playwright-driver.browsers` ships all three, easy to add later.
- **Persistent browser profiles** — clean state per container launch is correct sandbox behavior.
- **Playwright test runner (`@playwright/test`)** — different tool; users add it to profile packages independently.
- **HAR recording / replay** — not built into `@playwright/mcp`; `browser_network_requests` covers common request inspection needs.
