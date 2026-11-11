// The animated `brg status -w` on the front page: a fixed fictional team,
// drawn the way brg draws a terminal tree, stepping through a run.
//
// Taken from the code, so re-check it there when the look changes:
//   marks, tones and spinner frames  src/tree.rs (state_mark, Tone, ink, painted)
//   palette                          src/tree.rs palette, copied into style.css
//   which state gets which look      src/runtime.rs status_look
//   columns and vendor marks         src/runtime.rs status_cells, vendor_mark
//   summary band and watch footer    src/runtime.rs summary_band, src/tui.rs watch_frame
//   spinner speed                    src/tui.rs SPIN_INTERVAL (500 ms)
"use strict";

(function () {
  const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  const SPIN_MS = 500;
  const REFRESH_TICKS = 4; // `status -w` reads again every 2 s
  const STEP_TICKS = 8; // each step of the scene lasts two refreshes

  const HEADER = ["lamp-eng-x7k2", "folder ~/src/lighthouse"];

  // Members: [name, vendor mark and model]; copies: [name, team].
  const m = (name, model, state, detail, children) => ({
    kind: "member", name, model, state, detail: detail || "", children: children || [],
  });
  const copy = (name, team, running, children) => ({
    kind: "copy", name, team, running, children,
  });

  const OPUS = "§ opus high";
  const SONNET = "§ sonnet medium";
  const SOL = "Λ gpt-6.1-sol high";
  const TERRA = "Λ gpt-6.1-terra low";

  // The scene, one tree per step: a lead plans, its implementer builds, a
  // design panel sketches, the reviewer stops at a prompt for a person, and
  // the work comes back for the next piece.
  const SCENE = [
    m("lead-q8m3", OPUS, "working", "Read the fog-horn brief", [
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
    m("lead-q8m3", OPUS, "working", "Plan the fog-horn schedule", [
      m("implementer-b3n7", SOL, "working", "Add the fog-horn schedule"),
      m("researcher-f2g7", TERRA, "working", "Compare tide-table sources"),
    ]),
    m("lead-q8m3", OPUS, "idle", "", [
      m("implementer-b3n7", SOL, "working", "Add the fog-horn schedule"),
      copy("map-design", "design-panel", 2, [
        m("designer-a8e2", OPUS, "working", "Sketch the keeper's log view"),
        m("designer-n6y4", SOL, "working", "Sketch the keeper's log view"),
      ]),
      m("researcher-f2g7", TERRA, "idle", "1 unread"),
    ]),
    m("lead-q8m3", OPUS, "working", "Merge the design notes", [
      m("implementer-b3n7", SOL, "idle", "1 unread"),
      copy("map-design", "design-panel", 1, [
        m("designer-a8e2", OPUS, "working", "Write up the log view"),
        m("designer-n6y4", SOL, "idle"),
      ]),
      m("reviewer-t4s9", SONNET, "working", "Review the fog-horn schedule"),
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
    m("lead-q8m3", OPUS, "idle", "", [
      m("implementer-b3n7", SOL, "idle"),
      copy("map-design", "design-panel", 1, [
        m("designer-a8e2", OPUS, "working", "Write up the log view"),
        m("designer-n6y4", SOL, "stopped"),
      ]),
      m("reviewer-t4s9", SONNET, "blocked", "Review the fog-horn schedule"),
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
    m("lead-q8m3", OPUS, "idle", "2 unread", [
      m("implementer-b3n7", SOL, "idle"),
      copy("map-design", "design-panel", 0, [
        m("designer-a8e2", OPUS, "stopped"),
        m("designer-n6y4", SOL, "stopped"),
      ]),
      m("reviewer-t4s9", SONNET, "working", "Review the fog-horn schedule"),
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
    m("lead-q8m3", OPUS, "working", "Answer the review", [
      m("implementer-b3n7", SOL, "working", "Fix the lens rotation test"),
      m("reviewer-t4s9", SONNET, "idle"),
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
    m("lead-q8m3", OPUS, "working", "Hand off the fog-horn schedule", [
      m("implementer-b3n7", SOL, "stopped"),
      m("reviewer-t4s9", SONNET, "stopped"),
      m("researcher-f2g7", TERRA, "stopped"),
    ]),
  ];
  // The still frame for reduced motion: the step that shows every tone.
  const STILL = 4;

  // src/runtime.rs status_look, for the states the scene uses.
  function look(node) {
    if (node.kind === "copy") {
      return node.running > 0 ? { mark: "◆", tone: "team" } : { mark: null, tone: "faded" };
    }
    if (node.state === "working") return { mark: "●", tone: "busy" };
    if (node.state === "idle") return { mark: "·", tone: "plain" };
    if (node.state === "blocked") return { mark: "?", tone: "alert" };
    return { mark: null, tone: "faded" };
  }

  // src/tree.rs order_live_first: each parent before its children, siblings
  // that are running first, then by name.
  function order(node, indent, last, out) {
    out.push({ node, prefix: indent + (last ? "└─ " : "├─ ") });
    const below = indent + (last ? "   " : "│  ");
    const kids = node.children.slice().sort((a, b) => {
      const fa = look(a).tone === "faded", fb = look(b).tone === "faded";
      return fa !== fb ? (fa ? 1 : -1) : a.name < b.name ? -1 : 1;
    });
    kids.forEach((k, i) => order(k, below, i === kids.length - 1, out));
    return out;
  }

  // State first, then vendor/model or team, then details (status_cells).
  function cols(node) {
    return node.kind === "copy"
      ? [node.running + " running", node.team, ""]
      : [node.state, node.model, node.detail];
  }

  const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  // src/tree.rs paint: the style covers the text, not its trailing padding.
  function paint(text, cls) {
    if (!cls || !text.trim()) return esc(text);
    const body = text.replace(/\s+$/, "");
    return `<span class="${cls}">${esc(body)}</span>` + text.slice(body.length);
  }

  // src/tree.rs cells: the first column is followed by one space, the
  // others by two; the last isn't padded.
  function cells(rows) {
    const n = Math.max(...rows.map((r) => r.length));
    const widths = Array(n).fill(0);
    rows.forEach((r) => r.forEach((c, i) => (widths[i] = Math.max(widths[i], [...c].length))));
    return rows.map((r) =>
      r.map((c, i) => {
        const sep = i === 0 ? "" : i === 1 ? " " : "  ";
        const pad = i + 1 < n ? " ".repeat(widths[i] - [...c].length) : "";
        return sep + c + pad;
      })
    );
  }

  // src/tree.rs painted: furniture recedes; the mark, name and state take
  // the row's tone; an alert state is a filled label.
  function painted(prefix, row, lk) {
    const tone = { plain: "", busy: "b busy", team: "b team", alert: "b alert", faded: "faded" }[lk.tone];
    let line = paint(prefix, "faded") + paint(row[0].slice(prefix.length), tone);
    for (let i = 1; i < row.length; i++) {
      const cell = row[i];
      if (i === 1 && lk.tone === "alert") {
        const before = cell.length - cell.trimStart().length - 1;
        const after = cell.length - cell.trimEnd().length - 1;
        line += " ".repeat(before) + `<span class="b alert chip"> ${esc(cell.trim())} </span>` + " ".repeat(after);
        continue;
      }
      let cls;
      if (lk.tone === "faded") cls = "faded";
      else if (lk.tone === "team") cls = i === 2 ? "team" : "";
      else if (i === 1) cls = tone;
      else if (i === 2 || lk.tone === "alert") cls = "";
      else cls = "faded";
      line += paint(cell, cls);
    }
    return line.replace(/\s+$/, "");
  }

  function tree(root, spin) {
    const ordered = order(root, "", true, []);
    const rows = ordered.map(({ node, prefix }) => {
      const lk = look(node);
      let mark = lk.mark || " ";
      if (lk.tone === "busy" && spin !== null) mark = FRAMES[spin % FRAMES.length];
      const c = cols(node);
      return [prefix + mark + " " + node.name, " " + c[0] + " ", c[1], c[2]];
    });
    return cells(rows).map((row, i) => painted(ordered[i].prefix, row, look(ordered[i].node)));
  }

  // src/runtime.rs summary_band: attention first, then working, idle, unread.
  function band(root) {
    const all = order(root, "", true, []).map((o) => o.node);
    const members = all.filter((n) => n.kind === "member");
    const working = members.filter((n) => n.state === "working").length;
    const idle = members.filter((n) => n.state === "idle").length;
    const need = all.filter((n) => look(n).tone === "alert").length;
    const unread = members.reduce((sum, n) => {
      const hit = /(\d+) unread/.exec(n.detail);
      return sum + (hit ? Number(hit[1]) : 0);
    }, 0);
    const parts = [];
    if (need) parts.push(`<span class="b alert chip"> ! ${need} need you </span>`);
    if (working) parts.push(`<span class="b busy">● ${working} working</span>`);
    if (idle) parts.push(`<span class="faded">· ${idle} idle</span>`);
    if (unread) parts.push(`<span class="faded">${unread} unread</span>`);
    return `<span class="b">borgchart</span>    ` + parts.join("  ");
  }

  function clock(date) {
    return date.toISOString().slice(11, 19);
  }

  function frame(step, spin, refreshed) {
    const root = SCENE[step];
    return [
      band(root),
      "",
      `<span class="b">${HEADER[0]}</span>  <span class="faded">${HEADER[1]}</span>`,
      ...tree(root, spin),
      "",
      `<span class="faded">refreshed ${clock(refreshed)} UTC · Ctrl-C to stop</span>`,
    ].join("\n");
  }

  const out = document.getElementById("status");
  if (!out) return;
  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (still) {
    out.innerHTML = frame(STILL, null, new Date());
    return;
  }
  let tick = 0;
  let refreshed = new Date();
  const draw = () => {
    if (tick % REFRESH_TICKS === 0) refreshed = new Date();
    out.innerHTML = frame(Math.floor(tick / STEP_TICKS) % SCENE.length, tick, refreshed);
    tick++;
  };
  draw();
  setInterval(draw, SPIN_MS);
})();
