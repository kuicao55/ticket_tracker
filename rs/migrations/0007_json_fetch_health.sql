-- v4.11: 把全城扫描的 JSON 请求并入 Poll 健康 / 今日概览。
ALTER TABLE poll_health ADD COLUMN city_total INTEGER NOT NULL DEFAULT 0;
ALTER TABLE poll_health ADD COLUMN city_ok INTEGER NOT NULL DEFAULT 0;
ALTER TABLE poll_health ADD COLUMN city_err INTEGER NOT NULL DEFAULT 0;
ALTER TABLE poll_health ADD COLUMN city_blocked INTEGER NOT NULL DEFAULT 0;
ALTER TABLE poll_health ADD COLUMN city_duration_ms INTEGER NOT NULL DEFAULT 0;

-- 全城 watch 最近一轮的城市接口耗时（showdays + 城市影院列表的平均单次耗时）。
ALTER TABLE watch_city_state ADD COLUMN last_duration_ms INTEGER NOT NULL DEFAULT 0;
