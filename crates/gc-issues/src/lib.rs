//! `gc-issues` —— Issue 域。
//!
//! 当前阶段：把 [`gc_core::api`] 中的 Issue 网页端 v2 API 以本 crate
//! 命名空间重新导出，作为拆分的稳定入口；CLI 分发后续逐步迁入。

pub use gc_core::api::{
    issue_attachment_upload, issue_close, issue_create, issue_edit, issue_list, issue_reopen,
    issue_reply, issue_view,
};
