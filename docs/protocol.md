# The Sol protocol (version 2)

Sol is the one server. It keeps every world's data, the person's settings and
the connections between worlds. Each world is an app of its own (a desktop
app now, a phone app later) that pairs with Sol and works with its data there,
the way a music player works with a Navidrome server.

Orbit implements all of this: `orbit::client::Sol` (feature `client`) speaks
the protocol, and `orbit::link::Link` (feature `app`) runs it for a world's app,
with the token in the keyring, a local copy that works offline, and sync. This
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
  `number`, `toggle` and `time` (`HH:MM`). A setting with `"shared":
  "navidrome-url"` has one value for every world whose setting names the
  same thing: set in any of them, Sol keeps it once, hands it to all of them
  and sends each of them `sol.settings` when it changes. Use it for what the
  person would otherwise type twice, like the address and login of a server
  two worlds both talk to.

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
as `Authorization: Bearer sol_…`. Each device can be unpaired on its own, in
Sol or by the app itself with `DELETE /api/v1/me`; a `401` means the app should
pair again.

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

A world's inbox is shared by all of the person's devices for that world, so
before acting on a delivery an app claims it with
`POST /api/v1/inbox/{n}/claim`: `204` means this device acts on it, `409`
means another device already did. Claimed deliveries disappear from the other
devices' inboxes.

One delivery is for every device and is never claimed: `sol.settings`, sent
when the person changes the world's settings in Sol. Read
`GET /api/v1/settings` again when it arrives.

## Settings

`GET /api/v1/settings` returns the world's settings as set in Sol, with
defaults filled in and secrets included; a shared setting has the value set
in whichever world it was set.

## Widgets

`PUT /api/v1/widgets/{id}` with `{ "title", "view" }` replaces one of the
world's widgets on Sol's dashboard; `DELETE` takes it away. The view is a
`WidgetView`: a `figure` and `caption`, a `progress`, `rows` of `label: value`,
and `items` (a checklist when they have `done`; `toggle: true` lets people tick
them from the dashboard). Push again whenever what it shows changes.

## Versions and updates

Every request from an app carries two headers:

- `Sol-Protocol: 2`: the protocol it speaks (`orbit::PROTOCOL`). The number
  goes up only for changes that break one side.
- `Sol-App-Version: 0.2.0`: its own version; Sol shows it under Devices.

`GET /_sol/health` says which protocols Sol speaks (`protocol`, the newest,
and `min_protocol`). An app outside that range gets `426` with a message
saying which side to update, on everything except `/api/v1/me` and
`/api/v1/update*`, so an app that is too old can always update itself.

Apps never ask GitHub for updates (the repositories can be private):

1. `GET /api/v1/update?version=0.1.0&target=linux-x86_64&kind=portable`
   (`kind`: `portable`, `setup`, `deb`, `rpm`; see `orbit::update`). Sol looks
   at the world's latest GitHub release with its own token and answers
   `{ latest, offer, message }`. `offer` is there when a newer version has a
   signed file for that target and kind: `{ version, notes, file, size,
   download, signature }`.
2. `GET <offer.download>` streams the file through Sol.
3. The app checks `signature` (minisign, as `tauri signer sign` writes it)
   against the public key built into it, and only then installs.

Release files are named `sol-<world>-<version>-<target><suffix>` with a
`.sig` next to each: `.tar.gz` (the binary alone; installs and updates
itself), `.deb`, `.rpm`, `-setup.exe`. `orbit::updates::Updates` does all of
this for a Rust app.

## Errors

Every error is `{ "error": "<code>", "message": "<a sentence for people>" }`
with the matching status: `400` bad input, `401` not paired (any more), `404`
missing, `409` conflict, `410` pairing gone, `426` protocol mismatch, `429`
too many tries.
