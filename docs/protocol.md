# The Sol protocol (protocol 2, frozen for Sol 1.0)

Sol is the one server. It keeps every world's data, the person's settings and
the connections between worlds. Each world is a desktop app of its own that
pairs with Sol and works with its data there, the way a music player works
with a Navidrome server.

Protocol 2 is Sol 1.0's. This page is its whole contract: every request, every
shape, every limit and every error. It changes only in the ways
[Compatibility](#compatibility) allows; anything else is protocol 3.

Orbit implements all of it: `orbit::client::Sol` (feature `client`) speaks the
protocol request by request, and `orbit::link::Link` (feature `app`) runs it
for a world's app, with the token in the keyring, a local copy that works
offline, and sync. This page is for anyone writing a world in something else.

```
 Terra app ─┐                    ┌──────────────── Sol (one container) ────────────────┐
 Mercury app ├─ HTTPS, bearer ──▶│ /api/v1/*: docs, changes, events, inbox, settings   │
 Neptune app ┘   token per world  │ one SQLite file per world, plus Sol's own          │
                                  │ connections: event from one world → another's inbox │
 browser ──── HTTPS, cookie ─────▶│ /api/sol/*: Sol's web app (install, pair, settings) │
                                  └─────────────────────────────────────────────────────┘
```

Only `/api/v1/*` and `/_sol/health` are this protocol. `/api/sol/*` belongs to
Sol's own web app and may change with any Sol release.

## Conventions

- **JSON** in both directions, `content-type: application/json`, UTF-8.
- **Times** are RFC 3339 in UTC, e.g. `2026-03-01T19:04:00.123456Z` (Sol may
  send fractions of a second; read any RFC 3339 time).
- **Lengths** of names and sentences count characters; sizes of data count
  bytes of the JSON as Sol serialises it.
- **Optional fields** may be missing or `null`; treat both the same. Fields
  this page doesn't name may appear in answers: ignore them.
- **Every request** to `/api/v1/*` carries two headers:
  - `Sol-Protocol: 2`: the protocol the app speaks (`orbit::PROTOCOL`).
    Required (see [Versions](#versions-and-the-protocol-header)).
  - `Sol-App-Version: 0.2.0`: the app's own version (at most 32 letters,
    digits, `.`, `-`, `+`); Sol shows it under Devices. Optional.
- **Paired requests** carry `Authorization: Bearer sol_…`.

## A world's manifest

Every world publishes `sol-world.json` at the root of its repository (and with
each release). Sol has the built-in worlds' manifests already; others are
added under Worlds → Add a world from GitHub.

```json
{
  "id": "terra",
  "name": "Terra",
  "tagline": "Habits, diary and wellbeing",
  "role": "Habits and diary",
  "repo": "Draysh/sol-terra",
  "min_sol": "1.0.0",
  "protocol": 2,
  "collections": [{ "id": "habits", "label": "Habits" }],
  "emits": [{ "type": "terra.habit.checked", "label": "A habit is ticked" }],
  "actions": [
    { "id": "tick-habit", "label": "Tick a habit",
      "params": [{ "key": "habit", "label": "Habit", "kind": "text", "required": true }] }
  ],
  "settings": [
    { "key": "day_starts", "label": "Day starts at", "kind": "time", "default": "04:00",
      "hint": "Ticks and writing before this count for the day before." }
  ],
  "connections": [
    { "when": "mercury.review.finished", "then_world": "terra", "then_action": "tick-habit",
      "params": { "habit": "Japanese" }, "why": "A finished review ticks the Japanese habit." }
  ]
}
```

| Field | Required | What it is |
| --- | --- | --- |
| `id` | yes | Lowercase letters, digits and dashes, starting with a letter, at most 32, not `sol`; also the world's event namespace |
| `name` | yes | Not empty |
| `tagline` | yes | One line about what it is for |
| `role` | | The same in a word or three (`Habits and diary`), shown under its name in the app menu and the tray |
| `repo` | yes | `owner/name` on GitHub, where its releases are |
| `parent` | | The planet a moon belongs to, e.g. `neptune` for Triton |
| `min_sol` | | The oldest Sol version the world works with (`1.2.0`) |
| `protocol` | | The protocol its app speaks |
| `collections` | | `{ id, label }`: what it keeps in Sol, named for the data overview |
| `emits` | | `{ type, label }`: events connections can listen for; each `type` in its own namespace |
| `actions` | | `{ id, label, params }`: what it can do when a connection fires; ids unique |
| `settings` | | Set once in Sol, read by every paired device; keys unique |
| `connections` | | Connections it works best with, offered in Sol |

- A **field** (an action's `params` entry, and each setting) has a `key`, a
  `label`, a `kind` and an optional `hint` (one line shown under it); a param
  may be `required`, a setting may have a `default` (checked against its kind)
  and a `shared` name.
- **Kinds** are `text` and `secret` (at most 4000 characters; a secret is
  handed only to the world's own devices and never shown again), `url`
  (`http://` or `https://`, at most 2000, a trailing `/` dropped), `number`,
  `toggle` (`true`/`false`) and `time` (`HH:MM`, in the person's time zone).
  A kind Sol doesn't know reads as `unknown`: Sol shows the setting with a
  note that it needs a newer Sol, takes no value for it, and hands its apps
  the `default` alone.
- A setting with `"shared": "navidrome-url"` has one value for every world
  whose setting names the same thing: set in any of them, Sol keeps it once,
  hands it to all of them and sends each of them `sol.settings` when it
  changes. Use it for what the person would otherwise type twice. A shared
  `secret` is shared only between the worlds built into Sol.
- `connections`: `when` is an event type, `then_world` and `then_action` an
  installed world's action, `params` its parameters, `why` one sentence. Sol
  offers them once both worlds are installed and makes one only when the
  person says so. A moon lists the ones to and from its planet.
- **Fit:** Sol refuses to install a world whose `min_sol` is newer than
  itself, or whose `protocol` it doesn't speak, and says why on the world's
  page. A manifest Sol can't read (a newer Sol's, say) shows as a broken
  world with the reason, and its apps can't pair or sync until it is fixed.

Installing a world in Sol gives it a database and lets its apps pair. Sol's
world page offers the app files of its latest release: the Linux `.tar.gz`
the worlds ship (`sol-<world>-<version>-linux-<arch>.tar.gz`, the app's
binary), and an `.AppImage` if a release has one; other files (`.sig`,
`SHA256SUMS`, `sol-world.json`) are not offered.

## Pairing

| Request | Body | Answer |
| --- | --- | --- |
| `POST /api/v1/pair` | `PairRequest` | `200` `PairStarted` |
| `POST /api/v1/pair/claim?wait=S` | `PairClaim` | `200` `Paired`, `202` `Pending`, `410` |
| `GET /api/v1/me` | | `200` `DeviceMe` |
| `DELETE /api/v1/me` | | `204`: this device is unpaired |

```jsonc
// PairRequest
{ "world": "terra", "device": "Desktop", "platform": "linux" }
// PairStarted
{ "id": "…", "code": "ABCD-EFGH", "secret": "…", "expires_at": "2026-03-01T19:14:00Z" }
// PairClaim
{ "id": "…", "secret": "…" }
// Paired
{ "token": "sol_…", "device_id": "…", "world": "terra",
  "user": { "id": "…", "username": "lukas", "tz": "Europe/Copenhagen" } }
// Pending
{ "status": "pending" }
// DeviceMe
{ "device_id": "…", "world": "terra", "user": { … }, "database": "…" }
```

1. The app sends `POST /api/v1/pair` and shows the person the `code` (eight
   letters and digits, valid for ten minutes). `device` is 1 to 64
   characters; `platform` (optional, at most 16) is `linux`: Sol takes any
   short name and shows the ones it knows. `404` means the world isn't
   installed in Sol.
2. The person approves the request carrying that code in Sol, under Devices or
   on the world's page.
3. The app polls `POST /api/v1/pair/claim?wait=25` (Sol waits up to `S` ≤ 25
   seconds for the approval). `202` means not yet; `200` carries the token,
   once; `410` means it expired, was declined or is unknown: start again.

Sol keeps at most 20 open requests, 5 from one address; past that `pair` is
`429`. Ten failed attempts (unknown worlds, wrong secrets) from one address in
15 minutes shut pairing for that address for a while (`429` with
`Retry-After`).

`user` is the person the device belongs to: `tz` is their IANA time zone, and
"today" in a world means today there. The token works for that world and that
person only. Keep it in the system keyring. Each device can be unpaired on its
own, in Sol or by the app itself; a `401` means the app should pair again.

## Data

A world keeps JSON documents in named collections, separately for each
person. Every write takes the next `version` in that world's database.

| Request | Body | Answer |
| --- | --- | --- |
| `GET /api/v1/docs/{collection}` | | `200` `[Doc]`: the live documents, without tombstones |
| `GET /api/v1/docs/{collection}/{id}` | | `200` `Doc`, or `404` |
| `PUT /api/v1/docs/{collection}/{id}` | `Put` | `200` `Doc`, or `409` `Conflict` |
| `DELETE /api/v1/docs/{collection}/{id}` | | `200` `Doc` (the tombstone), or `404` |
| `GET /api/v1/changes?after=N&wait=S&limit=L` | | `200` `ChangePage` |

```jsonc
// Doc
{ "collection": "habits", "id": "walk", "version": 42, "updated_at": "2026-03-01T19:04:00Z",
  "deleted": false, "data": { "name": "Walk outside" } }
// Put
{ "data": { "name": "Walk outside" }, "if_version": 41 }
// Conflict (409)
{ "error": "conflict", "message": "the document changed in the meantime", "current": { …Doc } }
// ChangePage
{ "changes": [ …Doc ], "next": 42, "more": false, "database": "…" }
```

- **Names:** a collection is lowercase letters, digits, `-` and `_`, starting
  with a letter, at most 64 (`habits`, `check_ins`); an id is letters, digits,
  `-` and `_`, at most 128 (a UUID fits). Anything else is `400`.
- **Writes:** `data` is any JSON but `null` (`400`; delete instead), at most
  1 MiB (`413`). With `if_version` the write happens only if the document is
  still at that version (`0`: only if it doesn't exist yet); otherwise `409`
  with the document as it is now in `current` (`null` when there is none).
  Without it the write always happens.
  `Link` sends `if_version` with every document write: the version of
  Sol's copy the write was made on (`0` for one Sol hasn't seen). On `409`
  it keeps `current` and holds its own copy aside as a conflict for the
  person to settle (sending it again over `current`'s version if they
  pick it); when `current` already says the same, the write had gone
  through and nothing is held. Writes answered `400` or `413` are held
  with the message, not dropped. A `401` ends the pairing, and writes not
  yet sent are held until the device pairs again as the same person
  (`user.id`), then sent in order.
- **Deletes** leave a tombstone: `deleted: true`, `data: null`, a new
  `version`. It syncs like any change. Deleting what isn't there is `404`.
- **Changes** are every write after version `N`, oldest first, tombstones
  included: at most `L` (default and maximum 500) a page, `more: true` when
  there are more (ask again straight away). With nothing new, Sol waits up to
  `S` ≤ 25 seconds for a write before answering an empty page; `next` is then
  `N` again.
- **Staying in sync:** a device keeps the last `next` it saw and asks for the
  changes after it, together with the `database` it came with. `database` is
  the id of Sol's database: made when Sol first starts, and made again when
  Sol starts on one of its backup copies. Cursors belong to one id: when it
  changes, Sol was restored, its versions (and its inbox) run again from an
  older point, and the device starts over from `after=0`, keeping its own
  writes not yet sent. `Link` keeps what it has on screen until that full
  read is in, then drops what Sol no longer has.

## Events and connections

`POST /api/v1/events` with an `Emit` records something that happened and
answers `200` with the stored `Envelope`.

```jsonc
// Emit
{ "id": "0192f1c4-…", "type": "terra.habit.checked", "v": 1,
  "summary": "Did “Walk outside”, 4 days in a row",
  "subject": "habit/walk", "data": { "habit": "walk", "streak": 4 } }
// Envelope
{ "id": "0192f1c4-…", "type": "terra.habit.checked", "v": 1, "source": "terra", "seq": 812,
  "time": "2026-03-01T19:04:00Z", "user": "…", "subject": "habit/walk",
  "summary": "Did “Walk outside”, 4 days in a row", "data": { … } }
```

- `type` is `<world>.<thing>.<verb>` in the app's own namespace: at least
  three parts, each 1 to 40 lowercase letters, digits, `-` and `_`.
- `summary` is one sentence for people, at most 200 characters; Sol's
  activity log and notifications show it as written.
- `data` is at most 64 KiB; keep the rest in documents. `v` is the version
  of `data`'s shape for this type (`1` when left out).
- `id` (a UUIDv7, optional) makes retries safe: the same id posted again by
  the same world and person returns the stored envelope. An id another world
  or person already used is `400`; give the event a new one.
- `source` and `user` are set by Sol from the pairing; `seq` is the event's
  place in Sol's log.

When an event's type matches a connection the person made in Sol ("when
Mercury finishes a review, tick the Japanese habit in Terra"), Sol puts a
delivery in the target world's inbox.

## The inbox

| Request | Answer |
| --- | --- |
| `GET /api/v1/inbox?after=N&wait=S` | `200` `InboxPage` |
| `POST /api/v1/inbox/{n}/claim` | `204`: this device acts on it; `409`: another device already did |

```jsonc
// InboxPage
{ "deliveries": [{ "n": 7, "action": "tick-habit", "params": { "habit": "Japanese" },
                   "event": { …Envelope }, "connection": "…",
                   "created_at": "2026-03-01T19:04:00Z" }],
  "next": 7, "database": "…" }
```

- A page holds at most 100 deliveries after `N`, oldest first; with none,
  Sol waits up to `S` ≤ 25 seconds. `next` and `database` work as for
  changes.
- A world's inbox is shared by all of the person's devices for that world,
  so before acting on a delivery an app claims it. Claimed deliveries
  disappear from the other devices' inboxes; claiming one this device
  already claimed is `204` again.
- `event` is the event that set it off and `connection` the connection's id,
  when there was one.
- Two actions come from Sol itself. `widget.toggle`, with `{ "widget",
  "item", "done" }`, is someone ticking a box in one of the world's widgets on
  Sol's dashboard. `sol.settings`, with `{}`, is for every device and is never
  claimed: the world's settings changed, read them again.
- Sol keeps deliveries for 30 days.

## Settings

`GET /api/v1/settings` answers `200` with the world's settings as set in Sol,
as an object of key to value: defaults filled in, secrets included, a shared
setting's value from whichever world it was set in. A setting with no value
and no default is left out.

## Widgets

`PUT /api/v1/widgets/{id}` with `{ "title", "view" }` replaces one of the
world's widgets on Sol's dashboard (`204`); `DELETE` takes it away (`204`).
The id is a document id of at most 64 characters, the title 1 to 60
characters, the view at most 32 KiB. Push again whenever what it shows
changes.

```jsonc
// WidgetView: every part optional
{ "figure": "2 / 3", "caption": "habits done today",
  "progress": { "value": 2, "max": 3 },
  "rows": [{ "label": "Streak", "value": "4 days" }],
  "items": [{ "id": "walk", "label": "Walk outside", "meta": "4 days", "done": true, "toggle": true }],
  "empty": "Nothing to do today." }
```

`items` are a checklist when they have `done`; `toggle: true` lets people tick
them from the dashboard, which Sol shows at once and delivers as
`widget.toggle`.

## The write budget

Each paired device may make `SOL_WRITE_BUDGET` writes an hour (10 000 unless
the person set another; `0` is no limit), all at once if it likes: the budget
fills up again at that rate. Writes are document `PUT`s and `DELETE`s, events,
and widget `PUT`s and `DELETE`s; reading is never limited. Past the budget
Sol answers `429` with `Retry-After: <seconds>`. The app waits that long
before it writes again, keeping its writes in order meanwhile (`Link` does).

## Versions and the protocol header

`GET /_sol/health` (no token, no header) answers
`{ "status": "ok", "version", "arch", "protocol", "min_protocol", "uptime_s", "db" }`:
`protocol` is the newest protocol Sol speaks and `min_protocol` the oldest.

Every request to `/api/v1/*` must carry `Sol-Protocol`. A request without it,
or with a protocol outside that range, is `426` with `error: "protocol"` and a
message saying which side to update. `GET /api/v1/me` and `/api/v1/update*`
are exempt, so an app that is too old can always find out who it is and
update itself. (Sol checks the token first: a bad token is `401` either way.)

## Updates

Apps never ask GitHub for updates (the repositories can be private):

1. `GET /api/v1/update?version=0.1.0&target=linux-x86_64&kind=portable`
   (`target` is `<os>-<arch>`; `kind` is how the copy was installed:
   `portable`, the app's own binary installed by itself, is the one kind
   today; Sol answers any other without an offer). Sol looks at the world's
   latest GitHub release with its own token and answers a `Check`.
2. `GET <offer.download>` (`/api/v1/update/download/{asset}`) streams the
   file through Sol; only files of the world's own latest release (`404`
   otherwise; `502` when GitHub doesn't hand it over).
3. The app checks `signature` against the public keys built into it (one or
   more, for moving to a new key), checks that the signature's trusted
   comment names `file` (`file:<name>`, as minisign writes it), that `file`
   is the name for its world, `version` and target, and that `version` is
   newer than its own, and only then installs. `orbit::updates` also starts
   the new binary with `--version` before it replaces the old one, keeps
   the old one beside it, and puts it back if the new version never comes
   up.

```jsonc
// Check
{ "latest": "v0.2.0", "offer": { …Offer }, "message": null }
// Offer
{ "version": "0.2.0", "notes": "…", "published_at": "2026-10-06T08:00:00Z",
  "file": "sol-terra-0.2.0-linux-x86_64.tar.gz", "size": 1234567,
  "download": "/api/v1/update/download/123", "signature": "…" }
```

`offer` is there when a newer version has a signed file for that target and
kind; otherwise `message` says why not, for people (no release yet, GitHub
unreachable, no file for this platform, unsigned), and is `null` when the app
is up to date.

Release files are named `sol-<world>-<version>-<target>.tar.gz`, holding a
single file `sol-<world>` (the app's binary), with the signature beside each
as `<file>.sig`; Sol offers a file only when its signature is there. The
signature is minisign's: the `.minisig` file's text (an untrusted comment
line, the signature, a trusted comment line and the global signature) encoded
in base64, and the app's key is the minisign public key file's text (its
comment line and the key) encoded the same way. `orbit::updates::Updates`
does all of this for a Rust app.

## Errors

Every error is `{ "error": "<code>", "message": "<a sentence for people>" }`
(`ErrorBody`), except `409` on a document, which adds `current`. The codes are
stable; the messages are not.

| Status | `error` | When |
| --- | --- | --- |
| `400` | `bad_request` | Bad input: names, sizes, event types, an event id that isn't the app's, a setting value of the wrong kind |
| `401` | `unauthorized` | No token, or the device isn't paired (any more): pair again |
| `403` | `forbidden` | Only in Sol's web API |
| `404` | `not_found` | No such document, world or file |
| `409` | `conflict` | `if_version` didn't match, or the delivery is another device's |
| `410` | `gone` | The pairing request expired, was declined or is unknown |
| `413` | `too_large` | A document over 1 MiB (a request body over 2 MiB is turned away before it is read, without this body) |
| `426` | `protocol` | No `Sol-Protocol`, or one Sol doesn't speak |
| `429` | `rate_limited` | Too many pairing requests or failures, or the write budget is used up; `Retry-After` says when to come back |
| `500` | `internal` | Something went wrong in Sol; the details stay in its log |
| `502` | `github` | GitHub didn't hand over a release file |

## Compatibility

Protocol 2 stays as it is for Sol 1.x and the apps built on orbit 1.x. These
are **not** breaking, and can come in any Sol or orbit release:

- New optional fields in requests, answers and the manifest, with a default
  when left out. Readers ignore fields they don't know.
- New values of an enumeration (`kind` of a setting or an action's param,
  `kind` of an install, a release's `status`): readers take a value they don't
  know as `unknown` and carry on (orbit's enums end in `Unknown`), so a newer
  world or Sol never makes an older one fail to read.
- New requests, new actions from Sol (like `sol.settings`), new event types,
  new error codes for new situations.
- Loosening a limit (a bigger page, a longer name).

These **are** breaking, and only come with protocol 3: removing or renaming a
request, a field or an enumeration value; changing a field's type or meaning;
making an optional field required; tightening a limit; changing what a status
code means, the release file names or the signature format. Sol keeps
speaking the older protocol for a while after a new one (`min_protocol`), and
an app outside its range is told so with `426`.
