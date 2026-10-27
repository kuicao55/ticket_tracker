-- v4.1: Web 面板的电影/影院选择器缓存
CREATE TABLE IF NOT EXISTS movies (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    show_type   INTEGER NOT NULL DEFAULT 1, -- 1=热映 2=即将 3=经典 0=从 watch 导入
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_movies_name ON movies(name);
