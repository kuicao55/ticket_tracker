# 猫眼接口实测笔记

本文记录 ticket-tracker v3 小时桶引擎依赖的关键事实。接口可能随猫眼改版失效，
升级前请用 `curl` 重新验证。

## 影院详情（cinema-scope watch 主接口）

```http
GET https://m.maoyan.com/ajax/cinemaDetail?cinemaId=<影院ID>
User-Agent: <iPhone Safari UA>
Accept: application/json, text/plain, */*
```

返回结构（节选）：

```json
{
  "cinemaId": 37534,
  "showData": {
    "cinemaId": 37534,
    "cinemaName": "MOViE MOViE 影城（前滩太古里店）",
    "movies": [
      {
        "id": 1545360,
        "nm": "奥德赛",
        "showCount": 8,
        "shows": [
          {
            "showDate": "2026-08-09",
            "plist": [
              {
                "seqNo": "202608090008709",
                "dt": "2026-08-09",
                "tm": "15:40",
                "th": "IMAX 激光厅",
                "tp": "IMAX2D"
              }
            ]
          }
        ]
      }
    ]
  }
}
```

要点：

- **一次返回单个影院的全部在售影片/场次**；
- `plist[]` 里没有 `seqNo` 的条目仍参与场次计数，但无法参与增场 diff；
- 实测 `cinemaId=37534`：约 **92 KB** 原始 JSON，**约 6 KB gzip**；
- 支持 gzip；ticket-tracker 的 reqwest 已开启 `gzip` feature。

## 城市维度（全城开售 watch）

v4.10 起，全城开售 watch 使用猫眼 H5 购票页的两个接口。**它们不是公开 API**，
改版风险高；所有调用集中在 `rs/src/maoyan.rs` + `rs/src/city.rs`，失败按错误处理。

### 可预售日期

```http
GET https://m.maoyan.com/api/mtrade/mmcs/show/v1/movie/showdays.json
    ?movieId=<电影ID>&channelId=4&cityId=<城市ID>
```

已开售：

```json
{"code":0,"data":{"dates":[{"date":"2026-12-31","isPredate":0}, ...]}}
```

**完全未开售**：

```json
{"code":109,"data":null,"errMsg":"未获取到该城市下影片的上映日期","success":false}
```

`code=109` 是正常状态（等待开售），不是错误。ticket-tracker 会把它当作空日期列表，
每轮只探这个轻量接口；等它返回日期后再查影院列表。

### 城市 × 影片影院列表

```http
GET https://m.maoyan.com/api/mtrade/mmcs/cinema/v2/select/movie/cinemas.json
    ?limit=200&offset=0&utm_term=7.5&client=iphone&channelId=4
    &showDate=YYYY-MM-DD&movieId=<电影ID>&sort=distance
    &cityId=<城市ID>&ci=<城市ID>&lat=-1&lng=-1
```

返回：

```json
{
  "code": 0,
  "data": {
    "paging": {"limit": 200, "offset": 0, "total": 119, "hasMore": false},
    "cinemas": [
      {
        "id": 37534,
        "name": "MOViE MOViE 影城（前滩太古里店）",
        "addr": "...",
        "sellPrice": "42.9",
        "showTimes": "近期场次: 10:00 | 15:40",
        "cellShows": [],
        "labels": [{"name": "IMAX厅"}]
      }
    ]
  }
}
```

要点：

- `limit=200` 实测一次拿全上海 119 家；仍按 `paging.hasMore` 分页，offset += 200。
- 非 IMAX 请求：`showTimes` 是“近期场次”摘要，实测与 `cinemaDetail` 的当日场次一致。
- IMAX 筛选：追加 `hallTypeIds=["2"]&hallType=["2"]`，只返回 IMAX 影院，且
  `cellShows[]` 带精确场次：

  ```json
  {"seqNo":"202612310012345","showTime":"10:00","langAndType":"英语 2D","price":"189元"}
  ```

- 也支持 `brandIds`、`dimIds`、`districtId`、`serviceIds` 等筛选；任一非 all 筛选会填充 `cellShows`。
- 实测 `2026-11-08` 上海《奥德赛》：119 家影院；加 IMAX 筛选后 27 家；gzip 后约 12.5KB。
- 未来日期只返回已开预售的影院；完全没排片的影片返回空 `cinemas` + `paging.total=0`。
- 不需要额外 token；手机 UA + Referer 即可，但请保持低频率并遵守猫眼风控。

### 全城 watch 的取数策略

1. 每轮按 `(cityId, movieId)` 调一次 showdays；`code=109` → 等待开售。
2. 拿到日期后按日期升序查城市影院列表；遇到第一个非空日期即停止（日期优先）。
3. 在该日期内按 `showTime` 取最早场次；同时间按影院 ID/seqNo 稳定排序。
4. 首批非空结果立即推送一次；`watch_city_state.fired` 落库后停止轮询。
5. 需要精确到厅/制式时优先用 `cellShows`；只有 `showTimes` 时保留 `seq_no=None`。

## 不存在的东西（不要浪费时间）

| 尝试 | 结果 |
|---|---|
| `cinemaDetail?cinemaId=37534,42020` | 返回空 `cinemaData` |
| `cinemaDetail?cinemaId=37534&cinemaIds=37534,42020` | `cinemaIds` 被忽略，只返回第一个影院 |
| `cinemaDetail?cinemaId=37534&movieId=1545360` | 返回内容不变，不能按影片过滤 |
| `ajax/shows?cinemaId=...&movieId=...&date=...` | 404 |
| `ajax/cinema/movies?ci=10&cinemaId=...` | 404（旧接口已下线） |

因此：

- **cinema-scope watch 的抓取最小原子单位仍是影院**；
- 一个 cinema watch 只关心影院 A 的影片 1，也必须拿影院 A 的完整 JSON；
- 若之后在同一影院加影片 2，**不会增加请求数**；
- 所有 enabled cinema watch 的影院 ID 去重并集，就是每轮的最小抓取集合；
- **全城开售 watch 是例外**：它走上面的城市维度接口，不逐影院抓取；
  但对单个影院的完整场次/厅/seat diff，仍以 `cinemaDetail` 为准。

## 缓存 / 条件请求

响应头带 `ETag`，但实测携带 `If-None-Match` 仍然返回 `200` + 完整 body，
**不能依赖 304 省流量**。主要带宽优化是 gzip 和跨轮复用连接。

## 并发建议

- 单实例默认 `fetch_concurrency = 8`，范围 1..=64；
- 每轮每个影院只请求一次；
- 对 1000+ 影院的大规模部署，后续应按影院哈希分片到多进程/多机，避免单进程
  同时打满猫眼风控。
