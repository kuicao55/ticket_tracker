//! ticket-tracker —— v4 无头主服务库入口。
//!
//! 只保留服务端需要的模块：
//! - [`db`]  —— SQLite 数据层
//! - [`engine`] —— 单轮询引擎：影院并集抓取 + 纯内存分发
//! - [`maoyan`] —— 猫眼 HTTP 客户端
//! - [`notify`] —— Discord / webhook / 邮件（本机通知由 Gofer 负责）
//! - [`lock`] —— 锁票结果类型（实际执行在 Gofer）
//! - [`server`] —— axum API + Web 面板 + DB-backed monitor
//! - [`silent`] / [`time_spec`] / [`paths`] / [`presets`] —— 共享工具

pub mod db;
pub mod engine;
pub mod lock;
pub mod maoyan;
pub mod notify;
pub mod paths;
pub mod presets;
pub mod server;
pub mod silent;
pub mod time_spec;
pub mod update;

pub use paths::{log_file, pid_file, state_dir};
pub use presets::{get_preset, list_presets, PRESETS};
