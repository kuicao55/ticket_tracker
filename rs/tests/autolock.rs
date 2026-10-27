use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use ticket_tracker::db::{Db, NewWatch};
use ticket_tracker::server::{api, dispatcher, AppState};
use ticket_tracker::time_spec::TimeSpec;
use tower::ServiceExt;

async fn make_watch(db: &Db, auto_lock: bool) -> String {
    db.create_or_reuse_watch(&NewWatch {
        movie_id: 1545360,
        movie_name: Some("奥德赛".into()),
        cinemas: vec!["37534".into()],
        time_spec: TimeSpec::parse("2026-11-12 19:00-22:00").unwrap().to_json(),
        imax_only: false,
        created_by: None,
        visibility: "public".into(),
        auto_lock,
        lock_num_seats: 2,
        lock_max_retries: 2,
        lock_seats: vec![],
    })
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn autolock_dispatches_once_and_callback_updates_state() {
    let db = Db::connect_memory().await.unwrap();
    let state = AppState::new(db.clone());
    let wid = make_watch(&db, true).await;

    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    async fn mock_lock(
        State(cap): State<Arc<Mutex<Vec<Value>>>>,
        Json(body): Json<Value>,
    ) -> StatusCode {
        cap.lock().unwrap().push(body);
        StatusCode::ACCEPTED
    }
    let app = Router::new()
        .route("/lock", post(mock_lock))
        .with_state(cap);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    db.set_setting("agent_url", &format!("http://{}", addr))
        .await
        .unwrap();

    let event = json!({
        "event_id": "evt_test_1",
        "watch_id": wid,
        "triggers": [{
            "cinema_id": "37534",
            "cinema_name": "测试影院",
            "date": "2026-11-12",
            "hour": 19,
            "added": []
        }]
    });
    dispatcher::maybe_dispatch_autolock(state.clone(), &event).await;
    dispatcher::maybe_dispatch_autolock(state.clone(), &event).await;
    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "inflight should prevent duplicate submit");
    assert_eq!(reqs[0]["confirm"], Value::Null); // confirm 是 agent 内部固定 true
    assert_eq!(reqs[0]["watch_id"], wid);
    assert_eq!(reqs[0]["cinema_id"], "37534");
    assert_eq!(reqs[0]["time_range"], "19:00-22:00");

    // 模拟 agent 回调失败结果
    let app = api::router(state.clone());
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/agent/lock-result")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "event_id": "evt_test_1:37534",
                        "watch_id": wid,
                        "movie_name": "奥德赛",
                        "cinema_id": "37534",
                        "cinema_name": "测试影院",
                        "show_date": "2026-11-12",
                        "result": "ShowGone",
                        "summary": "找不到场次",
                        "elapsed_sec": 1.2
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(b["status"], "failed");
    assert_eq!(b["tries"], 1);

    // 同一 event 重放 -> duplicate，不重复消耗重试
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/agent/lock-result")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "event_id": "evt_test_1:37534",
                        "watch_id": wid,
                        "cinema_id": "37534",
                        "show_date": "2026-11-12",
                        "result": "ShowGone",
                        "summary": "找不到场次"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let b: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(b["duplicate"], true);
    let ls = db.lock_state(&wid, "37534").await.unwrap().unwrap();
    assert_eq!(ls.tries, 1);
}

#[tokio::test]
async fn autolock_skips_watch_without_switch() {
    let db = Db::connect_memory().await.unwrap();
    let state = AppState::new(db.clone());
    let wid = make_watch(&db, false).await;
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    // no agent needed: auto_lock=false returns before HTTP
    db.set_setting("agent_url", "http://127.0.0.1:1")
        .await
        .unwrap();
    dispatcher::maybe_dispatch_autolock(
        state,
        &json!({
            "event_id": "evt", "watch_id": wid,
            "triggers": [{"cinema_id": "37534", "date": "2026-11-12"}]
        }),
    )
    .await;
    assert!(captured.lock().unwrap().is_empty());
}
