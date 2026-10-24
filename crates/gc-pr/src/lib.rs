//! `gc-pr` —— PR 域。
//!
//! 当前阶段：把 [`gc_core::api`] 中的 PR 网页端 API 以本 crate 命名空间
//! 重新导出，作为拆分的稳定入口；CLI 分发后续逐步迁入。

pub use gc_core::api::{pr_approval, pr_cherry_pick, pr_list_count, pr_list_web, pr_note_create, pr_revert};
