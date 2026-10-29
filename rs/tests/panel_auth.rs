//! 面板登录：首次升级设密、登录/登出、数据保留。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ticket_tracker::auth::AuthState;
use ticket_tracker::db::Db;
use ticket_tracker::server::{api, web, AppState};
use tower::ServiceExt;

fn app_with_auth(db: Db, auth: AuthState) -> axum::Router {
    let state: Arc<AppState> = AppState::new_with_auth(db, auth);
    api::router(state.clone()).merge(web::router(state))
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap()
}

fn post_form(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post_form_cookie(uri: &str, body: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn location(resp: &axum::response::Response) -> String {
    resp.headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn session_cookie(resp: &axum::response::Response) -> String {
    resp.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn body_text(resp: axum::response::Response) -> String {
    String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

async fn create_watch(app: &axum::Router) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/watches")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "movie_id": 42, "movie_name": "Auth 数据",
                        "cinemas": ["2"], "time": "all"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn first_run_setup_login_logout_and_data_survive() {
    let db = Db::connect_memory().await.unwrap();
    let app = app_with_auth(db.clone(), AuthState::new(None, vec![7u8; 32], false));

    // 已有安装升级后：面板跳转到 /setup，API 与静态资源仍公开。
    let resp = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/setup");

    let resp = app.clone().oneshot(get("/setup")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_text(resp).await.contains("设置面板密码"));

    let resp = app
        .clone()
        .oneshot(get("/static/css/app.css"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app.clone().oneshot(get("/api/v1/health")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 设密前先创建的数据，设密后仍然在。
    create_watch(&app).await;

    // 太短的密码被拒绝。
    let resp = app
        .clone()
        .oneshot(post_form("/setup", "password=short&confirm=short"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 两次不一致被拒绝。
    let resp = app
        .clone()
        .oneshot(post_form(
            "/setup",
            "password=password123&confirm=password124",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 正常设密，直接登录。
    let resp = app
        .clone()
        .oneshot(post_form(
            "/setup",
            "password=password123&confirm=password123",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/");
    let cookie = session_cookie(&resp);

    let resp = app
        .clone()
        .oneshot(get_with_cookie("/", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("Auth 数据"));
    assert!(html.contains("退出"));

    // 已设密后 /setup 会转到登录页。
    let resp = app.clone().oneshot(get("/setup")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/login");

    // 篡改 cookie 无效。
    let resp = app
        .clone()
        .oneshot(get_with_cookie("/", &format!("{cookie}x")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/login");

    // 登录页和错误密码。
    let resp = app.clone().oneshot(get("/login")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_text(resp).await.contains("登录面板"));

    let resp = app
        .clone()
        .oneshot(post_form("/login", "password=wrong-password"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(post_form("/login", "password=password123"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/");
    let cookie = session_cookie(&resp);

    // 登出后再访问会回到登录页。
    let resp = app
        .clone()
        .oneshot(post_form_cookie("/logout", "", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/login");
    assert!(resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));

    let resp = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), "/login");

    // 数据没有丢。
    let resp = app.clone().oneshot(get("/api/v1/watches")).await.unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["watches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["movie_name"] == "Auth 数据"));

    // 密码和 session secret 只写进现有 settings 表。
    let map = db.settings_map().await.unwrap();
    assert!(map.contains_key(ticket_tracker::auth::ADMIN_HASH_KEY));
    assert!(map.contains_key(ticket_tracker::auth::SESSION_SECRET_KEY));
}

#[tokio::test]
async fn session_and_password_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.db");
    let path_s = path.to_string_lossy().to_string();

    let db = Db::connect(&path_s).await.unwrap();
    let auth = AuthState::load(&db).await.unwrap();
    assert!(!auth.has_admin_password());
    let app = app_with_auth(db, auth);

    let resp = app
        .clone()
        .oneshot(post_form(
            "/setup",
            "password=restart-pass&confirm=restart-pass",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&resp);
    drop(app);

    // 重新打开同一个数据库，密码和 session secret 都还在。
    let db2 = Db::connect(&path_s).await.unwrap();
    let auth2 = AuthState::load(&db2).await.unwrap();
    assert!(auth2.has_admin_password());
    assert!(auth2.verify_admin_password("restart-pass".into()).await);
    assert!(!auth2.verify_admin_password("wrong".into()).await);

    let app2 = app_with_auth(db2, auth2);
    let resp = app2
        .clone()
        .oneshot(get_with_cookie("/", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
