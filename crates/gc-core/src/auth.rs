//! `gc auth login` —— GitCode 登录。
//!
//! 交互流程参考 gh (cli/cli) 的 `auth login`：先选认证方式（浏览器 / 粘贴 token），
//! 不采用设备码。浏览器流复用**已登录 GitCode 的浏览器**，自动抓取网页端 Cookie
//! （`GITCODE_ACCESS_TOKEN` / `GITCODE_REFRESH_TOKEN`）：
//! - Chromium 系：以用户默认 profile + 调试端口启动，走 CDP（WebSocket）读全部 Cookie；
//! - Firefox：直接读默认 profile 的明文 `cookies.sqlite`。
//! 抓到后写入配置，再利用 API 自动拉取并展示当前账户的完整信息。

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 登录时打开的 GitCode 页面（任意需要登录态即可，Cookie 与页面无关）
const LOGIN_URL: &str = "https://atomgit.com/toads/gitcode-cil/forks";
const ACCESS_COOKIE: &str = "GITCODE_ACCESS_TOKEN";
const REFRESH_COOKIE: &str = "GITCODE_REFRESH_TOKEN";

// ---------- 交互提示 ----------

/// 编号选择题：打印选项，读一行输入；空输入取默认值。
fn prompt_choice(question: &str, options: &[&str], default: usize) -> Result<usize> {
    println!("? {}", question);
    for (i, opt) in options.iter().enumerate() {
        let mark = if i == default { "*" } else { " " };
        println!("  {} [{}] {}", mark, i + 1, opt);
    }
    loop {
        print!("请选择 [{}]: ", default + 1);
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(default);
        }
        let t = line.trim();
        if t.is_empty() {
            return Ok(default);
        }
        if let Ok(n) = t.parse::<usize>() {
            if (1..=options.len()).contains(&n) {
                return Ok(n - 1);
            }
        }
        eprintln!("无效输入，请输入 1-{}", options.len());
    }
}

/// 是/否确认（空输入取默认值）
fn confirm(question: &str, default: bool) -> Result<bool> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let t = line.trim().to_lowercase();
    Ok(match t.as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

// ---------- 浏览器探测 / 安装 ----------

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Chromium,
    Firefox,
}

struct Browser {
    name: &'static str,
    bin: &'static str,
    family: Family,
    /// Chromium 系的 user-data-dir（profile 根目录）
    profile: Option<PathBuf>,
}

fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("无法确定主目录"))
}

fn in_path(bin: &str) -> bool {
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths).any(|d| d.join(bin).is_file()),
        None => false,
    }
}

/// 按常见安装位置探测浏览器（Linux），按 profile 目录去重。
fn detect_browsers() -> Vec<Browser> {
    let Ok(h) = home() else { return Vec::new() };
    let candidates: [(&str, &str, Family, Option<&str>); 9] = [
        (
            "Chrome",
            "google-chrome",
            Family::Chromium,
            Some(".config/google-chrome"),
        ),
        (
            "Chrome",
            "google-chrome-stable",
            Family::Chromium,
            Some(".config/google-chrome"),
        ),
        (
            "Chromium",
            "chromium",
            Family::Chromium,
            Some(".config/chromium"),
        ),
        (
            "Chromium",
            "chromium-browser",
            Family::Chromium,
            Some(".config/chromium"),
        ),
        (
            "Microsoft Edge",
            "microsoft-edge",
            Family::Chromium,
            Some(".config/microsoft-edge"),
        ),
        (
            "Brave",
            "brave-browser",
            Family::Chromium,
            Some(".config/brave-browser"),
        ),
        (
            "Vivaldi",
            "vivaldi",
            Family::Chromium,
            Some(".config/vivaldi"),
        ),
        ("Opera", "opera", Family::Chromium, Some(".config/opera")),
        ("Firefox", "firefox", Family::Firefox, None),
    ];
    let mut out: Vec<Browser> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (name, bin, family, rel) in candidates {
        if !in_path(bin) {
            continue;
        }
        // Chromium 系按 profile 目录去重（同一浏览器可能有多个软链名）
        let key = match family {
            Family::Chromium => rel
                .map(|r| h.join(r).to_string_lossy().to_string())
                .unwrap_or_else(|| bin.to_string()),
            Family::Firefox => format!("firefox:{bin}"),
        };
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(Browser {
            name,
            bin,
            family,
            profile: rel.map(|r| h.join(r)),
        });
    }
    out
}

/// 没有浏览器时自动安装（apt / dnf / pacman / zypper，必要时 sudo）。
fn install_browser() -> Result<()> {
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        })
        .map(|uid| uid == 0)
        .unwrap_or(false);
    let plans: [(&str, &[&str]); 4] = [
        ("apt-get", &["install", "-y", "chromium"]),
        ("dnf", &["install", "-y", "chromium-browser"]),
        ("pacman", &["-Sy", "--noconfirm", "chromium"]),
        ("zypper", &["install", "-y", "chromium"]),
    ];
    for (pm, args) in plans {
        if !in_path(pm) {
            continue;
        }
        let mut cmd = std::process::Command::new(if is_root { pm } else { "sudo" });
        if !is_root {
            cmd.arg(pm);
        }
        cmd.args(args);
        eprintln!("正在通过 {} 安装 chromium（可能需要输入密码）…", pm);
        match cmd.status() {
            Ok(s) if s.success() => return Ok(()),
            _ => eprintln!("{} 安装失败，尝试下一个包管理器…", pm),
        }
    }
    Err(anyhow!(
        "自动安装失败。请手动安装浏览器（chromium / firefox）后重试"
    ))
}

// ---------- Chromium 系：CDP 抓 Cookie ----------

/// 取一个空闲的本地端口
fn free_port() -> Result<u16> {
    let l =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| anyhow!("获取空闲端口失败: {e}"))?;
    Ok(l.local_addr()
        .map_err(|e| anyhow!("读取端口失败: {e}"))?
        .port())
}

/// 以用户默认 profile + 调试端口启动 Chromium 系浏览器，打开登录页，
/// 通过 CDP（WebSocket）的 `Network.getAllCookies` 读出 GitCode Cookie，结束后关掉进程。
fn chromium_cookies(browser: &Browser, port: u16) -> Result<Vec<(String, String)>> {
    let profile = browser
        .profile
        .clone()
        .ok_or_else(|| anyhow!("内部错误：缺少 profile 目录"))?;
    let mut child = std::process::Command::new(browser.bin)
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--user-data-dir")
        .arg(&profile)
        .args(["--no-first-run", "--no-default-browser-check"])
        .arg(LOGIN_URL)
        .spawn()
        .map_err(|e| anyhow!("启动 {} 失败: {e}", browser.bin))?;

    eprintln!(
        "已启动 {}（使用你的默认 profile），正在等待调试端口就绪…",
        browser.name
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("创建 HTTP 客户端失败")?;
    let base = format!("http://127.0.0.1:{}", port);
    let mut version: Option<Value> = None;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        if child.try_wait().context("等待浏览器进程")?.is_some() {
            return Err(anyhow!(
                "{} 进程已退出——通常是因为你已打开了这个浏览器（profile 被占用）。请关闭它后重试",
                browser.name
            ));
        }
        match client.get(format!("{base}/json/version")).send() {
            Ok(resp) if resp.status().is_success() => {
                version = Some(resp.json::<Value>().context("解析 CDP 版本响应失败")?);
                break;
            }
            _ => {}
        }
    }
    let version = match version {
        Some(v) => v,
        None => {
            let _ = child.kill();
            return Err(anyhow!(
                "等待 {} 调试端口超时——通常是因为你已打开了这个浏览器（profile 被占用）。请关闭它后重试",
                browser.name
            ));
        }
    };
    let ws_url = version["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| anyhow!("CDP 响应缺少 webSocketDebuggerUrl"))?;

    let (mut sock, _) = tungstenite::connect(ws_url).map_err(|e| {
        let _ = child.kill();
        anyhow!("连接 CDP WebSocket 失败: {e}")
    })?;
    sock.send(tungstenite::Message::Text(
        json!({"id": 1, "method": "Network.getAllCookies"}).to_string(),
    ))
    .map_err(|e| {
        let _ = child.kill();
        anyhow!("发送 CDP 请求失败: {e}")
    })?;
    let mut found: Vec<(String, String)> = Vec::new();
    for _ in 0..100 {
        let m = sock.read().map_err(|e| {
            let _ = child.kill();
            anyhow!("读取 CDP 响应失败: {e}")
        })?;
        let Ok(text) = m.to_text() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        if v["id"].as_u64() == Some(1) {
            if let Some(arr) = v["result"]["cookies"].as_array() {
                for c in arr {
                    let Some(name) = c["name"].as_str() else {
                        continue;
                    };
                    if name == ACCESS_COOKIE || name == REFRESH_COOKIE {
                        if let Some(val) = c["value"].as_str() {
                            if !val.is_empty() {
                                found.push((name.to_string(), val.to_string()));
                            }
                        }
                    }
                }
            }
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(found)
}

// ---------- Firefox：明文 sqlite 读 Cookie ----------

/// Firefox 默认 profile 目录：优先解析 profiles.ini，回退找最新的 *.default* 目录。
fn default_firefox_profile(base: &Path) -> Option<PathBuf> {
    if let Ok(ini) = std::fs::read_to_string(base.join("profiles.ini")) {
        let mut in_profile = false;
        let mut is_default = false;
        let mut rel: Option<String> = None;
        let mut absolute: Option<String> = None;
        for line in ini.lines() {
            let t = line.trim();
            if let Some(sec) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                in_profile = sec.starts_with("Profile") && sec != "Profiles";
                is_default = false;
                rel = None;
                absolute = None;
                continue;
            }
            if !in_profile {
                continue;
            }
            if t == "Default=1" || t == "IsDefault=1" {
                is_default = true;
            }
            if let Some(v) = t.strip_prefix("Path=") {
                if v.starts_with('/') {
                    absolute = Some(v.to_string());
                } else {
                    rel = Some(v.to_string());
                }
            }
        }
        if is_default {
            if let Some(p) = absolute {
                let pb = PathBuf::from(p);
                if pb.exists() {
                    return Some(pb);
                }
            } else if let Some(p) = rel {
                let pb = base.join(p);
                if pb.exists() {
                    return Some(pb);
                }
            }
        }
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(base) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.contains(".default") || !e.path().is_dir() {
                continue;
            }
            let Ok(st) = e.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if best.as_ref().map_or(true, |(bt, _)| st > *bt) {
                best = Some((st, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// 读 Firefox 默认 profile 的 cookies.sqlite（先拷贝再读，避免 Firefox 运行中锁库）。
fn firefox_cookies() -> Result<Vec<(String, String)>> {
    let h = home()?;
    let base = h.join(".mozilla/firefox");
    let profile_dir = default_firefox_profile(&base).ok_or_else(|| {
        anyhow!(
            "未找到 Firefox 默认 profile（{}）。请确认已安装并使用过 Firefox",
            base.display()
        )
    })?;
    let db = profile_dir.join("cookies.sqlite");
    if !db.exists() {
        return Err(anyhow!("{} 不存在——请确认已使用过 Firefox", db.display()));
    }
    let tmp = std::env::temp_dir().join(format!("gc-cookies-{}.sqlite", std::process::id()));
    std::fs::copy(&db, &tmp)
        .with_context(|| format!("读取 Firefox cookie 数据库 {} 失败", db.display()))?;
    let result = (|| -> Result<Vec<(String, String)>> {
        let conn = rusqlite::Connection::open(&tmp)?;
        let mut stmt = conn.prepare(&format!(
            "SELECT name, value FROM moz_cookies WHERE name IN ('{ACCESS_COOKIE}', '{REFRESH_COOKIE}')"
        ))?;
        let rows = stmt.query_map([], |r| {
            let n: String = r.get(0)?;
            let v: String = r.get(1)?;
            Ok((n, v))
        })?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter(|(_, v)| !v.is_empty())
            .collect())
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

// ---------- 账户信息 ----------

/// 登录成功后：利用 API 自动拉取当前账户的信息并展示（gh auth login 风格）。
pub fn show_account(api: &crate::api::ApiClient, jwt: Option<&str>) {
    match api.authenticated_user() {
        Ok(u) => {
            let login = u["login"].as_str().unwrap_or("").to_string();
            println!("Logged in to gitcode.com as {}", login);
            println!();
            println!("账户:");
            println!("  用户名:  {}", u["login"].as_str().unwrap_or("?"));
            if let Some(n) = u["name"].as_str() {
                if !n.is_empty() {
                    println!("  姓名:    {n}");
                }
            }
            if let Some(e) = u["email"].as_str() {
                if !e.is_empty() {
                    println!("  邮箱:    {e}");
                }
            }
            if let Some(a) = u["avatar_url"].as_str() {
                if !a.is_empty() {
                    println!("  头像:    {a}");
                }
            }
            if !login.is_empty() {
                println!("  主页:    https://gitcode.com/{login}");
            }
            if let Ok(p) = crate::api::user_profile(jwt.unwrap_or(""), &login) {
                if !p.nickname.is_empty() {
                    println!("  昵称:    {}", p.nickname);
                }
                if let Some(d) = &p.profile.description {
                    if !d.is_empty() {
                        println!("  简介:    {d}");
                    }
                }
                if let Some(w) = &p.profile.website {
                    if !w.is_empty() {
                        println!("  网站:    {w}");
                    }
                }
                if let Some(c) = &p.profile.company {
                    if !c.is_empty() {
                        println!("  公司:    {c}");
                    }
                }
                if let Some(l) = &p.profile.location {
                    if !l.is_empty() {
                        println!("  位置:    {l}");
                    }
                }
                if let Some(g) = &p.github_account {
                    if !g.is_empty() {
                        println!("  GitHub:  {g}");
                    }
                }
            }
            if let Ok(emails) = api.user_emails() {
                if let Some(arr) = emails.as_array() {
                    let list: Vec<&str> = arr
                        .iter()
                        .filter_map(|e| e.as_str().or_else(|| e["email"].as_str()))
                        .collect();
                    if !list.is_empty() {
                        println!("  邮箱列表: {}", list.join(", "));
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("提示: 无法通过 v5 API 获取当前账户信息: {e}");
        }
    }
    println!();
    if let Ok(path) = crate::config::config_path() {
        println!("token 已保存到 {}", path.display());
    }
}

// ---------- 登录入口 ----------

/// `gc auth login --qr`：OAuth broker 扫码登录。
/// broker 返回的 access token 直接存为 PAT，随后走与手动登录一致的验证/展示。
pub fn qr_login(broker: Option<&str>, user: Option<&str>) -> Result<()> {
    let base = crate::qr_login::broker_base_url(broker)?;
    let session = crate::qr_login::start_login(&base)?;
    println!("请用已登录 GitCode 的手机 / 浏览器扫描下方二维码：");
    if let Some(rendered) = crate::qr_login::render_qr(session.url()) {
        println!("{rendered}");
    } else {
        println!("（链接过长，无法渲染二维码，请直接打开下方链接）");
    }
    println!("如无法扫码，请打开: {}", session.url());

    let token = crate::qr_login::wait_for_authorization(&session)?;
    if token.access_token.trim().is_empty() {
        anyhow::bail!("broker 返回的 access token 为空");
    }
    let name = token
        .user
        .clone()
        .unwrap_or_else(|| "GitCode 用户".to_string());
    crate::config::save_token_for(user, &token.access_token)?;
    if let Some(u) = user {
        println!("已扫码登录: {name}（保存为用户 #{u} 并切换）");
    } else {
        println!("已扫码登录: {name}");
    }
    if token.refresh_token.is_some() {
        println!("提示: broker 提供了 refresh token，但 gc 目前不自动刷新 PAT，过期后请重新扫码");
    }
    let api = crate::api::ApiClient::new(crate::config::api_base(), Some(token.access_token))?;
    show_account(&api, None);
    Ok(())
}

/// 读一行文本输入（空输入报错）
fn prompt_text(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        anyhow::bail!("未输入内容");
    }
    let t = line.trim().to_string();
    if t.is_empty() {
        anyhow::bail!("用户名为空");
    }
    Ok(t)
}

/// `gc auth login`：
/// - `--qr`：OAuth broker 扫码登录；
/// - `--with-token`：从 stdin 读 token（脚本场景，与 gh 一致）；
/// - `user`：Some(名字) 时 token 存为该用户档案并切换（`gc auth add <名字>` 即此路径）；
/// - TTY：交互式选择「浏览器登录」或「粘贴 token」；已有用户时先问「重新登录 / 添加新账号」；
/// - 非 TTY 且无参数：报错，要求显式传参。
pub fn login(
    web: bool,
    with_token: bool,
    qr: bool,
    broker: Option<&str>,
    user: Option<&str>,
) -> Result<()> {
    if qr && (web || with_token) {
        anyhow::bail!("--qr 与 --web / --with-token 只能使用其一");
    }
    if qr {
        return qr_login(broker, user);
    }
    if with_token && web {
        anyhow::bail!("--web 与 --with-token 只能使用其一");
    }
    if with_token {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| anyhow!("读取 stdin 失败: {e}"))?;
        let token = line.trim();
        if token.is_empty() {
            anyhow::bail!("stdin 中未读到 token");
        }
        return token_login(token, user);
    }

    let tty = std::io::stdin().is_terminal();
    // gh 风格：已有用户且未指定目标时，先问「重新登录当前账号 / 添加新账号」
    let user: Option<String> = match user {
        Some(u) => Some(u.to_string()),
        None if tty && !crate::config::list_users()?.is_empty() => {
            let active = crate::config::active_user()?.unwrap_or_default();
            let opt = prompt_choice(
                &format!("已有用户（当前激活: {active}）。如何处理？"),
                &["重新登录当前账号", "添加新账号"],
                0,
            )?;
            if opt == 1 {
                Some(prompt_text("新账号名字: ")?)
            } else {
                None
            }
        }
        None => None,
    };
    let user = user.as_deref();

    let mode = if !tty {
        if web {
            0
        } else {
            anyhow::bail!("非交互环境：请使用 --web 走浏览器登录，或用 --with-token 传入 token");
        }
    } else {
        let options = ["使用浏览器登录（推荐）", "粘贴访问令牌 (Token)"];
        prompt_choice("你希望如何认证 GitCode (gitcode.com)?", &options, 0)?
    };

    if mode == 1 {
        let token = rpassword::prompt_password_stderr("请输入 GitCode 个人访问令牌: ")
            .map_err(|e| anyhow!("读取 token 失败: {e}"))?;
        return token_login(token.trim(), user);
    }
    browser_login(user)
}

/// 粘贴 token 登录：保存 + 验证 + 展示账户信息。
/// `user` 为 Some 时存为该（新）档案并切换。
fn token_login(token: &str, user: Option<&str>) -> Result<()> {
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("token 为空，已取消");
    }
    crate::config::save_token_for(user, &token)?;
    let api = crate::api::ApiClient::new(crate::config::api_base(), Some(token))?;
    show_account(&api, None);
    Ok(())
}

/// 浏览器登录：选浏览器 → 自动抓 GitCode Cookie → 存配置 → 拉取账户信息。
/// `user` 为 Some 时 PAT 存为该（新）档案并切换。
fn browser_login(user: Option<&str>) -> Result<()> {
    let mut browsers = detect_browsers();
    if browsers.is_empty() {
        println!("未检测到可用浏览器（chrome / chromium / edge / brave / firefox 等）。");
        let ok = confirm("是否自动安装 chromium？[y/N] ", false)?;
        if ok {
            install_browser()?;
            browsers = detect_browsers();
            if browsers.is_empty() {
                anyhow::bail!("安装后仍未检测到浏览器，请手动安装后重试");
            }
        } else {
            anyhow::bail!("没有可用浏览器。请手动安装浏览器（chromium / firefox）后重试");
        }
    }

    let names: Vec<&str> = browsers.iter().map(|b| b.name).collect();
    println!("检测到以下浏览器:");
    let sel = prompt_choice(
        "请选择一个已经登录过 GitCode 的浏览器（如果它正在运行请先关闭）",
        &names,
        0,
    )?;
    let browser = &browsers[sel];

    eprintln!(
        "提示: 将使用 {} 的默认 profile 打开 {LOGIN_URL} ，请确认该浏览器已登录 GitCode。",
        browser.name
    );
    let cookies = match browser.family {
        Family::Chromium => chromium_cookies(browser, free_port()?)?,
        Family::Firefox => firefox_cookies()?,
    };

    let mut access: Option<String> = None;
    let mut refresh: Option<String> = None;
    for (n, v) in &cookies {
        match n.as_str() {
            ACCESS_COOKIE => access = Some(v.clone()),
            REFRESH_COOKIE => refresh = Some(v.clone()),
            _ => {}
        }
    }
    let Some(access) = access else {
        anyhow::bail!(
            "该浏览器中未找到 GitCode 登录态（缺少 {ACCESS_COOKIE} cookie）。请确认已在这个浏览器里登录过 GitCode 后重试"
        );
    };
    let refresh = refresh.unwrap_or_default();

    // 先落盘网页端 token（讨论 / 文件下载等 v1 接口用）
    crate::config::save_web_tokens(&access, &refresh)?;

    // 网页端 JWT 在 v5 API 可能同样可用：试一下，成功则一并存为 API token
    let api = crate::api::ApiClient::new(crate::config::api_base(), Some(access.clone()))?;
    match api.authenticated_user() {
        Ok(_) => {
            let _ = crate::config::save_token_for(user, &access);
        }
        Err(_) => {
            eprintln!(
                "提示: 网页端 token 暂不能直接用于 v5 API，API 命令仍会使用已有 token。\
                 如还没有，可到 GitCode 生成个人访问令牌后运行 `gc token set`。"
            );
        }
    }
    show_account(&api, Some(&access));
    Ok(())
}
