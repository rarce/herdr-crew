import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { scopedPiSessions } from "../../experiments/pi_sessions_scope.ts";

const root = mkdtempSync(join(tmpdir(), "crew-pi-scope-"));
const previous = process.env.CREW_PI_ROSTER;
try {
  const checkout = join(root, "repo"), foreign = join(root, "foreign");
  mkdirSync(checkout);
  mkdirSync(foreign);
  process.env.CREW_PI_ROSTER = join(root, "roster.json");
  const roster = { version: 1, roles: {
    lead: { id: "lead-uuid", cwd: checkout }, dev: { id: "dev-uuid", cwd: checkout },
  } };
  const save = () => writeFileSync(process.env.CREW_PI_ROSTER, JSON.stringify(roster));
  save();
  const definitions = new Map();
  let active = [], sends = 0;
  let sessions = [
    { sessionId: "dev-uuid", cwd: checkout },
    { sessionId: "foreign-uuid", cwd: foreign },
  ];
  const pi = {
    registerTool: (tool) => definitions.set(tool.name, tool),
    setActiveTools: (names) => { active = names; },
  };
  scopedPiSessions(pi, (api) => {
    api.registerTool({ name: "session_reachable", execute: async () => ({ details: { sessions } }) });
    api.registerTool({ name: "session_send_message", execute: async () => { sends++; return { delivered: true }; } });
    api.setActiveTools(["read", "session_reachable", "session_send_message", "session_cancel"]);
  });
  assert(!active.includes("session_cancel"));
  const ctx = { cwd: checkout, sessionManager: { getSessionId: () => "lead-uuid" } };
  const list = () => definitions.get("session_reachable").execute("call", {}, undefined, undefined, ctx);
  const send = (id) => definitions.get("session_send_message").execute("call", { session: id }, undefined, undefined, ctx);
  const found = await list();
  assert.deepEqual(found.details.sessions, [{ sessionId: "dev-uuid", cwd: checkout, title: "dev" }]);
  assert.deepEqual(JSON.parse(found.content[0].text), found.details);
  await send("dev-uuid");
  assert.equal(sends, 1);
  await assert.rejects(send("foreign-uuid"), /outside this live crew roster/);
  // A previously discovered peer must be revalidated on each send.
  sessions = [{ sessionId: "dev-uuid", cwd: foreign }];
  await assert.rejects(send("dev-uuid"), /outside this live crew roster/);
  sessions = [];
  await assert.rejects(send("dev-uuid"), /outside this live crew roster/);
  // A stale caller binding is rejected too, even if its checkout still matches.
  roster.roles.lead.id = "replacement-uuid";
  save();
  await assert.rejects(list(), /Current session is outside/);
  assert.equal(sends, 1);
  console.log("Verified scoped discovery, live UUID/checkout checks, stale callers and blocked cancellation.");
} finally {
  if (previous === undefined) delete process.env.CREW_PI_ROSTER;
  else process.env.CREW_PI_ROSTER = previous;
  rmSync(root, { recursive: true, force: true });
}
