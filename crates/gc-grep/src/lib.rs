//! `gc-grep` —— 仓库内容搜索域。
//!
//! 当前阶段：把 [`gc_core::api`] 中供 grep 使用的文件树 / 下载 API
//! 以本 crate 命名空间重新导出（`repo grep` 的 ripgrep 搜索与 SHA
//! 稀疏缓存逻辑在 gc-cli，后续迁入本 crate）。

pub use gc_core::api::{download_files, download_tree, repo_file_content, repo_file_list, WebFile};

use anyhow::{anyhow, Context as _, Result};
use gc_core::api::ApiClient;
use gc_core::ctx::web_access_token;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `gc repo grep` 实现：列出仓库文件 → 按 --include 通配过滤 → rayon 并行下载到临时目录
/// → 用 grep crate（ripgrep 库版）做行级正则搜索 → 输出 file:line: text。
pub fn run_repo_grep(
    api: &ApiClient,
    owner: &str,
    name: &str,
    pattern: &str,
    include: &[String],
    branch: &str,
    ignore_case: bool,
) -> Result<()> {
    use grep::searcher::SearcherBuilder;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let token = web_access_token()?;
    let files = gc_core::api::repo_file_list(&token, owner, name, branch)?;
    if files.is_empty() {
        anyhow::bail!("仓库 {}/{} 分支 {} 没有文件", owner, name, branch);
    }
    // --include 通配过滤（* 与 ?，大小写不敏感）
    let matched: Vec<&String> = files
        .iter()
        .filter(|f| include.is_empty() || include.iter().any(|pat| wildcard_match(f, pat)))
        .collect();
    if matched.is_empty() {
        println!("（--include 过滤后没有文件）");
        return Ok(());
    }

    // ---- 稀疏缓存下载 ----
    // 1) v5 git trees 拿每个文件的 blob SHA；
    // 2) 与缓存目录里的 sha 记录对比：文件存在且 SHA 未变 → 直接复用本地缓存；
    // 3) 只下载缺失 / 有变化的文件（rayon 并行），下载后更新 SHA 记录。
    // 缓存位于 ~/.cache/gc/grep/{owner}/{name}/{branch}/，同一分支多次 grep 时大幅减少网络量。
    let tree = api.repo_tree_entries(owner, name, branch)?;
    let tree_map: std::collections::HashMap<&str, &str> =
        tree.iter().map(|(p, s)| (p.as_str(), s.as_str())).collect();

    let cache_root = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("gc/grep")
        .join(owner)
        .join(name)
        .join(branch);
    let files_dir = cache_root.join("files");
    let sha_file = cache_root.join("shas.json");
    let mut cached_shas: std::collections::HashMap<String, String> =
        std::fs::read_to_string(&sha_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

    // 只保留 --include 命中的文件（稀疏：不碰仓库里其他文件）
    let matched: Vec<&str> = tree_map
        .keys()
        .copied()
        .filter(|f| include.is_empty() || include.iter().any(|pat| wildcard_match(f, pat)))
        .collect();
    if matched.is_empty() {
        println!("（--include 过滤后没有文件）");
        return Ok(());
    }

    // 分拣：缓存新鲜 vs 需要下载
    let mut to_download: Vec<(String, String)> = Vec::new(); // (path, local_rel)
    for f in &matched {
        let path: &str = f;
        let need = match cached_shas.get(path) {
            Some(old_sha) => tree_map.get(path) != Some(&old_sha.as_str()),
            None => true,
        };
        if need || !files_dir.join(path).exists() {
            to_download.push((path.to_string(), path.to_string()));
        }
    }
    if !to_download.is_empty() {
        std::fs::create_dir_all(&files_dir).context("创建 grep 缓存目录失败")?;
        eprintln!(
            "下载 {} / {} 个文件（其余命中缓存，未变化跳过）...",
            to_download.len(),
            matched.len()
        );
        let results =
            gc_core::api::download_files(&token, owner, name, branch, &to_download, &files_dir)?;
        for (path, r) in &results {
            if let Err(e) = r {
                eprintln!("警告：{} 下载失败（{}），跳过", path, e);
            }
        }
        // 更新 SHA 记录（只记成功下载的；缓存里多出来的旧记录顺手清掉）
        for (path, r) in &results {
            if r.is_ok() {
                if let Some(sha) = tree_map.get(path.as_str()) {
                    cached_shas.insert(path.clone(), sha.to_string());
                }
            }
        }
        let keep: std::collections::HashSet<&str> = matched.iter().copied().collect();
        cached_shas.retain(|k, _| keep.contains(k.as_str()));
        std::fs::write(&sha_file, serde_json::to_string(&cached_shas)?)
            .context("写入 grep 缓存 SHA 记录失败")?;
    }

    // 在缓存目录上执行搜索（此时所有 matched 文件都已在 files_dir 下且与远端一致）

    let matcher = grep::regex::RegexMatcherBuilder::new()
        .case_insensitive(ignore_case)
        .build(pattern)
        .map_err(|e| anyhow!("正则表达式无效：{}", e))?;
    let mut searcher = grep::searcher::SearcherBuilder::new()
        .binary_detection(grep::searcher::BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    let file_count = AtomicUsize::new(0);
    for path in &matched {
        let local = files_dir.join(path);
        let content = match std::fs::read(&local) {
            Ok(c) => c,
            Err(_) => continue, // 缓存缺失（极少见：下载失败），跳过
        };
        let path_str = path.to_string();
        let matched_any = AtomicUsize::new(0);
        searcher
            .search_slice(
                &matcher,
                &content,
                grep::searcher::sinks::UTF8(|line_no, line| {
                    matched_any.fetch_add(1, Ordering::Relaxed);
                    let text = line.trim_end_matches(['\n', '\r']);
                    println!("{}:{}: {}", path_str, line_no, text);
                    Ok(true)
                }),
            )
            .with_context(|| format!("搜索 {} 失败", path))?;
        if matched_any.load(Ordering::Relaxed) > 0 {
            file_count.fetch_add(1, Ordering::Relaxed);
        }
    }
    let files_hit = file_count.load(Ordering::Relaxed);
    println!();
    println!(
        "（{} 个文件命中，缓存：{}）",
        files_hit,
        files_dir.display()
    );
    Ok(())
}

/// 极简通配符匹配（支持 * 与 ?，大小写不敏感），用于 --include 过滤
pub fn wildcard_match(s: &str, pattern: &str) -> bool {
    fn inner(s: &[u8], p: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            b'*' => {
                for i in 0..=s.len() {
                    if inner(&s[i..], &p[1..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !s.is_empty() && inner(&s[1..], &p[1..]),
            c => !s.is_empty() && s[0].eq_ignore_ascii_case(&c) && inner(&s[1..], &p[1..]),
        }
    }
    inner(s.as_bytes(), pattern.to_lowercase().as_bytes())
}
