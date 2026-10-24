//! `gc auth login --qr` —— OAuth broker 扫码登录。
//!
//! 流程（契约逆向自 lming112/gitcode-cli 的 docs/openspec.md）：
//! 1. `GET {broker}/auth/login?provider=gitcode` 拿 `login_url` + `state`；
//! 2. 终端渲染二维码（`qrcode` crate，Unicode 半块字符）；
//! 3. 轮询 `GET {broker}/auth/check?state=...` 直到授权完成；
//! 4. `GET {broker}/auth/token?state=...` 换取 access token（即 PAT）。
//!
//! broker 地址优先级：`--broker` 参数 > 环境变量 `GITCODE_BROKER`。
//! 没有公开的默认 broker——broker 自行持有 OAuth client_secret，
//! gc 本身不存任何 secret（与 lming112 相同的安全模型）。

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::io::Write;
use std::time::Duration;

/// 轮询间隔
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 轮询总时长上限（秒）
const POLL_TIMEOUT_SECS: u64 = 300;
/// OAuth provider 名（openspec 明确要求用 gitcode 而非 atomgit）
const PROVIDER: &str = "gitcode";

/// broker 基址：参数 > `GITCODE_BROKER` 环境变量；都没有则报错并说明用法。
/// 归一化会去掉结尾斜杠。
pub fn broker_base_url(explicit: Option<&str>) -> Result<String> {
    let raw = explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("GITCODE_BROKER")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .ok_or_else(|| {
            anyhow!(
                "未配置 OAuth broker。请用 --broker <URL> 或设置环境变量 GITCODE_BROKER。\n\
                 broker 是一个自行部署的服务，负责持有 GitCode OAuth client_secret 并完成授权码交换；\n\
                 契约参考 https://atomgit.com/lming112/gitcode-cli 的 docs/openspec.md"
            )
        })?;
    Ok(raw.trim_end_matches('/').to_string())
}

/// broker 返回的登录会话
pub struct LoginSession {
    state: String,
    login_url: String,
    broker_base: String,
    client: reqwest::blocking::Client,
}

/// broker `/auth/token` 返回的令牌信息
#[derive(Debug, Clone)]
pub struct BrokerToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// 登录用户名（broker 可选返回）
    pub user: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    login_url: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// 不同 broker 的用户字段名不一，尽量兼容
    #[serde(default)]
    user: Option<BrokerUser>,
}

#[derive(Debug, Deserialize)]
struct BrokerUser {
    #[serde(default, alias = "username", alias = "name")]
    login: Option<String>,
}

impl LoginSession {
    pub fn url(&self) -> &str {
        &self.login_url
    }

    /// 轮询一次授权状态；`true` = 已授权。
    pub fn poll_once(&self) -> Result<bool> {
        let value: serde_json::Value = self
            .client
            .get(format!("{}/auth/check", self.broker_base))
            .query(&[("state", self.state.as_str())])
            .send()
            .context("请求 broker /auth/check 失败")?
            .error_for_status()
            .context("broker /auth/check 返回错误")?
            .json()
            .context("解析 broker /auth/check 响应失败")?;
        Ok(response_is_authorized(&value))
    }

    /// 授权完成后换取 token
    pub fn finish(&self) -> Result<BrokerToken> {
        fetch_token(&self.client, &self.broker_base, &self.state)
    }
}

/// 向 broker 发起登录会话
pub fn start_login(broker_base: &str) -> Result<LoginSession> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("创建 HTTP 客户端失败")?;
    let response: LoginResponse = client
        .get(format!("{}/auth/login", broker_base))
        .query(&[("provider", PROVIDER)])
        .send()
        .context("请求 broker /auth/login 失败")?
        .error_for_status()
        .context("broker /auth/login 返回错误")?
        .json()
        .context("解析 broker /auth/login 响应失败")?;
    Ok(LoginSession {
        state: response.state,
        // 去掉 force_login=true，复用浏览器已登录的 GitCode 会话
        login_url: strip_force_login(&response.login_url),
        broker_base: broker_base.trim_end_matches('/').to_string(),
        client,
    })
}

/// 轮询直到授权或超时（进度点打印到 stdout）。返回 broker 令牌。
pub fn wait_for_authorization(session: &LoginSession) -> Result<BrokerToken> {
    let max_polls = POLL_TIMEOUT_SECS / POLL_INTERVAL.as_secs();
    for i in 0..max_polls {
        if session.poll_once()? {
            return session.finish();
        }
        if i == 0 {
            println!("等待扫码授权（最长 {} 秒）…", POLL_TIMEOUT_SECS);
        }
        print!(".");
        std::io::stdout().flush().ok();
        std::thread::sleep(POLL_INTERVAL);
    }
    println!();
    Err(anyhow!(
        "等待扫码授权超时（{} 秒），请重试",
        POLL_TIMEOUT_SECS
    ))
}

fn fetch_token(
    client: &reqwest::blocking::Client,
    broker_base: &str,
    state: &str,
) -> Result<BrokerToken> {
    let token: TokenResponse = client
        .get(format!("{}/auth/token", broker_base))
        .query(&[("state", state)])
        .send()
        .context("请求 broker /auth/token 失败")?
        .error_for_status()
        .context("broker /auth/token 返回错误")?
        .json()
        .context("解析 broker /auth/token 响应失败")?;
    Ok(BrokerToken {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        user: token.user.and_then(|u| u.login),
    })
}

/// 终端渲染二维码（Unicode 半块，Dense1x2）；URL 过长（QR 容量不足）时返回 None。
pub fn render_qr(url: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    use qrcode::QrCode;
    let code = QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Dark)
            .light_color(Dense1x2::Light)
            .quiet_zone(true)
            .build(),
    )
}

/// broker /auth/check 的授权判定：兼容多种响应形状。
fn response_is_authorized(value: &serde_json::Value) -> bool {
    value.get("valid").and_then(|v| v.as_bool()) == Some(true)
        || value.get("authorized").and_then(|v| v.as_bool()) == Some(true)
        || value
            .get("status")
            .and_then(|v| v.as_str())
            .is_some_and(|s| matches!(s, "authorized" | "success" | "done"))
}

/// 去掉 force_login=true（复用浏览器里已登录的 GitCode 会话，避免强制重新登录）
fn strip_force_login(url: &str) -> String {
    url.replace("&force_login=true", "")
        .replace("?force_login=true&", "?")
        .replace("?force_login=true", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_broker_base_flag_priority() {
        // 显式参数优先于环境变量
        std::env::set_var("GITCODE_BROKER", "https://env-broker.example");
        let base = broker_base_url(Some("https://flag-broker.example/")).unwrap();
        assert_eq!(base, "https://flag-broker.example");
        std::env::remove_var("GITCODE_BROKER");
    }

    #[test]
    fn test_broker_base_from_env() {
        std::env::set_var("GITCODE_BROKER", "https://env-broker.example/");
        let base = broker_base_url(None).unwrap();
        assert_eq!(base, "https://env-broker.example");
        std::env::remove_var("GITCODE_BROKER");
    }

    #[test]
    fn test_broker_base_missing_errors() {
        std::env::remove_var("GITCODE_BROKER");
        assert!(broker_base_url(None).is_err());
        assert!(broker_base_url(Some("  ")).is_err());
    }

    #[test]
    fn test_response_is_authorized_shapes() {
        assert!(response_is_authorized(&serde_json::json!({"valid": true})));
        assert!(response_is_authorized(
            &serde_json::json!({"authorized": true})
        ));
        assert!(response_is_authorized(
            &serde_json::json!({"status": "authorized"})
        ));
        assert!(response_is_authorized(
            &serde_json::json!({"status": "success"})
        ));
        assert!(response_is_authorized(
            &serde_json::json!({"status": "done"})
        ));
        assert!(!response_is_authorized(
            &serde_json::json!({"valid": false})
        ));
        assert!(!response_is_authorized(
            &serde_json::json!({"status": "pending"})
        ));
        assert!(!response_is_authorized(&serde_json::json!({})));
    }

    #[test]
    fn test_strip_force_login() {
        assert_eq!(
            strip_force_login("https://gitcode.com/oauth?provider=gitcode&force_login=true"),
            "https://gitcode.com/oauth?provider=gitcode"
        );
        assert_eq!(
            strip_force_login("https://gitcode.com/oauth?force_login=true&provider=gitcode"),
            "https://gitcode.com/oauth?provider=gitcode"
        );
        assert_eq!(
            strip_force_login("https://gitcode.com/oauth?force_login=true"),
            "https://gitcode.com/oauth"
        );
    }

    #[test]
    fn test_render_qr_visible_output() {
        // 典型登录 URL 能渲染出非空终端二维码
        let rendered = render_qr("https://gitcode.com/oauth/authorize?state=abc123").unwrap();
        assert!(!rendered.trim().is_empty());
        // 过长的 URL 超出 QR 容量时返回 None 而不是 panic
        assert!(render_qr(&"x".repeat(5000)).is_none());
    }
}
