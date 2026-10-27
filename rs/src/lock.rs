//! 锁票结果类型。实际执行在独立 Gofer 服务中；主服务只负责接收结果回调。

/// booker 真锁成功后，用户需要在猫眼 App 限时支付的时间（分钟）。
pub const PAY_WINDOW_MINUTES: u64 = 15;

/// Gofer/agent 回调的锁票结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockResult {
    /// 真锁成功（15 分钟内去 App 付款）
    Ok,
    /// dry-run：未真锁
    DryRun,
    /// 场次被抢光 / 时间错位
    ShowGone,
    /// 流程异常
    Error,
    /// 点击 / 加载超时
    Timeout,
    /// 其他未分类
    Unknown,
}

impl LockResult {
    /// 给通知/日志的短名。
    pub fn label(self) -> &'static str {
        match self {
            LockResult::Ok => "锁成功",
            LockResult::DryRun => "dry-run（未真锁）",
            LockResult::ShowGone => "场次没了",
            LockResult::Error => "流程异常",
            LockResult::Timeout => "超时",
            LockResult::Unknown => "未知",
        }
    }
}
