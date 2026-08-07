use anyhow::{anyhow, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::api::ApiClient;
use crate::config;

#[derive(Parser)]
#[command(
    name = "gc",
    version,
    about = "GitCode (AtomGit) 命令行工具，类似 gh",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 检查当前 token 是否有效
    Auth,
    /// 管理个人访问令牌
    Token(TokenCmd),
    /// 仓库操作
    Repo(RepoCmd),
    /// 列出仓库文件树（网页端 v2 接口，等价 repo tree）
    Tree(TreeArgs),
    /// 读取单个文件内容（网页端 v2 接口，等价 repo read）
    Read(ReadArgs),
    /// 下载多个文件到本地目录（网页端 v2 接口，等价 repo download）
    Download(DownloadArgs),
    /// Issue 操作
    Issue(IssueCmd),
    /// Pull Request 操作
    Pr(PrCmd),
    /// 发行版操作
    Release(ReleaseCmd),
    /// 搜索（仓库 / 用户 / Issue）
    Search(SearchCmd),
    /// 讨论（Discussions，网页端 v1 接口）
    Discussion(DiscussionCmd),
    /// 刷新网页端 JWT（用于讨论等 v1 接口）
    Refresh,
    /// 管理网页端 JWT（access / refresh token）
    WebToken(WebTokenCmd),
    /// 用户操作（网页端 v1 接口）
    User(UserCmd),
    /// Wiki 操作（网页端 v2 接口）
    Wiki(WikiCmd),
}

#[derive(Parser)]
struct RepoCmd {
    #[command(subcommand)]
    action: RepoAction,
}
/// 共享参数：列出文件树（repo tree 与顶层 tree 共用）
#[derive(Parser)]
#[command(name = "tree")]
struct TreeArgs {
    repo: String,
    #[arg(long = "ref", default_value = "main")]
    branch: String,
}

/// 共享参数：读取单个文件（repo read 与顶层 read 共用）
#[derive(Parser)]
#[command(name = "read")]
struct ReadArgs {
    repo: String,
    #[arg(help = "仓库内文件路径，如 src/api.rs")]
    file: String,
    #[arg(long = "ref", default_value = "main")]
    branch: String,
    #[arg(long)]
    output: Option<String>,
}

/// 共享参数：下载多个文件（repo download 与顶层 download 共用）
#[derive(Parser)]
#[command(name = "download")]
struct DownloadArgs {
    repo: String,
    #[arg(help = "一个或多个仓库内文件路径，如 src/api.rs README.md")]
    files: Vec<String>,
    #[arg(long = "ref", default_value = "main")]
    branch: String,
    #[arg(long, default_value = ".")]
    output: String,
}

#[derive(Subcommand)]
enum RepoAction {
    /// 查看仓库详情
    View {
        repo: String,
        /// 直接打印完整 JSON（默认打印精简摘要）
        #[arg(long)]
        json: bool,
    },
    /// 列出仓库（省略 owner 则列出当前用户）
    List { owner: Option<String> },
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
    },
    /// Fork 仓库到当前用户命名空间（网页端 v2 接口）
    Fork {
        repo: String,
        /// 新仓库名称（默认与源仓库同名，冲突时服务端自动加后缀）
        #[arg(long)]
        name: Option<String>,
        /// 新仓库描述
        #[arg(long, default_value = "")]
        description: String,
        /// fork 后在「当前 git 仓库」里添加 upstream 远程（指向源仓库），不自动克隆
        #[arg(long)]
        link_remote: bool,
        /// fork 后将可见性设为私有（默认公开；fork 接口本身不支持该参数，会再用 v5 补一次更新）
        #[arg(long)]
        private: bool,
    },
    /// 同步 fork 仓库与其上游（网页端 v2 接口）
    Sync {
        repo: String,
        /// 指定要同步的分支（默认同步默认分支）
        #[arg(long = "branch", default_value = "")]
        branch: String,
        /// 只提交同步任务，不轮询等待完成
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
}

#[derive(Parser)]
struct TokenCmd {
    #[command(subcommand)]
    action: TokenAction,
}
#[derive(Subcommand)]
enum TokenAction {
    /// 交互式输入并把 token 保存到 ~/.config/gitcode.toml
    Set,
}

#[derive(Parser)]
struct IssueCmd {
    #[command(subcommand)]
    action: IssueAction,
}
#[derive(Subcommand)]
enum IssueAction {
    /// 列出 Issue
    List {
        repo: String,
        #[arg(long, default_value = "open")]
        state: String,
    },
    /// 查看某个 Issue
    View { repo: String, number: u64 },
    /// 创建 Issue
    Create {
        repo: String,
        #[arg(long)] title: String,
        #[arg(long, default_value = "")] body: String,
    },
}

#[derive(Parser)]
struct PrCmd {
    #[command(subcommand)]
    action: PrAction,
}
#[derive(Subcommand)]
enum PrAction {
    /// 列出 Pull Request
    List {
        repo: String,
        #[arg(long, default_value = "open")]
        state: String,
    },
    /// 查看某个 PR
    View { repo: String, number: u64 },
    /// 创建 Pull Request
    Create {
        repo: String,
        #[arg(long)] title: String,
        #[arg(long, default_value = "")] body: String,
        #[arg(long)] head: String,
        #[arg(long)] base: String,
    },
}

#[derive(Parser)]
struct ReleaseCmd {
    #[command(subcommand)]
    action: ReleaseAction,
}
#[derive(Subcommand)]
enum ReleaseAction {
    /// 列出发行版
    List { repo: String },
    /// 查看某个发行版
    View { repo: String, tag: String },
    /// 创建发行版
    Create {
        repo: String,
        tag: String,
        #[arg(long)] name: String,
        #[arg(long, default_value = "")] body: String,
        #[arg(long, default_value = "")] target: String,
    },
    /// 上传附件到发行版
    Upload { repo: String, tag: String, file: String },
}

#[derive(Parser)]
struct SearchCmd {
    #[command(subcommand)]
    action: SearchAction,
}
#[derive(Subcommand)]
enum SearchAction {
    /// 搜索仓库
    Repos { query: String },
    /// 搜索用户
    Users { query: String },
    /// 搜索 Issue
    Issues { query: String },
}

#[derive(Parser)]
struct DiscussionCmd {
    #[command(subcommand)]
    action: DiscussionAction,
}
#[derive(Subcommand)]
enum DiscussionAction {
    /// 在仓库讨论区创建一条讨论
    Create {
        /// 仓库数字 ID（如 10544417，从仓库页面的 page-repo-id 获取）
        #[arg(long)]
        source_id: u64,
        /// 来源类型，默认 2
        #[arg(long, default_value_t = 2)]
        source_type: u64,
        /// 讨论分类 ID（创建页 URL 中 /discussions/create/<hash> 的 hash）
        #[arg(long)]
        category_id: String,
        #[arg(long)] title: String,
        /// 讨论正文（Markdown）
        #[arg(long, default_value = "")] content: String,
    },
    /// 删除一条讨论（id 为讨论的 hash，取自讨论页 URL）
    Delete {
        /// 讨论的 hash id（讨论页 URL 中 /discussions/<hash> 的 hash）
        id: String,
    },
}

#[derive(Parser)]
struct WebTokenCmd {
    #[command(subcommand)]
    action: WebTokenAction,
}
#[derive(Subcommand)]
enum WebTokenAction {
    /// 交互式录入网页端 access / refresh token 并保存到配置
    Set,
}

#[derive(Parser)]
struct UserCmd {
    #[command(subcommand)]
    action: UserAction,
}
#[derive(Subcommand)]
enum UserAction {
    /// 查看用户资料（网页端 v1 接口）
    View { username: String },
    /// 获取 / 下载用户头像（网页端 v1 接口）
    Avatar {
        username: String,
        /// 下载头像到该路径（不指定则只打印头像 URL）
        #[arg(long)]
        output: Option<String>,
    },
}

#[derive(Parser)]
struct WikiCmd {
    #[command(subcommand)]
    action: WikiAction,
}
#[derive(Subcommand)]
enum WikiAction {
    /// 创建 wiki 页面（网页端 v2 接口）
    Create {
        repo: String,
        /// 页面标题（同时用作文件名 <title>.md）
        title: String,
        /// 页面内容（Markdown）；也可用 --file 从文件读取
        #[arg(long, default_value = "")]
        content: String,
        /// 从文件读取内容（与 --content 同时给时以 --file 为准）
        #[arg(long)]
        file: Option<String>,
        /// 提交说明
        #[arg(long, default_value = "创建 wiki 页面")]
        message: String,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let token = config::load_token()?;
    let api = ApiClient::new(config::api_base(), token)?;

    match cli.command {
        Command::Auth => {
            let u = api.authenticated_user()?;
            println!(
                "已认证: {} ({})",
                u["login"].as_str().unwrap_or("?"),
                u["name"].as_str().unwrap_or("?")
            );
        }

        Command::Token(c) => match c.action {
            TokenAction::Set => {
                let token = config::prompt_and_save_token()?;
                println!("token 已保存到 {}", config::config_path()?.display());
                // 验证一下
                let check = ApiClient::new(config::api_base(), Some(token))?;
                match check.authenticated_user() {
                    Ok(u) => println!(
                        "验证成功: {} ({})",
                        u["login"].as_str().unwrap_or("?"),
                        u["name"].as_str().unwrap_or("?")
                    ),
                    Err(e) => eprintln!("警告：保存成功但验证失败: {}", e),
                }
            }
        },

        Command::Repo(c) => match c.action {
            RepoAction::View { repo, json } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.repo(&o, &n)?;
                if json {
                    println!("{}", pretty(&v));
                } else {
                    print_repo_summary(&v);
                }
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
            RepoAction::Clone { repo, dest, api, path, branch } => {
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
            RepoAction::Create { name, description, private } => {
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
                    println!("克隆: {}", http.ends_with(".git").then_some(http.to_string()).unwrap_or_else(|| format!("{}.git", http)));
                }
            }
            RepoAction::Fork { repo, name, description, link_remote, private } => {
                let (o, n) = split_repo(&repo)?;
                let token = web_access_token()?;
                let new_name = name.unwrap_or_else(|| n.clone());
                let v = crate::api::fork_repo(&token, &o, &n, &new_name, &description)?;
                let full = v["path_with_namespace"].as_str().unwrap_or(&new_name);
                println!("已 fork: {}", full);
                if let Some(u) = v["http_url_to_repo"].as_str() {
                    println!("克隆: {}", u);
                }
                if let Some(u) = v["web_url"].as_str() {
                    println!("地址: {}", u);
                }
                if link_remote {
                    link_upstream_remote(&repo)?;
                }
                if private {
                    // fork 接口本身不支持可见性参数，这里用 v5 PATCH 补一次
                    let full = v["path_with_namespace"].as_str().unwrap_or(&new_name);
                    let (po, pn) = split_repo(full)?;
                    let pv = api.set_repo_visibility(&po, &pn, true)?;
                    let vis = pv["visibility"].as_str().unwrap_or("?");
                    println!("已将可见性设为：{}", vis);
                }
            }
            RepoAction::Sync { repo, branch, no_wait } => {
                let (o, n) = split_repo(&repo)?;
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
            RepoAction::Tree(a) => run_tree(&a.repo, &a.branch)?,
            RepoAction::Read(a) => run_read(&a.repo, &a.file, &a.branch, a.output.as_deref())?,
            RepoAction::Download(a) => run_download(&a.repo, &a.files, &a.branch, &a.output)?,
        },

        Command::Issue(c) => match c.action {
            IssueAction::List { repo, state } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.issues(&o, &n, &state)?;
                print_list(&v, |it| {
                    format!(
                        "#{} [{}] {}",
                        it["number"].as_u64().unwrap_or(0),
                        it["state"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
            IssueAction::View { repo, number } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.issue(&o, &n, number)?;
                println!("{}", pretty(&v));
            }
            IssueAction::Create { repo, title, body } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_issue(&o, &n, &title, &body)?;
                println!("已创建: #{} {}", v["number"].as_u64().unwrap_or(0), v["html_url"].as_str().unwrap_or(""));
            }
        },

        Command::Pr(c) => match c.action {
            PrAction::List { repo, state } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.pulls(&o, &n, &state)?;
                print_list(&v, |it| {
                    format!(
                        "#{} [{}] {}",
                        it["number"].as_u64().unwrap_or(0),
                        it["state"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
            PrAction::View { repo, number } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.pull(&o, &n, number)?;
                println!("{}", pretty(&v));
            }
            PrAction::Create { repo, title, body, head, base } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_pull(&o, &n, &title, &body, &head, &base)?;
                println!("已创建 PR: #{} {}", v["number"].as_u64().unwrap_or(0), v["html_url"].as_str().unwrap_or(""));
            }
        },

        Command::Release(c) => match c.action {
            ReleaseAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.releases(&o, &n)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({}) — {}",
                        it["tag_name"].as_str().unwrap_or("?"),
                        it["name"].as_str().unwrap_or(""),
                        it["body"].as_str().unwrap_or("")
                    )
                });
            }
            ReleaseAction::View { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.release(&o, &n, &tag)?;
                println!("{}", pretty(&v));
            }
            ReleaseAction::Create { repo, tag, name, body, target } => {
                let (o, n) = split_repo(&repo)?;
                let rname = if name.is_empty() { tag.clone() } else { name };
                let v = api.create_release(&o, &n, &tag, &rname, &body, &target)?;
                println!("已创建发行版: {} {}", v["tag_name"].as_str().unwrap_or(""), v["html_url"].as_str().unwrap_or(""));
            }
            ReleaseAction::Upload { repo, tag, file } => {
                let (o, n) = split_repo(&repo)?;
                let path = std::path::Path::new(&file);
                let v = api.upload_asset(&o, &n, &tag, path)?;
                println!("已上传附件: {}", v["name"].as_str().unwrap_or(&file));
            }
        },

        Command::Search(c) => match c.action {
            SearchAction::Repos { query } => {
                let v = api.search_repos(&query)?;
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
            SearchAction::Users { query } => {
                let v = api.search_users(&query)?;
                print_list(&v, |it| {
                    let login = it["login"].as_str().unwrap_or("?");
                    let name = it["name"].as_str().unwrap_or("");
                    if name.is_empty() {
                        login.to_string()
                    } else {
                        format!("{} ({})", login, name)
                    }
                });
            }
            SearchAction::Issues { query } => {
                let v = api.search_issues(&query)?;
                print_list(&v, |it| {
                    format!(
                        "#{} [{}] {}",
                        it["number"].as_u64().unwrap_or(0),
                        it["state"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
        },

        Command::Discussion(c) => match c.action {
            DiscussionAction::Create {
                source_id,
                source_type,
                category_id,
                title,
                content,
            } => {
                let token = web_access_token()?;
                let v = crate::api::create_discussion(
                    &token,
                    source_id,
                    source_type,
                    &category_id,
                    &title,
                    &content,
                )?;
                println!("已创建讨论，ID: {}", v);
            }
            DiscussionAction::Delete { id } => {
                let token = web_access_token()?;
                crate::api::delete_discussion(&token, &id)?;
                println!("已删除讨论: {}", id);
            }
        },

        Command::Refresh => {
            let (access, refresh) = config::load_web_tokens()?;
            if refresh.trim().is_empty() {
                anyhow::bail!(
                    "刷新需要 refresh token。请同时配置 GITCODE_WEB_REFRESH_TOKEN。\
                     \n注意：只带 access token 调刷新接口会让服务端注销会话！"
                );
            }
            let (na, nr) = crate::api::refresh_web_token(&access, &refresh)?;
            config::save_web_tokens(&na, &nr)?;
            println!("网页端 token 已刷新并保存到 {}", config::config_path()?.display());
        }

        Command::WebToken(c) => match c.action {
            WebTokenAction::Set => {
                config::prompt_and_save_web_tokens()?;
                println!("已保存网页端 token 到 {}", config::config_path()?.display());
            }
        },
        Command::Tree(a) => run_tree(&a.repo, &a.branch)?,
        Command::Read(a) => run_read(&a.repo, &a.file, &a.branch, a.output.as_deref())?,
        Command::Download(a) => run_download(&a.repo, &a.files, &a.branch, &a.output)?,
        Command::User(c) => match c.action {
            UserAction::View { username } => {
                let token = web_access_token()?;
                let p = crate::api::user_profile(&token, &username)?;
                println!("用户名: {}", p.username);
                println!("昵称:   {}", p.nickname);
                if !p.user_id.is_empty() {
                    println!("ID:     {}", p.user_id);
                }
                if let Some(bio) = &p.profile.description {
                    if !bio.is_empty() {
                        println!("简介:   {}", bio);
                    }
                }
                if let Some(web) = &p.profile.website {
                    if !web.is_empty() {
                        println!("网站:   {}", web);
                    }
                }
                if let Some(co) = &p.profile.company {
                    if !co.is_empty() {
                        println!("公司:   {}", co);
                    }
                }
                if let Some(loc) = &p.profile.location {
                    if !loc.is_empty() {
                        println!("位置:   {}", loc);
                    }
                }
                if let Some(gh) = &p.github_account {
                    if !gh.is_empty() {
                        println!("GitHub: {}", gh);
                    }
                }
                println!("头像:   {}", p.avatar);
                if let Some(t) = &p.update_time {
                    println!("更新:   {}", t);
                }
            }
            UserAction::Avatar { username, output } => {
                let token = web_access_token()?;
                let p = crate::api::user_profile(&token, &username)?;
                if let Some(out) = output {
                    let client = reqwest::blocking::Client::builder()
                        .user_agent(
                            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                             Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0",
                        )
                        .build()
                        .context("创建 HTTP 客户端失败")?;
                    let resp = client
                        .get(&p.avatar)
                        .header("User-Agent", "Mozilla/5.0")
                        .send()
                        .context("下载头像失败")?;
                    let status = resp.status();
                    if !status.is_success() {
                        anyhow::bail!("下载头像失败 {}: {:?}", status, status.canonical_reason());
                    }
                    let bytes = resp.bytes().context("读取头像数据失败")?;
                    std::fs::write(&out, &bytes).map_err(|e| anyhow!("写入 {} 失败: {}", out, e))?;
                    println!("头像已下载: {} ({} 字节)", out, bytes.len());
                } else {
                    println!("{}", p.avatar);
                }
            }
        },
        Command::Wiki(c) => match c.action {
            WikiAction::Create {
                repo,
                title,
                content,
                file,
                message,
            } => {
                let token = web_access_token()?;
                let body = match &file {
                    Some(f) => std::fs::read_to_string(f)
                        .map_err(|e| anyhow!("读取文件 {} 失败: {}", f, e))?,
                    None => content.clone(),
                };
                let name = crate::api::wiki_create(&token, &repo, &title, &body, &message)?;
                println!("已创建 wiki 页面：{}", name);
            }
        },
    }

    Ok(())
}

/// 列出仓库文件树（repo tree 与顶层 tree 共用）
fn run_tree(repo: &str, branch: &str) -> Result<()> {
    let (o, n) = split_repo(repo)?;
    let token = web_access_token()?;
    let files = crate::api::repo_file_list(&token, &o, &n, branch)?;
    print_tree(&files);
    Ok(())
}

/// 读取单个文件内容（repo read 与顶层 read 共用）
fn run_read(repo: &str, file: &str, branch: &str, output: Option<&str>) -> Result<()> {
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
fn run_download(repo: &str, files: &[String], branch: &str, output: &str) -> Result<()> {
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

/// fork 后在当前 git 仓库里添加 upstream 远程（指向源仓库 owner/name）。
/// 不自动克隆：仅在当前目录已是 git 仓库时生效；upstream 已存在则跳过。
fn link_upstream_remote(repo: &str) -> Result<()> {
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

/// owner/name -> (owner, name)
fn split_repo(s: &str) -> Result<(String, String)> {
    let mut it = s.splitn(2, '/');
    let o = it.next().ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    let n = it.next().ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    if o.is_empty() || n.is_empty() {
        anyhow::bail!("仓库格式应为 owner/name");
    }
    Ok((o.to_string(), n.to_string()))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// 打印仓库精简摘要（替代原来整屏 JSON）。
/// 字段缺失时自动跳过，license 可能是字符串或对象。
fn print_repo_summary(v: &Value) {
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

/// 打印列表：兼容直接数组与 { "data": [...] } 两种返回结构
fn print_list(v: &Value, fmt: impl Fn(&Value) -> String) {
    let arr = match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o.get("data").and_then(|d| d.as_array()),
        _ => None,
    };
    match arr {
        Some(a) => {
            if a.is_empty() {
                println!("(无结果)");
            }
            for it in a {
                println!("{}", fmt(it));
            }
        }
        None => println!("{}", pretty(v)),
    }
}

/// 目录树节点（用于把扁平路径列表组装成树）
#[derive(Default)]
struct Node {
    children: std::collections::BTreeMap<String, Node>,
}

/// 判断内容是否为文本（用于决定打印还是要求 --output 落盘）
fn is_text(file_type: &Option<String>, raw: &[u8]) -> bool {
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
fn print_tree(paths: &[String]) {
    print!("{}", build_tree_string(paths));
}

/// 由扁平路径列表（如 ["src/api.rs", "README.md"]）构造目录树字符串
fn build_tree_string(paths: &[String]) -> String {
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

fn build_node(node: &Node, prefix: &str, out: &mut String) {
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

/// 获取可用于讨论接口的 access token：
/// - 若同时配置了 refresh token，则先刷新（并落盘）再返回；
/// - 若只有 access token，则直接返回（不刷新，避免只带 access 调用会注销会话）；
/// - 刷新失败时回退到已有 access token。
fn web_access_token() -> Result<String> {
    let (access, refresh) = match config::load_web_tokens() {
        Ok(p) => p,
        Err(e) => return Err(e),
    };
    if refresh.trim().is_empty() {
        return Ok(access);
    }
    match crate::api::refresh_web_token(&access, &refresh) {
        Ok((new_access, new_refresh)) => {
            let _ = config::save_web_tokens(&new_access, &new_refresh);
            Ok(new_access)
        }
        Err(e) => {
            eprintln!("警告：自动刷新 token 失败（{}），将使用现有 token", e);
            Ok(access)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_repo_ok() {
        assert_eq!(
            split_repo("alice/demo").unwrap(),
            ("alice".to_string(), "demo".to_string())
        );
    }

    #[test]
    fn test_split_repo_keeps_subpath_name() {
        // 仓库名里带斜杠不应被拆分
        assert_eq!(
            split_repo("org/some/repo").unwrap(),
            ("org".to_string(), "some/repo".to_string())
        );
    }

    #[test]
    fn test_split_repo_missing_slash() {
        assert!(split_repo("justname").is_err());
    }

    #[test]
    fn test_split_repo_empty_parts() {
        assert!(split_repo("/demo").is_err());
        assert!(split_repo("alice/").is_err());
    }

    #[test]
    fn test_clone_invokes_git() {
        // 验证 clone_url 能拼出合法地址（真实 git 调用在 CLI 中执行）
        let (o, n) = split_repo("ghhhJoshrjnrhr/gb").unwrap();
        assert_eq!(config::clone_url(&o, &n), "https://gitcode.com/ghhhJoshrjnrhr/gb.git");
    }

    #[test]
    fn test_build_tree_string() {
        let paths = vec![
            "README.md".to_string(),
            "src/api.rs".to_string(),
            "src/cli.rs".to_string(),
            "src/sub/mod.rs".to_string(),
        ];
        let out = build_tree_string(&paths);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.contains(&"README.md"));
        assert!(lines.iter().any(|l| l.contains("src")));
        assert!(lines.iter().any(|l| l.contains("api.rs")));
        assert!(lines.iter().any(|l| l.contains("cli.rs")));
        assert!(lines.iter().any(|l| l.contains("sub")));
        // 树形前缀应出现
        assert!(out.contains("├──") || out.contains("└──"));
    }

    #[test]
    fn test_build_tree_empty() {
        assert_eq!(build_tree_string(&[]), "(空仓库)");
    }

    #[test]
    fn test_is_text() {
        assert!(is_text(&Some("text/plain; charset=utf-8".into()), b"hello"));
        assert!(!is_text(&Some("image/png".into()), &[0u8, 1, 2, 3]));
        assert!(is_text(&None, b"plain ascii text"));
        assert!(!is_text(&None, &[0u8, 0xff, 0xfe, 0x00]));
    }
}
