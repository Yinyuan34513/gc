use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::Path;

/// 多用户令牌档案：一个名字 + 一个 PAT（可带备注）。
/// `gc auth user list` 的编号即 users 数组下标 + 1。
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct UserProfile {
    /// 用户名 / 备注名（`gc auth switch` 按列表序号切换）
    pub name: String,
    /// GitCode 个人访问令牌
    pub token: String,
    /// 备注（账号用途等）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 解析 ~/.config/gitcode.toml（可选）
#[derive(Debug, Deserialize, serde::Serialize)]
struct TomlConfig {
    /// 旧版单 token 字段：users 为空时迁入 default 用户；迁移后不再写入
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    web_token: Option<String>,
    #[serde(default)]
    web_refresh: Option<String>,
    /// 多用户令牌档案
    #[serde(default)]
    users: Vec<UserProfile>,
    /// 当前激活用户名（load_token 取它的 token）
    #[serde(default)]
    active_user: Option<String>,
}

/// 旧配置迁移 / 首个令牌落库时使用的默认用户名
const DEFAULT_USER: &str = "default";

impl TomlConfig {
    fn empty() -> Self {
        TomlConfig {
            token: None,
            web_token: None,
            web_refresh: None,
            users: Vec::new(),
            active_user: None,
        }
    }

    /// 旧版单 token → default 用户（内存迁移，落盘由后续 save 完成）；
    /// active_user 指向已删除用户时回退到第一个用户。
    fn migrate(&mut self) {
        if self.users.is_empty() {
            if let Some(t) = self.token.take().filter(|t| !t.trim().is_empty()) {
                self.users.push(UserProfile {
                    name: DEFAULT_USER.to_string(),
                    token: t,
                    note: None,
                });
                self.active_user = Some(DEFAULT_USER.to_string());
            }
        }
        let active_missing = self
            .active_user
            .as_ref()
            .is_some_and(|a| !self.users.iter().any(|u| &u.name == a));
        if active_missing || (self.active_user.is_none() && !self.users.is_empty()) {
            self.active_user = self.users.first().map(|u| u.name.clone());
        }
    }

    /// 当前激活档案
    fn active(&self) -> Option<&UserProfile> {
        self.active_user
            .as_ref()
            .and_then(|a| self.users.iter().find(|u| &u.name == a))
    }

    /// 按 1-based 序号取档案
    fn by_index(&self, num: usize) -> Option<&UserProfile> {
        if num == 0 {
            return None;
        }
        self.users.get(num - 1)
    }
}

/// 读配置文件并做旧版迁移（迁移只在内存，写盘由 save 类函数完成）
fn read_config(path: &Path) -> Result<TomlConfig> {
    if !path.exists() {
        return Ok(TomlConfig::empty());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
    let mut cfg: TomlConfig =
        toml::from_str(&text).map_err(|e| anyhow!("解析 gitcode.toml 失败: {}", e))?;
    cfg.migrate();
    Ok(cfg)
}

fn write_config(path: &Path, cfg: &TomlConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("创建目录 {} 失败: {}", parent.display(), e))?;
    }
    let body = toml::to_string(cfg).map_err(|e| anyhow!("序列化配置失败: {}", e))?;
    std::fs::write(path, body).map_err(|e| anyhow!("写入 {} 失败: {}", path.display(), e))?;
    Ok(())
}

/// 读取 GitCode 个人访问令牌（可选）：
/// 1) 优先环境变量 GITCODE_TOKEN
/// 2) 当前激活用户的 token（`gc auth switch` 切换的对象）
/// 找不到时返回 None，调用方据此判断是否需要鉴权。
pub fn load_token() -> Result<Option<String>> {
    if let Ok(t) = std::env::var("GITCODE_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(Some(t.trim().to_string()));
        }
    }
    let cfg = read_config(&config_path()?)?;
    Ok(cfg.active().map(|u| u.token.clone()))
}

/// 保存 API token 到**当前激活用户**（没有任何用户时创建 default 用户）。
/// 返回规范化后的 token，便于调用方立即做有效性验证。
pub fn save_token(token: &str) -> Result<String> {
    save_token_at(&config_path()?, token)
}

fn save_token_at(path: &Path, token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("token 为空，已取消");
    }
    let mut cfg = read_config(path)?;
    let active = cfg.active_user.clone();
    if cfg.users.is_empty() {
        cfg.users.push(UserProfile {
            name: DEFAULT_USER.to_string(),
            token: token.to_string(),
            note: None,
        });
        cfg.active_user = Some(DEFAULT_USER.to_string());
    } else if let Some(u) = cfg
        .users
        .iter_mut()
        .find(|u| Some(&u.name) == active.as_ref())
    {
        u.token = token.to_string();
    } else {
        cfg.users[0].token = token.to_string();
        cfg.active_user = Some(cfg.users[0].name.clone());
    }
    cfg.token = None; // 已迁移进 users，清除旧字段（单一数据源）
    write_config(path, &cfg)?;
    Ok(token.to_string())
}

/// 列出全部用户档案（list 编号 = 数组下标 + 1）
pub fn list_users() -> Result<Vec<UserProfile>> {
    Ok(read_config(&config_path()?)?.users)
}

/// 当前激活用户名
pub fn active_user() -> Result<Option<String>> {
    Ok(read_config(&config_path()?)?.active_user)
}

/// 按 1-based 序号切换激活用户；返回切换到的档案
pub fn switch_user(num: usize) -> Result<UserProfile> {
    switch_user_at(&config_path()?, num)
}

fn switch_user_at(path: &Path, num: usize) -> Result<UserProfile> {
    let mut cfg = read_config(path)?;
    let target = cfg.by_index(num).cloned().ok_or_else(|| {
        anyhow!(
            "序号 {} 不存在（共 {} 个用户，请输入 1-{}）。用 `gc auth user list` 查看",
            num,
            cfg.users.len(),
            cfg.users.len()
        )
    })?;
    cfg.active_user = Some(target.name.clone());
    write_config(path, &cfg)?;
    Ok(target)
}

/// 新增用户令牌（重名拒绝）；返回 (序号, 档案)。
/// 新增后自动切换为激活用户（加 token 的意图就是要用它）。
pub fn add_user(name: &str, token: &str, note: Option<&str>) -> Result<(usize, UserProfile)> {
    add_user_at(&config_path()?, name, token, note)
}

fn add_user_at(
    path: &Path,
    name: &str,
    token: &str,
    note: Option<&str>,
) -> Result<(usize, UserProfile)> {
    let name = name.trim();
    let token = token.trim();
    if name.is_empty() {
        anyhow::bail!("用户名为空");
    }
    if token.is_empty() {
        anyhow::bail!("token 为空，已取消");
    }
    let mut cfg = read_config(path)?;
    if let Some(pos) = cfg.users.iter().position(|u| u.name == name) {
        anyhow::bail!(
            "用户 {} 已存在（序号 {}）。更新其 token 请先 `gc auth switch {}` 再登录",
            name,
            pos + 1,
            pos + 1
        );
    }
    let profile = UserProfile {
        name: name.to_string(),
        token: token.to_string(),
        note: note.map(str::to_string),
    };
    cfg.users.push(profile.clone());
    cfg.active_user = Some(profile.name.clone());
    cfg.token = None;
    write_config(path, &cfg)?;
    Ok((cfg.users.len(), profile))
}

/// 按 1-based 序号删除用户；返回被删用户名。删的是激活用户时激活态顺延到第一个
pub fn remove_user(num: usize) -> Result<String> {
    remove_user_at(&config_path()?, num)
}

fn remove_user_at(path: &Path, num: usize) -> Result<String> {
    let mut cfg = read_config(path)?;
    let target = cfg.by_index(num).map(|u| u.name.clone()).ok_or_else(|| {
        anyhow!(
            "序号 {} 不存在（共 {} 个用户）。用 `gc auth user list` 查看",
            num,
            cfg.users.len()
        )
    })?;
    cfg.users.retain(|u| u.name != target);
    if cfg.active_user.as_deref() == Some(target.as_str()) {
        cfg.active_user = cfg.users.first().map(|u| u.name.clone());
    }
    write_config(path, &cfg)?;
    Ok(target)
}

/// API 基址，可用环境变量 GITCODE_API_BASE 覆盖（默认官方 AtomGit v5 接口）
pub fn api_base() -> String {
    std::env::var("GITCODE_API_BASE")
        .unwrap_or_else(|_| "https://api.gitcode.com/api/v5".to_string())
}

/// Git 克隆基址，可用环境变量 GITCODE_GIT_BASE 覆盖（默认 https://gitcode.com）
pub fn git_base() -> String {
    std::env::var("GITCODE_GIT_BASE").unwrap_or_else(|_| "https://gitcode.com".to_string())
}

/// 拼出仓库的 Git HTTPS 地址
pub fn clone_url(owner: &str, name: &str) -> String {
    format!(
        "{}/{}/{}.git",
        git_base().trim_end_matches('/'),
        owner,
        name
    )
}

/// ~/.config/gitcode.toml 的完整路径
pub fn config_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| anyhow!("无法确定配置目录"))?;
    Ok(dir.join("gitcode.toml"))
}

/// 交互式隐藏输入 token，并写入当前激活用户（保留其它字段）。
/// 返回读取到的 token，便于调用方立即做有效性验证。
pub fn prompt_and_save_token() -> Result<String> {
    let token = rpassword::prompt_password_stderr("请输入 GitCode 个人访问令牌: ")
        .map_err(|e| anyhow!("读取密码失败: {}", e))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("token 为空，已取消");
    }
    save_token(&token)
}

/// token 脱敏展示：保留前 6 后 4（够识别是哪个 key，又不足以被盗用）
pub fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 12 {
        return "***".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{}…{}", head, tail)
}

/// 读取网页端 JWT（用于讨论等 v1 接口）的 access + refresh：
/// 优先环境变量 GITCODE_WEB_TOKEN / GITCODE_WEB_REFRESH_TOKEN，回退到配置文件。
pub fn load_web_tokens() -> Result<(String, String)> {
    if let (Ok(a), Ok(r)) = (
        std::env::var("GITCODE_WEB_TOKEN"),
        std::env::var("GITCODE_WEB_REFRESH_TOKEN"),
    ) {
        if !a.trim().is_empty() && !r.trim().is_empty() {
            return Ok((a.trim().to_string(), r.trim().to_string()));
        }
    }
    let cfg = read_config(&config_path()?)?;
    if let (Some(a), Some(r)) = (cfg.web_token, cfg.web_refresh) {
        if !a.trim().is_empty() && !r.trim().is_empty() {
            return Ok((a.trim().to_string(), r.trim().to_string()));
        }
    }
    Err(anyhow!(
        "未找到网页端 token。请设置 GITCODE_WEB_TOKEN 与 GITCODE_WEB_REFRESH_TOKEN（可用 `gc webtoken set` 录入）"
    ))
}

/// 保存网页端 access + refresh token 到 ~/.config/gitcode.toml（保留其它字段）
pub fn save_web_tokens(access: &str, refresh: &str) -> Result<()> {
    let path = config_path()?;
    let mut cfg = read_config(&path)?;
    cfg.web_token = Some(access.to_string());
    cfg.web_refresh = Some(refresh.to_string());
    write_config(&path, &cfg)?;
    Ok(())
}

/// 交互式隐藏录入网页端 access / refresh token，并写入配置
pub fn prompt_and_save_web_tokens() -> Result<(String, String)> {
    let access = rpassword::prompt_password_stderr("网页端 access token: ")
        .map_err(|e| anyhow!("读取密码失败: {}", e))?;
    let refresh = rpassword::prompt_password_stderr("网页端 refresh token: ")
        .map_err(|e| anyhow!("读取密码失败: {}", e))?;
    let access = access.trim().to_string();
    let refresh = refresh.trim().to_string();
    if access.is_empty() || refresh.is_empty() {
        anyhow::bail!("token 为空，已取消");
    }
    save_web_tokens(&access, &refresh)?;
    Ok((access, refresh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gc-cfg-test-{}-{}.toml", tag, std::process::id()))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_clone_url_default() {
        std::env::remove_var("GITCODE_GIT_BASE");
        assert_eq!(
            clone_url("alice", "demo"),
            "https://gitcode.com/alice/demo.git"
        );
    }

    #[test]
    fn test_clone_url_with_base() {
        std::env::set_var("GITCODE_GIT_BASE", "https://git.example.com/");
        assert_eq!(
            clone_url("bob", "repo"),
            "https://git.example.com/bob/repo.git"
        );
        std::env::remove_var("GITCODE_GIT_BASE");
    }

    #[test]
    fn test_migrate_legacy_token_to_default_user() {
        let path = tmp_path("migrate");
        cleanup(&path);
        // 旧版格式：只有 token 字段
        std::fs::write(&path, "token = \"legacy-pat\"\n").unwrap();
        let cfg = read_config(&path).unwrap();
        assert_eq!(cfg.users.len(), 1);
        assert_eq!(cfg.users[0].name, DEFAULT_USER);
        assert_eq!(cfg.users[0].token, "legacy-pat");
        assert_eq!(cfg.active_user.as_deref(), Some(DEFAULT_USER));
        // 激活档案即旧 token
        assert_eq!(cfg.active().map(|u| u.token.as_str()), Some("legacy-pat"));
        cleanup(&path);
    }

    #[test]
    fn test_save_token_creates_default_user() {
        let path = tmp_path("save-new");
        cleanup(&path);
        let t = save_token_at(&path, "  new-pat  ").unwrap();
        assert_eq!(t, "new-pat");
        let cfg = read_config(&path).unwrap();
        assert_eq!(cfg.users.len(), 1);
        assert_eq!(cfg.users[0].token, "new-pat");
        assert!(cfg.token.is_none(), "迁移后旧字段应被清除");
        cleanup(&path);
    }

    #[test]
    fn test_save_token_updates_active_user_only() {
        let path = tmp_path("save-active");
        cleanup(&path);
        save_token_at(&path, "first-pat").unwrap();
        add_user_at(&path, "second", "second-pat", Some("工作号")).unwrap();
        // add 后自动切换到 second
        save_token_at(&path, "second-pat-2").unwrap();
        let cfg = read_config(&path).unwrap();
        assert_eq!(cfg.active_user.as_deref(), Some("second"));
        assert_eq!(cfg.active().unwrap().token, "second-pat-2");
        // 切回 first 后保存只改 first
        switch_user_at(&path, 1).unwrap();
        save_token_at(&path, "first-pat-2").unwrap();
        let cfg = read_config(&path).unwrap();
        assert_eq!(cfg.active().unwrap().token, "first-pat-2");
        assert_eq!(cfg.users[1].token, "second-pat-2");
        cleanup(&path);
    }

    #[test]
    fn test_switch_user_by_num() {
        let path = tmp_path("switch");
        cleanup(&path);
        save_token_at(&path, "a").unwrap();
        add_user_at(&path, "b", "b-token", None).unwrap();
        add_user_at(&path, "c", "c-token", None).unwrap();
        let u = switch_user_at(&path, 3).unwrap();
        assert_eq!(u.name, "c");
        assert_eq!(
            read_config(&path).unwrap().active_user.as_deref(),
            Some("c")
        );
        // 越界与 0 报错
        assert!(switch_user_at(&path, 4).is_err());
        assert!(switch_user_at(&path, 0).is_err());
        cleanup(&path);
    }

    #[test]
    fn test_add_user_duplicate_rejected() {
        let path = tmp_path("dup");
        cleanup(&path);
        save_token_at(&path, "a").unwrap();
        add_user_at(&path, "b", "b-token", None).unwrap();
        let err = add_user_at(&path, "b", "other", None).unwrap_err();
        assert!(err.to_string().contains("已存在"));
        cleanup(&path);
    }

    #[test]
    fn test_remove_active_user_falls_back() {
        let path = tmp_path("remove");
        cleanup(&path);
        save_token_at(&path, "a").unwrap();
        add_user_at(&path, "b", "b-token", None).unwrap();
        // 当前激活 b（add 自动切换）；删 b 后激活回退到第一个
        let removed = remove_user_at(&path, 2).unwrap();
        assert_eq!(removed, "b");
        let cfg = read_config(&path).unwrap();
        assert_eq!(cfg.users.len(), 1);
        assert_eq!(cfg.active_user.as_deref(), Some(DEFAULT_USER));
        assert!(remove_user_at(&path, 5).is_err());
        cleanup(&path);
    }

    #[test]
    fn test_mask_token() {
        assert_eq!(mask_token("short"), "***");
        let m = mask_token("glpat-ABCDEFGHIJKLMNOP");
        assert!(m.starts_with("glpat-"));
        assert!(m.ends_with("MNOP"));
        assert!(!m.contains("GHIJKL"));
    }
}
