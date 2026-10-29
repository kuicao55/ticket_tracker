//! iGlance `iglance.service.v1` 协议接口测试。
//!
//! stop 会 `std::process::exit`（与 Gofer 一致），因此本文件不直接调用 stop；
//! 真实进程的启动 → manifest → stop 流程由 `scripts/` 之外的 curl 冒烟验证。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ticket_tracker::db::Db;
use ticket_tracker::server::{api, AppState};
use tower::ServiceExt;

async fn app() -> axum::Router {
    let db = Db::connect_memory().await.unwrap();
    let state: Arc<AppState> = AppState::new(db);
    api::router(state)
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn manifest_subcommand_prints_one_line_without_starting_service() {
    let exe = env!("CARGO_BIN_EXE_tt-server");
    let out = std::process::Command::new(exe)
        .args(["manifest", "--base-url", "http://127.0.0.1:9999"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    let json = lines[0]
        .strip_prefix("IGLANCE_SERVICE_MANIFEST ")
        .expect("manifest prefix");
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(v["schema"], "iglance.service.v1");
    assert_eq!(v["base_url"], "http://127.0.0.1:9999");
    assert_eq!(v["service"]["id"], "ticket_tracker");
    assert!(v["auth"].get("token").is_none());
    assert!(!stdout.contains("token\":\""));
}

#[tokio::test]
async fn service_status_exposes_protocol_fields_and_memory_groups() {
    let app = app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/service")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;

    assert_eq!(v["schema"], "iglance.service.v1");
    assert_eq!(v["ok"], true);
    assert_eq!(v["service"]["id"], "ticket_tracker");
    assert_eq!(v["service"]["name"], "Ticket Tracker");
    assert_eq!(v["service"]["kind"], "headless-service");
    assert_eq!(v["state"], "running");
    assert_eq!(v["paused"], false);
    assert_eq!(v["stopping"], false);
    assert_eq!(v["pid"], std::process::id());
    assert!(v["uptime_sec"].as_u64().is_some());
    assert!(v["started_at"].as_str().unwrap().contains('T'));

    assert!(v["urls"]["base"].as_str().unwrap().starts_with("http://"));
    assert!(v["urls"]["health"].as_str().unwrap().ends_with("/health"));
    assert_eq!(v["auth"]["header"], "X-Gofer-Token");
    assert_eq!(v["auth"]["token_required"], false);

    let groups = v["memory"]["groups"].as_array().unwrap();
    let ids: Vec<&str> = groups.iter().filter_map(|g| g["id"].as_str()).collect();
    assert_eq!(ids, ["service", "workers"]);
    let service = groups.iter().find(|g| g["id"] == "service").unwrap();
    assert_eq!(service["processes"], 1);
    assert_eq!(service["pids"][0], std::process::id());
    assert!(v["memory"]["total_rss_bytes"].as_u64().unwrap() > 0);

    let start = &v["controls"]["start"];
    assert_eq!(start["type"], "command");
    assert_eq!(start["shell"], "/bin/zsh");
    assert_eq!(start["manifest_prefix"], "IGLANCE_SERVICE_MANIFEST");
    assert!(start["binding_command"].is_string());
    assert!(start["manifest_command"]
        .as_str()
        .unwrap()
        .contains("manifest"));
    assert_eq!(v["controls"]["protocol"]["schema"], "iglance.service.v1");

    // /v1/service 别名与 /memory 端点。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/service")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/service/memory")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["memory"]["available"], true);
    assert_eq!(v["memory"]["source"], "ps-rss");

    // 公开健康检查。
    for path in ["/health", "/api/v1/health"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn service_token_is_enforced_when_panel_token_is_set() {
    let db = Db::connect_memory().await.unwrap();
    db.set_setting("panel_token", "test-token").await.unwrap();
    let state: Arc<AppState> = AppState::new(db);
    let app = api::router(state);

    let no_token = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/service")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);

    let wrong = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/service")
                .header("X-Gofer-Token", "nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    for (header, value) in [
        ("X-Gofer-Token", "test-token"),
        ("X-Agent-Token", "test-token"),
        ("Authorization", "Bearer test-token"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/service")
                    .header(header, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{header}");
        let v = json_body(resp).await;
        assert_eq!(v["auth"]["token_required"], true);
    }

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/service/pause")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pause_resume_and_toggle_change_monitor_state() {
    let app = app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/service/pause")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["paused"], true);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/service")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["state"], "paused");
    assert_eq!(v["paused"], true);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/service/resume")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["paused"], false);

    for expected in [true, false] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/service/toggle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["paused"], expected);
    }
}
