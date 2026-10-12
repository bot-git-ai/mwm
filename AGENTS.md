# mwm

A small tiling window manager for macOS: it keeps every screen's windows in
i3-style columns (fractional counts like `2.5` allowed) and retiles whenever
windows appear, move or close. See `README.md` for user-facing install,
run and keybinding documentation; this file is the map to the source.

License: AGPL-3.0, see `LICENSE.md` — source files carry no per-file header.

## Build, check, test

```sh
cargo fmt --check
cargo clippy --all-targets -- -W clippy::pedantic -D warnings
cargo test
cargo build --release --locked
```

The crate compiles on any OS: non-macOS targets get a stub window system, so
the pure logic and its tests run everywhere. Behavioural checks against the
real macOS APIs (`--target aarch64-apple-darwin`) run in CI; the binaries are
built on macOS (`macos-latest`) — see `.github/workflows/`. Every push to
`master` publishes a release; there is nothing to tag by hand.

## Layout (the map)

- `src/main.rs` — entry point; everything is `daemon::cli_main`.
- `src/daemon.rs` — CLI parsing, the Unix-socket client/daemon, the event
  loop and the retile; also `mwm install` (LaunchAgent plist + launchctl).
- `src/layout.rs` — the n-column geometry and bookkeeping. Pure: no system
  calls, fully testable anywhere.
- `src/keymap.rs` — chords (`alt-h`, `shift-cmd-left`), the default bindings
  and the `keybindings.json` parser.
- `src/request.rs` — the daemon protocol: command strings to `Request`s to
  one-line JSON over the socket.
- `src/types.rs` — shared value types (`Rect`, `WindowInfo`, `ScreenInfo`,
  modifiers, directions).

## The platform seam

All system access goes through the `WindowSystem` trait in `src/platform.rs`;
everything above it is pure and testable on any OS. Implementations:

- `src/platform_darwin.rs` — the real thing: Accessibility (AX),
  CoreGraphics and CoreFoundation, hand-bound C FFI (no `cocoa` crate).
  Compiled only on macOS; holds the section dividers and the FFI-safety
  rationale in its header.
- `src/platform_stub.rs` — every operation fails or is empty; keeps
  non-macOS builds (and the pure test suite) working.

## Rules

- Keep the seam pure: never let a system call leak above `WindowSystem`.
- Every doc comment must earn its line: units, origins, invariants and
  platform quirks stay; restatements of the name go.
- Commands and keybindings are user documentation — they live in
  `README.md`, not here.
