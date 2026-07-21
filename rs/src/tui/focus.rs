//! 焦点管理。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Watches,
    Detail,
    Events,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Focus::Watches => Focus::Detail,
            Focus::Detail => Focus::Events,
            Focus::Events => Focus::Watches,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Focus::Watches => Focus::Events,
            Focus::Detail => Focus::Watches,
            Focus::Events => Focus::Detail,
        }
    }
    pub fn left(self) -> Self {
        match self {
            Focus::Events => Focus::Detail,
            Focus::Detail => Focus::Watches,
            other => other,
        }
    }
    pub fn right(self) -> Self {
        match self {
            Focus::Watches => Focus::Detail,
            Focus::Detail => Focus::Events,
            other => other,
        }
    }
}
