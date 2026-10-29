//! 编译期嵌入的 Web 静态资源。
//!
//! Release 只发布 `tt-server` 一个二进制，因此 `/static` 依赖的 CSS / JS
//! 必须编进二进制。启动时释放到：
//!
//! ```text
//! ~/.local/state/ticket-tracker/runtime/v<version>/web
//! ```
//!
//! 释放是幂等的：内容相同的文件不会重写。显式设置 `TT_WEB_DIR` 时保留
//! 外部目录（开发时热改 CSS/JS），二进制不再释放/覆盖。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{include_dir, Dir, DirEntry};

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web");

/// 递归收集所有嵌入文件（`Dir::files()` 只返回当前层级）。
fn collect_files<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::File(file) => out.push(file),
            DirEntry::Dir(sub) => collect_files(sub, out),
        }
    }
}

fn web_files() -> Vec<&'static include_dir::File<'static>> {
    let mut out = Vec::new();
    collect_files(&WEB, &mut out);
    out
}

/// 嵌入的静态资源数量（测试 / 诊断用）。
pub fn embedded_file_count() -> usize {
    web_files().len()
}

/// 释放内置资源并返回实际使用的 web 目录。
///
/// 用户显式设置 `TT_WEB_DIR` 时直接返回该目录；否则写入
/// `<state_dir>/runtime/v<version>/web` 并把 `TT_WEB_DIR` 指向它，
/// 这样现有 `ServeDir` 代码无需改动。
pub fn materialize() -> Result<PathBuf> {
    if let Some(dir) = explicit_web_dir() {
        return Ok(dir);
    }

    let web = runtime_web_dir();
    materialize_into(&web)?;
    // 进程内单线程启动阶段设置；ServeDir 随后读取。
    std::env::set_var("TT_WEB_DIR", &web);
    tracing::debug!("内置 Web 资源已释放到 {}", web.display());
    Ok(web)
}

/// 释放到指定 web 目录，返回实际写入的文件数（内容相同则跳过）。
pub fn materialize_into(web: &Path) -> Result<usize> {
    let mut written = 0usize;
    for file in web_files() {
        let path = web.join(file.path());
        let bytes = file.contents();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 Web 资源目录失败: {}", parent.display()))?;
        }
        let needs_write = match std::fs::read(&path) {
            Ok(existing) => existing != bytes,
            Err(_) => true,
        };
        if needs_write {
            std::fs::write(&path, bytes)
                .with_context(|| format!("写入 Web 资源失败: {}", path.display()))?;
            written += 1;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
    }
    Ok(written)
}

/// `<state_dir>/runtime/v<version>/web`。
pub fn runtime_web_dir() -> PathBuf {
    crate::paths::state_dir()
        .join("runtime")
        .join(format!("v{}", env!("CARGO_PKG_VERSION")))
        .join("web")
}

fn explicit_web_dir() -> Option<PathBuf> {
    std::env::var("TT_WEB_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded(name: &str) -> &'static [u8] {
        WEB.get_file(name)
            .unwrap_or_else(|| panic!("embedded asset missing: {name}"))
            .contents()
    }

    #[test]
    fn embedded_web_assets_are_not_empty() {
        let count = embedded_file_count();
        assert!(count >= 5, "expected bundled web assets, got {count}");
        for name in ["css/app.css", "js/app.js", "js/htmx.min.js", "favicon.svg"] {
            assert!(!embedded(name).is_empty(), "{name} is empty");
        }
    }

    #[test]
    fn materialize_writes_files_matching_source_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let written = materialize_into(dir.path()).unwrap();
        assert_eq!(written, embedded_file_count());

        for name in ["css/app.css", "js/app.js", "favicon.svg"] {
            let out = std::fs::read(dir.path().join(name)).unwrap();
            let source =
                std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join(name))
                    .unwrap();
            assert_eq!(out, embedded(name), "{name} differs from embedded bytes");
            assert_eq!(out, source, "{name} differs from source file");
        }

        // 第二次释放不重写任何文件。
        assert_eq!(materialize_into(dir.path()).unwrap(), 0);
    }
}
