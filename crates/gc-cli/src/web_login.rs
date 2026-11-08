// SPDX-License-Identifier: MIT
//! `gc auth weblogin` —— 外置 Python 登录脚本的 PTY 拉起器。
//!
//! 流程（对应 scripts/gc_web_login.py 的输出协议）：
//! 1. `nix::pty::openpty` 开一个伪终端；
//! 2. 在 PTY 里运行 Python 脚本（Playwright 有头浏览器打开登录页，人工登录）；
//! 3. 父进程从 master 端持续回读脚本输出（stderr 进度日志透传到本终端）；
//! 4. 脚本登录成功并等 Cookie 稳定后，在 stdout 打印哨兵包裹的 JSON；
//! 5. gc 解析 JSON，去重合并写入 SQLite（`cookies` 表），并同步 web token 配置。

use anyhow::{anyhow, Context as _, Result};
use nix::pty::openpty;
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::process::{Command, Stdio};

const BEGIN: &str = "---GC_COOKIES_BEGIN---";
const END: &str = "---GC_COOKIES_END---";

/// 定位登录脚本：环境变量 > 配置目录固定副本 > CWD 逐级向上 > 可执行文件旁。
fn login_script_path() -> Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("GC_LOGIN_SCRIPT") {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("GC_LOGIN_SCRIPT 指向的脚本不存在: {}", path.display());
    }
    // 配置目录的固定副本（`gc install` / 首次运行时落位，装好的二进制也能找到）
    let conf_copy = dirs::config_dir().map(|d| d.join("gc/gc_web_login.py"));
    if let Some(c) = &conf_copy {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    // 从 CWD 逐级向上找仓库里的 scripts/gc_web_login.py（开发场景）
    let mut dir = std::env::current_dir()?;
    for _ in 0..6 {
        let c = dir.join("scripts/gc_web_login.py");
        if c.exists() {
            return Ok(c);
        }
        if !dir.pop() {
            break;
        }
    }
    let exe_dir = std::env::current_exe()?
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let candidates = [
        exe_dir.join("../../scripts/gc_web_login.py"),
        exe_dir.join("../scripts/gc_web_login.py"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "找不到 gc_web_login.py（试过配置目录副本、CWD 逐级向上与 {:?}）；\
         可用 GC_LOGIN_SCRIPT 指定路径，或把脚本放到 {}",
        candidates,
        conf_copy
            .map(|c| format!("{}", c.display()))
            .unwrap_or_else(|| "~/.config/gc/gc_web_login.py".into())
    )
}

/// 把仓库里的登录脚本复制到配置目录做固定副本（覆盖旧副本）。
/// 返回是否成功（失败不致命：CWD 逐级向上仍可能找到）。
pub fn ensure_login_script_copy() -> bool {
    let Some(dest) = dirs::config_dir().map(|d| d.join("gc/gc_web_login.py")) else {
        return false;
    };
    let Ok(mut dir) = std::env::current_dir() else {
        return false;
    };
    let mut src = None;
    for _ in 0..6 {
        let c = dir.join("scripts/gc_web_login.py");
        if c.exists() {
            src = Some(c);
            break;
        }
        if !dir.pop() {
            break;
        }
    }
    let Some(src) = src else { return false };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(&src, &dest).is_ok()
}

fn pick_python() -> &'static str {
    // 有 nix shell 环境优先（flake/shell.nix 里带 playwright），
    // 否则回落系统 python3
    "python3"
}

pub fn run_web_login(headed_hint: bool) -> Result<()> {
    // 首次运行把仓库里的脚本复制到配置目录，之后装好的二进制也能找到
    if ensure_login_script_copy() {
        println!("[gc-login] 已把登录脚本复制到配置目录（后续可直接使用）");
    }
    let script = login_script_path()?;
    let python = pick_python();

    // 1) 开 PTY
    let pty = openpty(None, None).context("openpty 失败")?;
    // dup 三份 slave 给子进程的 stdin/stdout/stderr（各自独立关闭）
    let dup_slave = || nix::unistd::dup(pty.slave.as_raw_fd()).context("dup slave 失败");
    let s_in = dup_slave()?;
    let s_out = dup_slave()?;
    let s_err = dup_slave()?;

    let mut cmd = Command::new(python);
    cmd.arg(&script)
        .stdin(unsafe { Stdio::from_raw_fd(s_in) })
        .stdout(unsafe { Stdio::from_raw_fd(s_out) })
        .stderr(unsafe { Stdio::from_raw_fd(s_err) })
        .env("TERM", "xterm-256color")
        .env("GC_COOKIES_OUT", "pty"); // 明确告知脚本：输出走 PTY
    if !headed_hint {
        cmd.env("GC_LOGIN_HEADLESS", "1");
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("在 PTY 中启动 {python} {} 失败", script.display()))?;
    drop(pty.slave); // 父进程关 slave：子进程退出后 master 读到 EOF

    println!(
        "[gc-login] PTY 已启动 {python} {}（浏览器窗口会自动弹出，请完成登录）",
        script.display()
    );

    // 2) 父进程回读 master（非阻塞 + poll 轮询）：进度透传，收集哨兵段。
    //    不依赖 EIO 语义（部分内核/场景下 slave 关闭后 master 的 read 不返回），
    //    而是以「子进程已退出且 200ms 内无新输出」作为确定性收口条件。
    nix::fcntl::fcntl(
        pty.master.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETFL(
            nix::fcntl::OFlag::O_NONBLOCK,
        ),
    )
    .context("设置 master 非阻塞失败")?;
    let mut collected: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut pollfds = [nix::poll::PollFd::new(
        unsafe { std::os::fd::BorrowedFd::borrow_raw(pty.master.as_raw_fd()) },
        nix::poll::PollFlags::POLLIN,
    )];
    loop {
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        if std::env::var("GC_PTY_DEBUG").is_ok() && exited {
            eprintln!("[pty-debug] 子进程已退出，等待 master 静默收口");
        }
        match nix::poll::poll(&mut pollfds, 200u16) {
            Ok(0) => {
                if exited {
                    eprintln!("[pty-debug] 200ms 无输出且子进程已退出 -> 收口");
                    break; // 子进程已退出且 200ms 无新输出：收口
                }
                continue;
            }
            Ok(_) => match nix::unistd::read(pty.master.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // 进度日志透传到当前终端；哨兵段也顺便透传（用户可见），
                    // 最终以收集到的文本解析为准。
                    std::io::stdout().write_all(chunk).ok();
                    std::io::stdout().flush().ok();
                    collected.extend_from_slice(chunk);
                }
                Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EWOULDBLOCK) => continue,
                Err(nix::errno::Errno::EIO) => break, // Linux：slave 全部关闭
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(anyhow!("读 PTY master 失败: {e}")),
            },
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(anyhow!("poll PTY master 失败: {e}")),
        }
    }
    let status = child.wait()?;

    // 3) 解析哨兵段
    let text = String::from_utf8_lossy(&collected);
    let payload = extract_payload(&text)
        .ok_or_else(|| anyhow!("未在脚本输出中找到 {BEGIN}...{END} 段（登录未完成？）"))?;
    let data: serde_json::Value = serde_json::from_str(payload)
        .with_context(|| format!("哨兵段不是合法 JSON: {}", &payload[..payload.len().min(120)]))?;
    let cookies = data["cookies"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("JSON 里没有 cookies 数组"))?;

    // 4) 去重合并写入 SQLite
    let (written, replaced) = gc_core::db::cookie_merge(&cookies)?;
    gc_core::db::kv_set(
        "web_login_at",
        &time_now_local(),
    )?;
    gc_core::db::kv_set("web_login_page", data["page_url"].as_str().unwrap_or(""))?;

    println!(
        "\n[gc-login] ✓ 完成：脚本回传 {} 条 Cookie，合并写入 SQLite {} 条（覆盖旧值 {replaced} 条）",
        cookies.len(),
        written
    );
    if !status.success() {
        anyhow::bail!("登录脚本退出码非 0: {status}");
    }
    Ok(())
}

/// 从整段 PTY 输出里截取哨兵之间的 JSON 文本。
fn extract_payload(text: &str) -> Option<&str> {
    let start = text.find(BEGIN)? + BEGIN.len();
    let end = text[start..].find(END)? + start;
    let seg = text[start..end].trim();
    // PTY 会把 \n 回显成 \r\n；JSON 本身允许空白，直接交给解析器
    Some(seg)
}

fn time_now_local() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 无 chrono 依赖：粗粒度时间戳即可（入库用）
    format!("{now}")
}

use std::os::unix::io::FromRawFd as _;
