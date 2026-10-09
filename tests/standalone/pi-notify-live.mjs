import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { connect, createServer as createTcpServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

async function waitFor(predicate, label) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error(`Timed out: ${label}`);
}

/** External OpenAI-compatible service; each request waits for the test to release it. */
async function startProvider(t) {
  const requests = [];
  const errors = [];
  const server = createHttpServer(async (request, response) => {
    try {
      assert.equal(request.method, "POST");
      assert.equal(request.url, "/v1/chat/completions");
      let body = "";
      for await (const chunk of request) body += chunk;
      const input = JSON.parse(body);
      assert.equal(input.stream, true);
      assert.equal(input.model, "attention");
      assert.ok(input.messages.some(message => message.role === "user"));
      requests.push({
        input,
        fail(status, message) {
          response.writeHead(status, { "Content-Type": "application/json" });
          response.end(JSON.stringify({ error: { message, type: "server_error" } }));
        },
        complete(text) {
          response.writeHead(200, { "Content-Type": "text/event-stream" });
          for (const [delta, finish_reason] of [[{ role: "assistant" }, null], [{ content: text }, null], [{}, "stop"]]) {
            response.write(`data: ${JSON.stringify({
              id: "chatcmpl-attention", object: "chat.completion.chunk", created: 1, model: input.model,
              choices: [{ index: 0, delta, finish_reason }],
            })}\n\n`);
          }
          response.end("data: [DONE]\n\n");
        },
      });
    } catch (error) {
      errors.push(error);
      response.writeHead(400);
      response.end(String(error));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise((resolve, reject) => {
    server.close(error => error ? reject(error) : resolve());
    server.closeAllConnections();
  }));
  return { requests, errors, baseUrl: `http://127.0.0.1:${server.address().port}/v1` };
}

/** Record actual client envelopes, optionally forwarding them to the production daemon. */
async function startCapture(t, port = 0, forwardEndpoint) {
  const records = [];
  const errors = [];
  const sockets = new Set();
  const forwards = [];
  const server = createTcpServer(socket => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", error => errors.push(error));
    socket.setEncoding("utf8");
    let buffer = "";
    socket.on("data", chunk => {
      buffer += chunk;
      let newline;
      while ((newline = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        try {
          records.push(JSON.parse(line));
          if (forwardEndpoint) {
            forwards.push(new Promise((resolve, reject) => {
              const [host, endpointPort] = forwardEndpoint.split(":");
              const downstream = connect({ host, port: Number(endpointPort) }, () => downstream.end(`${line}\n`));
              downstream.on("error", reject);
              downstream.on("close", resolve);
            }));
          }
        } catch (error) {
          errors.push(error);
        }
      }
    });
  });
  server.listen(port, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise((resolve, reject) => {
    for (const socket of sockets) socket.destroy();
    server.close(error => error ? reject(error) : resolve());
  }));
  return { records, errors, forwards, endpoint: `127.0.0.1:${server.address().port}` };
}

async function startPi(t, provider, endpoint, options = {}) {
  const root = mkdtempSync(join(tmpdir(), "wrix-pi-notify-"));
  const home = join(root, "home");
  const cwd = join(root, "workspace");
  const agentDir = join(home, ".pi/agent");
  mkdirSync(agentDir, { recursive: true });
  mkdirSync(cwd);
  cpSync(process.env.PI_TEST_EXTENSION_PACKAGE, agentDir, { recursive: true, dereference: true });
  const settings = JSON.parse(readFileSync(process.env.PI_TEST_SETTINGS, "utf8"));
  settings.retry = { enabled: true, maxRetries: 1, baseDelayMs: 10, provider: { maxRetries: 0 } };
  settings.compaction = { enabled: options.compaction ?? false, reserveTokens: 1024, keepRecentTokens: 100 };
  writeFileSync(join(agentDir, "settings.json"), JSON.stringify(settings));
  writeFileSync(join(agentDir, "auth.json"), "{}", { mode: 0o600 });
  writeFileSync(join(agentDir, "models.json"), JSON.stringify({ providers: { fixture: {
    baseUrl: provider.baseUrl, api: "openai-completions", apiKey: "fixture-key",
    models: [{ id: "attention", name: "Attention fixture", reasoning: false,
      input: ["text"], contextWindow: 128000, maxTokens: 1024,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
  } } }));
  const child = spawn(process.env.PI_TEST_BIN, ["--mode", "rpc", "--offline", "--no-session",
    "--model", "fixture/attention", "--no-skills", "--no-prompt-templates", "--no-themes", "--no-context-files"], {
    cwd, env: { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: agentDir,
      WRIX_NOTIFY_TCP: endpoint, WRIX_NOTIFY_VERBOSE: "0", WRIX_FOCUS_TARGET: process.env.WRIX_FOCUS_TARGET ?? "host:2.1",
      TMUX: "in-container-debug-pane", PI_SESSION_ID: "conversation:9.9", WRIX_EXECUTION_ID: "execution:9.9" },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const closed = once(child, "close");
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    await closed;
    rmSync(root, { recursive: true, force: true });
  });
  let stderr = "";
  let buffer = "";
  const events = [];
  const framingErrors = [];
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", chunk => { stderr += chunk; });
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", chunk => {
    buffer += chunk;
    let newline;
    while ((newline = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      try { events.push(JSON.parse(line)); } catch (error) { framingErrors.push(error); }
    }
  });
  let nextId = 0;
  return {
    events,
    stderr: () => stderr,
    count: type => events.filter(event => event.type === type).length,
    async command(type, fields = {}) {
      const id = `request-${nextId++}`;
      child.stdin.write(`${JSON.stringify({ id, type, ...fields })}\n`);
      await waitFor(() => events.some(event => event.id === id), `${type} response: ${stderr}`);
      const response = events.find(event => event.id === id);
      assert.equal(response.success, true, JSON.stringify(response));
      return response.data;
    },
    async finish() {
      child.stdin.end();
      await waitFor(() => child.exitCode !== null || child.signalCode !== null, "orderly Pi shutdown");
      assert.deepEqual(await closed, [0, null], stderr);
      assert.deepEqual(framingErrors, []);
      assert.equal(events.some(event => event.type === "extension_error"), false, JSON.stringify(events));
      assert.deepEqual(provider.errors, []);
    },
  };
}

async function assertIdleSuccess(pi, text) {
  const state = await pi.command("get_state");
  assert.equal(state.isStreaming, false);
  assert.equal(state.isCompacting, false);
  assert.equal(state.pendingMessageCount, 0);
  const { messages } = await pi.command("get_messages");
  const last = messages.findLast(message => message.role === "assistant");
  assert.equal(last.stopReason, "stop", JSON.stringify(last));
  assert.equal(last.content.map(block => block.text ?? "").join(""), text);
}

async function runQueuedRecovery(pi, provider, capture) {
  assert.equal((await pi.command("prompt", { message: "Recover, then finish queued work." })).disposition, "started");
  await waitFor(() => provider.requests.length === 1, "initial provider request");
  provider.requests[0].fail(503, "503 Service Unavailable");
  await waitFor(() => provider.requests.length === 2, "real Pi automatic recovery request");
  assert.equal(pi.count("agent_end"), 1);
  assert.equal(pi.events.find(event => event.type === "agent_end").willRetry, true);
  assert.equal(pi.count("auto_retry_start"), 1);
  assert.equal(pi.count("agent_settled"), 0);
  assert.equal(capture.records.length, 0, "intermediate agent_end notified before recovery");
  assert.equal((await pi.command("follow_up", { message: "queued attention probe" })).disposition, "queued");
  assert.ok(pi.events.some(event => event.type === "queue_update" && event.followUp.includes("queued attention probe")));
  provider.requests[1].complete("recovered");
  await waitFor(() => provider.requests.length === 3, "queued follow-up provider request");
  assert.ok(provider.requests[2].input.messages.some(message => message.role === "user" &&
    (typeof message.content === "string" ? message.content : message.content.map(block => block.text ?? "").join("")) === "queued attention probe"),
    JSON.stringify(provider.requests[2].input.messages));
  assert.equal(pi.count("agent_settled"), 0);
  assert.equal(capture.records.length, 0, "queued work notified before completion");
  provider.requests[2].complete("queued work finished");
  await waitFor(() => pi.count("agent_settled") === 1 && capture.records.length === 1, "final settled notification");
  assert.equal(pi.count("agent_end"), 2);
  assert.ok(pi.events.some(event => event.type === "auto_retry_end" && event.success));
  await assertIdleSuccess(pi, "queued work finished");
  assert.deepEqual(capture.records, [{ title: "Pi", message: "Waiting for input", sound: "", focus_target: process.env.WRIX_FOCUS_TARGET ?? "host:2.1" }]);
  assert.deepEqual(capture.errors, []);
}

async function focusWorker() {
  const cleanup = [];
  const t = { after: fn => cleanup.unshift(fn) };
  try {
    assert.ok(process.env.WRIX_FOCUS_TARGET, "launcher must supply registered focus target");
    const provider = await startProvider(t);
    const capture = await startCapture(t, 0, process.env.WRIX_NOTIFY_TEST_ENDPOINT);
    const pi = await startPi(t, provider, capture.endpoint);
    await runQueuedRecovery(pi, provider, capture);
    await pi.finish();
    await Promise.all(capture.forwards);
    writeFileSync(process.env.PI_TEST_FOCUS_CAPTURE, `${JSON.stringify(capture.records[0])}\n`);
  } finally {
    for (const fn of cleanup) await fn();
  }
}

if (process.argv[2] === "focus-worker") {
  await focusWorker();
} else {
  test("provider fixture conforms to packaged streaming and retry error contracts", async t => {
    const provider = await startProvider(t);
    const { complete } = await import(`${process.env.PI_TEST_PACKAGE}/node_modules/@earendil-works/pi-ai/dist/compat.js`);
    const model = { provider: "fixture", id: "attention", name: "Attention fixture", api: "openai-completions",
      baseUrl: provider.baseUrl, reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 1024,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } };
    const context = { messages: [{ role: "user", content: "probe", timestamp: 1 }] };
    const failed = complete(model, context, { apiKey: "fixture-key", retry: { maxRetries: 0 } });
    await waitFor(() => provider.requests.length === 1, "fixture failure conformance");
    provider.requests[0].fail(503, "503 Service Unavailable");
    const error = await failed;
    assert.equal(error.stopReason, "error");
    assert.match(error.errorMessage, /503/);
    const successful = complete(model, context, { apiKey: "fixture-key" });
    await waitFor(() => provider.requests.length === 2, "fixture streaming conformance");
    provider.requests[1].complete("provider complete");
    const result = await successful;
    assert.equal(result.stopReason, "stop");
    assert.deepEqual(result.content, [{ type: "text", text: "provider complete" }]);
    assert.deepEqual(provider.errors, []);
  });

  test("packaged Pi settled notifies once after real recovery and queued work", async t => {
    const provider = await startProvider(t);
    const capture = await startCapture(t);
    const pi = await startPi(t, provider, capture.endpoint);
    await runQueuedRecovery(pi, provider, capture);
    await pi.finish();
    assert.equal(capture.records.length, 1, "shutdown must not duplicate attention");
  });

  test("packaged Pi settled waits for native overflow compaction and recovery", async t => {
    const provider = await startProvider(t);
    const capture = await startCapture(t);
    const pi = await startPi(t, provider, capture.endpoint, { compaction: true });
    await pi.command("prompt", { message: "Establish earlier context. ".repeat(100) });
    await waitFor(() => provider.requests.length === 1, "warm-up request");
    provider.requests[0].complete("Earlier context established. ".repeat(100));
    await waitFor(() => pi.count("agent_settled") === 1 && capture.records.length === 1, "warm-up settlement");
    await pi.command("prompt", { message: "Recover this context overflow." });
    await waitFor(() => provider.requests.length === 2, "overflowing request");
    provider.requests[1].fail(400, "context_length_exceeded");
    await waitFor(() => provider.requests.length === 3, "native compaction provider request");
    assert.equal(pi.count("agent_end"), 2);
    assert.equal(pi.count("agent_settled"), 1);
    assert.equal(capture.records.length, 1, "overflow agent_end emitted premature attention");
    assert.ok(pi.events.some(event => event.type === "compaction_start" && event.reason === "overflow"));
    provider.requests[2].complete("## Goal\nRecover context and finish the request.\n## Next Steps\nFinish recovery.");
    await waitFor(() => provider.requests.length === 4, "post-compaction recovery request");
    assert.ok(pi.events.some(event => event.type === "compaction_end" && event.reason === "overflow" && event.willRetry && event.result));
    assert.equal(pi.count("agent_settled"), 1);
    assert.equal(capture.records.length, 1, "compaction notified before recovery completed");
    provider.requests[3].complete("overflow recovery completed");
    await waitFor(() => pi.count("agent_settled") === 2 && capture.records.length === 2, "recovery settlement");
    await assertIdleSuccess(pi, "overflow recovery completed");
    await pi.finish();
    assert.equal(pi.count("agent_end"), 3);
    assert.equal(capture.records.length, 2);
    assert.deepEqual(capture.errors, []);
  });

  test("unavailable transport is diagnosed without failing Pi or stopping later work", async t => {
    const reservation = createTcpServer();
    reservation.listen(0, "127.0.0.1");
    await once(reservation, "listening");
    const port = reservation.address().port;
    await new Promise(resolve => reservation.close(resolve));
    const provider = await startProvider(t);
    const pi = await startPi(t, provider, `127.0.0.1:${port}`);
    await pi.command("prompt", { message: "Finish despite unavailable notifications." });
    await waitFor(() => provider.requests.length === 1, "first work request");
    provider.requests[0].complete("first turn completed");
    await waitFor(() => pi.count("agent_settled") === 1, "best-effort settlement");
    await assertIdleSuccess(pi, "first turn completed");
    assert.match(pi.stderr(), /wrix-notify: TCP send failed/);
    const capture = await startCapture(t, port);
    await pi.command("prompt", { message: "Keep working after notification failure." });
    await waitFor(() => provider.requests.length === 2, "subsequent work request");
    provider.requests[1].complete("second turn completed");
    await waitFor(() => pi.count("agent_settled") === 2 && capture.records.length === 1, "subsequent successful notification");
    await assertIdleSuccess(pi, "second turn completed");
    await pi.finish();
    assert.equal(capture.records.length, 1);
    assert.equal(capture.records[0].title, "Pi");
    assert.deepEqual(capture.errors, []);
  });
}
