//! 扁平安装测试：只依赖 tt-server 二进制本身。
//!
//! 不设置 `TT_WEB_DIR`，验证内置 Web 资产会在空 state 目录中释放，
//! 面板 `/` 和 `/static/css/app.css` 都能正常返回。

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn wait_for_health(client: &reqwest::Client, url: &str, child: &mut Child, log: &TempDir) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            let stdout = std::fs::read_to_string(log.path().join("stdout.log")).unwrap_or_default();
            let stderr = std::fs::read_to_string(log.path().join("stderr.log")).unwrap_or_default();
            panic!("tt-server exited early: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        }
        if let Ok(resp) = client.get(format!("{url}/health")).send().await {
            if resp.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("tt-server did not become healthy in 10s");
}

#[tokio::test]
async fn bundled_panel_works_without_repo_or_web_dir() {
    let state = TempDir::new().unwrap();
    let logs = TempDir::new().unwrap();
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");

    let stdout = std::fs::File::create(logs.path().join("stdout.log")).unwrap();
    let stderr = std::fs::File::create(logs.path().join("stderr.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_tt-server"))
        .env("TT_BIND", format!("127.0.0.1:{port}"))
        .env("TT_DB", state.path().join("ticket-tracker.db"))
        .env("XDG_STATE_HOME", state.path())
        .env("TT_PID_FILE", state.path().join("tt-server.pid"))
        .env("TT_LOG_FILE", state.path().join("tt-server.log"))
        .env("TT_NO_MONITOR", "true")
        .env_remove("TT_WEB_DIR")
        .env_remove("TT_SERVER_BIN")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let mut guard = KillOnDrop(child);

    let client = reqwest::Client::new();
    // 通过 guard 访问子进程做健康轮询；KillOnDrop 仍负责最终清理。
    wait_for_health(&client, &base, &mut guard.0, &logs).await;

    let panel = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(panel.status(), reqwest::StatusCode::OK);
    assert!(panel
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/html"));
    let html = panel.text().await.unwrap();
    assert!(
        html.contains("Quiet eyes"),
        "panel HTML missing dashboard marker"
    );

    let css = client
        .get(format!("{base}/static/css/app.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(css.status(), reqwest::StatusCode::OK);
    assert!(css
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/css"));
    let css_body = css.text().await.unwrap();
    assert!(css_body.len() > 1000, "css body too small");

    // 内置资源应释放到 <XDG_STATE_HOME>/ticket-tracker/runtime/v<version>/web。
    let runtime_web = state
        .path()
        .join("ticket-tracker")
        .join("runtime")
        .join(format!("v{}", env!("CARGO_PKG_VERSION")))
        .join("web");
    assert!(
        runtime_web.join("css/app.css").is_file(),
        "runtime web not materialized at {}",
        runtime_web.display()
    );
    assert_eq!(
        std::fs::read(runtime_web.join("css/app.css")).unwrap(),
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/css/app.css"))
            .unwrap()
    );
}
