-- v4.1: 每个 watch 独立配置通知渠道；自动锁票也作为 agent 渠道
ALTER TABLE watch_channels ADD COLUMN config_json TEXT NOT NULL DEFAULT '{}';

INSERT OR IGNORE INTO channels (id, name, type, config_json, enabled)
VALUES ('autolock-local', '自动锁票', 'autolock', '{}', 1);

-- 兼容旧数据：已开启 auto_lock 的 watch 自动绑定自动锁票渠道
INSERT OR IGNORE INTO watch_channels (watch_id, channel_id, enabled, config_json)
SELECT id, 'autolock-local', 1, '{}' FROM watches WHERE auto_lock = 1;
