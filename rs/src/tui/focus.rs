//! 焦点管理。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Watches,
    Detail,
    Events,
    Actions,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Focus::Watches => Focus::Detail,
            Focus::Detail => Focus::Events,
            Focus::Events => Focus::Actions,
            Focus::Actions => Focus::Watches,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Focus::Watches => Focus::Actions,
            Focus::Actions => Focus::Events,
            Focus::Events => Focus::Detail,
            Focus::Detail => Focus::Watches,
        }
    }
    /// ← 键：Watches ← 已无；Detail ← Watches；Events ← Detail；Actions 单独
    pub fn left(self) -> Self {
        match self {
            Focus::Detail => Focus::Watches,
            Focus::Events => Focus::Detail,
            other => other,
        }
    }
    /// → 键
    pub fn right(self) -> Self {
        match self {
            Focus::Watches => Focus::Detail,
            Focus::Detail => Focus::Events,
            other => other,
        }
    }
}
