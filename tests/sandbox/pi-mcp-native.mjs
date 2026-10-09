import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const { startModelServer } = await import(process.env.PI_TEST_MODEL_FIXTURE);
const { McpClient, StdioTransport } = await import(`${process.env.PI_TEST_PACKAGE}/node_modules/@earendil-works/pi-mcp/dist/index.js`);
const imageData = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";
const tool = "mcp__alpha__wrix_test_echo";
const model = baseUrl => ({
  id: "tools", name: "Fixture model", api: "openai-completions", baseUrl,
  reasoning: false, input: ["text", "image"], contextWindow: 128000, maxTokens: 1024,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
});
const json = path => JSON.parse(readFileSync(path, "utf8"));
const log = path => existsSync(path) ? readFileSync(path, "utf8").trim().split("\n").filter(Boolean).map(JSON.parse) : [];

async function until(predicate, diagnostic = () => "condition timed out") {
  const deadline = Date.now() + 30000;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error(diagnostic());
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    if (process.platform === "linux") return readFileSync(`/proc/${pid}/stat`, "utf8").split(" ")[2] !== "Z";
    return true;
  } catch (error) {
    if (error.code === "ESRCH" || error.code === "ENOENT") return false;
    throw error;
  }
}

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "wrix-pi-mcp-"));
  for (const path of ["home/.pi/agent", "workspace/.pi", "host/.pi/agent"]) mkdirSync(join(root, path), { recursive: true });
  const hostConfig = join(root, "host/.pi/agent/mcp.json");
  writeFileSync(hostConfig, '{"mcpServers":{"host-only":{"command":"host-only"}}}\n');
  writeFileSync(join(root, "models.json"), "{}");
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, home: join(root, "home"), cwd: join(root, "workspace"),
    config: join(root, "home/.pi/agent/mcp.json"), hostConfig, available: join(root, "available.json"),
    models: join(root, "models.json"), logs: name => join(root, `${name}.jsonl`) };
}

function serverEntry(f, name, extraEnv = {}) {
  return { name, command: process.execPath, args: [process.env.PI_TEST_MCP_FIXTURE, "two words", ""],
    env: { WRIX_MCP_TEST_ENV: name, WRIX_MCP_TEST_LOG: f.logs(name), ...extraEnv } };
}

function available(f, runtime = true, names = ["alpha", "beta"], extraEnv = {}) {
  const servers = names.map(name => serverEntry(f, name, extraEnv));
  writeFileSync(f.available, JSON.stringify({ schema: 1, runtime_selection: runtime, servers }));
  return servers;
}

function environment(f, platform, selection, agent = "pi") {
  const env = { PATH: process.env.PATH, REPO_ROOT: process.env.REPO_ROOT,
    PI_TEST_ROOT: f.root, PI_TEST_PLATFORM: platform, PI_TEST_AGENT: agent,
    PI_TEST_AVAILABLE: f.available, PI_TEST_SETTINGS: process.env.PI_TEST_SETTINGS, PI_TEST_MODELS: f.models };
  if (selection !== undefined) env.WRIX_MCP = selection;
  return env;
}

async function rpc(t, f, platform, selection, code, { approve, abort = false } = {}) {
  const server = await startModelServer({ name: "codemode", arguments: { code } });
  t.after(server.close);
  writeFileSync(f.models, JSON.stringify({ providers: { fixture: {
    baseUrl: server.baseUrl, api: "openai-completions", apiKey: "fixture-key", models: [model(server.baseUrl)],
  } } }));
  writeFileSync(join(f.home, ".pi/agent/models.json"), readFileSync(f.models));
  const args = ["--offline", "--no-session", "--model", "fixture/tools", "--no-skills", "--no-prompt-templates", "--no-themes", "--no-context-files"];
  if (approve !== undefined) args.push(approve ? "--approve" : "--no-approve");
  const child = spawn(process.env.PI_TEST_BASH, [process.env.PI_TEST_ENTRYPOINT, ...args], {
    env: environment(f, platform, selection), stdio: ["pipe", "pipe", "pipe"],
  });
  const closed = once(child, "close");
  let stderr = "";
  let buffer = "";
  let parseError;
  const events = [];
  child.stderr.setEncoding("utf8").on("data", chunk => { stderr += chunk; });
  child.stdout.setEncoding("utf8").on("data", chunk => {
    buffer += chunk;
    let newline;
    while ((newline = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      try { events.push(JSON.parse(line)); } catch (error) { parseError = error; }
    }
  });
  const send = message => child.stdin.write(`${JSON.stringify(message)}\n`);
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) {
      child.stdin.end();
      await until(() => child.exitCode !== null || child.signalCode !== null, () => `Pi shutdown timeout: ${stderr}`);
    }
    await closed;
  });
  send({ id: "probe", type: "prompt", message: "Run the MCP probe." });
  if (abort) {
    await until(() => log(f.logs("alpha")).some(entry => entry.event === "tools/call"), () => stderr);
    send({ id: "abort", type: "abort" });
  }
  await until(() => {
    if (parseError) throw parseError;
    if (child.exitCode !== null) throw new Error(`Pi exited (${child.exitCode}): ${stderr}`);
    return events.some(event => event.type === "agent_settled") && (!abort || events.some(event => event.id === "abort"));
  }, () => `Pi settlement timeout: ${stderr}\n${JSON.stringify(events)}`);
  child.stdin.end();
  const [exitCode] = await closed;
  assert.equal(exitCode, 0, stderr);
  assert.equal(events.find(event => event.id === "probe")?.success, true, stderr);
  if (abort) assert.equal(events.find(event => event.id === "abort")?.success, true, stderr);
  assert.deepEqual(server.errors, []);
  if (!abort) assert.equal(server.requests.length, 2, JSON.stringify(events));
  const result = events.find(event => event.type === "tool_execution_end" && event.toolName === "codemode");
  assert.ok(result, JSON.stringify(events));
  return { result, events, requests: server.requests, stderr };
}

function output(run) {
  return run.result.result.content.filter(block => block.type === "text").map(block => block.text).join("\n");
}

function scriptValue(run) {
  assert.equal(run.result.isError, false, output(run));
  const text = output(run);
  assert.match(text, /^Script completed\nWall time [\d.]+ seconds\nOutput:\n/);
  return JSON.parse(text.slice(text.indexOf("Output:\n") + "Output:\n".length).trim());
}

function calls(f, name = "alpha") {
  return log(f.logs(name)).filter(entry => entry.event === "tools/call");
}

async function nativeClient(t, f, env = {}) {
  const entry = serverEntry(f, "alpha", env);
  const transport = new StdioTransport(entry);
  const client = new McpClient({ name: "fixture-conformance", version: "1" });
  t.after(() => client.close());
  await client.connect(transport);
  assert.equal(client.serverInfo.name, "wrix-test-mcp");
  assert.deepEqual((await client.listTools()).map(tool => tool.name), ["wrix_test_echo"]);
  return client;
}

for (const kind of ["structured", "large", "image", "tool-error", "protocol-error", "transport-error"]) {
  test(`MCP fixture conforms to native client ${kind} contract`, async t => {
    const f = fixture(t);
    const client = await nativeClient(t, f);
    const call = () => client.callTool("wrix_test_echo", { text: "tail", kind });
    if (kind === "protocol-error") await assert.rejects(call, /fixture protocol failure/);
    else if (kind === "transport-error") await assert.rejects(call, /closed/i);
    else {
      const result = await call();
      if (kind === "image") assert.deepEqual(result, { content: [{ type: "image", data: imageData, mimeType: "image/png" }] });
      else if (kind === "tool-error") assert.deepEqual(result, { content: [{ type: "text", text: "fixture tool failure" }], structuredContent: { failed: true }, isError: true });
      else {
        const payload = kind === "large" ? `${"x".repeat(80000)}:tail` : "tail";
        assert.deepEqual(result, { content: [{ type: "text", text: `alpha:${payload}` }],
          structuredContent: { env: "alpha", payload, nested: { values: [1, null, { ok: true }] } }, isError: false });
      }
    }
    assert.equal(calls(f).length, 1);
  });
}

test("MCP fixture conforms to native cancellation notifications", async t => {
  const f = fixture(t);
  const client = await nativeClient(t, f);
  const controller = new AbortController();
  const call = client.callTool("wrix_test_echo", { text: "cancel", kind: "slow" }, { signal: controller.signal });
  const rejected = assert.rejects(call, /abort|cancel/i);
  await until(() => calls(f).length === 1);
  controller.abort();
  await rejected;
  await until(() => log(f.logs("alpha")).some(entry => entry.event === "notifications/cancelled"));
  assert.equal(calls(f).length, 1);
});

for (const platform of ["linux", "darwin"]) {
  test(`${platform} explicit and runtime manifest handoff preserves all agent mappings`, async t => {
    let canonical;
    for (const runtime of [false, true]) {
      for (const agent of ["direct", "claude", "pi"]) {
        const f = fixture(t);
        const servers = available(f, runtime, ["alpha"]);
        const run = spawnSync(process.env.PI_TEST_BASH, [process.env.PI_TEST_ENTRYPOINT,
          ...(agent === "pi" ? ["--version"] : [process.env.PI_TEST_BASH, "-c", 'printf "%s" "$(<"$WRIX_MCP_MANIFEST")"'])], {
          env: environment(f, platform, runtime ? "alpha" : "ignored", agent), encoding: "utf8", timeout: 30000,
        });
        assert.equal(run.status, 0, run.stderr);
        const manifest = json(join(f.cwd, "selected-mcp.json"));
        assert.deepEqual(manifest, { schema: 1, servers });
        const mapping = { alpha: { command: servers[0].command, args: servers[0].args, env: servers[0].env } };
        if (agent === "claude") {
          assert.deepEqual(json(join(f.home, ".claude.json")).mcpServers, mapping);
          assert.equal(json(join(f.home, ".claude/settings.json")).mcpServers, undefined);
        } else if (agent === "pi") {
          assert.deepEqual(json(f.config), { mcpServers: { alpha: { ...mapping.alpha, exposure: "codemode" } } });
        }
        if (canonical === undefined) canonical = manifest.servers.map(entry => ({ ...entry, env: { WRIX_MCP_TEST_ENV: entry.env.WRIX_MCP_TEST_ENV } }));
        assert.deepEqual(manifest.servers.map(entry => ({ ...entry, env: { WRIX_MCP_TEST_ENV: entry.env.WRIX_MCP_TEST_ENV } })), canonical);
      }
    }
  });

  test(`${platform} packaged Pi selects native namespaces and regenerates stale config without host copyback`, async t => {
    const f = fixture(t);
    available(f);
    symlinkSync(f.hostConfig, f.config);
    const hostBefore = readFileSync(f.hostConfig, "utf8");
    const code = 'text((await searchTools("echo", {limit: 10})).map(tool => tool.name).sort());';
    for (const [selection, expected] of [[undefined, ["alpha", "beta"]], ["all", ["alpha", "beta"]], [" beta, beta ", ["beta"]], ["", []]]) {
      const before = Object.fromEntries(["alpha", "beta"].map(name => [name, log(f.logs(name)).length]));
      const run = await rpc(t, f, platform, selection, code);
      for (const name of ["alpha", "beta"]) {
        const starts = log(f.logs(name)).slice(before[name]).filter(entry => entry.event === "started");
        assert.equal(starts.length, expected.includes(name) ? 1 : 0);
        if (starts.length) {
          assert.deepEqual(starts[0].args, ["two words", ""]);
          assert.equal(starts[0].env, name);
        }
      }
      assert.deepEqual(scriptValue(run), expected.map(name => `mcp__${name}__wrix_test_echo`));
      assert.deepEqual(Object.keys(json(f.config).mcpServers).sort(), expected);
      assert.equal(statSync(f.config).mode & 0o777, 0o600);
      assert.equal(readFileSync(f.hostConfig, "utf8"), hostBefore);
      assert.equal(existsSync(join(f.cwd, ".pi/mcp.json")), false);
      const declared = run.requests[0].tools.map(tool => tool.function.name).sort();
      assert.deepEqual(declared, ["bash", "codemode", "edit", "read", "write"]);
    }
  });

  test(`${platform} unknown runtime server fails before packaged Pi executes`, async t => {
    const f = fixture(t);
    for (const names of [["alpha", "beta"], []]) {
      available(f, true, names);
      const run = spawnSync(process.env.PI_TEST_BASH, [process.env.PI_TEST_ENTRYPOINT, "--version"], {
        env: environment(f, platform, "unknown"), encoding: "utf8", timeout: 30000,
      });
      assert.equal(run.status, 1, run.stderr);
      assert.match(run.stderr, /WRIX_MCP selects unknown servers: unknown/);
      assert.equal(existsSync(f.config), false);
      assert.equal(log(f.logs("alpha")).length, 0);
    }
  });

  test(`${platform} packaged Pi independently honors trusted project same-name precedence`, async t => {
    const f = fixture(t);
    available(f);
    const projectEntry = serverEntry(f, "project");
    const config = { command: projectEntry.command, args: projectEntry.args, env: projectEntry.env };
    writeFileSync(join(f.cwd, ".pi/mcp.json"), JSON.stringify({ mcpServers: { alpha: config } }));
    const projectBefore = readFileSync(join(f.cwd, ".pi/mcp.json"), "utf8");
    for (const [selection, approve, expected] of [["alpha", true, "project"], ["alpha", false, "alpha"], ["", true, "project"]]) {
      const run = await rpc(t, f, platform, selection, `text({env:(await tools.${tool}({text:"override"})).structuredContent.env});`, { approve });
      assert.equal(scriptValue(run).env, expected);
      assert.deepEqual(Object.keys(json(f.config).mcpServers), selection ? ["alpha"] : []);
      assert.equal(readFileSync(join(f.cwd, ".pi/mcp.json"), "utf8"), projectBefore);
    }
  });
}

test("packaged Pi codemode preserves complete structured and large MCP envelopes", async t => {
  const f = fixture(t);
  available(f);
  const run = await rpc(t, f, "linux", "all", `
    const small = await tools.${tool}({text:"small", kind:"structured"});
    const large = await tools.${tool}({text:"tail", kind:"large"});
    const beta = await tools.mcp__beta__wrix_test_echo({text:"beta"});
    const namespace = await describeNamespace("mcp__alpha");
    text({ small, largeKeys: Object.keys(large).sort(), length: large.structuredContent.payload.length,
      complete: large.content[0].text === "alpha:" + large.structuredContent.payload,
      tail: large.structuredContent.payload.slice(-5), nested: large.structuredContent.nested,
      beta: beta.structuredContent.env, namespace });
  `);
  const value = scriptValue(run);
  assert.deepEqual(value.small, { content: [{ type: "text", text: "alpha:small" }],
    structuredContent: { env: "alpha", payload: "small", nested: { values: [1, null, { ok: true }] } }, isError: false });
  assert.deepEqual(value.largeKeys, Object.keys(value.small).sort());
  assert.equal(value.length, 80005);
  assert.equal(value.complete, true);
  assert.equal(value.tail, ":tail");
  assert.deepEqual(value.nested, value.small.structuredContent.nested);
  assert.equal(value.beta, "beta");
  assert.equal(value.namespace.name, "mcp__alpha");
  assert.match(value.namespace.instructions, /complete MCP result envelopes/);
  assert.deepEqual(value.namespace.tools, [tool]);
  assert.equal(calls(f).length, 2);
  assert.equal(calls(f, "beta").length, 1);
  assert.ok(run.events.filter(event => event.toolName === tool).every(event => event.parentToolCallId));
});

test("packaged Pi forwards native MCP images from codemode to RPC and model", async t => {
  const f = fixture(t);
  available(f);
  const run = await rpc(t, f, "linux", "alpha", `const result = await tools.${tool}({text:"image",kind:"image"}); image(result.content[0]);`);
  assert.equal(run.result.isError, false, output(run));
  assert.deepEqual(run.result.result.content.find(block => block.type === "image"), { type: "image", data: imageData, mimeType: "image/png" });
  assert.ok(run.requests[1].messages.some(message => Array.isArray(message.content) && message.content.some(block => block.type === "image_url" && block.image_url.url === `data:image/png;base64,${imageData}`)));
  assert.equal(calls(f).length, 1);
});

test("packaged Pi codemode preserves isError results without rejecting or replaying calls", async t => {
  const f = fixture(t);
  available(f);
  const run = await rpc(t, f, "linux", "alpha", `text(await tools.${tool}({text:"error",kind:"tool-error"}));`);
  assert.deepEqual(scriptValue(run), { content: [{ type: "text", text: "fixture tool failure" }], structuredContent: { failed: true }, isError: true });
  assert.equal(calls(f).length, 1);
});

for (const kind of ["protocol-error", "transport-error"]) {
  test(`packaged Pi propagates ${kind} without replaying a side-effecting MCP call`, async t => {
    const f = fixture(t);
    available(f);
    const run = await rpc(t, f, "linux", "alpha", `text(await tools.${tool}({text:"failure",kind:"${kind}"}));`);
    assert.equal(run.result.isError, true, output(run));
    assert.match(output(run), kind === "protocol-error" ? /fixture protocol failure/ : /closed/i);
    assert.equal(calls(f).length, 1);
  });
}

test("packaged Pi RPC cancellation reaches the native MCP server once", async t => {
  const f = fixture(t);
  available(f);
  const run = await rpc(t, f, "linux", "alpha", `text(await tools.${tool}({text:"cancel",kind:"slow"}));`, { abort: true });
  assert.equal(run.result.isError, true, output(run));
  assert.match(output(run), /abort|cancel/i);
  assert.equal(calls(f).length, 1);
  const notification = log(f.logs("alpha")).find(entry => entry.event === "notifications/cancelled");
  assert.ok(notification);
  assert.equal(notification.params.requestId, calls(f)[0].id);
});

for (const native of [false, true]) {
  test(`${native ? "packaged Pi RPC" : "MCP fixture conformance"} shutdown stops stdio server and spawned child`, async t => {
    const f = fixture(t);
    available(f, true, ["alpha"], { WRIX_MCP_TEST_CHILD: "1" });
    if (native) await rpc(t, f, "linux", "alpha", `text(await tools.${tool}({text:"shutdown"}));`);
    else {
      const client = await nativeClient(t, f, { WRIX_MCP_TEST_CHILD: "1" });
      await until(() => log(f.logs("alpha")).some(entry => entry.event === "child"));
      await client.close();
    }
    const entries = log(f.logs("alpha"));
    const pids = entries.filter(entry => ["started", "child"].includes(entry.event)).map(entry => entry.pid);
    assert.equal(pids.length, 2);
    await until(() => pids.every(pid => !alive(pid)), () => `MCP descendants survived shutdown: ${pids}`);
    assert.ok(entries.some(entry => entry.event === "stdin-end"));
    assert.ok(entries.some(entry => entry.event === "sigterm"));
  });
}
