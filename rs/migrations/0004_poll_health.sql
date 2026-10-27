-- v4.0.4: 每轮 poll 的健康采样
CREATE TABLE IF NOT EXISTS poll_health (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    recorded_at       TEXT NOT NULL DEFAULT (datetime('now')),
    cinemas_total     INTEGER NOT NULL DEFAULT 0,
    cinemas_ok        INTEGER NOT NULL DEFAULT 0,
    cinemas_err       INTEGER NOT NULL DEFAULT 0,
    blocked_cinemas   INTEGER NOT NULL DEFAULT 0,
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    error_summary     TEXT
);
CREATE INDEX IF NOT EXISTS idx_poll_health_time ON poll_health(recorded_at);
