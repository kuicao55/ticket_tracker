-- v4: 无头主服务数据库（SQLite）
-- 通知投递不持久化；这里只存配置、watch/poll 状态、用户订阅与图表采样。

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 影院注册表（收藏夹/名称缓存）
CREATE TABLE IF NOT EXISTS cinemas (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL DEFAULT '',
    favorite    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- poll = 当前被 enabled watch 使用的唯一影院抓取目标
CREATE TABLE IF NOT EXISTS polls (
    cinema_id        TEXT PRIMARY KEY,
    cinema_name      TEXT NOT NULL DEFAULT '',
    watch_count      INTEGER NOT NULL DEFAULT 0,
    last_polled_at   TEXT,
    last_ok          INTEGER,
    last_error       TEXT,
    shows_count      INTEGER NOT NULL DEFAULT 0,
    last_duration_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS watches (
    id          TEXT PRIMARY KEY,
    filter_key  TEXT NOT NULL UNIQUE,
    movie_id    INTEGER NOT NULL,
    movie_name  TEXT NOT NULL DEFAULT '',
    time_spec   TEXT NOT NULL DEFAULT '{"type":"all"}',
    imax_only   INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_by  TEXT,
    visibility  TEXT NOT NULL DEFAULT 'public',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS watch_cinemas (
    watch_id   TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    cinema_id  TEXT NOT NULL,
    PRIMARY KEY (watch_id, cinema_id)
);
CREATE INDEX IF NOT EXISTS idx_watch_cinemas_cinema ON watch_cinemas(cinema_id);

-- 每个 watch × 影院的上次桶基线（engine::WatchCinemaState JSON）
CREATE TABLE IF NOT EXISTS watch_state (
    watch_id    TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    cinema_id   TEXT NOT NULL,
    state_json  TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (watch_id, cinema_id)
);

-- 基线历史采样（折线图）
CREATE TABLE IF NOT EXISTS watch_metrics (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id     TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    recorded_at  TEXT NOT NULL,
    total_shows  INTEGER NOT NULL DEFAULT 0,
    imax_shows   INTEGER NOT NULL DEFAULT 0,
    bucket_count INTEGER NOT NULL DEFAULT 0,
    added_shows  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_watch_metrics_watch_time
    ON watch_metrics(watch_id, recorded_at);

CREATE TABLE IF NOT EXISTS users (
    id          TEXT PRIMARY KEY,
    openid      TEXT UNIQUE,
    unionid     TEXT,
    nickname    TEXT NOT NULL DEFAULT '',
    avatar_url  TEXT,
    status      TEXT NOT NULL DEFAULT 'active',
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS channels (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    type        TEXT NOT NULL,
    config_json TEXT NOT NULL DEFAULT '{}',
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS watch_channels (
    watch_id    TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    channel_id  TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    enabled     INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (watch_id, channel_id)
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    watch_id    TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    channel_id  TEXT REFERENCES channels(id) ON DELETE SET NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_sub_user_watch_channel
    ON subscriptions(user_id, watch_id, COALESCE(channel_id, ''));
CREATE INDEX IF NOT EXISTS idx_sub_watch ON subscriptions(watch_id);

CREATE TABLE IF NOT EXISTS push_consents (
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    template_id  TEXT NOT NULL,
    remaining    INTEGER NOT NULL DEFAULT 0,
    expires_at   TEXT,
    PRIMARY KEY (user_id, template_id)
);

-- 默认渠道（不自动绑定到任何 watch）
INSERT OR IGNORE INTO channels (id, name, type, config_json, enabled) VALUES
    ('discord-default', 'Discord', 'discord', '{}', 1),
    ('email-default', 'Email', 'email', '{}', 1),
    ('webhook-default', 'Webhook', 'webhook', '{}', 1),
    ('wechat-local', '微信（本机）', 'wechat_local', '{}', 1),
    ('xhs-local', '小红书（本机）', 'xhs_local', '{}', 1),
    ('wechat-miniprogram', '微信小程序', 'wechat_miniprogram', '{}', 1);
