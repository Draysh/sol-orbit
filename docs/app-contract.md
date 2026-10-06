# The Sol app contract (version 1)

Every app (world) in the Sol ecosystem is an HTTP service in its own container.
Sol reaches it on the internal Docker network; browsers never do. This page is
everything an app must do to join. Orbit implements all of it, so a Rust app
gets it by calling `orbit::run`.

## Endpoints every app serves

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /_sol/health` | none | `{"status":"ok","version","arch","uptime_s","db"}`; Sol polls it to show the app as up or down |
| `GET /_sol/manifest` | none | Who the app is, its widgets and its events (below) |
| `GET /_sol/openapi.json` | none | The app's API, used to generate its UI's types |
| `GET /_sol/outbox?after=N&wait=S` | `system` token | Events after cursor `N`, long-polling up to `S` (max 25) seconds |
| `GET /ui/*` | none | The app's web UI (static files); Sol shows it at `/<app>/*` |
| everything else | `user` token | The app's own API, reached by browsers as `/api/<app>/…` |

Sol never forwards a browser request to a `/_sol/*` path, and maps
`/<app>/<path>` only onto `/ui/<path>`, so pages can't reach the API without
going through `/api/<app>/`.

## Manifest

```json
{
  "contract": 1,
  "id": "terra",
  "name": "Terra",
  "version": "0.1.0",
  "description": "Habits, diary and wellbeing",
  "ui": true,
  "widgets": [
    { "id": "today", "title": "Today", "path": "/widgets/today", "refresh_on": ["terra.habit.*"] }
  ],
  "events": {
    "emits": [
      { "type": "terra.habit.created", "v": 1 },
      { "type": "terra.habit.checked", "v": 1, "notify": true }
    ],
    "consumes": []
  }
}
```

- `ui` is set by Orbit when the app embeds a web UI.
- `notify: true` asks Sol to raise a system notification for that event type
  while Sol isn't in front.

## Web UI

Each app ships its own SvelteKit build, embedded in the binary and served at
`/ui/`. It is built with `paths.base = '/<app>'`, uses `@sol/design` for every
component, and runs on Sol's origin, so it shares the session cookie and calls
`/api/sol/*` and `/api/<app>/*` directly. Moving between apps is a page load.

An app's UI runs with the signed-in person's session: only install apps you
trust, as you would any program on your server.

## Widgets

A widget is an API route returning a `WidgetView`. Sol's dashboard fetches it
at `/api/<app><path>` and draws it with the shared components, again when an
event matching `refresh_on` arrives.

```json
{
  "figure": "2 / 3",
  "caption": "habits today",
  "progress": { "value": 2, "max": 3 },
  "rows": [{ "label": "Streak", "value": "12 days" }],
  "items": [
    {
      "id": "0192…",
      "label": "Walk outside",
      "meta": "4 d",
      "done": true,
      "toggle": { "method": "PUT", "path": "/habits/0192…/today", "field": "done" }
    }
  ],
  "empty": "No habits yet."
}
```

Every field is optional. Ticking an item sends `{ "<field>": <new state> }`
with `method` to `/api/<app><path>`.

## Tokens

Sol signs a fresh Ed25519 JWT (EdDSA) for every API request it proxies:

```json
{ "iss": "sol", "aud": "terra", "sub": "<user id>", "scope": "user", "tz": "Europe/Copenhagen", "iat": 0, "exp": 300 }
```

- `aud` names exactly one app, so a token can't be replayed against another.
- Apps accept only `EdDSA`, with 30 seconds of clock leeway.
- Public keys are at `http://sol:8080/_sol/jwks`. Apps fetch them on first use
  and again when a token names an unknown `kid`, so containers can start in
  any order.
- `system` tokens (`sub: "sol"`) are only minted by Sol's own background tasks.
- `tz` is the person's time zone; use it to decide what "today" means
  (`claims.today()`). The container clock is UTC.

Sol strips cookies, `Authorization`, `Forwarded`, `X-Forwarded-*`,
`X-Real-IP` and `X-Sol-*` from browser requests before adding its own, and
drops any `Set-Cookie` an app sends back. Requests for app pages carry no
token at all.

## Events

An app writes an event in the same SQLite transaction as the change it
describes (`orbit::event::append`), then wakes the long-poll
(`Outbox::notify`). Sol pulls each outbox, stores what is new and streams it to
browsers over `GET /api/sol/events` (server-sent events, resumable with
`Last-Event-ID`).

```json
{
  "id": "0192f1c4-…",
  "type": "terra.habit.checked",
  "v": 1,
  "source": "terra",
  "seq": 42,
  "time": "2026-10-05T08:12:33.120Z",
  "user": "<user id>",
  "subject": "habit/0192…",
  "summary": "Did “Walk outside”, 4 days in a row",
  "data": { "habit_id": "0192…", "name": "Walk outside", "date": "2026-10-05", "streak": 4 }
}
```

- `id` is a UUIDv7 chosen by the app; delivery is at least once, so consumers
  deduplicate on it.
- `type` is `<app>.<entity>.<past-tense verb>`. Sol drops events outside the
  app's own `<app>.` namespace and sets `source` itself.
- `summary` is one sentence for people. Sol's activity feed and notifications
  show it as written, so Sol never needs to understand an app's events.
- Bump `v` when the shape of `data` changes.

## Running

Each app is a binary with three subcommands: `serve`, `healthcheck` (for
container healthchecks) and `openapi`. Configuration is by environment:

| Variable | Default | Meaning |
| --- | --- | --- |
| `ORBIT_BIND` | `0.0.0.0:8080` | Listen address |
| `ORBIT_DATA_DIR` | `data` | Where the app keeps `<app>.db` |
| `SOL_INTERNAL_URL` | `http://sol:8080` | Where to fetch Sol's signing keys |
| `ORBIT_LOG_FORMAT` | text | `json` for structured logs |
| `RUST_LOG` | `info` | Log filter |

App ids are lowercase letters, digits and dashes. `sol`, `api`, `login`,
`setup`, `connect`, `icons` and `assets` are taken by Sol's own pages.
