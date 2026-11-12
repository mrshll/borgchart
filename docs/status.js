// The front page's `brg status -w`: the real root of the team that builds
// borgchart (engineering-56uw and its lead), with a made-up tree beneath it
// that grows and shrinks on its own. Dev teams, design panels, researchers
// and monitors come and go, their members turn working and idle, and
// finished ones stop and are deleted, only as the repository's own team
// templates allow (teams/engineering, teams/dev-team, teams/dev-design-team).
//
// Taken from the code, so re-check it there when the look changes:
//   marks, tones and spinner frames  src/tree.rs (state_mark, Tone, ink, painted)
//   palette                          src/tree.rs palette, copied into style.css
//   which state gets which look      src/runtime.rs status_look
//   columns and vendor marks         src/runtime.rs status_cells, vendor_mark
//   summary band and watch footer    src/runtime.rs summary_band, src/tui.rs watch_frame
//   spinner speed                    src/tui.rs SPIN_INTERVAL (500 ms)
//   member ID suffixes               src/registry.rs ALPHABET
"use strict";

(function () {
  const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  const SPIN_MS = 500;
  const REFRESH_MS = 2000; // `status -w` reads again every 2 s
  const MAX_ROWS = 12; // the tree never has more rows than this, root included
  const WIDEST = 84; // nor a line wider than this (the CSS fits it)
  const WARM_BEATS = 12; // beats run before the first frame, so it opens on a tree

  const HEADER = ["engineering-56uw", "folder ~/workspace/borgchart"];

  // A member: its name, vendor mark and model, state and details. A copy
  // (a nested team): its name, its team, and its members.
  const m = (name, model, state, detail, children) => ({
    kind: "member", name, model, state, detail: detail || "", children: children || [],
  });
  // brg counts a copy's own members that are not stopped as running, at
  // any depth inside it but not inside deeper copies.
  const live = (nodes) => nodes.reduce((n, k) =>
    k.kind === "copy" ? n : n + (k.state !== "stopped" ? 1 : 0) + live(k.children), 0);
  const copy = (name, team, children) => ({
    kind: "copy", name, team, children, get running() { return live(this.children); },
  });

  // Each role's vendor mark and model, from the templates' front matter.
  const MODEL = {
    root: "§ fable low", // teams/engineering lead
    researcher: "Λ gpt-6.1-terra medium", // teams/engineering researcher (codex)
    monitor: "§ sonnet low", // teams/engineering monitor
    lead: "§ opus high", // teams/dev-team lead (orchestrator.md)
    implementer: "Λ gpt-6.1-sol high", // teams/dev-team implementer (codex)
    reviewer: "§ opus high", // teams/dev-team reviewer
    designerClaude: "§ fable medium", // teams/dev-design-team designer-claude
    designerCodex: "Λ gpt-6-astra high", // teams/dev-design-team designer-codex
  };
  // What a member is doing, as its activity reads in the details column.
  const SAYS = {
    lead: ["Read the brief", "Plan the work", "Split the work", "Check the build"],
    implementer: ["Build the fix", "Write tests", "Run the checks", "Tidy the diff"],
    reviewer: ["Review the PR", "Read the diff", "Try it out"],
    researcher: ["Compare tools", "Read the docs", "Check sources", "Write it up"],
    monitor: ["Watch CI", "Check the logs", "Watch quotas"],
    designer: ["Sketch a page", "Draft options", "Try a layout", "Write it up"],
  };

  // A seeded generator (mulberry32), fresh each load.
  let seed = (Date.now() ^ 0x9e3779b9) >>> 0;
  function rand() {
    seed = (seed + 0x6d2b79f5) >>> 0;
    let t = Math.imul(seed ^ (seed >>> 15), seed | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  const pick = (xs) => xs[Math.floor(rand() * xs.length)];
  const chance = (p) => rand() < p;
  // A member ID's suffix: four of brg's suffix letters, never issued twice.
  const ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz";
  const issued = new Set(["th8v"]);
  function id(role) {
    let s;
    do s = Array.from({ length: 4 }, () => pick(ALPHABET)).join("");
    while (issued.has(s));
    issued.add(s);
    return `${role}-${s}`;
  }

  // The root lead, and the units under it. A unit is one child of the root
  // (a copy, a researcher or a monitor) with the steps it still has to take;
  // its rows are the most it can grow to, kept within MAX_ROWS.
  const root = m("lead-th8v", MODEL.root, "idle");
  let unacked = 0, pending = 0, busyUntil = 0, beatNo = 0;
  let units = [];

  const work = (n, says) => { n.state = "working"; n.detail = says; };
  const rest = (n) => { n.state = "idle"; n.detail = ""; };
  const stop = (n) => {
    if (n.kind === "member") { n.state = "stopped"; n.detail = ""; }
    n.children.forEach(stop);
  };
  // The root lead starts something, then goes back to waiting; a unit that
  // reports back leaves it a message to read. brg would print a working
  // lead's unacked count after its activity, too wide for the page, so a
  // report lands only once the root is idle, and the root reads its
  // messages before starting anything new.
  const rootWork = (says) => { work(root, says); busyUntil = beatNo + 1; };
  const report = () => { pending++; };

  const KINDS = {
    dev: { rows: 5, most: 2, make: devTeam },
    design: { rows: 3, most: 1, make: designPanel },
    researcher: { rows: 1, most: 2, make: researcher },
    monitor: { rows: 1, most: 1, make: monitor },
  };

  // A dev team: its lead plans, brings in an implementer (sometimes two),
  // then a reviewer, hands off, and the copy is stopped and deleted.
  function devTeam() {
    const lead = m(id("lead"), MODEL.lead, "working", "Read the brief");
    const team = copy(id("dev"), "dev-team", [lead]);
    const builders = [];
    const add = () => {
      const b = m(id("implementer"), MODEL.implementer, "working", pick(SAYS.implementer));
      builders.push(b);
      lead.children.push(b);
    };
    const checker = m(id("reviewer"), MODEL.reviewer, "working", pick(SAYS.reviewer));
    rootWork("Start a team");
    return { node: team, steps: [
      () => work(lead, pick(SAYS.lead)),
      () => { add(); rest(lead); },
      () => (chance(0.5) ? add() : work(builders[0], pick(SAYS.implementer))),
      () => { builders.forEach(rest); work(lead, "Check the build"); },
      () => { rest(lead); lead.children.push(checker); },
      () => { rest(checker); work(builders[0], "Fix the review"); },
      () => { rest(builders[0]); work(lead, "Hand off"); },
      () => { rest(lead); report(); },
      () => stop(team),
      null,
    ] };
  }

  // A design panel: two designers, one per vendor, sketch side by side.
  function designPanel() {
    const a = m(id("designer-claude"), MODEL.designerClaude, "working", pick(SAYS.designer));
    const b = m(id("designer-codex"), MODEL.designerCodex, "working", pick(SAYS.designer));
    const team = copy(id("design"), "dev-design-team", [a, b]);
    rootWork("Start a panel");
    return { node: team, steps: [
      () => (chance(0.5) ? rest(a) : rest(b)),
      () => { work(a, pick(SAYS.designer)); work(b, pick(SAYS.designer)); },
      () => rest(b),
      () => { rest(a); report(); },
      () => stop(team),
      null,
    ] };
  }

  function researcher() {
    const r = m(id("researcher"), MODEL.researcher, "working", pick(SAYS.researcher));
    rootWork("Ask a question");
    return { node: r, steps: [
      () => work(r, pick(SAYS.researcher)),
      () => work(r, "Write it up"),
      () => { rest(r); report(); },
      () => stop(r),
      null,
    ] };
  }

  // A monitor watches for a while, waking now and then, before it stops.
  function monitor() {
    const w = m(id("monitor"), MODEL.monitor, "working", pick(SAYS.monitor));
    rootWork("Set a watch");
    const steps = [];
    for (let i = 3 + Math.floor(rand() * 4); i > 0; i--) {
      steps.push(() => rest(w), () => work(w, pick(SAYS.monitor)));
    }
    steps.push(() => rest(w), () => stop(w), null);
    return { node: w, steps };
  }

  // One `status -w` refresh: often nothing changes; otherwise one thing,
  // now and then two. A unit's last step (null) deletes it. When nobody is
  // working, a unit takes its next step, so the tree rarely sits still.
  const busy = (n) => (n.state === "working" ? 1 : 0) + n.children.reduce((k, c) => k + busy(c), 0);
  function beat() {
    beatNo++;
    if (root.state === "working" && beatNo > busyUntil) rest(root);
    if (root.state === "idle") {
      unacked += pending;
      pending = 0;
    }
    if (!units.length || !chance(0.3)) {
      for (let i = chance(0.2) ? 2 : 1; i > 0; i--) act();
    }
    if (units.length && !busy(root)) advance(pick(units));
    if (root.state === "idle") root.detail = unacked ? `${unacked} unacked` : "";
  }
  function act() {
    if (unacked && root.state === "idle" && chance(0.4)) {
      unacked = 0;
      rootWork("Read a report");
      return;
    }
    const held = units.reduce((n, u) => n + KINDS[u.kind].rows, 1);
    const room = Object.keys(KINDS).filter((k) =>
      held + KINDS[k].rows <= MAX_ROWS && units.filter((u) => u.kind === k).length < KINDS[k].most);
    if (!unacked && room.length && (units.length === 0 || chance(units.length < 3 ? 0.35 : 0.12))) {
      const kind = pick(room);
      const unit = { kind, ...KINDS[kind].make() };
      units.push(unit);
      root.children.push(unit.node);
      return;
    }
    if (units.length) advance(pick(units));
  }
  function advance(unit) {
    const step = unit.steps.shift();
    if (step) step();
    else {
      units = units.filter((u) => u !== unit);
      root.children = root.children.filter((n) => n !== unit.node);
    }
  }

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

  // src/runtime.rs summary_band: attention first, then working, idle, unacked.
  function band(root) {
    const all = order(root, "", true, []).map((o) => o.node);
    const members = all.filter((n) => n.kind === "member");
    const working = members.filter((n) => n.state === "working").length;
    const idle = members.filter((n) => n.state === "idle").length;
    const need = all.filter((n) => look(n).tone === "alert").length;
    const unacked = members.reduce((sum, n) => {
      const hit = /(\d+) unacked/.exec(n.detail);
      return sum + (hit ? Number(hit[1]) : 0);
    }, 0);
    const parts = [];
    if (need) parts.push(`<span class="b alert chip"> ! ${need} need you </span>`);
    if (working) parts.push(`<span class="b busy">● ${working} working</span>`);
    if (idle) parts.push(`<span class="faded">· ${idle} idle</span>`);
    if (unacked) parts.push(`<span class="faded">${unacked} unacked</span>`);
    return `<span class="b">borgchart</span>    ` + parts.join("  ");
  }

  function clock(date) {
    return date.toISOString().slice(11, 19);
  }


  function frame(spin, refreshed, rows) {
    return [
      band(root),
      "",
      `<span class="b">${HEADER[0]}</span>  <span class="faded">${HEADER[1]}</span>`,
      ...(rows || tree(root, spin)),
      "",
      `<span class="faded">refreshed ${clock(refreshed)} UTC · Ctrl-C to stop</span>`,
    ].join("\n");
  }

  const out = document.getElementById("status");
  if (!out) return;
  // Hold the room of the largest tree the generator allows, so the page
  // never moves as it grows and shrinks.
  // It is as wide as the widest line too, so it keeps room for a sideways
  // scrollbar wherever the live frame could need one.
  out.previousElementSibling.innerHTML = frame(null, new Date(), Array(MAX_ROWS).fill(" ".repeat(WIDEST)));
  for (let i = 0; i < WARM_BEATS; i++) beat();

  // The tree changes once per 2 s refresh (only once, however many a
  // backgrounded tab missed), and the spinner turns every 500 ms. Time runs
  // on the page's own clock; the wall clock only names the refresh.
  const start = performance.now();
  let read = 0, refreshed = new Date(), shown = "";
  const draw = () => {
    const t = performance.now() - start;
    const now = Math.floor(t / REFRESH_MS);
    if (now !== read) {
      read = now;
      refreshed = new Date();
      beat();
    }
    const html = frame(Math.floor(t / SPIN_MS), refreshed);
    if (html !== shown) out.innerHTML = shown = html;
    setTimeout(draw, SPIN_MS - (t % SPIN_MS) + 5);
  };
  draw();
})();
