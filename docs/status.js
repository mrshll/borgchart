// The front page's hero: a request typed to a team's lead on the right, and
// on the left the team's `brg status -w`, growing as the lead takes it on.
// A fixed fictional team, drawn the way brg draws a terminal tree.
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
  const REFRESH_MS = 2000; // `status -w` reads again every 2 s
  const STEP_MS = 4000; // each step of the scene lasts two refreshes
  const TYPE_AFTER_MS = 2500; // the tree shows alone before typing starts
  const SEND_AFTER_MS = 1000; // a pause between the last key and Enter
  const HOLD_MS = 12000; // the grown tree rests before the scene restarts

  const HEADER = ["lamp-eng-x7k2", "folder ~/src/lighthouse"];

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
    kind: "copy", name, team, children, running: live(children),
  });

  const OPUS = "§ opus high";
  const SONNET = "§ sonnet medium";
  const SOL = "Λ gpt-6.1 high";

  // The root lead, and the dev team it starts: a copy whose one root is
  // its own lead, with the implementer and reviewer under that lead, as in
  // teams/dev-team.
  const lead = (state, detail, kids) => m("lead-q8m3", OPUS, state, detail, kids);
  const dev = (state, detail, kids) => [
    copy("dev-k2m8", "dev-team", [m("lead-c4nd", OPUS, state, detail, kids)]),
  ];
  const builder = (state, detail) => m("implementer-h7p2", SOL, state, detail);
  const checker = (state, detail) => m("reviewer-w9d3", SONNET, state, detail);
  const done = () => dev("idle", "", [builder("idle"), checker("idle")]);

  // The scene, one tree per step. The first shows while the request is
  // typed; the rest follow its sending, each with what the lead says then.
  // The lead plans and starts a dev team, whose lead brings in an
  // implementer and then a reviewer, and the pull request comes back up.
  const SCENE = [
    { tree: lead("idle") },
    { tree: lead("working", "Read the request") },
    {
      tree: lead("working", "Plan the site"),
      say: "Planning it: a dev team builds the page, and a reviewer checks it before it comes to you.",
    },
    {
      tree: lead("idle", "", dev("working", "Read the brief")),
      say: "Started dev-k2m8 on it.",
    },
    { tree: lead("idle", "", dev("working", "Plan the page", [builder("working", "Build the tree")])) },
    { tree: lead("idle", "", dev("idle", "", [builder("working", "Write the man page")])) },
    {
      tree: lead("idle", "", dev("working", "Start a review", [
        builder("idle"),
        checker("working", "Review the page"),
      ])),
    },
    {
      tree: lead("idle", "", dev("idle", "", [
        builder("working", "Fix the phone view"),
        checker("idle"),
      ])),
    },
    {
      tree: lead("idle", "", dev("working", "Write the handoff", [
        builder("idle"),
        checker("idle"),
      ])),
    },
    { tree: lead("idle", "1 unacked", done()) },
    { tree: lead("working", "Check the handoff", done()) },
    { tree: lead("idle", "", done()), say: "The pull request is ready for your review." },
  ];
  // The still frame for reduced motion: the grown tree mid-run, every
  // member in it and some at work.
  const STILL = 6;

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

  const clock = (date) => date.toISOString().slice(11, 19);

  function frame(step, spin, refreshed) {
    const root = SCENE[step].tree;
    return [
      band(root),
      "",
      `<span class="b">${HEADER[0]}</span>  <span class="faded">${HEADER[1]}</span>`,
      ...tree(root, spin),
      "",
      `<span class="faded">refreshed ${clock(refreshed)} UTC · Ctrl-C to stop</span>`,
    ].join("\n");
  }

  // The session beside the tree: the request as far as it is typed, then,
  // once sent, what the lead has said so far and a fresh prompt.
  const CURSOR = '<span class="cursor">&nbsp;</span>';
  // Hyphenated words (one-page, --help) wrap whole, as a terminal would.
  const words = (text) => esc(text).replace(/\S*-\S*/g, '<span class="whole">$&</span>');
  function session(request, typed, sent, step) {
    const said = `<p class="said"><span class="faded">›</span> ${words(request.slice(0, typed))}${sent ? "" : CURSOR}</p>`;
    if (!sent) return said;
    const replies = SCENE.slice(1, step + 1)
      .filter((s) => s.say)
      .map((s) => `<p class="reply">${esc(s.say)}</p>`);
    return [said, ...replies, `<p class="said"><span class="faded">›</span> ${CURSOR}</p>`].join("");
  }

  const out = document.getElementById("status");
  const talk = document.getElementById("session");
  const typedText = document.getElementById("request");
  if (!out || !talk || !typedText) return;
  const request = typedText.textContent.trim().replace(/\s+/g, " ");
  talk.setAttribute("role", "img");
  talk.setAttribute("aria-label", "Someone types a request to the team's lead: " + request);

  // Hold the room the largest frame and the whole session need, so the page
  // doesn't move as the tree grows and the lead answers.
  const last = SCENE.length - 1;
  const roomy = SCENE.reduce((best, s, i) => (tree(s.tree, null).length > tree(SCENE[best].tree, null).length ? i : best), 0);
  out.previousElementSibling.innerHTML = frame(roomy, null, new Date());
  talk.previousElementSibling.innerHTML = session(request, request.length, true, last);

  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    out.innerHTML = frame(STILL, null, new Date());
    talk.innerHTML = session(request, request.length, true, STILL);
    return;
  }

  // When each key lands: an unhurried, slightly uneven pace, with a breath
  // after punctuation. A fixed seed keeps every run the same.
  let seed = 7;
  const rand = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  const keys = [];
  let at = TYPE_AFTER_MS;
  for (const ch of request) {
    at += 55 + rand() * 45 + (/[.,]/.test(ch) ? 380 : 0);
    keys.push(at);
  }
  const sendAt = at + SEND_AFTER_MS;
  // The tree changes only when `status -w` reads again, on its 2 s beat.
  const firstRead = Math.ceil(sendAt / REFRESH_MS) * REFRESH_MS;
  const cycle = firstRead + (last - 1) * STEP_MS + HOLD_MS;

  let start = Date.now();
  let shownTree = "", shownTalk = "";
  const draw = () => {
    let t = Date.now() - start;
    if (t >= cycle) {
      start += cycle * Math.floor(t / cycle);
      t = Date.now() - start;
    }
    const read = Math.floor(t / REFRESH_MS) * REFRESH_MS;
    const step = read < firstRead ? 0 : Math.min(last, 1 + Math.floor((read - firstRead) / STEP_MS));
    const html = frame(step, Math.floor(t / SPIN_MS), new Date(Date.now() - (t - read)));
    if (html !== shownTree) out.innerHTML = shownTree = html;
    let typed = 0;
    while (typed < keys.length && keys[typed] <= t) typed++;
    const said = session(request, typed, t >= sendAt, step);
    if (said !== shownTalk) talk.innerHTML = shownTalk = said;
  };
  draw();
  setInterval(draw, 50);
})();
