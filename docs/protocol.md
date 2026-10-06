# The Sol protocol (version 2)

Sol is the one server. It keeps every world's data, the person's settings and
the connections between worlds. Each world is an app of its own (a desktop
app now, a phone app later) that pairs with Sol and works with its data there,
the way a music player works with a Navidrome server.

Orbit's `client` feature implements all of this as `orbit::client::Sol`; this
page is for anyone writing a world in something else.

```
 Terra app ─┐                    ┌──────────────── Sol (one container) ────────────────┐
 Mercury app ├─ HTTPS, bearer ──▶│ /api/v1/*: docs, changes, events, inbox, settings   │
 Neptune app ┘   token per world  │ one SQLite file per world, plus Sol's own          │
                                  │ connections: event from one world → another's inbox │
 browser ──── HTTPS, cookie ─────▶│ /api/sol/*: Sol's web app (install, pair, settings) │
                                  └─────────────────────────────────────────────────────┘
```

## A world's manifest

Every world publishes `sol-world.json` at the root of its repository. Sol has
the built-in worlds' manifests already; others are added under Worlds → Add a
world from GitHub.

```json
{
  "id": "terra",
  "name": "Terra",
  "tagline": "Habits, diary and wellbeing",
  "repo": "Draysh/sol-terra",
  "collections": [{ "id": "habits", "label": "Habits" }],
  "emits": [{ "type": "terra.habit.checked", "label": "A habit is ticked" }],
  "actions": [
    { "id": "tick-habit", "label": "Tick a habit",
      "params": [{ "key": "habit", "label": "Habit", "kind": "text", "required": true }] }
  ],
  "settings": [
    { "key": "day_starts", "label": "Day starts at", "kind": "time", "default": "04:00" }
  ]
}
```

- `id`: lowercase letters, digits and dashes; also the world's event namespace.
- `collections`: what the world keeps in Sol, named for the data overview.
- `emits`: events connections can listen for.
- `actions`: what the world can do when a connection fires.
- `settings`: set once in Sol, read by every paired device. Kinds are `text`,
  `url`, `secret` (handed only to the world's own devices, never shown again),
  `number`, `toggle` and `time` (`HH:MM`).

Installing a world in Sol gives it a database and lets its apps pair. Its
releases on GitHub carry the app downloads, which Sol's world page offers per
platform (`.AppImage`, `.deb`, `.rpm`, `.msi`, `.exe`, `.dmg`, `.apk`).

## Pairing

1. The app sends `POST /api/v1/pair` with `{ "world", "device", "platform" }`
   and shows the person the `code` it gets back (`ABCD-EFGH`, valid for ten
   minutes).
2. The person approves the request carrying that code in Sol, under Devices or
   on the world's page.
3. The app polls `POST /api/v1/pair/claim?wait=25` with `{ "id", "secret" }`.
   `202` means not yet; `200` carries the `token`; `410` means start again.

The token works for that world only. Keep it in the system keyring and send it
as `Authorization: Bearer sol_…`. Each device can be unpaired on its own; a
`401` means the app should pair again.

## Data

A world keeps JSON documents in named collections. Every write gets the next
`version` in that world's database.

| Request | What it does |
| --- | --- |
| `GET /api/v1/docs/{collection}` | Every live document in the collection |
| `GET /api/v1/docs/{collection}/{id}` | One document, or `404` |
| `PUT /api/v1/docs/{collection}/{id}` `{ "data", "if_version"? }` | Write; with `if_version` only if unchanged since (`0`: only if new), else `409` with the `current` document |
| `DELETE /api/v1/docs/{collection}/{id}` | Leave a tombstone that syncs like any change |
| `GET /api/v1/changes?after=N&wait=S` | Every write after version `N`, oldest first; waits up to `S` (≤ 25) seconds when there's nothing yet |

To stay in sync, a device keeps the last `next` it saw and asks for the
changes after it; `more: true` means ask again straight away. Collection
names are lowercase (`habits`, `check_ins`); ids are letters, digits, `-` and
`_` (a UUID fits). A document is at most 1 MiB.

## Events and connections

`POST /api/v1/events` records something that happened:

```json
{ "type": "terra.habit.checked", "summary": "Did “Walk outside”, 4 days in a row",
  "subject": "habit/walk", "data": { "habit": "walk", "streak": 4 } }
```

- The type must be in the app's own namespace: `<world>.<thing>.<verb>`.
- `summary` is one sentence for people; Sol's activity log and notifications
  show it as written.
- Send an `id` (UUIDv7) to make retries safe; the same id is stored once.

When an event's type matches a connection someone made in Sol ("when Mercury
finishes a review, tick the Japanese habit in Terra"), Sol puts a delivery in
the target world's inbox. The app reads it with
`GET /api/v1/inbox?after=N&wait=S`:

```json
{ "deliveries": [{ "n": 7, "action": "tick-habit", "params": { "habit": "Japanese" },
                   "event": { "type": "mercury.review.finished", … }, "connection": "…" }],
  "next": 7 }
```

Ticking a box in a widget on Sol's dashboard arrives the same way, as the
action `widget.toggle` with `{ "widget", "item", "done" }`.

## Settings

`GET /api/v1/settings` returns the world's settings as set in Sol, with
defaults filled in and secrets included.

## Widgets

`PUT /api/v1/widgets/{id}` with `{ "title", "view" }` replaces one of the
world's widgets on Sol's dashboard; `DELETE` takes it away. The view is a
`WidgetView`: a `figure` and `caption`, a `progress`, `rows` of `label: value`,
and `items` (a checklist when they have `done`; `toggle: true` lets people tick
them from the dashboard). Push again whenever what it shows changes.

## Errors

Every error is `{ "error": "<code>", "message": "<a sentence for people>" }`
with the matching status: `400` bad input, `401` not paired (any more), `404`
missing, `409` conflict, `410` pairing gone, `429` too many tries.
