# herdr-notifications (rich fork)

Rich native OS desktop notifications for [herdr](https://herdr.dev) agent
status changes — enough context to act on without opening herdr, and one
click to land on the exact agent pane that needs you.

Fork of [`quinnjr/herdr-notifications`] with:

- **Rich, deterministic context** — the notification title/body are built
  from herdr's own metadata (agent kind, workspace/tab labels, stripped
  terminal title, `agent explain` matched rules, and a small extracted
  prompt excerpt from the detection snapshot). **No LLM/model/API calls,
  ever** — enrichment is pure local rule evaluation and text extraction.
- **Click-to-focus that crosses the compositor boundary** — clicking runs
  `herdr agent focus <pane_id>` (right workspace → tab → pane) and then
  foregrounds the correct terminal *window* on Niri by matching the
  window-title marker herdr writes (`[ui] window_title`), never just "any
  WezTerm".
- **herdr-native timing semantics** — honours herdr's `[ui.toast]
  delay_seconds`, re-verifies the pane is still in the same state after the
  delay, skips the active tab of the focused workspace (like herdr's own
  popups), and dedupes via a generation-tagged state file so a
  `blocked → working → blocked` cycle notifies exactly once.
- **Privacy hardening** — ANSI/control stripping, box-glyph removal,
  whitespace normalisation, length bounds, and redaction of obvious
  secrets/tokens before anything leaves the pane.

Set herdr's own popups to `[ui.toast] delivery = "off"` so this plugin fully
owns popup delivery; herdr's independent `[ui.sound]` keeps playing sounds.

## Install

From a checkout:

```sh
herdr plugin install quinnjr/herdr-notifications   # upstream (basic)
# or, this fork:
herdr plugin link /path/to/herdr-rich-notifications
cargo build --release   # link does not build; do it yourself
```

Nix/Home Manager deployment (the motivating setup) lives in the consuming
host config: build the crate with `rustPlatform.buildRustPackage`,
substitute the store binary path into `herdr-plugin.toml`, and link the
resulting plugin dir idempotently at activation time.

## Configuration

Optional `config.toml` under the plugin's config directory
(`herdr plugin config-dir quinnjr.herdr-notifications`):

```toml
statuses = ["blocked", "done"]   # which statuses notify
delay_ms = 1000                  # overrides herdr's toast delay when set
suppress_active_tab = true       # skip the focused workspace's active tab
detail = "rich"                  # rich | minimal (no terminal excerpts)

click_to_focus = true
click_wait_secs = 600            # how long the notification stays clickable
expire_secs = 30                 # requested on-screen lifetime

[niri]
enabled = true                   # compositor foregrounding (Linux)
app_id = "wezterm"               # case-insensitive app_id substring
title_marker = " · herdr"        # window-title marker identifying herdr
focus_timeout_ms = 2000          # bounded wait for the title to settle
poll_interval_ms = 100
```

Pair `title_marker` with herdr's `[ui] window_title` template, e.g.
`window_title = "{hostname}: {workspace} · herdr"`. After `herdr agent
focus`, every herdr client's title then carries the workspace label, so the
plugin can pick the right window among several terminals.

## Commands

- `herdr-notifications event` — the `pane.agent_status_changed` hook.
- `herdr-notifications notify --smoke` — real rich notification for the
  focused agent pane, including click-to-focus (bound as the
  `test-notification` plugin action).
- `herdr-notifications inspect [pane]` — print the title/body (and the
  deterministic evidence behind them) without notifying.

Logs go to stderr and are captured by herdr (`herdr plugin log list`).

## How click-to-focus resolves the window

1. `herdr agent focus <pane_id>` via the event's own `HERDR_BIN_PATH` /
   `HERDR_SOCKET_PATH` (correct session, always attempted).
2. Poll `niri msg --json windows` (bounded, state-based): prefer the window
   whose `app_id` matches, whose title contains the marker, and whose title
   now contains the target workspace label (herdr refreshes the outer title
   after the focus command; polling waits out that propagation without
   fixed sleeps). Fall back to a unique marker match. Ambiguity among
   herdr windows is safe — with server-global focus they all display the
   workspace just focused.
3. `niri msg action focus-window --id <id>`.

Failures at any step are logged and never prevent the other step.

## License

MIT — see LICENSE. Fork maintained from `quinnjr/herdr-notifications`
(upstream history preserved in this repository's `main`).

[`quinnjr/herdr-notifications`]: https://github.com/quinnjr/herdr-notifications
