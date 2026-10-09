// installed by herdr-crew; `herdr-crew pi-install` overwrites this file.
// HERDR_CREW_PI_EXTENSION=1
// Keeps a crew role's prompt with its pi conversation: a fresh role passes the prompt file with
// --herdr-crew-prompt, this extension saves the text in the session as a custom entry and appends
// it to the system prompt on every turn. herdr resumes pi with a plain `pi --session <file>`; the
// entry in that file brings the role back. Sessions without the entry are left alone.
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

  pi.on("session_start", async (event, ctx) => {
    role = stored(ctx.sessionManager.getEntries());
    if (role) {
      return;
    }
    const file = pi.getFlag("herdr-crew-prompt");
    if (typeof file === "string" && file) {
      const name = pi.getFlag("herdr-crew-role");
      const data = { version: 1, role: typeof name === "string" ? name : "", prompt: readFileSync(file, "utf8") };
      if (!valid(data)) {
        throw new Error(`herdr-crew: ${file} must hold a nonempty role prompt of at most ${MAX_PROMPT} bytes`);
      }
      role = data;
    } else if (event?.previousSessionFile) {
      // /new, /fork or /resume of an unmarked session inside a crew role keeps the role.
      role = fromFile(event.previousSessionFile);
    }
    if (role) {
      pi.appendEntry(ENTRY, role);
      if (role.role && !pi.getSessionName()) {
        pi.setSessionName(role.role);
      }
    }
  });

  pi.on("before_agent_start", (event) => {
    if (role) {
      return { systemPrompt: `${event.systemPrompt}\n\n${role.prompt}` };
    }
  });
}
