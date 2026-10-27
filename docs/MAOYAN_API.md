# 猫眼接口实测笔记

本文记录 ticket-tracker v3 小时桶引擎依赖的关键事实。接口可能随猫眼改版失效，
升级前请用 `curl` 重新验证。

## 影院详情（唯一需要抓取的接口）

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

## 不存在的东西（不要浪费时间）

| 尝试 | 结果 |
|---|---|
| `cinemaDetail?cinemaId=37534,42020` | 返回空 `cinemaData` |
| `cinemaDetail?cinemaId=37534&cinemaIds=37534,42020` | `cinemaIds` 被忽略，只返回第一个影院 |
| `cinemaDetail?cinemaId=37534&movieId=1545360` | 返回内容不变，不能按影片过滤 |
| `ajax/shows?cinemaId=...&movieId=...&date=...` | 404 |
| `ajax/cinema/movies?ci=10&cinemaId=...` | 404（旧接口已下线） |

因此：

- **抓取的最小原子单位是影院**；
- 一个 watch 只关心影院 A 的影片 1，也必须拿影院 A 的完整 JSON；
- 若之后在同一影院加影片 2，**不会增加请求数**；
- 所有 enabled watch 的影院 ID 去重并集，就是每轮的最小抓取集合。

## 缓存 / 条件请求

响应头带 `ETag`，但实测携带 `If-None-Match` 仍然返回 `200` + 完整 body，
**不能依赖 304 省流量**。主要带宽优化是 gzip 和跨轮复用连接。

## 并发建议

- 单实例默认 `fetch_concurrency = 8`，范围 1..=64；
- 每轮每个影院只请求一次；
- 对 1000+ 影院的大规模部署，后续应按影院哈希分片到多进程/多机，避免单进程
  同时打满猫眼风控。
