use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::Read;

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
        let client = reqwest::blocking::Client::builder()
            .user_agent("gc/0.1 (GitCode CLI)")
            .build()
            .context("创建 HTTP 客户端失败")?;
        Ok(Self { base, token, client })
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
        let resp = builder.send().context("请求 GitCode API 失败")?;
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

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, &[], Some(body))
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

    // ---- Issue ----
    pub fn issues(&self, owner: &str, name: &str, state: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/issues", owner, name),
            &[("state", state)],
        )
    }

    pub fn issue(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/issues/{}", owner, name, num), &[])
    }

    pub fn create_issue(&self, owner: &str, name: &str, title: &str, body: &str) -> Result<Value> {
        let b = serde_json::json!({ "title": title, "body": body });
        self.post(&format!("/repos/{}/{}/issues", owner, name), &b)
    }

    // ---- Pull Request ----
    pub fn pulls(&self, owner: &str, name: &str, state: &str) -> Result<Value> {
        self.get(
            &format!("/repos/{}/{}/pulls", owner, name),
            &[("state", state)],
        )
    }

    pub fn pull(&self, owner: &str, name: &str, num: u64) -> Result<Value> {
        self.get(&format!("/repos/{}/{}/pulls/{}", owner, name, num), &[])
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
        let total = std::fs::metadata(file)
            .map(|m| m.len())
            .unwrap_or(0);
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
        let reader = ProgressReader {
            inner: std::fs::File::open(file)
                .map_err(|e| anyhow!("打开文件失败: {}", e))?,
            bar: bar.clone(),
        };
        let part = reqwest::blocking::multipart::Part::reader_with_length(reader, total)
            .file_name(file_name);
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        let resp = self
            .client
            .post(&url)
            .multipart(form)
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

/// 讨论接口基址，可用 GITCODE_WEB_BASE 覆盖（默认网页端）
pub fn web_base() -> String {
    std::env::var("GITCODE_WEB_BASE")
        .unwrap_or_else(|_| "https://web-api.gitcode.com".to_string())
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
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
pub fn repo_file_list(token: &str, owner: &str, name: &str, ref_name: &str) -> Result<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
    let repo_path = format!("{}/{}", owner, name);
    let path_seg = repo_path.replace('/', "%2F"); // owner%2Fname（路径段，单次编码）
    let repo_id = repo_path.replace('/', "%252F"); // owner%252Fname（repoId 参数，二次编码）
    let url = format!(
        "{}/api/v2/projects/{}/repository/file-list?repoId={}&ref_name={}",
        web_base().trim_end_matches('/'),
        path_seg,
        repo_id,
        ref_name,
    );
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
        return Ok(Vec::new());
    }
    #[derive(serde::Deserialize)]
    struct FileList {
        #[serde(default)]
        content: Vec<String>,
    }
    let v: FileList = serde_json::from_str(&text).context("解析文件列表响应失败")?;
    Ok(v.content)
}

/// 刷新网页端 JWT（GET /uc/api/v1/user/oauth/token）
/// 必须在 Cookie 里同时携带旧的 access 与 refresh，否则服务端会注销会话。
/// 返回新的 (access, refresh)。token 会同时出现在 JSON body 与 Set-Cookie 中。
pub fn refresh_web_token(access: &str, refresh: &str) -> Result<(String, String)> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
        let a = v["access_token"].as_str().or_else(|| v["accessToken"].as_str());
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
