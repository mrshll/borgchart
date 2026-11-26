// The first screen: a small made-up team template (hero-team.js) typed by
// hand, beside the team it makes. Type a reviewer role and its prompt, run
// `brg start`: a reviewer joins. Add a role naming another template,
// create and start it: a child team nests under the lead. Add an on-call
// role and a chat plugin that delivers to it, run `brg start`: the plugin
// comes up, an alert lands, and the on-call member wakes. Then it starts
// again.
//
// The team side is simpler than `brg status` (no details column for
// members, no refresh footer) but keeps its marks, tones, order and
// columns, from src/tree.rs and src/runtime.rs:
//   marks, tones and spinner frames  src/tree.rs (state_mark, painted)
//   which state gets which look      src/runtime.rs status_look
//   columns and vendor marks         src/runtime.rs status_cells, vendor_mark
//   root plugins after the tree      src/tree.rs render_named
//   summary band                     src/runtime.rs summary_band
//   spinner speed                    src/tui.rs SPIN_INTERVAL (500 ms)
"use strict";

(function () {
  const stage = document.getElementById("hero-stage");
  const data = window.HERO_TEAM;
  if (!stage || !data) return;
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (sel) => stage.querySelector(sel);
  const filesEl = $(".files"), cmdEl = $(".cmd-text");
  const bandEl = $(".band"), headEl = $(".head"), rowsEl = $(".rows");

  const TEAM = `${data.team}-k3f9`;
  const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  const SPIN_MS = 500;
  // Each role's vendor mark and model as status shows them, from the
  // role files in hero-team.js (`codex: sol high` resolves to a full slug).
  const MODEL = {
    lead: ["§", "opus high"],
    coder: ["Λ", "gpt-6.2-sol high"],
    reviewer: ["§", "opus high"],
    oncall: ["§", "haiku"],
    qaLead: ["§", "sonnet medium"],
  };
  const [REVIEWER, QA, ONCALL] = data.beats.map((b) => b.edits);

  // ---- timing ----
  let run = 0; // bumped on reset, so a stale beat stops typing
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const between = (lo, hi) => lo + Math.random() * (hi - lo);
  // A person's typing: quick within a word, a beat at spaces and
  // punctuation, now and then a longer hesitation.
  function keyDelay(ch, next) {
    let ms = between(20, 40);
    if (ch === " ") ms += between(10, 35);
    if (":,.{[".includes(ch)) ms += between(25, 70);
    if (next === undefined) ms += between(40, 90);
    if (Math.random() < 0.02) ms += between(120, 260);
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
    return (files[name] = { name, el, lines: [] });
  }
  function paintLine(f, ln) {
    let html;
    if (f.name.endsWith(".yaml")) html = yamlLine(ln.text);
    else {
      const i = f.lines.indexOf(ln);
      const fence = f.lines.findIndex((l, j) => j > 0 && l.text === "---");
      const front = f.lines[0].text.startsWith("---") && (fence < 0 || i <= fence);
      html = front ? `<span class="faded">${esc(ln.text)}</span>` : esc(ln.text);
    }
    ln.tx.innerHTML = html;
    if (ln === caretAt) ln.tx.appendChild(caret);
  }
  // A line at `at` (the end by default); a fresh one opens, tinted.
  function addLine(f, text, fresh, at = f.lines.length) {
    const div = document.createElement("div");
    div.className = "ln" + (fresh ? " enter fresh" : "");
    const no = document.createElement("span");
    no.className = "no";
    const tx = document.createElement("span");
    div.append(no, tx);
    f.el.insertBefore(div, f.lines[at] ? f.lines[at].div : null);
    const ln = { div, no, tx, text };
    f.lines.splice(at, 0, ln);
    f.lines.forEach((l, i) => (l.no.textContent = String(i + 1).padStart(2) + " "));
    paintLine(f, ln);
    if (fresh) {
      requestAnimationFrame(() => requestAnimationFrame(() => div.classList.remove("enter")));
      setTimeout(() => div.classList.remove("fresh"), 2600);
    }
    return ln;
  }
  let caretAt = null;
  function putCaret(f, ln) {
    const was = caretAt;
    caretAt = ln;
    if (was && was !== ln) was.tx.contains(caret) && caret.remove();
    if (ln) {
      paintLine(f, ln);
      follow(f, ln);
    } else caret.remove();
  }
  // Keep the caret's line in view where the editor shows fewer lines than
  // the file has (a phone).
  function follow(f, ln) {
    const h = f.lines[0].div.offsetHeight || 1;
    const i = f.lines.indexOf(ln);
    const top = filesEl.scrollTop;
    let next = top;
    if ((i + 1) * h > top + filesEl.clientHeight) next = (i + 1) * h - filesEl.clientHeight;
    if (i * h < top) next = i * h;
    if (next !== top) filesEl.scrollTo({ top: next, behavior: still ? "auto" : "smooth" });
  }
  function show(name) {
    for (const f of Object.values(files)) f.el.classList.toggle("away", f.name !== name);
    stage.querySelectorAll(".tab").forEach((t) => t.setAttribute("aria-pressed", String(t.dataset.file === name)));
  }
  stage.querySelectorAll(".tab").forEach((t) =>
    t.addEventListener("click", () => files[t.dataset.file] && show(t.dataset.file)));

  // Type one edit, as an editor with auto-indent would take it: the indent
  // comes with Enter, the rest one key at a time.
  async function type(edit) {
    const my = run;
    const f = files[edit.file];
    let at = edit.after === undefined ? f.lines.length : edit.after;
    for (const text of edit.lines) {
      const indent = text.match(/^ */)[0];
      const ln = addLine(f, indent, true, at++);
      putCaret(f, ln);
      await sleep(between(70, 130));
      const rest = text.slice(indent.length);
      for (let k = 0; k < rest.length; k++) {
        if (my !== run) return;
        ln.text += rest[k];
        paintLine(f, ln);
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
    await sleep(between(250, 380));
    for (let k = 0; k < text.length; k++) {
      if (my !== run) return;
      cmdEl.textContent += text[k];
      await sleep(keyDelay(text[k], text[k + 1]) * 0.6);
    }
    await sleep(280);
    caret.remove();
    cmdEl.classList.add("ran");
    await sleep(320);
  }

  // ---- the team ----
  const m = (name, model, state) => ({ kind: "member", name, model, state, children: [] });
  const live = (nodes) => nodes.reduce((n, k) =>
    k.kind === "child" ? n : n + (k.state === "working" || k.state === "idle" ? 1 : 0) + live(k.children), 0);
  const childTeam = (name, template) => ({
    kind: "child", name, template, children: [], get running() { return live(this.children); },
  });
  let top = []; // the team's top-level members
  let plugin = null; // the chat plugin's row, once started

  // src/runtime.rs status_look, for the states shown here.
  function look(node) {
    if (node.kind === "child") return node.running > 0 ? { mark: "◆", tone: "team" } : { mark: " ", tone: "faded" };
    if (node.state === "working") return { mark: "●", tone: "busy" };
    if (node.state === "idle") return { mark: "·", tone: "plain" };
    return { mark: " ", tone: "faded" };
  }
  // src/tree.rs order_live_first: parents first, running siblings first, then by name.
  const sorted = (nodes) => nodes.slice().sort((a, b) => {
    const fa = look(a).tone === "faded", fb = look(b).tone === "faded";
    return fa !== fb ? (fa ? 1 : -1) : a.name < b.name ? -1 : 1;
  });
  function order(nodes, indent, out) {
    const kids = sorted(nodes);
    kids.forEach((node, i) => {
      const last = i === kids.length - 1;
      out.push({ node, prefix: indent + (last ? "└─ " : "├─ ") });
      order(node.children, indent + (last ? "   " : "│  "), out);
    });
    return out;
  }
  // Fixed column widths for every row the loop can show, so no column moves.
  const W = [21, 9];
  const span = (cls, s) => (cls ? `<span class="${cls}">${s}</span>` : s);
  // The mark sits in a one-column box: a spinner frame drawn from a
  // fallback font must not push the columns after it.
  const markBox = (mark) => `<span class="mark">${esc(mark)}</span>`;
  // src/tree.rs painted: furniture recedes; mark, name and state take the
  // row's tone; a child team's template is gold. The model after the vendor
  // mark gives way first on a narrow screen.
  function paintRow(node, prefix, spin) {
    const lk = look(node);
    let mark = lk.mark;
    if (lk.tone === "busy" && spin !== null) mark = FRAMES[spin % FRAMES.length];
    const tone = { plain: "", busy: "b busy", team: "b team", faded: "faded" }[lk.tone];
    const state = node.kind === "child" ? node.running + " running" : node.state;
    const third = node.kind === "child"
      ? span(lk.tone === "team" ? "team" : "faded", esc(node.template))
      : esc(node.model[0]) + `<span class="model"> ${esc(node.model[1])}</span>`;
    return span("faded", esc(prefix)) + span(tone, markBox(mark) + " " + esc(node.name)) +
      " ".repeat(W[0] - [...prefix].length - 2 - node.name.length) + "  " +
      span(lk.tone === "busy" ? tone : lk.tone === "faded" ? "faded" : "", esc(state)) +
      " ".repeat(W[1] - state.length) + "   " + third;
  }
  // A healthy root plugin: the whole row recedes, below the tree.
  const paintPlugin = (p) => span("faded", markBox("·") + " plugin " + esc(p.name) + "  alive" +
    `<span class="model">   ${esc(p.detail)}</span>`);

  const rows = new Map();
  let spin = still ? null : 0;
  function place(key, html, i, fresh) {
    let row = rows.get(key);
    if (!row) {
      row = document.createElement("div");
      row.className = fresh ? "row enter fresh" : "row";
      row.style.transform = `translateY(${i * 100}%)`;
      rowsEl.appendChild(row);
      rows.set(key, row);
      if (fresh) {
        requestAnimationFrame(() => requestAnimationFrame(() => row.classList.remove("enter")));
        setTimeout(() => row.classList.remove("fresh"), 2600);
      }
    }
    row.style.transform = `translateY(${i * 100}%)`;
    row.innerHTML = html;
    return row;
  }
  function render(fresh = !still) {
    const ordered = order(top, "", []);
    ordered.forEach(({ node, prefix }, i) => place(node, paintRow(node, prefix, spin), i, fresh));
    if (plugin) place(plugin, paintPlugin(plugin), ordered.length, fresh);
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
  const flash = (row) => {
    row.classList.add("fresh");
    setTimeout(() => row.classList.remove("fresh"), 1800);
  };
  // A new member: up and idle, then (unless it waits for work) at work on
  // what its parent hands it.
  async function join(list, node, waits) {
    node.state = "idle";
    node.since = performance.now();
    list.push(node);
    render();
    if (waits) return;
    await sleep(1000);
    node.state = "working";
    node.since = performance.now();
    render();
  }

  // ---- the loop ----
  function reset() {
    run++;
    filesEl.textContent = "";
    for (const k of Object.keys(files)) delete files[k];
    caretAt = null;
    const yaml = makeFile("template.yaml");
    data.start["template.yaml"].forEach((t) => addLine(yaml, t, false));
    makeFile("reviewer.md");
    $('.tab[data-file="reviewer.md"]').classList.add("unmade");
    show("template.yaml");
    filesEl.scrollTop = 0;
    cmdEl.textContent = "";
    cmdEl.classList.remove("ran");
    rowsEl.textContent = "";
    rows.clear();
    const lead = m("lead-q8m3", MODEL.lead, "idle");
    lead.children.push(m("coder-7k2m", MODEL.coder, "working"));
    top = [lead];
    plugin = null;
    return lead;
  }

  async function story() {
    const lead = reset();
    const my = run;
    const yaml = files["template.yaml"];
    putCaret(yaml, yaml.lines[yaml.lines.length - 1]);
    render(false);
    stage.classList.remove("dim");
    stage.dataset.loops = String(Number(stage.dataset.loops || 0) + 1);
    await sleep(1000);

    // A new role and its prompt; a full start gives it a member.
    await type(REVIEWER[0]);
    await sleep(400);
    $('.tab[data-file="reviewer.md"]').classList.remove("unmade");
    show("reviewer.md");
    await sleep(400);
    await type(REVIEWER[1]);
    await sleep(500);
    await command(`brg start ${TEAM}`);
    await join(lead.children, m("reviewer-p7xm", MODEL.reviewer));
    await sleep(900);

    // A role naming another template: create the child team (stopped),
    // start it, and its lead comes up inside it.
    show("template.yaml");
    putCaret(yaml, yaml.lines[yaml.lines.length - 1]);
    await sleep(500);
    await type(QA[0]);
    await sleep(400);
    await command(`brg create ${TEAM}/qa`);
    const qa = childTeam("qa-x2b9", "qa");
    lead.children.push(qa);
    render();
    await sleep(900);
    await command(`brg start ${TEAM}/qa-x2b9`);
    await join(qa.children, m("lead-h4nw", MODEL.qaLead));
    await sleep(900);

    // An on-call role and the chat plugin that delivers to it. A full
    // start runs the plugin's process and gives the role its member, who
    // waits; an alert lands in the channel and it starts work.
    await type(ONCALL[0]);
    await sleep(300);
    await type(ONCALL[1]);
    await sleep(500);
    await command(`brg start ${TEAM}`);
    plugin = { name: "chat", detail: "watching #alerts" };
    const oncall = m("oncall-m5rt", MODEL.oncall);
    await join(top, oncall, true);
    await sleep(1400);
    if (my !== run) return;
    plugin.detail = "1 new in #alerts";
    render();
    flash(rows.get(plugin));
    await sleep(700);
    oncall.state = "working";
    oncall.since = performance.now();
    render();
    flash(rows.get(oncall));
    await sleep(1400);
    plugin.detail = "watching #alerts";
    render();
    await sleep(2300);
    if (my !== run) return;
    stage.classList.add("dim");
    await sleep(700);
  }

  // Members already at work turn idle and back now and then, as they do.
  // The on-call member only works when an alert comes.
  function stir() {
    const members = order(top, "", []).map((o) => o.node).filter((n) =>
      n.kind === "member" && n.name !== "oncall-m5rt" && !(n.since && performance.now() - n.since < 3000));
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
  // the lines that made them and the rows they made tinted.
  function stillFrame() {
    const lead = reset();
    for (const edit of [...REVIEWER, ...QA, ...ONCALL]) {
      const f = files[edit.file];
      let at = edit.after === undefined ? f.lines.length : edit.after;
      for (const t of edit.lines) {
        const ln = addLine(f, t, false, at++);
        if (f.name.endsWith(".yaml")) ln.div.classList.add("fresh");
      }
    }
    $('.tab[data-file="reviewer.md"]').classList.remove("unmade");
    cmdEl.textContent = `brg start ${TEAM}`;
    cmdEl.classList.add("ran");
    const qa = childTeam("qa-x2b9", "qa");
    qa.children.push(m("lead-h4nw", MODEL.qaLead, "working"));
    lead.children.push(m("reviewer-p7xm", MODEL.reviewer, "idle"), qa);
    top.push(m("oncall-m5rt", MODEL.oncall, "working"));
    plugin = { name: "chat", detail: "1 new in #alerts" };
    render(false);
    for (const [node, row] of rows) if (node !== lead && node.name !== "coder-7k2m") row.classList.add("fresh");
  }

  if (still) {
    stillFrame();
    return;
  }
  (async () => { for (;;) await story(); })();
  setInterval(() => { spin++; render(); }, SPIN_MS);
  setTimeout(stir, 3000);
})();
