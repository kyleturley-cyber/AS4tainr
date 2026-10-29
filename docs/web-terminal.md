# Browser 5250 terminal

`as4tainr tn5250 --web-listen ADDR` also serves a 5250 terminal as a web page. Open it in a browser and you get the as4tainr sign-on screen, with a row of clickable function keys above it, laid out like a 5250 keyboard. You don't need to install an emulator.

```sh
as4tainr tn5250 --listen 0.0.0.0:23 --web-listen 0.0.0.0:8080
```

In Docker it is on by default. After `docker compose up`, open **http://localhost:5250**.

It runs in the same process and the same container as the 5250 server. There is no sidecar and no extra service.

## The page

```
 ┌────────────────────────────────────────────────────────────────────────────┐
 │ F13 F14 F15 F16   F17 F18 F19 F20   F21 F22 F23 F24 │ Help  PgUp Reset   Tab   │     │
 │ F1  F2  F3  F4    F5  F6  F7  F8    F9  F10 F11 F12 │ Clear PgDn FldExit BkTab │Enter│
 │         Exit                                 Cancel │                   │     │
 ├────────────────────────────────────────────────────────────────────────────┤
 │                         Work with Customers                                │
 │  ...the 24x80 screen...                                                    │
 └────────────────────────────────────────────────────────────────────────────┘
   X SYSTEM                                                            07/003
```

- **F13–F24 sit above F1–F12** in groups of four, as on the 122-key 5250 keyboard.
- **A key shows what it does on the current screen.** The page reads the key hints on the screen, such as `F3=Exit`. Each named key shows its function under its number and is highlighted. Keys the screen does not name are dimmed, but you can still click them. PgDn is highlighted when a list says `More...`.
- **The keys on the right** are Help, Clear, Page Up/Down (Roll Down/Up), Reset, Tab and BkTab (next and previous input field), Field Exit and Enter. Tab, BkTab and Field Exit are there for phones and tablets, whose keyboards have no Tab key.
- **The status line** at the bottom shows `X SYSTEM` while the host is working (keyboard locked), input errors such as "Cursor in protected area", the insert mode, and the cursor position.
- Clicking the screen moves the cursor. Clicking a key hint on the screen, such as `F3=Exit`, presses that key.

## Keyboard

| Key | 5250 function |
| --- | --- |
| F1–F12, Shift+F1–F12 | F1–F12, F13–F24 |
| Enter | Enter |
| Keypad Enter | Field Exit (clears the rest of the field and goes to the next one; numeric fields are right-adjusted) |
| Keypad + / − (in a numeric field) | Field+ / Field− |
| Tab, Shift+Tab | Next field, previous field |
| Page Up, Page Down | Roll Down, Roll Up |
| Home, End | First input field, end of the data in this field |
| Insert | Insert mode on or off |
| Backspace, Delete | Delete the character to the left / under the cursor |
| Esc | Reset (clears an input error) |

The page catches F1–F12 itself. F5 does not reload the page and F12 does not open the developer tools while the terminal has focus. Paste (Ctrl+V) types the first line of the clipboard into the fields.

### Phones and tablets

Tap the screen to place the cursor and open the on-screen keyboard. The keyboard's Enter (or Go) key is Enter. Use the bar's keys for the function keys, Tab, BkTab and Field Exit; a tap on the bar does not open the on-screen keyboard if it was closed.

The page types into a hidden text field that follows the cursor (the on-screen keyboard only opens for a text field). A desktop keyboard's keys are handled when they are pressed. Text from a phone keyboard or an input method arrives as input events instead, often as words the keyboard changes until they are done (suggestions, autocorrect). The page compares the field with what it held before and replays the difference on the screen as Backspaces and typed characters. Autocorrect, autocapitalization and spellcheck are turned off for that field.

Fields behave like a display station's: monocase fields fold to upper case, numeric fields accept only digits (and sign and decimal characters where the field allows them), nondisplay fields (passwords) are not shown, and auto-enter fields press Enter when they are full.

## How it works

```
 browser ──HTTP/WebSocket──▶ as4tainr tn5250 process
                              ├─ web terminal (core/src/web)
                              │    a 5250 display station: screen buffer,
                              │    fields, input records
                              │        │ TN5250 over loopback
                              │        ▼
                              └─ 5250 server (core/src/tn5250) ─▶ jobs, CL, RPG
```

Each browser tab opens a WebSocket. On the server side of that socket, as4tainr runs a 5250 display station. It is terminal type `IBM-3179-2`, a 24x80 color display. It connects to the 5250 server in the same process, so a browser session is an ordinary 5250 session. It goes through the same telnet negotiation, sign-on, screens and jobs as tn5250 or IBM ACS. Each tab gets its own `QPADEVnnnn` device and its own job, and `WRKACTJOB` lists them.

The 5250 data stream stays on the server. The display station applies the host's records (Clear Unit, Write To Display with SBA, IC, SF, RA, EA, TD and attribute bytes, Read MDT Fields). It sends the browser the screen as text, attribute positions and the field table. The browser draws the screen and edits the fields. When you press an AID key, the browser sends the key, the cursor and the fields you changed. The server builds the Read MDT Fields reply: cursor, AID, then SBA and data for each changed field. Typed control characters are sent as blanks, so input cannot inject orders.

Messages on the WebSocket are JSON:

```json
{"t":"screen","rows":24,"cols":80,"lines":["...", "..."],"attrs":[[34,34],[42,32]],
 "fields":[{"p":452,"l":10,"f":[64,32],"a":36}],"cursor":452,"locked":false,"error":null}
{"t":"aid","aid":"F3","cursor":452,"fields":[{"p":452,"v":"QSECOFR"}]}
{"t":"closed","reason":"The 5250 session ended."}
```

Positions are 0-based offsets on the 24x80 screen (`row * 80 + col`). `f` is the field format word and `a` the screen attribute.

## Security

- **The page has no login of its own.** You sign on on the 5250 sign-on screen, as with any emulator.
- **There is no TLS.** HTTP and WebSocket traffic is in clear text, like telnet on port 23, and that includes passwords. To use it beyond your own machine or a trusted network, put it behind a reverse proxy that terminates TLS (nginx, Caddy, Traefik) and serve it as `https://`. The page then uses `wss://` automatically.
- **Only the page's own origin can open a WebSocket.** The browser's `Origin` must match the `Host` it connected to, so another web site cannot open 5250 sessions through your browser. A reverse proxy must pass the original `Host` header through.
- **Scripts and styles come only from this server.** The page is sent with a strict Content-Security-Policy (`default-src 'self'`), `X-Frame-Options: DENY` and `nosniff`. There is nothing from a CDN, so the terminal works offline.

## Configuration

| Setting | Default | |
| --- | --- | --- |
| `--web-listen` / `AS4TAINR_WEB_LISTEN` | off (CLI); `0.0.0.0:8080` in the Docker image | Address of the web terminal. Empty turns it off |
| `/healthz` | | Answers `ok`, for container health checks |

## Tests

- Unit tests: `cargo test web::`. They cover the display station against screens from as4tainr's own data stream encoder, with round trips through the server's input parser. They also cover WebSocket framing and the handshake (RFC 6455 examples), SHA-1 (RFC 3174 vectors), base64 (RFC 4648 vectors), the telnet replies, and the origin check.
- End to end in Chromium: `python3 tools/e2e_web.py http://127.0.0.1:5250` against a running server with the samples. It covers sign-on, the key bar's labels, clicked and typed function keys, a clicked key hint, Field Exit, subfile paging, order entry, two tabs as two jobs, phone width, and a touch device whose keyboard sends input events only (as Android's do): typing, Backspace, a composed word, the Tab key of the bar and Enter as a new line.

What is not there yet is in [fidelity-gaps.md](fidelity-gaps.md) under "Browser terminal".
