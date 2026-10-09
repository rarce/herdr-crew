// installed by herdr-crew; `herdr-crew pi-install` overwrites this file.
// HERDR_CREW_PI_EXTENSION=1
// Keeps a crew role's prompt with its pi conversation: a fresh role passes the prompt file with
// --herdr-crew-prompt, this extension saves the text in the session as a custom entry and appends
// it to the system prompt on every turn. herdr resumes pi with a plain `pi --session <file>`; the
// entry in that file brings the role back. /new and /fork carry the role; a resumed session
// without the entry is left alone. An unreadable prompt file stops pi instead of running bare.
// @ts-nocheck

import { readFileSync } from "node:fs";

const ENTRY = "herdr-crew-role";
const MAX_PROMPT = 64 * 1024;

function valid(data) {
  return (
    data?.version === 1 &&
    typeof data.role === "string" &&
    typeof data.prompt === "string" &&
    data.prompt.length > 0 &&
    Buffer.byteLength(data.prompt) <= MAX_PROMPT
  );
}

function stored(entries) {
  let found;
  for (const entry of entries ?? []) {
    if (entry?.type === "custom" && entry.customType === ENTRY && valid(entry.data)) {
      found = entry.data;
    }
  }
  return found;
}

function fromFile(file) {
  try {
    return stored(
      readFileSync(file, "utf8")
        .split("\n")
        .filter((line) => line.trim())
        .map((line) => {
          try {
            return JSON.parse(line);
          } catch {
            return undefined;
          }
        }),
    );
  } catch {
    return undefined;
  }
}

export default function (pi) {
  pi.registerFlag("herdr-crew-role", { description: "herdr-crew role name", type: "string" });
  pi.registerFlag("herdr-crew-prompt", { description: "herdr-crew role prompt file", type: "string" });

  let role;
  let broken;

  pi.on("session_start", async (event, ctx) => {
    role = stored(ctx.sessionManager.getEntries());
    broken = undefined;
    if (role) {
      return;
    }
    if (event?.reason === "new" || event?.reason === "fork") {
      // /new and /fork inside a role keep the role of the session they come from.
      role = event.previousSessionFile ? fromFile(event.previousSessionFile) : undefined;
    } else if (event?.reason === "startup" || event?.reason === "reload") {
      const file = pi.getFlag("herdr-crew-prompt");
      if (typeof file === "string" && file) {
        const name = pi.getFlag("herdr-crew-role");
        let prompt;
        try {
          prompt = readFileSync(file, "utf8");
        } catch {
          prompt = undefined;
        }
        const data = { version: 1, role: typeof name === "string" ? name : "", prompt };
        if (!valid(data)) {
          // pi logs handler errors and keeps going; a role must not run without its prompt.
          broken = `herdr-crew: ${file} must hold a nonempty role prompt of at most ${MAX_PROMPT} bytes`;
          console.error(broken);
          ctx.ui?.notify?.(broken, "error");
          process.exitCode = 1;
          ctx.shutdown();
          return;
        }
        role = data;
      }
    }
    // A resumed session without the entry (/resume of an unrelated session) is left alone.
    if (role) {
      pi.appendEntry(ENTRY, role);
      if (role.role && !pi.getSessionName()) {
        pi.setSessionName(role.role);
      }
    }
  });

  pi.on("input", () => {
    if (broken) {
      return { action: "handled" };
    }
  });

  pi.on("before_agent_start", (event, ctx) => {
    if (broken) {
      ctx.abort();
      return;
    }
    if (role) {
      return { systemPrompt: `${event.systemPrompt}\n\n${role.prompt}` };
    }
  });
}
