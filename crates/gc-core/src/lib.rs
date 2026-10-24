//! `gc-core` —— gc 的公共层。
//!
//! 承载三大基础设施，供所有域 crate（gc-repo / gc-pr / gc-issues / gc-grep /
//! gc-search）与 `gc-cli`、`gc-mcp` 复用：
//!
//! - [`api`]：v5 开放 API 客户端（`ApiClient`）与网页端 v1/v2 接口封装
//! - [`auth`]：登录（浏览器抓取 / 粘贴 token）
//! - [`config`]：`~/.config/gitcode.toml` 的读写与 PAT / 网页端 JWT 管理

pub mod api;
pub mod auth;
pub mod config;
pub mod ctx;
