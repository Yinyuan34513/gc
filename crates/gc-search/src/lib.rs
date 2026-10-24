//! `gc-search` —— 全站搜索域。
//!
//! 当前阶段：把 [`gc_core::api`] 中的网页端 v1 搜索 API 以本 crate
//! 命名空间重新导出，作为拆分的稳定入口；CLI 分发后续逐步迁入。

pub use gc_core::api::{search_digest, web_search};
