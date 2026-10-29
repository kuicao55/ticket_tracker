use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ticket_tracker::db::Db;
use ticket_tracker::server::{api, web, AppState};
use tower::ServiceExt;

async fn app() -> axum::Router {
    let db = Db::connect_memory().await.unwrap();
    let state: Arc<AppState> = AppState::new(db);
    api::router(state.clone()).merge(web::router(state))
}

#[tokio::test]
async fn watch_api_and_dashboard_render() {
    let app = app().await;

    let body = serde_json::json!({
        "movie_id": 1545360,
        "movie_name": "奥德赛",
        "cinemas": ["37534"],
        "time": "2026-11-12 19:00-22:00"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/watches")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["created"], true);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["counts"]["watches"], 1);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("Quiet eyes"));
    assert!(html.contains("奥德赛"));
}

#[tokio::test]
async fn watch_list_shows_facts_and_deletes() {
    let db = Db::connect_memory().await.unwrap();
    db.upsert_cinema("37534", "万达影城（朝阳大悦城店）", false)
        .await
        .unwrap();
    let state: Arc<AppState> = AppState::new(db.clone());
    let app = api::router(state.clone()).merge(web::router(state));

    let body = serde_json::json!({
        "movie_id": 1545360,
        "movie_name": "奥德赛",
        "cinemas": ["37534"],
        "time": "2026-11-12 19:00-22:00",
        "imax_only": true
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/watches")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let wid = v["watch_id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/watches")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("万达影城（朝阳大悦城店）"));
    assert!(html.contains("11-12 19:00–22:00"));
    assert!(html.contains(">IMAX</span>"));
    assert!(html.contains(&format!("/actions/watches/{wid}/delete")));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/actions/watches/{wid}/delete"))
                .header("hx-request", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("0 watches"));
    assert!(db.get_watch(&wid).await.unwrap().is_none());
    assert!(db.watch_cinemas(&wid).await.unwrap().is_empty());
}

#[tokio::test]
async fn watch_filter_key_is_deduped_via_api() {
    let app = app().await;
    let make = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/watches")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "movie_id": 1, "movie_name": "A",
                    "cinemas": ["2"], "time": "all"
                })
                .to_string(),
            ))
            .unwrap()
    };
    let r1 = app.clone().oneshot(make()).await.unwrap();
    let r2 = app.oneshot(make()).await.unwrap();
    let b1: serde_json::Value =
        serde_json::from_slice(&r1.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let b2: serde_json::Value =
        serde_json::from_slice(&r2.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(b1["watch_id"], b2["watch_id"]);
    assert_eq!(b2["created"], false);
}

#[tokio::test]
async fn dashboard_renders_poll_health_instead_of_baseline_chart() {
    let db = Db::connect_memory().await.unwrap();
    let state: Arc<AppState> = AppState::new(db.clone());
    let app = api::router(state.clone()).merge(web::router(state));

    db.insert_poll_health(3, 0, 0, 120, None).await.unwrap();
    db.insert_poll_health(0, 2, 2, 260, Some("37534: 猫眼接口异常（code=406）"))
        .await
        .unwrap();

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(html.contains("Poll 健康 · 今日"));
    assert!(html.contains("poll-health-svg"));
    assert!(html.contains(">00:00<"));
    assert!(html.contains(">24:00<"));
    assert!(html.contains("风控"));
    assert!(html.contains("code=406"));
    assert!(!html.contains("基线趋势"));
    assert!(!html.contains("data-chart-url"));
}

#[tokio::test]
async fn watch_channels_have_per_watch_config_and_autolock_card() {
    let db = Db::connect_memory().await.unwrap();
    let state: Arc<AppState> = AppState::new(db.clone());
    let app = api::router(state.clone()).merge(web::router(state.clone()));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/watches")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "movie_id": 9, "movie_name": "渠道测试",
                        "cinemas": ["2"], "time": "all"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let wid = v["watch_id"].as_str().unwrap().to_string();

    let body = "group=test-group&agent_url=http%3A%2F%2F127.0.0.1%3A9999";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/actions/watches/{wid}/channels/xhs-local/config"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cfg = db.watch_channel_config(&wid, "xhs-local").await.unwrap();
    assert_eq!(cfg["group"], "test-group");
    assert_eq!(cfg["agent_url"], "http://127.0.0.1:9999");

    let body = "lock_num_seats=2&lock_max_retries=4&lock_seats=5%E6%8E%926%E5%BA%A7";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/actions/watches/{wid}/channels/autolock-local/config"
                ))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let watch = db.get_watch(&wid).await.unwrap().unwrap();
    assert!(watch.auto_lock);
    assert_eq!(watch.lock_max_retries, 4);
    assert_eq!(watch.lock_seats_value(), vec!["5排6座"]);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/watches/{wid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("channel-dialog-xhs-local"));
    assert!(html.contains("群：test-group"));
    assert!(html.contains("AUTO LOCK"));
    assert!(html.contains("channel-dialog-autolock-local"));
    assert!(html.contains("已开启"));
}

#[tokio::test]
async fn monitor_toggle_pauses_and_resumes() {
    let db = Db::connect_memory().await.unwrap();
    let state: Arc<AppState> = AppState::new(db.clone());
    let app = api::router(state.clone()).merge(web::router(state.clone()));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/actions/monitor/toggle")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        db.settings_map()
            .await
            .unwrap()
            .get("monitor_enabled")
            .map(String::as_str),
        Some("false")
    );

    let mut html = String::new();
    for _ in 0..30 {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        html = String::from_utf8(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        if html.contains("已暂停") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(html.contains("已暂停"));
    assert!(html.contains("action=\"/actions/monitor/toggle\""));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/actions/monitor/toggle")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        db.settings_map()
            .await
            .unwrap()
            .get("monitor_enabled")
            .map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn watch_local_channel_can_reuse_global_gofer() {
    let db = Db::connect_memory().await.unwrap();
    db.set_setting("agent_url", "http://global-gofer:8790")
        .await
        .unwrap();
    db.set_setting("agent_token", "global-token").await.unwrap();
    let state: Arc<AppState> = AppState::new(db.clone());
    let app = api::router(state.clone()).merge(web::router(state.clone()));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/watches")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "movie_id": 11, "movie_name": "Gofer 复用测试",
                        "cinemas": ["2"], "time": "all"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let wid = v["watch_id"].as_str().unwrap().to_string();

    let post_config = |body: &'static str| {
        let app = app.clone();
        let wid = wid.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/actions/watches/{wid}/channels/wechat-local/config"
                    ))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // 选择全局时，即使表单里残留自定义地址/ token，也不能写进 watch。
    let resp = post_config(
        "agent_source=global&agent_url=http%3A%2F%2Fshould-not-save%3A9999&agent_token=ignored",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cfg = db.watch_channel_config(&wid, "wechat-local").await.unwrap();
    assert!(cfg.get("agent_url").is_none(), "global must not store URL");
    assert!(
        cfg.get("agent_token").is_none(),
        "global must not store token"
    );

    let html = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/watches/{wid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("使用设置里的全局 Gofer"));
    assert!(html.contains("http://global-gofer:8790"));
    assert!(html.contains("Gofer 全局"));

    // 手动填写时保存到该 watch 自己的配置。
    let resp = post_config(
        "agent_source=custom&agent_url=http%3A%2F%2Fcustom-gofer%3A9999&agent_token=custom-token",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cfg = db.watch_channel_config(&wid, "wechat-local").await.unwrap();
    assert_eq!(cfg["agent_url"], "http://custom-gofer:9999");
    assert_eq!(cfg["agent_token"], "custom-token");

    let html = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/watches/{wid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("Gofer 自定义"));
    assert!(html.contains("http://custom-gofer:9999"));

    // 再切回全局会清掉该 watch 的自定义配置。
    let resp = post_config("agent_source=global").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cfg = db.watch_channel_config(&wid, "wechat-local").await.unwrap();
    assert!(cfg.get("agent_url").is_none());
    assert!(cfg.get("agent_token").is_none());
}
