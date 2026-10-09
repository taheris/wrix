import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { StringDecoder } from "node:string_decoder";

const decoder = new StringDecoder("utf8");
const imageData = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";
let buffer = "";
const pending = new Set();

function record(event) {
  if (process.env.WRIX_MCP_TEST_LOG) {
    appendFileSync(process.env.WRIX_MCP_TEST_LOG, `${JSON.stringify(event)}\n`);
  }
}

record({ event: "started", pid: process.pid, env: process.env.WRIX_MCP_TEST_ENV, args: process.argv.slice(2) });
if (process.env.WRIX_MCP_TEST_CHILD === "1") {
  const child = spawn(process.execPath, ["-e", `
    require('node:fs').appendFileSync(process.env.WRIX_MCP_TEST_LOG,
      JSON.stringify({event: 'child', pid: process.pid}) + '\\n');
    setInterval(() => {}, 1000);
  `], { stdio: "ignore" });
  child.on("error", error => { throw error; });
  process.on("SIGTERM", () => record({ event: "sigterm" }));
  setInterval(() => {}, 1000);
}

function respond(id, result) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
}

function receive(line) {
  const message = JSON.parse(line);
  record({ event: message.method, id: message.id, params: message.params });
  if (message.method === "initialize") {
    respond(message.id, {
      protocolVersion: message.params.protocolVersion,
      capabilities: { tools: {} },
      serverInfo: { name: "wrix-test-mcp", version: "1" },
      instructions: "Fixture tools return complete MCP result envelopes.",
    });
  } else if (message.method === "tools/list") {
    respond(message.id, { tools: [{
      name: "wrix_test_echo",
      description: "Return a deterministic MCP result or failure.",
      inputSchema: {
        type: "object",
        properties: {
          text: { type: "string" },
          kind: { type: "string", enum: ["echo", "structured", "large", "image", "tool-error", "protocol-error", "transport-error", "slow"] },
        },
        required: ["text"],
        additionalProperties: false,
      },
    }] });
  } else if (message.method === "tools/call") {
    const { text, kind = "echo" } = message.params.arguments;
    if (kind === "protocol-error") {
      process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id,
        error: { code: -32001, message: "fixture protocol failure" } })}\n`);
    } else if (kind === "transport-error") {
      process.exit(23);
    } else if (kind === "slow") {
      pending.add(message.id);
    } else if (kind === "image") {
      respond(message.id, { content: [{ type: "image", data: imageData, mimeType: "image/png" }] });
    } else if (kind === "tool-error") {
      respond(message.id, { content: [{ type: "text", text: "fixture tool failure" }],
        structuredContent: { failed: true }, isError: true });
    } else {
      const payload = kind === "large" ? `${"x".repeat(80000)}:${text}` : text;
      respond(message.id, {
        content: [{ type: "text", text: `${process.env.WRIX_MCP_TEST_ENV}:${payload}` }],
        structuredContent: { env: process.env.WRIX_MCP_TEST_ENV, payload, nested: { values: [1, null, { ok: true }] } },
        isError: false,
      });
    }
  } else if (message.method === "notifications/cancelled") {
    pending.delete(message.params.requestId);
  } else if (message.method === "ping") {
    respond(message.id, {});
  } else if (message.id !== undefined) {
    process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id,
      error: { code: -32601, message: "Unknown fixture method" } })}\n`);
  }
}

function readLines() {
  let newline;
  while ((newline = buffer.indexOf("\n")) !== -1) {
    const line = buffer.slice(0, newline).replace(/\r$/, "");
    buffer = buffer.slice(newline + 1);
    if (line.length > 0) receive(line);
  }
}

process.stdin.on("data", chunk => {
  buffer += decoder.write(chunk);
  readLines();
});
process.stdin.on("end", () => {
  buffer += decoder.end();
  readLines();
  record({ event: "stdin-end", pending: pending.size });
  if (buffer.length > 0) throw new Error("Incomplete JSONL fixture request");
});
