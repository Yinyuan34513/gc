//! `gc-repo` —— 仓库域。
//!
//! 当前阶段：把 [`gc_core::api`] 中的仓库相关 API 以本 crate 命名空间
//! 重新导出，作为拆分的稳定入口；CLI 分发后续逐步迁入。

pub use gc_core::api::{
    fork_repo, repo_compare, repo_file_content, repo_file_list, star_repo, sync_repo,
    sync_repo_status, unstar_repo, update_project_v2, wiki_create, WebFile,
};

pub use gc_core::api;
pub mod cli;
