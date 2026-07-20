# herdr-notifications

Native OS desktop notifications for [herdr](https://herdr.dev) agent status
changes — get pinged by your system's real notification center when an
agent needs you or finishes a task, instead of having to keep an eye on the
terminal.

- **Cross-platform**: Linux, macOS, and Windows via [`notify-rust`](https://github.com/hoodie/notify-rust)
  (also builds and runs on the BSDs, though herdr's own plugin manifest
  schema doesn't have a platform value for them yet).
- **Only notifies when it matters**: fires on `blocked` (agent needs input)
  and `done` (agent finished), and dedupes so an unchanged status never
  re-notifies — but a `blocked → working → blocked` cycle correctly notifies
  again, since it's not a repeat.
- **Click to focus**: clicking a status-change notification focuses the
  originating pane back in herdr.
- **Zero runtime config required**: works out of the box; the dedup state
  lives under herdr's own per-plugin state directory (or a per-user local
  data directory as a fallback), never a shared/world-writable location.

## Install

```sh
herdr plugin install quinnjr/herdr-notifications
```

herdr builds the plugin (`cargo build --release`) on install, then wires up
the `pane.agent_status_changed` event automatically.

## Usage

Nothing to configure — once installed and enabled, notifications just
happen. To confirm your OS notification permissions/backend are working
without waiting for a real agent-status change, run the bundled smoke-test
action from herdr's command palette or:

```sh
herdr plugin action invoke test-notification --plugin quinnjr.herdr-notifications
```

## How it works

herdr fires a `pane.agent_status_changed` event (idle / working / blocked /
done / unknown) for every agent pane whenever its status changes. This
plugin's binary is invoked once per event:

1. Every status transition is recorded to a small on-disk dedup table (one
   entry per pane), written atomically (temp file + rename) and guarded by
   a short-lived exclusive lock, so concurrent status changes across
   multiple panes can't corrupt or race on it.
2. Only `blocked` and `done` are surfaced as notifications — `idle` /
   `working` / `unknown` are recorded (so the next `blocked`/`done` is
   correctly recognized as new) but never notify on their own.
3. The notification is shown on a background thread with a bounded wait, so
   a stuck notification daemon can never hang the process indefinitely.
4. If you click the notification, the plugin runs `herdr agent focus
   <pane_id>` to bring that pane back into view in herdr.

## Requirements

- herdr ≥ 0.7.0
- A working OS notification backend: a D-Bus session + notification daemon
  on Linux/BSD (present on virtually every desktop environment), or the
  native notification center on macOS/Windows.

## Development

```sh
cargo build --release
cargo test
herdr plugin link .   # develop against a local checkout instead of installing
```

## License

MIT — see [LICENSE](LICENSE).
