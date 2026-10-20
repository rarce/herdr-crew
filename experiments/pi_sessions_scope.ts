// Experimental adapter for pi-sessions 0.20.0. Never installed by pi-install.
// The caller loads the package explicitly and supplies a disposable roster.
import { readFileSync, realpathSync } from "node:fs";

function roster(ctx) {
  const file = process.env.CREW_PI_ROSTER;
  if (!file) throw new Error("CREW_PI_ROSTER is required");
  const data = JSON.parse(readFileSync(file, "utf8"));
  if (data.version !== 1 || typeof data.roles !== "object") {
    throw new Error("Invalid experimental crew roster");
  }
  const cwd = realpathSync(ctx.cwd);
  const self = ctx.sessionManager.getSessionId();
  const roles = Object.entries(data.roles).map(([role, entry]: [string, any]) => ({
    role, ...entry, cwd: realpathSync(entry.cwd),
  }));
  if (!roles.some((entry) => entry.id === self && entry.cwd === cwd)) {
    throw new Error("Current session is outside this crew roster");
  }
  return roles;
}

// Keep the package's public tool implementations and broker. Filter discovery and validate
// every send against both the explicit UUID roster and the live directory's checkout.
// This routing constraint is not a sandbox: another extension can use its own transport.
export function scopedPiSessions(pi, install) {
  let discover;
  async function reachable(ctx, args = []) {
    const roles = roster(ctx);
    if (!discover) throw new Error("pi-sessions discovery is unavailable");
    const result = await discover(...args.slice(0, 4), ctx);
    const sessions = result.details.sessions.flatMap((session) => {
      const entry = roles.find((role) => role.id === session.sessionId);
      if (!entry || !session.cwd) return [];
      try {
        if (realpathSync(session.cwd) !== entry.cwd) return [];
      } catch { return []; }
      return [{ ...session, title: entry.role }];
    });
    const details = { scope: "user", sessions };
    return {
      content: [{ type: "text", text: JSON.stringify(details) }],
      details, structuredContent: details,
    };
  }
  install(new Proxy(pi, {
    get(target, key) {
      if (key === "setActiveTools") {
        return (names) => target.setActiveTools(names.filter((name) => name !== "session_cancel"));
      }
      if (key !== "registerTool") return Reflect.get(target, key);
      return (tool) => {
        if (tool.name === "session_reachable") {
          discover = tool.execute;
          target.registerTool({ ...tool, execute: (...args) => reachable(args[4], args) });
        } else if (tool.name === "session_send_message") {
          target.registerTool({ ...tool, async execute(...args) {
            const params = args[1];
            const peers = await reachable(args[4], [args[0], {}, args[2], args[3]]);
            if (!peers.details.sessions.some((entry) => entry.sessionId === params.session)) {
              throw new Error("Recipient is outside this live crew roster");
            }
            return tool.execute(...args);
          } });
        } else {
          target.registerTool(tool);
        }
      };
    },
  }));
}
