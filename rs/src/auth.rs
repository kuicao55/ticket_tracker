//! 面板密码登录：Argon2 密码哈希 + HMAC 签名会话 Cookie。
//!
//! 与 Gofer 的面板认证保持一致，但只保护 Web 面板：
//! - `/api/v1/*`、`/health`、`/static/*` 保持公开；
//! - 首次升级后没有密码哈希时，面板会跳到 `/setup` 创建密码；
//! - 密码哈希和 session secret 写入现有 `settings` 表的新 key，
//!   不新增表、不改旧数据；
//! - 支持 `TT_ADMIN_PASSWORD` 重置密码（>= 8 字符）。

use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use argon2::password_hash::rand_core::OsRng as ArgonOsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

use crate::db::Db;
use crate::server::AppState;

type HmacSha256 = Hmac<Sha256>;

pub const SESSION_COOKIE: &str = "tt_session";
pub const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const ADMIN_HASH_KEY: &str = "panel.password_hash";
pub const SESSION_SECRET_KEY: &str = "panel.session_secret";
pub const ADMIN_PASSWORD_ENV: &str = "TT_ADMIN_PASSWORD";
pub const COOKIE_SECURE_ENV: &str = "TT_COOKIE_SECURE";

pub struct AuthState {
    admin_hash: RwLock<Option<String>>,
    session_secret: RwLock<Vec<u8>>,
    cookie_secure: bool,
    /// Serializes first-run setup so only one request can create the password.
    pub setup_lock: tokio::sync::Mutex<()>,
}

impl AuthState {
    pub fn new(admin_hash: Option<String>, session_secret: Vec<u8>, cookie_secure: bool) -> Self {
        Self {
            admin_hash: RwLock::new(admin_hash),
            session_secret: RwLock::new(session_secret),
            cookie_secure,
            setup_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 从 SQLite 加载密码哈希与 session secret；缺失时生成 secret。
    ///
    /// `TT_ADMIN_PASSWORD` 是可选的密码重置入口：与库里不同则重新哈希并
    /// 轮换 session secret（让旧会话失效）。
    pub async fn load(db: &Db) -> Result<Self> {
        let env_password = std::env::var(ADMIN_PASSWORD_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        if let Some(pw) = env_password.as_deref() {
            if pw.chars().count() < 8 {
                return Err(anyhow!("TT_ADMIN_PASSWORD 至少需要 8 个字符"));
            }
        }

        let mut admin_hash = db.get_setting(ADMIN_HASH_KEY).await?;
        if let Some(hash) = admin_hash.as_deref() {
            if PasswordHash::new(hash).is_err() {
                tracing::error!("面板密码哈希损坏；请设置 TT_ADMIN_PASSWORD 后重启以重置");
            }
        }

        let mut rotate_sessions = false;
        if let Some(pw) = env_password {
            let matches = match admin_hash.as_deref() {
                Some(hash) => verify_password_async(pw.clone(), hash.to_string())
                    .await
                    .unwrap_or(false),
                None => false,
            };
            if !matches {
                let hash = hash_password_async(pw).await?;
                db.set_setting(ADMIN_HASH_KEY, &hash).await?;
                admin_hash = Some(hash);
                rotate_sessions = true;
                tracing::info!("已从 TT_ADMIN_PASSWORD 设置面板密码");
            }
        }

        let valid_secret = db
            .get_setting(SESSION_SECRET_KEY)
            .await?
            .as_deref()
            .and_then(decode_hex)
            .filter(|b| b.len() == 32);
        let secret = match (rotate_sessions, valid_secret) {
            (false, Some(secret)) => secret,
            _ => {
                let secret = random_bytes(32);
                db.set_setting(SESSION_SECRET_KEY, &hex_encode(&secret))
                    .await?;
                secret
            }
        };

        Ok(Self::new(admin_hash, secret, cookie_secure_from_env()))
    }

    pub fn has_admin_password(&self) -> bool {
        self.admin_hash
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .is_some_and(|h| !h.trim().is_empty())
    }

    pub fn set_admin_hash(&self, hash: Option<String>) {
        *self.admin_hash.write().unwrap_or_else(|e| e.into_inner()) = hash;
    }

    pub fn rotate_session_secret(&self) -> Vec<u8> {
        let secret = random_bytes(32);
        *self
            .session_secret
            .write()
            .unwrap_or_else(|e| e.into_inner()) = secret.clone();
        secret
    }

    pub fn session_secret_hex(&self) -> String {
        hex_encode(
            &self
                .session_secret
                .read()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    pub async fn verify_admin_password(&self, password: String) -> bool {
        let Some(hash) = self
            .admin_hash
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return false;
        };
        verify_password_async(password, hash).await.unwrap_or(false)
    }

    pub fn issue_session(&self) -> String {
        self.issue_session_at(now_secs())
    }

    fn issue_session_at(&self, issued_at: u64) -> String {
        let expires_at = issued_at.saturating_add(SESSION_TTL_SECS);
        let nonce = random_hex(16);
        let payload = format!("v1.{issued_at}.{expires_at}.{nonce}");
        let signature = sign(&self.session_secret(), payload.as_bytes());
        format!("{payload}.{signature}")
    }

    pub fn valid_session(&self, value: &str) -> bool {
        self.valid_session_at(value, now_secs())
    }

    fn valid_session_at(&self, value: &str, now: u64) -> bool {
        let Some((payload, signature)) = value.rsplit_once('.') else {
            return false;
        };
        let parts: Vec<&str> = payload.split('.').collect();
        if parts.len() != 4 || parts[0] != "v1" {
            return false;
        }
        let Ok(issued_at) = parts[1].parse::<u64>() else {
            return false;
        };
        let Ok(expires_at) = parts[2].parse::<u64>() else {
            return false;
        };
        if expires_at <= now || issued_at > now.saturating_add(300) {
            return false;
        }
        let Some(signature) = decode_hex(signature) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(&self.session_secret())
            .expect("hmac accepts arbitrary key lengths");
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature).is_ok()
    }

    fn session_secret(&self) -> Vec<u8> {
        self.session_secret
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

fn cookie_secure_from_env() -> bool {
    if std::env::var(COOKIE_SECURE_ENV)
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    std::env::var("TT_PUBLIC_URL")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase().starts_with("https://"))
        .unwrap_or(false)
}

pub async fn hash_password_async(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| anyhow!("密码哈希任务失败: {e}"))?
}

pub async fn verify_password_async(password: String, phc: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || verify_password(&password, &phc))
        .await
        .map_err(|e| anyhow!("密码校验任务失败: {e}"))?
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut ArgonOsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("Argon2 哈希失败: {e}"))
}

fn verify_password(password: &str, phc: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc).map_err(|e| anyhow!("无效的密码哈希: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn session_from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == SESSION_COOKIE).then(|| value.to_string())
    })
}

pub fn session_cookie(value: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}{secure}"
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}")
}

/// Cookie 认证 POST 的基础 CSRF 防护；SameSite=Strict 是主要防线。
pub fn same_origin_ok(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }

    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if !authority_matches(origin, host) {
            return false;
        }
    }
    if let Some(referer) = headers.get(header::REFERER).and_then(|v| v.to_str().ok()) {
        if !authority_matches(referer, host) {
            return false;
        }
    }
    true
}

fn authority_matches(url: &str, host: &str) -> bool {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    authority.eq_ignore_ascii_case(host)
}

/// 保护整个 Web 面板。`AppState.auth == None` 时放行（库/测试的看守关闭模式）。
pub async fn require_panel(
    State(st): State<std::sync::Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(auth) = st.auth.as_ref() else {
        return next.run(req).await;
    };
    let authenticated =
        session_from_headers(req.headers()).is_some_and(|value| auth.valid_session(&value));
    if !authenticated {
        return auth_redirect(auth, req.headers());
    }

    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn auth_redirect(auth: &AuthState, headers: &HeaderMap) -> Response {
    let target = if auth.has_admin_password() {
        "/login"
    } else {
        "/setup"
    };
    if headers.contains_key("hx-request") {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        if let Ok(value) = HeaderValue::from_str(target) {
            response
                .headers_mut()
                .insert(HeaderName::from_static("hx-redirect"), value);
        }
        return response;
    }
    Redirect::to(target).into_response()
}

pub fn random_hex(bytes: usize) -> String {
    hex_encode(&random_bytes(bytes))
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() & 1 != 0 {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect()
}

fn sign(secret: &[u8], payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts arbitrary key lengths");
    mac.update(payload);
    hex_encode(&mac.finalize().into_bytes())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> AuthState {
        AuthState::new(None, vec![7u8; 32], false)
    }

    #[test]
    fn session_roundtrip_and_tampering() {
        let auth = auth();
        let now = 1_700_000_000;
        let value = auth.issue_session_at(now);
        assert!(auth.valid_session_at(&value, now + 1));
        assert!(!auth.valid_session_at(&value, now + SESSION_TTL_SECS));
        assert!(!auth.valid_session_at(&format!("{value}x"), now + 1));
        assert!(!auth.valid_session_at("v1.1.2.abc", now + 1));

        let other = AuthState::new(None, vec![8u8; 32], false);
        assert!(!other.valid_session_at(&value, now + 1));
    }

    #[test]
    fn session_rejects_future_issue_time() {
        let auth = auth();
        let now = 1_700_000_000;
        let value = auth.issue_session_at(now + 301);
        assert!(!auth.valid_session_at(&value, now));
    }

    #[test]
    fn cookie_parsing_and_attributes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; tt_session=abc.def; b=2"),
        );
        assert_eq!(session_from_headers(&headers).as_deref(), Some("abc.def"));
        let cookie = session_cookie("abc", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(clear_session_cookie(false).contains("Max-Age=0"));
    }

    #[test]
    fn same_origin_checks() {
        let mut headers = HeaderMap::new();
        assert!(same_origin_ok(&headers));
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8787"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8787"),
        );
        assert!(same_origin_ok(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );
        assert!(!same_origin_ok(&headers));
    }

    #[tokio::test]
    async fn password_hash_roundtrip() {
        let hash = hash_password_async("correct horse".into()).await.unwrap();
        assert!(verify_password_async("correct horse".into(), hash.clone())
            .await
            .unwrap());
        assert!(!verify_password_async("wrong".into(), hash).await.unwrap());
    }
}
