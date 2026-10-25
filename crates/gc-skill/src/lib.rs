//! `gc-skill` —— AI 工具技能域：检测并安装 gc 技能（顶层别名 extension / extsion）。

use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
pub struct SkillCmd {
    #[command(subcommand)]
    pub action: SkillAction,
}
#[derive(Subcommand)]
pub enum SkillAction {
    /// 安装 gc 技能到检测到的 AI 工具（不给参数即自动检测全部；--all 强制装到全部 6 个工具）
    Install {
        /// 指定工具名（claude-code / codex / opencode / codebuddy / gemini / atomcode，可多个，跳过检测）
        tools: Vec<String>,
        /// 强制安装到全部 6 个工具（无论是否检测到）
        #[arg(long)]
        all: bool,
        /// 覆盖已存在的技能文件
        #[arg(long)]
        force: bool,
    },
    /// 查看各 AI 工具的检测状态与 gc 技能安装情况
    List,
}

/// 支持的 AI 工具表：(名称, 家目录下的配置子目录, 说明)
pub fn skill_tools() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("claude-code", ".claude", "Claude Code"),
        ("codex", ".codex", "Codex"),
        ("opencode", ".config/opencode", "OpenCode"),
        ("codebuddy", ".codebuddy", "CodeBuddy CLI"),
        ("gemini", ".gemini", "Gemini CLI"),
        ("atomcode", ".atomcode", "AtomCode"),
    ]
}

/// 某工具的标准根目录（家目录 + 配置子目录）
pub fn tool_standard_dir(name: &str) -> Option<std::path::PathBuf> {
    dirs::home_dir().and_then(|h| {
        skill_tools()
            .into_iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, sub, _)| h.join(sub))
    })
}

/// 检测某工具的根目录（opencode 额外探测 ~/.opencode）
pub fn detect_tool_dir(name: &str) -> Option<std::path::PathBuf> {
    if let Some(d) = tool_standard_dir(name) {
        if d.exists() {
            return Some(d);
        }
    }
    if name == "opencode" {
        if let Some(h) = dirs::home_dir() {
            let alt = h.join(".opencode");
            if alt.exists() {
                return Some(alt);
            }
        }
    }
    None
}

/// gc 技能文件内容（SKILL.md，含 Claude 式 frontmatter，其他工具也可读）
pub fn skill_md() -> String {
    format!(
        r#"---
name: gc
description: 用 gc 命令行管理 GitCode/AtomGit 仓库、Issue、PR、Release 与讨论；查仓库内容、免密推送、透传 API。当用户要求操作 GitCode/AtomGit（建仓、提 PR、查 issue、grep 代码、推送等）时使用。
---

# gc — GitCode (AtomGit) 命令行

`gc` 是 GitCode/AtomGit 的 `gh` 风格 CLI。先决条件：PAT 已配置（`gc token set`）。

## 认证与安装

```bash
gc auth              # 检查 PAT 是否有效
gc auth pal          # 直接输出 PAT（可管道）
gc auth git          # 注册 git 凭据助手，gitcode/atomgit 的 push/pull 免密
gc auth user list    # 列出全部用户令牌（编号 + 激活标记）
gc auth switch 2     # 切换激活用户（序号或名字均可）
gc auth add work     # 添加新用户并登录（gh auth add 风格，自动切换）
gc auth login --user work --with-token   # 脚本场景：登录到指定用户
gc auth user add x   # 新增一个用户令牌（隐藏输入，自动切换）
gc auth user remove work                 # 删除用户（序号或名字）
gc web-token set        # 录入网页端 JWT 到当前激活用户（别名 webtoken）
gc web-token list       # 查看各用户的网页端 token 状态
gc web-token clear      # 清除当前用户的网页端 token
gc install --system  # 安装二进制到 /usr/local/bin（自动 sudo）
```

## 常用命令

```bash
# 仓库
gc repo view <owner/name>            # 摘要 + README/LICENSE
gc repo create <name> --private      # 建仓
gc repo grep "<regex>" --include=*.rs --repo owner/name   # 内容搜索（SHA 缓存增量下载）

# Issue / PR（仓库参数可省略：在 git 仓库内自动推断）
gc issue list [owner/name] --state open
gc issue create --title "..." --file body.md
gc pr list --web --order-by merged_at --count
gc pr create --head feature --base main --file body.md
gc pr checkout 12                    # 检出 PR
gc pr approval 12 --action approve   # 审批 / reset 撤销
gc pr cherry-pick 12                 # 已合并 PR 摘樱桃 / revert 回滚
gc pr comment add 12 --body "..."    # 评论（--web 走网页端通道）

# 结构化输出（jq 完整语法，jaq 引擎）
gc pr list owner/name --jq '.[] | select(.state == "open") | {{number, title}}'
gc repo view owner/name --json

# 万能逃生舱：v5 裸 API 透传
gc api -m POST -p /repos/owner/name/issues -b '{{"title":"test"}}'
```

## 注意

- 仓库参数支持 owner/name、完整 URL（gitcode.com / atomgit.com / git@ 均可）。
- 网页端接口（讨论 / Issue v2 / PR cherry-pick 等）需要 `gc web-token set` 录入浏览器 JWT。
- 完整文档：`gc --help`、`gc <子命令> --help`。
"#
    )
}

/// `gc skill install` 实现：把 SKILL.md 写入各 AI 工具的技能目录
pub fn run_skill_install(tools: &[String], all: bool, force: bool) -> Result<()> {
    let content = skill_md();
    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for (name, _, label) in skill_tools() {
        let forced = all || tools.iter().any(|t| t == name);
        let dir = detect_tool_dir(name);
        if dir.is_none() && !forced {
            skipped.push((name, label, "未检测到"));
            continue;
        }
        // 未检测到但被强制指定：仍写入其标准家目录
        let root = dir.or_else(|| tool_standard_dir(name));
        let Some(root) = root else {
            skipped.push((name, label, "无 HOME 目录"));
            continue;
        };
        let skill_path = root.join("skills/gc/SKILL.md");
        if skill_path.exists() && !force {
            skipped.push((name, label, "已安装（--force 覆盖）"));
            continue;
        }
        std::fs::create_dir_all(skill_path.parent().unwrap())
            .with_context(|| format!("创建 {} 失败", skill_path.parent().unwrap().display()))?;
        std::fs::write(&skill_path, &content)
            .with_context(|| format!("写入 {} 失败", skill_path.display()))?;
        installed.push((name, label, skill_path.clone()));
        println!("✅ {}（{}）：{}", name, label, skill_path.display());
    }

    if installed.is_empty() {
        println!();
        println!("没有安装任何工具的技能。检测方式：各工具的配置目录（如 ~/.claude、~/.codex）是否存在。");
        println!("可用 --all 强制全部安装，或指定名称：gc skill install claude-code codex");
    } else {
        println!();
        println!(
            "共安装 {} 个工具，跳过 {} 个。",
            installed.len(),
            skipped.len()
        );
        for (name, label, reason) in &skipped {
            println!("  ⏭ {}（{}）：{}", name, label, reason);
        }
    }
    Ok(())
}

/// `gc skill list`：展示检测状态与安装情况
pub fn run_skill_list() -> Result<()> {
    let content = skill_md();
    println!("{:<12} {:<6} {:<10} 路径", "工具", "检测", "技能");
    for (name, _, label) in skill_tools() {
        let dir = detect_tool_dir(name);
        let skill_path = match &dir {
            Some(d) => Some(d.join("skills/gc/SKILL.md")),
            None => None,
        };
        let installed = skill_path.as_ref().map(|p| p.exists()).unwrap_or(false);
        let up_to_date = installed
            && std::fs::read_to_string(skill_path.as_ref().unwrap())
                .map(|old| old == content)
                .unwrap_or(false);
        println!(
            "{:<12} {:<6} {:<10} {}",
            name,
            if dir.is_some() { "✅" } else { "—" },
            if installed {
                if up_to_date {
                    "最新"
                } else {
                    "旧版可更新"
                }
            } else {
                "未安装"
            },
            skill_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
    }
    println!();
    println!("安装：gc skill install [工具名...] [--all] [--force]");
    Ok(())
}
