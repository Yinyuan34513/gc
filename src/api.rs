use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::Value;
use std::sync::Arc;
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

/// 对 URL query 参数值做百分号编码：非 unreserved 字符（含 / & = 空格 中文等）全部编码。
/// 用于 file_path / ref 等参数，避免 "&" 被当成 query 分隔符、中文/特殊字符导致 404。
fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

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
        let url = format!("{}&page_num={}&page_size=100", base, page);
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
        all.extend(v.content);
        if v.page_count == 0 || v.page_num >= v.page_count {
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
#[derive(serde::Deserialize, Debug)]
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

#[derive(serde::Deserialize, Debug, Default)]
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
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
        assert_eq!(
            got,
            vec!["b.rs".to_string(), "x/c.rs".to_string()]
        );
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
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
    let client = Arc::new(
        reqwest::blocking::Client::builder()
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
            )
            .build()
            .context("创建 HTTP 客户端失败")?,
    );
    let token = token.to_string();
    let owner = owner.to_string();
    let name = name.to_string();
    let r#ref = r#ref.to_string();
    let out_dir = out_dir.to_path_buf();

    let mut handles = Vec::new();
    for (api_path, local_path) in files {
        let client = Arc::clone(&client);
        let (token, owner, name, r#ref, out_dir, api_path, local_path) = (
            token.clone(),
            owner.clone(),
            name.clone(),
            r#ref.clone(),
            out_dir.clone(),
            api_path.clone(),
            local_path.clone(),
        );
        let h = std::thread::spawn(move || {
            let res = download_one(
                &client, &token, &owner, &name, &r#ref, &api_path, &local_path, &out_dir,
            );
            (api_path, res)
        });
        handles.push(h);
    }

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.join().map_err(|_| anyhow!("下载线程崩溃"))?);
    }
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
        .send()
        .context("请求 GitCode 文件内容接口失败")?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("文件内容接口错误 {}: {:?}", status, status.canonical_reason()));
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
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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

/// 给仓库点 Star（网页端 v2 接口：POST /api/v2/projects/{id}/star）
/// 注意：该接口用「数字项目 ID」而非 owner/name，需先通过 v5 接口解析出 id。
/// 需要浏览器会话 JWT（Bearer）+ 浏览器头。响应体含最新 star_count。
pub fn star_repo(token: &str, project_id: u64) -> Result<u64> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
        )
        .build()
        .context("创建 HTTP 客户端失败")?;
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
