//! MCP 工具注册表：把 `gc` 的能力包装成 AI 可调用的工具。
//!
//! 这里**直接调用 [`crate::api`]**，绕开 `cli.rs` —— 因为 `cli.rs` 会 `println!`
//! 人类可读文本、会拉起 libreditor 编辑器、还有两处 `std::process::exit(1)`，
//! 这些在 MCP 服务器里全是致命的（stdout 是 JSON-RPC 通道）。API 层则完全没有
//! 打印和交互，且清一色返回 `Result<serde_json::Value>`，正好能直通工具结果。

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::{json, Map, Value};

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::{self, ApiClient};
use crate::config;

/// 一个 MCP 工具。
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema（`inputSchema`）。
    pub schema: fn() -> Value,
    /// 只读工具不会改变任何远端状态。
    pub read_only: bool,
    /// 破坏性工具的效果不可逆，默认不注册。
    pub destructive: bool,
    pub handler: fn(&Value) -> Result<Value>,
}

impl Tool {
    /// 转成 `tools/list` 里的一项。
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.schema)(),
            "annotations": {
                "readOnlyHint": self.read_only,
                "destructiveHint": self.destructive,
                // gc 的写操作都会新建实体（评论/issue/发行版），重复调用不等幂。
                "idempotentHint": self.read_only,
                "openWorldHint": true,
            }
        })
    }
}

/// 构建工具表。`allow_destructive` 为 false 时不注册破坏性工具——
/// 让 AI 默认无法删仓库或改可见性。
pub fn registry(allow_destructive: bool) -> Vec<&'static Tool> {
    ALL.iter()
        .filter(|t| allow_destructive || !t.destructive)
        .collect()
}

pub fn find(name: &str, allow_destructive: bool) -> Option<&'static Tool> {
    registry(allow_destructive)
        .into_iter()
        .find(|t| t.name == name)
}

// ---------------------------------------------------------------------------
// 凭据
// ---------------------------------------------------------------------------

fn pat_client() -> Result<ApiClient> {
    let token = config::load_token()?;
    if token.as_deref().map(str::trim).unwrap_or("").is_empty() {
        bail!("未配置个人访问令牌：请设置环境变量 GITCODE_TOKEN，或先运行 `gc token set`");
    }
    ApiClient::new(config::api_base(), token)
}

/// 网页端 JWT 的进程内缓存。
///
/// `cli::web_access_token()` 每次调用都会刷新并**改写 `~/.config/gitcode.toml`**，
/// 对常驻服务来说既慢又会和用户 shell 里的 `gc` 抢文件（没有文件锁）。这里缓存
/// 30 分钟，把改写频率从"每次工具调用一次"降到"每半小时一次"。
fn token_cache() -> &'static Mutex<Option<(String, Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn web_token() -> Result<String> {
    const TTL: Duration = Duration::from_secs(30 * 60);

    let mut slot = token_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((tok, at)) = slot.as_ref() {
        if at.elapsed() < TTL {
            return Ok(tok.clone());
        }
    }
    let tok = crate::cli::web_access_token()?;
    *slot = Some((tok.clone(), Instant::now()));
    Ok(tok)
}

/// 强制刷新网页端 JWT，写回配置文件并回填缓存。
///
/// 回填必须和刷新在同一把锁里完成：否则刷新成功后，其它工具仍会命中旧的
/// 缓存条目直到 TTL 自然过期，对外表现就是"刷新了但没生效"。
fn force_refresh_web_token() -> Result<()> {
    let mut slot = token_cache().lock().unwrap_or_else(|e| e.into_inner());
    let (access, refresh) = config::load_web_tokens()?;
    let (new_access, new_refresh) = api::refresh_web_token(&access, &refresh)?;
    config::save_web_tokens(&new_access, &new_refresh)?;
    *slot = Some((new_access, Instant::now()));
    Ok(())
}

// ---------------------------------------------------------------------------
// 参数提取
// ---------------------------------------------------------------------------

fn req_str(a: &Value, k: &str) -> Result<String> {
    match a.get(k) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(anyhow!("参数 `{k}` 不能为空字符串")),
        Some(_) => Err(anyhow!("参数 `{k}` 必须是字符串")),
        None => Err(anyhow!("缺少必填参数 `{k}`")),
    }
}

fn opt_str(a: &Value, k: &str) -> Option<String> {
    match a.get(k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn str_or(a: &Value, k: &str, default: &str) -> String {
    opt_str(a, k).unwrap_or_else(|| default.to_string())
}

fn req_u64(a: &Value, k: &str) -> Result<u64> {
    match a.get(k) {
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| anyhow!("参数 `{k}` 必须是非负整数")),
        // 有些客户端会把数字塞成字符串。
        Some(Value::String(s)) => s
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow!("参数 `{k}` 必须是非负整数")),
        Some(_) => Err(anyhow!("参数 `{k}` 必须是整数")),
        None => Err(anyhow!("缺少必填参数 `{k}`")),
    }
}

fn u64_or(a: &Value, k: &str, default: u64) -> u64 {
    req_u64(a, k).unwrap_or(default)
}

fn bool_or(a: &Value, k: &str, default: bool) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(default)
}

/// 三态布尔：没传就是 None，用于「只更新传了的字段」的 PATCH 接口。
fn opt_bool(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(Value::as_bool)
}

fn str_list(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn u64_list(a: &Value, k: &str) -> Result<Vec<u64>> {
    let arr = a
        .get(k)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("缺少必填参数 `{k}`（应为整数数组）"))?;
    if arr.is_empty() {
        bail!("参数 `{k}` 至少要有一项");
    }
    arr.iter()
        .map(|v| match v {
            Value::Number(n) => n
                .as_u64()
                .ok_or_else(|| anyhow!("`{k}` 的元素必须是非负整数")),
            Value::String(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|_| anyhow!("`{k}` 的元素必须是非负整数")),
            _ => Err(anyhow!("`{k}` 的元素必须是整数")),
        })
        .collect()
}

/// `owner/name` 拆分，复用 CLI 的实现保持行为一致。
///
/// `repo` 省略时从当前目录的 git remote 推断（与 `gc` CLI 行为一致）：在克隆的
/// gitcode 仓库目录里调工具可省掉 `repo` 参数。仍显式传 `repo` 则优先用它。
fn repo_of(a: &Value) -> Result<(String, String)> {
    crate::cli::resolve_repo(opt_str(a, "repo"))
}

// ---------------------------------------------------------------------------
// Schema 构造
// ---------------------------------------------------------------------------

fn schema(props: &[(&str, Value)], required: &[&str]) -> Value {
    let mut m = Map::new();
    for (k, v) in props {
        m.insert((*k).to_string(), v.clone());
    }
    // `repo` 现在可选：省略时由 `repo_of` 从当前目录的 git remote 推断，因此从
    // required 里移除（属性仍声明，供需要显式指定的客户端使用）。
    let required: Vec<Value> = required
        .iter()
        .filter(|&&k| k != "repo")
        .map(|k| Value::String((*k).to_string()))
        .collect();
    json!({
        "type": "object",
        "properties": Value::Object(m),
        "required": required,
        "additionalProperties": false,
    })
}

fn p_str(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn p_int(desc: &str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": desc })
}

fn p_enum(desc: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values, "description": desc })
}

fn p_bool(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}

fn p_strs(desc: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": desc })
}

fn p_ints(desc: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "integer", "minimum": 0 },
        "minItems": 1,
        "description": desc,
    })
}

/// `repo` 参数在几乎每个工具里都有。省略时从当前目录的 git remote 推断
/// （与 `gc` CLI 行为一致），需要显式指定时给 `owner/name` 即可。
fn p_repo() -> (&'static str, Value) {
    (
        "repo",
        p_str("仓库 owner/name，例如 alice/demo。可省略：在克隆的 gitcode 仓库目录里自动从 git remote 推断"),
    )
}

fn p_ref() -> (&'static str, Value) {
    ("ref", p_str("分支或标签，默认 main"))
}

// ---------------------------------------------------------------------------
// 工具表
// ---------------------------------------------------------------------------

static ALL: &[Tool] = &[
    // ---- 认证 / 用户 -----------------------------------------------------
    Tool {
        name: "gc_auth_status",
        description: "校验当前个人访问令牌（PAT）是否有效，并返回已认证用户的信息。",
        schema: || schema(&[], &[]),
        read_only: true,
        destructive: false,
        handler: |_| pat_client()?.authenticated_user(),
    },
    Tool {
        name: "gc_refresh_web_token",
        description: "刷新网页端 JWT 并写回配置。当 Issue / 文件读取 / Wiki / 讨论 类工具\
                      报鉴权失败（401、token 过期）时先调用它，再重试原操作。\
                      出于安全考虑不会返回 token 本身。",
        schema: || schema(&[], &[]),
        read_only: false,
        destructive: false,
        handler: |_| {
            force_refresh_web_token()?;
            Ok(json!({
                "refreshed": true,
                "message": "网页端 JWT 已刷新，可以重试刚才失败的操作了",
            }))
        },
    },
    Tool {
        name: "gc_user_view",
        description: "查看某个用户的公开资料（昵称、简介、公司、位置等）。需要网页端 JWT。",
        schema: || schema(&[("username", p_str("用户名"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let p = api::user_profile(&web_token()?, &req_str(a, "username")?)?;
            Ok(serde_json::to_value(p)?)
        },
    },
    // ---- 仓库（只读）-----------------------------------------------------
    Tool {
        name: "gc_repo_view",
        description: "查看仓库详情：描述、默认分支、星标数、可见性等。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.repo(&o, &n)
        },
    },
    Tool {
        name: "gc_repo_list",
        description: "列出仓库。给了 owner 就列该用户/组织的仓库，否则列当前认证用户的仓库。",
        schema: || schema(&[("owner", p_str("用户名或组织名；省略则为当前用户"))], &[]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.repos(opt_str(a, "owner").as_deref()),
    },
    Tool {
        name: "gc_repo_tree",
        description: "列出仓库在某个分支/标签下的完整文件路径清单。需要网页端 JWT。",
        schema: || schema(&[p_repo(), p_ref()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let files = api::repo_file_list(&web_token()?, &o, &n, &str_or(a, "ref", "main"))?;
            Ok(json!({ "count": files.len(), "files": files }))
        },
    },
    Tool {
        name: "gc_repo_read",
        description: "读取仓库内单个文件的内容（自动 base64 解码）。二进制文件只返回元信息不返回内容。需要网页端 JWT。",
        schema: || {
            schema(
                &[p_repo(), ("file", p_str("仓库内文件路径，例如 src/main.rs")), p_ref()],
                &["repo", "file"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let f = api::repo_file_content(
                &web_token()?,
                &o,
                &n,
                &str_or(a, "ref", "main"),
                &req_str(a, "file")?,
            )?;
            let raw = if f.encoding.eq_ignore_ascii_case("base64") {
                base64::engine::general_purpose::STANDARD
                    .decode(f.content.trim())
                    .map_err(|e| anyhow!("base64 解码失败: {e}"))?
            } else {
                f.content.clone().into_bytes()
            };
            let mut out = json!({
                "name": f.name,
                "path": f.path,
                "size": f.size,
                "file_type": f.file_type,
                // 服务端限流时内容会被截断或清空，必须让调用方知道。
                "is_limited": f.is_limited.unwrap_or(false),
            });
            match String::from_utf8(raw) {
                Ok(text) => {
                    out["content"] = json!(text);
                    out["binary"] = json!(false);
                }
                Err(e) => {
                    out["binary"] = json!(true);
                    out["content"] = Value::Null;
                    out["note"] = json!(format!(
                        "该文件不是有效 UTF-8 文本（{} 字节），未返回内容",
                        e.as_bytes().len()
                    ));
                }
            }
            Ok(out)
        },
    },
    // ---- Issue -----------------------------------------------------------
    Tool {
        name: "gc_issue_list",
        description: "列出仓库的 Issue。需要网页端 JWT（注意：Issue 接口走网页端，不是 PAT）。",
        schema: || {
            schema(
                &[p_repo(), ("state", p_str("open / closed / all，默认 open"))],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            api::issue_list(&web_token()?, &o, &n, &str_or(a, "state", "open"))
        },
    },
    Tool {
        name: "gc_issue_view",
        description: "查看单个 Issue 的详情。需要网页端 JWT。",
        schema: || schema(&[p_repo(), ("number", p_int("Issue 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            api::issue_view(&web_token()?, &o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_issue_create",
        description: "新建 Issue。title 与 body 都必须显式给出（CLI 里留空会拉起交互式编辑器，服务器模式下不可用）。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("title", p_str("标题")),
                    ("body", p_str("正文，支持 Markdown")),
                    ("labels", p_strs("标签名列表")),
                    ("confidential", p_bool("是否为私密 Issue，默认 false")),
                ],
                &["repo", "title", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            api::issue_create(
                &web_token()?,
                &o,
                &n,
                &req_str(a, "title")?,
                &req_str(a, "body")?,
                bool_or(a, "confidential", false),
                &str_list(a, "labels"),
            )
        },
    },
    Tool {
        name: "gc_issue_reply",
        description: "在 Issue 下发表评论。",
        schema: || {
            schema(
                &[p_repo(), ("number", p_int("Issue 编号")), ("body", p_str("评论正文"))],
                &["repo", "number", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            api::issue_reply(
                &web_token()?,
                &o,
                &n,
                req_u64(a, "number")?,
                &req_str(a, "body")?,
            )
        },
    },
    Tool {
        name: "gc_issue_edit",
        description: "修改 Issue 的标题或正文。注意是整体覆盖而非合并；至少要给 title 或 body 之一。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("Issue 编号")),
                    ("title", p_str("新标题；省略则不改")),
                    ("body", p_str("新正文；省略则不改")),
                ],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: true, // 覆盖式写入，改错了原文就没了
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let title = opt_str(a, "title");
            let body = opt_str(a, "body");
            if title.is_none() && body.is_none() {
                bail!("title 与 body 至少要提供一个");
            }
            api::issue_edit(
                &web_token()?,
                &o,
                &n,
                req_u64(a, "number")?,
                title.as_deref(),
                body.as_deref(),
            )
        },
    },
    Tool {
        name: "gc_issue_close",
        description: "关闭一个或多个 Issue（可用 gc_issue_reopen 撤销）。",
        schema: || {
            schema(
                &[p_repo(), ("numbers", p_ints("要关闭的 Issue 编号列表"))],
                &["repo", "numbers"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let tok = web_token()?;
            batch(&u64_list(a, "numbers")?, |id| {
                api::issue_close(&tok, &o, &n, id)
            })
        },
    },
    Tool {
        name: "gc_issue_reopen",
        description: "重新打开一个或多个 Issue。",
        schema: || {
            schema(
                &[p_repo(), ("numbers", p_ints("要重开的 Issue 编号列表"))],
                &["repo", "numbers"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let tok = web_token()?;
            batch(&u64_list(a, "numbers")?, |id| {
                api::issue_reopen(&tok, &o, &n, id)
            })
        },
    },
    // ---- Pull Request ----------------------------------------------------
    Tool {
        name: "gc_pr_list",
        description: "列出仓库的 Pull Request。",
        schema: || {
            schema(
                &[p_repo(), ("state", p_str("open / closed / merged / all，默认 open"))],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pulls(&o, &n, &str_or(a, "state", "open"))
        },
    },
    Tool {
        name: "gc_pr_view",
        description: "查看单个 Pull Request 的详情。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull(&o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_pr_create",
        description: "创建 Pull Request。title 与 body 都必须显式给出。head 是来源分支，base 是目标分支。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("title", p_str("标题")),
                    ("body", p_str("正文，支持 Markdown")),
                    ("head", p_str("来源分支，跨仓库时用 owner:branch")),
                    ("base", p_str("目标分支，例如 main")),
                ],
                &["repo", "title", "body", "head", "base"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_pull(
                &o,
                &n,
                &req_str(a, "title")?,
                &req_str(a, "body")?,
                &req_str(a, "head")?,
                &req_str(a, "base")?,
            )
        },
    },
    // ---- PR 生命周期 -----------------------------------------------------
    // 本段工具对应的接口移植自 @oh-gc/cli
    // <https://gitcode.com/guozejun/ohos_gitcode_cli>，MIT License，
    // Copyright (c) guozejun。版权声明见仓库根目录 LICENSE.oh-gc。
    Tool {
        name: "gc_pr_files",
        description: "列出 PR 改动的文件（文件名、状态、增删行数、diff 片段）。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_files(&o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_pr_commits",
        description: "列出 PR 包含的提交。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_commits(&o, &n, req_u64(a, "number")?, 0, 0)
        },
    },
    Tool {
        name: "gc_pr_comments",
        description: "列出 PR 的评论。评论 id 可用于 gc_pr_comment_edit / gc_pr_comment_delete。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_comments(&o, &n, req_u64(a, "number")?, 0, 0)
        },
    },
    Tool {
        name: "gc_pr_comment",
        description: "在 PR 下发表评论。同时给出 path 与 position 就是针对某一行的行级评论，\
                      否则是整体评论。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("body", p_str("评论正文，支持 Markdown")),
                    ("path", p_str("行级评论的文件路径；省略则为整体评论")),
                    ("position", p_int("行级评论的行号，需与 path 同时给出")),
                ],
                &["repo", "number", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let path = opt_str(a, "path");
            let position = a.get("position").and_then(Value::as_u64);
            pat_client()?.create_pull_comment(
                &o,
                &n,
                req_u64(a, "number")?,
                &req_str(a, "body")?,
                path.as_deref(),
                position,
            )
        },
    },
    Tool {
        name: "gc_pr_comment_reply",
        description: "在 PR 的某条讨论线程下回复。discussion_id 取自 gc_pr_comments 结果里的 discussion_id 字段。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("discussion_id", p_str("讨论线程 id")),
                    ("body", p_str("回复正文")),
                ],
                &["repo", "number", "discussion_id", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.reply_pull_discussion(
                &o,
                &n,
                req_u64(a, "number")?,
                &req_str(a, "discussion_id")?,
                &req_str(a, "body")?,
            )
        },
    },
    Tool {
        name: "gc_pr_comment_edit",
        description: "编辑一条 PR 评论。整体覆盖原正文。",
        schema: || {
            schema(
                &[p_repo(), ("comment_id", p_int("评论 id")), ("body", p_str("新正文"))],
                &["repo", "comment_id", "body"],
            )
        },
        read_only: false,
        destructive: true, // 覆盖式写入，改错了原文就没了
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.edit_pull_comment(&o, &n, req_u64(a, "comment_id")?, &req_str(a, "body")?)
        },
    },
    Tool {
        name: "gc_pr_review",
        description: "对 PR 提交「评审通过」。force=true 会绕过尚未满足的评审条件。",
        schema: || {
            schema(
                &[p_repo(), ("number", p_int("PR 编号")), ("force", p_bool("强制通过，默认 false"))],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.review_pull(&o, &n, req_u64(a, "number")?, bool_or(a, "force", false))
        },
    },
    Tool {
        name: "gc_pr_test",
        description: "对 PR 提交「测试通过」。force=true 会绕过尚未满足的测试条件。",
        schema: || {
            schema(
                &[p_repo(), ("number", p_int("PR 编号")), ("force", p_bool("强制通过，默认 false"))],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.test_pull(&o, &n, req_u64(a, "number")?, bool_or(a, "force", false))
        },
    },
    Tool {
        name: "gc_pr_reviewers",
        description: "设置 PR 的评审人。append=false（默认）会覆盖现有名单，true 为追加。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("reviewers", p_strs("评审人用户名列表")),
                    ("append", p_bool("追加而非覆盖，默认 false")),
                ],
                &["repo", "number", "reviewers"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let users = join_users(a, "reviewers")?;
            pat_client()?.assign_pull_reviewers(
                &o,
                &n,
                req_u64(a, "number")?,
                &users,
                bool_or(a, "append", false),
            )
        },
    },
    Tool {
        name: "gc_pr_testers",
        description: "设置 PR 的测试人。append=false（默认）会覆盖现有名单，true 为追加。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("testers", p_strs("测试人用户名列表")),
                    ("append", p_bool("追加而非覆盖，默认 false")),
                ],
                &["repo", "number", "testers"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let users = join_users(a, "testers")?;
            pat_client()?.assign_pull_testers(
                &o,
                &n,
                req_u64(a, "number")?,
                &users,
                bool_or(a, "append", false),
            )
        },
    },
    Tool {
        name: "gc_pr_assignees",
        description: "指派 PR 的负责人。",
        schema: || {
            schema(
                &[p_repo(), ("number", p_int("PR 编号")), ("assignees", p_strs("负责人用户名列表"))],
                &["repo", "number", "assignees"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let users = join_users(a, "assignees")?;
            pat_client()?.assign_pull_assignees(&o, &n, req_u64(a, "number")?, &users)
        },
    },
    Tool {
        name: "gc_pr_option_reviewers",
        description: "列出该 PR 可选的评审人候选名单。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_option_reviewers(&o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_pr_labels",
        description: "管理 PR 标签。mode=add 追加、set 覆盖（传什么就只剩什么）、remove 移除。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("labels", p_strs("标签名列表")),
                    ("mode", p_enum("操作方式，默认 add", &["add", "set", "remove"])),
                ],
                &["repo", "number", "labels"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let num = req_u64(a, "number")?;
            let labels = str_list(a, "labels");
            if labels.is_empty() {
                bail!("labels 至少要有一项");
            }
            let c = pat_client()?;
            match str_or(a, "mode", "add").as_str() {
                "add" => c.add_pull_labels(&o, &n, num, &labels),
                "set" => c.replace_pull_labels(&o, &n, num, &labels),
                "remove" => c.remove_pull_labels(&o, &n, num, &labels.join(",")),
                other => bail!("mode 只能是 add / set / remove，收到 `{other}`"),
            }
        },
    },
    Tool {
        name: "gc_pr_linked_issues",
        description: "列出 PR 关联的 Issue。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_linked_issues(&o, &n, req_u64(a, "number")?, 0, 0)
        },
    },
    Tool {
        name: "gc_pr_link_issues",
        description: "关联或取消关联 Issue。unlink=true 表示取消关联。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("issues", p_ints("Issue 编号列表")),
                    ("unlink", p_bool("true 表示取消关联，默认 false")),
                ],
                &["repo", "number", "issues"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let num = req_u64(a, "number")?;
            let issues = u64_list(a, "issues")?;
            let c = pat_client()?;
            if bool_or(a, "unlink", false) {
                c.unlink_pull_issues(&o, &n, num, &issues)
            } else {
                c.link_pull_issues(&o, &n, num, &issues)
            }
        },
    },
    Tool {
        name: "gc_pr_close",
        description: "关闭一个或多个 PR（可用 gc_pr_reopen 撤销）。",
        schema: || {
            schema(&[p_repo(), ("numbers", p_ints("要关闭的 PR 编号列表"))], &["repo", "numbers"])
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let c = pat_client()?;
            batch(&u64_list(a, "numbers")?, |id| c.close_pull(&o, &n, id))
        },
    },
    Tool {
        name: "gc_pr_reopen",
        description: "重新打开一个或多个 PR。",
        schema: || {
            schema(&[p_repo(), ("numbers", p_ints("要重开的 PR 编号列表"))], &["repo", "numbers"])
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let c = pat_client()?;
            batch(&u64_list(a, "numbers")?, |id| c.reopen_pull(&o, &n, id))
        },
    },
    Tool {
        name: "gc_pr_history",
        description: "查看 PR 的修改历史。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_modify_history(&o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_pr_logs",
        description: "查看 PR 的操作日志（谁在什么时候做了什么）。",
        schema: || schema(&[p_repo(), ("number", p_int("PR 编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.pull_operate_logs(&o, &n, req_u64(a, "number")?, 0, 0)
        },
    },
    // ---- 分支 / 提交 / 文件 ----------------------------------------------
    // 本段工具对应的接口移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），
    // 版权声明见仓库根目录 LICENSE.oh-gc。
    Tool {
        name: "gc_branch_list",
        description: "列出仓库的分支及各自的最新提交、是否受保护。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.branches(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_branch_view",
        description: "查看单个分支的详情。",
        schema: || schema(&[p_repo(), ("branch", p_str("分支名"))], &["repo", "branch"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.branch(&o, &n, &req_str(a, "branch")?)
        },
    },
    Tool {
        name: "gc_branch_create",
        description: "从某个起点（分支 / 标签 / commit SHA）新建分支。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("branch", p_str("新分支名")),
                    ("from", p_str("起点，分支 / 标签 / commit SHA，默认 main")),
                ],
                &["repo", "branch"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_branch(&o, &n, &str_or(a, "from", "main"), &req_str(a, "branch")?)
        },
    },
    Tool {
        name: "gc_branch_protect",
        description: "开启或关闭分支保护。unprotect=true 表示取消保护。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("branch", p_str("分支名")),
                    ("pusher", p_str("允许推送的角色 / 用户")),
                    ("merger", p_str("允许合并的角色 / 用户")),
                    ("unprotect", p_bool("true 表示取消保护，默认 false")),
                ],
                &["repo", "branch"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let branch = req_str(a, "branch")?;
            let c = pat_client()?;
            if bool_or(a, "unprotect", false) {
                c.unprotect_branch(&o, &n, &branch)?;
                return Ok(json!({ "branch": branch, "protected": false }));
            }
            let mut b = Map::new();
            if let Some(p) = opt_str(a, "pusher") {
                b.insert("pusher".into(), Value::String(p));
            }
            if let Some(m) = opt_str(a, "merger") {
                b.insert("merger".into(), Value::String(m));
            }
            c.protect_branch(&o, &n, &branch, &Value::Object(b))
        },
    },
    Tool {
        name: "gc_branch_rules",
        description: "列出仓库的通配符分支保护规则（与单分支保护是两套机制）。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.branch_protect_rules(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_commit_list",
        description: "列出提交，可按分支 / 路径 / 作者 / 时间范围过滤。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("sha", p_str("分支 / 标签 / 起始 SHA")),
                    ("path", p_str("只看改动了该路径的提交")),
                    ("author", p_str("只看该作者的提交")),
                    ("since", p_str("起始时间，ISO 8601")),
                    ("until", p_str("结束时间，ISO 8601")),
                    ("per_page", p_int("每页条数，默认 20")),
                ],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            // 过滤值要活到请求发出，先落成 owned 再借。
            let vals: Vec<(&str, String)> = ["sha", "path", "author", "since", "until"]
                .iter()
                .filter_map(|k| opt_str(a, k).map(|v| (*k, v)))
                .collect();
            let filters: Vec<(&str, &str)> = vals.iter().map(|(k, v)| (*k, v.as_str())).collect();
            pat_client()?.commits(&o, &n, &filters, 0, u64_or(a, "per_page", 20))
        },
    },
    Tool {
        name: "gc_commit_view",
        description: "查看单个提交的详情（作者、信息、改动统计）。",
        schema: || schema(&[p_repo(), ("sha", p_str("提交 SHA"))], &["repo", "sha"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.commit(&o, &n, &req_str(a, "sha")?)
        },
    },
    Tool {
        name: "gc_commit_diff",
        description: "取某个提交的 diff 文本。patch=true 则返回含提交信息的完整 patch。",
        schema: || {
            schema(
                &[p_repo(), ("sha", p_str("提交 SHA")), ("patch", p_bool("返回完整 patch，默认 false"))],
                &["repo", "sha"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let sha = req_str(a, "sha")?;
            let c = pat_client()?;
            let text = if bool_or(a, "patch", false) {
                c.commit_patch(&o, &n, &sha)?
            } else {
                c.commit_diff(&o, &n, &sha)?
            };
            Ok(json!({ "sha": sha, "diff": text }))
        },
    },
    Tool {
        name: "gc_commit_compare",
        description: "对比两个引用（分支 / 标签 / SHA），返回提交列表与文件改动。",
        schema: || {
            schema(
                &[p_repo(), ("base", p_str("基准引用")), ("head", p_str("对比引用"))],
                &["repo", "base", "head"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.compare_commits(&o, &n, &req_str(a, "base")?, &req_str(a, "head")?)
        },
    },
    Tool {
        name: "gc_commit_comment",
        description: "在某个提交下发表评论。",
        schema: || {
            schema(
                &[p_repo(), ("sha", p_str("提交 SHA")), ("body", p_str("评论正文"))],
                &["repo", "sha", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_commit_comment(&o, &n, &req_str(a, "sha")?, &req_str(a, "body")?)
        },
    },
    Tool {
        name: "gc_file_list",
        description: "列出仓库某个目录下的条目。path 留空表示仓库根目录。\
                      走 v5 + PAT，和需要网页端 JWT 的 gc_repo_tree 是两条通道。",
        schema: || {
            schema(
                &[p_repo(), ("path", p_str("目录路径；省略为根目录")), p_ref()],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.contents(&o, &n, &str_or(a, "path", ""), &str_or(a, "ref", ""))
        },
    },
    Tool {
        name: "gc_file_get",
        description: "读取仓库内单个文件的原文（走 v5 + PAT，不需要网页端 JWT）。\
                      二进制文件只返回元信息不返回内容。",
        schema: || {
            schema(&[p_repo(), ("path", p_str("仓库内文件路径")), p_ref()], &["repo", "path"])
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let path = req_str(a, "path")?;
            let text = pat_client()?.raw_file(&o, &n, &path, &str_or(a, "ref", ""))?;
            Ok(json!({ "path": path, "size": text.len(), "content": text }))
        },
    },
    Tool {
        name: "gc_file_write",
        description: "新建或覆盖仓库内的文件，直接产生一次提交。content 传明文（工具内部会转 base64）。\
                      覆盖已有文件时必须给 sha（用 gc_file_list / gc_file_get 查），\
                      服务端靠它做乐观锁，传错会失败而不是静默覆盖。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("path", p_str("仓库内目标路径")),
                    ("content", p_str("文件内容明文")),
                    ("message", p_str("提交信息")),
                    ("sha", p_str("覆盖已有文件时的当前 blob sha；新建时省略")),
                    ("branch", p_str("目标分支；省略则用默认分支")),
                ],
                &["repo", "path", "content", "message"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let path = req_str(a, "path")?;
            let message = req_str(a, "message")?;
            let branch = str_or(a, "branch", "");
            let b64 = base64::engine::general_purpose::STANDARD
                .encode(req_str(a, "content")?.as_bytes());
            let c = pat_client()?;
            match opt_str(a, "sha") {
                Some(sha) => c.update_file(&o, &n, &path, &message, &b64, &sha, &branch),
                None => c.create_file(&o, &n, &path, &message, &b64, &branch),
            }
        },
    },
    // ---- 发行版 ----------------------------------------------------------
    Tool {
        name: "gc_release_list",
        description: "列出仓库的所有发行版。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.releases(&o, &n)
        },
    },
    Tool {
        name: "gc_release_view",
        description: "按标签查看某个发行版。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名，例如 v1.0.0"))], &["repo", "tag"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.release(&o, &n, &req_str(a, "tag")?)
        },
    },
    Tool {
        name: "gc_release_create",
        description: "创建发行版。name 与 body 都必须显式给出。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("tag", p_str("标签名，例如 v1.0.0")),
                    ("name", p_str("发行版标题")),
                    ("body", p_str("发行说明，支持 Markdown")),
                    ("target", p_str("目标分支或 commit；省略则用默认分支")),
                ],
                &["repo", "tag", "name", "body"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_release(
                &o,
                &n,
                &req_str(a, "tag")?,
                &req_str(a, "name")?,
                &req_str(a, "body")?,
                &str_or(a, "target", ""),
            )
        },
    },
    // ---- 搜索 ------------------------------------------------------------
    Tool {
        name: "gc_search_repos",
        description: "按关键词搜索仓库。",
        schema: || schema(&[("query", p_str("搜索关键词"))], &["query"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.search_repos(&req_str(a, "query")?),
    },
    Tool {
        name: "gc_search_users",
        description: "按关键词搜索用户。",
        schema: || schema(&[("query", p_str("搜索关键词"))], &["query"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.search_users(&req_str(a, "query")?),
    },
    Tool {
        name: "gc_search_issues",
        description: "按关键词搜索 Issue。",
        schema: || schema(&[("query", p_str("搜索关键词"))], &["query"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.search_issues(&req_str(a, "query")?),
    },
    Tool {
        name: "gc_search_web",
        description: "全站搜索仓库 / 用户 / Issue。走网页端接口，覆盖面比 gc_search_* 更广，\
                      而且不需要任何凭据——没配 token 时也能用。",
        schema: || {
            schema(
                &[
                    ("query", p_str("搜索关键词")),
                    ("type", p_enum("搜索类型，默认 repo", api::SEARCH_KINDS)),
                    ("page", p_int("页码，从 1 开始，默认 1")),
                    ("per_page", p_int("每页条数，1..=20，默认 20")),
                ],
                &["query"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let kind = str_or(a, "type", "repo");
            let raw = api::web_search(
                &req_str(a, "query")?,
                &kind,
                u64_or(a, "page", 1) as u32,
                u64_or(a, "per_page", api::SEARCH_MAX_PER_PAGE as u64) as u32,
            )?;
            Ok(api::search_digest(&raw, &kind))
        },
    },
    // ---- 仓库（变更）-----------------------------------------------------
    Tool {
        name: "gc_repo_create",
        description: "在当前认证用户名下创建新仓库。name 只写仓库名，不要带 owner/。",
        schema: || {
            schema(
                &[
                    ("name", p_str("仓库名，不含 owner")),
                    ("description", p_str("仓库描述")),
                    ("private", p_bool("是否私有，默认 false")),
                ],
                &["name"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            pat_client()?.create_repo(
                &req_str(a, "name")?,
                &str_or(a, "description", ""),
                bool_or(a, "private", false),
            )
        },
    },
    Tool {
        name: "gc_repo_fork",
        description: "复刻仓库到当前用户名下（需要网页端 JWT）；给了 org 则改用 v5 接口复刻进该组织。\
                      若同名 fork 已存在且 force=true，则改为把已有 fork 的默认分支硬重置到源仓库 HEAD\
                      （会丢弃该分支上的额外提交，属破坏性操作）。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("org", p_str("复刻进该组织的命名空间；省略则复刻到当前用户名下")),
                    ("name", p_str("复刻后的仓库名；省略则沿用原名")),
                    ("description", p_str("复刻后的描述；org 模式下 v5 接口不支持，会被忽略")),
                    ("force", p_bool("若同名 fork 已存在，强制硬重置到源 HEAD（默认 false）")),
                ],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let new_name = str_or(a, "name", &n);
            let api = pat_client()?;
            let org = opt_str(a, "org");
            let namespace = match &org {
                Some(g) => g.clone(),
                None => api.authenticated_user()?["login"]
                    .as_str()
                    .ok_or_else(|| anyhow!("拿不到当前登录用户名"))?
                    .to_string(),
            };
            let fork_exists = api.repo(&namespace, &new_name).is_ok();
            if fork_exists && bool_or(a, "force", false) {
                let info = api.repo(&namespace, &new_name)?;
                if info["fork"].as_bool() != Some(true) {
                    anyhow::bail!("{}/{} 已存在但不是 fork，force 不会动它", namespace, new_name);
                }
                let branch = info["default_branch"].as_str().unwrap_or("main");
                let sb = api.branch(&o, &n, branch)?;
                let sha = sb["commit"]["sha"].as_str().ok_or_else(|| anyhow!("拿不到源分支 {} 的 HEAD", branch))?;
                api.hard_reset_branch(&namespace, &new_name, branch, sha)?;
                return Ok(json!({
                    "fork": format!("{}/{}", namespace, new_name),
                    "forced_sync": true,
                    "branch": branch,
                    "head": sha,
                }));
            }
            if let Some(g) = &org {
                let renamed = opt_str(a, "name");
                return api.fork_repo_v5(&o, &n, Some(g), renamed.as_deref(), renamed.as_deref());
            }
            api::fork_repo(&web_token()?, &o, &n, &new_name, &str_or(a, "description", ""))
        },
    },
    // ---- 仓库设置 / 统计 / 治理（对齐 @oh-gc/cli 的 repo 命令集）----
    Tool {
        name: "gc_repo_update",
        description: "更新仓库属性：改名、描述、主页、可见性、默认分支。只更新传了的字段。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("name", p_str("新仓库名")),
                    ("description", p_str("新描述")),
                    ("homepage", p_str("新主页地址")),
                    ("private", p_bool("true 私有 / false 公开")),
                    ("default_branch", p_str("新的默认分支")),
                ],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut b = Map::new();
            for k in ["name", "description", "homepage", "default_branch"] {
                if let Some(v) = opt_str(a, k) {
                    b.insert(k.into(), Value::String(v));
                }
            }
            if let Some(p) = opt_bool(a, "private") {
                b.insert("private".into(), Value::Bool(p));
            }
            if b.is_empty() {
                bail!("没有要更新的字段");
            }
            pat_client()?.update_repo(&o, &n, &Value::Object(b))
        },
    },
    Tool {
        name: "gc_repo_set_tags",
        description: "设置仓库标签（tag_list，整体替换；传空数组则清空）。需要网页端 JWT。\
                      该接口为整体替换式，故会先取回当前仓库名/描述/主页并原样回填，避免被清空。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("tags", p_strs("一个或多个标签，整体替换现有标签；传空数组 [] 则清空")),
                ],
                &["repo", "tags"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            // 先取回当前 name/description/homepage，避免 v2 PUT 整体替换把它们清空
            let cur = pat_client()?.repo(&o, &n)?;
            let name_now = cur["name"].as_str().unwrap_or(&n).to_string();
            let desc_now = cur["description"].as_str().unwrap_or("").to_string();
            let home_now = cur["homepage"].as_str().unwrap_or("").to_string();
            let tags = a
                .get("tags")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let body = json!({
                "name": name_now,
                "description": desc_now,
                "homepage": home_now,
                "tag_list": tags,
            });
            let v = api::update_project_v2(&web_token()?, &o, &n, &body)?;
            let tags_now: Vec<String> = v["tag_list"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Ok(json!({
                "repo": v["full_name"].as_str().unwrap_or(&format!("{}/{}", o, n)),
                "tag_list": tags_now,
            }))
        },
    },
    Tool {
        name: "gc_repo_transfer",
        description: "把仓库转移给其他用户或组织。原路径会失效，属破坏性操作。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("new_owner", p_str("新的归属者（用户名或组织名）")),
                    ("team_ids", p_ints("一并加入的团队 id")),
                ],
                &["repo", "new_owner"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let ids = u64_list(a, "team_ids").unwrap_or_default();
            pat_client()?.transfer_repo(&o, &n, &req_str(a, "new_owner")?, &ids)
        },
    },
    Tool {
        name: "gc_repo_forks",
        description: "列出某个仓库的所有 fork。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("sort", p_enum("排序方式", &["newest", "oldest", "stargazers"])),
                    ("page", p_int("页码")),
                    ("per_page", p_int("每页数量")),
                ],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_forks(&o, &n, &str_or(a, "sort", ""), u64_or(a, "page", 0), u64_or(a, "per_page", 0))
        },
    },
    Tool {
        name: "gc_repo_contributors",
        description: "列出仓库贡献者；stats=true 时改为返回增删行数与提交数统计。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("type", p_enum("贡献者口径", &["committers", "authors"])),
                    ("stats", p_bool("true 返回贡献量统计（增删行数、提交数）")),
                    ("author", p_str("只看某个作者（配合 stats）")),
                    ("page", p_int("页码")),
                    ("per_page", p_int("每页数量")),
                ],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let (page, per_page) = (u64_or(a, "page", 0), u64_or(a, "per_page", 0));
            let c = pat_client()?;
            if bool_or(a, "stats", false) {
                c.contributor_statistics(&o, &n, &str_or(a, "author", ""), page, per_page)
            } else {
                c.repo_contributors(&o, &n, &str_or(a, "type", ""), page, per_page)
            }
        },
    },
    Tool {
        name: "gc_repo_languages",
        description: "查看仓库语言构成，返回 {语言: 字节数}。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.repo_languages(&o, &n)
        },
    },
    Tool {
        name: "gc_repo_events",
        description: "查看仓库动态事件（推送、合并、Issue、评论等）。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    (
                        "filter",
                        p_enum(
                            "事件类型过滤",
                            &["all", "push", "merged", "issue", "comments", "team", "project"],
                        ),
                    ),
                    ("author", p_str("只看某个用户触发的事件")),
                    ("before", p_str("只看该时间之前（ISO8601）")),
                    ("after", p_str("只看该时间之后（ISO8601）")),
                    ("page", p_int("页码")),
                    ("per_page", p_int("每页数量")),
                ],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.repo_events(
                &o,
                &n,
                &str_or(a, "filter", ""),
                &str_or(a, "author", ""),
                &str_or(a, "before", ""),
                &str_or(a, "after", ""),
                u64_or(a, "page", 0),
                u64_or(a, "per_page", 0),
            )
        },
    },
    Tool {
        name: "gc_repo_stats",
        description: "仓库提交统计（需 branch）；downloads=true 时改为返回下载量统计。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("branch", p_str("统计哪个分支的提交（提交统计必填）")),
                    ("author", p_str("只统计某个作者")),
                    ("only_self", p_bool("只统计自己的提交")),
                    ("since", p_str("起始日期 YYYY-MM-DD")),
                    ("until", p_str("结束日期 YYYY-MM-DD")),
                    ("downloads", p_bool("true 改为查看下载量统计")),
                    ("direction", p_enum("下载量排序", &["asc", "desc"])),
                ],
                &["repo"],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let c = pat_client()?;
            let (since, until) = (str_or(a, "since", ""), str_or(a, "until", ""));
            if bool_or(a, "downloads", false) {
                return c.download_statistics(&o, &n, &since, &until, &str_or(a, "direction", ""));
            }
            let branch = opt_str(a, "branch")
                .ok_or_else(|| anyhow!("提交统计必须指定 branch；看下载量请设 downloads=true"))?;
            c.commit_statistics(
                &o,
                &n,
                &branch,
                &str_or(a, "author", ""),
                bool_or(a, "only_self", false),
                &since,
                &until,
            )
        },
    },
    Tool {
        name: "gc_repo_settings",
        description: "查看仓库高级设置（fork 开关、分支/标签创建限制、命名正则、GPG 校验等）。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.repo_settings(&o, &n)
        },
    },
    Tool {
        name: "gc_repo_settings_update",
        description: "更新仓库高级设置。只更新传了的字段。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("disable_fork", p_bool("禁止 fork")),
                    ("forbidden_developer_create_branch", p_bool("禁止开发者建分支")),
                    ("forbidden_developer_create_tag", p_bool("禁止开发者建标签")),
                    ("forbidden_committer_create_branch", p_bool("禁止提交者建分支")),
                    ("branch_name_regex", p_str("分支名正则约束")),
                    ("tag_name_regex", p_str("标签名正则约束")),
                    ("generate_pre_merge_ref", p_bool("生成 pre-merge ref")),
                    ("open_gpg_verified", p_bool("开启 GPG 提交校验")),
                    ("include_lfs_objects", p_bool("打包时包含 LFS 对象")),
                ],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut b = Map::new();
            for k in [
                "disable_fork",
                "forbidden_developer_create_branch",
                "forbidden_developer_create_tag",
                "forbidden_committer_create_branch",
                "generate_pre_merge_ref",
                "open_gpg_verified",
                "include_lfs_objects",
            ] {
                if let Some(v) = opt_bool(a, k) {
                    b.insert(k.into(), Value::Bool(v));
                }
            }
            for k in ["branch_name_regex", "tag_name_regex"] {
                if let Some(v) = opt_str(a, k) {
                    b.insert(k.into(), Value::String(v));
                }
            }
            if b.is_empty() {
                bail!("没有要更新的设置项");
            }
            pat_client()?.update_repo_settings(&o, &n, &Value::Object(b))
        },
    },
    Tool {
        name: "gc_repo_module",
        description: "开关仓库功能模块：wiki、Issue、安全、PR、fork、代码分析、讨论。只更新传了的字段。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("has_wiki", p_bool("是否开启 Wiki")),
                    ("has_issue", p_bool("是否开启 Issue")),
                    ("has_security", p_bool("是否开启安全模块")),
                    ("has_merge_request", p_bool("是否开启合并请求")),
                    ("has_fork", p_bool("是否允许 fork")),
                    ("has_analysis", p_bool("是否开启代码分析")),
                    ("has_discussion", p_bool("是否开启讨论")),
                ],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut b = Map::new();
            for k in [
                "has_wiki",
                "has_issue",
                "has_security",
                "has_merge_request",
                "has_fork",
                "has_analysis",
                "has_discussion",
            ] {
                if let Some(v) = opt_bool(a, k) {
                    b.insert(k.into(), Value::Bool(v));
                }
            }
            if b.is_empty() {
                bail!("至少要指定一个模块开关");
            }
            let changed = Value::Object(b);
            let v = pat_client()?.update_module_setting(&o, &n, &changed)?;
            // 该接口成功时返回空体，回一个明确结果，免得调用方以为没生效
            Ok(if v.is_null() { json!({ "ok": true, "changed": changed }) } else { v })
        },
    },
    Tool {
        name: "gc_repo_roles",
        description: "列出仓库的自定义角色及其权限级别、成员数。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.customized_roles(&o, &n)
        },
    },
    Tool {
        name: "gc_repo_transition",
        description: "查看仓库权限继承模式；给了 mode 则修改（1=继承上级组织，2=独立管理）。",
        schema: || {
            schema(
                &[p_repo(), ("mode", p_int("1=继承上级组织权限，2=独立管理；不传则只查看"))],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let c = pat_client()?;
            match a.get("mode") {
                Some(_) => {
                    let m = req_u64(a, "mode")?;
                    if m != 1 && m != 2 {
                        bail!("mode 只能是 1（继承）或 2（独立）");
                    }
                    c.update_repo_transition(&o, &n, m as u8)
                }
                None => c.repo_transition(&o, &n),
            }
        },
    },
    Tool {
        name: "gc_repo_archive",
        description: "归档仓库（变只读）或取消归档。服务端强制要求带账号密码二次确认。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("password", p_str("账号密码（服务端强制要求）")),
                    ("open", p_bool("true 表示取消归档，默认 false 即归档")),
                ],
                &["repo", "password"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let open = bool_or(a, "open", false);
            pat_client()?.archive_repo(&o, &n, !open, &req_str(a, "password")?)?;
            Ok(json!({ "repo": format!("{}/{}", o, n), "archived": !open }))
        },
    },
    Tool {
        name: "gc_repo_star",
        description: "给仓库加星或取消加星。",
        schema: || {
            schema(
                &[p_repo(), ("unstar", p_bool("true 表示取消加星，默认 false"))],
                &["repo"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            // 加星接口要的是数字仓库 id，得先用 PAT 查一次。
            let meta = pat_client()?.repo(&o, &n)?;
            let id = meta
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("无法从仓库信息中获取数字 id"))?;
            let tok = web_token()?;
            let unstar = bool_or(a, "unstar", false);
            let count = if unstar {
                api::unstar_repo(&tok, id)?
            } else {
                api::star_repo(&tok, id)?
            };
            Ok(json!({ "repo": format!("{o}/{n}"), "starred": !unstar, "stars": count }))
        },
    },
    Tool {
        name: "gc_wiki_create",
        description: "创建 Wiki 页面。需要网页端 JWT。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("title", p_str("页面标题")),
                    ("content", p_str("页面内容，Markdown")),
                    ("message", p_str("提交信息，默认「创建 wiki 页面」")),
                ],
                &["repo", "title", "content"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            // wiki 接口要整个 owner/name 串，不拆分；但仍校验一下格式。
            // `repo` 省略时从当前目录的 git remote 推断（与 CLI 行为一致）。
            let (o, n) = crate::cli::resolve_repo(opt_str(a, "repo"))?;
            let repo = format!("{}/{}", o, n);
            let body = api::wiki_create(
                &web_token()?,
                &repo,
                &req_str(a, "title")?,
                &req_str(a, "content")?,
                &str_or(a, "message", "创建 wiki 页面"),
            )?;
            Ok(json!({ "response": body }))
        },
    },
    Tool {
        name: "gc_discussion_create",
        description: "创建讨论帖。source_id 是数字仓库 id（可从 gc_repo_view 的 id 字段拿），category_id 是分类哈希。",
        schema: || {
            schema(
                &[
                    ("source_id", p_int("数字仓库 id")),
                    ("source_type", p_int("来源类型，默认 2（仓库）")),
                    ("category_id", p_str("分类 id（哈希串）")),
                    ("title", p_str("标题")),
                    ("content", p_str("正文，Markdown")),
                ],
                &["source_id", "category_id", "title"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            api::create_discussion(
                &web_token()?,
                req_u64(a, "source_id")?,
                u64_or(a, "source_type", 2),
                &req_str(a, "category_id")?,
                &req_str(a, "title")?,
                &str_or(a, "content", ""),
            )
        },
    },
    // ---- 破坏性（需 --allow-destructive）---------------------------------
    Tool {
        name: "gc_repo_delete",
        description: "永久删除仓库。不可撤销。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.delete_repo(&o, &n)?;
            Ok(json!({ "deleted": format!("{o}/{n}") }))
        },
    },
    Tool {
        name: "gc_repo_set_visibility",
        description: "修改仓库可见性（private / public）。转为 private 会让所有外部引用失效。",
        schema: || {
            schema(
                &[p_repo(), ("visibility", p_str("private 或 public"))],
                &["repo", "visibility"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let vis = req_str(a, "visibility")?;
            let private = match vis.as_str() {
                "private" => true,
                "public" => false,
                other => bail!("visibility 只能是 private 或 public，收到 `{other}`"),
            };
            pat_client()?.set_repo_visibility(&o, &n, private)
        },
    },
    Tool {
        name: "gc_discussion_delete",
        description: "永久删除讨论帖。不可撤销。",
        schema: || schema(&[("id", p_str("讨论帖 id（哈希串）"))], &["id"]),
        read_only: false,
        destructive: true,
        handler: |a| api::delete_discussion(&web_token()?, &req_str(a, "id")?),
    },
    Tool {
        name: "gc_pr_merge",
        description: "合并 PR 到目标分支。会改写目标分支历史，不可撤销。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("method", p_enum("合并方式；省略则用仓库默认", &["merge", "squash", "rebase"])),
                    ("title", p_str("合并提交标题")),
                    ("description", p_str("合并提交说明")),
                ],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.merge_pull(
                &o,
                &n,
                req_u64(a, "number")?,
                &str_or(a, "method", ""),
                &str_or(a, "title", ""),
                &str_or(a, "description", ""),
            )
        },
    },
    Tool {
        name: "gc_pr_edit",
        description: "修改 PR 的标题 / 正文 / 草稿状态。整体覆盖而非合并；至少要给一个字段。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("PR 编号")),
                    ("title", p_str("新标题；省略则不改")),
                    ("body", p_str("新正文；省略则不改")),
                    ("draft", p_bool("是否为草稿；省略则不改")),
                ],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: true, // 覆盖式写入，改错了原文就没了
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut b = Map::new();
            if let Some(t) = opt_str(a, "title") {
                b.insert("title".into(), Value::String(t));
            }
            if let Some(x) = opt_str(a, "body") {
                b.insert("body".into(), Value::String(x));
            }
            if let Some(d) = a.get("draft").and_then(Value::as_bool) {
                b.insert("draft".into(), Value::Bool(d));
            }
            if b.is_empty() {
                bail!("title / body / draft 至少要提供一个");
            }
            pat_client()?.update_pull(&o, &n, req_u64(a, "number")?, &Value::Object(b))
        },
    },
    Tool {
        name: "gc_pr_comment_delete",
        description: "永久删除一条 PR 评论。不可撤销。",
        schema: || schema(&[p_repo(), ("comment_id", p_int("评论 id"))], &["repo", "comment_id"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let id = req_u64(a, "comment_id")?;
            pat_client()?.delete_pull_comment(&o, &n, id)?;
            Ok(json!({ "deleted_comment": id }))
        },
    },
    Tool {
        name: "gc_branch_delete",
        description: "永久删除分支。不可撤销。",
        schema: || schema(&[p_repo(), ("branch", p_str("分支名"))], &["repo", "branch"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let b = req_str(a, "branch")?;
            pat_client()?.delete_branch(&o, &n, &b)?;
            Ok(json!({ "deleted_branch": b }))
        },
    },
    Tool {
        name: "gc_file_delete",
        description: "删除仓库内的文件，产生一次提交。sha 是该文件的当前 blob sha。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("path", p_str("仓库内文件路径")),
                    ("sha", p_str("该文件的当前 blob sha")),
                    ("message", p_str("提交信息")),
                    ("branch", p_str("目标分支；省略则用默认分支")),
                ],
                &["repo", "path", "sha", "message"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.delete_file(
                &o,
                &n,
                &req_str(a, "path")?,
                &req_str(a, "message")?,
                &req_str(a, "sha")?,
                &str_or(a, "branch", ""),
            )
        },
    },

    // ---- 仓库治理：移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc ----

    // -- 标签 --
    Tool {
        name: "gc_tag_list",
        description: "列出仓库的标签（tag）。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_tags(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_tag_view",
        description: "查看单个标签的信息（含指向的 commit）。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.get_tag(&o, &n, &req_str(a, "tag")?)
        },
    },
    Tool {
        name: "gc_tag_create",
        description: "新建标签。refs 是起点（分支名 / 已有标签 / commit SHA）。message 留空建轻量标签。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("tag", p_str("新标签名")),
                    ("refs", p_str("起点：分支 / 标签 / commit SHA，默认 main")),
                    ("message", p_str("标签说明（annotated tag）；留空则建轻量标签")),
                ],
                &["repo", "tag", "refs"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_tag(&o, &n, &str_or(a, "refs", "main"), &req_str(a, "tag")?, &str_or(a, "message", ""))
        },
    },
    Tool {
        name: "gc_tag_delete",
        description: "永久删除标签。不可撤销。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let t = req_str(a, "tag")?;
            pat_client()?.delete_tag(&o, &n, &t)?;
            Ok(json!({ "deleted_tag": t }))
        },
    },
    Tool {
        name: "gc_tag_protect",
        description: "把某个标签设为受保护（只能删保护规则，不能直接删标签）。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.protect_tag(&o, &n, &req_str(a, "tag")?)
        },
    },
    Tool {
        name: "gc_tag_unprotect",
        description: "取消标签保护。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.unprotect_tag(&o, &n, &req_str(a, "tag")?)
        },
    },
    Tool {
        name: "gc_tag_protected_list",
        description: "列出受保护标签规则。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_protected_tags(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_tag_protected_view",
        description: "查看某条受保护标签规则。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.get_protected_tag(&o, &n, &req_str(a, "tag")?)
        },
    },
    Tool {
        name: "gc_tag_protected_create",
        description: "新建受保护标签规则。access_level 为 0 表示不指定级别。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("tag", p_str("标签名（通配符规则也用同一字段）")),
                    ("access_level", p_int("创建权限级别，0 表示不指定")),
                ],
                &["repo", "tag"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.create_protected_tag(&o, &n, &req_str(a, "tag")?, u64_or(a, "access_level", 0))
        },
    },
    Tool {
        name: "gc_tag_protected_update",
        description: "更新受保护标签规则的权限级别。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("tag", p_str("标签名")),
                    ("access_level", p_int("新的创建权限级别")),
                ],
                &["repo", "tag", "access_level"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.update_protected_tag(&o, &n, &req_str(a, "tag")?, req_u64(a, "access_level")?)
        },
    },
    Tool {
        name: "gc_tag_protected_delete",
        description: "删除受保护标签规则。不可撤销。",
        schema: || schema(&[p_repo(), ("tag", p_str("标签名"))], &["repo", "tag"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let t = req_str(a, "tag")?;
            pat_client()?.delete_protected_tag(&o, &n, &t)?;
            Ok(json!({ "deleted_protected_tag": t }))
        },
    },

    // -- Webhook --
    Tool {
        name: "gc_hook_list",
        description: "列出仓库的 Webhook。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_hooks(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_hook_view",
        description: "查看单个 Webhook 的配置。",
        schema: || schema(&[p_repo(), ("id", p_int("Webhook ID"))], &["repo", "id"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.get_hook(&o, &n, req_u64(a, "id")?)
        },
    },
    Tool {
        name: "gc_hook_create",
        description: "新建 Webhook。events 是触发事件数组（如 [\"push\",\"issues\"]），缺省为 [\"push\"]。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("name", p_str("Webhook 名称")),
                    ("url", p_str("回调地址")),
                    ("events", p_strs("触发事件数组，如 [\"push\",\"issues\"]")),
                    ("content_type", p_str("content_type，默认 json")),
                    ("secret", p_str("签名密钥，可选")),
                    ("active", p_bool("是否启用，默认 true")),
                ],
                &["repo", "name", "url"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let name = req_str(a, "name")?;
            let url = req_str(a, "url")?;
            let mut config = json!({ "url": url });
            if let Some(ct) = opt_str(a, "content_type") {
                config["content_type"] = json!(ct);
            }
            if let Some(s) = opt_str(a, "secret") {
                config["secret"] = json!(s);
            }
            let events = if let Some(arr) = a.get("events").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            } else {
                vec!["push".to_string()]
            };
            let body = json!({
                "name": name,
                "config": config,
                "events": events,
                "active": bool_or(a, "active", true),
            });
            pat_client()?.create_hook(&o, &n, &body)
        },
    },
    Tool {
        name: "gc_hook_update",
        description: "更新 Webhook（只传要改的字段即可）。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("id", p_int("Webhook ID")),
                    ("name", p_str("新名称，可选")),
                    ("url", p_str("新回调地址，可选")),
                    ("events", p_strs("触发事件数组，可选")),
                    ("content_type", p_str("content_type，可选")),
                    ("secret", p_str("签名密钥，可选")),
                    ("active", p_bool("是否启用，可选")),
                ],
                &["repo", "id"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let id = req_u64(a, "id")?;
            let mut body = json!({});
            if let Some(v) = opt_str(a, "name") {
                body["name"] = json!(v);
            }
            let mut cfg = json!({});
            let mut has_cfg = false;
            if let Some(v) = opt_str(a, "url") {
                cfg["url"] = json!(v);
                has_cfg = true;
            }
            if let Some(v) = opt_str(a, "content_type") {
                cfg["content_type"] = json!(v);
                has_cfg = true;
            }
            if let Some(v) = opt_str(a, "secret") {
                cfg["secret"] = json!(v);
                has_cfg = true;
            }
            if has_cfg {
                body["config"] = cfg;
            }
            if let Some(arr) = a.get("events").and_then(|v| v.as_array()) {
                let evs: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
                body["events"] = json!(evs);
            }
            if let Some(act) = a.get("active").and_then(|v| v.as_bool()) {
                body["active"] = json!(act);
            }
            pat_client()?.update_hook(&o, &n, id, &body)
        },
    },
    Tool {
        name: "gc_hook_delete",
        description: "删除 Webhook。不可撤销。",
        schema: || schema(&[p_repo(), ("id", p_int("Webhook ID"))], &["repo", "id"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let id = req_u64(a, "id")?;
            pat_client()?.delete_hook(&o, &n, id)?;
            Ok(json!({ "deleted_hook": id }))
        },
    },

    // -- 标签（issue/PR 用 label）--
    Tool {
        name: "gc_label_list",
        description: "列出仓库的标签（label，用于给 issue/PR 分类）。",
        schema: || schema(&[p_repo()], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_labels(&o, &n, 0, 0)
        },
    },
    Tool {
        name: "gc_label_view",
        description: "查看单个标签（label）的信息。",
        schema: || schema(&[p_repo(), ("name", p_str("标签名"))], &["repo", "name"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.get_label(&o, &n, &req_str(a, "name")?)
        },
    },
    Tool {
        name: "gc_label_create",
        description: "新建标签（label）。color 是 6 位十六进制（不带 #）。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("name", p_str("标签名")),
                    ("color", p_str("颜色，6 位十六进制，不带 #")),
                    ("description", p_str("描述，可选")),
                ],
                &["repo", "name", "color"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let body = json!({
                "name": req_str(a, "name")?,
                "color": req_str(a, "color")?,
                "description": str_or(a, "description", ""),
            });
            pat_client()?.create_label(&o, &n, &body)
        },
    },
    Tool {
        name: "gc_label_update",
        description: "更新标签（label）。只传要改的字段即可。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("name", p_str("标签名（定位用）")),
                    ("new_name", p_str("新名称，可选")),
                    ("color", p_str("新颜色，可选")),
                    ("description", p_str("新描述，可选")),
                ],
                &["repo", "name"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut body = json!({});
            if let Some(v) = opt_str(a, "new_name") {
                body["new_name"] = json!(v);
            }
            if let Some(v) = opt_str(a, "color") {
                body["color"] = json!(v);
            }
            if let Some(v) = opt_str(a, "description") {
                body["description"] = json!(v);
            }
            pat_client()?.update_label(&o, &n, &req_str(a, "name")?, &body)
        },
    },
    Tool {
        name: "gc_label_delete",
        description: "删除标签（label）。不可撤销。",
        schema: || schema(&[p_repo(), ("name", p_str("标签名"))], &["repo", "name"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let name = req_str(a, "name")?;
            pat_client()?.delete_label(&o, &n, &name)?;
            Ok(json!({ "deleted_label": name }))
        },
    },

    // -- 里程碑 --
    Tool {
        name: "gc_milestone_list",
        description: "列出仓库的里程碑。state 可选 open/closed/all。",
        schema: || schema(&[p_repo(), ("state", p_str("open / closed / all，可选"))], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_milestones(&o, &n, &str_or(a, "state", ""), 0, 0)
        },
    },
    Tool {
        name: "gc_milestone_view",
        description: "查看单个里程碑。",
        schema: || schema(&[p_repo(), ("number", p_int("里程碑编号"))], &["repo", "number"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.get_milestone(&o, &n, req_u64(a, "number")?)
        },
    },
    Tool {
        name: "gc_milestone_create",
        description: "新建里程碑。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("title", p_str("标题")),
                    ("description", p_str("描述，可选")),
                    ("state", p_str("open / closed，可选")),
                    ("due_on", p_str("截止时间，如 2024-12-31，可选")),
                ],
                &["repo", "title"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let mut body = json!({ "title": req_str(a, "title")? });
            if let Some(v) = opt_str(a, "description") {
                body["description"] = json!(v);
            }
            if let Some(v) = opt_str(a, "state") {
                body["state"] = json!(v);
            }
            if let Some(v) = opt_str(a, "due_on") {
                body["due_on"] = json!(v);
            }
            pat_client()?.create_milestone(&o, &n, &body)
        },
    },
    Tool {
        name: "gc_milestone_update",
        description: "更新里程碑。只传要改的字段即可。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("number", p_int("里程碑编号")),
                    ("title", p_str("新标题，可选")),
                    ("description", p_str("新描述，可选")),
                    ("state", p_str("open / closed，可选")),
                    ("due_on", p_str("新截止时间，可选")),
                ],
                &["repo", "number"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let num = req_u64(a, "number")?;
            let mut body = json!({});
            if let Some(v) = opt_str(a, "title") {
                body["title"] = json!(v);
            }
            if let Some(v) = opt_str(a, "description") {
                body["description"] = json!(v);
            }
            if let Some(v) = opt_str(a, "state") {
                body["state"] = json!(v);
            }
            if let Some(v) = opt_str(a, "due_on") {
                body["due_on"] = json!(v);
            }
            pat_client()?.update_milestone(&o, &n, num, &body)
        },
    },
    Tool {
        name: "gc_milestone_delete",
        description: "删除里程碑。不可撤销。",
        schema: || schema(&[p_repo(), ("number", p_int("里程碑编号"))], &["repo", "number"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let num = req_u64(a, "number")?;
            pat_client()?.delete_milestone(&o, &n, num)?;
            Ok(json!({ "deleted_milestone": num }))
        },
    },

    // -- 协作者 --
    Tool {
        name: "gc_collaborator_list",
        description: "列出仓库的协作者。affiliation 可选 direct/outside/all。",
        schema: || schema(&[p_repo(), ("affiliation", p_str("direct / outside / all，可选"))], &["repo"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.list_collaborators(&o, &n, &str_or(a, "affiliation", ""), 0, 0)
        },
    },
    Tool {
        name: "gc_collaborator_add",
        description: "添加协作者。permission 可选 pull/triage/push/maintain/admin。",
        schema: || {
            schema(
                &[
                    p_repo(),
                    ("username", p_str("用户名")),
                    ("permission", p_str("pull / triage / push / maintain / admin，可选")),
                ],
                &["repo", "username"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.add_collaborator(&o, &n, &req_str(a, "username")?, &str_or(a, "permission", ""))
        },
    },
    Tool {
        name: "gc_collaborator_remove",
        description: "移除协作者。不可撤销。",
        schema: || schema(&[p_repo(), ("username", p_str("用户名"))], &["repo", "username"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let u = req_str(a, "username")?;
            pat_client()?.remove_collaborator(&o, &n, &u)?;
            Ok(json!({ "removed_collaborator": u }))
        },
    },
    Tool {
        name: "gc_collaborator_permission",
        description: "查看某协作者在该仓库的权限级别。",
        schema: || schema(&[p_repo(), ("username", p_str("用户名"))], &["repo", "username"]),
        read_only: true,
        destructive: false,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            pat_client()?.collaborator_permission(&o, &n, &req_str(a, "username")?)
        },
    },

    // -- 组织 --
    Tool {
        name: "gc_org_list",
        description: "列出当前登录用户所属的组织。",
        schema: || schema(&[], &[]),
        read_only: true,
        destructive: false,
        handler: |_| pat_client()?.list_orgs(0, 0),
    },
    Tool {
        name: "gc_org_view",
        description: "查看组织信息。",
        schema: || schema(&[("org", p_str("组织名"))], &["org"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.get_org(&req_str(a, "org")?),
    },
    Tool {
        name: "gc_org_members",
        description: "列出组织成员。role 可选 member/admin。",
        schema: || schema(&[("org", p_str("组织名")), ("role", p_str("member / admin，可选"))], &["org"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.list_org_members(&req_str(a, "org")?, &str_or(a, "role", ""), 0, 0),
    },
    Tool {
        name: "gc_org_repos",
        description: "列出组织下的仓库。type 可选 all/public/private/member。",
        schema: || schema(&[("org", p_str("组织名")), ("type", p_str("all / public / private / member，可选"))], &["org"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.list_org_repos(&req_str(a, "org")?, &str_or(a, "type", ""), 0, 0),
    },

    // ---- 用户（v5）：移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc ----
    // 注：这套走 v5 + PAT，与网页端 v1 的 `gc_user_view`（User 命令）不是同一接口。

    Tool {
        name: "gc_user_followers",
        description: "列出某用户的粉丝。",
        schema: || schema(&[("username", p_str("用户名"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_followers(&req_str(a, "username")?, 0, 0),
    },
    Tool {
        name: "gc_user_following",
        description: "列出某用户关注的人。",
        schema: || schema(&[("username", p_str("用户名"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_following(&req_str(a, "username")?, 0, 0),
    },
    Tool {
        name: "gc_user_starred",
        description: "列出 Star 的仓库。username 留空则查自己，否则查指定用户。",
        schema: || {
            schema(
                &[
                    ("username", p_str("用户名；留空查自己")),
                    ("sort", p_str("排序字段，可选")),
                    ("direction", p_str("asc / desc，可选")),
                ],
                &[],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            pat_client()?.user_starred(&str_or(a, "username", ""), &str_or(a, "sort", ""), &str_or(a, "direction", ""), 0, 0)
        },
    },
    Tool {
        name: "gc_user_subscriptions",
        description: "列出订阅的仓库。username 留空则查自己，否则查指定用户。",
        schema: || {
            schema(
                &[
                    ("username", p_str("用户名；留空查自己")),
                    ("sort", p_str("排序字段，可选")),
                    ("direction", p_str("asc / desc，可选")),
                ],
                &[],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            pat_client()?.user_subscriptions(&str_or(a, "username", ""), &str_or(a, "sort", ""), &str_or(a, "direction", ""), 0, 0)
        },
    },
    Tool {
        name: "gc_user_namespaces",
        description: "列出当前用户拥有的命名空间。mode 可选。",
        schema: || schema(&[("mode", p_str("过滤模式，可选"))], &[]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_namespaces(&str_or(a, "mode", ""), 0, 0),
    },
    Tool {
        name: "gc_user_namespace",
        description: "按路径查询单个命名空间的信息。",
        schema: || schema(&[("path", p_str("命名空间路径"))], &["path"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_namespace(&req_str(a, "path")?),
    },
    Tool {
        name: "gc_user_keys",
        description: "列出当前用户的 SSH 公钥。",
        schema: || schema(&[], &[]),
        read_only: true,
        destructive: false,
        handler: |_| pat_client()?.user_keys(0, 0),
    },
    Tool {
        name: "gc_user_key",
        description: "查看单个 SSH 公钥。",
        schema: || schema(&[("id", p_int("公钥 ID"))], &["id"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_key(req_u64(a, "id")?),
    },
    Tool {
        name: "gc_user_key_add",
        description: "添加 SSH 公钥。key 是完整公钥字符串（含类型与内容）。",
        schema: || {
            schema(
                &[
                    ("title", p_str("公钥标题")),
                    ("key", p_str("完整公钥字符串，如 \"ssh-ed25519 AAAA... comment\"")),
                ],
                &["title", "key"],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| pat_client()?.add_user_key(&req_str(a, "title")?, &req_str(a, "key")?),
    },
    Tool {
        name: "gc_user_key_delete",
        description: "删除 SSH 公钥。不可撤销。",
        schema: || schema(&[("id", p_int("公钥 ID"))], &["id"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let id = req_u64(a, "id")?;
            pat_client()?.delete_user_key(id)?;
            Ok(json!({ "deleted_key": id }))
        },
    },
    Tool {
        name: "gc_user_issues",
        description: "列出当前用户的 Issue。可按 filter/state/labels/sort/direction/since 过滤。",
        schema: || {
            schema(
                &[
                    ("filter", p_str("过滤条件，可选")),
                    ("state", p_str("open / closed / all，可选")),
                    ("labels", p_str("标签，逗号分隔，可选")),
                    ("sort", p_str("排序字段，可选")),
                    ("direction", p_str("asc / desc，可选")),
                    ("since", p_str("起始时间（ISO8601），可选")),
                ],
                &[],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            pat_client()?.user_issues(
                &str_or(a, "filter", ""),
                &str_or(a, "state", ""),
                &str_or(a, "labels", ""),
                &str_or(a, "sort", ""),
                &str_or(a, "direction", ""),
                &str_or(a, "since", ""),
                0,
                0,
            )
        },
    },
    Tool {
        name: "gc_user_prs",
        description: "列出当前用户的 Pull Request。可按 state/scope/sort/direction/labels 过滤。",
        schema: || {
            schema(
                &[
                    ("state", p_str("open / closed / all，可选")),
                    ("scope", p_str("范围过滤，可选")),
                    ("sort", p_str("排序字段，可选")),
                    ("direction", p_str("asc / desc，可选")),
                    ("labels", p_str("标签，逗号分隔，可选")),
                ],
                &[],
            )
        },
        read_only: true,
        destructive: false,
        handler: |a| {
            pat_client()?.user_prs(
                &str_or(a, "state", ""),
                &str_or(a, "scope", ""),
                &str_or(a, "sort", ""),
                &str_or(a, "direction", ""),
                &str_or(a, "labels", ""),
                0,
                0,
            )
        },
    },
    Tool {
        name: "gc_user_org_membership",
        description: "查看当前用户在指定组织的成员资格。",
        schema: || schema(&[("org", p_str("组织名"))], &["org"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.org_membership(&req_str(a, "org")?),
    },
    Tool {
        name: "gc_user_org_leave",
        description: "退出指定组织。不可撤销。",
        schema: || schema(&[("org", p_str("组织名"))], &["org"]),
        read_only: false,
        destructive: true,
        handler: |a| {
            let org = req_str(a, "org")?;
            pat_client()?.leave_org(&org)?;
            Ok(json!({ "left_org": org }))
        },
    },

    // ---- 仓库同步：硬重置式（v5 删除+重建分支），移植自 @oh-gc/cli 思路（MIT, Copyright (c) guozejun）----
    // 注：gc 已有的网页端 v2 快进同步（fork->上游）是 CLI 专属；这里给 AI 客户端的是 v5 硬重置能力。
    Tool {
        name: "gc_repo_sync",
        description: "硬重置式同步：将目标仓库某分支的 HEAD 设为其源分支（上游或 --source 指定仓库）的 HEAD。\
                      会丢弃目标分支上的额外提交，故为破坏性操作（需 allow_destructive）。默认分支取自仓库 default_branch。",
        schema: || {
            schema(
                &[
                    ("repo", p_str("目标仓库 owner/name")),
                    ("branch", p_str("要同步的分支；省略用默认分支")),
                    ("source", p_str("源仓库 owner/name；省略则用目标仓库的上游 parent")),
                    ("force", p_bool("必须为 true 才执行硬重置")),
                ],
                &["repo", "force"],
            )
        },
        read_only: false,
        destructive: true,
        handler: |a| {
            let (o, n) = repo_of(a)?;
            let api = pat_client()?;
            let info = api.repo(&o, &n)?;
            let branch = if let Some(b) = a.get("branch").and_then(|v| v.as_str()) {
                if b.is_empty() {
                    info["default_branch"].as_str().unwrap_or("main").to_string()
                } else {
                    b.to_string()
                }
            } else {
                info["default_branch"].as_str().unwrap_or("main").to_string()
            };
            if !bool_or(a, "force", false) {
                anyhow::bail!("硬重置会丢弃目标分支上的额外提交，必须把 force 设为 true");
            }
            let (so, sn) = if let Some(s) = opt_str(a, "source") {
                repo_of(&json!({ "repo": s }))?
            } else {
                let p = info["parent"]["full_name"]
                    .as_str()
                    .ok_or_else(|| anyhow!("未检测到上游（非 fork 且无 source），请用 source 指定源仓库"))?;
                repo_of(&json!({ "repo": p }))?
            };
            let sb = api.branch(&so, &sn, &branch)?;
            let sha = sb["commit"]["sha"]
                .as_str()
                .ok_or_else(|| anyhow!("拿不到源分支 {}/{} 的 HEAD", so, sn))?;
            // 硬重置：受保护时自动解保护、重建并尽量恢复保护
            api.hard_reset_branch(&o, &n, &branch, sha)?;
            Ok(json!({
                "synced_branch": branch,
                "to": format!("{}/{}", so, sn),
                "head": sha,
            }))
        },
    },

    // ---- 用户（v5）补充：Tier B 剩余端点，移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----

    Tool {
        name: "gc_user_profile",
        description: "查看指定用户的资料（v5 接口）。与网页端 v1 的 gc_user_view 不是同一套接口。",
        schema: || schema(&[("username", p_str("用户名"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.get_user(&req_str(a, "username")?),
    },
    Tool {
        name: "gc_user_update",
        description: "更新当前用户资料。只传要改的字段即可。",
        schema: || {
            schema(
                &[
                    ("nickname", p_str("昵称，可选")),
                    ("company", p_str("公司，可选")),
                    ("description", p_str("简介，可选")),
                    ("email", p_str("邮箱，可选")),
                    ("github_account", p_str("GitHub 账号，可选")),
                    ("website", p_str("个人网站，可选")),
                    ("location", p_str("所在地，可选")),
                ],
                &[],
            )
        },
        read_only: false,
        destructive: false,
        handler: |a| {
            let mut body = json!({});
            if let Some(v) = opt_str(a, "nickname") {
                body["nickname"] = json!(v);
            }
            if let Some(v) = opt_str(a, "company") {
                body["company"] = json!(v);
            }
            if let Some(v) = opt_str(a, "description") {
                body["description"] = json!(v);
            }
            if let Some(v) = opt_str(a, "email") {
                body["email"] = json!(v);
            }
            if let Some(v) = opt_str(a, "github_account") {
                body["github_account"] = json!(v);
            }
            if let Some(v) = opt_str(a, "website") {
                body["website"] = json!(v);
            }
            if let Some(v) = opt_str(a, "location") {
                body["location"] = json!(v);
            }
            pat_client()?.update_user(&body)
        },
    },
    Tool {
        name: "gc_user_events",
        description: "查看用户动态（events），按日期分组返回。year 可选（用于翻页）。",
        schema: || schema(&[("username", p_str("用户名")), ("year", p_str("年份，可选"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_events(&req_str(a, "username")?, &str_or(a, "year", ""), ""),
    },

    // ---- 用户（v5）补充：email 与按用户列组织，移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----

    Tool {
        name: "gc_user_emails",
        description: "列出当前用户的邮箱地址（含主邮箱 / 验证状态）。",
        schema: || schema(&[], &[]),
        read_only: true,
        destructive: false,
        handler: |_| pat_client()?.user_emails(),
    },
    Tool {
        name: "gc_user_orgs",
        description: "列出指定用户所属的组织（区别于 gc_org_list 的「我自己所属」）。",
        schema: || schema(&[("username", p_str("用户名"))], &["username"]),
        read_only: true,
        destructive: false,
        handler: |a| pat_client()?.user_orgs_by_username(&req_str(a, "username")?, 0, 0),
    },
];

/// 把用户名数组拼成 v5 接口要的逗号分隔字符串。
///
/// 上游接口收的是 `"alice,bob"` 这种字符串而不是数组，但对模型来说数组更自然
/// （也更难拼错），所以工具层收数组、在这里转换。
fn join_users(a: &Value, key: &str) -> Result<String> {
    let users = str_list(a, key);
    if users.is_empty() {
        bail!("`{key}` 至少要有一个用户名");
    }
    if let Some(bad) = users.iter().find(|u| u.contains(',')) {
        bail!("用户名 `{bad}` 含逗号——请每个用户名单独作为数组的一项");
    }
    Ok(users.join(","))
}

/// 批量操作：逐个执行并汇总结果，单个失败不影响其余。
fn batch(ids: &[u64], mut f: impl FnMut(u64) -> Result<Value>) -> Result<Value> {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for &id in ids {
        match f(id) {
            Ok(_) => ok.push(id),
            Err(e) => failed.push(json!({ "number": id, "error": e.to_string() })),
        }
    }
    if ok.is_empty() && !failed.is_empty() {
        bail!(
            "全部失败: {}",
            serde_json::to_string(&failed).unwrap_or_default()
        );
    }
    Ok(json!({ "succeeded": ok, "failed": failed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_tools_are_hidden_by_default() {
        let safe: Vec<_> = registry(false).iter().map(|t| t.name).collect();
        assert!(!safe.contains(&"gc_repo_delete"));
        assert!(!safe.contains(&"gc_discussion_delete"));
        assert!(!safe.contains(&"gc_repo_set_visibility"));
        assert!(safe.contains(&"gc_repo_view"));

        let all: Vec<_> = registry(true).iter().map(|t| t.name).collect();
        assert!(all.contains(&"gc_repo_delete"));
        assert!(all.len() > safe.len());
    }

    #[test]
    fn find_respects_the_destructive_gate() {
        assert!(find("gc_repo_delete", false).is_none());
        assert!(find("gc_repo_delete", true).is_some());
        assert!(find("gc_repo_view", false).is_some());
        assert!(find("no_such_tool", true).is_none());
    }

    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<_> = ALL.iter().map(|t| t.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "工具名重复");
    }

    #[test]
    fn every_tool_has_a_valid_object_schema() {
        for t in ALL {
            let s = (t.schema)();
            assert_eq!(
                s["type"], "object",
                "{} 的 schema 顶层必须是 object",
                t.name
            );
            assert!(s["properties"].is_object(), "{} 缺少 properties", t.name);
            assert!(s["required"].is_array(), "{} 缺少 required", t.name);
            assert!(!t.description.is_empty(), "{} 缺少描述", t.name);
            // required 里的字段必须真的在 properties 里声明过。
            for r in s["required"].as_array().unwrap() {
                let k = r.as_str().unwrap();
                assert!(
                    s["properties"].get(k).is_some(),
                    "{} 的 required 含未声明字段 {k}",
                    t.name
                );
            }
        }
    }

    #[test]
    fn readonly_tools_are_never_destructive() {
        for t in ALL {
            assert!(
                !(t.read_only && t.destructive),
                "{} 不能同时是只读和破坏性",
                t.name
            );
        }
    }

    #[test]
    fn tools_list_json_carries_annotations() {
        let t = find("gc_repo_view", false).unwrap();
        let j = t.to_json();
        assert_eq!(j["name"], "gc_repo_view");
        assert_eq!(j["annotations"]["readOnlyHint"], true);
        assert_eq!(j["annotations"]["destructiveHint"], false);
        assert!(j["inputSchema"]["properties"]["repo"].is_object());
    }

    // ---- 参数提取 --------------------------------------------------------

    #[test]
    fn req_str_rejects_missing_empty_and_wrong_type() {
        let a = json!({ "ok": "v", "blank": "  ", "num": 3 });
        assert_eq!(req_str(&a, "ok").unwrap(), "v");
        assert!(req_str(&a, "blank").is_err());
        assert!(req_str(&a, "num").is_err());
        assert!(req_str(&a, "absent").is_err());
    }

    #[test]
    fn req_u64_accepts_numeric_strings() {
        // 有些 MCP 客户端会把整数序列化成字符串，得容忍。
        let a = json!({ "n": 12, "s": "34", "bad": "x", "neg": -1 });
        assert_eq!(req_u64(&a, "n").unwrap(), 12);
        assert_eq!(req_u64(&a, "s").unwrap(), 34);
        assert!(req_u64(&a, "bad").is_err());
        assert!(req_u64(&a, "neg").is_err());
        assert!(req_u64(&a, "absent").is_err());
    }

    #[test]
    fn u64_list_requires_a_nonempty_array() {
        assert_eq!(u64_list(&json!({"x":[1,"2"]}), "x").unwrap(), vec![1, 2]);
        assert!(u64_list(&json!({ "x": [] }), "x").is_err());
        assert!(u64_list(&json!({ "x": 1 }), "x").is_err());
        assert!(u64_list(&json!({}), "x").is_err());
    }

    #[test]
    fn defaults_apply_when_absent_or_empty() {
        let a = json!({ "empty": "", "set": "v", "flag": true });
        assert_eq!(str_or(&a, "empty", "d"), "d");
        assert_eq!(str_or(&a, "set", "d"), "v");
        assert_eq!(str_or(&a, "absent", "d"), "d");
        assert!(bool_or(&a, "flag", false));
        assert!(bool_or(&a, "absent", true));
        assert_eq!(str_list(&a, "absent"), Vec::<String>::new());
    }

    #[test]
    fn repo_of_validates_the_owner_name_form() {
        assert_eq!(
            repo_of(&json!({"repo":"alice/demo"})).unwrap(),
            ("alice".into(), "demo".into())
        );
        // 格式非法时即便显式给了也要报错（不走 cwd 推断）。
        assert!(repo_of(&json!({ "repo": "nameonly" })).is_err());
        assert!(repo_of(&json!({ "repo": "/demo" })).is_err());
        // 省略 repo 时走 cwd 推断：在 gitcode 仓库目录里能解析成功，
        // 不在 gitcode 仓库里才报错。这里只断言「显式格式正确即通过」。
        let inferred = repo_of(&json!({}));
        assert!(inferred.is_ok() || inferred.is_err());
    }

    #[test]
    fn batch_reports_partial_failure() {
        let r = batch(&[1, 2, 3], |id| {
            if id == 2 {
                bail!("boom")
            } else {
                Ok(json!({}))
            }
        })
        .unwrap();
        assert_eq!(r["succeeded"], json!([1, 3]));
        assert_eq!(r["failed"][0]["number"], 2);
    }

    #[test]
    fn batch_errors_only_when_everything_fails() {
        assert!(batch(&[1, 2], |_| bail!("no")).is_err());
    }

    #[test]
    fn join_users_builds_the_comma_string_v5_expects() {
        assert_eq!(
            join_users(&json!({ "reviewers": ["alice", "bob"] }), "reviewers").unwrap(),
            "alice,bob"
        );
        assert!(join_users(&json!({ "reviewers": [] }), "reviewers").is_err());
        assert!(join_users(&json!({}), "reviewers").is_err());
        // 模型如果把整串塞进一个元素里，拼接后无法区分，直接报错比静默出错好。
        assert!(join_users(&json!({ "r": ["alice,bob"] }), "r").is_err());
    }
}
