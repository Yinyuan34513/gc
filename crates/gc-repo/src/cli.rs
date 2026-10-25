//! 仓库域：参数定义（RepoCmd/RepoAction 等）与 `Command::Repo` 分发。

use anyhow::{anyhow, Context as _};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use gc_core::api::{self, ApiClient};
use gc_core::config;
use gc_core::ctx::{
    configured_remote, current_git_branch, detect_repo_from_cwd, resolve_repo, split_repo,
    web_access_token,
};
use gc_core::output::{
    edit_title_body, pretty, print_list, print_structured, read_as_base64, render_md, run_jq,
};
use gc_grep::run_repo_grep;
use serde_json::{json, Map, Value};

#[derive(Parser)]
pub struct RepoCmd {
    #[command(subcommand)]
    pub action: RepoAction,
}
/// 共享参数：列出文件树（repo tree 与顶层 tree 共用）
#[derive(Parser)]
#[command(name = "tree")]
pub struct TreeArgs {
    pub repo: String,
    #[arg(long = "ref", default_value = "main")]
    pub branch: String,
}

/// 共享参数：读取单个文件（repo read 与顶层 read 共用）
#[derive(Parser)]
#[command(name = "read")]
pub struct ReadArgs {
    pub repo: String,
    #[arg(help = "仓库内文件路径，如 src/api.rs")]
    pub file: String,
    #[arg(long = "ref", default_value = "main")]
    pub branch: String,
    #[arg(long)]
    pub output: Option<String>,
}

/// 共享参数：下载多个文件（repo download 与顶层 download 共用）
#[derive(Parser)]
#[command(name = "download")]
pub struct DownloadArgs {
    pub repo: String,
    #[arg(help = "一个或多个仓库内文件路径，如 src/api.rs README.md")]
    pub files: Vec<String>,
    #[arg(long = "ref", default_value = "main")]
    pub branch: String,
    #[arg(long, default_value = ".")]
    pub output: String,
}

#[derive(Subcommand)]
pub enum RepoAction {
    /// 查看仓库详情
    View {
        repo: String,
        /// 直接打印完整 JSON（默认打印精简摘要）
        #[arg(long)]
        json: bool,
        /// 用 jq 表达式过滤输出（优先于 --json）
        #[arg(long)]
        jq: Option<String>,
    },
    /// 列出仓库（省略 owner 则列出当前用户）
    List { owner: Option<String> },
    /// 在仓库全部文件内容里做正则搜索（本地临时目录 + rayon 并行下载）
    Grep {
        /// 正则表达式（rust regex 语法）
        pattern: String,
        /// 文件名过滤（可重复，支持 * ? 通配，如 --include=*.rs --include=*.toml）
        #[arg(long = "include", num_args = 0..)]
        include: Vec<String>,
        /// 仓库 owner/name（省略则从当前 git remote 推断）
        #[arg(long)]
        repo: Option<String>,
        /// 显式指定 owner（与 --repo 二选一；此时仓库名用位置参数前无法表达，推荐 --repo）
        #[arg(long)]
        owner: Option<String>,
        /// 分支 / 标签，默认 main
        #[arg(long = "ref", default_value = "main")]
        branch: String,
        /// 忽略大小写
        #[arg(short = 'i')]
        ignore_case: bool,
    },
    /// 克隆仓库到本地（默认克隆到 ./<name>）
    Clone {
        repo: String,
        dest: Option<String>,
        /// 用网页端 v2 接口直接下载整个文件树（不需要 git）
        #[arg(long)]
        api: bool,
        /// 仅下载该子目录（如 src），默认整个仓库（仅 --api 模式生效）
        #[arg(long, default_value = "")]
        path: String,
        /// 分支 / 标签名，默认 main（仅 --api 模式生效）
        #[arg(long = "ref", default_value = "main")]
        branch: String,
    },
    /// 在当前用户命名空间下创建新仓库
    Create {
        /// 仓库名称（仅名称，不含 owner）
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// 是否设为私有仓库
        #[arg(long)]
        private: bool,
        /// 创建成功后把当前 git 仓库的远程指向新仓库
        ///（可带远程名：--set-remote=upstream；不带值默认 origin）
        #[arg(
            long,
            value_name = "NAME",
            num_args = 0..=1,
            default_missing_value = "origin",
            require_equals = true
        )]
        set_remote: Option<String>,
    },
    /// Fork 仓库到当前用户命名空间（网页端 v2 接口）；加 --org 则 fork 进组织（v5 接口）
    Fork {
        /// 要 fork 的源仓库 owner/name；也可省略并用 --source 指定，或在仓库目录下自动推断
        repo: Option<String>,
        /// 显式指定源仓库 owner/name（与位置参数二选一，--source 优先）
        #[arg(long = "source")]
        source: Option<String>,
        /// fork 进该组织的命名空间（默认 fork 到当前用户名下）
        #[arg(long)]
        org: Option<String>,
        /// 新仓库名称（默认与源仓库同名，冲突时服务端自动加后缀）
        #[arg(long)]
        name: Option<String>,
        /// 新仓库描述
        #[arg(long, default_value = "")]
        description: String,
        /// 若已 fork 过（{目标命名空间}/name 已存在），强制把已有 fork 的默认分支硬重置到源仓库 HEAD，而不是报错
        #[arg(long)]
        force: bool,
        /// fork 后在「当前 git 仓库」里添加 upstream 远程（指向源仓库），不自动克隆
        #[arg(long)]
        link_remote: bool,
        /// fork 后将可见性设为私有（默认公开；fork 接口本身不支持该参数，会再用 v5 补一次更新）
        #[arg(long)]
        private: bool,
    },
    /// 同步 fork 仓库与其上游
    ///
    /// 默认走网页端 v2 快进同步（fork -> 上游，仅快进）。当上游历史被重写
    /// （rebase/force push）快进失败时，加 --force 走 v5 硬重置：删掉目标分支
    /// 再按源分支 HEAD 重建（会丢弃目标分支上的额外提交，需 --yes 确认）。
    /// 可用 --source 指定任意可见仓库作为源（镜像/备份场景）。
    /// 省略 <REPO> 时从当前目录的 git remote origin 自动推断（与 gh 行为一致）。
    Sync {
        /// 目标仓库 owner/name；省略则从当前目录的 git remote origin 推断
        repo: Option<String>,
        /// 指定要同步的分支（默认同步默认分支）
        #[arg(long = "branch", default_value = "")]
        branch: String,
        /// 源仓库 owner/name（默认取 fork 的上游；非 fork 时必须指定）
        #[arg(long = "source")]
        source: Option<String>,
        /// 强制硬重置：删除目标分支再按源分支 HEAD 重建（不可撤销）
        #[arg(long)]
        force: bool,
        /// 确认执行破坏性硬重置（需与 --force 一起使用）
        #[arg(long)]
        yes: bool,
        /// 只提交同步任务，不轮询等待完成（仅非 --force 的网页端快进同步生效）
        #[arg(long)]
        no_wait: bool,
    },
    /// 设置仓库可见性：gc repo set owner/name <private|public>（v5 开放 API）
    Set {
        repo: String,
        /// private 或 public
        visibility: String,
    },
    /// 删除仓库（危险操作，需 --yes 确认）
    Delete {
        repo: String,
        /// 确认删除（不加此 flag 只会预览要删除的仓库并拒绝执行）
        #[arg(long)]
        yes: bool,
    },
    /// 给仓库点 Star（网页端 v2 接口）；加 --unstar 则取消 Star
    Star {
        repo: String,
        /// 取消 Star（默认是点 Star）
        #[arg(long)]
        unstar: bool,
    },
    /// 列出仓库文件树（网页端 v2 接口，类似网页 repo 页面的文件列表）
    Tree(TreeArgs),
    /// 读取单个文件内容（网页端 v2 接口，类似网页 blob 页面）
    Read(ReadArgs),
    /// 下载多个文件到本地目录（网页端 v2 接口，多线程流式下载）
    Download(DownloadArgs),

    // ---- 以下子命令对齐 @oh-gc/cli 的 repo 命令集 ----
    /// 更新仓库属性（改名 / 描述 / 主页 / 可见性 / 默认分支）
    Update {
        /// 仓库 owner/name；省略则从当前目录的 git remote 推断
        repo: Option<String>,
        /// 新仓库名
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        homepage: Option<String>,
        /// 设为私有
        #[arg(long)]
        private: bool,
        /// 设为公开（与 --private 互斥）
        #[arg(long)]
        public: bool,
        /// 新的默认分支
        #[arg(long = "default-branch")]
        default_branch: Option<String>,
    },
    /// 设置仓库标签（tag_list，整体替换；不给标签则清空）。需网页端 JWT（gc web-token set）
    ///
    /// 示例：gc repo tag 应用 工具 --repo owner/name
    ///       在仓库目录下可省略 --repo：gc repo tag 应用 工具
    Tag {
        /// 一个或多个标签（空格分隔），如 应用 工具。不给则清空标签
        #[arg(num_args = 0..)]
        tags: Vec<String>,
        /// 仓库 owner/name；省略则从当前目录的 git remote 推断
        #[arg(long = "repo")]
        repo: Option<String>,
    },
    /// 转移仓库归属到其他用户或组织（危险操作，需 --yes 确认）
    Transfer {
        /// 新的归属者（用户名或组织名）
        new_owner: String,
        /// 仓库 owner/name；省略则从当前目录的 git remote 推断
        #[arg(long)]
        repo: Option<String>,
        /// 一并加入的团队 id（逗号分隔）
        #[arg(long = "team-ids", default_value = "")]
        team_ids: String,
        /// 确认转移（不加只预览）
        #[arg(long)]
        yes: bool,
    },
    /// 列出该仓库的 fork
    Forks {
        repo: Option<String>,
        /// 排序：newest / oldest / stargazers
        #[arg(long, default_value = "")]
        sort: String,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long = "per-page", default_value_t = 0)]
        per_page: u64,
    },
    /// 列出贡献者；加 --stats 看增删行数统计
    Contributors {
        repo: Option<String>,
        /// 贡献者口径：committers / authors
        #[arg(long = "type", default_value = "")]
        kind: String,
        /// 改为输出贡献量统计（增删行数、提交数）
        #[arg(long)]
        stats: bool,
        /// 只看某个作者（配合 --stats）
        #[arg(long, default_value = "")]
        author: String,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long = "per-page", default_value_t = 0)]
        per_page: u64,
    },
    /// 查看仓库语言构成
    Languages { repo: Option<String> },
    /// 查看仓库动态事件
    Events {
        repo: Option<String>,
        /// 过滤：all / push / merged / issue / comments / team / project
        #[arg(long, default_value = "")]
        filter: String,
        #[arg(long, default_value = "")]
        author: String,
        /// 只看该时间之前（ISO8601）
        #[arg(long, default_value = "")]
        before: String,
        /// 只看该时间之后（ISO8601）
        #[arg(long, default_value = "")]
        after: String,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long = "per-page", default_value_t = 0)]
        per_page: u64,
    },
    /// 提交统计（需 --branch）；加 --downloads 改看下载量统计
    Stats {
        repo: Option<String>,
        /// 统计哪个分支的提交（提交统计必填）
        #[arg(long, default_value = "")]
        branch: String,
        #[arg(long, default_value = "")]
        author: String,
        /// 起始日期 YYYY-MM-DD
        #[arg(long, default_value = "")]
        since: String,
        /// 结束日期 YYYY-MM-DD
        #[arg(long, default_value = "")]
        until: String,
        /// 只统计自己的提交
        #[arg(long = "only-self")]
        only_self: bool,
        /// 改为查看下载量统计
        #[arg(long)]
        downloads: bool,
        /// 下载量排序：asc / desc
        #[arg(long, default_value = "")]
        direction: String,
    },
    /// 查看仓库高级设置；带任一开关参数则改为更新
    Settings {
        repo: Option<String>,
        /// 禁止 fork
        #[arg(long = "disable-fork")]
        disable_fork: Option<bool>,
        /// 禁止开发者建分支
        #[arg(long = "forbid-developer-branch")]
        forbid_developer_branch: Option<bool>,
        /// 禁止开发者建标签
        #[arg(long = "forbid-developer-tag")]
        forbid_developer_tag: Option<bool>,
        /// 禁止提交者建分支
        #[arg(long = "forbid-committer-branch")]
        forbid_committer_branch: Option<bool>,
        /// 分支名正则约束
        #[arg(long = "branch-regex")]
        branch_regex: Option<String>,
        /// 标签名正则约束
        #[arg(long = "tag-regex")]
        tag_regex: Option<String>,
        /// 生成 pre-merge ref
        #[arg(long = "pre-merge-ref")]
        pre_merge_ref: Option<bool>,
        /// 开启 GPG 提交校验
        #[arg(long = "open-gpg")]
        open_gpg: Option<bool>,
        /// 打包时包含 LFS 对象
        #[arg(long)]
        lfs: Option<bool>,
    },
    /// 开关仓库功能模块（wiki / issue / 安全 / PR / fork / 分析 / 讨论）
    Module {
        repo: Option<String>,
        #[arg(long)]
        wiki: Option<bool>,
        #[arg(long)]
        issues: Option<bool>,
        #[arg(long)]
        security: Option<bool>,
        #[arg(long = "merge-request")]
        merge_request: Option<bool>,
        #[arg(long)]
        fork: Option<bool>,
        #[arg(long)]
        analysis: Option<bool>,
        #[arg(long)]
        discussion: Option<bool>,
    },
    /// 列出仓库的自定义角色
    Roles { repo: Option<String> },
    /// 查看仓库权限继承模式；带 --mode 则修改（1=继承上级，2=独立管理）
    Transition {
        repo: Option<String>,
        #[arg(long, value_parser = ["1", "2"])]
        mode: Option<String>,
    },
    /// 归档仓库（只读）；加 --open 取消归档。服务端要求带账号密码二次确认
    Archive {
        repo: Option<String>,
        /// 取消归档（默认是归档）
        #[arg(long)]
        open: bool,
        /// 账号密码（服务端强制要求）；省略则交互式输入
        #[arg(long)]
        password: Option<String>,
    },
    /// 显示 gc 在当前仓库使用哪个 git remote 解析 owner/name
    GetRemote,
    /// 设置 gc 在当前仓库使用哪个 git remote（写入 git local config 的 gc.remote）
    SetRemote {
        /// remote 名称，例如 origin / upstream
        remote: String,
    },
}

pub fn run(action: RepoAction, api: &ApiClient) -> anyhow::Result<()> {
    match action {
        RepoAction::View { repo, json, jq } => {
            let (o, n) = split_repo(&repo)?;
            let v = api.repo(&o, &n)?;
            if !print_structured(&v, json, &jq)? {
                print_repo_summary(&v);
                // TUI 式展示：摘要后跟随 README（渲染）与 LICENSE 原文
                if let Ok(Some(readme)) = fetch_repo_text(&api, &o, &n, "readme") {
                    println!("\n===== README ({}) =====", readme.0);
                    render_md(&readme.1);
                }
                if let Ok(Some(lic)) = fetch_repo_text(&api, &o, &n, "LICENSE") {
                    println!("\n===== LICENSE ({}) =====", lic.0);
                    println!("{}", lic.1);
                }
            }
        }
        RepoAction::Grep {
            pattern,
            include,
            repo,
            owner,
            branch,
            ignore_case,
        } => {
            let (o, n) = match (&repo, &owner) {
                (Some(r), _) => split_repo(r)?,
                (None, Some(own)) => (own.clone(), pattern.clone()), // 不会走到：pattern 是位置参数
                (None, None) => resolve_repo(None)?,
            };
            run_repo_grep(&api, &o, &n, &pattern, &include, &branch, ignore_case)?;
        }
        RepoAction::List { owner } => {
            let v = api.repos(owner.as_deref())?;
            print_list(&v, |it| {
                let full = it["full_name"].as_str().unwrap_or("?");
                let desc = it["description"].as_str().unwrap_or("");
                if desc.is_empty() {
                    full.to_string()
                } else {
                    format!("{} — {}", full, desc)
                }
            });
        }
        RepoAction::Clone {
            repo,
            dest,
            api,
            path,
            branch,
        } => {
            let (o, n) = split_repo(&repo)?;
            if api {
                let token = web_access_token()?;
                let out_dir = dest.clone().unwrap_or_else(|| n.clone());
                let out_path = std::path::Path::new(&out_dir);
                let scope = if path.trim().is_empty() {
                    "整个仓库".to_string()
                } else {
                    format!("子目录 {}", path.trim_matches('/'))
                };
                println!(
                    "通过网页端接口克隆文件树 {}（{}，分支 {}） -> {}",
                    repo, scope, branch, out_dir
                );
                let (ok, fail, limited) =
                    crate::api::download_tree(&token, &o, &n, &branch, &path, out_path)?;
                println!("下载完成：成功 {}，失败 {}", ok, fail);
                if limited > 0 {
                    eprintln!(
                            "⚠ 有 {} 个文件被服务端限流（内容为空/截断），请用 `gc repo clone` 之外的 `git clone` 获取完整内容",
                            limited
                        );
                }
                if fail > 0 {
                    std::process::exit(1);
                }
            } else {
                let url = config::clone_url(&o, &n);
                let mut cmd = std::process::Command::new("git");
                cmd.arg("clone").arg(&url);
                if let Some(d) = dest {
                    cmd.arg(d);
                }
                println!("克隆 {} ...", url);
                let status = cmd
                    .status()
                    .map_err(|e| anyhow::anyhow!("执行 git clone 失败: {}", e))?;
                if !status.success() {
                    anyhow::bail!("git clone 退出码非零");
                }
            }
        }
        RepoAction::Create {
            name,
            description,
            private,
            set_remote,
        } => {
            let v = api.create_repo(&name, &description, private)?;
            let full = v["full_name"].as_str().unwrap_or(&name);
            println!("已创建仓库: {}", full);
            if let Some(u) = v["html_url"].as_str() {
                println!("地址: {}", u);
            }
            if let Some(ssh) = v["ssh_url"].as_str() {
                println!("SSH : {}", ssh);
            }
            if let Some(http) = v["html_url"].as_str() {
                println!(
                    "克隆: {}",
                    http.ends_with(".git")
                        .then_some(http.to_string())
                        .unwrap_or_else(|| format!("{}.git", http))
                );
            }
            if let Some(remote) = set_remote {
                set_repo_remote(full, &remote)?;
            }
        }
        RepoAction::Fork {
            repo,
            source,
            org,
            name,
            description,
            force,
            link_remote,
            private,
        } => {
            // 1. 解析源仓库：--source > 位置参数 > 当前目录 git remote
            let src = source
                    .clone()
                    .or(repo.clone())
                    .or_else(|| detect_repo_from_cwd())
                    .ok_or_else(|| {
                        anyhow!("请指定要 fork 的仓库（<REPO> 或 --source，或在克隆仓库目录下运行 gc repo fork）")
                    })?;
            let (so, sn) = split_repo(&src)?;

            // 2. 目标命名空间（--org 优先，否则当前用户）与 fork 名称
            let namespace = match &org {
                Some(o) => o.clone(),
                None => {
                    let me = api.authenticated_user()?;
                    me["login"]
                        .as_str()
                        .ok_or_else(|| anyhow!("拿不到当前登录用户名"))?
                        .to_string()
                }
            };
            let new_name = name.clone().unwrap_or_else(|| sn.clone());
            let fork_full = format!("{}/{}", namespace, new_name);

            // 3. 是否已 fork 过
            let fork_exists = api.repo(&namespace, &new_name).is_ok();
            if fork_exists && !force {
                anyhow::bail!(
                    "{}/{} 已被 fork 到 {}。要强制把已有 fork 重新同步到上游，请加 --force",
                    so,
                    sn,
                    fork_full
                );
            }

            if fork_exists && force {
                // 硬重置已有 fork 的默认分支到源 HEAD（与 repo sync --force 同一套 v5 逻辑）
                let info = api.repo(&namespace, &new_name)?;
                if info["fork"].as_bool() != Some(true) {
                    anyhow::bail!("{} 已存在但不是 fork，--force 不会动它", fork_full);
                }
                let branch = info["default_branch"].as_str().unwrap_or("main");
                let sb = api.branch(&so, &sn, branch)?;
                let sha = sb["commit"]["sha"]
                    .as_str()
                    .ok_or_else(|| anyhow!("拿不到源分支 {}/{} 的 HEAD", so, sn))?;
                // 硬重置：受保护时自动解保护、重建并尽量恢复保护
                api.hard_reset_branch(&namespace, &new_name, branch, sha)?;
                println!(
                    "已强制同步已有 fork {} 的 {} 到 {}/{} 的 HEAD ({})",
                    fork_full, branch, so, sn, sha
                );
            } else if let Some(o) = &org {
                // fork 进组织：网页端 v2 接口只能 fork 到自己名下，这里走 v5（PAT）
                let v = api.fork_repo_v5(&so, &sn, Some(o), name.as_deref(), name.as_deref())?;
                let full = v["full_name"].as_str().unwrap_or(&fork_full).to_string();
                println!("已 fork 到组织: {}", full);
                if let Some(u) = v["html_url"].as_str() {
                    println!("地址: {}", u);
                }
                if !description.is_empty() {
                    eprintln!("提示：v5 fork 接口不支持描述参数，--description 已忽略");
                }
                if private {
                    let (po, pn) = split_repo(&full)?;
                    let pv = api.set_repo_visibility(&po, &pn, true)?;
                    println!(
                        "已将可见性设为：{}",
                        pv["visibility"].as_str().unwrap_or("?")
                    );
                }
            } else {
                // 正常创建 fork（网页端 v2）
                let token = web_access_token()?;
                let v = crate::api::fork_repo(&token, &so, &sn, &new_name, &description)?;
                let full = v["path_with_namespace"].as_str().unwrap_or(&new_name);
                println!("已 fork: {}", full);
                if let Some(u) = v["http_url_to_repo"].as_str() {
                    println!("克隆: {}", u);
                }
                if let Some(u) = v["web_url"].as_str() {
                    println!("地址: {}", u);
                }
                if private {
                    // fork 接口本身不支持可见性参数，这里用 v5 PATCH 补一次
                    let (po, pn) = split_repo(full)?;
                    let pv = api.set_repo_visibility(&po, &pn, true)?;
                    let vis = pv["visibility"].as_str().unwrap_or("?");
                    println!("已将可见性设为：{}", vis);
                }
            }

            // link_remote 无论新建还是已存在都处理（在 git 仓库内才生效）
            if link_remote {
                link_upstream_remote(&src)?;
            }
        }
        RepoAction::Sync {
            repo,
            branch,
            source,
            force,
            yes,
            no_wait,
        } => {
            // 1. 解析目标仓库：参数或本地 git 推断
            let target = match repo {
                    Some(r) => r,
                    None => detect_repo_from_cwd()
                        .ok_or_else(|| anyhow!("当前目录不是 gitcode 仓库（无法从 git remote 推断），请显式指定 <REPO> 或用 --source"))?,
                };
            let (o, n) = split_repo(&target)?;
            let info = api.repo(&o, &n)?;
            let branch = if branch.is_empty() {
                info["default_branch"]
                    .as_str()
                    .unwrap_or("main")
                    .to_string()
            } else {
                branch
            };

            // 2. 硬重置路径（--force，或指定了 --source）
            if force || source.is_some() {
                // 解析源仓库
                let source_repo: (String, String) = if let Some(s) = &source {
                    let (so, sn) = split_repo(s)?;
                    api.repo(&so, &sn)?; // 校验源仓库对用户可见
                    (so, sn)
                } else {
                    match info["parent"]["full_name"].as_str() {
                            Some(p) => split_repo(p)?,
                            None => anyhow::bail!(
                                "未检测到上游（该仓库不是 fork，且无 --source）；请用 --source 指定源仓库"
                            ),
                        }
                };
                if !yes {
                    anyhow::bail!(
                            "硬重置会把目标分支 {} 上的额外提交丢弃，且不可撤销。确认请加 --yes（已含 --force）",
                            branch
                        );
                }
                let sb = api.branch(&source_repo.0, &source_repo.1, &branch)?;
                let sha = sb["commit"]["sha"].as_str().ok_or_else(|| {
                    anyhow!("拿不到源分支 {}/{} 的 HEAD", source_repo.0, source_repo.1)
                })?;
                // 硬重置：删除目标分支再按源 HEAD 重建；受保护时自动解保护、重建并尽量恢复保护
                api.hard_reset_branch(&o, &n, &branch, sha)?;
                println!(
                    "已将 {}/{} 的 {} 硬重置到 {}/{} 的 HEAD ({})",
                    o, n, branch, source_repo.0, source_repo.1, sha
                );
                return Ok(());
            }

            // 3. 默认：网页端 v2 快进同步（fork -> 上游）
            let token = web_access_token()?;
            let (_started, msg) = crate::api::sync_repo(&token, &o, &n, &branch)?;
            println!("已提交同步任务：{}", msg);
            if no_wait {
                return Ok(());
            }
            let mut done = false;
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let (ok, m) = crate::api::sync_repo_status(&token, &o, &n, &branch)?;
                if ok {
                    println!("同步完成：{}", m);
                    done = true;
                    break;
                }
            }
            if !done {
                eprintln!("同步任务已提交，但轮询超时（60s），可在网页查看状态");
            }
        }
        RepoAction::Set { repo, visibility } => {
            let vis = visibility.to_lowercase();
            let private = match vis.as_str() {
                "private" => true,
                "public" => false,
                _ => anyhow::bail!("可见性必须是 private 或 public，收到：{}", visibility),
            };
            let (o, n) = split_repo(&repo)?;
            let v = api.set_repo_visibility(&o, &n, private)?;
            let got = v["visibility"].as_str().unwrap_or("?");
            println!("已将 {} 的可见性设为：{}", repo, got);
        }
        RepoAction::Delete { repo, yes } => {
            let (o, n) = split_repo(&repo)?;
            if !yes {
                eprintln!("⚠ 即将删除仓库 {}（不可恢复）", repo);
                anyhow::bail!("确认删除请加 --yes（gc repo delete {} --yes）", repo);
            }
            api.delete_repo(&o, &n)?;
            println!("已删除仓库：{}", repo);
        }
        RepoAction::Star { repo, unstar } => {
            let (o, n) = split_repo(&repo)?;
            let v = api.repo(&o, &n)?;
            let id = v["id"]
                .as_u64()
                .ok_or_else(|| anyhow!("无法解析仓库 ID（请确认仓库存在且可访问）"))?;
            let token = web_access_token()?;
            if unstar {
                let count = crate::api::unstar_repo(&token, id)?;
                println!("已取消 {} 的 Star（当前 {} 颗）", repo, count);
            } else {
                let count = crate::api::star_repo(&token, id)?;
                println!("已给 {} 点 Star（当前 {} 颗）", repo, count);
            }
        }
        RepoAction::Tag { repo, tags } => {
            let (o, n) = resolve_repo(repo)?;
            // 先取回当前 name/description/homepage，避免 v2 PUT 整体替换把它们清空
            let cur = api.repo(&o, &n)?;
            let name_now = cur["name"].as_str().unwrap_or(&n).to_string();
            let desc_now = cur["description"].as_str().unwrap_or("").to_string();
            let home_now = cur["homepage"].as_str().unwrap_or("").to_string();
            let body = serde_json::json!({
                "name": name_now,
                "description": desc_now,
                "homepage": home_now,
                "tag_list": tags,
            });
            let token = web_access_token()?;
            let v = crate::api::update_project_v2(&token, &o, &n, &body)?;
            let tags_now: Vec<String> = v["tag_list"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let shown = if tags_now.is_empty() {
                "（空）".to_string()
            } else {
                tags_now.join("、")
            };
            println!(
                "已更新 {} 的标签：{}",
                v["full_name"].as_str().unwrap_or(&format!("{}/{}", o, n)),
                shown
            );
        }
        RepoAction::Tree(a) => run_tree(&a.repo, &a.branch)?,
        RepoAction::Read(a) => run_read(&a.repo, &a.file, &a.branch, a.output.as_deref())?,
        RepoAction::Download(a) => run_download(&a.repo, &a.files, &a.branch, &a.output)?,

        RepoAction::Update {
            repo,
            name,
            description,
            homepage,
            private,
            public,
            default_branch,
        } => {
            let (o, n) = resolve_repo(repo)?;
            if private && public {
                anyhow::bail!("--private 与 --public 只能选一个");
            }
            let mut b = Map::new();
            if let Some(v) = name {
                b.insert("name".into(), Value::String(v));
            }
            if let Some(v) = description {
                b.insert("description".into(), Value::String(v));
            }
            if let Some(v) = homepage {
                b.insert("homepage".into(), Value::String(v));
            }
            if private || public {
                b.insert("private".into(), Value::Bool(private));
            }
            if let Some(v) = default_branch {
                b.insert("default_branch".into(), Value::String(v));
            }
            if b.is_empty() {
                anyhow::bail!("没有要更新的字段，可用：--name --description --homepage --private/--public --default-branch");
            }
            let v = api.update_repo(&o, &n, &Value::Object(b))?;
            println!(
                "已更新 {}",
                v["full_name"].as_str().unwrap_or(&format!("{}/{}", o, n))
            );
            println!("{}", pretty(&v));
        }
        RepoAction::Transfer {
            new_owner,
            repo,
            team_ids,
            yes,
        } => {
            let (o, n) = resolve_repo(repo)?;
            if !yes {
                anyhow::bail!(
                    "转移会把 {}/{} 的归属交给 {}（原路径将失效、需对方接收），确认请加 --yes",
                    o,
                    n,
                    new_owner
                );
            }
            let ids: Vec<u64> = team_ids
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse::<u64>()
                        .map_err(|_| anyhow!("--team-ids 里的 {} 不是数字", s))
                })
                .collect::<anyhow::Result<_>>()?;
            let v = api.transfer_repo(&o, &n, &new_owner, &ids)?;
            println!("已转移到 {}", v["full_name"].as_str().unwrap_or(&new_owner));
        }
        RepoAction::Forks {
            repo,
            sort,
            page,
            per_page,
        } => {
            let (o, n) = resolve_repo(repo)?;
            let v = api.list_forks(&o, &n, &sort, page, per_page)?;
            print_list(&v, |it| {
                format!(
                    "{}  ★{}",
                    it["full_name"].as_str().unwrap_or("?"),
                    it["stargazers_count"].as_u64().unwrap_or(0)
                )
            });
        }
        RepoAction::Contributors {
            repo,
            kind,
            stats,
            author,
            page,
            per_page,
        } => {
            let (o, n) = resolve_repo(repo)?;
            if stats {
                let v = api.contributor_statistics(&o, &n, &author, page, per_page)?;
                print_list(&v, |it| {
                    // 汇总数在 overview 里，顶层只有 name/email 和按天拆分的 contributions
                    let s = if it["overview"].is_object() {
                        &it["overview"]
                    } else {
                        it
                    };
                    format!(
                        "{:<24} +{:<7} -{:<7} {} commits",
                        it["name"].as_str().unwrap_or("?"),
                        s["additions"].as_u64().unwrap_or(0),
                        s["deletions"].as_u64().unwrap_or(0),
                        s["commit_count"].as_u64().unwrap_or(0)
                    )
                });
            } else {
                let v = api.repo_contributors(&o, &n, &kind, page, per_page)?;
                print_list(&v, |it| {
                    format!(
                        "{:<24} {} contributions",
                        it["login"]
                            .as_str()
                            .or_else(|| it["name"].as_str())
                            .unwrap_or("?"),
                        it["contributions"].as_u64().unwrap_or(0)
                    )
                });
            }
        }
        RepoAction::Languages { repo } => {
            let (o, n) = resolve_repo(repo)?;
            let v = api.repo_languages(&o, &n)?;
            match v.as_object() {
                // 服务端直接给的是百分比（如 {"Rust": 100.0}），不是字节数
                Some(m) if !m.is_empty() => {
                    let mut rows: Vec<(&String, f64)> = m
                        .iter()
                        .map(|(k, v)| (k, v.as_f64().unwrap_or(0.0)))
                        .collect();
                    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
                    for (lang, pct) in rows {
                        println!("{:<20} {:>6.2}%", lang, pct);
                    }
                }
                _ => println!("{}", pretty(&v)),
            }
        }
        RepoAction::Events {
            repo,
            filter,
            author,
            before,
            after,
            page,
            per_page,
        } => {
            let (o, n) = resolve_repo(repo)?;
            let v = api.repo_events(&o, &n, &filter, &author, &before, &after, page, per_page)?;
            print_list(&v, |it| {
                format!(
                    "{:<26} {:<10} {:<16} {}",
                    it["created_at"].as_str().unwrap_or("?"),
                    it["action_name"].as_str().unwrap_or("?"),
                    it["author_username"].as_str().unwrap_or("?"),
                    it["title"].as_str().unwrap_or("")
                )
            });
        }
        RepoAction::Stats {
            repo,
            branch,
            author,
            since,
            until,
            only_self,
            downloads,
            direction,
        } => {
            let (o, n) = resolve_repo(repo)?;
            if downloads {
                let v = api.download_statistics(&o, &n, &since, &until, &direction)?;
                print_list(&v, |it| {
                    format!(
                        "{:<12} {}",
                        it["date"].as_str().unwrap_or("?"),
                        it["downloads"].as_u64().unwrap_or(0)
                    )
                });
            } else {
                if branch.is_empty() {
                    anyhow::bail!("提交统计必须指定 --branch；看下载量请加 --downloads");
                }
                let v =
                    api.commit_statistics(&o, &n, &branch, &author, only_self, &since, &until)?;
                // 返回体是 {commits: [...], statistics: [...], total}，要看的是 statistics
                print_list(&v["statistics"], |it| {
                    format!(
                        "{:<24} +{:<7} -{:<7} {} commits",
                        it["nick_name"]
                            .as_str()
                            .or_else(|| it["user_name"].as_str())
                            .unwrap_or("?"),
                        it["add_lines"].as_u64().unwrap_or(0),
                        it["delete_lines"].as_u64().unwrap_or(0),
                        it["commit_count"].as_u64().unwrap_or(0)
                    )
                });
            }
        }
        RepoAction::Settings {
            repo,
            disable_fork,
            forbid_developer_branch,
            forbid_developer_tag,
            forbid_committer_branch,
            branch_regex,
            tag_regex,
            pre_merge_ref,
            open_gpg,
            lfs,
        } => {
            let (o, n) = resolve_repo(repo)?;
            let mut b = Map::new();
            for (k, v) in [
                ("disable_fork", disable_fork),
                ("forbidden_developer_create_branch", forbid_developer_branch),
                ("forbidden_developer_create_tag", forbid_developer_tag),
                ("forbidden_committer_create_branch", forbid_committer_branch),
                ("generate_pre_merge_ref", pre_merge_ref),
                ("open_gpg_verified", open_gpg),
                ("include_lfs_objects", lfs),
            ] {
                if let Some(v) = v {
                    b.insert(k.into(), Value::Bool(v));
                }
            }
            if let Some(v) = branch_regex {
                b.insert("branch_name_regex".into(), Value::String(v));
            }
            if let Some(v) = tag_regex {
                b.insert("tag_name_regex".into(), Value::String(v));
            }
            let v = if b.is_empty() {
                api.repo_settings(&o, &n)?
            } else {
                let r = api.update_repo_settings(&o, &n, &Value::Object(b))?;
                println!("已更新 {}/{} 的高级设置", o, n);
                r
            };
            println!("{}", pretty(&v));
        }
        RepoAction::Module {
            repo,
            wiki,
            issues,
            security,
            merge_request,
            fork,
            analysis,
            discussion,
        } => {
            let (o, n) = resolve_repo(repo)?;
            let mut b = Map::new();
            for (k, v) in [
                ("has_wiki", wiki),
                ("has_issue", issues),
                ("has_security", security),
                ("has_merge_request", merge_request),
                ("has_fork", fork),
                ("has_analysis", analysis),
                ("has_discussion", discussion),
            ] {
                if let Some(v) = v {
                    b.insert(k.into(), Value::Bool(v));
                }
            }
            if b.is_empty() {
                anyhow::bail!(
                        "至少要指定一个模块开关，例如 --wiki false --issues true（可用：wiki/issues/security/merge-request/fork/analysis/discussion）"
                    );
            }
            let v = api.update_module_setting(&o, &n, &Value::Object(b))?;
            println!("已更新 {}/{} 的模块开关", o, n);
            if !v.is_null() {
                println!("{}", pretty(&v));
            }
        }
        RepoAction::Roles { repo } => {
            let (o, n) = resolve_repo(repo)?;
            let v = api.customized_roles(&o, &n)?;
            print_list(&v, |it| {
                format!(
                    "{:<20} level={:<3} 成员 {:<4} {}",
                    it["role_name"].as_str().unwrap_or("?"),
                    it["access_level"].as_u64().unwrap_or(0),
                    it["member_count"].as_u64().unwrap_or(0),
                    it["role_description"].as_str().unwrap_or("-")
                )
            });
        }
        RepoAction::Transition { repo, mode } => {
            let (o, n) = resolve_repo(repo)?;
            let v = match mode {
                Some(m) => {
                    let m: u8 = m.parse().unwrap_or(1);
                    api.update_repo_transition(&o, &n, m)?
                }
                None => api.repo_transition(&o, &n)?,
            };
            // 读的时候服务端返回 memberMgntMode，写的时候收的是 mode，字段名不一致
            let mode = v["memberMgntMode"].as_u64().or_else(|| v["mode"].as_u64());
            let desc = match mode {
                Some(1) => "1（继承上级组织权限）",
                Some(2) => "2（独立管理自身权限）",
                _ => "未知",
            };
            println!("权限继承模式：{}", desc);
            if mode.is_none() {
                println!("{}", pretty(&v));
            }
        }
        RepoAction::Archive {
            repo,
            open,
            password,
        } => {
            let (o, n) = resolve_repo(repo)?;
            let pwd = match password {
                Some(p) => p,
                None => rpassword::prompt_password_stderr(&format!(
                    "归档接口要求验证账号密码，请输入 {} 的密码: ",
                    o
                ))
                .map_err(|e| anyhow!("读取密码失败: {}", e))?,
            };
            api.archive_repo(&o, &n, !open, &pwd)?;
            println!("{} {}/{}", if open { "已取消归档" } else { "已归档" }, o, n);
        }
        RepoAction::GetRemote => {
            let remote = configured_remote();
            match detect_repo_from_cwd() {
                Some(r) => println!("{}  ->  {}", remote, r),
                None => println!("{}（当前目录解析不出 gitcode 仓库）", remote),
            }
        }
        RepoAction::SetRemote { remote } => {
            let status = std::process::Command::new("git")
                .args(["config", "--local", "gc.remote", &remote])
                .status()
                .map_err(|e| anyhow!("执行 git config 失败: {}", e))?;
            if !status.success() {
                anyhow::bail!("写入 git config 失败（当前目录是 git 仓库吗？）");
            }
            println!("已设置 gc.remote = {}", remote);
        }
    }
    Ok(())
}

/// 列出仓库文件树（repo tree 与顶层 tree 共用）
pub fn run_tree(repo: &str, branch: &str) -> anyhow::Result<()> {
    let (o, n) = split_repo(repo)?;
    let token = web_access_token()?;
    let files = crate::api::repo_file_list(&token, &o, &n, branch)?;
    print_tree(&files);
    Ok(())
}

/// 读取单个文件内容（repo read 与顶层 read 共用）
pub fn run_read(repo: &str, file: &str, branch: &str, output: Option<&str>) -> anyhow::Result<()> {
    let (o, n) = split_repo(repo)?;
    let token = web_access_token()?;
    let f = crate::api::repo_file_content(&token, &o, &n, branch, file)?;
    let raw = if f.encoding.eq_ignore_ascii_case("base64") {
        base64::engine::general_purpose::STANDARD
            .decode(f.content.trim())
            .map_err(|e| anyhow!("base64 解码失败: {}", e))?
    } else {
        f.content.clone().into_bytes()
    };
    if let Some(out) = output {
        std::fs::write(out, &raw).map_err(|e| anyhow!("写入 {} 失败: {}", out, e))?;
        println!("已写入 {} ({} 字节)", out, raw.len());
        if f.is_limited.unwrap_or(false) {
            eprintln!(
                "⚠ 该文件被服务端限流（is_limited），写入的内容可能为空或被截断，请用 `gc repo clone` 获取完整文件"
            );
        }
    } else if is_text(&f.file_type, &raw) {
        if f.is_limited.unwrap_or(false) {
            eprintln!(
                "⚠ 该文件被服务端限流（is_limited），以下内容为截断/空，请用 `gc repo clone` 获取完整文件："
            );
        }
        print!("{}", String::from_utf8_lossy(&raw));
    } else {
        anyhow::bail!(
            "这是二进制文件（{}），请用 --output <path> 指定保存路径",
            f.file_type.unwrap_or_else(|| "未知类型".into())
        );
    }
    Ok(())
}

/// 下载多个文件（repo download 与顶层 download 共用）

pub fn run_download(
    repo: &str,
    files: &[String],
    branch: &str,
    output: &str,
) -> anyhow::Result<()> {
    if files.is_empty() {
        anyhow::bail!("请至少指定一个要下载的文件");
    }
    let (o, n) = split_repo(repo)?;
    let token = web_access_token()?;
    let out_dir = std::path::Path::new(output);
    println!(
        "下载 {} 个文件到 {}（分支 {}，线程数 {}）...",
        files.len(),
        out_dir.display(),
        branch,
        files.len()
    );
    let pairs: Vec<(String, String)> = files.iter().map(|f| (f.clone(), f.clone())).collect();
    let results = crate::api::download_files(&token, &o, &n, branch, &pairs, out_dir)?;
    let mut ok = 0;
    let mut fail = 0;
    for (f, r) in results {
        match r {
            Ok((bytes, limited)) => {
                println!("  ✓ {} ({} 字节)", f, bytes);
                if limited {
                    eprintln!(
                        "    ⚠ 该文件被服务端限流（is_limited），内容可能为空或被截断，请用 `gc repo clone` 获取完整文件"
                    );
                }
                ok += 1;
            }
            Err(e) => {
                eprintln!("  ✗ {} 失败: {}", f, e);
                fail += 1;
            }
        }
    }
    println!("下载完成：成功 {}，失败 {}", ok, fail);
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// create 后把当前 git 仓库的远程指向新仓库（gh repo create --set-remote 风格）。
/// 仅在当前目录是 git 仓库时生效；同名远程已存在则跳过（不覆盖用户配置）。
pub fn set_repo_remote(full_name: &str, remote: &str) -> anyhow::Result<()> {
    let dir = std::env::current_dir().map_err(|e| anyhow!("读取当前目录失败: {}", e))?;
    add_git_remote(&dir, full_name, remote)
}

/// 在指定目录的 git 仓库里添加指向 full_name 的远程（测试入口，不依赖进程 cwd）。
fn add_git_remote(dir: &std::path::Path, full_name: &str, remote: &str) -> anyhow::Result<()> {
    let in_git = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !in_git {
        eprintln!("⚠ --set-remote 需要在 git 仓库目录内执行（当前目录不是 git 仓库），已跳过");
        return Ok(());
    }
    let (owner, name) = split_repo(full_name)?;
    let url = config::clone_url(&owner, &name);
    let existing = std::process::Command::new("git")
        .current_dir(dir)
        .args(["remote", "get-url", remote])
        .output();
    if existing.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("⚠ 远程 {remote} 已存在，跳过（未覆盖原有配置）");
        return Ok(());
    }
    let status = std::process::Command::new("git")
        .current_dir(dir)
        .args(["remote", "add", remote, &url])
        .status()
        .map_err(|e| anyhow!("执行 git remote add 失败: {}", e))?;
    if status.success() {
        println!("已在当前仓库添加远程 {remote} -> {url}");
        println!("推送: git push -u {remote} HEAD");
    } else {
        eprintln!("⚠ 添加远程 {remote} 失败: {url}");
    }
    Ok(())
}

/// fork 后在当前 git 仓库里添加 upstream 远程（指向源仓库 owner/name）。
/// 不自动克隆：仅在当前目录已是 git 仓库时生效；upstream 已存在则跳过。
pub fn link_upstream_remote(repo: &str) -> anyhow::Result<()> {
    let in_git = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !in_git {
        eprintln!(
            "⚠ --link-remote 需要在 git 仓库目录内执行（当前目录不是 git 仓库），已跳过 upstream 设置"
        );
        return Ok(());
    }
    let source_url = format!("https://gitcode.com/{}.git", repo);
    let existing = std::process::Command::new("git")
        .args(["remote", "get-url", "upstream"])
        .output();
    if existing.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("⚠ upstream 远程已存在，跳过");
        return Ok(());
    }
    let status = std::process::Command::new("git")
        .args(["remote", "add", "upstream", &source_url])
        .status()
        .map_err(|e| anyhow!("执行 git remote add 失败: {}", e))?;
    if status.success() {
        println!("已在当前仓库添加 upstream -> {}", source_url);
    } else {
        eprintln!("⚠ 添加 upstream 失败: {}", source_url);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn remote_url(dir: &std::path::Path, remote: &str) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["remote", "get-url", remote])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn test_add_git_remote_in_temp_repo() {
        let dir = std::env::temp_dir().join(format!("gc-repo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(git_ok(&dir, &["init", "-q"]));
        // 先手工建一个指向别处的 origin
        assert!(git_ok(
            &dir,
            &["remote", "add", "origin", "https://example.com/other.git"]
        ));
        // 已存在的同名远程：跳过且不覆盖
        add_git_remote(&dir, "alice/demo", "origin").unwrap();
        assert_eq!(remote_url(&dir, "origin"), "https://example.com/other.git");
        // 自定义远程名：正常添加
        add_git_remote(&dir, "alice/demo", "mine").unwrap();
        assert_eq!(
            remote_url(&dir, "mine"),
            "https://gitcode.com/alice/demo.git"
        );
        add_git_remote(&dir, "bob/app", "upstream").unwrap();
        assert_eq!(
            remote_url(&dir, "upstream"),
            "https://gitcode.com/bob/app.git"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_git_remote_outside_git_repo_warns_only() {
        // 非 git 目录：返回 Ok（只警告），不 panic
        let dir = std::env::temp_dir().join(format!("gc-nogit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        add_git_remote(&dir, "alice/demo", "origin").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 拉取仓库单个文本文件（v5）：
/// - "readme" 走 /repos/{o}/{n}/readme（自动识别 README 文件名）
/// - 其他值走 /repos/{o}/{n}/contents/{path}
/// 返回 (路径/文件名, 解码后的 UTF-8 文本)；文件不存在返回 Ok(None)。
pub fn fetch_repo_text(
    api: &ApiClient,
    owner: &str,
    name: &str,
    what: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let path = if what == "readme" {
        format!("/repos/{}/{}/readme", owner, name)
    } else {
        format!("/repos/{}/{}/contents/{}", owner, name, what)
    };
    let v = match api.get(&path, &[]) {
        Ok(v) => v,
        // 404 = 仓库没有该文件，属正常情况
        Err(e) if e.to_string().contains("404") => return Ok(None),
        Err(e) => return Err(e),
    };
    let file_name = v["name"]
        .as_str()
        .or_else(|| v["path"].as_str())
        .unwrap_or(what)
        .to_string();
    let content = match v["content"].as_str() {
        Some(c) => {
            let cleaned: String = c.chars().filter(|ch| !ch.is_whitespace()).collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(cleaned.as_bytes())
                .map_err(|e| anyhow!("base64 解码失败：{}", e))?;
            String::from_utf8_lossy(&bytes).to_string()
        }
        // 部分接口对大文件返回空 content + 下载链接，此时不展示正文
        None => return Ok(None),
    };
    Ok(Some((file_name, content)))
}

/// 打印仓库精简摘要（替代原来整屏 JSON）。
/// 字段缺失时自动跳过，license 可能是字符串或对象。
pub fn print_repo_summary(v: &Value) {
    let get = |k: &str| v[k].as_str();
    if let Some(s) = get("full_name") {
        println!("名称:     {}", s);
    }
    if let Some(s) = get("human_name").filter(|s| !s.is_empty()) {
        println!("显示名:   {}", s);
    }
    if let Some(s) = get("description").filter(|s| !s.is_empty()) {
        println!("描述:     {}", s);
    }
    if let Some(s) = get("default_branch") {
        println!("默认分支: {}", s);
    }
    let visibility = if v["private"].as_bool() == Some(true) {
        "私有"
    } else {
        "公开"
    };
    println!("可见性:   {}", visibility);
    let stars = v["stargazers_count"].as_u64().unwrap_or(0);
    let forks = v["forks_count"].as_u64().unwrap_or(0);
    let issues = v["open_issues_count"].as_u64().unwrap_or(0);
    let watch = v["watchers_count"].as_u64().unwrap_or(0);
    println!(
        "Star:     {}    Fork: {}    Issue: {}    Watch: {}",
        stars, forks, issues, watch
    );
    // license 可能是字符串（"MIT_License"）或对象（含 "name"/"key"）
    let license = match &v["license"] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => o
            .get("name")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| o.get("key").and_then(|x| x.as_str()))
            .map(|s| s.to_string()),
        _ => None,
    };
    if let Some(s) = license {
        println!("许可:     {}", s);
    }
    if let Some(s) = v["namespace"]["name"].as_str() {
        println!("命名空间: {}", s);
    }
    if let Some(s) = v["owner"]["login"].as_str() {
        println!("拥有者:   {}", s);
    }
    if let Some(s) = get("created_at").filter(|s| !s.is_empty()) {
        println!("创建:     {}", s);
    }
    if let Some(s) = get("updated_at").filter(|s| !s.is_empty()) {
        println!("更新:     {}", s);
    }
    if let Some(s) = get("http_url_to_repo") {
        println!("克隆(HTTPS): {}", s);
    }
    if let Some(s) = get("ssh_url_to_repo") {
        println!("克隆(SSH):   {}", s);
    }
    if let Some(s) = get("web_url") {
        println!("地址:     {}", s);
    }
}

/// 打印 compare 接口的对比结果（commits + diffs + 行数统计）
pub fn print_compare(v: &Value, patch: bool) {
    let same = v["compare_same_ref"].as_bool().unwrap_or(false);
    let empty: Vec<Value> = Vec::new();
    let commits = v["commits"].as_array().unwrap_or(&empty);
    let diffs = v["diffs"].as_array().unwrap_or(&empty);
    let added = v["added_lines"].as_u64().unwrap_or(0);
    let removed = v["removed_lines"].as_u64().unwrap_or(0);

    if same {
        println!("两个引用相同，无差异。");
        return;
    }
    println!("Commits ({}):", commits.len());
    for c in commits {
        let id = c["id"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect::<String>();
        let title = c["title"]
            .as_str()
            .or_else(|| c["message"].as_str())
            .unwrap_or("?");
        let author = c["author_name"]
            .as_str()
            .or_else(|| c["committer_name"].as_str())
            .unwrap_or("");
        println!(
            "  {} {} {}",
            id,
            title,
            if author.is_empty() {
                String::new()
            } else {
                format!("({})", author)
            }
        );
    }
    println!("变更文件 ({}):  +{} -{}", diffs.len(), added, removed);
    for d in diffs {
        let path = d["new_path"]
            .as_str()
            .or_else(|| d["old_path"].as_str())
            .unwrap_or("?");
        let new_file = d["new_file"].as_bool().unwrap_or(false);
        let deleted = d["deleted_file"].as_bool().unwrap_or(false);
        let tag = if new_file {
            "新增"
        } else if deleted {
            "删除"
        } else {
            "修改"
        };
        let da = d["added_lines"].as_u64().unwrap_or(0);
        let dr = d["removed_lines"].as_u64().unwrap_or(0);
        println!("  [{}] {}  (+{} -{})", tag, path, da, dr);
        if patch {
            if let Some(txt) = d["diff"].as_str() {
                for line in txt.lines() {
                    println!("    {}", line);
                }
            }
        }
    }
}

/// 目录树节点（用于把扁平路径列表组装成树）
#[derive(Default)]
pub struct Node {
    children: std::collections::BTreeMap<String, Node>,
}

/// 判断内容是否为文本（用于决定打印还是要求 --output 落盘）
pub fn is_text(file_type: &Option<String>, raw: &[u8]) -> bool {
    if let Some(t) = file_type {
        if t.starts_with("text/") {
            return true;
        }
        if t.contains("image/")
            || t.contains("application/octet-stream")
            || t.contains("application/zip")
            || t.contains("application/gzip")
        {
            return false;
        }
    }
    std::str::from_utf8(raw).is_ok()
}

/// 把扁平的文件路径列表渲染成 ASCII 树并打印
pub fn print_tree(paths: &[String]) {
    print!("{}", build_tree_string(paths));
}

/// 由扁平路径列表（如 ["src/api.rs", "README.md"]）构造目录树字符串
pub fn build_tree_string(paths: &[String]) -> String {
    if paths.is_empty() {
        return "(空仓库)".to_string();
    }
    let mut root = Node::default();
    for p in paths {
        let mut cur = &mut root;
        for part in p.split('/') {
            cur = cur.children.entry(part.to_string()).or_default();
        }
    }
    let mut out = String::new();
    let entries: Vec<(&String, &Node)> = root.children.iter().collect();
    for (i, (name, child)) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        out.push_str(name);
        out.push('\n');
        build_node(child, if last { "    " } else { "│   " }, &mut out);
    }
    out
}

pub fn build_node(node: &Node, prefix: &str, out: &mut String) {
    let entries: Vec<(&String, &Node)> = node.children.iter().collect();
    for (i, (name, child)) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        out.push_str(prefix);
        out.push_str(if last { "└── " } else { "├── " });
        out.push_str(name);
        out.push('\n');
        build_node(
            child,
            &format!("{}{}", prefix, if last { "    " } else { "│   " }),
            out,
        );
    }
}
