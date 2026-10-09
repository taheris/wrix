import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const { startModelServer } = await import(process.env.PI_TEST_MODEL_FIXTURE);
const { complete } = await import(`${process.env.PI_TEST_PACKAGE}/node_modules/@earendil-works/pi-ai/dist/compat.js`);
const call = {
  name: "codemode",
  arguments: { code: `
    await tools.write({ path: "probe.txt", content: "before" });
    await tools.edit({ path: "probe.txt", edits: [{ oldText: "before", newText: "after" }] });
    const read = await tools.read({ path: "probe.txt" });
    const shell = await tools.bash({ command: "printf shell-ok" });
    text({ read, output: shell.output, exitCode: shell.exit_code });
  ` },
};
const builtinNames = ["bash", "edit", "read", "write"];
const model = baseUrl => ({
  provider: "fixture", id: "tools", name: "Fixture model", api: "openai-completions", baseUrl,
  reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 1024,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
});

async function runPackagedRpc(t, settingsFile, server) {
  const root = mkdtempSync(join(tmpdir(), "wrix-pi-tools-"));
  const home = join(root, "home");
  const cwd = join(root, "workspace");
  const agentDir = join(home, ".pi/agent");
  mkdirSync(agentDir, { recursive: true });
  mkdirSync(cwd);
  copyFileSync(settingsFile, join(agentDir, "settings.json"));
  writeFileSync(join(agentDir, "auth.json"), "{}", { mode: 0o600 });
  writeFileSync(join(agentDir, "models.json"), JSON.stringify({ providers: { fixture: {
    baseUrl: server.baseUrl, api: "openai-completions", apiKey: "fixture-key",
    models: [model(server.baseUrl)],
  } } }));
  assert.equal(existsSync(join(agentDir, "mcp.json")), false);
  assert.equal(existsSync(join(cwd, ".pi/mcp.json")), false);
  const child = spawn(process.env.PI_TEST_BIN, ["--mode", "rpc", "--offline", "--no-session",
    "--model", "fixture/tools", "--no-skills", "--no-prompt-templates", "--no-themes", "--no-context-files"], {
    cwd, env: { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: agentDir },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const closed = once(child, "close");
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    await closed;
    rmSync(root, { recursive: true, force: true });
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", chunk => { stderr += chunk; });
  const events = [];
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Pi RPC timed out: ${stderr}`)), 30000);
    let buffer = "";
    let settled = false;
    child.once("error", error => { clearTimeout(timeout); reject(error); });
    child.once("close", code => {
      clearTimeout(timeout);
      if (settled && code === 0) resolve();
      else reject(new Error(`Pi RPC exited (${code}) before clean settlement: ${stderr}`));
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
          events.push(event);
          if (event.type === "agent_settled") {
            settled = true;
            child.stdin.end();
          }
        } catch (error) {
          clearTimeout(timeout);
          reject(error);
        }
      }
    });
    child.stdin.write(`${JSON.stringify({ id: "probe", type: "prompt", message: "Run the tool probe." })}\n`);
  });
  assert.equal(events.find(event => event.id === "probe")?.success, true, stderr);
  assert.deepEqual(server.errors, []);
  assert.equal(server.requests.length, 2, JSON.stringify(events));
  return { events, cwd, agentDir };
}

function declaredTools(server) {
  return server.requests[0].tools.map(tool => tool.function.name).sort();
}

function toolResult(events) {
  const event = events.find(event => event.type === "tool_execution_end" && event.toolName === "codemode");
  assert.ok(event, JSON.stringify(events));
  return event;
}

test("model fixture conforms to packaged provider streaming and tool-result contracts", async t => {
  const server = await startModelServer(call);
  t.after(server.close);
  const context = { messages: [{ role: "user", content: "probe", timestamp: 1 }], tools: [{
    name: "codemode", description: "Run a script", parameters: {
      type: "object", properties: { code: { type: "string" } }, required: ["code"],
    },
  }] };
  const options = { apiKey: "fixture-key" };
  const assistant = await complete(model(server.baseUrl), context, options);
  assert.equal(assistant.stopReason, "toolUse", JSON.stringify(assistant));
  assert.deepEqual(assistant.content, [{ type: "toolCall", id: "call_fixture", ...call }]);
  context.messages.push(assistant, { role: "toolResult", toolCallId: "call_fixture", toolName: "codemode",
    content: [{ type: "text", text: "probe output" }], isError: false, timestamp: 2 });
  const final = await complete(model(server.baseUrl), context, options);
  assert.equal(final.stopReason, "stop");
  assert.deepEqual(final.content, [{ type: "text", text: "fixture complete" }]);
  assert.deepEqual(server.errors, []);
  assert.equal(server.requests.length, 2);
  assert.deepEqual(declaredTools(server), ["codemode"]);
});

test("packaged Pi adds native codemode and executes built-in tools without MCP", async t => {
  const server = await startModelServer(call);
  t.after(server.close);
  const { events, cwd, agentDir } = await runPackagedRpc(t, process.env.PI_TEST_DEFAULT_SETTINGS, server);
  assert.deepEqual(declaredTools(server), [...builtinNames, "codemode"].sort());
  const result = toolResult(events);
  assert.equal(result.isError, false, JSON.stringify(result));
  const text = result.result.content.map(block => block.text ?? "").join("\n");
  assert.match(text, /Script completed/);
  assert.match(text, /"read":"after"/);
  assert.match(text, /"output":"shell-ok"/);
  assert.match(text, /"exitCode":0/);
  assert.equal(readFileSync(join(cwd, "probe.txt"), "utf8"), "after");
  const nested = events.filter(event => event.type === "tool_execution_end" && event.parentToolCallId);
  assert.deepEqual(nested.map(event => event.toolName).sort(), builtinNames);
  assert.ok(nested.every(event => !event.isError));
  assert.equal(readFileSync(join(agentDir, "auth.json"), "utf8"), "{}");
});

test("consumer settings disable codemode without removing direct built-in tools", async t => {
  const server = await startModelServer(call);
  t.after(server.close);
  const { events, cwd } = await runPackagedRpc(t, process.env.PI_TEST_DISABLED_SETTINGS, server);
  assert.deepEqual(declaredTools(server), builtinNames);
  const result = toolResult(events);
  assert.equal(result.isError, true, JSON.stringify(result));
  assert.equal(existsSync(join(cwd, "probe.txt")), false);
  assert.equal(events.some(event => event.parentToolCallId), false);
});
