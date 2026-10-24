//! `gc-cli` —— gc 的命令行层。
//!
//! 承载全部 clap 参数定义、子命令分发与人类可读输出（列表打印、
//! `--json`/`--jq` 结构化输出、termimad Markdown 渲染）。
//!
//! 分域 crate（gc-repo / gc-pr / gc-issues / gc-grep / gc-search / gc-skill）
//! 提供各域的 API 层；`gc-core` 提供客户端、认证与配置等基础设施。

// 把 gc-core 的模块原样挂到本 crate 根下，cli.rs 里的 `crate::api` /
// `crate::config` 等路径无需改动。
pub use gc_core::{api, auth, config, ctx, output};

// MCP 服务端由独立 crate 提供，cli.rs 以 `crate::mcp` 引用。
pub use gc_mcp::mcp;

pub mod cli;
