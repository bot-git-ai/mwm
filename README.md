# mwm

A small tiling window manager for macOS. It keeps your windows in columns —
like [i3](https://i3wm.org) — on every screen and desktop, and puts them back
where they belong whenever a window appears, moves or closes.

How many columns you keep is up to you, and the number can be fractional:
with `2.5` the screen shows two full columns and a half-width one on the
right; with `1` every window stacks in a single column.

## Requirements

- macOS
- The Accessibility permission, which macOS requires before any app may read
  and move other apps' windows. mwm uses it to read window positions and
  sizes, to set them, and to notice when a window opens, moves, resizes or
  closes.

## Install

```sh
cargo install --path .
```

## Run

Generate the LaunchAgent file and load it:

```sh
mwm launchd-plist > ~/Library/LaunchAgents/mwm.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/mwm.plist
```

Grant the Accessibility permission to the installed `mwm` binary in
**System Settings → Privacy & Security → Accessibility**, then start it
again. To stop it:

```sh
launchctl bootout gui/$(id -u)/mwm
```

The daemon listens on `$XDG_RUNTIME_DIR/mwm-$UID.sock`, or
`/tmp/mwm-$UID.sock` when that variable is not set. Every command below
talks to the running daemon.

## Commands

| Command | What it does |
| --- | --- |
| `mwm focus left\|right\|up\|down` | move keyboard focus |
| `mwm move left\|right\|up\|down` | move the focused window |
| `mwm goto-desktop 1..10` | switch desktop (Space) |
| `mwm columns <number>` | set how many columns to keep, e.g. `2.5` |
| `mwm fullscreen` | fullscreen the focused window, or take it back |
| `mwm close` | close the focused window |
| `mwm retile` | re-apply the layout now |
| `mwm status` | one-line status: columns, windows, socket |
| `mwm stop` / `mwm restart` | stop or restart the daemon |

## Default keybindings

| Keys | Action |
| --- | --- |
| `alt-h` / `alt-j` / `alt-k` / `alt-l` | focus left / down / up / right |
| `cmd-←` / `cmd-↓` / `cmd-↑` / `cmd-→` | focus left / down / up / right |
| `shift-alt-h` / `shift-alt-j` / `shift-alt-k` / `shift-alt-l` | move the window |
| `shift-cmd-←` / `shift-cmd-↓` / `shift-cmd-↑` / `shift-cmd-→` | move the window |
| `alt-1` … `alt-9`, `alt-0` | switch to desktop 1 … 10 |
| `shift-alt-q` | close the focused window |
| `alt-f` | fullscreen |
| `alt-r` | retile |
| `shift-alt-r` | restart |
| `ctrl-alt-1` / `ctrl-alt-2` / `ctrl-alt-3` | 1 / 2 / 3 columns |
| `ctrl-alt-4` / `ctrl-alt-5` | 2.5 / 1.7 columns |
| `alt-space` | status |

## Your own keybindings

To replace the defaults, put a `keybindings.json` in your configuration
directory:

```sh
$XDG_CONFIG_HOME/mwm/keybindings.json      # usually ~/.config/mwm/keybindings.json
```

If that file exists, mwm uses it instead of the defaults; if it does not, the
defaults apply and there is nothing to do. The file maps a chord to a command,
using the commands from the table above:

```json
{
  "alt-h": "focus left",
  "shift-alt-h": "move left",
  "ctrl-alt-4": "columns 2.5",
  "shift-alt-q": "close"
}
```

Chord modifiers may be written as `cmd`, `ctrl`, `alt` and `shift`, with
either `-` or `+` between the parts (`shift-cmd-left`, `shift+cmd+left`).

You can also point the daemon at a specific file, which is useful for testing
an alternative set without replacing the one you use:

```sh
mwm daemon --keybindings /path/to/keybindings.json
```

## Releases

Every `v*` tag builds a release automatically: the binary is built, packaged
with the license and this file, and published with notes generated from the
commits since the previous tag.

```sh
git tag v1.1.0 && git push origin v1.1.0
```

Once a release is published its tag and its files cannot be changed or deleted:
the repository protects release tags. To try a new version before committing to
it, use a pre-release (`v1.1.0-rc1`) — those are ordinary tags and can be
replaced until you cut the real one.

## Troubleshooting

- **Nothing happens when I press a key.** Check `mwm status` — it tells you
  whether the daemon is running, how many columns it keeps and where its
  socket is. If the daemon is not running, load the LaunchAgent as above.
- **Windows do not move.** The Accessibility permission is the usual cause;
  macOS grants it per binary, so re-grant it after reinstalling.
- **Several displays.** Every display is tiled independently, with its own
  columns.
- **A window is left alone.** Only ordinary document windows are tiled.
  Panels, sheets and other auxiliary windows keep the size and position
  their app gave them, as do windows too small to tile.

## License

[AGPL-3.0](LICENSE.md)
