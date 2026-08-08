//! `gc` —— GitCode (AtomGit) 命令行工具。
//!
//! 以 lib + bin 的形式组织：`main.rs` 只是薄壳，真正的实现都在这里，
//! 这样 [`mcp`] 模块（`gc mcp start`）可以直接复用 [`api`] 层，
//! 而不必去解析 CLI 的人类可读输出。

pub mod api;
pub mod cli;
pub mod config;
pub mod mcp;
