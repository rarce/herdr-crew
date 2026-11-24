// installed by herdr-crew; `herdr-crew app-install` overwrites this folder.
// HERDR_CREW_APP_MOD=1
//
// Crew: brings the focused space's crew up on herdr-core. It shows what
// `herdr-crew up --dry-run` would do for the focused terminal's folder (each
// role's verdict) and runs `herdr-crew up` on request: after a core restart that
// resumes every role's conversation and restarts the board. Open it with
// `mod.open herdr-crew`, or bind a key to that command in keys.json.

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, focused, spaces, theme, props, closeOverlay, checking } from "herdr";

// The herdr-crew that installed this mod (`app-install` writes its path here).
const CREW = "herdr-crew";

// The crew runs on herdr-core, whatever the environment of this window.
const crew = (args, cwd, timeout) =>
  exec("env", ["CREW_HOST=herdr-core", CREW, ...args], { cwd, timeout });

export default class Crew extends View {
  init(_props, cx) {
    this.handle = cx.focus_handle();
    this.handle.focus();
    this.folder = this.where();
    this.plan = null; // the dry run's output
    this.output = null; // the last `up`'s output
    this.failed = false;
    this.busy = false;
    if (checking()) {
      // `herdr-app check` renders once without running anything: show every part.
      this.folder = "/project";
      this.plan = { ok: true, text: "lead    create\nstatus  create\nplan: 2 steps" };
      this.output = "herdr-crew: lead in t1";
      return;
    }
    this.preview(cx);
  }

  /** The focused terminal's folder, else its space's. */
  where() {
    const f = focused();
    if (f?.cwd) return f.cwd;
    const space = spaces().find((s) => s.id === f?.space);
    return space?.folder || null;
  }

  preview(cx) {
    if (!this.folder) return;
    cx.spawn(async (cx) => {
      const r = await crew(["up", "--dry-run"], this.folder, 30000).catch((e) => ({
        code: 127,
        stdout: "",
        stderr: `${e}`,
      }));
      this.plan = { ok: r.code === 0, text: (r.stdout + r.stderr).trim() };
      cx.notify();
    });
  }

  up(cx) {
    if (this.busy || !this.folder) return;
    this.busy = true;
    this.output = null;
    cx.notify();
    cx.spawn(async (cx) => {
      const r = await crew(["up", "--no-attach"], this.folder, 600000).catch((e) => ({
        code: 127,
        stdout: "",
        stderr: `${e}`,
      }));
      this.busy = false;
      this.failed = r.code !== 0;
      this.output = (r.stdout + r.stderr).trim();
      cx.notify();
      this.preview(cx);
    });
  }

  close() {
    closeOverlay(props().overlay);
  }

  key(e, cx) {
    if (e.key === "escape") {
      this.close();
      cx.stop_propagation();
    }
  }

  /** The verdict lines of a dry run (`<role>  <verdict>`, before its `plan:` line), then its
   *  warnings (`herdr-crew: …`). */
  verdicts() {
    const lines = (this.plan?.text ?? "").split("\n");
    const end = lines.findIndex((l) => l.startsWith("plan:"));
    return lines
      .slice(0, end < 0 ? lines.length : end)
      .filter((l) => l.trim() && !l.startsWith("("));
  }

  block(t, text, color) {
    return div()
      .id(`crew-out-${color}`)
      .max_h(240)
      .min_h(0)
      .overflow_y_scroll()
      .p(10)
      .rounded(8)
      .bg(t.background)
      .text_size(12)
      .line_height("18px")
      .font_family("monospace")
      .text_color(color)
      .child(text);
  }

  render() {
    const t = theme();
    const configured = this.plan?.ok;
    const button = div()
      .id("crew-up")
      .h(34)
      .px(14)
      .flex()
      .items_center()
      .rounded(8)
      .bg(t.solid)
      .text_color(t.solid_foreground)
      .text_size(12.5)
      .font_medium()
      .when(this.busy || !configured, (el) => el.opacity(0.4))
      .cursor_pointer()
      .on_click((_e, cx) => this.up(cx))
      .child(this.busy ? "Bringing the crew up…" : "Bring the crew up");
    return v_flex()
      .id("herdr-crew")
      .track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .w_full()
      .min_h(0)
      .p(18)
      .gap(10)
      .bg(t.popover)
      .text_color(t.foreground)
      .child(
        h_flex()
          .items_center()
          .child(div().flex_1().text_size(15).font_semibold().child("Crew"))
          .child(
            div()
              .id("crew-esc")
              .px(6)
              .py(2)
              .rounded(5)
              .text_size(11)
              .text_color(t.dim)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click(() => this.close())
              .child("Esc"),
          ),
      )
      .child(
        div()
          .text_size(12.8)
          .text_color(t.muted)
          .child(this.folder ? `In ${this.folder}` : "Focus a terminal in the project first."),
      )
      .when(this.folder && this.plan === null, (el) =>
        el.child(div().text_size(12.5).text_color(t.dim).child("Reading the crew…")),
      )
      .when(this.plan !== null && !configured, (el) => el.child(this.block(t, this.plan.text, t.muted)))
      .when(Boolean(configured), (el) =>
        el.child(
          v_flex()
            .gap(2)
            .children(this.verdicts().map((l, i) => div().id(`crew-v-${i}`).text_size(12.5).child(l.trim()))),
        ),
      )
      .when(this.output !== null, (el) => el.child(this.block(t, this.output, this.failed ? t.danger ?? t.foreground : t.foreground)))
      .child(h_flex().justify_end().child(button));
  }
}
