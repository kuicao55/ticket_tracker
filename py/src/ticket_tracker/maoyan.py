"""猫眼移动端接口客户端（无需登录、纯 JSON）。"""

import http.cookiejar
import json
import re
import ssl
import urllib.error
import urllib.request

API_TEMPLATE = "https://m.maoyan.com/ajax/cinemaDetail?cinemaId={cinema_id}"
FILM_API = "https://m.maoyan.com/ajax/detailmovie?movieId={movie_id}"
BUY_MOBILE = "https://m.maoyan.com/shows/{cinema_id}"
BUY_PC = "https://www.maoyan.com/cinema/{cinema_id}"
FILMS_LIST_URL = "https://www.maoyan.com/films?showType={show_type}"

USER_AGENT = ("Mozilla/5.0 (iPhone; CPU iPhone OS 15_0 like Mac OS X) "
              "AppleWebKit/605.1.15 (KHTML, like Gecko) "
              "Version/15.0 Mobile/15E148 Safari/604.1")
USER_AGENT_PC = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
                 "AppleWebKit/537.36 (KHTML, like Gecko) "
                 "Chrome/120 Safari/537.36")


def _ctx():
    c = ssl.create_default_context()
    c.check_hostname = False
    c.verify_mode = ssl.CERT_NONE
    return c


# 带 cookie jar 的 opener：PC films 页要跟 302 重定向并保留 uuid cookie
_pc_opener = urllib.request.build_opener(
    urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
    urllib.request.HTTPSHandler(context=_ctx()),
)


def _get(url, referer=None, retries=3, timeout=15):
    headers = {
        "User-Agent": USER_AGENT,
        "Accept": "application/json, text/plain, */*",
    }
    if referer:
        headers["Referer"] = referer
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(url, headers=headers)
            with urllib.request.urlopen(req, timeout=timeout, context=_ctx()) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
            last = e
            if i < retries - 1:
                import time
                time.sleep(3)
    raise RuntimeError("猫眼接口请求失败 {} : {}".format(url, last))


def fetch_films_list(show_type=1):
    """猫眼 PC films 列表（最多约 20 条）。

    showType=1 → 正在热映
    showType=2 → 即将上映
    showType=3 → 经典影片

    返回 [{id, name}, ...]，按页面顺序去重。
    """
    url = FILMS_LIST_URL.format(show_type=show_type)
    req = urllib.request.Request(url, headers={
        "User-Agent": USER_AGENT_PC,
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "Referer": "https://www.maoyan.com/",
    })
    with _pc_opener.open(req, timeout=15) as resp:
        html = resp.read().decode("utf-8")
    # <a href="/films/ID" ...>片名</a> 在页面里同时承担详情链接 + 标题，
    # 按 ID 去重保序即可拿到 (id, name) 对。
    pairs = re.findall(r'<a href="/films/(\d+)"[^>]*>([^<]+)</a>', html)
    seen = {}
    for mid, name in pairs:
        if mid not in seen:
            seen[mid] = name.strip()
    return [{"id": mid, "name": name} for mid, name in seen.items()]


# ----------------- 影城 -----------------

def fetch_cinema(cinema_id):
    """返回该影城当前 showData.movies[]（已开预售/排片的电影）。"""
    data = _get(API_TEMPLATE.format(cinema_id=cinema_id),
                referer=BUY_MOBILE.format(cinema_id=cinema_id))
    show_data = data.get("showData") or {}
    cinema_name = show_data.get("cinemaName") or "影城 {}".format(cinema_id)
    return {
        "cinema_id": str(cinema_id),
        "cinema_name": cinema_name,
        "movies": show_data.get("movies") or [],
    }


def movie_dates(movie):
    """提取该电影 show.plist[].dt 集合，升序。"""
    ds = set()
    for s in (movie.get("shows") or []):
        for p in (s.get("plist") or []):
            if p.get("dt"):
                ds.add(p["dt"])
    return sorted(ds)


def find_movie(cinema_payload, movie_id, keywords=None):
    """按 id 精确匹配，其次按关键词模糊。"""
    keywords = keywords or []
    for m in cinema_payload["movies"]:
        if str(m.get("id")) == str(movie_id):
            return m
    for m in cinema_payload["movies"]:
        nm = m.get("nm") or ""
        if any(kw in nm for kw in keywords):
            return m
    return None


# ----------------- 影片详情（辅助验证 / 智能补全名称） -----------------

def fetch_movie_name(movie_id):
    """返回影片名（nm），失败返回 None。"""
    try:
        data = _get(FILM_API.format(movie_id=movie_id))
    except RuntimeError:
        return None
    mv = data.get("detailMovie") or data.get("movie") or data
    return mv.get("nm") if isinstance(mv, dict) else None


def search_cinemas(cinema_ids, keyword):
    """[已废弃] 关键词搜索：猫眼没开放全局搜索 API，请改用 fetch_films_list。"""
    raise NotImplementedError(
        "search_cinemas 已废弃；猫眼无关键词搜索 API。"
        "请用 maoyan.fetch_films_list(1|2) 拿到热映/即将上映列表。")
