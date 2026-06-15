# Plan: serve the llama.cpp web UI through the proxy (`/ui`)

**Status:** implemented (`src/proxy/ui.rs`, `tests/proxy_ui.rs`)
**Origin:** brainstorm 2026-06-15 (UI via llamastash). No separate requirements doc — decisions captured here.
**Scope guard:** small, MVP-only. Reuse what the proxy already has; defer everything in Non-goals.

## Goal

One stable, port-stable browser entry point — `http://127.0.0.1:11435/ui/` — that serves the
running model's own web UI through the proxy, so users stop hunting the ephemeral backend port in
`status`. Shared single origin: chat history persists across model switches (it is browser-origin
keyed, not server-side — verified: state survived an L2→L3 swap on the reused port 41100).

## Why this is small

- The stock UI is **base-path aware**: its index uses relative assets (`./bundle.js`) and SvelteKit
  computes `base` from the URL (`new URL('.', location)`). Served under `/ui/`, all its asset and
  (base-relative) API requests stay under `/ui/`. No rewriting, no vhost, no custom shell.
- `forward::forward_to_upstream` (`src/proxy/forward.rs`) already builds
  `http://127.0.0.1:{port}{prefix}{path_and_query}` and streams the response — it is already a
  reverse proxy with a `prefix` slot. We feed it the stripped path.
- The proxy already holds the `supervisors` registry + MRU (`src/proxy/state.rs`, `src/proxy/mru.rs`)
  — same source `status` uses for the running-model list.
- Bearer auth already ships (`src/proxy/auth.rs`, merged via PR #26). `/ui` rides the same gate.

## Decisions (locked in brainstorm)

- **Surface:** `/ui` on the existing proxy listener (`:11435`), one shared origin.
- **Switcher:** MRU default + chooser when several models run. The chooser is **ours** (the stock UI
  is single-model — no reliable in-app switcher). Picking repoints the active backend; the browser
  reloads; history persists (same origin).
- **Chooser scope:** running models only (v1). Auto-start-on-pick is a Non-goal.
- **Auth:** `/ui*` goes through the existing `ProxyAuth`, extended to also accept HTTP **Basic** (the
  key as the password) so browsers authenticate over LAN; `/ui` 401s carry `WWW-Authenticate: Basic`.
  Bearer stays the API path. Keyless loopback = open. See "LAN `/ui` + browser auth".

## Design

Backend selection for any `/ui/...` request, in order:
1. Cookie `ls_ui_target=<launch_id>` present and still running → that backend.
2. Else exactly one running model → that backend (the MRU-degenerate case).
3. Else zero running → a small "no model running" page (hint: start one via TUI/CLI).
4. Else (>1, no valid cookie) → the chooser page.

Once a target is known: strip the `/ui` prefix and forward `/{rest}` to `127.0.0.1:{port}` via
`forward_to_upstream`, streaming. The UI's relative assets + base-relative calls (`/ui/bundle.js`,
`/ui/props`, `/ui/v1/chat/completions`) all land here and route to the same target. The cookie keeps
asset/API requests pinned to the model whose UI is loaded.

`GET /` stays the Ollama identity handshake — untouched. `GET /ui` 302s to `/ui/` (trailing slash so
`./` resolves correctly).

**Switching once pinned (added during implementation).** Because the cookie keeps `/ui/` forwarding
to the pinned model, there is a reserved `/ui/switch` path that always renders the chooser regardless
of the cookie (marking the active model), so a user can re-pick without clearing the cookie by hand.
`/ui/?target=<launch_id>` remains the direct re-pin. Still no injection into the stock UI — the
switch affordance is an out-of-band URL, surfaced via a hint on the chooser page and in `docs/usage.md`.

## Implementation steps

1. **`src/proxy/router.rs`** — add to the `route()` match: `GET /ui` → redirect to `/ui/`; and a
   `path.starts_with("/ui/")` branch (before `_ => not_found()`) delegating to the new `ui` module.
   Do **not** add `/ui` to `auth_exempt` — it inherits the bearer gate.
2. **`src/proxy/ui.rs`** (new) —
   - `resolve_target(state, &req) -> UiTarget` (cookie → running lookup → MRU → Chooser/None), reading
     the registry through the IPC context handle on `ProxyState`.
   - `serve(state, req)`: strip `/ui` prefix, call `forward::forward_to_upstream` with the target port
     and stripped path-and-query.
   - `chooser_html(running)` + `no_model_html()` — minimal static HTML; chooser links point to
     `/ui/?target=<launch_id>`, which sets the cookie (`Path=/ui`, `SameSite=Lax`) and 302s to `/ui/`.
   - cookie read/write helpers.
3. **Browser auth** (`src/proxy/auth.rs` + `src/proxy/router.rs`) — extend `ProxyAuth::check` to also
   accept `Authorization: Basic base64(user:pass)` where `pass` matches the key (constant-time;
   `base64` 0.22 is already a direct dep, already imported in `auth.rs`). For `/ui*`, on auth failure
   return 401 with `WWW-Authenticate: Basic realm="llamastash"` (a `unauthorized_basic()` beside the
   existing `unauthorized()`) so the browser prompts. ~15–20 lines; reuses the auto-provisioned key.
   Flip the `auth.rs` test that asserts `Basic …` is rejected (~line 170).
4. **Reuse, don't rebuild** — confirm `forward_to_upstream` forwards arbitrary methods/paths (GET
   assets, GET `/props`), not just `POST /v1/*`. It pipes bytes, so it should; add a thin wrapper if a
   signature tweak is needed.
5. **TUI Daemon pane** (`src/tui/info_pane.rs`) — make the UI discoverable + tidy separators:
   - `proxy_row` (the `"listening"` arm, ~line 64): change body from `format!("listening {listen}")`
     to `format!("{listen} · ui {listen}/ui")`. Result: `proxy   127.0.0.1:11435 · ui 127.0.0.1:11435/ui`.
     Preserve the auth flag — append ` (auth)` after the API endpoint when `auth == "enforced"`
     (`127.0.0.1:11435 (auth) · ui …`), so it reads as a listener property, not a UI one.
   - `daemon_row` (~lines 119, 121): change the separator spans `"  pid "` → `" · pid "` and
     `"  up "` → `" · up "`. Result: `port    48134 · pid 3398288 · up 1h32m`. (` · ` is already the
     TUI's separator convention — `help_bar::HINT_SEP`, host pane `NVML · 1 GPU`.)
   - Update the `daemon_row` doc-comment example (line ~91) to the middot form.
6. **Docs** — AGENTS.md §Scope boundaries: note the proxy serves a web-UI surface (`/ui`), LAN-capable
   with Basic auth reusing the proxy key; `docs/usage.md`: how to open it + the LAN/Basic flow;
   `CHANGELOG.md` `[Unreleased]` one-liner.

## Tests (`tests/` + inline, `fake_llama_server` fixture)

- `GET /ui` → 302 `/ui/`.
- one running model → `GET /ui/` 200, body forwarded from backend; `/ui/props` reaches backend.
- two running, no cookie → `/ui/` serves the chooser; `/ui/?target=<id>` sets cookie + redirects.
- cookie pins the target across asset requests.
- auth enforced + no credential → 401 on `/ui/` **with `WWW-Authenticate: Basic`** (exempt paths open).
- auth enforced + `Authorization: Basic base64(x:<key>)` → `/ui/` passes; wrong key → 401.
- auth enforced + `Authorization: Bearer <key>` → still passes (API path unchanged).
- zero running → "no model" page (not a 500).
- **TUI:** update `proxy_row_renders_listening_endpoint_when_set` (`src/tui/info_pane.rs:1146`) — it
  asserts the old `listening 127.0.0.1:11434`; switch to `127.0.0.1:11434 · ui 127.0.0.1:11434/ui`.
  Add a `daemon_row` assertion for the ` · pid ` / ` · up ` separators. Refresh the
  `tests/golden/dashboard-overview.txt` fixture (`make render` / `UPDATE_GOLDEN=1`).

## LAN `/ui` + browser auth (decision — supported)

`/ui` **is supported over LAN, authenticated, reusing the existing auto-provisioned proxy key** — via
**HTTP Basic auth**. A browser can't send `Authorization: Bearer` by navigating, but it speaks Basic
natively: on a 401 carrying `WWW-Authenticate: Basic`, the browser prompts, the user pastes the proxy
key as the password, and the browser remembers + resends it per-origin. Same key, no cookies, no login
page, no sessions, no key-in-URL.

Behaviour:
- **Loopback (default, keyless):** works, no prompt.
- **LAN, auth enforced:** browser prompts once → paste the proxy key → works. API clients keep using
  `Authorization: Bearer`, unchanged.
- **LAN with `--insecure-no-auth`:** works, no prompt (operator's explicit opt-in).

Honest caveat: the proxy is plain HTTP (no TLS), so the key crosses the LAN as cleartext base64 —
**identical to the existing bearer API-auth posture**. No new degradation; TLS is a separate, larger
piece and stays out of scope.

## Not pursued (permanent non-goals)

- Anything heavier than Basic for browser auth — login page, sessions, cookie/key-in-URL bridge. Basic
  reuses the existing key and is enough.
- Auto-start a stopped model from the chooser (running-only).
- In-page model switch-bar / iframe embedding of the stock UI.
- Per-model isolated history (chosen: shared workspace).
- Forking/hosting a custom UI.

## Open questions (resolve during implementation, not blocking)

- ~~Does the stock UI issue API calls base-relative (`/ui/v1/...`) or root-absolute (`/v1/...`)?~~
  **Resolved by design — both work.** `proxy::ui::serve` strips the `/ui` prefix and forwards
  base-relative calls (`/ui/v1/...` → `/v1/...`) to the cookie-pinned backend; a root-absolute
  `/v1/...` simply never enters the `/ui/` branch and rides the existing body-model routing. No
  explicit `/ui/v1/*` special-casing needed. Covered by `tests/proxy_ui.rs`
  (`single_running_serves_ui_and_forwards_paths`, `two_running_shows_chooser_then_cookie_pins`).
- **Deferred (needs a real `llama-server` web UI to observe):** SvelteKit client-side routes — if the
  UI pushes paths like `/ui/chat/<id>`, a hard reload strips to `/chat/<id>` and the backend `404`s
  instead of falling back to the UI index. Today `serve` forwards verbatim with no SPA index-fallback.
  Tracked in `TODO.md` (**Proxy `/ui` SPA deep-link fallback**); confirm against a real UI build, and
  add an index fallback under `/ui/` only if deep-link reloads actually happen.
