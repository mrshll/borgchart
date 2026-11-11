# site

The borgchart.ai page: `brg status -w` and `man brg`, as a terminal would print them. Static files with no build step or dependencies; serve this folder from any static host, or open `index.html` straight from disk.

- **Command text** lives in one place, the `COMMANDS` list in `index.html`: the first sentence of each command's `brg --help` entry.
- **The animated tree** is `status.js`: a fixed fictional team drawn with brg's marks, columns and spinner. Its header names the source each part was copied from; the palette is in `style.css`.
