# RubyChat Agent Guide

RubyChat is a desktop IRC client in Rust. `docs/DESIGN.md` is the design of
record: read the sections that govern a change before making it, and update
the document in the same change when a decision moves.

Status: milestones M0 (window, renderer, tab strip), M1 (IRC over TCP and
TLS, the server buffer and input line), M2 (channels and queries, sidebar,
header, nick list, chat view, commands, completion, Wayland clipboard,
context menus), M3 (saved networks and secrets, the Server Manager, SASL
PLAIN, SCRAM-SHA-256 and EXTERNAL, NickServ, certificate policies, pins and
the trust prompt, client certificates, session restore) and M4 (the
hand-built D-Bus client, the StatusNotifierItem tray and its dbusmenu,
notifications, closing to the tray, xdg-activation, single instance and
`irc://` hand-over, the desktop file and icons) and M5 (batches,
echo-message, labeled-response, away and account state, WHOX, MONITOR,
standard replies, STS, the smart filter, ignores) are implemented on Linux;
DESIGN.md §16 lists what each left for later. M6 (history and search) is
next. DESIGN.md §17 records the settled decisions.

## Binding rules

1. **No client-side lag.** The UI thread never blocks: no network, DNS, file
   or process work, and no lock an I/O thread can hold. Work that might take
   longer than about a millisecond runs on another thread while the UI draws
   a placeholder of the final size. Check hot-path changes against the
   budgets in DESIGN.md §10.
2. **No idle work.** The event loop sleeps in `ControlFlow::Wait`. A timer
   exists only for something visibly in progress.
3. **Dependencies earn their place.** A new crate needs a row in DESIGN.md §4
   giving its purpose, why it isn't hand-built, and its transitive cost
   measured with `cargo tree -e normal,build`. Protocol layers (IRC, SOCKS,
   SASL, D-Bus) are hand-built.
4. **Standards first.** Before implementing protocol or platform behavior,
   read the governing specification (DESIGN.md §18) through
   `web-standards-skill` or `/big/web-standards/rfc/text/`. When a needed
   specification is missing from the library, add it rather than working from
   memory. Cite the specification and section in a nearby comment or the
   commit message when it will help the next reader.
5. **Bounded and contained.** Every buffer, cache and queue has a cap. A bad
   message, server, font or platform service fails locally.

## Visual reference

trust-desktop (`../TRust`) defines the look. The tab strip ports from
`src/render/tab_strip.rs` and `src/bin/trust-desktop/window_chrome.rs`;
colors come from `src/theme.rs` and the COMMAND panel
(`src/render/command_panel.rs`). TRust writes its strip previews with
`TRUST_TAB_STRIP_PREVIEW=<dir> cargo test --release --lib tab_strip_preview -- --ignored`,
run in `../TRust` (it rebuilds TRust's tests when they are stale). Never
modify TRust from this project.

## Build and verify

```sh
cargo build
cargo test
cargo test --release
cargo clippy --all-targets
cargo fmt --check
cargo build --release
```

Run focused tests while iterating, then the full set before handoff.
Performance benchmarks are `#[ignore]`d; run them explicitly after touching
rendering, layout or event handling. Check user-visible behavior with the
release binary. Never install or replace an installed RubyChat binary without
the user's explicit approval.

## Diagnostics

- `RUBYCHAT_TRACE=1` logs every frame's paint and present times, its damage
  and buffer age, and every event-loop wake. An idle window logs nothing.
- `RUBYCHAT_EXIT_AFTER=SECONDS` quits cleanly (QUIT to every server) after
  that long; `RUBYCHAT_SNAPSHOT=FILE.png` saves the window's last frame on
  exit. Together they check the real window without screenshots, for
  example `RUBYCHAT_EXIT_AFTER=9 RUBYCHAT_SNAPSHOT=scratch/shot.png
  target/release/rubychat irc.libera.chat` (`scratch/` is ignored by git).
- RubyChat now keeps a session and pins: runs that connect anywhere should
  use throwaway directories, so the user's next start isn't changed, for
  example `XDG_CONFIG_HOME=$PWD/scratch/xdg/config
  XDG_STATE_HOME=$PWD/scratch/xdg/state target/release/rubychat …`. A
  `networks.toml` there tries saved networks, logins included. The single
  instance is keyed by the executable and configuration directory, so such
  a run never hands over to the user's own RubyChat (and a second launch
  with the same directories hands over to the first).
- The tray and notifications are real D-Bus traffic: `busctl --user` reads
  the item (`org.kde.StatusNotifierItem-<pid>-1`, `/StatusNotifierItem`,
  menu at `/MenuBar`; `Event isvu -- 1 clicked s "" 0` clicks Show/Hide)
  and `makoctl list -j` / `makoctl invoke -n <id> default` see and click
  notifications. Prefer `--tray` for checks that don't need the window: a
  window that opens takes the keyboard from whatever the user is doing.
- `cargo test --lib session_bus -- --ignored --nocapture` says Hello to
  the session bus; `live_tray` shows a tray item and a notification for a
  few seconds (`RUBYCHAT_LIVE_SECONDS`); `activates_a_surface` checks
  xdg-activation on the running compositor with an unmapped surface.
- `python3 tools/gen_icons.py` regenerates `assets/desktop/icons/` from the
  logo (ImageMagick).
- IRCv3 behavior is checked on a local ergo in `scratch/` (download the
  official release into its own directory and check its SHA-256 against the
  release's checksum file): plaintext on 127.0.0.1:16670 with STS to TLS on
  16697 under a self-signed `localhost` certificate, pinned in a scratch
  `known_hosts`; accounts registered through NickServ, one always-on for
  bouncer checks. A saved network's `commands` script what RubyChat sends,
  and a small client with SASL plays the other side.
- `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib snapshots -- --ignored` writes the
  tab strip, window, context menu, Server Manager, trust prompt and tray
  icon snapshots; `tab-strip-parity.png` uses trust-desktop's preview titles for
  side-by-side comparison.
- `--demo` opens placeholder networks with channels, members, formatting,
  links, highlights and unread badges: the whole M2 window without
  connecting. `rubychat 'ircs://irc.libera.chat/##rubychat-test'` connects
  and joins a channel (IRC URLs, DESIGN.md §17).
- `cargo test --release --lib paint_budgets -- --ignored --nocapture` checks
  the §10 frame budgets at 2560 × 1440.
- `RUBYCHAT_CHECK_CLIPBOARD=1` pastes the clipboard's text into the input,
  then copies a check string, once the window has the keyboard. With
  `wl-copy` before and `wl-paste` while it runs, it checks both directions;
  save and restore the user's clipboard around it.
- `cargo test --lib connects_to_libera -- --ignored --nocapture` registers
  on Libera.Chat over TLS and reads the MOTD.
- `python3 tools/gen_parser_vectors.py` regenerates `src/irc/vectors.rs`
  from the ircdocs parser tests in the standards library.

## Conventions

- Rust 2024 edition, stable toolchain.
- Unit tests live beside the code in `#[cfg(test)]` modules. Protocol
  scenarios run the network thread against a scripted in-process server
  (`net::tests`). App behavior runs headless (`app::tests`, with
  `Options::isolated`), capturing what would be sent.
- The development machine is aarch64 Linux running labwc (Wayland), with
  sfwbar's tray and mako for notifications.
