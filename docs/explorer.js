// The team explorer at the foot of the page: the repository's own team
// files, listed like `tree` on the left, the chosen one shown on the right.
// The files come from teams.js, which tests/site_teams.rs generates from the
// team folders; it also names the file to open on.
"use strict";

(function () {
  const data = window.TEAM_FILES;
  const list = document.getElementById("team-tree");
  const shown = document.getElementById("team-file");
  const cat = document.getElementById("team-cat");
  if (!data || !list || !shown || !cat) return;

  const esc = (s) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

  // Light colouring, line by line: YAML comments and Markdown front matter
  // recede, keys and headings are bold, and so is **bold** text.
  function yamlLine(line) {
    const hash = line.search(/(^|\s)#/);
    const body = hash < 0 ? line : line.slice(0, hash);
    const note = hash < 0 ? "" : `<span class="faded">${esc(line.slice(hash))}</span>`;
    return esc(body).replace(/^(\s*(?:- )?)([\w.-]+)(:)/, '$1<b>$2</b>$3') + note;
  }
  function markdownLine(line) {
    if (/^#{1,6} /.test(line)) return `<b>${esc(line)}</b>`;
    return esc(line).replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>");
  }
  function painted(path, text) {
    const lines = text.replace(/\n$/, "").split("\n");
    if (path.endsWith(".yaml")) return lines.map(yamlLine).join("\n");
    // A prompt's front matter (its models) is YAML between --- lines.
    let front = lines[0] === "---";
    return lines.map((line, i) => {
      if (front && i > 0 && line === "---") {
        front = false;
        return `<span class="faded">${esc(line)}</span>`;
      }
      if (front) return i === 0 ? `<span class="faded">${esc(line)}</span>` : yamlLine(line);
      return markdownLine(line);
    }).join("\n");
  }

  // The files by folder, in the order teams.js lists them.
  const folders = [];
  for (const file of data.files) {
    const cut = file.path.lastIndexOf("/");
    const dir = file.path.slice(0, cut + 1);
    let group = folders.find((f) => f.dir === dir);
    if (!group) folders.push((group = { dir, files: [] }));
    group.files.push({ name: file.path.slice(cut + 1), path: file.path, text: file.text });
  }
  const top = data.start.slice(0, data.start.indexOf("/") + 1);

  // Drawn like `tree`: the furniture recedes, each file is a button.
  const lines = [`<b>${esc(top)}</b>`];
  folders.forEach((group, i) => {
    const lastDir = i === folders.length - 1;
    lines.push(`<span class="faded">${lastDir ? "└─ " : "├─ "}</span><b>${esc(group.dir.slice(top.length))}</b>`);
    group.files.forEach((file, j) => {
      const fork = (lastDir ? "   " : "│  ") + (j === group.files.length - 1 ? "└─ " : "├─ ");
      lines.push(`<span class="faded">${fork}</span><button type="button" class="leaf" data-path="${esc(file.path)}">${esc(file.name)}</button>`);
    });
  });
  list.innerHTML = lines.join("\n");

  const byPath = new Map(folders.flatMap((g) => g.files).map((f) => [f.path, f]));
  const buttons = Array.from(list.querySelectorAll("button.leaf"));
  function open(path) {
    const file = byPath.get(path);
    if (!file) return;
    cat.innerHTML = `<span class="faded">$</span> cat ${esc(file.path)}`;
    shown.innerHTML = painted(file.path, file.text);
    // YAML keeps its lines, as `cat` in a wide terminal would; prose wraps.
    shown.classList.toggle("wrap", !file.path.endsWith(".yaml"));
    shown.scrollTop = 0;
    shown.setAttribute("aria-label", file.path);
    for (const b of buttons) {
      if (b.dataset.path === path) b.setAttribute("aria-current", "true");
      else b.removeAttribute("aria-current");
    }
  }
  list.addEventListener("click", (e) => {
    const b = e.target.closest("button.leaf");
    if (b) open(b.dataset.path);
  });
  open(data.start);
})();
