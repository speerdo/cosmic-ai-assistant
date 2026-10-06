# Security

cosmo gives a microphone, and a language model, the ability to act on your
desktop. This file says what it defends against, what it doesn't, and how
to report a problem.

## Reporting

Please report vulnerabilities privately through GitHub's "Report a
vulnerability" on the repository's Security tab, not as a public issue.
This is an early, single-maintainer project: expect an acknowledgement
within a few days, not hours.

## Threat model

**What cosmo treats as untrusted:** anything that reaches the microphone
(a video, a call, another person, your own speakers), and every piece of
text the model reads (web pages, search results, news feeds, terminal
output, tool results). Any of them can try to steer the model.

**What stands between that and your machine:**

| Defence | What it does |
|---|---|
| Policy gate | Every tool call is judged before it runs: allow, hold for confirmation, or deny. The gate is stricter than the MCP agent's own hints. |
| Hold and confirm | Destructive actions wait. A spoken "confirm" counts only if it was spoken during a physical Right Ctrl hold, so nothing the mic picks up can approve an action. A confirmation and the request it approves can't come from the same model response. |
| Command matcher | Shell commands (and any text typed through `dictate`) are checked against deny and hold lists. Wrappers like `bash -c` don't launder a command. |
| Reflex allowlist | The no-model fast path can only reach a fixed set of safe verbs. |
| No `run_shell` | The MCP agent's raw shell tool is never registered. |
| Lock awareness | Screen, click and typing tools refuse while the session is locked or its state is unknown. |
| `dictate` | Types one line, refuses any control character (so never Enter, Tab or Esc), and is checked like a command. |
| `read_page` and `open_url` | http(s) only. `read_page` also refuses `localhost`, private, link-local and similar addresses, checked on every redirect hop, so a page can't aim cosmo at your own network. |
| Secrets | API keys are kept in the Secret Service and redacted from logs. The daemon socket is mode `0600` in `$XDG_RUNTIME_DIR`. `profile.json` is `0600`. |
| Models | Downloaded per user, each file checked against a pinned SHA-256. |

## Known limits (read these)

- **The reasoning provider sees your requests.** Speech is transcribed on
  your machine, but the text of what you ask, plus page text and tool
  results, goes to the cloud model you choose. `cosmo use local` keeps it
  all on the machine.
- **Prompt injection is mitigated, not solved.** A hostile web page can
  still try to steer the model. The gate decides what runs, but actions
  that are allowed without confirmation (opening a URL, launching an app,
  typing text, changing volume or workspace) can be attempted by an
  injected page. In particular, opening a URL puts whatever the model
  puts in it into your browser's address bar, which can carry data out in
  the query string. Hold the key and confirm for anything you didn't ask
  for, and don't ask cosmo to read pages you don't trust while sensitive
  things are open.
- **`dictate` types where focus is.** It cannot tell a password field from
  any other. Focus the field you mean before you ask. It never submits.
- **The desktop agent is third-party code.** `computer-use-linux` is an
  npm package that cosmo drives over MCP, and cosmo is tested against
  version 0.5.0 (install it pinned: `npm install -g
  @agent-sh/computer-use-linux@0.5.0`). Treat it as you would any global
  npm tool: it runs as you.
- **The daemon runs as you, unsandboxed.** The systemd unit does not
  confine it, because the apps it launches and the terminal it runs
  commands in would inherit any confinement. Run it only on a machine you
  trust.
- **COSMIC doesn't publish its lock state.** cosmo infers it (logind's
  `Lock` signal plus whether a window is active). It fails closed, but it
  is an inference.
- **The trigger key is read from evdev** through logind's `uaccess` ACL,
  not the `input` group. The daemon sees only the trigger keycode; it is
  not a keylogger and says so in code (`docs/cosmo-blueprint.md`).

## For users

- Prefer a pinned install of the desktop agent.
- Use `cosmo doctor` after install, and again after upgrades.
- Keep the wake word off in shared or noisy rooms. The key hold is the
  only thing that can confirm an action either way.
