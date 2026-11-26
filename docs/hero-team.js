// The made-up team the first screen's editor types (hero.js draws it). Each
// beat's edits are typed in order; tests/site_hero.rs writes the template
// after the start and after every beat, with the files the editor never
// shows and the `qa` template beside it, and checks each one validates.
// The Slack tokens come from the secrets resolver (made-up op:// paths);
// `environment` names what a person exports before `brg start` (the
// #ops-high channel ID), and the test sets a stand-in for it.
// Keep the lines short: the widest has to fit a phone.
window.HERO_TEAM = {
  "team": "engineering",
  "start": {
    "template.yaml": [
      "default: claude",
      "roles:",
      "  lead: {prompt: lead.md}",
      "  coder:",
      "    prompt: coder.md",
      "    vendor: codex",
      "    parent: lead"
    ]
  },
  "beats": [
    {
      "name": "a reviewer role and its prompt",
      "edits": [
        {"file": "template.yaml", "lines": [
          "  reviewer:",
          "    prompt: reviewer.md",
          "    parent: lead"
        ]},
        {"file": "reviewer.md", "lines": [
          "---",
          "models: {claude: opus high}",
          "---",
          "Review each change a coder makes.",
          "Tell the lead what needs fixing."
        ]}
      ]
    },
    {
      "name": "a child team",
      "edits": [
        {"file": "template.yaml", "lines": [
          "  qa: {template: qa, parent: lead}"
        ]}
      ]
    },
    {
      "name": "an on-call role woken by the Slack plugin",
      "edits": [
        {"file": "template.yaml", "lines": [
          "  oncall:",
          "    prompt: oncall.md",
          "    restricted_access: true",
          "    plugins: {slack: [read, reply]}"
        ]},
        {"file": "template.yaml", "after": 1, "lines": [
          "plugins:",
          "  slack:",
          "    process: [brg-slack, watch]",
          "    delivers_to: oncall",
          "    env:",
          "      SLACK_APP_TOKEN:",
          "        from: \"op://ops/slack/app\"",
          "      SLACK_BOT_TOKEN:",
          "        from: \"op://ops/slack/bot\"",
          "      SLACK_CHANNEL_ID:  # #ops-high",
          "    actions:",
          "      read: brg-slack read <ts>",
          "      reply: brg-slack reply <ts> <text>"
        ]}
      ]
    }
  ],
  "environment": ["SLACK_CHANNEL_ID"],
  "unseen": {
    "lead.md": "---\nmodels: {claude: opus high}\n---\nSplit the work and hand it out.\n",
    "coder.md": "---\nmodels: {codex: sol high}\n---\nMake the change the lead hands you.\n",
    "oncall.md": "---\nmodels: {claude: haiku}\n---\nRead each alert. Reply in its thread with what broke.\n"
  },
  "templates": {
    "qa": {
      "template.yaml": "default: claude\nroles:\n  lead: {prompt: lead.md}\n",
      "lead.md": "---\nmodels: {claude: sonnet medium}\n---\nTest the shop the way a customer would.\n"
    }
  }
};
