// The prototype first screen: a small made-up team template being edited
// by hand, beside the team it makes. Raise the coder role's max and create
// one: a second coder joins. Type a reviewer role and its prompt and create
// it: a reviewer joins. Add a role naming another template, create and
// start it: a child team nests under the lead. Then it starts again.
//
// The file is valid template syntax (docs/format.md): with a `qa` template
// beside it, every stage passes `brg validate`. The team side is simpler
// than `brg status` (no details column, no refresh footer) but keeps its
// marks, tones, order and columns, as status.js copies them from
// src/tree.rs and src/runtime.rs.
"use strict";

(function () {
  const stage = document.getElementById("hero-stage");
  if (!stage) return;
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (sel) => stage.querySelector(sel);
  const filesEl = $(".files"), cmdEl = $(".cmd-text");
  const bandEl = $(".band"), headEl = $(".head"), rowsEl = $(".rows");

  const TEAM = "my-team-k3f9";
  const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  const SPIN_MS = 500; // src/tui.rs SPIN_INTERVAL
  const MODEL = {
    lead: "§ opus high",
    coder: "Λ gpt-6.2-sol high", // `codex: sol high`, resolved as status shows it
    reviewer: "§ opus high",
    qaLead: "§ sonnet medium",
  };

  // The template as it opens, and what each beat types.
  const START = [
    "default: claude",
    "roles:",
    "  lead:",
    "    prompt: lead.md",
    "  coder:",
    "    prompt: coder.md",
    "    vendor: codex",
    "    parent: lead",
    "    start: false",
  ];
  const MAX = ["    max: 2"];
  const REVIEWER = ["  reviewer:", "    prompt: reviewer.md", "    parent: lead", "    start: false"];
  const REVIEWER_MD = [
    "---",
    "models: {claude: opus high}",
    "---",
    "Review each change a coder hands you.",
    "Tell the lead: ship it, or what to fix.",
  ];
  const QA = ["  qa: {template: qa, parent: lead, start: false}"];

  // ---- timing ----
  let run = 0; // bumped on reset, so a stale beat stops
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const between = (lo, hi) => lo + Math.random() * (hi - lo);
  // A person's typing: quick within a word, a beat at spaces and
  // punctuation, now and then a longer hesitation.
  function keyDelay(ch, next) {
    let ms = between(38, 85);
    if (ch === " ") ms += between(20, 70);
    if (ch === ":" || ch === "," || ch === "." || ch === "{") ms += between(60, 140);
    if (next === undefined) ms += between(80, 160);
    if (Math.random() < 0.04) ms += between(180, 380);
    return ms;
  }

  // ---- the editor ----
  const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  // explorer.js's colouring: keys bold, a prompt's front matter faded.
  const yamlLine = (s) => esc(s).replace(/^(\s*)([\w.-]+)(:)/, "$1<b>$2</b>$3")
    .replace(/(\{|, )([\w.-]+)(:)/g, "$1<b>$2</b>$3");
  const caret = document.createElement("span");
  caret.className = "caret";

  const files = {};
  function makeFile(name) {
    const el = document.createElement("div");
    el.className = "file away";
    filesEl.appendChild(el);
    const f = { name, el, lines: [], front: name.endsWith(".md") };
    files[name] = f;
    return f;
  }
  function paintLine(f, i) {
    const ln = f.lines[i];
    let html;
    if (f.name.endsWith(".yaml")) html = yamlLine(ln.text);
    else {
      const fence = f.lines.findIndex((l, j) => j > 0 && l.text === "---");
      const inFront = f.lines[0] && f.lines[0].text.startsWith("---") && (fence < 0 || i <= fence);
      html = inFront ? `<span class="faded">${esc(ln.text)}</span>` : esc(ln.text);
    }
    ln.tx.innerHTML = html;
    if (ln.hasCaret) ln.tx.appendChild(caret);
  }
  function addLine(f, text, fresh) {
    const div = document.createElement("div");
    div.className = "ln" + (fresh ? " enter fresh" : "");
    const no = document.createElement("span");
    no.className = "no";
    no.textContent = String(f.lines.length + 1).padStart(2) + " ";
    const tx = document.createElement("span");
    div.append(no, tx);
    f.el.appendChild(div);
    const ln = { div, tx, text, hasCaret: false };
    f.lines.push(ln);
    paintLine(f, f.lines.length - 1);
    if (fresh) {
      requestAnimationFrame(() => requestAnimationFrame(() => div.classList.remove("enter")));
      setTimeout(() => div.classList.remove("fresh"), 2600);
    }
    return ln;
  }
  function putCaret(f, i) {
    for (const file of Object.values(files)) for (const l of file.lines) l.hasCaret = false;
    if (f) {
      f.lines[i].hasCaret = true;
      paintLine(f, i);
      follow(f, i);
    } else caret.remove();
  }
  // Keep the caret's line in view where the editor is shorter than the file.
  function follow(f, i) {
    const h = f.lines[0].div.offsetHeight || 1;
    filesEl.scrollTo({ top: Math.max(0, (i + 1) * h - filesEl.clientHeight), behavior: still ? "auto" : "smooth" });
  }
  function show(name) {
    for (const f of Object.values(files)) f.el.classList.toggle("away", f.name !== name);
    stage.querySelectorAll(".tab").forEach((t) => t.setAttribute("aria-pressed", String(t.dataset.file === name)));
    shown = name;
  }
  let shown = "template.yaml";
  stage.querySelectorAll(".tab").forEach((t) => t.addEventListener("click", () => files[t.dataset.file] && show(t.dataset.file)));

  // Type lines at the end of a file, as an editor with auto-indent would
  // take them: the indent comes with Enter, the rest one key at a time.
  async function typeLines(f, lines) {
    const my = run;
    for (const text of lines) {
      const indent = text.match(/^ */)[0];
      const ln = addLine(f, indent, true);
      putCaret(f, f.lines.length - 1);
      await sleep(between(140, 260));
      const rest = text.slice(indent.length);
      for (let k = 0; k < rest.length; k++) {
        if (my !== run) return;
        ln.text += rest[k];
        paintLine(f, f.lines.length - 1);
        await sleep(keyDelay(rest[k], rest[k + 1]));
      }
    }
  }
  // The shell line: clear it, type a command, press Enter.
  async function command(text) {
    const my = run;
    putCaret(null);
    cmdEl.classList.remove("ran");
    cmdEl.textContent = "";
    cmdEl.after(caret);
    await sleep(between(250, 400));
    for (let k = 0; k < text.length; k++) {
      if (my !== run) return;
      cmdEl.textContent += text[k];
      await sleep(keyDelay(text[k], text[k + 1]) * 0.8);
    }
    await sleep(300);
    caret.remove();
    cmdEl.classList.add("ran");
    await sleep(350);
  }

  // ---- the team ----
  const m = (name, model, state) => ({ kind: "member", name, model, state, children: [] });
  const live = (nodes) => nodes.reduce((n, k) =>
    k.kind === "child" ? n : n + (k.state === "working" || k.state === "idle" ? 1 : 0) + live(k.children), 0);
  const childTeam = (name, template) => ({
    kind: "child", name, template, children: [], get running() { return live(this.children); },
  });
  let root;

  // src/runtime.rs status_look, for the states shown here.
  function look(node) {
    if (node.kind === "child") return node.running > 0 ? { mark: "◆", tone: "team" } : { mark: " ", tone: "faded" };
    if (node.state === "working") return { mark: "●", tone: "busy" };
    if (node.state === "idle") return { mark: "·", tone: "plain" };
    return { mark: " ", tone: "faded" };
  }
  // src/tree.rs order_live_first: parents first, running siblings first, then by name.
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
  const cols = (n) => n.kind === "child" ? [n.running + " running", n.template] : [n.state, n.model];
  // Column widths for every row the loop can show, so no column ever moves.
  const W = [21, 9];
  // src/tree.rs painted: furniture recedes; mark, name and state take the
  // row's tone; a child team's template is gold.
  function paintRow(node, prefix, spin) {
    const lk = look(node);
    let mark = lk.mark;
    if (lk.tone === "busy" && spin !== null) mark = FRAMES[spin % FRAMES.length];
    const [state, third] = cols(node);
    const tone = { plain: "", busy: "b busy", team: "b team", faded: "faded" }[lk.tone];
    const span = (cls, s) => cls ? `<span class="${cls}">${esc(s)}</span>` : esc(s);
    const width = [...prefix].length + 2 + node.name.length;
    // The mark sits in a one-column box: a spinner frame drawn from a
    // fallback font must not push the columns after it.
    const cell = `<span class="mark">${esc(mark)}</span>`;
    const named = tone ? `<span class="${tone}">${cell} ${esc(node.name)}</span>` : `${cell} ${esc(node.name)}`;
    return span("faded", prefix) + named + " ".repeat(W[0] - width) +
      "  " + span(lk.tone === "busy" ? tone : lk.tone === "faded" ? "faded" : "", state) +
      " ".repeat(W[1] - state.length) + "   " +
      span(lk.tone === "team" ? "team" : lk.tone === "faded" ? "faded" : "", third);
  }

  const rows = new Map();
  let spin = still ? null : 0;
  function render() {
    const ordered = order(root, "", true, []);
    const seen = new Set();
    ordered.forEach(({ node, prefix }, i) => {
      let row = rows.get(node);
      if (!row) {
        row = document.createElement("div");
        row.className = still ? "row" : "row enter fresh";
        row.style.transform = `translateY(${i * 100}%)`;
        rowsEl.appendChild(row);
        rows.set(node, row);
        if (!still) {
          requestAnimationFrame(() => requestAnimationFrame(() => row.classList.remove("enter")));
          setTimeout(() => row.classList.remove("fresh"), 2600);
        }
      }
      row.style.transform = `translateY(${i * 100}%)`;
      row.innerHTML = paintRow(node, prefix, spin);
      seen.add(node);
    });
    for (const [node, row] of rows) if (!seen.has(node)) { row.remove(); rows.delete(node); }
    // src/runtime.rs summary_band: working, then idle.
    const members = ordered.map((o) => o.node).filter((n) => n.kind === "member");
    const working = members.filter((n) => n.state === "working").length;
    const idle = members.filter((n) => n.state === "idle").length;
    const parts = [];
    if (working) parts.push(`<span class="b busy">● ${working} working</span>`);
    if (idle) parts.push(`<span class="faded">· ${idle} idle</span>`);
    bandEl.innerHTML = `<span class="b">borgchart</span>    ` + parts.join("  ");
    headEl.innerHTML = `<span class="b">${TEAM}</span>  <span class="faded">folder ~/src/shop</span>`;
  }
  // A new member: up and idle, then at work on what its parent hands it.
  async function join(parent, node) {
    node.state = "idle";
    node.since = performance.now();
    parent.children.push(node);
    render();
    await sleep(1100);
    node.state = "working";
    node.since = performance.now();
    render();
  }

  // ---- the loop ----
  function reset() {
    run++;
    filesEl.textContent = "";
    for (const k of Object.keys(files)) delete files[k];
    const yaml = makeFile("template.yaml");
    START.forEach((t) => addLine(yaml, t, false));
    makeFile("reviewer.md");
    $('.tab[data-file="reviewer.md"]').classList.add("unmade");
    show("template.yaml");
    filesEl.scrollTop = 0;
    putCaret(yaml, START.length - 1);
    cmdEl.textContent = "";
    cmdEl.classList.remove("ran");
    rowsEl.textContent = "";
    rows.clear();
    root = m("lead-q8m3", MODEL.lead, "idle");
    root.children.push(m("coder-7k2m", MODEL.coder, "working"));
  }

  async function story() {
    reset();
    const my = run;
    const yaml = files["template.yaml"], md = files["reviewer.md"];
    // Open on a settled team, without each row arriving.
    for (const node of order(root, "", true, []).map((o) => o.node)) {
      const row = document.createElement("div");
      row.className = "row";
      rowsEl.appendChild(row);
      rows.set(node, row);
    }
    render();
    stage.classList.remove("dim");
    await sleep(1800);

    // Raise max, create the second coder.
    await typeLines(yaml, MAX);
    await sleep(600);
    await command(`brg create ${TEAM}/coder`);
    await join(root, m("coder-c4hd", MODEL.coder));
    await sleep(2000);

    // A new role and its prompt, then its member.
    await typeLines(yaml, REVIEWER);
    await sleep(500);
    $('.tab[data-file="reviewer.md"]').classList.remove("unmade");
    show("reviewer.md");
    await sleep(500);
    await typeLines(md, REVIEWER_MD);
    await sleep(600);
    await command(`brg create ${TEAM}/reviewer`);
    await join(root, m("reviewer-p7xm", MODEL.reviewer));
    await sleep(2000);

    // A role naming another template: create the child team (stopped),
    // start it, and its lead comes up inside it.
    show("template.yaml");
    putCaret(yaml, yaml.lines.length - 1);
    await sleep(700);
    await typeLines(yaml, QA);
    await sleep(600);
    await command(`brg create ${TEAM}/qa`);
    const qa = childTeam("qa-x2b9", "qa");
    root.children.push(qa);
    render();
    await sleep(1400);
    await command(`brg start ${TEAM}/qa-x2b9`);
    await join(qa, m("lead-h4nw", MODEL.qaLead));
    await sleep(5000);
    if (my !== run) return;
    stage.classList.add("dim");
    await sleep(800);
  }

  // Members already at work turn idle and back now and then, as they do.
  function stir() {
    const members = order(root, "", true, []).map((o) => o.node)
      .filter((n) => n.kind === "member" && !(n.since && performance.now() - n.since < 3000));
    if (members.length) {
      const n = members[Math.floor(Math.random() * members.length)];
      const working = members.filter((k) => k.state === "working").length;
      if (n.state === "idle") n.state = "working";
      else if (working > 1) n.state = "idle";
      n.since = performance.now();
      render();
    }
    setTimeout(stir, between(2200, 3800));
  }

  // The still frame for reduced motion: every edit made, every member up,
  // the lines that made them marked.
  function stillFrame() {
    reset();
    const yaml = files["template.yaml"], md = files["reviewer.md"];
    const mark = (ln) => ln.div.classList.add("fresh");
    [...MAX, ...REVIEWER, ...QA].forEach((t) => mark(addLine(yaml, t, false)));
    REVIEWER_MD.forEach((t) => addLine(md, t, false));
    $('.tab[data-file="reviewer.md"]').classList.remove("unmade");
    putCaret(null);
    cmdEl.textContent = `brg start ${TEAM}/qa-x2b9`;
    cmdEl.classList.add("ran");
    root.children.push(m("coder-c4hd", MODEL.coder, "working"), m("reviewer-p7xm", MODEL.reviewer, "idle"));
    const qa = childTeam("qa-x2b9", "qa");
    qa.children.push(m("lead-h4nw", MODEL.qaLead, "working"));
    root.children.push(qa);
    render();
    for (const [node, row] of rows) if (node !== root && node.name !== "coder-7k2m") row.classList.add("fresh");
  }

  if (still) {
    stillFrame();
    return;
  }
  (async () => { for (;;) await story(); })();
  setInterval(() => { spin++; render(); }, SPIN_MS);
  setTimeout(stir, 3000);
})();
