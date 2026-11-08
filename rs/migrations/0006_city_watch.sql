-- v4.10: 全城开售 watch（独立 scope；不写 watch_cinemas）
ALTER TABLE watches ADD COLUMN scope TEXT NOT NULL DEFAULT 'cinema';
ALTER TABLE watches ADD COLUMN city_id INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN city_name TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_watches_scope_enabled ON watches(scope, enabled);

CREATE TABLE IF NOT EXISTS watch_city_state (
    watch_id          TEXT PRIMARY KEY REFERENCES watches(id) ON DELETE CASCADE,
    fired             INTEGER NOT NULL DEFAULT 0,
    fired_at          TEXT,
    fired_cinema_id   TEXT,
    fired_cinema_name TEXT,
    fired_date        TEXT,
    fired_show_time   TEXT,
    fired_seq_no      TEXT,
    fired_is_imax     INTEGER NOT NULL DEFAULT 0,
    last_polled_at    TEXT,
    last_ok           INTEGER,
    last_error        TEXT,
    last_dates_json   TEXT NOT NULL DEFAULT '[]',
    last_cinema_count INTEGER NOT NULL DEFAULT 0,
    last_show_count   INTEGER NOT NULL DEFAULT 0,
    last_imax_count   INTEGER NOT NULL DEFAULT 0,
    last_bucket_count INTEGER NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
