//! SIGINFO 支持 —— 在 macOS / *BSD（FreeBSD、OpenBSD、NetBSD、DragonFly）上
//! 按 Ctrl-T（SIGINFO）打印当前操作状态。
//!
//! BSD 惯例：长任务收到 SIGINFO 时向 stderr 报告进度。gc 在启动时安装一个
//! 信号处理器（`install()`）：处理函数只置位一个原子标志（async-signal-safe），
//! 由常驻监控线程轮询该标志并汇总「当前命令 / 已运行时长 / 当前操作 / 进度」
//! 输出到 stderr。SIGINFO 是 macOS 独有信号，其它平台 `install()` 是 no-op，
//! 状态记录本身（`set_status` / `track`）跨平台可用、无副作用。
//!
//! 用法：
//! - 入口处调 [`install`]（见 `gc-cli::cli::run`）
//! - 长操作开始时 [`set_status`]（结束/失败时 [`clear_status`]），
//!   有 indicatif 进度条时顺带 [`track`] 一份，SIGINFO 会带上字节进度

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Instant;

/// 进程启动时刻，SIGINFO 输出里的「已运行」从这算起。
static START: OnceLock<Instant> = OnceLock::new();

/// 当前操作的一句话描述（如「上传 dist/foo.tar.gz」）；None = 无进行中的操作。
static STATUS: RwLock<Option<String>> = RwLock::new(None);

/// 当前操作的进度条（若有）。handler 输出时读 position/length/message。
static BAR: Mutex<Option<indicatif::ProgressBar>> = Mutex::new(None);

/// SIGINFO 到达标志：handler 置位，监控线程消费后清零。
static PENDING: AtomicBool = AtomicBool::new(false);

/// 记录启动时刻；在程序入口尽早调用（`install` 内部也会兜底调用）。
pub fn init() {
    let _ = START.get_or_init(Instant::now);
}

/// 设置当前操作描述（长操作开始时调用）。空串等价于清除。
pub fn set_status(desc: &str) {
    let mut cur = STATUS.write().unwrap();
    if desc.is_empty() {
        *cur = None;
    } else {
        *cur = Some(desc.to_string());
    }
}

/// 清除当前操作描述（操作结束/失败时调用）。
pub fn clear_status() {
    *STATUS.write().unwrap() = None;
}

/// 登记当前操作的进度条，SIGINFO 输出会带上已传字节 / 总量。
pub fn track(bar: indicatif::ProgressBar) {
    *BAR.lock().unwrap() = Some(bar);
}

/// 汇总一行状态文本（SIGINFO 输出与测试共用）。
fn snapshot() -> String {
    let argv: Vec<String> = std::env::args().collect();
    let cmd = if argv.len() > 1 {
        argv[1..].join(" ")
    } else {
        argv.first().map(String::as_str).unwrap_or("gc").to_string()
    };
    let elapsed = START
        .get_or_init(Instant::now)
        .elapsed()
        .as_secs();
    let mut line = format!("[gc] {cmd} — 已运行 {elapsed}s");
    if let Some(st) = STATUS.read().unwrap().as_deref() {
        line.push_str(" — ");
        line.push_str(st);
    }
    if let Some(bar) = BAR.lock().unwrap().as_ref() {
        let total = bar.length().unwrap_or(0);
        let pos = bar.position();
        if total > 0 {
            line.push_str(&format!(
                " — {}/{} 字节（{}%）",
                pos,
                total,
                pos * 100 / total
            ));
        }
    }
    line
}

// SIGINFO 是 macOS / *BSD 独有信号；Linux / Windows 上 install() 是 no-op。
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub fn install() {
    init();
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINFO, &PENDING);
    std::thread::Builder::new()
        .name("siginfo".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if PENDING.swap(false, Ordering::Relaxed) {
                let line = snapshot();
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "{line}");
            }
        })
        .ok();
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub fn install() {
    init(); // 仅记录启动时刻，不注册信号
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_command_and_elapsed() {
        init();
        set_status("测试操作");
        let s = snapshot();
        assert!(s.starts_with("[gc] "), "意外的前缀: {s}");
        assert!(s.contains("已运行 "), "缺耗时: {s}");
        assert!(s.contains("测试操作"), "缺状态: {s}");
        clear_status();
    }
}
