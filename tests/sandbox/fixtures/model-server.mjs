import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";

/** A loopback OpenAI chat-completions fixture that requests one tool, then stops. */
export async function startModelServer(toolCall) {
  const requests = [];
  const errors = [];
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.method, "POST");
      assert.equal(request.url, "/v1/chat/completions");
      let body = "";
      for await (const chunk of request) body += chunk;
      const input = JSON.parse(body);
      assert.equal(input.stream, true);
      requests.push(input);
      const completed = input.messages.some(message => message.role === "tool");
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const send = (delta, finish_reason = null) => response.write(`data: ${JSON.stringify({
        id: "chatcmpl-fixture", object: "chat.completion.chunk", created: 1, model: input.model,
        choices: [{ index: 0, delta, finish_reason }],
      })}\n\n`);
      send({ role: "assistant" });
      if (completed) {
        send({ content: "fixture complete" });
        send({}, "stop");
      } else {
        const args = JSON.stringify(toolCall.arguments);
        const midpoint = Math.floor(args.length / 2);
        send({ tool_calls: [{ index: 0, id: "call_fixture", type: "function",
          function: { name: toolCall.name, arguments: args.slice(0, midpoint) } }] });
        send({ tool_calls: [{ index: 0, function: { arguments: args.slice(midpoint) } }] });
        send({}, "tool_calls");
      }
      response.end("data: [DONE]\n\n");
    } catch (error) {
      errors.push(error);
      response.writeHead(400);
      response.end(String(error));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    requests,
    errors,
    close: () => new Promise((resolve, reject) => {
      server.close(error => error ? reject(error) : resolve());
      server.closeAllConnections();
    }),
  };
}
