# RubyChat design

Draft 2 · 30 October 2026 · decisions settled (§17); milestones M0 to M3
are implemented (§16).

RubyChat is a desktop IRC client written in Rust. Its overriding goal is that
nothing on the client side ever makes you wait. It draws its own interface in
trust-desktop's visual language on the CPU, speaks IRC through a hand-built
protocol layer on its own network thread, and keeps its dependencies to a
short, justified list.

Vocabulary: a **network** is a saved server definition and, while connected,
one tab. A **buffer** is one conversation view inside a network: the server
buffer, a channel, or a query (a private conversation).

## 1. Decisions at a glance

| Area | Decision |
|---|---|
| Platforms | Linux (Wayland first, X11) on x86-64 and ARM64; Windows x64 |
| Window | winit, undecorated; the network tab strip is the title bar, ported from trust-desktop |
| Rendering | Own CPU compositor into a softbuffer surface with damage tracking; no GPU, no UI toolkit |
| Text | skrifa (font data, outlines, light hinting, emoji bitmaps) and harfrust (shaping); own rasterizer and glyph cache |
| Networking | One mio reactor thread for every connection; rustls with aws-lc-rs, post-quantum hybrid key exchange preferred |
| IRC | Hand-built: RFC 1459/2812, Modern IRC, IRCv3 (CAP 302, SASL, tags, batches, history) |
| Tray, notifications | Hand-built D-Bus client: StatusNotifierItem, dbusmenu, Desktop Notifications; Shell_NotifyIcon on Windows |
| Configuration | Hand-parsed TOML subset in XDG directories; passwords in a separate owner-only file |
| Dependencies | 89 crates in the lock file on Linux; 97 once color emoji lands (§4) |

## 2. Goals

From the brief:

1. A highly efficient, responsive chat interface with zero client-side lag
   (made measurable in §10).
2. As few dependencies as possible.
3. Plain TCP and TLS connections.
4. A good in-app menu for managing saved servers and logins.
5. Several servers at once, one tab each, with the tabs serving as the
   window's title bar in trust-desktop's design.
6. Each server's channels listed down the left side.
7. Closing the window leaves RubyChat running as a system-tray icon.
8. A modern appearance consistent with trust-desktop.

Not planned for 1.0 (see §17): DCC, link and media previews, scripting and
plugins, macOS, a GPU renderer.

## 3. Principles

These bind every change; AGENTS.md repeats them.

1. **The UI thread never waits.** It does no network, DNS, file or process
   work and takes no lock an I/O thread can hold. Anything that might take
   longer than about a millisecond runs elsewhere while the UI draws a
   placeholder of the final size.
2. **Nothing runs while nothing happens.** No polling, no idle timers, and no
   animation except while something is visibly in progress.
3. **Every dependency earns its place** in §4. Protocol layers (IRC, SOCKS,
   SASL, D-Bus) are hand-built and auditable.
4. **Standards first.** Read the governing specification before implementing
   protocol or platform behavior (§18).
5. **Everything is bounded:** scrollback, caches, queues, and the cost of
   rendering any one message.
6. **Failures stay local.** A bad server, malformed message, missing font,
   absent tray or broken clipboard never takes down the rest.

## 4. Dependencies

Measured on 30 October 2026 with `cargo tree -e normal,build` against the local
registry, for Linux.

| Crate | Used for | Why not hand-built |
|---|---|---|
| winit 0.30, without its CSD feature | Windows, input, IME and scale factors on Wayland, X11 and Win32 | Three windowing systems; 64 crates, most of the graph |
| softbuffer 0.4 | Presenting CPU pixels with damage rectangles | Per-platform shared-memory plumbing; adds 4 crates to winit's |
| rustls 0.23 with aws-lc-rs, and webpki-roots | TLS 1.2/1.3, X25519MLKEM768, Mozilla roots; aws-lc-rs also supplies SHA-256, HMAC, PBKDF2 and randomness for SCRAM | Never hand-roll TLS |
| mio 1 | Socket readiness and a cross-thread waker | Portable epoll, kqueue and Windows AFD in 3 crates |
| skrifa 0.44 | Font tables, outlines, hinting, color and bitmap glyph data | A large, security-sensitive parsing surface |
| harfrust 0.12 | OpenType shaping (the HarfBuzz port) | A specialist domain; shares read-fonts with skrifa |
| unicode-segmentation 1 | Grapheme and word boundaries (UAX #29) | Unicode data tables; no dependencies of its own |
| png 0.18, from M7 | Decoding Noto Color Emoji's CBDT bitmaps | inflate plus PNG; 8 more crates |

Totals: 89 crates for everything but png, 97 with it. On the same base,
trust-desktop's Parley and Vello CPU text stack plus ksni for the tray would
make 190; AccessKit adds 71; arboard with Wayland data-control adds 18; zbus
on its own is 67.

Used directly because winit or rustls already brings them, with the same
features: libc (Unix), windows-sys (Windows), wayland-client and x11rb
(clipboard), and aws-lc-rs (SCRAM's SHA-256, HMAC and PBKDF2, randomness,
and certificate fingerprints).

Hand-built: the IRC parser, serializer and session state machines; Happy
Eyeballs connection racing; SOCKS5; SASL PLAIN, SCRAM-SHA-256 and EXTERNAL;
the D-Bus client and the tray, menu and notification services on it; the
Win32 tray; the clipboard on Wayland (`wl_data_device`), X11 (selections) and
Windows; the TOML subset; chat logs; the rasterizer, compositor and glyph
cache; widgets and layout; line breaking; mIRC formatting; URL detection;
local time (`localtime_r`, Win32); single-instance IPC; a PNG writer for test
snapshots.

Adding a crate means adding a row here with its measured transitive cost.

## 5. Architecture

### 5.1 Threads

```
              keyboard · pointer · IME · window events
                                     │
┌────────────────────────────────────▼────────────────────────────────────┐
│ UI thread · winit event loop                                            │
│   App: networks, buffers, members, lines, input, layout caches          │
│   → damage → paint into canvas → softbuffer present_with_damage         │
└───▲──────────────────▲──────────────────▲──────────────────▲────────────┘
    │ NetEvent         │ StoreEvent       │ DesktopEvent     │ WorkDone
┌───┴────────────┐ ┌───┴────────────┐ ┌───┴────────────┐ ┌───┴────────────┐
│ net (mio)      │ │ store          │ │ desktop        │ │ workers ×2     │
│ sockets, TLS   │ │ chat logs      │ │ D-Bus: tray,   │ │ font loading   │
│ IRC sessions   │ │ backlog reads  │ │ menu, notify   │ │ fallback index │
│ timers, flood  │ │ atomic config  │ │ Win32 tray     │ │ emoji decode   │
│ send queues    │ │ writes         │ │ instance IPC   │ │ log search     │
└────────────────┘ └────────────────┘ └────────────────┘ └────────────────┘
    ▲
    │ resolver threads (getaddrinfo), one per lookup
```

- From the UI to the other threads: `std::sync::mpsc` channels; the network
  thread is woken with `mio::Waker`.
- From the other threads to the UI: an mpsc channel each, plus one coalesced
  `EventLoopProxy` wake. A sender posts a wake only when it flips a shared
  "wake pending" flag from false to true, so a burst of ten thousand messages
  costs one wake.
- Queued work is drained in `about_to_wait`, after the iteration's input
  events, under a time budget (§10). If work remains, the UI posts itself
  another wake, so input that arrives meanwhile is handled first.

### 5.2 Who owns what

- The **network thread** owns transport and the protocol work that must not
  wait on the UI: connecting, TLS, registration, capability negotiation,
  SASL, nick fallback during registration, PING and PONG, lag probes, STS,
  reconnect backoff and the flood-control send queues. Once a connection is
  registered it forwards every parsed message.
- The **UI thread** owns IRC semantics and presentation: membership, modes,
  topics, reply routing, unread state, highlights, layout and painting.
  Applying a message takes microseconds; layout happens only for lines that
  become visible.
- The **store thread** owns files. The UI thread never opens one.

### 5.3 The window is disposable

The app outlives its window. Closing to the tray drops the window and its
surface on Wayland, where winit cannot hide a window (`set_visible` is
unsupported there), and hides it on X11 and Windows. Restoring creates a
window at the saved size and repaints once; every cache survives. The first
time, a notification explains that RubyChat is still running in the tray.

Without a tray host, closing quits: each server is sent QUIT, with at most one
second to flush.

## 6. Networking and IRC

### 6.1 Connecting

1. Resolve on a short-lived resolver thread (`getaddrinfo` blocks).
2. Race the addresses as Happy Eyeballs v2 does (RFC 8305): system order,
   250 ms apart, first success wins.
3. Handshake TLS when enabled. (SOCKS5 and Tor are not planned for now.)
4. Register (§6.4).
5. After a loss, reconnect with exponential backoff from 2 s to 5 min with
   ±20 % jitter, rejoining channels. `/reconnect` retries at once. An ERROR
   that reports a ban stops automatic retries.

### 6.2 TLS

- Port 6697 by default (RFC 7194). Certificates are verified against the
  WebPKI roots with RFC 9525 service-identity checks.
- A certificate that fails verification opens a prompt with its SHA-256
  fingerprint, subject, issuer and validity: **Trust once**, **Always trust
  (pin)** or **Cancel**. Pins live in `known_hosts` as `host:port sha256-hex`,
  the format TRust uses for Telnet. If a pinned server's certificate changes,
  the connection fails closed with a prominent warning.
- Per network: verify (the default), pinned, or no verification. The last must
  be chosen explicitly and shows a broken lock on the tab and in the sidebar.
- Client certificates (a PEM chain and key) for CertFP and SASL EXTERNAL.
  Generating one in the app comes later.
- STS: a plaintext connection that advertises `sts` is upgraded to TLS on the
  advertised port; the policy and its expiry are saved and enforced on later
  connections.
- The X25519MLKEM768 hybrid key exchange is preferred, as in TRust.

### 6.3 Wire format

- Lines end in CRLF; a bare LF is accepted. A message may take 512 bytes
  (CRLF included) and its tags 8191; longer lines are dropped with a warning
  in the server buffer.
- The parser follows Modern IRC and IRCv3 message-tags, tag-value escaping
  included, and is checked against the ircdocs parser-tests vectors. A parsed
  message is a single allocation: the raw line plus offsets.
- Text is decoded as UTF-8. A line that isn't valid UTF-8 is decoded with the
  network's fallback encoding (CP1252 by default, from a built-in table).
  RubyChat sends UTF-8 unless the network is set to a legacy encoding.
- Outgoing text is split to fit the limit at word and grapheme boundaries,
  counting the `nick!user@host` prefix the server will add.

### 6.4 Registration and capabilities

`CAP LS 302`, then `PASS` if configured, `NICK` and `USER`, `CAP REQ` for the
wanted capabilities, SASL, and `CAP END`. If the nickname is taken during
registration, the alternates are tried, then `_` is appended.

Requested when offered: `account-notify`, `account-tag`, `away-notify`,
`batch`, `cap-notify`, `chghost`, `echo-message`, `extended-join`,
`invite-notify`, `labeled-response`, `message-tags`, `multi-prefix`, `sasl`,
`server-time`, `setname`, `standard-replies`, `sts`, `userhost-in-names`; from
M6, `draft/chathistory`, `draft/event-playback`, `draft/read-marker` and
`draft/multiline`.

ISUPPORT tokens used: `CASEMAPPING`, `CHANMODES`, `CHANTYPES`, `PREFIX`,
`STATUSMSG`, `TARGMAX`, `NICKLEN`, `TOPICLEN`, `NETWORK`, `MONITOR`, `WHOX`,
`UTF8ONLY`, `BOT`, `CHATHISTORY`.

### 6.5 Logging in

Server password; SASL PLAIN (RFC 4616), SCRAM-SHA-256 (RFC 5802, RFC 7677)
and EXTERNAL (RFC 4422 appendix A); NickServ IDENTIFY for networks without
SASL. With "require login" set, a failed SASL exchange disconnects instead of
continuing unidentified.

As built (IRCv3 SASL 3.1 and 3.2): `sasl` is requested only when its
`CAP LS` value offers the chosen mechanism, or names none; `CAP END` waits
for the exchange to finish. Responses go in 400-byte base64 chunks. With a
required login, a server that doesn't offer SASL, refuses the capability,
fails the exchange or registers the client without it is disconnected
without retrying, as the specification asks. SCRAM checks that the server's
nonce extends the client's and verifies the server's signature, abandoning
the exchange with `AUTHENTICATE *` otherwise; it refuses iteration counts
above 100,000, which would stall the network thread, and credentials that
aren't plain ASCII (§17). NickServ IDENTIFY goes after `001`.

### 6.6 Keepalive and lag

After 90 s without traffic RubyChat sends PING; without a reply in 60 s it
reconnects. A periodic PING measures lag, shown in the tab's tooltip and the
sidebar header.

### 6.7 Flood control

Each connection's send queue follows the penalty timer of RFC 1459 §8.10: by
default a burst of five lines, then one every two seconds, adjustable per
network. PONG and QUIT skip the queue, and so does registration, a short
fixed exchange that servers expect at once. Local echo never waits for it, so
sending always feels instant; while a long paste drains, the input bar shows
how many lines are still queued.

### 6.8 Semantics on the UI thread

- Casemapping per `CASEMAPPING` (`ascii`, `rfc1459` and its strict variant,
  `rfc7613`), `rfc1459` when the server doesn't say.
- Membership from NAMES (with `multi-prefix` and `userhost-in-names`), JOIN,
  PART, QUIT, KICK, NICK and MODE; away and account state from `away-notify`,
  `account-notify`, `extended-join`, and a rate-limited WHOX after joining;
  `chghost` and `setname`.
- Channel modes by `CHANMODES` class (A to D) and `PREFIX`.
- Replies go to the buffer that asked: by `labeled-response` label when
  offered, otherwise by the issuing buffer (WHOIS to the current buffer),
  falling back to the server buffer.
- Netsplit and netjoin batches collapse into one line. History is inserted by
  server time and de-duplicated by `msgid`.
- CTCP ACTION, VERSION, PING, TIME and CLIENTINFO are answered, rate-limited
  and individually switchable. DCC offers appear as inert notices.
- Commands: `/join /part /msg /query /me /notice /nick /topic /mode /kick /ban
  /invite /whois /who /away /quote /list /ignore /close /clear /connect
  /disconnect /reconnect /server /ctcp /search`, plus user aliases.

## 7. Data model and memory

- IDs: `NetworkId`, `BufferId`, and a per-buffer monotonic `LineId`.
- A `Line` holds its UTC timestamp, kind (message, action, notice, join,
  part, quit, nick, mode, topic, kick, info, error), interned sender, raw
  text with formatting codes, flags (highlight, own, pending echo, history)
  and optional `msgid`. Formatting spans are derived when the line is laid
  out.
- Scrollback is a `VecDeque<Line>` per buffer, capped at 5,000 lines by
  default (2,000 for server buffers). Older lines stay in the log on disk and
  page back in asynchronously when you scroll past the top, behind a "loading
  earlier messages" row, or come from `chathistory` on bouncers.
- Members are kept in a vector sorted by prefix rank, then casefolded nick,
  with an index for lookups; rendering walks the vector directly.
- Nicks are interned per network.
- Each buffer tracks message and highlight counts, an activity level, and its
  last-read position (synchronized with `draft/read-marker` when offered).
- Other caps: 200 history entries per input, a layout cache by line count,
  and the glyph and emoji caches by bytes (32 MiB and 16 MiB by default).

## 8. Interface

### 8.1 Layout

Illustrative; proportions are approximate.

```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ ■ Libera.Chat  @2 ✕ ▏ ■ OFTC         ✕ ▏ ◌ hackint      ✕   +            ╲╲╲     ─    □    ✕ │
├───────────────────┬──────────────────────────────────────────────────────┬───────────────────┤
│ LIBERA.CHAT       │ #rust   +nt · 1,203 members                          │ OPERATORS · 4     │
│ ● TLS · 38 ms     │ Rust programming | https://rust-lang.org | …         │ @ferris           │
│                   ├──────────────────────────────────────────────────────┤ @rustbot          │
│ >_ server         │ 12:01     ferris │ hello world                       │ VOICED · 2        │
│ CHANNELS          │ 12:02       ruby │ a longer message that wraps       │ +alice            │
│ # rust         14 │                  │ with a hanging indent             │ USERS · 1,197     │
│ # labwc        @2 │ 12:03  → bob joined                                  │ bob               │
│ # rust-beginners  │ ─────────────────── NEW MESSAGES ─────────────────── │ carol             │
│ QUERIES           │ 12:04      carol │ ruby: have you seen this?         │ dave              │
│ ◇ alice         • │ 12:04       ruby │ not yet!                          │ …                 │
│                   ├──────────────────────────────────────────────────────┤                   │
│ + JOIN     Ctrl+J │ ruby› type a message…                                │                   │
└───────────────────┴──────────────────────────────────────────────────────┴───────────────────┘
 sidebar · 216 px    chat view                                              nick list · 176 px
```

The default window is 1100 × 720, the minimum 640 × 400. The sidebar and nick
list resize by dragging their dividers; the sidebar can collapse to icons and
the nick list can be hidden.

### 8.2 Visual language

RubyChat uses trust-desktop's chrome: COMMAND's chamfered corners (small at the
top left, larger at the top right), one-pixel etched edges, accent bars (pink
for the selected thing, cyan for hover and focus, amber for work in
progress), small-caps section labels, amber keycap hints and decorative
traces in empty chrome. When the window loses focus, accents fall back to
dim, as trust-desktop's strip does.

| Token | Color | Use |
|---|---|---|
| strip | `#070612` | Tab strip |
| surface | `#0c091c` | Active tab, sidebar, header, nick list, overlays |
| canvas | `#0b0221` | Chat view (TRust's `BG`) |
| hover | `#141028` | Hovered tabs and rows |
| inset | `#050813` | Input fields, pressed controls |
| edge | `#2c4350` | Etched edges |
| rule | `#243042` | Separators |
| text | `#c8c8dc` | Primary text |
| muted | `#939cb3` | Secondary text, labels |
| disabled | `#585c70` | Inactive items |
| key | `#1f1b30` | Keycaps |
| pink | `#ff2bd6` | Selection, highlights, destructive hover |
| cyan | `#00fff9` | Focus, hover, links, prompt, connected |
| amber | `#ffb000` | In progress, notices, keycap text |
| dim | `#6e4e9e` | Accents while unfocused |
| heart | `#ffe8f6` | Glyphs on pink |

Typography: JetBrainsMono Nerd Font, the face trust-desktop's chrome uses.
Chrome text is 11.5 px at weight 700, like the strip; chat text is 11 pt
(14.67 px) at weight 400, with nicks at 700. Every face and size is
configurable.

Icons (channel, query, server, lock, broken lock, bell, gear, plus, close,
chevrons) are vector paths drawn in code like the strip's buttons: crisp at
any scale, with no image files.

Nick colors come from a hash of the casefolded nick into 16 colors chosen for
at least 4.5:1 contrast on the canvas; pink is reserved for highlights. mIRC's
99 colors are drawn as specified, except that a color too close to the
background is lightened. Colors can be stripped entirely.

### 8.3 Tab strip and title bar

One tab per network. The strip ports trust-desktop's geometry and behavior
from `src/render/tab_strip.rs` and `src/bin/trust-desktop/window_chrome.rs`:
26 px tall (32 px on Windows), tabs from 112 to 224 px wide, chamfered
outlines, overflow scrolling with arrows, drag to reorder, middle-click to
close, double-click the empty strip to maximize, right-click it for the window
menu, resizing from the edges, a reserved stretch for dragging the window,
decorative traces, and window buttons with the pink close hover.

- The jewel shows the connection: cyan when connected, an amber heartbeat
  while connecting (the only animation, and only then), a hollow square while
  disconnected or waiting to reconnect, pink after a failure (login refused,
  banned, certificate rejected).
- The slot trust-desktop uses for its sound indicator shows activity in
  background networks: a pink `@n` for highlights, a cyan dot for unread
  messages.
- The title is the network name; its tooltip gives the server and port, nick,
  TLS version and lag.
- `+` opens the Server Manager ready to connect.
- Closing a connected network asks inline first ("Disconnect from
  Libera.Chat? ⏎ / Esc").
- The window title still follows the active buffer (`#rust — Libera.Chat —
  RubyChat`) for task switchers.

### 8.4 Sidebar

The active network's buffers. A header names the network in small caps above
a status line (`● TLS · 38 ms`, `◌ RECONNECTING IN 8 S`, a broken lock when
unverified). Then come the server buffer (`>_`, trust-desktop's terminal
glyph), CHANNELS and QUERIES.

Each row has an icon, the name (ellipsized) and a badge: a pink count for
highlights, a cyan count for unread messages, a dim dot for other activity.
The active row has the hover fill and a 2 px pink left accent; hover adds a
fainter fill and a cyan hairline. Channels you've left stay listed, dimmed, until closed. Rows
reorder by dragging and have a context menu (part or close, mute, mark read,
copy name). The footer holds `+ JOIN` (Ctrl+J).

### 8.5 Header

The buffer name, mode string and member count, then the topic on one line with
formatting and links. Clicking it shows the whole topic and, where you're
allowed, edits it. A query shows the nick, account and away message; the
server buffer shows the server and network names.

### 8.6 Chat view

- Columns: time (`HH:MM` by default); nick, right-aligned and colored, as wide
  as the nicks in view up to a cap, then ellipsized; a one-pixel rule; and the
  message with a hanging indent.
- Joins, parts, quits, and nick, mode and topic changes are compact and
  muted. A smart filter hides joins, parts and quits from people who haven't
  spoken recently.
- Highlights get a 2 px pink left accent and a faint pink tint; notices an
  amber `-nick-`; actions `* nick` in italics; your own lines a muted nick.
- The unread marker and day changes are labeled rules (`NEW MESSAGES`,
  `FRIDAY 30 OCTOBER 2026`).
- Links and channel names are clickable; the full URL shows on hover before
  anything opens. Nicks open their context menu.
- The scrollbar is a thin overlay with a cyan thumb while hovered or moving.
  When you've scrolled up, a `▼ 12 NEW` pill returns to the present.
- Drag to select across lines. Copying a part of one line gives its text;
  copying across lines gives each as `[12:02] <ruby> text`.
- With `echo-message`, your line shows immediately, slightly dimmed, until the
  server's echo confirms it.

### 8.7 Nick list

Members are grouped by rank under labels with counts (`OPERATORS · 4`,
`VOICED · 12`, `USERS · 1,187`), with away members dimmed, a filter field for
large channels, only visible rows drawn, and a context menu (query, whois,
op/deop, voice, kick, ban, ignore, copy).

### 8.8 Input bar

COMMAND's input field: an inset surface, a 2 px cyan left edge and a cyan
`nick›` prompt. Editing moves by grapheme and word, with selection, undo,
history (Up and Down), Tab completion (nicks, most recent speakers first and
with `: ` at the start of a line; channels; commands) and inline IME preedit.
The bar grows to eight lines, then scrolls. A byte count appears near the
per-message limit. Pasting more than three lines asks before sending, or
sends one `draft/multiline` batch where the server supports it. Hints use
amber keycaps (`TAB complete · ESC cancel`).

### 8.9 Server Manager

A COMMAND-style overlay (pink `SERVERS` tag, etched frame), usable entirely
from the keyboard.

- **Quick connect:** `host[:port]`, a TLS toggle (on by default, with port
  6697, or 6667 without TLS) and a nick, then Connect. Afterwards it offers
  to save the network.
- **Saved networks**, each with its connection jewel, beside an editor:
  - Network: name, connect at startup, reconnect automatically.
  - Servers, tried in order: host, port, TLS, certificate policy (verify;
    pinned, showing the fingerprint with Forget; or no verification).
  - Identity: nick, alternates, username, real name.
  - Login: none, server password, SASL PLAIN, SASL SCRAM-SHA-256, SASL
    EXTERNAL (client certificate file) or NickServ; account and password
    (masked, with a reveal toggle); require login.
  - Channels to join (with keys) and commands to run after connecting.
  - Advanced: fallback encoding, flood-control rate, CTCP replies, quit and
    part messages.
- **Actions:** Connect or Disconnect, Save (Ctrl+S), Duplicate, Delete
  (confirmed).

It opens with Ctrl+T, the `+` button, or Edit network in a tab's context menu.
On first launch it opens as a welcome screen with quick connect and a few
well-known networks preset with their TLS addresses.

As built: Ctrl+T and `+` show the start page, the Server Manager's home:
quick connect, then the saved networks (suggested ones until one is saved)
beside the recent servers. A saved network connects with a click, or opens
in the editor with EDIT; `+ NEW` starts one. The server row's context menu
offers Edit network, or Save network for an address opened with quick
connect. In the editor, Tab and the arrows move between controls, Enter and
Space press, Left and Right change choices, Ctrl+S saves, Ctrl+Enter
connects and Esc closes, asking again when there are unsaved changes.
Passwords and channel keys stay masked until Show. Connect saves first when
there are changes, and becomes Disconnect or Reconnect for an open network.
A pinned server's row offers to forget its pin. Not yet: the tab context
menu, and advanced settings beyond encoding, flood rate, CTCP replies and
quit and part messages.

### 8.10 Other surfaces

- Quick switcher (Ctrl+K): fuzzy search across every buffer of every network,
  most active first.
- Context menus in trust-desktop's menu design.
- Dialogs: certificate trust, close confirmation, paste confirmation, join.
- Search (Ctrl+F) through scrollback and the logs on disk, with matches
  highlighted in place.
- Settings: faces and sizes, timestamps, filters, highlight words and
  ignores, notifications, tray, logging, link handlers.

### 8.11 Keyboard

| Keys | Action |
|---|---|
| Ctrl+T | Server Manager |
| Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+1–9 | Next, previous, nth network |
| Alt+Up, Alt+Down, Alt+1–9 | Previous, next, nth buffer |
| Alt+A | Next buffer with activity, highlights first |
| Ctrl+K | Quick switcher |
| Ctrl+J | Join a channel |
| Ctrl+W | Close the buffer (part, or close the query) |
| Ctrl+F | Search |
| PageUp, PageDown, Ctrl+End | Scroll; return to the present |
| F7, F8 | Toggle the nick list, the sidebar |
| Esc | Close the overlay; otherwise clear the selection and return to the present |
| Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+Shift+C | Insert bold, italic, underline, color |
| Ctrl+Q | Quit (closing the window goes to the tray) |

## 9. Rendering

### 9.1 Model

- Layout uses logical pixels (f32); rasterization uses physical pixels at
  winit's scale factor, fractional scales included. Edges and rules snap at a
  single quantization step so one-pixel lines stay crisp.
- A canvas (an XRGB8888 backing store the size of the window) and a damage
  list.
- Primitives: solid and blended rectangles; anti-aliased paths (a
  coverage-accumulation rasterizer with nonzero winding) for chamfers and
  icons; glyph masks blended through a contrast table tuned for light text on
  dark backgrounds; premultiplied RGBA images for color glyphs and icons; a
  clip stack.

### 9.2 Damage

Each region (strip, sidebar, header, chat, nick list, input, overlays)
repaints only its invalidated rectangles:

- a keystroke repaints the input row;
- a badge change repaints one sidebar row;
- a hover change repaints one row or button;
- a new message while you're at the bottom moves the chat region's existing
  pixels up with `copy_within` and paints only the new rows, as terminals do.

### 9.3 Presentation

softbuffer reports each buffer's age. RubyChat copies the union of the last
*age* frames' damage from the canvas into the buffer (everything when the age
is 0) and calls `present_with_damage`. `pre_present_notify` keeps to
Wayland's frame pacing, and a redraw is requested only when something is
damaged.

### 9.4 Text

1. Split text into style runs (formatting) and font runs (fallback per
   cluster).
2. Shape each run with harfrust. An ASCII run in a monospace face with
   ligatures off, the chat default, skips shaping and uses a precomputed
   advance table.
3. Break lines at spaces, between CJK ideographs, after `-` and `/` inside
   URLs, and at grapheme boundaries when a word can't fit.
4. Position glyphs in quarter-pixel horizontal steps.
5. Rasterize each glyph on first use into the glyph cache, from skrifa
   outlines with light vertical hinting (what TRust settled on for JetBrains
   Mono at 11 pt).
6. Blit.

Each message and each nick is laid out as its own bidi isolate, so direction
overrides in one can't reorder anything around it.

### 9.5 Fonts

- If JetBrainsMono Nerd Font isn't installed, JetBrains Mono Regular and Bold
  (OFL), embedded in the binary, keep the look.
- A worker scans the standard font directories (`/usr/share/fonts`,
  `/usr/local/share/fonts`, `~/.local/share/fonts`, `~/.fonts`; the system
  and per-user font folders on Windows), records each face's character
  coverage, and caches the index in `$XDG_CACHE_HOME/rubychat/`, keyed by
  path, size and modification time. Preferred fallbacks (Noto Color Emoji,
  Noto Sans CJK, DejaVu) are configurable.
- A glyph whose fallback face hasn't loaded yet is drawn as an empty box with
  the primary face's advance. When the worker delivers the face, only the
  visible lines that need it are laid out again.
- Color emoji in CBDT (the format Noto Color Emoji ships in) are decoded and
  downscaled on a worker and cached premultiplied. COLRv1 comes later.

### 9.6 Instrumentation

`RUBYCHAT_TRACE=1` logs each frame's event, layout, paint and present times
and its damaged area. A headless mode renders any region to PNG for tests.

## 10. Responsiveness

Zero client-side lag means that no action of yours ever waits on the network,
a disk, DNS, a lock or a long computation, and that its result is in the next
presented frame. The budgets below are UI-thread CPU time on the development
machine at 2560 × 1440, scale 1, and benchmarks check them (§14).

| Interaction | Budget | How |
|---|---|---|
| Keystroke to presented frame | 1 ms | Input-row damage; cached glyphs |
| Enter to message on screen | Same frame | Local echo; the send happens on the network thread |
| Switching buffer or network | 4 ms | A layout cache per buffer; only visible lines laid out |
| Scrolling, per frame | 3 ms | Scroll blit plus the exposed strip |
| Resizing, per frame | 8 ms | Only visible lines reflowed; the view stays anchored |
| Message bursts (joins, LIST, netsplits) | Input waits at most 4 ms | Budgeted draining; coalesced wakes |
| Idle | No UI wakeups | No timers without a visible reason |
| Launch to first frame | 100 ms | Connections, backlog and fallback fonts start after the first frame |

The interface is optimistic: `/join` opens the buffer at once in a joining
state, `/query` opens immediately, `/part` closes immediately, and a nick
change shows as pending until the server confirms it.

The view is anchored to a line and an offset rather than a pixel position, so
new messages, inserted history and width changes never make it jump. The
scrollbar uses estimated heights for lines not yet laid out and corrects them
as they're measured.

## 11. Persistence

| What | Linux | Windows |
|---|---|---|
| Settings and networks | `$XDG_CONFIG_HOME/rubychat/settings.toml`, `networks.toml` | `%APPDATA%\RubyChat\` |
| Passwords, owner-only | `$XDG_CONFIG_HOME/rubychat/secrets.toml` | `%APPDATA%\RubyChat\` |
| Certificate pins, STS policies | `$XDG_CONFIG_HOME/rubychat/known_hosts`, `sts.toml` | `%APPDATA%\RubyChat\` |
| Client certificates | `$XDG_CONFIG_HOME/rubychat/certs/` (0700) | `%APPDATA%\RubyChat\certs\` |
| Session: window, order, open buffers, read positions | `$XDG_STATE_HOME/rubychat/session.toml` | `%LOCALAPPDATA%\RubyChat\` |
| Chat logs | `$XDG_DATA_HOME/rubychat/logs/<network>/<buffer>/<YYYY-MM-DD>.log` | `%LOCALAPPDATA%\RubyChat\logs\` |
| Font index | `$XDG_CACHE_HOME/rubychat/` | `%LOCALAPPDATA%\RubyChat\cache\` |
| Single-instance socket and lock | `$XDG_RUNTIME_DIR/` | A named pipe |

- Format: a strict TOML subset (tables, arrays of tables, strings, integers,
  booleans, arrays), parsed by hand with line and column errors. The store
  thread writes files atomically (temporary file, fsync, rename), debounced.
- Passwords live apart from `networks.toml` so it can be shared or backed up.
  They are never logged and are masked in the interface; the raw protocol
  view redacts PASS, AUTHENTICATE and NickServ IDENTIFY.
- Logs: a file per buffer per day, one tab-separated line per event, such as
  `2026-10-30T12:02:17.123Z⇥msg⇥ruby⇥raw text` (⇥ is a tab). They stay greppable and keep
  formatting codes so backlog restores faithfully. Writes are buffered and
  flushed every second and on exit.
- Session restore: networks connected at exit, or set to connect at startup,
  reconnect with their channels and queries in their saved order. Each
  buffer's last lines load from its log the first time it's shown.
- As built: the session keeps networks that are connected or reconnecting,
  with the channels they are in or joining, their queries, the buffer each
  shows and the network shown. It is saved as it changes, with queued saves
  of one file collapsing into the newest, and at exit. Window geometry and
  read positions are not kept yet; logs arrive with M6. `secrets.toml` is
  written owner-only from the moment it exists and tightened again if found
  open. A `networks.toml` that fails to load is never written over. Client
  certificate paths are taken as given when absolute, under the home
  directory for `~/`, and in `certs/` otherwise. Keys of channels joined by
  hand are not kept, so they rejoin without one.

```toml
# networks.toml
[[network]]
name = "Libera.Chat"
connect_at_startup = true
nick = "ruby"
alt_nicks = ["ruby_", "ruby__"]
username = "ruby"
realname = "Ruby"
autojoin = ["#rust", "#labwc"]
fallback_encoding = "cp1252"

[[network.server]]
host = "irc.libera.chat"
port = 6697
tls = true
certificate = "verify"          # verify | pinned | none

[network.login]
method = "sasl-scram-sha-256"   # none | server-password | sasl-plain | sasl-scram-sha-256 | sasl-external | nickserv
account = "ruby"                # the password is in secrets.toml
require = true
```

## 12. Desktop integration

### 12.1 Tray

- The icon is present for as long as RubyChat runs; a setting can limit it to
  while the window is closed. States: normal, unread (cyan dot) and highlight
  (pink dot, SNI status `NeedsAttention`). sfwbar setups commonly hide
  `Passive` items (the development machine's does), so the normal state is
  `Active`.
- Left click shows or hides the window. The menu: Show/Hide RubyChat, Mark
  all read, Away/Back, Quit.
- Linux: a StatusNotifierItem with a `com.canonical.dbusmenu` menu, over
  RubyChat's D-Bus client. It re-registers when the panel restarts (watching
  `NameOwnerChanged`), and if the tray disappears while the window is hidden,
  the window comes back.
- Windows: `Shell_NotifyIconW`, a message-only window and `TrackPopupMenuEx`
  on the desktop thread.
- The icon, a faceted ruby with neon edges, is drawn in code and pre-rendered
  to ARGB pixmaps from 16 to 64 px.

### 12.2 D-Bus

A small client: session-bus address parsing (`unix:path=`, `unix:abstract=`),
SASL EXTERNAL authentication, marshalling of the D-Bus type system, method
calls, returns, errors and signals, and `Properties` and `Introspectable` for
exported objects. One thread reads; writes go through a mutex. Roughly 1,500
lines with byte-level tests. It also carries notifications and, if wanted
later, the Secret Service.

### 12.3 Notifications

Highlights and private messages notify while the window is unfocused or
hidden, coalesced per buffer, and each buffer can be muted. Clicking a
notification opens that buffer, passing on the notification server's
xdg-activation token when it sends one, so the compositor lets the window take
focus. An open but unfocused window is also marked urgent
(`request_user_attention`). Windows uses the tray's balloon notifications.

### 12.4 Single instance and links

As in trust-desktop, a lock file and a Unix socket in `$XDG_RUNTIME_DIR` (a
named pipe on Windows) make later launches hand over and exit: `--show`, or an
`irc://` or `ircs://` URL. `rubychat.desktop` registers
`x-scheme-handler/irc` and `x-scheme-handler/ircs`, and the Wayland app ID and
X11 class are both `rubychat`. `--tray` starts hidden, for an optional
autostart entry. Links open with `xdg-open` or `ShellExecuteW` from a worker,
with per-scheme overrides (for example, `gemini://` and `gopher://` in
trust-desktop).

### 12.5 Clipboard

Wayland: `wl_data_device` (core protocol) on winit's own connection, with an
event queue of RubyChat's that the event loop dispatches without blocking on
every wake; the loop already wakes whenever the socket has news, ours
included. A keyboard object of its own supplies the input serial that
`set_selection` needs. Copying offers `text/plain;charset=utf-8` and the
legacy text types; pasting asks for the best text type on offer. Data moves
through pipes on short-lived threads, nonblocking with a 3-second stall
limit, and a paste reads at most 1 MiB. The clipboard is dropped in
`exiting`, while the connection it shares is still open. X11 and Windows
keep an in-app clipboard until their selection code lands (M7);
the primary selection (middle click) is not supported.

## 13. Security and robustness

- TLS, pins and STS: §6.2. Passwords: §11.
- Untrusted text: control characters other than formatting codes are drawn
  as visible symbols; bidi controls in nicks are neutralized and every
  message is isolated; the rendering cost of any one message is capped
  (stacked combining marks, very long words).
- Messages never trigger network fetches (there are no previews), so a link
  can't reveal your address until you open it.
- The full URL is shown before a link opens.
- CTCP replies are rate-limited and switchable; the VERSION reply is
  configurable.
- Floods can't stall input: per-buffer line caps, coalesced wakes and
  budgeted processing.
- A malformed message is logged and dropped, never fatal. A worker that
  panics is restarted and the failure reported.

## 14. Verification

- Unit tests sit beside the code in `#[cfg(test)]` modules, as in TRust.
- Conformance: the ircdocs parser-tests vectors, the RFC 7677 SCRAM-SHA-256
  example, and casemapping tables.
- Scenarios against a scripted in-process IRC server (expected and sent
  lines, with a fake clock): registration, multi-line `CAP LS`, SASL success
  and failure, nick collisions, STS upgrade, reconnect backoff, flood-control
  timing, batches, history, and labeled-response routing.
- TLS: a local rustls server with committed self-signed test certificates
  for verification, pinning and mismatch.
- Rendering: PNG snapshots per component. The tab strip is compared side by
  side with TRust's `tab_strip_preview` output.
- Performance benchmarks, ignored by default and run explicitly: at least
  50,000 messages per second ingested; the §10 frame budgets on a synthetic
  100,000-line buffer; no UI wakeups during ten idle seconds. Present since
  M2: `paint_budgets` (keystroke, buffer switch and scrolling frame at
  2560 × 1440 with a 5,000-line channel of 1,000 members), failing in
  optimized builds when a median breaks its budget.
- The app runs headless in tests (`Options::isolated`: no network or store
  threads, sent commands captured), so commands, completion, menus and routing
  are tested without a window. Scrolling by moving pixels is checked against
  full repaints at scales 1, 1.5 and 2.
- Since M3: SASL PLAIN, a required login that fails, and SCRAM against RFC
  7677's test vector; TLS against a local rustls server presenting the
  committed self-signed certificates in `src/net/testdata/`: refused with
  what the prompt shows, trusted by a pin, refused when the pin changed,
  accepted without verification, and a client certificate presented. The
  Server Manager and the trust prompt have snapshots.
- Before handoff: `cargo fmt --check`, `cargo clippy --all-targets`,
  `cargo test`, `cargo test --release`, `cargo build --release`.
- By hand: Libera.Chat and OFTC over TLS; a local ergo for self-signed
  certificates and SASL; soju for bouncer features.

## 15. Repository layout

```
Cargo.toml        one package: library `rubychat` and binary `rubychat`
AGENTS.md         working rules
docs/DESIGN.md    this document
assets/
  fonts/          JetBrains Mono Regular and Bold (OFL), with license
  desktop/        rubychat.desktop; generated icons
src/
  main.rs         startup, single-instance handoff, event loop
  lib.rs
  app/            app state, ApplicationHandler, window lifecycle, commands, key map
  irc/            message, tags, casemap, isupport, cap, sasl/, ctcp, format, modes,
                  numerics, split
  net/            reactor, connect (Happy Eyeballs), tls (verifiers, pins), session,
                  flood, resolve, socks
  model/          network, buffer, line, members, highlight, ignore, input history, unread
  ui/             theme, layout, tab_strip, sidebar, header, chat_view, nick_list, input,
                  context_menu, colors, server_manager, switcher, dialog, widgets
  render/         canvas, damage, raster, paint, glyph_cache, text (shape, wrap, bidi),
                  fonts, emoji, present, snapshot (PNG writer)
  platform/       paths, time, open, clipboard, instance, dbus/, tray/, notify
  config/         TOML subset, settings, networks, secrets, session
  store/          log writer and reader, backlog paging, search
tests/            scenario tests; fixtures (parser-tests vectors, test certificates)
```

## 16. Milestones

Each ends with a usable build that passes §14's checks. M0 and M1 are done:
the strip matches trust-desktop's preview to within 0.5 of 255 grey levels,
an idle window never wakes, and RubyChat registers on Libera.Chat over TLS
and shows its MOTD.

M2's scope is implemented and tested: RubyChat has run on Libera.Chat and
OFTC at once, joining channels from `ircs://` addresses; a day of real use is
the check that remains. Measured with `paint_budgets` (optimized,
2560 × 1440): a keystroke paints in 0.05 ms, a buffer switch in 3.5 ms and a
scrolling frame in 0.75 ms. Not yet built from §8: dragging the sidebar and
nick list dividers, collapsing the sidebar to icons (F8 hides it), the nick
list's filter field, the scrollbar and `▼ 12 NEW` pill, the whole topic on
click, an input bar that grows to eight lines with its byte count and paste
confirmation, reordering rows by dragging, muting, Alt+A, and the inline
confirmation before closing a connected network.

M3's scope is implemented and tested: saved networks, secrets, pins and the
session load before anything connects; a saved network connected at startup
on Libera.Chat, joined its channel and ran its commands, and the next start
restored it from the session alone. Logins are tested against scripted
servers, as no test accounts exist: every method works end to end there
(EXTERNAL with a real client certificate over TLS). A saved network with
several servers moves to the next after a failure, on the link's backoff.
Not yet: STS (M5), creating client certificates in the app, the broken-lock
icon on the tab (the sidebar says "TLS UNVERIFIED"), and window geometry in
the session.

| | Scope | Done when |
|---|---|---|
| M0 | Window and renderer: skeleton; undecorated window with the ported strip; move, resize, maximize, minimize, close; canvas, damage and presentation; rasterizer; text in embedded JetBrains Mono; tracing; snapshots | The strip snapshot matches TRust's preview, and an idle window wakes nothing |
| M1 | One server: parser and conformance vectors; reactor; TCP and TLS (verification only); registration, PING, reconnect; server buffer; an input line that sends raw lines | Connects to Libera.Chat over TLS, shows the MOTD, and `/quote` works |
| M2 | A real client: channels and queries; sidebar; chat view (wrapping, columns, formatting, nick colors, timestamps, links, selection, copy); header; nick list; a tab per network; input editing with history and completion; commands; unread and highlights; clipboard; context menus | Usable all day on two networks at once |
| M3 | Saved servers and logins: TOML subset, Server Manager, secrets file, SASL PLAIN, SCRAM and EXTERNAL, NickServ, connect at startup, autojoin, certificate prompt and pins, client certificates, session restore | Every login method works from the manager, and a restart restores everything |
| M4 | Tray and desktop: D-Bus client, SNI and dbusmenu, close to tray (recreating the window on Wayland), notifications, single instance, `irc://` handler, desktop file and icon, Windows tray | Close, then a highlight notifies, and clicking it opens that buffer |
| M5 | Modern IRC: echo-message with pending echo, labeled-response, batches and netsplits, away, account, chghost and setname, MONITOR, WHOX, standard replies, STS; smart filter; ignores | Correct behavior on ergo and through a bouncer |
| M6 | History and search: logs, backlog paging, chathistory and read markers, search, a streamed LIST browser | A 100,000-entry LIST stays within §10 |
| M7 | Polish: fallback-font index, color emoji, IME, settings, quick switcher, a performance pass against §10, the Windows build, release profile and packaging | Every §10 budget is met |

## 17. Decisions

Settled on 30 October 2026:

1. **Saved passwords:** an owner-only file (`secrets.toml`, §11), as TRust
   keeps credentials.
2. **Scope:** no SOCKS5 or Tor for now; no DCC; no link previews. Bouncer
   support stays planned (M6).
3. **Platforms:** Linux and Windows, as TRust; no macOS.
4. **License:** MPL-2.0, like TRust, so the strip code ports as is.
5. **Chat text:** JetBrainsMono Nerd Font at 11 pt, regular weight (the
   embedded JetBrains Mono until the fallback index lands).
6. **Accessibility:** no accessibility crates for now (AccessKit would add
   71).

Settled while building M2:

7. **Pasting several lines** joins them with spaces into the input until the
   multi-line input bar lands; nothing is sent without Enter.
8. **Context menus** run the commands a person would type (`/op alice`), so
   menus and commands behave alike; operator actions are listed but
   disabled for those who can't use them.
9. **Tab badges** show only on networks in the background; the sidebar shows
   the shown network's activity.
10. **Addresses** follow the IRC URL draft: one entity per address, a
    channel (with `?key=`) or, with `,isuser`, a nick to open a query with;
    `user:password@` passes the password as the server password.

Settled while building M3:

11. **SCRAM credentials** must be plain ASCII. RFC 5802 §2.2 lets clients
    refuse other characters instead of applying SASLprep, which needs
    Unicode normalization tables RubyChat doesn't carry; SASL PLAIN over TLS
    takes any password.
12. **Trusting a certificate** pins it in `known_hosts` and leaves the
    network's policy as it was: a pinned certificate passes "verify". "Trust
    once" lasts until the network is closed.
13. **Ctrl+T** shows the start page, which leads to the editor, rather than
    opening the editor itself.
14. **Commands after connecting** run after every registration, as if typed
    in the server buffer.
15. **Channel keys** of saved networks live in `secrets.toml`, and the
    session keeps none.

## 18. Standards

Read the governing text before implementing; see `web-standards-skill`.

In the local library: RFC 1459 and 2812 (the IRC client protocol), 2813,
7194 (IRC over TLS), 4422, 4616, 5802 and 7677 (SASL), 8446 (TLS 1.3), 5280
and 9525 (certificates and service identity), 8305 (Happy Eyeballs), 1928 and
1929 (SOCKS5), 3986 (URIs), 7613 and 8265 (PRECIS casemapping), all under
`/big/web-standards/rfc/text/`. Added on 30 October 2026 (see the library's
`COVERAGE.md`): Modern IRC's client protocol, formatting and CTCP
(`repositories/ircdocs/modern-irc/`), the IRCv3 specifications
(`repositories/ircv3/ircv3-specifications/`) and the ircdocs parser tests,
from which `tools/gen_parser_vectors.py` generates `src/irc/vectors.rs`;
then, as documents under `documents/`, the XDG Base Directory Specification
(`xdg-basedir-*`), the Wayland protocol specification (`wayland-protocol-*`,
for the clipboard) and the IRC URL draft (`irc-url-*`,
`draft-butcher-irc-url-04`).

M3 follows IRCv3 SASL 3.1 and 3.2, RFC 4616 (PLAIN), RFC 4422 appendix A
(EXTERNAL), RFC 5802 and 7677 (SCRAM, with 7677's test vector), RFC 4013
(SASLprep, declined as §17 says), RFC 4648 (base64) and RFC 5280 (the
certificate fields the trust prompt reads).

Not in the library yet, to be added through the skill's maintenance procedure
before the milestone that needs them:

- freedesktop: the D-Bus specification, StatusNotifierItem, the dbusmenu
  interface (libdbusmenu's XML) and Desktop Notifications — M4
- the Desktop Entry specification — M4

## 19. Alternatives considered

- **UI toolkits** (egui, iced, Slint, GTK): hundreds of crates or a C stack,
  and none produce trust-desktop's look without a fight.
- **GPU rendering** (wgpu, Vello): a large dependency graph and device setup
  for a 2-D text interface that a damage-tracked CPU renderer handles well
  within budget.
- **trust-desktop's Parley and Vello CPU text stack:** text identical to
  TRust's, but 190 crates instead of 97.
- **ring instead of aws-lc-rs:** 4 fewer crates, but no post-quantum hybrid
  key exchange.
- **tokio:** RubyChat only needs readiness on a handful of sockets.
- **zbus or ksni for the tray:** 62 to 67 crates against about 1,500 lines of
  our own.
- **arboard for the clipboard:** 18 more crates on Wayland; the hand-built
  `wl_data_device` client adds none, since winit already brings
  wayland-client.
- **serde and toml:** a few hundred lines cover the subset.
- **The `irc` crate:** built on tokio and opinionated about state.
