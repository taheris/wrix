import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  pi.on("agent_settled", async () => {
    try {
      const result = await pi.exec("wrix-notify", ["Pi", "Waiting for input"], { timeout: 3000 });
      if (result.stderr) process.stderr.write(result.stderr);
      if (result.code !== 0 || result.killed) {
        process.stderr.write(`wrix-notify: attention delivery failed (exit ${result.code}, killed ${result.killed})\n`);
      }
    } catch (error) {
      process.stderr.write(`wrix-notify: attention delivery failed: ${String(error)}\n`);
    }
  });
}
