-- Writes the app couldn't send as they were, kept rather than lost.

-- The version of Sol's copy a waiting write was made on: sent as
-- `if_version`, so a write on top of an older copy comes back as a
-- conflict instead of replacing what another device wrote. Empty: the
-- write goes in whatever is there (writes from before this column, and
-- after Sol was restored from a copy).
ALTER TABLE pending ADD COLUMN base INTEGER;

-- Writes set aside, each with why:
--   conflict  the document changed on another device first; `theirs` is
--             Sol's copy (JSON), `body` this device's version
--   rejected  Sol turned it down (too large, a bad name); `message` says why
--   earlier   waiting when the device was unpaired; sent again when it pairs
--             with the same person (`person`), shown otherwise
CREATE TABLE held (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    why        TEXT NOT NULL,
    kind       TEXT NOT NULL,
    collection TEXT,
    id         TEXT,
    body       TEXT NOT NULL,
    base       INTEGER,
    theirs     TEXT,
    message    TEXT,
    person     TEXT,
    server     TEXT,
    at         TEXT NOT NULL
);
