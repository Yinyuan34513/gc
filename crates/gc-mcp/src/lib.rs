//! `gc-mcp` —— gc 的 MCP 服务端 crate。
//!
//! 承载原 `src/mcp` 模块（stdio 上的换行分隔 JSON-RPC 2.0、工具注册表、
//! 串行/并发执行、取消与进度），并把传输/协议层逐步迁移到官方
//! [`rmcp`](https://crates.io/crates/rmcp) SDK：
//!
//! - [`mcp`]：现有实现（`gc mcp start` 使用的稳定路径）
//! - [`rmcp_server`]：基于 rmcp `ServerHandler` 的桥接实现（同一份工具注册表）

// mcp 模块内的 handler 直接调用 api / config 层，挂到 crate 根保持
// `crate::api` 等路径不变；ctx 助手以 `crate::cli` 别名暴露，
// 保持 tools.rs 原有 `crate::cli::resolve_repo` 等调用点不动。
pub use gc_core::{api, auth, config};
pub use gc_core::ctx as cli;

pub mod mcp;
pub mod rmcp_server;
