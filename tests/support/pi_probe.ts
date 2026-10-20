// Observability and a controlled busy tool for the isolated Pi integration. No network APIs.
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Type } from "typebox";

export default function (pi) {
  const dir = process.env.CREW_PI_EVIDENCE;
  let id;
  function record(event, data = {}) {
    if (id) appendFileSync(join(dir, `${id}.jsonl`), JSON.stringify({ event, ...data }) + "\n");
  }
  pi.registerTool({
    name: "crew_probe", label: "Crew test probe", description: "Test-only controlled busy wait.",
    parameters: Type.Object({ operation: Type.Literal("hold") }),
    async execute(_id, _params, signal) {
      writeFileSync(join(dir, "busy"), id);
      const deadline = Date.now() + 45000;
      while (!existsSync(join(dir, "release"))) {
        if (signal?.aborted || Date.now() > deadline) throw new Error("Test hold interrupted");
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      return { content: [{ type: "text", text: "HOLD_RELEASED" }] };
    },
  });
  // A fresh process starts without this tool. Pi 1.3 should restore its saved activation.
  pi.on("session_start", (_event, ctx) => {
    id = ctx.sessionManager.getSessionId();
    if (!ctx.sessionManager.getEntries().some((entry) => entry.type === "message")) {
      pi.setActiveTools(pi.getActiveTools().filter((name) => name !== "crew_probe"));
    }
    record("start", { cwd: ctx.cwd, file: ctx.sessionManager.getSessionFile(), tools: pi.getActiveTools() });
  });
  pi.on("before_agent_start", (event) => {
    if (event.prompt?.includes("PROBE_INITIAL")) {
      pi.setActiveTools([...new Set([...pi.getActiveTools(), "crew_probe"])]);
    }
    record("before", { messageType: event.messageType, systemPrompt: event.systemPrompt });
  });
  pi.on("tool_execution_start", (event) => record("tool_start", { name: event.toolName, args: event.args }));
  pi.on("tool_execution_end", (event) => record("tool_end", { name: event.toolName, result: event.result, isError: event.isError }));
  pi.on("agent_settled", (_event, ctx) => {
    record("settled", { tools: pi.getActiveTools(), idle: ctx.isIdle() });
  });
}
