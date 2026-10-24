//! `gc-grep` —— 仓库内容搜索域。
//!
//! 当前阶段：把 [`gc_core::api`] 中供 grep 使用的文件树 / 下载 API
//! 以本 crate 命名空间重新导出（`repo grep` 的 ripgrep 搜索与 SHA
//! 稀疏缓存逻辑在 gc-cli，后续迁入本 crate）。

pub use gc_core::api::{download_files, download_tree, repo_file_content, repo_file_list, WebFile};
