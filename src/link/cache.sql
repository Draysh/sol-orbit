-- A world app's local copy of its data in Sol, and what it still has to send.
CREATE TABLE docs (
    collection TEXT NOT NULL,
    id         TEXT NOT NULL,
    -- The version in Sol; 0 for a document Sol hasn't seen yet.
    version    INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0,
    data       TEXT NOT NULL,
    PRIMARY KEY (collection, id)
);

-- Local writes and events waiting for Sol, in order.
CREATE TABLE pending (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,
    collection TEXT,
    id         TEXT,
    body       TEXT NOT NULL
);
CREATE INDEX pending_by_doc ON pending (collection, id);

-- Where this device stands: its server, user, cursors and cached settings.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
