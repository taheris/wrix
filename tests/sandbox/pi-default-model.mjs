import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [pi, settings] = process.argv.slice(2);
const root = mkdtempSync(join(tmpdir(), "wrix-pi-default-"));
const agentDir = join(root, ".pi/agent");
let child;
let closed;
try {
  mkdirSync(agentDir, { recursive: true });
  copyFileSync(settings, join(agentDir, "settings.json"));
  writeFileSync(join(agentDir, "auth.json"), JSON.stringify({
    "openai-codex": {
      type: "oauth", access: "fixture-access", refresh: "fixture-refresh",
      expires: Date.now() + 3600000, accountId: "fixture-account",
    },
  }), { mode: 0o600 });
  assert.equal(existsSync(join(agentDir, "models-store.json")), false);
  assert.equal(existsSync(join(agentDir, "models.json")), false);

  child = spawn(pi, ["--mode", "rpc", "--offline", "--no-session", "--no-extensions",
    "--no-skills", "--no-prompt-templates", "--no-themes", "--no-context-files"], {
    cwd: root,
    env: { PATH: process.env.PATH, HOME: root, PI_CODING_AGENT_DIR: agentDir },
    stdio: ["pipe", "pipe", "pipe"],
  });
  closed = once(child, "close");
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", chunk => { stderr += chunk; });
  const response = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Pi startup timed out: ${stderr}`)), 30000);
    let buffer = "";
    child.once("error", error => { clearTimeout(timeout); reject(error); });
    child.once("close", (code, signal) => {
      clearTimeout(timeout);
      reject(new Error(`Pi exited before returning state (${code}, ${signal}): ${stderr}`));
    });
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", chunk => {
      buffer += chunk;
      let newline;
      while ((newline = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        try {
          const event = JSON.parse(line);
          if (event.id === "startup-state") {
            clearTimeout(timeout);
            resolve(event);
          }
        } catch (error) {
          clearTimeout(timeout);
          reject(error);
        }
      }
    });
  });
  child.stdin.write('{"id":"startup-state","type":"get_state"}\n');
  const state = await response;
  assert.equal(state.success, true, JSON.stringify(state));
  assert.equal(state.data.model?.provider, "openai-codex");
  assert.equal(state.data.model?.id, "gpt-6.1-sol", "fresh Pi must not fall back to another model");
  assert.equal(state.data.model.api, "openai-codex-responses");
  assert.equal(state.data.model.baseUrl, "https://chatgpt.com/backend-api");
  assert.equal(state.data.thinkingLevel, "xhigh");
  console.log("PASS: fresh offline Pi selects openai-codex/gpt-6.1-sol with xhigh reasoning");
} finally {
  if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  if (closed) await closed;
  rmSync(root, { recursive: true, force: true });
}
