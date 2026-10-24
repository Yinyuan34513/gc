use anyhow::{anyhow, Result};
use serde::Deserialize;

/// 解析 ~/.config/gitcode.toml（可选）
#[derive(Debug, Deserialize, serde::Serialize)]
struct TomlConfig {
    token: Option<String>,
    #[serde(default)]
    web_token: Option<String>,
    #[serde(default)]
    web_refresh: Option<String>,
}

/// 读取 GitCode 个人访问令牌（可选）：
/// 1) 优先环境变量 GITCODE_TOKEN
/// 2) 回退到 ~/.config/gitcode.toml 的 token 字段
/// 找不到时返回 None，调用方据此判断是否需要鉴权。
pub fn load_token() -> Result<Option<String>> {
    if let Ok(t) = std::env::var("GITCODE_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(Some(t.trim().to_string()));
        }
    }
    if let Some(cfg_dir) = dirs::config_dir() {
        let path = cfg_dir.join("gitcode.toml");
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
            let cfg: TomlConfig =
                toml::from_str(&text).map_err(|e| anyhow!("解析 gitcode.toml 失败: {}", e))?;
            if let Some(t) = cfg.token {
                if !t.trim().is_empty() {
                    return Ok(Some(t.trim().to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// API 基址，可用环境变量 GITCODE_API_BASE 覆盖（默认官方 AtomGit v5 接口）
pub fn api_base() -> String {
    std::env::var("GITCODE_API_BASE")
        .unwrap_or_else(|_| "https://api.gitcode.com/api/v5".to_string())
}

/// Git 克隆基址，可用环境变量 GITCODE_GIT_BASE 覆盖（默认 https://gitcode.com）
pub fn git_base() -> String {
    std::env::var("GITCODE_GIT_BASE")
        .unwrap_or_else(|_| "https://gitcode.com".to_string())
}

/// 拼出仓库的 Git HTTPS 地址
pub fn clone_url(owner: &str, name: &str) -> String {
    format!("{}/{}/{}.git", git_base().trim_end_matches('/'), owner, name)
}

/// ~/.config/gitcode.toml 的完整路径
pub fn config_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| anyhow!("无法确定配置目录"))?;
    Ok(dir.join("gitcode.toml"))
}

/// 交互式隐藏输入 token，并写入 ~/.config/gitcode.toml（保留其它字段）。
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

/// 保存 API token 到 ~/.config/gitcode.toml（保留其它字段，非交互）。
/// 返回读取到的 token，便于调用方立即做有效性验证。
pub fn save_token(token: &str) -> Result<String> {
    if token.trim().is_empty() {
        anyhow::bail!("token 为空，已取消");
    }

    let path = config_path()?;
    let mut cfg: TomlConfig = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
        toml::from_str(&text).unwrap_or(TomlConfig {
            token: None,
            web_token: None,
            web_refresh: None,
        })
    } else {
        TomlConfig {
            token: None,
            web_token: None,
            web_refresh: None,
        }
    };
    cfg.token = Some(token.trim().to_string());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("创建目录 {} 失败: {}", parent.display(), e))?;
    }
    let body = toml::to_string(&cfg).map_err(|e| anyhow!("序列化配置失败: {}", e))?;
    std::fs::write(&path, body)
        .map_err(|e| anyhow!("写入 {} 失败: {}", path.display(), e))?;

    Ok(token.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    if let Some(cfg_dir) = dirs::config_dir() {
        let path = cfg_dir.join("gitcode.toml");
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
            let cfg: TomlConfig =
                toml::from_str(&text).unwrap_or(TomlConfig {
                    token: None,
                    web_token: None,
                    web_refresh: None,
                });
            if let (Some(a), Some(r)) = (cfg.web_token, cfg.web_refresh) {
                if !a.trim().is_empty() && !r.trim().is_empty() {
                    return Ok((a.trim().to_string(), r.trim().to_string()));
                }
            }
        }
    }
    Err(anyhow!(
        "未找到网页端 token。请设置 GITCODE_WEB_TOKEN 与 GITCODE_WEB_REFRESH_TOKEN（可用 `gc webtoken set` 录入）"
    ))
}

/// 保存网页端 access + refresh token 到 ~/.config/gitcode.toml（保留其它字段）
pub fn save_web_tokens(access: &str, refresh: &str) -> Result<()> {
    let path = config_path()?;
    let mut cfg: TomlConfig = if path.exists() {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        toml::from_str(&text).unwrap_or(TomlConfig {
            token: None,
            web_token: None,
            web_refresh: None,
        })
    } else {
        TomlConfig {
            token: None,
            web_token: None,
            web_refresh: None,
        }
    };
    cfg.web_token = Some(access.to_string());
    cfg.web_refresh = Some(refresh.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("创建目录 {} 失败: {}", parent.display(), e))?;
    }
    let body = toml::to_string(&cfg).map_err(|e| anyhow!("序列化配置失败: {}", e))?;
    std::fs::write(&path, body).map_err(|e| anyhow!("写入 {} 失败: {}", path.display(), e))?;
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

