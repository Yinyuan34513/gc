//! 仓库上下文与网页端 token 的共享助手。
//!
//! 从 gc-cli 提升而来，供 CLI 与 MCP 工具层共用：
//! 仓库参数解析（owner/name / URL / git remote 自动推断）与
//! 网页端 access token 获取（自动刷新）。

use anyhow::Result;

/// owner/name -> (owner, name)。允许直接传入 URL
/// （https://host/owner/repo 或 git@host:owner/repo）。
pub fn split_repo(s: &str) -> Result<(String, String)> {
    let s = parse_gitcode_repo(s).unwrap_or_else(|| s.to_string());
    let mut it = s.splitn(2, '/');
    let o = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    let n = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    if o.is_empty() || n.is_empty() {
        anyhow::bail!("仓库格式应为 owner/name");
    }
    Ok((o.to_string(), n.to_string()))
}

/// gc 在当前仓库用哪个 remote 解析 owner/name。存在 git local config `gc.remote`
/// 则用它，否则 origin。上游 @oh-gc/cli 是往仓库里写 `.gitcode` 文件记这个值；
/// 这里改用 git 自己的 local config，省掉一个需要进 .gitignore 的文件，
/// 也能直接 `git config gc.remote` 查改。
pub fn configured_remote() -> String {
    std::process::Command::new("git")
        .args(["config", "--get", "gc.remote"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "origin".to_string())
}

/// 从当前目录的 git remote 推断 owner/name。
/// 在克隆仓库目录下直接 `gc repo sync` 不指定 <REPO> 时使用，与 gh 行为一致。
pub fn detect_repo_from_cwd() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", &configured_remote()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_gitcode_repo(&String::from_utf8_lossy(&out.stdout))
}

/// 当前所在 git 分支（非 git 仓库或游离 HEAD 时返回 None）
pub fn current_git_branch() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}

/// 参数给了就用参数，否则从当前目录的 git remote 推断。
/// MCP 工具层与 CLI 共用（与 CLI 行为一致）。
pub fn resolve_repo(repo: Option<String>) -> Result<(String, String)> {
    let r = repo.or_else(detect_repo_from_cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "当前目录不是 gitcode 仓库（无法从 git remote {} 推断），请显式指定 <REPO>",
            configured_remote()
        )
    })?;
    split_repo(&r)
}

/// 解析仓库标识：只认 gitcode.com / atomgit.com 两种主机的三种形式：
/// https://host/owner/repo(.git)、git@host:owner/repo(.git)、owner/repo
pub fn parse_gitcode_repo(url: &str) -> Option<String> {
    let u = url.trim();
    const HOSTS: [&str; 2] = ["gitcode.com", "atomgit.com"];
    let stripped = if let Some(p) = u.find("://") {
        let rest = &u[p + 3..];
        let i = rest.find('/')?;
        let host = &rest[..i];
        if !HOSTS
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        {
            return None;
        }
        &rest[i + 1..]
    } else if let Some(r) = u.strip_prefix("git@") {
        let i = r.find(':')?;
        let host = &r[..i];
        if !HOSTS
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        {
            return None;
        }
        &r[i + 1..]
    } else {
        u
    };
    let repo = stripped.strip_suffix(".git").unwrap_or(stripped);
    if repo.contains('/') {
        Some(repo.to_string())
    } else {
        None
    }
}

/// 获取可用于讨论接口的 access token：
/// - 若同时配置了 refresh token，则先刷新（并落盘）再返回；
/// - 若只有 access token，则直接返回（不刷新，避免只带 access 调用会注销会话）；
/// - 刷新失败时回退到已有 access token。
pub fn web_access_token() -> Result<String> {
    let (access, refresh) = crate::config::load_web_tokens()?;
    if refresh.trim().is_empty() {
        return Ok(access);
    }
    match crate::api::refresh_web_token(&access, &refresh) {
        Ok((new_access, new_refresh)) => {
            let _ = crate::config::save_web_tokens(&new_access, &new_refresh);
            Ok(new_access)
        }
        Err(e) => {
            eprintln!("警告：自动刷新 token 失败（{}），将使用现有 token", e);
            Ok(access)
        }
    }
}

/// 运行 git 命令，失败时返回带 stderr 的错误。
pub fn git_run(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("运行 git 失败: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow::anyhow!("git {} 失败: {}", args.join(" "), err));
    }
    Ok(())
}
