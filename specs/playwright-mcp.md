# playwright-mcp

MCP server providing browser automation for AI-assisted frontend development and testing within wrapix sandboxes.

## Problem Statement

AI agents building web frontends cannot see what they've built. They edit HTML/CSS/JS but have no way to verify the result visually — detecting misalignment, overflow, broken layouts, or rendering bugs requires actually looking at the page. The single-command Bash tool can run a dev server but cannot interact with a browser.

## Overview

Wraps Microsoft's `@playwright/mcp` server (59 built-in tools) to provide browser automation inside wrapix sandboxes. The agent can navigate pages, take screenshots (returned as base64 PNG for direct visual interpretation), fill forms, click elements, and inspect accessibility trees — enabling a tight edit-code-check-browser iteration loop.

**Key design decisions:**

- Wraps `@playwright/mcp` — does not reimplement browser automation
- Uses `pkgs.playwright-mcp` from nixpkgs for offline operation (no `npx` at runtime)
- Uses `pkgs.playwright-driver.browsers` for Chromium (Playwright-specific build at the exact expected revision, avoids version skew)
- Chromium sandbox disabled (`--no-sandbox`) since the wrapix container is the security boundary
- Registered in main session when enabled — no prescribed subagent isolation

## Use Cases

1. **Visual verification** — Take screenshots after code changes to verify layout, alignment, and styling
2. **Frontend iteration** — Edit code, reload browser, screenshot, spot issues, fix, repeat
3. **Form/interaction testing** — Fill forms, click buttons, verify navigation flows work
4. **Responsive testing** — Resize viewport to check mobile/tablet/desktop layouts
5. **Accessibility inspection** — Use accessibility snapshots to verify semantic structure
6. **Error detection** — Capture console errors and failed network requests

## MCP Tools

All 59 tools from `@playwright/mcp` are available. Key tools for the primary use cases:

| Category | Tools | Purpose |
|----------|-------|---------|
| Navigation | `browser_navigate`, `browser_go_back`, `browser_go_forward` | Page navigation |
| Interaction | `browser_click`, `browser_fill`, `browser_select_option`, `browser_hover` | User input simulation |
| Vision | `browser_screenshot`, `browser_pdf_save` | Visual capture (base64 PNG) |
| Content | `browser_snapshot`, `browser_get_text` | Page structure, text extraction, and accessibility tree |
| Network | `browser_network_requests`, `browser_route` | Request monitoring and mocking |
| Tabs | `browser_tab_new`, `browser_tab_select`, `browser_tab_close` | Multi-page management |
| Console | `browser_console_messages` | JavaScript error detection |

## Configuration

### Image Size Impact

Enabling Playwright adds ~400MB to the container image (Chromium ~350MB, Node.js + MCP server ~50MB). This is opt-in — sandboxes without `mcp.playwright` are unaffected.

### MCP Opt-in

Enabled per-sandbox via the `mcp` parameter:

```nix
# Enable with defaults
mkSandbox {
  profile = profiles.rust;
  mcp.playwright = { };
}

# Custom viewport
mkSandbox {
  profile = profiles.rust;
  mcp.playwright = {
    viewport = { width = 1920; height = 1080; };
  };
}

# Combined with other MCP servers
mkSandbox {
  profile = profiles.rust;
  mcp.tmux = { };
  mcp.playwright = { };
}
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `headless` | bool | `true` | Run browser in headless mode (headful requires X forwarding, unsupported in v1) |
| `viewport` | `{ width, height }` | `{ width = 1280; height = 720; }` | Default browser viewport size |
| `config` | attrset | `{ }` | Passthrough to `@playwright/mcp` JSON config (serialized to a temp file and passed via `--config`) |

### Automatic Configuration (not user-facing)

These are always set by the Nix expression:

- `--executable-path` — derived from `playwright-driver.browsers` chromium path
- `--no-sandbox` — Chromium's internal sandbox is redundant inside a container (see Security Model)
- `--disable-dev-shm-usage` — avoids `/dev/shm` size limits in containers
- `--disable-gpu` — avoids GPU initialization failures in headless environments
- `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` — prevent runtime downloads
- `PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true` — bypass NixOS host validation

The automatic flags (`--no-sandbox`, `--disable-dev-shm-usage`, `--disable-gpu`) are always present in `browser.launchOptions.args`. When the user provides a `config` attrset with additional `launchOptions.args`, those args are appended to the automatic flags. The automatic flags cannot be overridden.

## Security Model

### Container as Trust Boundary

The MCP server runs inside the wrapix container. All browser processes execute within sandbox isolation — the same trust model as the Bash tool. No additional restrictions are needed beyond what the container provides.

### Chromium Sandbox Disabled

Chromium ships with its own multi-layer sandbox (user namespaces, seccomp-bpf, setuid helper) designed to isolate renderer processes on a regular desktop. Inside a wrapix container, this internal sandbox is both **redundant and non-functional**:

- **Linux (rootless Podman)**: The kernel restricts nested user namespace creation. Chromium's sandbox cannot initialize without `CAP_SYS_ADMIN` or relaxed seccomp policy — granting these would weaken the container's own isolation.
- **macOS (Apple containers)**: Each container runs in its own microVM via Virtualization.framework. The VM boundary is stronger than Chromium's process-level sandbox.

Disabling Chromium's sandbox (`--no-sandbox`) removes a redundant inner layer that cannot work, rather than weakening security. The outer isolation boundary — rootless container on Linux, microVM on macOS (and optionally on Linux with `WRAPIX_MICROVM=1`) — enforces the same protections at a higher level.

### No Privilege Escalation

- MCP server runs as the same unprivileged user as Claude Code
- Browser processes inherit sandbox constraints
- Filesystem access limited to `/workspace`

## Platform Support

All automatic Chromium flags (`--no-sandbox`, `--disable-dev-shm-usage`, `--disable-gpu`) are set on both platforms. Platform-specific notes:

### Linux (Podman)

- `/dev/shm` is typically 64MB in rootless containers; `--disable-dev-shm-usage` is essential here
- With `WRAPIX_MICROVM=1` (krun): runs inside a microVM, same flags apply

### macOS (Apple Containers)

- Each container runs in a Virtualization.framework microVM with a full Linux kernel — `/dev/shm` is not constrained, but `--disable-dev-shm-usage` is harmless
- The container image is Linux (aarch64), so `playwright-driver.browsers` provides the Linux Chromium build
- VirtioFS workspace mounting works normally — no special handling needed for Playwright

## Affected Files

| File | Action | Description |
|------|--------|-------------|
| `lib/mcp/playwright/default.nix` | Create | Server definition: name, packages, mkServerConfig |
| `lib/mcp/default.nix` | Edit | Add `playwright` to registry |
| `specs/playwright-mcp.md` | Create | This spec |
| `tests/mcp/playwright/smoke-test.sh` | Create | Smoke test: MCP server starts, responds to initialize |
| `tests/mcp/playwright/screenshot-test.sh` | Create | Screenshot test: navigate to page, capture base64 PNG |
| `tests/mcp/playwright/build-test.sh` | Create | Nix build test: image contains chromium + MCP server |

## Success Criteria

- [ ] MCP server starts and responds to initialize request with tool list
  [verify](tests/mcp/playwright/smoke-test.sh::test_mcp_initialize)
- [ ] Screenshot returns base64 PNG when navigating to a local HTTP server
  [verify](tests/mcp/playwright/screenshot-test.sh::test_screenshot_returns_png)
- [ ] Nix image builds with `mcp.playwright = {}` and contains chromium binary
  [verify](tests/mcp/playwright/build-test.sh::test_image_contains_chromium)
- [ ] Server runs fully offline (no network downloads at startup)
  [verify](tests/mcp/playwright/smoke-test.sh::test_offline_startup)
- [ ] Chromium path is correctly derived from playwright-driver.browsers
  [judge](tests/judges/playwright-mcp.sh::test_chromium_path_derivation)
- [ ] Container flags (--no-sandbox, --disable-dev-shm-usage, --disable-gpu) are always applied
  [judge](tests/judges/playwright-mcp.sh::test_container_flags)
- [ ] Configuration options (headless, viewport, config) are wired through to MCP server
  [judge](tests/judges/playwright-mcp.sh::test_config_passthrough)
- [ ] Server definition follows the MCP registry pattern (name, packages, mkServerConfig)
  [judge](tests/judges/playwright-mcp.sh::test_registry_pattern)

## Out of Scope

- **Performance metrics** — Not in `@playwright/mcp` tool set; would require custom tooling
- **Custom browser support (Firefox, WebKit)** — Chromium only for v1; `playwright-driver.browsers` ships all three, easy to add later
- **Persistent browser profiles** — Clean state per container launch is correct sandbox behavior
- **Playwright test runner (`@playwright/test`)** — Different tool; users can add it to profile packages independently
- **HAR recording/replay** — Not built into `@playwright/mcp`; the built-in `browser_network_requests` and `browser_route` tools cover common network debugging needs
