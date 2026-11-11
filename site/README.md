# site

The borgchart.ai page: `brg status -w` and `man brg`, as a terminal would print them. Static files with no build step or dependencies; serve this folder from any static host, or open `index.html` straight from disk.

- **Command text** lives in one place, the `COMMANDS` section in `index.html`: grouped and worded as `brg --help` lists the commands (`GROUPS` in `src/cli.rs`), each with its one-line summary.
- **The animated tree** is `status.js`: a fixed fictional team drawn with brg's marks, columns and spinner. Its header names the source each part was copied from; the palette is in `style.css`.

## Deploying

`.github/workflows/site.yml` publishes this folder to Cloudflare as the Worker `borgchart-site`, serving these files as static assets on `borgchart.ai`. It runs on pushes to `main` that change `site/` and by hand (`gh workflow run site.yml --ref main`); never for pull requests or forks. There is no build step and no Wrangler configuration file: the deploy command names the folder, Worker, domain and compatibility date, and `.assetsignore` keeps this README off the site.

It needs two repository secrets, set once by the account's owner:

- `CLOUDFLARE_ACCOUNT_ID`: the Cloudflare account's ID.
- `CLOUDFLARE_API_TOKEN`: an account API token from the **Edit Cloudflare Workers** template, scoped to that account and the `borgchart.ai` zone. The first deploy creates the Worker, which needs Workers Admin on the account; attaching the custom domain needs Workers Routes Edit on the zone. If the first run fails with an authorization error, the token is missing one of these.

The `borgchart.ai` zone must be on that Cloudflare account, with no existing DNS record on the bare hostname: the deploy creates it. Without the secrets the workflow stops and names the missing one.
