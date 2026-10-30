use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// 网页端接口统一伪装成浏览器 —— 服务端会按 UA 拦请求。
const WEB_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                      Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0";

fn secs_from_env(key: &str, default: u64) -> Duration {
    let n = std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default);
    Duration::from_secs(n)
}

/// 统一构造 HTTP 客户端。
///
/// 必须带超时：不设的话对端不回包就永久挂起。CLI 里还能 Ctrl-C，
/// 但 `gc mcp start` 挂住就等于 AI 客户端无限等待，没法自愈。
///
/// - `GITCODE_CONNECT_TIMEOUT`（秒，默认 10）：连不上就快速失败
/// - `GITCODE_TIMEOUT`（秒，默认 60）：单次请求总时长上限
///
/// 注意总超时会把上传/下载的传输时间也算进去，大文件要按需用
/// `RequestBuilder::timeout` 单独放宽（见 [`ApiClient::upload_asset`]）。
pub fn http_client(user_agent: &str) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(user_agent.to_string())
        .connect_timeout(secs_from_env("GITCODE_CONNECT_TIMEOUT", 10))
        .timeout(secs_from_env("GITCODE_TIMEOUT", 60))
        .build()
        .context("创建 HTTP 客户端失败")
}

/// 网页端（web-api）专用客户端。
fn web_client() -> Result<reqwest::blocking::Client> {
    http_client(WEB_UA)
}

/// 大文件传输的超时上限，可用 `GITCODE_TRANSFER_TIMEOUT` 覆盖（秒，默认 1 小时）。
fn transfer_timeout() -> Duration {
    secs_from_env("GITCODE_TRANSFER_TIMEOUT", 3600)
}

/// 包装一个 Reader，每读出一段就推进进度条（用于上传进度显示）
struct ProgressReader<R> {
    inner: R,
    bar: indicatif::ProgressBar,
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bar.inc(n as u64);
        }
        Ok(n)
    }
}

/// GitCode / AtomGit (Gitee v5 兼容) API 客户端
pub struct ApiClient {
    base: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl ApiClient {
    pub fn new(base: String, token: Option<String>) -> Result<Self> {
        let client = http_client("gc/0.1 (GitCode CLI)")?;
        Ok(Self {
            base,
            token,
            client,
        })
    }

    /// 从错误文本里抹掉 access_token（reqwest 的 URL 错误会把带 token 的完整链接打出来）。
    fn redact_token(s: &str) -> String {
        // 匹配 access_token=<任意非空白、非引号、非> 字符>
        let re = regex::Regex::new(r#"access_token=[^\s"'&<>]+"#).unwrap();
        re.replace_all(s, "access_token=***").to_string()
    }

    /// 带自动重试的发送：网络类失败（连接超时/重置等）按 500ms → 1s → 2s 退避共试 3 次；
    /// 服务端已返回响应（4xx/5xx）不重试。错误文本统一脱敏 access_token。
    fn send_with_retry(
        builder: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response> {
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500 * (1 << (attempt - 1))));
            }
            let b = builder
                .try_clone()
                .ok_or_else(|| anyhow!("请求体无法克隆，无法重试"))?;
            match b.send() {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(anyhow!(
            "请求 GitCode API 失败（已重试 3 次）: {}",
            Self::redact_token(&last_err.unwrap().to_string())
        ))
    }

    /// 统一请求：自动附加 access_token，支持额外 query 与 JSON body
    pub fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        extra_q: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let token = self.token.as_deref().unwrap_or("");
        let mut builder = self
            .client
            .request(method.clone(), &url)
            .query(&[("access_token", token)]);
        if !extra_q.is_empty() {
            builder = builder.query(extra_q);
        }
        if let Some(b) = body {
            builder = builder.json(b);
        }
        let resp = Self::send_with_retry(builder)?;
        let status = resp.status();
        let text = resp.text().context("读取响应体失败")?;
        if !status.is_success() {
            return Err(anyhow!("GitCode API 错误 {}: {}", status, text));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).context("解析 JSON 响应失败")
    }

    pub fn get(&self, path: &str, q: &[(&str, &str)]) -> Result<Value> {
        self.request(reqwest::Method::GET, path, q, None)
    }

    /// 裸 API 透传（gc api 子命令）：method 任意、path 相对 v5 base（可带 query）、
    /// body 为原始 JSON 字符串。返回解析后的 JSON（空响应返回 Null）。
    pub fn raw(&self, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
        let m = reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
            .map_err(|e| anyhow!("无效的 HTTP 方法 {}: {}", method, e))?;
        let parsed_body = match body {
            Some(s) if !s.trim().is_empty() => Some(
                serde_json::from_str::<Value>(s)
                    .map_err(|e| anyhow!("-b 的内容不是合法 JSON：{}", e))?,
            ),
            _ => None,
        };
        self.request(m, path, &[], parsed_body.as_ref())
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, &[], Some(body))
    }

    pub fn patch(&self, path: &str, body: &Value) -> Result<Value> {
        self.request(reqwest::Method::PATCH, path, &[], Some(body))
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.request(reqwest::Method::PUT, path, &[], Some(body))
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::DELETE, path, &[], None)
    }

    /// 带请求体的 DELETE。v5 的取消关联 issue / 移除评审人等接口都是这种形态。
    pub fn delete_body(&self, path: &str, body: &Value) -> Result<Value> {
        self.request(reqwest::Method::DELETE, path, &[], Some(body))
    }

    /// 分页 GET：`page` / `per_page` 传 0 表示不带该参数，用服务端默认值。
    fn get_paged(
        &self,
        path: &str,
        page: u64,
        per_page: u64,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        let p = page.to_string();
        let pp = per_page.to_string();
        let mut q: Vec<(&str, &str)> = extra.to_vec();
        if page > 0 {
            q.push(("page", &p));
        }
        if per_page > 0 {
            q.push(("per_page", &pp));
        }
        self.get(path, &q)
    }

    // ---- 认证 ----
    pub fn authenticated_user(&self) -> Result<Value> {
        self.get("/user", &[])
    }

    // ---- 仓库 ----
    pub fn repo(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}", owner, name), &[])
    }

    pub fn repos(&self, owner: Option<&str>) -> Result<Value> {
        match owner {
            Some(o) => self.get(&format!("/users/{}/repos", o), &[]),
            None => self.get("/user/repos", &[]),
        }
    }

    /// 在当前用户命名空间下创建仓库（Gitee v5 兼容：POST /user/repos）
    pub fn create_repo(&self, name: &str, description: &str, private: bool) -> Result<Value> {
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "private": private,
        });
        self.post("/user/repos", &body)
    }

    /// 设置仓库可见性（v5 开放 API：PATCH /repos/{owner}/{name}）
    /// v5 是部分更新，body 仅传 private 即可。private=true 私有，false 公开。
    /// 用 PAT（access_token），与 fork 用的网页 JWT 不同源。
    pub fn set_repo_visibility(&self, owner: &str, name: &str, private: bool) -> Result<Value> {
        let body = serde_json::json!({ "private": private });
        self.patch(&format!("/repos/{}/{}", owner, name), &body)
    }

    /// Fork 仓库（v5 开放 API：POST /repos/{owner}/{repo}/forks）。
    ///
    /// 与网页端 v2 的 [`fork_repo`] 相比，这条走 PAT，且支持 `organization`——
    /// 也就是把仓库 fork 进组织命名空间（v2 接口只能 fork 到当前用户名下）。
    ///
    /// 移植自 @oh-gc/cli（作者 guozejun，MIT 许可）src/api/repos.ts 的 forkRepo。
    /// Copyright (c) guozejun. 详见仓库根目录 LICENSE.oh-gc。
    pub fn fork_repo_v5(
        &self,
        owner: &str,
        name: &str,
        organization: Option<&str>,
        new_name: Option<&str>,
        path: Option<&str>,
    ) -> Result<Value> {
        let mut body = serde_json::Map::new();
        if let Some(o) = organization {
            body.insert("organization".into(), Value::String(o.to_string()));
        }
        if let Some(n) = new_name {
            body.insert("name".into(), Value::String(n.to_string()));
        }
        if let Some(p) = path {
            body.insert("path".into(), Value::String(p.to_string()));
        }
        self.post(
            &format!("/repos/{}/{}/forks", owner, name),
            &Value::Object(body),
        )
    }

    /// 删除仓库（v5 开放 API：DELETE /repos/{owner}/{name}）。危险操作，调用方需先确认。
    /// 成功时响应体为空（返回 Value::Null）。
    pub fn delete_repo(&self, owner: &str, name: &str) -> Result<Value> {
        self.request(
            reqwest::Method::DELETE,
            &format!("/repos/{}/{}", owner, name),
            &[],
            None,
        )
    }

    // ---- 仓库设置 / 统计 / 治理 ----
    // 以下端点移植自 @oh-gc/cli（作者 guozejun，MIT 许可）src/api/repos.ts。
    // Copyright (c) guozejun. 详见仓库根目录 LICENSE.oh-gc。

    /// 更新仓库属性（PATCH /repos/{owner}/{name}）。
    /// `body` 可含 name / description / homepage / private / default_branch，部分更新。
    pub fn update_repo(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.patch(&format!("/repos/{}/{}", owner, name), body)
    }

    /// 转移仓库归属（POST /repos/{owner}/{name}/transfer）。`team_ids` 为空则不带该字段。
    pub fn transfer_repo(
        &self,
        owner: &str,
        name: &str,
        new_owner: &str,
        team_ids: &[u64],
    ) -> Result<Value> {
        let mut body = serde_json::Map::new();
        body.insert("new_owner".into(), Value::String(new_owner.to_string()));
        if !team_ids.is_empty() {
            body.insert("team_ids".into(), json!(team_ids));
        }
        self.post(
            &format!("/repos/{}/{}/transfer", owner, name),
            &Value::Object(body),
        )
    }

    /// 列出仓库的 fork（GET /repos/{owner}/{name}/forks）。`sort`: newest / oldest / stargazers。
    pub fn list_forks(
        &self,
        owner: &str,
        name: &str,
        sort: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !sort.is_empty() {
            q.push(("sort", sort));
        }
        self.get_paged(
            &format!("/repos/{}/{}/forks", owner, name),
            page,
            per_page,
            &q,
        )
    }

    /// 读取仓库高级设置（GET /repos/{owner}/{name}/repo_settings）。
    pub fn repo_settings(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/repo_settings", owner, name), &[])
    }

    /// 更新仓库高级设置（PUT /repos/{owner}/{name}/repo_settings）。
    ///
    /// 上游写的是 PATCH，但服务端实测只认 PUT（PATCH 直接 405），下面几个写接口同理。
    pub fn update_repo_settings(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.put(&format!("/repos/{}/{}/repo_settings", owner, name), body)
    }

    /// 仓库语言构成（GET /repos/{owner}/{name}/languages），返回 {语言: 字节数}。
    pub fn repo_languages(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/languages", owner, name), &[])
    }

    /// 贡献者列表（GET /repos/{owner}/{name}/contributors）。`kind`: committers / authors。
    pub fn repo_contributors(
        &self,
        owner: &str,
        name: &str,
        kind: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !kind.is_empty() {
            q.push(("type", kind));
        }
        self.get_paged(
            &format!("/repos/{}/{}/contributors", owner, name),
            page,
            per_page,
            &q,
        )
    }

    /// 贡献者统计（GET /repos/{owner}/{name}/contributors/statistic）：增删行数、提交数。
    pub fn contributor_statistics(
        &self,
        owner: &str,
        name: &str,
        author: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !author.is_empty() {
            q.push(("author", author));
        }
        self.get_paged(
            &format!("/repos/{}/{}/contributors/statistic", owner, name),
            page,
            per_page,
            &q,
        )
    }

    /// 提交统计（GET /{owner}/{name}/repository/commit_statistics）。
    ///
    /// 注意这条路径没有 `/repos` 前缀，上游就是这么写的，别顺手「修正」。
    pub fn commit_statistics(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
        author: &str,
        only_self: bool,
        since: &str,
        until: &str,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![("branch_name", branch)];
        if !author.is_empty() {
            q.push(("author", author));
        }
        if only_self {
            q.push(("only_self", "true"));
        }
        if !since.is_empty() {
            q.push(("since", since));
        }
        if !until.is_empty() {
            q.push(("until", until));
        }
        self.get(
            &format!("/{}/{}/repository/commit_statistics", owner, name),
            &q,
        )
    }

    /// 下载量统计（GET /repos/{owner}/{name}/download_statistics）。`direction`: asc / desc。
    pub fn download_statistics(
        &self,
        owner: &str,
        name: &str,
        start_date: &str,
        end_date: &str,
        direction: &str,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !start_date.is_empty() {
            q.push(("start_date", start_date));
        }
        if !end_date.is_empty() {
            q.push(("end_date", end_date));
        }
        if !direction.is_empty() {
            q.push(("direction", direction));
        }
        self.get(
            &format!("/repos/{}/{}/download_statistics", owner, name),
            &q,
        )
    }

    /// 仓库动态（GET /repos/{owner}/{name}/events）。
    /// `filter`: all / push / merged / issue / comments / team / project。
    #[allow(clippy::too_many_arguments)]
    pub fn repo_events(
        &self,
        owner: &str,
        name: &str,
        filter: &str,
        author: &str,
        before: &str,
        after: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !filter.is_empty() {
            q.push(("filter", filter));
        }
        if !author.is_empty() {
            q.push(("author", author));
        }
        if !before.is_empty() {
            q.push(("before", before));
        }
        if !after.is_empty() {
            q.push(("after", after));
        }
        self.get_paged(
            &format!("/repos/{}/{}/events", owner, name),
            page,
            per_page,
            &q,
        )
    }

    /// 归档 / 取消归档仓库（PUT /org/{org}/repo/{repo}/status）。
    /// status=2 归档、0 取消；服务端要求带账号密码二次确认。
    pub fn archive_repo(
        &self,
        org: &str,
        repo: &str,
        archived: bool,
        password: &str,
    ) -> Result<Value> {
        let body = json!({ "status": if archived { 2 } else { 0 }, "password": password });
        self.put(&format!("/org/{}/repo/{}/status", org, repo), &body)
    }

    /// 读取仓库权限继承模式（GET /repos/{owner}/{name}/transition）。
    /// 注意读回来的字段名是 `memberMgntMode`，写进去的字段名却是 `mode`。1 继承、2 独立。
    pub fn repo_transition(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/transition", owner, name), &[])
    }

    /// 设置仓库权限继承模式（PUT /repos/{owner}/{name}/transition）。
    pub fn update_repo_transition(&self, owner: &str, name: &str, mode: u8) -> Result<Value> {
        self.put(
            &format!("/repos/{}/{}/transition", owner, name),
            &json!({ "mode": mode }),
        )
    }

    /// 开关仓库功能模块（PUT /repos/{owner}/{name}/module/setting）。
    /// `body` 可含 has_wiki / has_issue / has_security / has_merge_request / has_fork /
    /// has_analysis / has_discussion。成功返回 {"msg":"success","code":1}。
    pub fn update_module_setting(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.put(&format!("/repos/{}/{}/module/setting", owner, name), body)
    }

    /// 列出仓库自定义角色（GET /repos/{owner}/{name}/customized_roles）。
    pub fn customized_roles(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/customized_roles", owner, name), &[])
    }

    // ---- Issue ----
    // ---- Pull Request ----
    pub fn pulls(&self, owner: &str, name: &str, state: &str) -> Result<Value> {
        // v5 接口 state 枚举只认 open/closed/all，归一化常见变体
        let lower = state.to_lowercase();
        let st = match lower.as_str() {
            "close" | "closed" => "closed",
            "opened" => "open",
            other => other,
        };
        self.get(
            &format!("/repos/{}/{}/pulls", owner, name),
            &[("state", st)],
        )
    }

    /// 带任意过滤参数的 PR 列表（author / assignee / reviewer / base 等）
    pub fn pulls_query(&self, owner: &str, name: &str, q: &[(&str, &str)]) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/pulls", owner, name), q)
    }

    pub fn pull(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/pulls/{}", owner, name, num), &[])
    }

    /// 递归列出仓库文件树（v5 git trees）：返回 (path, sha)（仅 blob 文件，不含目录）。
    /// 用于 repo grep 的稀疏缓存：与本地缓存 SHA 对比后只下载有变化的文件。
    pub fn repo_tree_entries(
        &self,
        owner: &str,
        name: &str,
        ref_name: &str,
    ) -> Result<Vec<(String, String)>> {
        let v = self.get(
            &format!("/repos/{}/{}/git/trees/{}", owner, name, ref_name),
            &[("recursive", "1")],
        )?;
        let mut out = Vec::new();
        if let Some(arr) = v["tree"].as_array() {
            for e in arr {
                if e["type"].as_str() == Some("blob") {
                    if let (Some(p), Some(s)) = (e["path"].as_str(), e["sha"].as_str()) {
                        out.push((p.to_string(), s.to_string()));
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn create_pull(
        &self,
        owner: &str,
        name: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<Value> {
        let b = serde_json::json!({
            "title": title,
            "body": body,
            "head": head,
            "base": base,
        });
        self.post(&format!("/repos/{}/{}/pulls", owner, name), &b)
    }

    // ---- Pull Request：全生命周期 ----
    //
    // 本段接口契约（路径、请求体字段、参数语义）移植自 @oh-gc/cli
    // <https://gitcode.com/guozejun/ohos_gitcode_cli>，MIT License，
    // Copyright (c) guozejun。完整版权声明见仓库根目录 LICENSE.oh-gc。
    //
    // 几处反直觉的地方（照抄上游实测结论，别"顺手改成 RESTful"）：
    // - 合并是 PUT 不是 POST；
    // - 标签、关联 issue 的请求体是**裸数组**，不是 {"labels": [...]} 这种包装；
    // - 评审人 / 测试人 / 指派人用逗号分隔的**字符串**，不是数组；
    // - 单条评论的路径是 /pulls/comments/{id}（没有 PR 编号），
    //   而评论的历史与表情却在 /pulls/comment/{id}/...（单数 comment）。

    fn pull_path(owner: &str, name: &str, num: u64) -> String {
        format!("/repos/{}/{}/pulls/{}", owner, name, num)
    }

    /// 合并 PR。`method` 取 merge / squash / rebase，空串表示用服务端默认。
    pub fn merge_pull(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        method: &str,
        title: &str,
        description: &str,
    ) -> Result<Value> {
        let mut b = json!({});
        if !method.is_empty() {
            b["merge_method"] = json!(method);
        }
        if !title.is_empty() {
            b["title"] = json!(title);
        }
        if !description.is_empty() {
            b["description"] = json!(description);
        }
        self.put(&format!("{}/merge", Self::pull_path(owner, name, num)), &b)
    }

    /// 局部更新 PR：`body` 里只放要改的字段（title / body / state / labels / draft
    /// / milestone_number）。
    pub fn update_pull(&self, owner: &str, name: &str, num: u64, body: &Value) -> Result<Value> {
        self.patch(&Self::pull_path(owner, name, num), body)
    }

    pub fn close_pull(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.update_pull(owner, name, num, &json!({ "state": "closed" }))
    }

    pub fn reopen_pull(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.update_pull(owner, name, num, &json!({ "state": "open" }))
    }

    /// PR 改动的文件列表（含 patch 文本）。
    pub fn pull_files(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(&format!("{}/files", Self::pull_path(owner, name, num)), &[])
    }

    pub fn pull_commits(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("{}/commits", Self::pull_path(owner, name, num)),
            page,
            per_page,
            &[],
        )
    }

    pub fn pull_comments(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("{}/comments", Self::pull_path(owner, name, num)),
            page,
            per_page,
            &[],
        )
    }

    /// 发表 PR 评论。给了 `path` + `position` 就是行级评论，否则是整体评论。
    pub fn create_pull_comment(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        body: &str,
        path: Option<&str>,
        position: Option<u64>,
    ) -> Result<Value> {
        let mut b = json!({ "body": body });
        if let Some(p) = path {
            b["path"] = json!(p);
        }
        if let Some(p) = position {
            b["position"] = json!(p);
        }
        self.post(
            &format!("{}/comments", Self::pull_path(owner, name, num)),
            &b,
        )
    }

    /// 单条评论用的是不带 PR 编号的路径。
    fn comment_path(owner: &str, name: &str, id: u64) -> String {
        format!("/repos/{}/{}/pulls/comments/{}", owner, name, id)
    }

    pub fn pull_comment(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.get(&Self::comment_path(owner, name, id), &[])
    }

    pub fn edit_pull_comment(&self, owner: &str, name: &str, id: u64, body: &str) -> Result<Value> {
        self.patch(
            &Self::comment_path(owner, name, id),
            &json!({ "body": body }),
        )
    }

    pub fn delete_pull_comment(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.delete(&Self::comment_path(owner, name, id))
    }

    /// 评论的编辑历史。注意路径是单数 `comment`，和上面的 `comments` 不是一回事。
    pub fn pull_comment_history(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.get(
            &format!(
                "/repos/{}/{}/pulls/comment/{}/modify_history",
                owner, name, id
            ),
            &[],
        )
    }

    pub fn pull_comment_reactions(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.get(
            &format!(
                "/repos/{}/{}/pulls/comment/{}/user_reactions",
                owner, name, id
            ),
            &[],
        )
    }

    /// 在某条讨论线程下回复。
    pub fn reply_pull_discussion(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        discussion_id: &str,
        body: &str,
    ) -> Result<Value> {
        self.post(
            &format!(
                "{}/discussions/{}/comments",
                Self::pull_path(owner, name, num),
                enc(discussion_id)
            ),
            &json!({ "body": body }),
        )
    }

    /// 标记讨论线程已解决 / 未解决。
    pub fn resolve_pull_discussion(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        discussion_id: &str,
        resolved: bool,
    ) -> Result<Value> {
        self.put(
            &format!(
                "{}/comments/{}",
                Self::pull_path(owner, name, num),
                enc(discussion_id)
            ),
            &json!({ "resolved": resolved }),
        )
    }

    /// 添加评审人。`reviewers` 是逗号分隔的用户名；`append=false` 会覆盖现有评审人。
    pub fn assign_pull_reviewers(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        reviewers: &str,
        append: bool,
    ) -> Result<Value> {
        self.post(
            &format!("{}/reviewers", Self::pull_path(owner, name, num)),
            &json!({ "reviewers": reviewers, "add": append }),
        )
    }

    pub fn remove_pull_reviewers(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        reviewers: &str,
    ) -> Result<Value> {
        self.delete_body(
            &format!("{}/reviewers", Self::pull_path(owner, name, num)),
            &json!({ "reviewers": reviewers }),
        )
    }

    pub fn assign_pull_testers(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        testers: &str,
        append: bool,
    ) -> Result<Value> {
        self.post(
            &format!("{}/testers", Self::pull_path(owner, name, num)),
            &json!({ "testers": testers, "add": append }),
        )
    }

    pub fn remove_pull_testers(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        testers: &str,
    ) -> Result<Value> {
        self.delete_body(
            &format!("{}/testers", Self::pull_path(owner, name, num)),
            &json!({ "testers": testers }),
        )
    }

    /// 可选评审人（候选列表）。
    pub fn pull_option_reviewers(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(
            &format!("{}/option_reviewers", Self::pull_path(owner, name, num)),
            &[],
        )
    }

    /// 提交评审通过。`force` 用于绕过未满足的评审条件。
    pub fn review_pull(&self, owner: &str, name: &str, num: u64, force: bool) -> Result<Value> {
        self.post(
            &format!("{}/review", Self::pull_path(owner, name, num)),
            &json!({ "force": force }),
        )
    }

    /// 提交测试通过。
    pub fn test_pull(&self, owner: &str, name: &str, num: u64, force: bool) -> Result<Value> {
        self.post(
            &format!("{}/test", Self::pull_path(owner, name, num)),
            &json!({ "force": force }),
        )
    }

    /// 追加标签。请求体是裸数组。
    pub fn add_pull_labels(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        labels: &[String],
    ) -> Result<Value> {
        self.post(
            &format!("{}/labels", Self::pull_path(owner, name, num)),
            &json!(labels),
        )
    }

    /// 覆盖标签（PUT 语义：传什么就只剩什么）。
    pub fn replace_pull_labels(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        labels: &[String],
    ) -> Result<Value> {
        self.put(
            &format!("{}/labels", Self::pull_path(owner, name, num)),
            &json!(labels),
        )
    }

    /// 移除标签。`names` 是逗号分隔的标签名，放在路径里。
    pub fn remove_pull_labels(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        names: &str,
    ) -> Result<Value> {
        self.delete(&format!(
            "{}/labels/{}",
            Self::pull_path(owner, name, num),
            enc(names)
        ))
    }

    pub fn pull_linked_issues(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("{}/issues", Self::pull_path(owner, name, num)),
            page,
            per_page,
            &[],
        )
    }

    /// 关联 issue。请求体是裸数组。
    pub fn link_pull_issues(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        issues: &[u64],
    ) -> Result<Value> {
        self.post(
            &format!("{}/issues", Self::pull_path(owner, name, num)),
            &json!(issues),
        )
    }

    pub fn unlink_pull_issues(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        issues: &[u64],
    ) -> Result<Value> {
        self.delete_body(
            &format!("{}/issues", Self::pull_path(owner, name, num)),
            &json!(issues),
        )
    }

    /// 指派负责人。`assignees` 是逗号分隔的用户名。
    pub fn assign_pull_assignees(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        assignees: &str,
    ) -> Result<Value> {
        self.post(
            &format!("{}/assignees", Self::pull_path(owner, name, num)),
            &json!({ "assignees": assignees }),
        )
    }

    /// 重置指派状态；`reset_all=true` 清空全部。
    pub fn reset_pull_assignees(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        reset_all: bool,
    ) -> Result<Value> {
        let b = if reset_all {
            json!({ "reset_all": true })
        } else {
            json!({})
        };
        self.patch(
            &format!("{}/assignees", Self::pull_path(owner, name, num)),
            &b,
        )
    }

    /// 移除指派人；`assignees` 为空表示移除全部。
    pub fn remove_pull_assignees(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        assignees: &str,
    ) -> Result<Value> {
        let path = format!("{}/assignees", Self::pull_path(owner, name, num));
        if assignees.is_empty() {
            self.delete(&path)
        } else {
            self.delete_body(&path, &json!({ "assignees": assignees }))
        }
    }

    pub fn pull_reactions(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(
            &format!("{}/user_reactions", Self::pull_path(owner, name, num)),
            &[],
        )
    }

    pub fn create_pull_reaction(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        content: &str,
    ) -> Result<Value> {
        self.post(
            &format!("{}/user_reactions", Self::pull_path(owner, name, num)),
            &json!({ "content": content }),
        )
    }

    pub fn pull_modify_history(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(
            &format!("{}/modify_history", Self::pull_path(owner, name, num)),
            &[],
        )
    }

    pub fn pull_operate_logs(
        &self,
        owner: &str,
        name: &str,
        num: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("{}/operate_logs", Self::pull_path(owner, name, num)),
            page,
            per_page,
            &[],
        )
    }

    pub fn pull_settings(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/pull_request_settings", owner, name),
            &[],
        )
    }

    pub fn update_pull_settings(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.put(
            &format!("/repos/{}/{}/pull_request_settings", owner, name),
            body,
        )
    }

    // ---- 发行版 ----
    pub fn releases(&self, owner: &str, name: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/releases", owner, name), &[])
    }

    pub fn release(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/releases/{}", owner, name, tag), &[])
    }

    pub fn create_release(
        &self,
        owner: &str,
        name: &str,
        tag: &str,
        rname: &str,
        body: &str,
        target: &str,
    ) -> Result<Value> {
        let mut b = serde_json::json!({
            "tag_name": tag,
            "name": rname,
            "body": body,
        });
        if !target.is_empty() {
            b["target_commitish"] = serde_json::json!(target);
        }
        self.post(&format!("/repos/{}/{}/releases", owner, name), &b)
    }

    /// 上传发行版附件（multipart，字段名 file），带真实字节级进度条
    pub fn upload_asset(
        &self,
        owner: &str,
        name: &str,
        tag: &str,
        file: &std::path::Path,
    ) -> Result<Value> {
        let url = format!(
            "{}/repos/{}/{}/releases/{}/attachments?access_token={}",
            self.base,
            owner,
            name,
            tag,
            self.token.as_deref().unwrap_or("")
        );
        let total = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        let file_name = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let bar = indicatif::ProgressBar::new(total);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                )
                .unwrap()
                .progress_chars("#>-"),
        );
        bar.set_message("上传中");
        crate::siginfo::set_status(&format!("上传 {file_name}"));
        crate::siginfo::track(bar.clone());
        // 作用域结束（含 `?` 提前返回）时把 SIGINFO 状态和进度条登记清掉
        struct ClearSiginfo;
        impl Drop for ClearSiginfo {
            fn drop(&mut self) {
                crate::siginfo::clear_status();
            }
        }
        let _clear = ClearSiginfo;
        let reader = ProgressReader {
            inner: std::fs::File::open(file).map_err(|e| anyhow!("打开文件失败: {}", e))?,
            bar: bar.clone(),
        };
        let part = reqwest::blocking::multipart::Part::reader_with_length(reader, total)
            .file_name(file_name);
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        let resp = self
            .client
            .post(&url)
            .multipart(form)
            // 客户端默认那 60 秒是给普通接口的，大文件传输得单独放宽，
            // 否则一上传就被自己的超时掐断。
            .timeout(transfer_timeout())
            .send()
            .context("上传附件失败")?;
        bar.finish_with_message("上传完成");
        let status = resp.status();
        let text = resp.text().context("读取上传响应失败")?;
        if !status.is_success() {
            return Err(anyhow!("上传失败 {}: {}", status, text));
        }
        serde_json::from_str(&text).context("解析上传响应失败")
    }

    // ---- 分支 / 提交 / 文件 ----
    //
    // 本段接口契约移植自 @oh-gc/cli
    // <https://gitcode.com/guozejun/ohos_gitcode_cli>，MIT License，
    // Copyright (c) guozejun。完整版权声明见仓库根目录 LICENSE.oh-gc。

    /// 返回纯文本响应（diff / patch / raw 文件用），不做 JSON 解析。
    fn request_text(&self, path: &str, extra_q: &[(&str, &str)]) -> Result<String> {
        let url = format!("{}{}", self.base, path);
        let token = self.token.as_deref().unwrap_or("");
        let mut builder = self
            .client
            .get(&url)
            .query(&[("access_token", token)])
            .header("Accept", "text/plain");
        if !extra_q.is_empty() {
            builder = builder.query(extra_q);
        }
        let resp = Self::send_with_retry(builder)?;
        let status = resp.status();
        let text = resp.text().context("读取响应体失败")?;
        if !status.is_success() {
            return Err(anyhow!("GitCode API 错误 {}: {}", status, text));
        }
        Ok(text)
    }

    // -- 分支 --

    pub fn branches(&self, owner: &str, name: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/branches", owner, name),
            page,
            per_page,
            &[],
        )
    }

    pub fn branch(&self, owner: &str, name: &str, branch: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/branches/{}", owner, name, enc(branch)),
            &[],
        )
    }

    /// 建分支。`refs` 是起点（分支名 / 标签 / commit SHA）。
    pub fn create_branch(
        &self,
        owner: &str,
        name: &str,
        refs: &str,
        branch_name: &str,
    ) -> Result<Value> {
        self.post(
            &format!("/repos/{}/{}/branches", owner, name),
            &json!({ "refs": refs, "branch_name": branch_name }),
        )
    }

    pub fn delete_branch(&self, owner: &str, name: &str, branch: &str) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/branches/{}",
            owner,
            name,
            enc(branch)
        ))
    }

    /// 开启分支保护。`body` 里放 pusher / merger 等策略字段。
    pub fn protect_branch(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
        body: &Value,
    ) -> Result<Value> {
        self.put(
            &format!(
                "/repos/{}/{}/branches/{}/protection",
                owner,
                name,
                enc(branch)
            ),
            body,
        )
    }

    pub fn unprotect_branch(&self, owner: &str, name: &str, branch: &str) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/branches/{}/protection",
            owner,
            name,
            enc(branch)
        ))
    }

    /// 读取单分支的保护配置（用于硬重置后尽量恢复原样）。分支未受保护时返回错误。
    pub fn branch_protection(&self, owner: &str, name: &str, branch: &str) -> Result<Value> {
        self.get(
            &format!(
                "/repos/{}/{}/branches/{}/protection",
                owner,
                name,
                enc(branch)
            ),
            &[],
        )
    }

    /// 硬重置分支到指定 SHA：删除后按 SHA 重建。
    ///
    /// 若分支受保护导致删除失败，自动解除保护再删、重建，并尽量恢复原有保护规则
    /// （恢复失败仅打印告警，不中断整体流程——避免把分支留在「未保护」状态却不提示）。
    /// 这把 `repo sync --force` 与 `repo fork --force` 的「删+建」逻辑统一收口到这里。
    pub fn hard_reset_branch(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
        sha: &str,
    ) -> Result<Value> {
        // 解保护后要还原的策略；None 表示原本就没保护（或没读到），重建后不用管
        let mut restore: Option<Value> = None;

        // 分支存在才需要删；拿不到（404 等）就直接走创建
        if self.branch(owner, name, branch).is_ok() {
            if let Err(e) = self.delete_branch(owner, name, branch) {
                // 删除失败多半是分支受保护。先抓原策略，解保护后重试删除。
                let saved = self.branch_protection(owner, name, branch).ok();
                self.unprotect_branch(owner, name, branch).map_err(|ue| {
                    anyhow!(
                        "删除分支 {} 失败（{}），自动解除保护也失败：{}",
                        branch,
                        e,
                        ue
                    )
                })?;
                restore = Some(saved.as_ref().map_or_else(|| json!({}), protect_body_from));
                self.delete_branch(owner, name, branch).map_err(|de| {
                    anyhow!(
                        "已解除分支 {} 的保护但删除仍失败：{}（原保护规则未恢复，请到网页端检查）",
                        branch,
                        de
                    )
                })?;
            }
        }

        let created = self.create_branch(owner, name, sha, branch)?;

        // 分支重建后才能重新挂保护规则；失败只告警，避免静默把分支留在未保护状态
        if let Some(cfg) = restore {
            if let Err(re) = self.protect_branch(owner, name, branch, &cfg) {
                eprintln!(
                    "⚠ 已硬重置分支 {}，但未能自动恢复其保护规则：{}（请到网页端检查）",
                    branch, re
                );
            }
        }
        Ok(created)
    }

    /// 列出通配符保护规则（和单分支保护是两套东西）。
    pub fn branch_protect_rules(
        &self,
        owner: &str,
        name: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/protect_branches", owner, name),
            page,
            per_page,
            &[],
        )
    }

    /// 新建通配符保护规则。注意是 PUT 到一个固定的 `.../setting/new` 路径。
    pub fn create_branch_protect_rule(
        &self,
        owner: &str,
        name: &str,
        wildcard: &str,
        pusher: &str,
        merger: &str,
    ) -> Result<Value> {
        self.put(
            &format!("/repos/{}/{}/branches/setting/new", owner, name),
            &json!({ "wildcard": wildcard, "pusher": pusher, "merger": merger }),
        )
    }

    pub fn update_branch_protect_rule(
        &self,
        owner: &str,
        name: &str,
        wildcard: &str,
        pusher: &str,
        merger: &str,
    ) -> Result<Value> {
        let mut b = json!({});
        if !pusher.is_empty() {
            b["pusher"] = json!(pusher);
        }
        if !merger.is_empty() {
            b["merger"] = json!(merger);
        }
        self.put(
            &format!(
                "/repos/{}/{}/branches/{}/setting",
                owner,
                name,
                enc(wildcard)
            ),
            &b,
        )
    }

    pub fn delete_branch_protect_rule(
        &self,
        owner: &str,
        name: &str,
        wildcard: &str,
    ) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/branches/{}/setting",
            owner,
            name,
            enc(wildcard)
        ))
    }

    // -- 提交 --

    /// 提交列表。`filters` 可放 sha / path / author / since / until。
    pub fn commits(
        &self,
        owner: &str,
        name: &str,
        filters: &[(&str, &str)],
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/commits", owner, name),
            page,
            per_page,
            filters,
        )
    }

    pub fn commit(&self, owner: &str, name: &str, sha: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/commits/{}", owner, name, enc(sha)),
            &[],
        )
    }

    /// 提交的 diff 文本。路径是单数 `commit`，和上面的 `commits` 不是一回事。
    pub fn commit_diff(&self, owner: &str, name: &str, sha: &str) -> Result<String> {
        self.request_text(
            &format!("/repos/{}/{}/commit/{}/diff", owner, name, enc(sha)),
            &[],
        )
    }

    pub fn commit_patch(&self, owner: &str, name: &str, sha: &str) -> Result<String> {
        self.request_text(
            &format!("/repos/{}/{}/commit/{}/patch", owner, name, enc(sha)),
            &[],
        )
    }

    pub fn commit_comments(
        &self,
        owner: &str,
        name: &str,
        r#ref: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/commits/{}/comments", owner, name, enc(r#ref)),
            page,
            per_page,
            &[],
        )
    }

    pub fn create_commit_comment(
        &self,
        owner: &str,
        name: &str,
        sha: &str,
        body: &str,
    ) -> Result<Value> {
        self.post(
            &format!("/repos/{}/{}/commits/{}/comments", owner, name, enc(sha)),
            &json!({ "body": body }),
        )
    }

    /// 对比两个引用。v5 把范围塞在路径里：`compare/{base}...{head}`。
    pub fn compare_commits(
        &self,
        owner: &str,
        name: &str,
        base: &str,
        head: &str,
    ) -> Result<Value> {
        self.get(
            &format!(
                "/repos/{}/{}/compare/{}...{}",
                owner,
                name,
                enc(base),
                enc(head)
            ),
            &[],
        )
    }

    // -- 文件（contents）--
    //
    // 与网页端的 repo_file_content 不同：这套走 v5 + PAT，能写（增删改），
    // 但读大文件不如网页端接口稳。读用哪个都行，写只能用这套。

    fn contents_path(owner: &str, name: &str, path: &str) -> String {
        format!("/repos/{}/{}/contents/{}", owner, name, enc_path(path))
    }

    /// 读文件或列目录：路径指向文件就返回文件对象（content 为 base64），
    /// 指向目录就返回条目数组。
    pub fn contents(&self, owner: &str, name: &str, path: &str, r#ref: &str) -> Result<Value> {
        let q: Vec<(&str, &str)> = if r#ref.is_empty() {
            vec![]
        } else {
            vec![("ref", r#ref)]
        };
        self.get(&Self::contents_path(owner, name, path), &q)
    }

    /// 直接取文件原文（不经 base64）。
    pub fn raw_file(&self, owner: &str, name: &str, path: &str, r#ref: &str) -> Result<String> {
        let q: Vec<(&str, &str)> = if r#ref.is_empty() {
            vec![]
        } else {
            vec![("ref", r#ref)]
        };
        self.request_text(
            &format!("/repos/{}/{}/raw/{}", owner, name, enc_path(path)),
            &q,
        )
    }

    /// 新建文件。`content` 必须是 base64。
    pub fn create_file(
        &self,
        owner: &str,
        name: &str,
        path: &str,
        message: &str,
        content_b64: &str,
        branch: &str,
    ) -> Result<Value> {
        let mut b = json!({ "message": message, "content": content_b64 });
        if !branch.is_empty() {
            b["branch"] = json!(branch);
        }
        self.put(&Self::contents_path(owner, name, path), &b)
    }

    /// 覆盖已有文件。`sha` 是该文件的当前 blob sha（用 `contents` 查），
    /// 服务端靠它做乐观锁——传错会 409，别猜。
    pub fn update_file(
        &self,
        owner: &str,
        name: &str,
        path: &str,
        message: &str,
        content_b64: &str,
        sha: &str,
        branch: &str,
    ) -> Result<Value> {
        let mut b = json!({ "message": message, "content": content_b64, "sha": sha });
        if !branch.is_empty() {
            b["branch"] = json!(branch);
        }
        self.put(&Self::contents_path(owner, name, path), &b)
    }

    pub fn delete_file(
        &self,
        owner: &str,
        name: &str,
        path: &str,
        message: &str,
        sha: &str,
        branch: &str,
    ) -> Result<Value> {
        let mut b = json!({ "message": message, "sha": sha });
        if !branch.is_empty() {
            b["branch"] = json!(branch);
        }
        self.delete_body(&Self::contents_path(owner, name, path), &b)
    }

    // ---- 仓库治理：tag / hook / label / milestone / collaborator / org ----
    //
    // 本段接口契约移植自 @oh-gc/cli
    // <https://gitcode.com/guozejun/ohos_gitcode_cli>，MIT License，
    // Copyright (c) guozejun。完整版权声明见仓库根目录 LICENSE.oh-gc。

    // -- 标签 --
    pub fn list_tags(&self, owner: &str, name: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/tags", owner, name),
            page,
            per_page,
            &[],
        )
    }

    pub fn get_tag(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/tags/{}", owner, name, enc(tag)), &[])
    }

    /// 新建标签。`tag_name` 是新标签名，`refs` 是起点（SHA / 分支 / 已有标签）。
    pub fn create_tag(
        &self,
        owner: &str,
        name: &str,
        refs: &str,
        tag_name: &str,
        message: &str,
    ) -> Result<Value> {
        let mut b = json!({ "refs": refs, "tag_name": tag_name });
        if !message.is_empty() {
            b["tag_message"] = json!(message);
        }
        self.post(&format!("/repos/{}/{}/tags", owner, name), &b)
    }

    pub fn delete_tag(&self, owner: &str, name: &str, tag_name: &str) -> Result<Value> {
        self.delete(&format!("/repos/{}/{}/tags/{}", owner, name, enc(tag_name)))
    }

    /// 把某个标签设为受保护（只能删保护规则，不能删标签本身）。
    pub fn protect_tag(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.put(
            &format!("/repos/{}/{}/tags/{}/protection", owner, name, enc(tag)),
            &json!({}),
        )
    }

    pub fn unprotect_tag(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/tags/{}/protection",
            owner,
            name,
            enc(tag)
        ))
    }

    pub fn list_protected_tags(
        &self,
        owner: &str,
        name: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/protected_tags", owner, name),
            page,
            per_page,
            &[],
        )
    }

    pub fn get_protected_tag(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/protected_tags/{}", owner, name, enc(tag)),
            &[],
        )
    }

    /// 新建受保护标签规则。`access_level` 为 0 表示不指定级别。
    pub fn create_protected_tag(
        &self,
        owner: &str,
        name: &str,
        tag: &str,
        access_level: u64,
    ) -> Result<Value> {
        let mut b = json!({ "name": tag });
        if access_level > 0 {
            b["create_access_level"] = json!(access_level);
        }
        self.post(&format!("/repos/{}/{}/protected_tags", owner, name), &b)
    }

    pub fn update_protected_tag(
        &self,
        owner: &str,
        name: &str,
        tag: &str,
        access_level: u64,
    ) -> Result<Value> {
        self.put(
            &format!("/repos/{}/{}/protected_tags", owner, name),
            &json!({ "name": tag, "create_access_level": access_level }),
        )
    }

    pub fn delete_protected_tag(&self, owner: &str, name: &str, tag: &str) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/protected_tags/{}",
            owner,
            name,
            enc(tag)
        ))
    }

    // -- Webhook --
    pub fn list_hooks(&self, owner: &str, name: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/hooks", owner, name),
            page,
            per_page,
            &[],
        )
    }

    pub fn get_hook(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/hooks/{}", owner, name, id), &[])
    }

    /// `body` 形如 {"name":..,"config":{"url":..,"content_type":..,"secret":..},"events":[..],"active":..}
    pub fn create_hook(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.post(&format!("/repos/{}/{}/hooks", owner, name), body)
    }

    pub fn update_hook(&self, owner: &str, name: &str, id: u64, body: &Value) -> Result<Value> {
        self.patch(&format!("/repos/{}/{}/hooks/{}", owner, name, id), body)
    }

    pub fn delete_hook(&self, owner: &str, name: &str, id: u64) -> Result<Value> {
        self.delete(&format!("/repos/{}/{}/hooks/{}", owner, name, id))
    }

    // -- 标签（issue/PR 用的 label）--
    pub fn list_labels(&self, owner: &str, name: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/repos/{}/{}/labels", owner, name),
            page,
            per_page,
            &[],
        )
    }

    pub fn get_label(&self, owner: &str, name: &str, label: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/labels/{}", owner, name, enc(label)),
            &[],
        )
    }

    /// `body`: {"name":..,"color":..,"description":..}
    pub fn create_label(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.post(&format!("/repos/{}/{}/labels", owner, name), body)
    }

    pub fn update_label(
        &self,
        owner: &str,
        name: &str,
        label: &str,
        body: &Value,
    ) -> Result<Value> {
        self.patch(
            &format!("/repos/{}/{}/labels/{}", owner, name, enc(label)),
            body,
        )
    }

    pub fn delete_label(&self, owner: &str, name: &str, label: &str) -> Result<Value> {
        self.delete(&format!("/repos/{}/{}/labels/{}", owner, name, enc(label)))
    }

    // -- 里程碑 --
    pub fn list_milestones(
        &self,
        owner: &str,
        name: &str,
        state: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let q: Vec<(&str, &str)> = if state.is_empty() {
            vec![]
        } else {
            vec![("state", state)]
        };
        self.get_paged(
            &format!("/repos/{}/{}/milestones", owner, name),
            page,
            per_page,
            &q,
        )
    }

    pub fn get_milestone(&self, owner: &str, name: &str, number: u64) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/milestones/{}", owner, name, number),
            &[],
        )
    }

    /// `body`: {"title":..,"state":..,"description":..,"due_on":..}
    pub fn create_milestone(&self, owner: &str, name: &str, body: &Value) -> Result<Value> {
        self.post(&format!("/repos/{}/{}/milestones", owner, name), body)
    }

    pub fn update_milestone(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        body: &Value,
    ) -> Result<Value> {
        self.patch(
            &format!("/repos/{}/{}/milestones/{}", owner, name, number),
            body,
        )
    }

    pub fn delete_milestone(&self, owner: &str, name: &str, number: u64) -> Result<Value> {
        self.delete(&format!("/repos/{}/{}/milestones/{}", owner, name, number))
    }

    // -- 协作者 --
    pub fn list_collaborators(
        &self,
        owner: &str,
        name: &str,
        affiliation: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let q: Vec<(&str, &str)> = if affiliation.is_empty() {
            vec![]
        } else {
            vec![("affiliation", affiliation)]
        };
        self.get_paged(
            &format!("/repos/{}/{}/collaborators", owner, name),
            page,
            per_page,
            &q,
        )
    }

    /// `permission` 为 "" 时不传级别。
    pub fn add_collaborator(
        &self,
        owner: &str,
        name: &str,
        username: &str,
        permission: &str,
    ) -> Result<Value> {
        let mut b = json!({});
        if !permission.is_empty() {
            b["permission"] = json!(permission);
        }
        self.put(
            &format!("/repos/{}/{}/collaborators/{}", owner, name, enc(username)),
            &b,
        )
    }

    pub fn remove_collaborator(&self, owner: &str, name: &str, username: &str) -> Result<Value> {
        self.delete(&format!(
            "/repos/{}/{}/collaborators/{}",
            owner,
            name,
            enc(username)
        ))
    }

    pub fn collaborator_permission(
        &self,
        owner: &str,
        name: &str,
        username: &str,
    ) -> Result<Value> {
        self.get(
            &format!(
                "/repos/{}/{}/collaborators/{}/permission",
                owner,
                name,
                enc(username)
            ),
            &[],
        )
    }

    // -- 组织 --
    pub fn list_orgs(&self, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged("/user/orgs", page, per_page, &[])
    }

    pub fn get_org(&self, org: &str) -> Result<Value> {
        self.get(&format!("/orgs/{}", enc(org)), &[])
    }

    pub fn list_org_members(
        &self,
        org: &str,
        role: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let q: Vec<(&str, &str)> = if role.is_empty() {
            vec![]
        } else {
            vec![("role", role)]
        };
        self.get_paged(&format!("/orgs/{}/members", enc(org)), page, per_page, &q)
    }

    pub fn list_org_repos(
        &self,
        org: &str,
        r#type: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let q: Vec<(&str, &str)> = if r#type.is_empty() {
            vec![]
        } else {
            vec![("type", r#type)]
        };
        self.get_paged(&format!("/orgs/{}/repos", enc(org)), page, per_page, &q)
    }

    // ---- 用户（v5）：移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc。
    // 注意：本段走 v5 + PAT（与网页端 v1 的 `User` 命令不是同一套接口）。

    pub fn user_followers(&self, username: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/users/{}/followers", enc(username)),
            page,
            per_page,
            &[],
        )
    }

    pub fn user_following(&self, username: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/users/{}/following", enc(username)),
            page,
            per_page,
            &[],
        )
    }

    /// `username` 为空时查「我 Star 的仓库」，否则查指定用户的。
    pub fn user_starred(
        &self,
        username: &str,
        sort: &str,
        direction: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !sort.is_empty() {
            q.push(("sort", sort));
        }
        if !direction.is_empty() {
            q.push(("direction", direction));
        }
        if username.is_empty() {
            self.get_paged("/user/starred", page, per_page, &q)
        } else {
            self.get_paged(
                &format!("/users/{}/starred", enc(username)),
                page,
                per_page,
                &q,
            )
        }
    }

    /// 与 `user_starred` 同理，区分「我订阅的」与「某人订阅的」。
    pub fn user_subscriptions(
        &self,
        username: &str,
        sort: &str,
        direction: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !sort.is_empty() {
            q.push(("sort", sort));
        }
        if !direction.is_empty() {
            q.push(("direction", direction));
        }
        if username.is_empty() {
            self.get_paged("/user/subscriptions", page, per_page, &q)
        } else {
            self.get_paged(
                &format!("/users/{}/subscriptions", enc(username)),
                page,
                per_page,
                &q,
            )
        }
    }

    pub fn user_namespaces(&self, mode: &str, page: u64, per_page: u64) -> Result<Value> {
        let q: Vec<(&str, &str)> = if mode.is_empty() {
            vec![]
        } else {
            vec![("mode", mode)]
        };
        self.get_paged("/user/namespaces", page, per_page, &q)
    }

    pub fn user_namespace(&self, path: &str) -> Result<Value> {
        self.get("/user/namespace", &[("path", path)])
    }

    pub fn user_keys(&self, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged("/user/keys", page, per_page, &[])
    }

    pub fn user_key(&self, id: u64) -> Result<Value> {
        self.get(&format!("/user/keys/{}", id), &[])
    }

    /// 添加 SSH 公钥。`key` 是完整的公钥字符串（含类型与内容）。
    pub fn add_user_key(&self, title: &str, key: &str) -> Result<Value> {
        self.post("/user/keys", &json!({ "title": title, "key": key }))
    }

    pub fn delete_user_key(&self, id: u64) -> Result<Value> {
        self.delete(&format!("/user/keys/{}", id))
    }

    pub fn user_issues(
        &self,
        filter: &str,
        state: &str,
        labels: &str,
        sort: &str,
        direction: &str,
        since: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !filter.is_empty() {
            q.push(("filter", filter));
        }
        if !state.is_empty() {
            q.push(("state", state));
        }
        if !labels.is_empty() {
            q.push(("labels", labels));
        }
        if !sort.is_empty() {
            q.push(("sort", sort));
        }
        if !direction.is_empty() {
            q.push(("direction", direction));
        }
        if !since.is_empty() {
            q.push(("since", since));
        }
        self.get_paged("/user/issues", page, per_page, &q)
    }

    pub fn user_prs(
        &self,
        state: &str,
        scope: &str,
        sort: &str,
        direction: &str,
        labels: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !state.is_empty() {
            q.push(("state", state));
        }
        if !scope.is_empty() {
            q.push(("scope", scope));
        }
        if !sort.is_empty() {
            q.push(("sort", sort));
        }
        if !direction.is_empty() {
            q.push(("direction", direction));
        }
        if !labels.is_empty() {
            q.push(("labels", labels));
        }
        self.get_paged("/user/pulls", page, per_page, &q)
    }

    pub fn org_membership(&self, org: &str) -> Result<Value> {
        self.get(&format!("/user/memberships/orgs/{}", enc(org)), &[])
    }

    pub fn leave_org(&self, org: &str) -> Result<Value> {
        self.delete(&format!("/user/memberships/orgs/{}", enc(org)))
    }

    // ---- 用户（v5）补充：Tier B 剩余端点 ----
    // 以下三个移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc。

    /// 查看指定用户的资料（v5）。与网页端 v1 的 `User::View` 不是同一接口。
    pub fn get_user(&self, username: &str) -> Result<Value> {
        self.get(&format!("/users/{}", enc(username)), &[])
    }

    /// 更新当前用户资料。`body` 里的字段都可省略（只传要改的）。
    /// 上游收：nickname / company / description / email / github_account / website / location / avatar。
    pub fn update_user(&self, body: &Value) -> Result<Value> {
        self.put("/user", body)
    }

    /// 查看用户动态（events）。返回按日期分组的事件对象；`year`/`next` 用于翻页。
    pub fn user_events(&self, username: &str, year: &str, next: &str) -> Result<Value> {
        let mut q: Vec<(&str, &str)> = vec![];
        if !year.is_empty() {
            q.push(("year", year));
        }
        if !next.is_empty() {
            q.push(("next", next));
        }
        self.get(&format!("/users/{}/events", enc(username)), &q)
    }

    // ---- 用户（v5）补充：email 与按用户列组织 ----
    // 以下两个移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc。

    /// 列出当前用户的邮箱地址。
    pub fn user_emails(&self) -> Result<Value> {
        self.get("/emails", &[])
    }

    /// 列出指定用户所属的组织（区别于 `list_orgs` 的「我自己所属」）。
    pub fn user_orgs_by_username(&self, username: &str, page: u64, per_page: u64) -> Result<Value> {
        self.get_paged(
            &format!("/users/{}/orgs", enc(username)),
            page,
            per_page,
            &[],
        )
    }

    // ---- 搜索 ----
    pub fn search_repos(&self, q: &str) -> Result<Value> {
        self.get("/search/repositories", &[("q", q)])
    }

    pub fn search_users(&self, q: &str) -> Result<Value> {
        self.get("/search/users", &[("q", q)])
    }

    pub fn search_issues(&self, q: &str) -> Result<Value> {
        self.get("/search/issues", &[("q", q)])
    }
}

// ---- 讨论（Discussions）：网页端 v1 接口 ----
// 注意：该接口受 WAF 保护，PAT 会被拦截（HTTP 418），必须用浏览器会话 JWT（Bearer），
// 且需带浏览器头。JWT 从环境变量 GITCODE_WEB_TOKEN 读取（每日过期，需从浏览器
// cookie 的 GITCODE_ACCESS_TOKEN 拷贝）。下面的字段名是逆向推断的，可能需要按真实
// 请求体校准。

/// 对 URL query 参数值做百分号编码：非 unreserved 字符（含 / & = 空格 中文等）全部编码。
/// 用于 file_path / ref 等参数，避免 "&" 被当成 query 分隔符、中文/特殊字符导致 404。
fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// 对仓库内路径做编码，但保留 `/` 作为层级分隔符。
///
/// `contents/{path}` 里的斜杠是路径结构的一部分，整串用 [`enc`] 会把它编成
/// `%2F`，服务端就只能看到一个名字里带斜杠的文件，必然 404。
fn enc_path(s: &str) -> String {
    s.split('/').map(enc).collect::<Vec<_>>().join("/")
}

/// 把「读取分支保护」的响应挑成「设置分支保护」能接受的请求体。
///
/// GET 返回的字段比 PUT 能收的多得多（wildcard、时间戳、嵌套对象等），
/// 原样回灌会被服务端拒；PUT 实际只认 pusher / merger 两个策略字符串。
fn protect_body_from(saved: &Value) -> Value {
    let mut b = serde_json::Map::new();
    for k in ["pusher", "merger"] {
        if let Some(v) = saved.get(k).and_then(Value::as_str) {
            b.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    Value::Object(b)
}

/// 讨论接口基址，可用 GITCODE_WEB_BASE 覆盖（默认网页端）
pub fn web_base() -> String {
    std::env::var("GITCODE_WEB_BASE").unwrap_or_else(|_| "https://web-api.gitcode.com".to_string())
}

/// 全站搜索支持的类型。
///
/// 服务端对无法识别的 `type`（pr / org / model / dataset 等）不会报错，而是
/// 静默按 repo 处理并返回一模一样的结果，所以这里必须显式限制取值，
/// 否则调用方会拿到看似成功、实则答非所问的数据。
pub const SEARCH_KINDS: &[&str] = &["repo", "user", "issue"];

/// 每页条数上限。
///
/// 服务端的分页有坑：`pp` 在 21..=50 时只返回 20 条，但 `page_count` 仍按 `pp`
/// 计算，照着翻页会静默漏数据；`pp > 50` 直接返回空。20 是唯一诚实的取值。
pub const SEARCH_MAX_PER_PAGE: u32 = 20;

/// 网页端全站搜索（GET /api/v1/search/nauth/query）。
///
/// 路径里的 `nauth` 就是字面意思——这个接口**不需要任何凭据**，
/// 是 gc 里唯一一个匿名可用的能力。
pub fn web_search(query: &str, kind: &str, page: u32, per_page: u32) -> Result<Value> {
    if query.trim().is_empty() {
        return Err(anyhow!("搜索关键词不能为空"));
    }
    if !SEARCH_KINDS.contains(&kind) {
        return Err(anyhow!(
            "不支持的搜索类型 `{}`，可选：{}",
            kind,
            SEARCH_KINDS.join(" / ")
        ));
    }

    let client = web_client()?;

    // o / repo_type 是网页端固定发的：实测 o 换任何值结果都不变，
    // repo_type 只有 0 有数据，所以不对外暴露成参数。
    let url = format!(
        "{}/api/v1/search/nauth/query?q={}&type={}&p={}&pp={}&o=desc&repo_type=0",
        web_base().trim_end_matches('/'),
        enc(query),
        kind,
        page.max(1),
        per_page.clamp(1, SEARCH_MAX_PER_PAGE),
    );

    let resp = client
        .get(&url)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("Accept", "application/json")
        .header("X-Platform", "web")
        .send()
        .context("请求 GitCode 搜索接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 搜索接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析搜索响应失败")
}

/// 把搜索响应压成精简结构。
///
/// 原始条目有 40+ 个字段且绝大多数是 null，直接丢给模型既费 token 又难读；
/// CLI 打印要的也正是这几个字段，所以两边共用。纯函数，便于测试。
pub fn search_digest(raw: &Value, kind: &str) -> Value {
    let items: Vec<Value> = raw["content"]
        .as_array()
        .map(|a| a.iter().map(|c| digest_one(c, kind)).collect())
        .unwrap_or_default();
    json!({
        "kind": kind,
        "total": raw["total"].as_u64().unwrap_or(0),
        "page": raw["page_num"].as_u64().unwrap_or(1),
        "page_count": raw["page_count"].as_u64().unwrap_or(0),
        "count": items.len(),
        "items": items,
    })
}

fn digest_one(c: &Value, kind: &str) -> Value {
    // 空字符串和 null 一律省掉，别给模型喂噪音。
    let s = |k: &str| c[k].as_str().filter(|v| !v.trim().is_empty());
    match kind {
        "user" => json!({
            "username": s("username"),
            "nickname": s("nickname"),
            "url": s("url"),
            "created_at": s("created_at"),
        }),
        "issue" => json!({
            "repo": s("path_with_namespace"),
            "iid": c["iid"].as_u64(),
            "title": s("title"),
            "state": s("state"),
            "author": c["author"]["username"].as_str(),
            "url": s("url"),
            "updated_at": s("updated_at"),
        }),
        // repo（也是兜底）
        _ => json!({
            "repo": s("path_with_namespace"),
            "description": s("description").or_else(|| s("description_cn")),
            "language": s("main_language"),
            "stars": c["star_count"].as_u64(),
            "forks": c["forks_count"].as_u64(),
            "default_branch": s("default_branches"),
            "url": s("web_url"),
            "last_activity_at": s("last_activity_at"),
        }),
    }
}

/// 在仓库讨论区创建一条讨论（POST /api/v1/discuss）
/// 该接口读取 JSON body（蛇形字段）：title / md_content / source_id / source_type / category_id。
/// 成功时返回新建讨论的数字 ID（如 `3`）。
pub fn create_discussion(
    token: &str,
    source_id: u64,
    source_type: u64,
    category_id: &str,
    title: &str,
    md_content: &str,
) -> Result<Value> {
    let client = web_client()?;
    let body = serde_json::json!({
        "title": title,
        "md_content": md_content,
        "source_id": source_id,
        "source_type": source_type,
        "category_id": category_id,
    });
    let url = format!("{}/api/v1/discuss", web_base().trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .json(&body)
        .send()
        .context("请求 GitCode 讨论接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 讨论接口错误 {}: {}", status, text));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).context("解析 JSON 响应失败")
}

/// 删除一条讨论（DELETE /api/v1/discuss）
/// 注意：id 是讨论的 hash 字符串（取自讨论页 URL 中 /discussions/<hash> 的 hash），
/// 不是创建时返回的数字 ID。
pub fn delete_discussion(token: &str, id: &str) -> Result<Value> {
    let client = web_client()?;
    let body = serde_json::json!({ "id": id });
    let url = format!("{}/api/v1/discuss", web_base().trim_end_matches('/'));
    let resp = client
        .delete(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .json(&body)
        .send()
        .context("请求 GitCode 讨论接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 讨论接口错误 {}: {}", status, text));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).context("解析 JSON 响应失败")
}

/// 列出仓库文件（网页端 v2 接口：GET /api/v2/projects/{owner%2Fname}/repository/file-list）
/// 注意：该接口受 WAF 保护，需用浏览器会话 JWT（Bearer）+ 浏览器头，且 ref_name 必填
/// （不传值会 418）。repoId 参数需对 owner/name 做「二次」百分号编码（/ -> %252F）。
/// 返回扁平的文件路径列表（含子目录文件，如 "src/api.rs"），由调用方组装成树。
/// 该接口分页，这里会自动翻页拉全量。
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct FileList {
    #[serde(default)]
    page_num: u32,
    #[serde(default)]
    page_size: u32,
    #[serde(default)]
    page_count: u32,
    #[serde(default)]
    content: Vec<String>,
}

pub fn repo_file_list(token: &str, owner: &str, name: &str, ref_name: &str) -> Result<Vec<String>> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let repo_id = repo_path.replace('/', "%252F"); // owner%252Fname（repoId 参数，二次编码）
    let base = format!(
        "{}/api/v2/projects/{}/repository/file-list?repoId={}&ref_name={}",
        web_base().trim_end_matches('/'),
        path_seg,
        repo_id,
        enc(ref_name),
    );
    let mut all = Vec::new();
    let mut page = 1u32;
    loop {
        // 这个接口认的是 page / per_page；写成 page_num / page_size 会被静默忽略，
        // 服务端永远回第一页（还照抄 page_count），据它翻页就是死循环。
        let url = format!("{}&page={}&per_page=100", base, page);
        let resp = client
            .get(&url)
            .bearer_auth(token)
            .header("Origin", "https://gitcode.com")
            .header("Referer", "https://gitcode.com/")
            .header("Accept", "*/*")
            .header("X-Platform", "web")
            .send()
            .context("请求 GitCode 文件列表接口失败")?;
        let status = resp.status();
        let text = resp.text().context("读取响应体失败")?;
        if !status.is_success() {
            return Err(anyhow!("GitCode 文件列表接口错误 {}: {}", status, text));
        }
        if text.trim().is_empty() {
            break;
        }
        let v: FileList = serde_json::from_str(&text).context("解析文件列表响应失败")?;
        let got = v.content.len();
        all.extend(v.content);
        // 用本地的 page 计数判断终止，不信服务端回显的 page_num——它一旦不回显或回显
        // 成常数就会卡死。空页也直接收尾，双保险。
        if got == 0 || v.page_count == 0 || page >= v.page_count {
            break;
        }
        page += 1;
    }
    Ok(all)
}

/// 按 root 过滤子树，并去掉 root 前缀（root="" 表示整库）。纯函数，便于测试。
fn filter_subtree(files: &[String], root: &str) -> Vec<String> {
    let root = root.trim_matches('/');
    if root.is_empty() {
        return files.to_vec();
    }
    let prefix = format!("{}/", root);
    files
        .iter()
        .filter(|p| *p == root || p.starts_with(&prefix))
        .map(|p| {
            if *p == root {
                p.clone()
            } else {
                p[prefix.len()..].to_string()
            }
        })
        .collect()
}

/// 通过网页端 v2 接口"克隆"整个目录树（不需要 git）：
/// 1) 用 file-list 拿到扁平文件列表；2) 按 root 过滤子树；3) 多线程流式下载到 out_dir。
/// 返回 (成功数, 失败数)。
pub fn download_tree(
    token: &str,
    owner: &str,
    name: &str,
    r#ref: &str,
    root: &str,
    out_dir: &std::path::Path,
) -> Result<(usize, usize, usize)> {
    let files = repo_file_list(token, owner, name, r#ref)?;
    let root = root.trim_matches('/');
    let local_paths = filter_subtree(&files, root);
    if local_paths.is_empty() {
        return Ok((0, 0, 0));
    }
    // (api_path, local_rel_path)：子树克隆时 api 用完整路径，写盘用去掉 root 前缀的路径
    let pairs: Vec<(String, String)> = local_paths
        .iter()
        .map(|local| {
            let api = if root.is_empty() {
                local.clone()
            } else {
                format!("{}/{}", root, local)
            };
            (api, local.clone())
        })
        .collect();
    let results = download_files(token, owner, name, r#ref, &pairs, out_dir)?;
    let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
    let fail = results.len() - ok;
    let limited = results
        .iter()
        .filter(|(_, r)| matches!(r, Ok((_, true))))
        .count();
    Ok((ok, fail, limited))
}

/// 用户资料（网页端 v1 接口 /uc/api/v1/user/setting/profile?username=）
#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct UserProfile {
    #[serde(default)]
    pub profile: UserProfileInner,
    pub nickname: String,
    pub avatar: String,
    pub user_id: String,
    pub username: String,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub github_account: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Default)]
pub struct UserProfileInner {
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}

/// 查看用户资料（网页端 v1 接口：GET /uc/api/v1/user/setting/profile?username=）
/// 需浏览器会话 JWT（Bearer）+ 浏览器头。
pub fn user_profile(token: &str, username: &str) -> Result<UserProfile> {
    let client = web_client()?;
    let url = format!(
        "{}/uc/api/v1/user/setting/profile?username={}",
        web_base().trim_end_matches('/'),
        username
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("Accept", "application/json, text/plain, */*")
        .header("X-Platform", "web")
        .send()
        .context("请求 GitCode 用户资料接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 用户资料接口错误 {}: {}", status, text));
    }
    serde_json::from_str::<UserProfile>(&text).context("解析用户资料响应失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_rejects_bad_kind_before_any_request() {
        // 校验必须发生在联网之前，否则离线时报的错会答非所问。
        let e = web_search("vlc", "pr", 1, 10).unwrap_err().to_string();
        assert!(e.contains("pr"), "{e}");
        assert!(e.contains("repo / user / issue"), "{e}");
    }

    #[test]
    fn web_search_rejects_empty_query() {
        assert!(web_search("   ", "repo", 1, 10).is_err());
    }

    #[test]
    fn search_digest_projects_repo_fields() {
        let raw = json!({
            "total": 204, "page_num": 2, "page_count": 11,
            "content": [{
                "path_with_namespace": "gh_mirrors/vl/vlc",
                "description": "VLC media player",
                "description_cn": "中文简介",
                "main_language": "C",
                "star_count": 3, "forks_count": 1,
                "default_branches": "master",
                "web_url": "https://gitcode.com/gh_mirrors/vl/vlc",
                "last_activity_at": "2025-12-25T17:49:09.941+08:00",
                "hl_name": ["<span style=\"color:red\">vlc</span>"],
            }],
        });
        let d = search_digest(&raw, "repo");
        assert_eq!(d["total"], 204);
        assert_eq!(d["page"], 2);
        assert_eq!(d["page_count"], 11);
        assert_eq!(d["count"], 1);
        let it = &d["items"][0];
        assert_eq!(it["repo"], "gh_mirrors/vl/vlc");
        assert_eq!(it["description"], "VLC media player");
        assert_eq!(it["stars"], 3);
        assert_eq!(it["default_branch"], "master");
        // 带 HTML 高亮标记的字段不该出现在精简结果里。
        assert!(it.get("hl_name").is_none());
    }

    #[test]
    fn search_digest_falls_back_to_chinese_description() {
        let raw = json!({ "content": [{
            "path_with_namespace": "a/b",
            "description": "",
            "description_cn": "中文简介",
        }]});
        assert_eq!(
            search_digest(&raw, "repo")["items"][0]["description"],
            "中文简介"
        );
    }

    #[test]
    fn search_digest_projects_user_and_issue() {
        let u = json!({ "content": [{
            "username": "gcw_VuGo2vLc", "nickname": "小明",
            "url": "https://gitcode.com/gcw_VuGo2vLc", "created_at": "2025-04-20 16:19:51",
        }]});
        let it = &search_digest(&u, "user")["items"][0];
        assert_eq!(it["username"], "gcw_VuGo2vLc");
        assert_eq!(it["nickname"], "小明");

        let i = json!({ "content": [{
            "path_with_namespace": "org/repo", "iid": 2466, "title": "add ohos_vlc",
            "state": "closed", "author": { "username": "hu-xiao-" },
            "url": "https://gitcode.com/org/repo/issues/2466",
        }]});
        let it = &search_digest(&i, "issue")["items"][0];
        assert_eq!(it["iid"], 2466);
        assert_eq!(it["state"], "closed");
        assert_eq!(it["author"], "hu-xiao-");
    }

    #[test]
    fn search_digest_survives_missing_and_empty_content() {
        for raw in [
            json!({}),
            json!({ "content": [] }),
            json!({ "content": null }),
        ] {
            let d = search_digest(&raw, "repo");
            assert_eq!(d["count"], 0);
            assert_eq!(d["total"], 0);
            assert_eq!(d["items"].as_array().unwrap().len(), 0);
        }
    }

    #[test]
    fn enc_path_keeps_slashes_but_escapes_the_rest() {
        // 斜杠必须原样保留，否则 contents/{path} 会被当成单个文件名。
        assert_eq!(enc_path("src/api.rs"), "src/api%2Ers");
        assert_eq!(
            enc_path("docs/我的 文档.md"),
            "docs/%E6%88%91%E7%9A%84%20%E6%96%87%E6%A1%A3%2Emd"
        );
        // 整串编码是错的写法，这里确认两者确实不同。
        assert_ne!(enc_path("a/b"), enc("a/b"));
    }

    #[test]
    fn test_filter_subtree_full() {
        let files = vec![
            "a.txt".to_string(),
            "src/b.rs".to_string(),
            "src/x/c.rs".to_string(),
        ];
        assert_eq!(filter_subtree(&files, ""), files);
    }

    #[test]
    fn test_filter_subtree_subdir() {
        let files = vec![
            "a.txt".to_string(),
            "src/b.rs".to_string(),
            "src/x/c.rs".to_string(),
            "other/d.rs".to_string(),
        ];
        let got = filter_subtree(&files, "src");
        assert_eq!(got, vec!["b.rs".to_string(), "x/c.rs".to_string()]);
    }

    #[test]
    fn test_filter_subtree_root_file() {
        let files = vec!["README.md".to_string(), "src/a.rs".to_string()];
        assert_eq!(
            filter_subtree(&files, "README.md"),
            vec!["README.md".to_string()]
        );
    }

    #[test]
    fn test_parse_user_profile() {
        let json = r#"{"profile":{"website":null,"description":null,"company":null,"location":null},"nickname":"CNRio","avatar":"https://cdn-img.gitcode.com/x.png","user_id":"66ef1818ea29f30978f8509d","username":"dinofish","update_time":"2025-12-01T16:43:12.000+00:00","github_account":""}"#;
        let p: UserProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.username, "dinofish");
        assert_eq!(p.nickname, "CNRio");
        assert_eq!(p.avatar, "https://cdn-img.gitcode.com/x.png");
        assert!(p.profile.description.is_none());
    }
}

/// 读取单个文件内容（网页端 v2 接口：GET /api/v2/projects/{owner%2Fname}/repository/files）
/// 同样受 WAF 保护，需浏览器会话 JWT（Bearer）+ 浏览器头。
/// 参数：ref（分支/标签）、file_path（仓库内路径，可含 /，不会像 path= 那样触发 WAF）、
/// repoId（二次编码）。返回的 content 为 base64（encoding 字段为 "base64"）。
#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
pub struct WebFile {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub encoding: String,
    #[serde(default)]
    pub file_type: Option<String>,
    pub content: String,
    #[serde(default)]
    pub is_limited: Option<bool>,
}

pub fn repo_file_content(
    token: &str,
    owner: &str,
    name: &str,
    r#ref: &str,
    file_path: &str,
) -> Result<WebFile> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname
    let repo_id = repo_path.replace('/', "%252F"); // owner%252Fname（二次编码）
    let url = format!(
        "{}/api/v2/projects/{}/repository/files?repoId={}&ref={}&file_path={}",
        web_base().trim_end_matches('/'),
        path_seg,
        repo_id,
        enc(r#ref),
        enc(file_path),
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("Accept", "*/*")
        .header("X-Platform", "web")
        .send()
        .context("请求 GitCode 文件内容接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 文件内容接口错误 {}: {}", status, text));
    }
    serde_json::from_str::<WebFile>(&text).context("解析文件内容响应失败")
}

/// 多线程流式下载多个文件到本地目录（网页端 v2 /repository/files 接口）。
/// 每个文件一个线程，HTTP 响应体通过 serde_json::from_reader 流式解析，base64 解码后
/// 直接写入磁盘（不把解码结果整体堆在内存里）。返回每个文件的下载结果（成功则为字节数）。
/// files 中每个元素是 (api_path, local_rel_path)：
/// - api_path   : 用于 API 请求（仓库内完整路径，如 src/api.rs）
/// - local_rel_path : 用于写盘（相对 out_dir，子目录克隆时会去掉 root 前缀）
pub fn download_files(
    token: &str,
    owner: &str,
    name: &str,
    r#ref: &str,
    files: &[(String, String)],
    out_dir: &std::path::Path,
) -> Result<Vec<(String, Result<(u64, bool)>)>> {
    use rayon::prelude::*;

    let client = Arc::new(web_client()?);
    let token = token.to_string();
    let owner = owner.to_string();
    let name = name.to_string();
    let r#ref = r#ref.to_string();
    let out_dir = out_dir.to_path_buf();

    // rayon 全局线程池并行下载：线程数由池管理（默认 CPU 核数），
    // 不会随文件数量爆炸；par_iter 结果自动保持与输入一致的顺序。
    let results: Vec<(String, Result<(u64, bool)>)> = files
        .par_iter()
        .map(|(api_path, local_path)| {
            let res = download_one(
                &client, &token, &owner, &name, &r#ref, api_path, local_path, &out_dir,
            );
            (api_path.clone(), res)
        })
        .collect();
    Ok(results)
}

/// 下载单个文件：GET /repository/files -> 流式解析 JSON -> base64 解码 -> 写磁盘
fn download_one(
    client: &reqwest::blocking::Client,
    token: &str,
    owner: &str,
    name: &str,
    r#ref: &str,
    api_file_path: &str,
    local_rel_path: &str,
    out_dir: &std::path::Path,
) -> Result<(u64, bool)> {
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname
    let repo_id = repo_path.replace('/', "%252F"); // owner%252Fname（二次编码）
    let url = format!(
        "{}/api/v2/projects/{}/repository/files?repoId={}&ref={}&file_path={}",
        web_base().trim_end_matches('/'),
        path_seg,
        repo_id,
        enc(r#ref),
        enc(api_file_path),
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("Accept", "*/*")
        .header("X-Platform", "web")
        // 文件大小没有上限，用普通接口的 60 秒会把大文件掐断。
        .timeout(transfer_timeout())
        .send()
        .context("请求 GitCode 文件内容接口失败")?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!(
            "文件内容接口错误 {}: {:?}",
            status,
            status.canonical_reason()
        ));
    }
    // 流式解析 JSON 响应体（HTTP body 边收边解析），得到 WebFile
    let wf = serde_json::from_reader::<_, WebFile>(resp).context("解析文件内容响应失败")?;
    let is_limited = wf.is_limited.unwrap_or(false);
    let raw = if wf.encoding.eq_ignore_ascii_case("base64") {
        base64::engine::general_purpose::STANDARD
            .decode(wf.content.trim())
            .map_err(|e| anyhow!("base64 解码失败: {}", e))?
    } else {
        wf.content.clone().into_bytes()
    };
    let dest = out_dir.join(local_rel_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow!("创建目录失败: {}", e))?;
    }
    std::fs::write(&dest, &raw).map_err(|e| anyhow!("写入 {} 失败: {}", dest.display(), e))?;
    Ok((raw.len() as u64, is_limited))
}

/// Fork 一个仓库到当前网页登录用户的命名空间（网页端 v2 接口：POST /api/v2/projects/{owner%2Fname}/fork）
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。body 字段（逆向自网页请求）：
/// repoId（owner%2Fname 单次编码）、path、name（新仓库名，冲突时服务端自动加后缀）、
/// description、fetch_refs=default。返回新建仓库的完整 JSON。
pub fn fork_repo(
    token: &str,
    owner: &str,
    name: &str,
    new_name: &str,
    description: &str,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({
        "repoId": path_seg,
        "path": new_name,
        "name": new_name,
        "description": description,
        "fetch_refs": "default",
    });
    let url = format!(
        "{}/api/v2/projects/{}/fork",
        web_base().trim_end_matches('/'),
        path_seg,
    );
    let page_ref = format!("https://gitcode.com/{}", repo_path);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode fork 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode fork 接口错误 {}: {}", status, text));
    }
    serde_json::from_str::<Value>(&text).context("解析 fork 响应失败")
}

/// 更新仓库的网页端元数据（网页端 v2 接口：PUT /api/v2/projects/{owner%2Fname}）。
///
/// body 可含 name / description / homepage / tag_list，对应仓库设置页的「仓库名 / 简介 /
/// 主页 / 标签」。该接口是整体替换式：调用方需先取回当前值，把要保留的字段一并放进 body，
/// 否则会被清掉（例如只传 tag_list 会把 description / homepage 清空）。
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头（与讨论 / Star / Fork 的 v2 接口同源）。
pub fn update_project_v2(token: &str, owner: &str, name: &str, body: &Value) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let url = format!(
        "{}/api/v2/projects/{}",
        web_base().trim_end_matches('/'),
        path_seg,
    );
    let page_ref = format!("https://gitcode.com/{}", repo_path);
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(body)
        .send()
        .context("请求 GitCode 仓库更新接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 仓库更新接口错误 {}: {}", status, text));
    }
    serde_json::from_str::<Value>(&text).context("解析仓库更新响应失败")
}

/// 给仓库点 Star（网页端 v2 接口：POST /api/v2/projects/{id}/star）
/// 注意：该接口用「数字项目 ID」而非 owner/name，需先通过 v5 接口解析出 id。
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。响应体含最新 star_count。
pub fn star_repo(token: &str, project_id: u64) -> Result<u64> {
    let client = web_client()?;
    let url = format!(
        "{}/api/v2/projects/{}/star",
        web_base().trim_end_matches('/'),
        project_id,
    );
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Accept", "*/*")
        .send()
        .context("请求 GitCode star 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode star 接口错误 {}: {}", status, text));
    }
    let v: Value = serde_json::from_str(&text).context("解析 star 响应失败")?;
    Ok(v["star_count"].as_u64().unwrap_or(0))
}

/// 取消仓库 Star（网页端 v2 接口：POST /api/v2/projects/{id}/unstar）
/// 注意：unstar 同样是 POST（与直觉的 DELETE 不同，已抓包确认），同样用数字项目 ID。
/// 响应体含最新 star_count。
pub fn unstar_repo(token: &str, project_id: u64) -> Result<u64> {
    let client = web_client()?;
    let url = format!(
        "{}/api/v2/projects/{}/unstar",
        web_base().trim_end_matches('/'),
        project_id,
    );
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Accept", "*/*")
        .send()
        .context("请求 GitCode unstar 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode unstar 接口错误 {}: {}", status, text));
    }
    let v: Value = serde_json::from_str(&text).context("解析 unstar 响应失败")?;
    Ok(v["star_count"].as_u64().unwrap_or(0))
}

/// 同步 fork 仓库与其上游（网页端 v2 接口：PUT /api/v2/projects/{owner%2Fname}/sync_repo）
/// body: {"branch": <分支>} —— 空字符串表示同步默认分支。返回 (repo_sync_result, repo_sync_message)。
/// 该接口仅「提交同步任务」，需再轮询 sync_repo_status 直到 success。
pub fn sync_repo(token: &str, owner: &str, name: &str, branch: &str) -> Result<(bool, String)> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname
    let body = serde_json::json!({ "branch": branch });
    let url = format!(
        "{}/api/v2/projects/{}/sync_repo",
        web_base().trim_end_matches('/'),
        path_seg,
    );
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .context("请求 GitCode 同步接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 同步接口错误 {}: {}", status, text));
    }
    let v: Value = serde_json::from_str(&text).context("解析同步响应失败")?;
    Ok((
        v["repo_sync_result"].as_bool().unwrap_or(false),
        v["repo_sync_message"].as_str().unwrap_or("").to_string(),
    ))
}

/// 查询 fork 同步任务状态（网页端 v2 接口：GET /api/v2/projects/{owner%2Fname}/sync_repo?branch）
/// 返回 (repo_sync_result, repo_sync_message)；result=true 且 message="success" 表示完成。
/// branch 与提交时一致：空字符串用 `?branch`，否则 `?branch=<分支>`（做百分号编码）。
pub fn sync_repo_status(
    token: &str,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<(bool, String)> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname
    let q = if branch.is_empty() {
        "branch".to_string()
    } else {
        format!("branch={}", enc(branch))
    };
    let url = format!(
        "{}/api/v2/projects/{}/sync_repo?{}",
        web_base().trim_end_matches('/'),
        path_seg,
        q,
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Accept", "*/*")
        .send()
        .context("请求 GitCode 同步状态接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 同步状态接口错误 {}: {}", status, text));
    }
    let v: Value = serde_json::from_str(&text).context("解析同步状态响应失败")?;
    Ok((
        v["repo_sync_result"].as_bool().unwrap_or(false),
        v["repo_sync_message"].as_str().unwrap_or("").to_string(),
    ))
}

/// 创建 wiki 页面（网页端 v2 接口：POST /api/v2/projects/wiki/create）
/// body 字段（逆向自网页请求）：repo_path(owner/name)、name、file_path（均用页面标题）、
/// commit_message、content、currUserId(留空)。响应体就是新建的文件名（如 "Home.md"）。
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。
pub fn wiki_create(
    token: &str,
    repo_path: &str,
    title: &str,
    content: &str,
    message: &str,
) -> Result<String> {
    let client = web_client()?;
    let body = serde_json::json!({
        "repo_path": repo_path,
        "name": title,
        "file_path": title,
        "commit_message": message,
        "content": content,
        "currUserId": "",
    });
    let url = format!(
        "{}/api/v2/projects/wiki/create",
        web_base().trim_end_matches('/'),
    );
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .context("请求 GitCode wiki 创建接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode wiki 创建接口错误 {}: {}", status, text));
    }
    Ok(text.trim().to_string())
}

/// 通过网页端 v2 接口创建 Issue：POST /issuepr/api/v1/issue/{owner%2Fname}/issues
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。body 字段逆向自网页请求：
/// title、description（网页端用 description 而非 body）、confidential、
/// project_id（owner%2Fname 单次编码）、labels、assignee_ids 等。
/// 返回新建 Issue 的完整 JSON（含 iid / web_url）。
pub fn issue_create(
    token: &str,
    owner: &str,
    name: &str,
    title: &str,
    description: &str,
    confidential: bool,
    labels: &[String],
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({
        "title": title,
        "description": description,
        "confidential": confidential,
        "assignee_ids": [],
        "project_id": path_seg,
        "labels": labels,
        "enterprise_labels": [],
        "milestone_id": "",
        "priority": "",
        "kanban_ids": [],
        "issue_category": "-",
        "issue_stage": "-",
        "issue_severity": "-",
        "field_list": [],
    });
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues",
        web_base().trim_end_matches('/'),
        path_seg,
    );
    let page_ref = format!("https://gitcode.com/{}/issues", repo_path);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .header("page-title", "%E6%96%B0%E5%BB%BA%20issue")
        .json(&body)
        .send()
        .context("请求 GitCode 创建 Issue 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 创建 Issue 接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析创建 Issue 响应失败")
}

/// 列出 Issue（网页端 v2）：GET /issuepr/api/v1/issue/{owner%2Fname}/issues
/// state 映射：open -> opened（网页端用 opened/closed）。返回 Issue 数组。
pub fn issue_list(token: &str, owner: &str, name: &str, state: &str) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let st = if state.eq_ignore_ascii_case("open") {
        "opened"
    } else if state.eq_ignore_ascii_case("close") || state.eq_ignore_ascii_case("closed") {
        "closed"
    } else {
        state
    };
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues?state={}",
        web_base().trim_end_matches('/'),
        path_seg,
        st,
    );
    let page_ref = format!("https://gitcode.com/{}/issues", repo_path);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .send()
        .context("请求 GitCode Issue 列表接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode Issue 列表接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析 Issue 列表响应失败")
}

/// 查看单个 Issue（网页端 v2）：GET /issuepr/api/v1/issue/{owner%2Fname}/issues/{iid}
/// 返回 Issue 完整 JSON（含 iid / web_url / description 等）。
pub fn issue_view(token: &str, owner: &str, name: &str, iid: u64) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues/{}",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/issues/{}", repo_path, iid);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .send()
        .context("请求 GitCode Issue 详情接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode Issue 详情接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析 Issue 详情响应失败")
}

/// 关闭 Issue（网页端 v2）：PUT /issuepr/api/v1/issue/{owner%2Fname}/issues/{iid}/close
/// body：{"state_event":"close","discussions":""}（逆向自网页关闭请求）。返回更新后的 Issue JSON。
pub fn issue_close(token: &str, owner: &str, name: &str, iid: u64) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({ "state_event": "close", "discussions": "" });
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues/{}/close",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/issues/{}", repo_path, iid);
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode 关闭 Issue 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 关闭 Issue 接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析关闭 Issue 响应失败")
}

/// 重新打开 Issue（网页端 v2）：与关闭同一端点，仅 state_event 改为 "reopen"。
pub fn issue_reopen(token: &str, owner: &str, name: &str, iid: u64) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({ "state_event": "reopen", "discussions": "" });
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues/{}/close",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/issues/{}", repo_path, iid);
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode 重新打开 Issue 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!(
            "GitCode 重新打开 Issue 接口错误 {}: {}",
            status,
            text
        ));
    }
    serde_json::from_str(&text).context("解析重新打开 Issue 响应失败")
}

/// Cherry-pick 一个已合并的 PR（网页端接口）：POST /issuepr/api/v1/projects/{owner%2Fname}/merge_request/{iid}/cherry_pick
/// body 逆向自网页请求：{"repoId":"owner%2Fname","iid":n,"branch":"main","with_new_merge_request":true}。
/// 返回服务器 JSON（新建的 MR 信息或提示文本）。
pub fn pr_cherry_pick(
    token: &str,
    owner: &str,
    name: &str,
    iid: u64,
    branch: &str,
    with_new_merge_request: bool,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({
        "repoId": path_seg,
        "iid": iid,
        "branch": branch,
        "with_new_merge_request": with_new_merge_request,
    });
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/merge_request/{}/cherry_pick",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/pull/{}", repo_path, iid);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode cherry-pick 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode cherry-pick 接口错误 {}: {}", status, text));
    }
    Ok(serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.trim().to_string())))
}

/// Revert 一个已合并的 PR（网页端接口）：POST /issuepr/api/v1/projects/{owner%2Fname}/merge_request/{iid}/revert
/// body 与 cherry-pick 相同：{"repoId":"owner%2Fname","iid":n,"branch":"main","with_new_merge_request":true}。
pub fn pr_revert(
    token: &str,
    owner: &str,
    name: &str,
    iid: u64,
    branch: &str,
    with_new_merge_request: bool,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({
        "repoId": path_seg,
        "iid": iid,
        "branch": branch,
        "with_new_merge_request": with_new_merge_request,
    });
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/merge_request/{}/revert",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/pull/{}", repo_path, iid);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode revert 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode revert 接口错误 {}: {}", status, text));
    }
    Ok(serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.trim().to_string())))
}

/// 提交 / 撤销 PR 审批（网页端接口）：PUT /issuepr/api/v1/projects/{owner%2Fname}/merge_requests/{iid}/approval
/// action_type 为 "approve"（通过）或 "reset"（撤销）。body 逆向自网页请求：
/// {"repoId":"owner%2Fname","iid":n,"action_type":"approve","force":false}。
pub fn pr_approval(
    token: &str,
    owner: &str,
    name: &str,
    iid: u64,
    action_type: &str,
    force: bool,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body = serde_json::json!({
        "repoId": path_seg,
        "iid": iid,
        "action_type": action_type,
        "force": force,
    });
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/merge_requests/{}/approval",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/pull/{}", repo_path, iid);
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body)
        .send()
        .context("请求 GitCode PR 审批接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode PR 审批接口错误 {}: {}", status, text));
    }
    Ok(serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.trim().to_string())))
}

/// 列出 PR（网页端接口）：GET /issuepr/api/v1/projects/{owner%2Fname}/isource/merge_requests
/// 查询参数逆向自网页请求：sort、order_by（updated_at/merged_at/created_at）、scope=all、
/// state（opened/merged/closed）、page、repoId（owner%2Fname 双重编码即 %252F）、per_page、view=basic。
pub fn pr_list_web(
    token: &str,
    owner: &str,
    name: &str,
    state: &str,
    order_by: &str,
    sort: &str,
    page: u64,
    per_page: u64,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // 路径段：owner%2Fname
    let repo_id = repo_path.replace('/', "%252F"); // 查询参数 repoId：双重编码
    let st = if state.eq_ignore_ascii_case("open") {
        "opened"
    } else if state.eq_ignore_ascii_case("close") || state.eq_ignore_ascii_case("closed") {
        "closed"
    } else {
        state
    };
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/isource/merge_requests?sort={}&order_by={}&scope=all&state={}&page={}&repoId={}&per_page={}&group_id&view=basic",
        web_base().trim_end_matches('/'),
        path_seg,
        sort,
        order_by,
        st,
        page,
        repo_id,
        per_page,
    );
    let page_ref = format!("https://gitcode.com/{}/pulls", repo_path);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .send()
        .context("请求 GitCode PR 列表（网页端）接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!(
            "GitCode PR 列表（网页端）接口错误 {}: {}",
            status,
            text
        ));
    }
    serde_json::from_str(&text).context("解析 PR 列表（网页端）响应失败")
}

/// 统计各状态 PR 数量（网页端接口）：GET /issuepr/api/v1/projects/{owner%2Fname}/isource/merge_requests/count
/// 带 only_count=true，返回 {"count": n} 之类的计数 JSON。state 支持 all/merged/closed/opened。
pub fn pr_list_count(token: &str, owner: &str, name: &str, state: &str) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // 路径段：owner%2Fname
    let repo_id = repo_path.replace('/', "%252F"); // 查询参数 repoId：双重编码
    let st = if state.eq_ignore_ascii_case("open") {
        "opened"
    } else if state.eq_ignore_ascii_case("close") || state.eq_ignore_ascii_case("closed") {
        "closed"
    } else {
        state
    };
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/isource/merge_requests/count?sort=desc&order_by=updated_at&scope=all&state={}&page=1&repoId={}&per_page=1&group_id&view=basic&only_count=true",
        web_base().trim_end_matches('/'),
        path_seg,
        st,
        repo_id,
    );
    let page_ref = format!("https://gitcode.com/{}/pulls", repo_path);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .send()
        .context("请求 GitCode PR 统计（网页端）接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!(
            "GitCode PR 统计（网页端）接口错误 {}: {}",
            status,
            text
        ));
    }
    serde_json::from_str(&text).context("解析 PR 统计（网页端）响应失败")
}

/// 在 PR 下发表评论（网页端接口）：POST /issuepr/api/v1/projects/{owner%2Fname}/merge_request/{iid}/notes
/// body 逆向自网页请求：{"body":"...","need_to_resolve":false}。
pub fn pr_note_create(token: &str, owner: &str, name: &str, iid: u64, body: &str) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let body_json = serde_json::json!({ "body": body, "need_to_resolve": false });
    let url = format!(
        "{}/issuepr/api/v1/projects/{}/merge_request/{}/notes",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/pull/{}", repo_path, iid);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&body_json)
        .send()
        .context("请求 GitCode PR 评论接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode PR 评论接口错误 {}: {}", status, text));
    }
    Ok(serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.trim().to_string())))
}

/// 回复 Issue（网页端 v2）：POST /issuepr/api/v1/issue/{owner%2Fname}/issues/{iid}/notes
/// 回复 Issue（网页端 v2）：POST /issuepr/api/v1/issue/{owner%2Fname}/issues/{iid}/discussions
/// body：{"project_id":"owner%2Fname","issue_iid":<iid>,"body":"..."}（逆向自网页「回复」请求）。
/// 返回 discussion 对象（含 notes 数组；首条 note 的 id 即评论 id）。
pub fn issue_reply(token: &str, owner: &str, name: &str, iid: u64, body: &str) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let b = serde_json::json!({
        "project_id": path_seg,
        "issue_iid": iid,
        "body": body,
    });
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues/{}/discussions",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/issues/{}", repo_path, iid);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&b)
        .send()
        .context("请求 GitCode 回复 Issue 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 回复 Issue 接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析回复 Issue 响应失败")
}

/// 编辑 Issue（网页端 v2）：PUT /issuepr/api/v1/issue/{owner%2Fname}/issues/{iid}
/// body 可包含 title / description（逆向自网页「编辑」请求，仅传需要修改的字段）。
/// 返回更新后的 Issue 完整 JSON。
pub fn issue_edit(
    token: &str,
    owner: &str,
    name: &str,
    iid: u64,
    title: Option<&str>,
    description: Option<&str>,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let mut body = serde_json::Map::new();
    if let Some(t) = title {
        body.insert(
            "title".to_string(),
            serde_json::Value::String(t.to_string()),
        );
    }
    if let Some(d) = description {
        body.insert(
            "description".to_string(),
            serde_json::Value::String(d.to_string()),
        );
    }
    let url = format!(
        "{}/issuepr/api/v1/issue/{}/issues/{}",
        web_base().trim_end_matches('/'),
        path_seg,
        iid,
    );
    let page_ref = format!("https://gitcode.com/{}/issues/{}", repo_path, iid);
    let resp = client
        .put(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("Content-Type", "application/json")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .json(&serde_json::Value::Object(body))
        .send()
        .context("请求 GitCode 编辑 Issue 接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 编辑 Issue 接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析编辑 Issue 响应失败")
}

/// 上传 Issue 附件：POST /api/v2/projects/{owner%252Fname}/user_attachment/upload
/// 注意路径里的 owner 是「二次编码」(%252F)。以 multipart 携带文件字节（网页端上传附件的
/// 实际传输方式；粘贴的抓包里 Content-Length 应是被截断的）。响应返回上传后的文件元信息。
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。
pub fn issue_attachment_upload(
    token: &str,
    owner: &str,
    name: &str,
    file_path: &std::path::Path,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let repo_id = repo_path.replace('/', "%252F"); // owner%252Fname（二次编码）
    let bytes = std::fs::read(file_path)
        .map_err(|e| anyhow!("读取附件 {} 失败: {}", file_path.display(), e))?;
    let fname = file_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment")
        .to_string();
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(fname.clone())
        .mime_str("application/octet-stream")
        .unwrap_or_else(|_| reqwest::blocking::multipart::Part::bytes(Vec::new()));
    let form = reqwest::blocking::multipart::Form::new()
        .text("type", "ISSUE")
        .text("project_id", repo_id.clone())
        .text("file_name", fname)
        .part("file", part);
    let url = format!(
        "{}/api/v2/projects/{}/user_attachment/upload",
        web_base().trim_end_matches('/'),
        repo_id,
    );
    let page_ref = format!("https://gitcode.com/{}/issues", repo_path);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .multipart(form)
        .send()
        .context("请求 GitCode 附件上传接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 附件上传接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析附件上传响应失败")
}

/// 对比两个分支/引用（网页端 v2）：GET /api/v2/projects/{owner%2Fname}/repository/compare
/// 查询参数：repoId（当前仓库，二次编码）、target_id（目标仓库，二次编码，默认同 repo）、
/// from（源分支）、to（目标分支）、ignore_whitespace_change。返回 commits / diffs / 行数统计。
/// 逆向自网页「新建合并请求」页面的对比请求。
pub fn repo_compare(
    token: &str,
    owner: &str,
    name: &str,
    target_owner: &str,
    target_name: &str,
    from: &str,
    to: &str,
) -> Result<Value> {
    let client = web_client()?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let repo_id = repo_path.replace('/', "%252F"); // 二次编码（repoId / target_id）
    let target_id = format!("{}/{}", target_owner, target_name).replace('/', "%252F");
    let url = format!(
        "{}/api/v2/projects/{}/repository/compare?repoId={}&target_id={}&ignore_whitespace_change=false&from={}&to={}",
        web_base().trim_end_matches('/'),
        path_seg,
        repo_id,
        target_id,
        enc(from),
        enc(to),
    );
    let page_ref = format!("https://gitcode.com/{}/pull/new", repo_path);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .header("page-ref", &page_ref)
        .header("page-uri", &page_ref)
        .send()
        .context("请求 GitCode 对比接口失败")?;
    let status = resp.status();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("GitCode 对比接口错误 {}: {}", status, text));
    }
    serde_json::from_str(&text).context("解析对比响应失败")
}

/// 刷新网页端 JWT（GET /uc/api/v1/user/oauth/token）
/// 必须在 Cookie 里同时携带旧的 access 与 refresh，否则服务端会注销会话。
/// 返回新的 (access, refresh)。token 会同时出现在 JSON body 与 Set-Cookie 中。
pub fn refresh_web_token(access: &str, refresh: &str) -> Result<(String, String)> {
    let client = web_client()?;
    let cookie = format!(
        "GITCODE_ACCESS_TOKEN={}; GITCODE_REFRESH_TOKEN={}",
        access, refresh
    );
    let url = format!(
        "{}/uc/api/v1/user/oauth/token",
        web_base().trim_end_matches('/')
    );
    let resp = client
        .get(&url)
        .header("Cookie", cookie)
        .header("Origin", "https://gitcode.com")
        .header("Referer", "https://gitcode.com/")
        .header("X-Platform", "web")
        .send()
        .context("请求 token 刷新接口失败")?;
    let status = resp.status();
    let set_cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect();
    let text = resp.text().context("读取响应体失败")?;
    if !status.is_success() {
        return Err(anyhow!("刷新 token 失败 {}: {}", status, text));
    }

    // 优先从 JSON body 取
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        let a = v["access_token"]
            .as_str()
            .or_else(|| v["accessToken"].as_str());
        let r = v["refresh_token"]
            .as_str()
            .or_else(|| v["refreshToken"].as_str());
        if let (Some(a), Some(r)) = (a, r) {
            return Ok((a.to_string(), r.to_string()));
        }
    }
    // 回退：从 Set-Cookie 里解析
    let mut na = None;
    let mut nr = None;
    for c in &set_cookies {
        if let Some(v) = cookie_value(c, "GITCODE_ACCESS_TOKEN") {
            if !v.is_empty() {
                na = Some(v);
            }
        }
        if let Some(v) = cookie_value(c, "GITCODE_REFRESH_TOKEN") {
            if !v.is_empty() {
                nr = Some(v);
            }
        }
    }
    match (na, nr) {
        (Some(a), Some(r)) => Ok((a, r)),
        _ => Err(anyhow!("刷新响应中未找到新的 access/refresh token")),
    }
}

/// 从单个 Set-Cookie 字符串里取某个 cookie 的值
fn cookie_value(set_cookie: &str, name: &str) -> Option<String> {
    let prefix = format!("{}=", name);
    for part in set_cookie.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}
