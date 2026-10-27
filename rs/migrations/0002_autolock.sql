-- v4.1: 自动锁票移到 tt-agent
ALTER TABLE watches ADD COLUMN auto_lock INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN lock_num_seats INTEGER NOT NULL DEFAULT 2;
ALTER TABLE watches ADD COLUMN lock_max_retries INTEGER NOT NULL DEFAULT 3;
ALTER TABLE watches ADD COLUMN lock_seats TEXT NOT NULL DEFAULT '[]';

CREATE TABLE IF NOT EXISTS lock_state (
    watch_id      TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    cinema_id     TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending', -- pending|failed|ok|exhausted
    tries         INTEGER NOT NULL DEFAULT 0,
    last_result   TEXT,
    last_error    TEXT,
    last_event_id TEXT,
    updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (watch_id, cinema_id)
);

INSERT OR IGNORE INTO settings(key, value) VALUES ('lock_headless', 'true');
