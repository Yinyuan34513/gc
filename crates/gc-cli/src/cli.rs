use anyhow::{anyhow, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use serde_json::json;
use serde_json::Map;
use serde_json::Value;

use crate::api::ApiClient;
use crate::config;
use gc_core::ctx::{parse_gitcode_repo, split_repo, web_access_token};
use gc_core::output::{edit_title_body, pretty, print_list, read_as_base64};

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
    /// 认证（无子命令时检查当前 token 是否有效）
    Auth(AuthCmd),
    /// 管理个人访问令牌
    Token(TokenCmd),
    /// 仓库操作
    Repo(gc_repo::cli::RepoCmd),
    /// 列出仓库文件树（网页端 v2 接口，等价 repo tree）
    Tree(gc_repo::cli::TreeArgs),
    /// 读取单个文件内容（网页端 v2 接口，等价 repo read）
    Read(gc_repo::cli::ReadArgs),
    /// 下载多个文件到本地目录（网页端 v2 接口，等价 repo download）
    Download(gc_repo::cli::DownloadArgs),
    /// Issue 操作
    Issue(gc_issues::cli::IssueCmd),
    /// Pull Request 操作
    Pr(gc_pr::cli::PrCmd),
    /// 发行版操作
    Release(ReleaseCmd),
    /// 搜索（仓库 / 用户 / Issue）
    Search(SearchCmd),
    /// 讨论（Discussions，网页端 v1 接口）
    Discussion(DiscussionCmd),
    /// 刷新网页端 JWT（用于讨论等 v1 接口）
    Refresh,
    /// 安装 gc 二进制到 PATH（默认 ~/.local/bin；--system 装到 /usr/local/bin，需要时自动 sudo）
    Install {
        /// 安装到系统目录 /usr/local/bin（非 root 时自动用 sudo）
        #[arg(long)]
        system: bool,
    },
    /// 把 gc 的使用技能安装到 AI 编码工具（自动检测 Claude Code / Codex / OpenCode / CodeBuddy / Gemini CLI / AtomCode）
    #[command(name = "skill", alias = "extension", alias = "extsion")]
    Skill(gc_skill::SkillCmd),
    /// 管理网页端 JWT（access / refresh token）
    WebToken(WebTokenCmd),
    /// 裸 API 透传（直接调 v5 开放接口）
    Api(ApiArgs),
    /// 用户操作（网页端 v1 接口）
    User(UserCmd),
    /// Wiki 操作（网页端 v2 接口）
    Wiki(WikiCmd),
    /// 分支操作
    Branch(BranchCmd),
    /// 提交操作
    Commit(CommitCmd),
    /// 仓库内文件读写（v5 contents 接口，可增删改）
    File(FileCmd),
    /// 标签操作（list / create / delete / protect 等）
    Tag(TagCmd),
    /// Webhook 操作（list / get / create / update / delete）
    Hook(HookCmd),
    /// 仓库标签（label）操作（list / get / create / update / delete）
    Label(LabelCmd),
    /// 里程碑操作（list / get / create / update / delete）
    Milestone(MilestoneCmd),
    /// 协作者操作（list / add / remove / permission）
    Collaborator(CollaboratorCmd),
    /// 组织操作（list / view / members / repos）
    Org(OrgCmd),
    /// MCP 服务器（把 gc 的能力暴露给 AI 客户端）
    Mcp(McpCmd),
}

// ---- 分支 / 提交 / 文件：移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），
// ---- 版权声明见 LICENSE.oh-gc。

#[derive(Parser)]
struct BranchCmd {
    #[command(subcommand)]
    action: BranchAction,
}
#[derive(Subcommand)]
enum BranchAction {
    /// 列出分支
    List { repo: String },
    /// 查看某个分支
    View { repo: String, branch: String },
    /// 新建分支
    Create {
        repo: String,
        /// 新分支名
        branch: String,
        /// 起点（分支 / 标签 / commit SHA）
        #[arg(long, default_value = "main")]
        from: String,
    },
    /// 删除分支（需 --yes 确认）
    Delete {
        repo: String,
        branch: String,
        #[arg(long)]
        yes: bool,
    },
    /// 开启分支保护
    Protect {
        repo: String,
        branch: String,
        /// 允许推送的角色 / 用户
        #[arg(long, default_value = "")]
        pusher: String,
        /// 允许合并的角色 / 用户
        #[arg(long, default_value = "")]
        merger: String,
    },
    /// 取消分支保护
    Unprotect { repo: String, branch: String },
    /// 通配符保护规则
    Rule(BranchRuleCmd),
}

#[derive(Parser)]
struct BranchRuleCmd {
    #[command(subcommand)]
    action: BranchRuleAction,
}
#[derive(Subcommand)]
enum BranchRuleAction {
    /// 列出保护规则
    List { repo: String },
    /// 新建保护规则
    Create {
        repo: String,
        /// 通配符，如 release/*
        wildcard: String,
        #[arg(long, default_value = "")]
        pusher: String,
        #[arg(long, default_value = "")]
        merger: String,
    },
    /// 修改保护规则
    Update {
        repo: String,
        wildcard: String,
        #[arg(long, default_value = "")]
        pusher: String,
        #[arg(long, default_value = "")]
        merger: String,
    },
    /// 删除保护规则
    Delete {
        repo: String,
        wildcard: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Parser)]
struct CommitCmd {
    #[command(subcommand)]
    action: CommitAction,
}
#[derive(Subcommand)]
enum CommitAction {
    /// 列出提交
    List {
        repo: String,
        /// 分支 / 标签 / SHA
        #[arg(long, default_value = "")]
        sha: String,
        /// 只看某个文件路径的提交
        #[arg(long, default_value = "")]
        path: String,
        /// 只看某个作者
        #[arg(long, default_value = "")]
        author: String,
        #[arg(long, default_value_t = 20)]
        per_page: u64,
    },
    /// 查看单个提交
    View { repo: String, sha: String },
    /// 查看提交的 diff
    Diff {
        repo: String,
        sha: String,
        /// 输出完整 patch（含提交信息）而非纯 diff
        #[arg(long)]
        patch: bool,
    },
    /// 对比两个引用
    Compare {
        repo: String,
        base: String,
        head: String,
    },
    /// 提交评论
    Comment(CommitCommentCmd),
}

#[derive(Parser)]
struct CommitCommentCmd {
    #[command(subcommand)]
    action: CommitCommentAction,
}
#[derive(Subcommand)]
enum CommitCommentAction {
    /// 列出提交的评论
    List { repo: String, sha: String },
    /// 发表提交评论
    Add {
        repo: String,
        sha: String,
        #[arg(long)]
        body: String,
    },
}

#[derive(Parser)]
struct FileCmd {
    #[command(subcommand)]
    action: FileAction,
}
#[derive(Subcommand)]
enum FileAction {
    /// 列出目录内容
    List {
        repo: String,
        #[arg(default_value = "")]
        path: String,
        #[arg(long = "ref", default_value = "")]
        branch: String,
    },
    /// 读取文件内容
    Get {
        repo: String,
        path: String,
        #[arg(long = "ref", default_value = "")]
        branch: String,
        /// 写入本地文件而非打印
        #[arg(long)]
        output: Option<String>,
    },
    /// 新建文件
    Create {
        repo: String,
        /// 仓库内目标路径
        path: String,
        /// 本地源文件
        #[arg(long)]
        from: String,
        #[arg(long)]
        message: String,
        #[arg(long = "branch", default_value = "")]
        branch: String,
    },
    /// 覆盖已有文件（自动查当前 sha）
    Update {
        repo: String,
        path: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        message: String,
        #[arg(long = "branch", default_value = "")]
        branch: String,
    },
    /// 删除文件（需 --yes 确认）
    Delete {
        repo: String,
        path: String,
        #[arg(long)]
        message: String,
        #[arg(long = "branch", default_value = "")]
        branch: String,
        #[arg(long)]
        yes: bool,
    },
}

// ---- 仓库治理：移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），版权声明见 LICENSE.oh-gc。

#[derive(Parser)]
struct TagCmd {
    #[command(subcommand)]
    action: TagAction,
}
#[derive(Subcommand)]
enum TagAction {
    /// 列出标签
    List { repo: String },
    /// 查看单个标签
    View { repo: String, tag: String },
    /// 新建标签
    Create {
        repo: String,
        /// 新标签名
        tag: String,
        /// 起点：分支名 / 标签 / commit SHA
        #[arg(long, default_value = "main")]
        refs: String,
        /// 标签说明（annotated tag），留空则建轻量标签
        #[arg(long, default_value = "")]
        message: String,
    },
    /// 删除标签（需 --yes 确认）
    Delete {
        repo: String,
        tag: String,
        #[arg(long)]
        yes: bool,
    },
    /// 把标签设为受保护
    Protect { repo: String, tag: String },
    /// 取消标签保护
    Unprotect { repo: String, tag: String },
    /// 列出受保护标签规则
    Rules { repo: String },
    /// 查看某条受保护标签规则
    RuleView { repo: String, tag: String },
    /// 新建受保护标签规则
    RuleCreate {
        repo: String,
        tag: String,
        /// 0 表示不指定级别
        #[arg(long, default_value = "0")]
        access_level: u64,
    },
    /// 更新受保护标签规则
    RuleUpdate {
        repo: String,
        tag: String,
        access_level: u64,
    },
    /// 删除受保护标签规则（需 --yes 确认）
    RuleDelete {
        repo: String,
        tag: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Parser)]
struct HookCmd {
    #[command(subcommand)]
    action: HookAction,
}
#[derive(Subcommand)]
enum HookAction {
    /// 列出 Webhook
    List { repo: String },
    /// 查看单个 Webhook
    View { repo: String, id: u64 },
    /// 新建 Webhook
    Create {
        repo: String,
        /// hook 名
        name: String,
        /// 回调地址
        url: String,
        /// 触发事件，逗号分隔（如 push,issues）
        #[arg(long, default_value = "push")]
        events: String,
        #[arg(long, default_value = "json")]
        content_type: String,
        #[arg(long, default_value = "")]
        secret: String,
        #[arg(long, default_value = "true")]
        active: bool,
    },
    /// 更新 Webhook
    Update {
        repo: String,
        id: u64,
        #[arg(long, default_value = "")]
        name: String,
        #[arg(long, default_value = "")]
        url: String,
        #[arg(long, default_value = "")]
        events: String,
        #[arg(long, default_value = "")]
        content_type: String,
        #[arg(long, default_value = "")]
        secret: String,
        #[arg(long)]
        active: Option<bool>,
    },
    /// 删除 Webhook（需 --yes 确认）
    Delete {
        repo: String,
        id: u64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Parser)]
struct LabelCmd {
    #[command(subcommand)]
    action: LabelAction,
}
#[derive(Subcommand)]
enum LabelAction {
    /// 列出标签（issue/PR 用）
    List { repo: String },
    /// 查看单个标签
    View { repo: String, name: String },
    /// 新建标签
    Create {
        repo: String,
        name: String,
        /// 颜色，6 位十六进制（不带 #）
        color: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 更新标签
    Update {
        repo: String,
        name: String,
        #[arg(long, default_value = "")]
        new_name: String,
        #[arg(long, default_value = "")]
        color: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 删除标签（需 --yes 确认）
    Delete {
        repo: String,
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Parser)]
struct MilestoneCmd {
    #[command(subcommand)]
    action: MilestoneAction,
}
#[derive(Subcommand)]
enum MilestoneAction {
    /// 列出里程碑
    List {
        repo: String,
        #[arg(long, default_value = "")]
        state: String,
    },
    /// 查看单个里程碑
    View { repo: String, number: u64 },
    /// 新建里程碑
    Create {
        repo: String,
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        state: String,
        #[arg(long = "due", default_value = "")]
        due_on: String,
    },
    /// 更新里程碑
    Update {
        repo: String,
        number: u64,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        state: String,
        #[arg(long = "due", default_value = "")]
        due_on: String,
    },
    /// 删除里程碑（需 --yes 确认）
    Delete {
        repo: String,
        number: u64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Parser)]
struct CollaboratorCmd {
    #[command(subcommand)]
    action: CollaboratorAction,
}
#[derive(Subcommand)]
enum CollaboratorAction {
    /// 列出协作者
    List {
        repo: String,
        #[arg(long, default_value = "")]
        affiliation: String,
    },
    /// 添加协作者
    Add {
        repo: String,
        username: String,
        #[arg(long, default_value = "")]
        permission: String,
    },
    /// 移除协作者（需 --yes 确认）
    Remove {
        repo: String,
        username: String,
        #[arg(long)]
        yes: bool,
    },
    /// 查看协作者的权限
    Permission { repo: String, username: String },
}

#[derive(Parser)]
struct OrgCmd {
    #[command(subcommand)]
    action: OrgAction,
}
#[derive(Subcommand)]
enum OrgAction {
    /// 列出当前用户所属组织
    List,
    /// 查看组织信息
    View { org: String },
    /// 列出组织成员
    Members {
        org: String,
        #[arg(long, default_value = "")]
        role: String,
    },
    /// 列出组织仓库
    Repos {
        org: String,
        #[arg(long, default_value = "")]
        r#type: String,
    },
}

#[derive(Parser)]
struct McpCmd {
    #[command(subcommand)]
    action: McpAction,
}

#[derive(Subcommand)]
enum McpAction {
    /// 在 stdio 上启动 MCP 服务器（换行分隔的 JSON-RPC 2.0）
    Start {
        /// 同时暴露破坏性工具（删仓库 / 改可见性 / 删讨论 / 改他人 issue）
        #[arg(long)]
        allow_destructive: bool,
        /// 同时执行的 tools/call 数量（1 表示完全串行）；默认 4
        #[arg(long, default_value_t = crate::mcp::DEFAULT_CONCURRENCY)]
        max_concurrency: usize,
    },
}

#[derive(Parser)]
struct AuthCmd {
    #[command(subcommand)]
    action: Option<AuthAction>,
}

#[derive(Subcommand)]
enum AuthAction {
    /// 登录 GitCode（浏览器自动抓取登录态 / 粘贴 token，参考 gh auth login）
    Login {
        /// 强制走浏览器登录流程
        #[arg(long)]
        web: bool,
        /// 从 stdin 读取 token（脚本场景：gc auth login --with-token < mytoken.txt）
        #[arg(long)]
        with_token: bool,
        /// OAuth broker 扫码登录（终端渲染二维码，需 --broker 或 GITCODE_BROKER）
        #[arg(long, conflicts_with_all = ["web", "with_token"])]
        qr: bool,
        /// OAuth broker 地址（默认读环境变量 GITCODE_BROKER）
        #[arg(long, value_name = "URL", requires = "qr")]
        broker: Option<String>,
    },
    /// 直接输出 PAT（明文打印，可管道给其他工具：export GIT_TOKEN=$(gc auth pal)）
    Pal {
        /// 输出网页端 access token 而非 PAT
        #[arg(long)]
        web: bool,
    },
    /// 把 gc 注册为 git 凭据助手（gitcode.com / atomgit.com）：
    /// 之后 git push/pull 走 HTTPS 时自动带上 PAT，不再需要配置账号密码
    Git {
        /// 只配置当前仓库（默认写入全局配置）
        #[arg(long)]
        local: bool,
    },
    /// 供 git credential helper 调用（勿手动使用）
    #[command(hide = true)]
    Credential {
        /// git 传入的操作：get / store / erase（git 调用 helper 时必带）
        #[arg(default_value = "get")]
        operation: String,
    },
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
        /// 发行版名称（省略则打开编辑器输入首行；留空则用 tag）
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, default_value = "")]
        target: String,
    },
    /// 上传附件到发行版
    Upload {
        repo: String,
        tag: String,
        file: String,
    },
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
    /// 全站搜索（网页端接口，无需任何凭据）
    Web {
        query: String,
        /// 搜索类型：repo / user / issue
        #[arg(long, default_value = "repo")]
        r#type: String,
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// 每页条数，服务端上限 20
        #[arg(long, default_value_t = 20)]
        per_page: u32,
        /// 输出原始 JSON 而非表格
        #[arg(long)]
        json: bool,
    },
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
        #[arg(long)]
        title: String,
        /// 讨论正文（Markdown）
        #[arg(long, default_value = "")]
        content: String,
    },
    /// 删除一条讨论（id 为讨论的 hash，取自讨论页 URL）
    Delete {
        /// 讨论的 hash id（讨论页 URL 中 /discussions/<hash> 的 hash）
        id: String,
    },
}

#[derive(Parser)]
struct ApiArgs {
    /// HTTP 方法（GET/POST/PUT/PATCH/DELETE，默认 GET）
    #[arg(short = 'm', long, default_value = "GET")]
    method: String,
    /// API 路径（相对 v5 base，可带 query，如 /repos/owner/name/pulls?state=open）
    #[arg(short = 'p', long)]
    path: String,
    /// 请求体（JSON 字符串，GET 可省略）
    #[arg(short = 'b', long, default_value = "")]
    body: String,
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
    // ---- 以下 v5 接口（PAT），移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
    /// 列出某用户的粉丝
    Followers { username: String },
    /// 列出某用户关注的人
    Following { username: String },
    /// 列出 Star 的仓库（不指定 username 则查自己）
    Starred {
        #[arg(default_value = "")]
        username: String,
    },
    /// 列出订阅的仓库（不指定 username 则查自己）
    Subscriptions {
        #[arg(default_value = "")]
        username: String,
    },
    /// 列出我拥有的命名空间
    Namespaces {
        #[arg(long, default_value = "")]
        mode: String,
    },
    /// 按路径查询单个命名空间
    Namespace { path: String },
    /// 列出我的 SSH 公钥
    Keys,
    /// 查看单个 SSH 公钥
    Key { id: u64 },
    /// 添加 SSH 公钥
    KeyAdd {
        title: String,
        /// 完整公钥字符串（如 "ssh-ed25519 AAAA... comment"）
        key: String,
    },
    /// 删除 SSH 公钥（需 --yes 确认）
    KeyDelete {
        id: u64,
        #[arg(long)]
        yes: bool,
    },
    /// 列出我的 Issue（可带过滤条件）
    Issues {
        #[arg(long, default_value = "")]
        filter: String,
        #[arg(long, default_value = "")]
        state: String,
        #[arg(long, default_value = "")]
        labels: String,
        #[arg(long, default_value = "")]
        sort: String,
        #[arg(long, default_value = "")]
        direction: String,
        #[arg(long, default_value = "")]
        since: String,
    },
    /// 列出我的 Pull Request
    Prs {
        #[arg(long, default_value = "")]
        state: String,
        #[arg(long, default_value = "")]
        scope: String,
        #[arg(long, default_value = "")]
        sort: String,
        #[arg(long, default_value = "")]
        direction: String,
        #[arg(long, default_value = "")]
        labels: String,
    },
    /// 查看我在某组织的成员资格
    OrgMembership { org: String },
    /// 退出某组织（需 --yes 确认）
    OrgLeave {
        org: String,
        #[arg(long)]
        yes: bool,
    },
    // ---- 以下 v5 接口（PAT），Tier B 剩余端点，移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
    /// 查看指定用户资料（v5 接口，与网页端 v1 的 View 不是同一套）
    Profile { username: String },
    /// 更新当前用户资料（v5 接口）
    UpdateProfile {
        #[arg(long, default_value = "")]
        nickname: String,
        #[arg(long, default_value = "")]
        company: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        email: String,
        #[arg(long = "github", default_value = "")]
        github_account: String,
        #[arg(long, default_value = "")]
        website: String,
        #[arg(long, default_value = "")]
        location: String,
    },
    /// 查看用户动态（events，按日期分组）
    Events {
        username: String,
        #[arg(long, default_value = "")]
        year: String,
    },
    // ---- 以下 v5 接口（PAT），移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
    /// 列出当前用户的邮箱
    Emails,
    /// 列出指定用户所属的组织
    Orgs { username: String },
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

    // MCP 是长时服务：凭据缺失时也要能启动（缺什么由具体工具在调用时报错，
    // AI 客户端才拿得到可读的提示），所以在加载 token 之前就分流出去。
    if let Command::Mcp(c) = &cli.command {
        let McpAction::Start {
            allow_destructive,
            max_concurrency,
        } = c.action;
        return crate::mcp::start(allow_destructive, max_concurrency);
    }

    let token = config::load_token()?;
    let api = ApiClient::new(config::api_base(), token)?;

    match cli.command {
        Command::Auth(c) => match c.action {
            None => {
                if config::load_token()?.is_none() {
                    eprintln!("未登录。运行 `gc auth login` 登录");
                    return Ok(());
                }
                let u = api.authenticated_user()?;
                println!(
                    "已认证: {} ({})",
                    u["login"].as_str().unwrap_or("?"),
                    u["name"].as_str().unwrap_or("?")
                );
            }
            Some(AuthAction::Login {
                web,
                with_token,
                qr,
                broker,
            }) => {
                crate::auth::login(web, with_token, qr, broker.as_deref())?;
            }
            Some(AuthAction::Git { local }) => {
                // 把 gc 注册为对应主机的 git credential helper：
                // git 在需要凭据时会调用 `gc auth credential`，由它输出 PAT，
                // 之后 HTTPS push/pull 不再要求输入账号密码。
                for host in ["gitcode.com", "atomgit.com"] {
                    let key = format!("credential.https://{}.helper", host);
                    // 注意：Command::args 是追加语义，必须每轮新建，否则参数会累积
                    let status = std::process::Command::new("git")
                        .arg("config")
                        .arg(if local { "--local" } else { "--global" })
                        .args(["--replace-all", &key, "!gc auth credential"])
                        .status()
                        .context("执行 git config 失败（未安装 git？）")?;
                    if !status.success() {
                        anyhow::bail!("git config {} 失败", key);
                    }
                    println!(
                        "已注册凭据助手：{}（{}）",
                        key,
                        if local { "本仓库" } else { "全局" }
                    );
                }
                println!("之后 git push/pull gitcode.com / atomgit.com 将自动使用 gc 的 PAT，无需再配置凭据。");
            }
            Some(AuthAction::Credential { operation }) => {
                // git credential helper 协议：helper 以 `gc auth credential <get|store|erase>` 调用，
                // stdin 收到请求（含 host=...），stdout 输出键值对。
                // store / erase 无事可做（token 常驻 gc 配置），直接成功退出。
                if operation != "get" {
                    return Ok(());
                }
                use std::io::Read;
                let mut input = String::new();
                let _ = std::io::stdin().read_to_string(&mut input);
                let host = input
                    .lines()
                    .find_map(|l| l.strip_prefix("host="))
                    .unwrap_or("")
                    .to_lowercase();
                // 只服务于 GitCode / AtomGit，其他主机不输出（git 会继续找下一个 helper）
                if !(host.contains("gitcode.com") || host.contains("atomgit.com")) {
                    std::process::exit(1);
                }
                let token = config::load_token()?.unwrap_or_default();
                if token.trim().is_empty() {
                    eprintln!("gc：未配置 PAT，请先执行 gc token set");
                    std::process::exit(1);
                }
                println!("username=gc");
                println!("password={}", token);
            }
            Some(AuthAction::Pal { web }) => {
                let token = if web {
                    let (access, _) = config::load_web_tokens()?;
                    access
                } else {
                    config::load_token()?.unwrap_or_default()
                };
                if token.trim().is_empty() {
                    anyhow::bail!("未配置 token，请先 gc token set / gc auth login");
                }
                // 明文直接输出，方便 $(gc auth pal) 管道使用；不带任何前缀
                println!("{}", token);
            }
            None => {
                // 无子命令时检查 token 有效性
                let token = config::load_token()?.unwrap_or_default();
                let api = ApiClient::new(
                    config::api_base(),
                    if token.is_empty() { None } else { Some(token) },
                )?;
                let me = api.authenticated_user()?;
                println!(
                    "token 有效，当前用户：{}",
                    me["login"].as_str().unwrap_or("?")
                );
            }
        },

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

        Command::Repo(c) => gc_repo::cli::run(c.action, &api)?,
        Command::Issue(c) => gc_issues::cli::run(c.action, &api)?,
        Command::Pr(c) => gc_pr::cli::run(c.action, &api)?,
        Command::Branch(c) => match c.action {
            BranchAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.branches(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    let protected = if it["protected"].as_bool().unwrap_or(false) {
                        " [受保护]"
                    } else {
                        ""
                    };
                    format!(
                        "{}{} — {}",
                        it["name"].as_str().unwrap_or("?"),
                        protected,
                        it["commit"]["sha"].as_str().unwrap_or("")
                    )
                });
            }
            BranchAction::View { repo, branch } => {
                let (o, n) = split_repo(&repo)?;
                println!("{}", pretty(&api.branch(&o, &n, &branch)?));
            }
            BranchAction::Create { repo, branch, from } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_branch(&o, &n, &from, &branch)?;
                println!(
                    "已创建分支 {}（起点 {}）",
                    v["name"].as_str().unwrap_or(&branch),
                    from
                );
            }
            BranchAction::Delete { repo, branch, yes } => {
                if !yes {
                    anyhow::bail!("删除分支不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_branch(&o, &n, &branch)?;
                println!("已删除分支 {}", branch);
            }
            BranchAction::Protect {
                repo,
                branch,
                pusher,
                merger,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut b = serde_json::Map::new();
                if !pusher.is_empty() {
                    b.insert("pusher".into(), Value::String(pusher));
                }
                if !merger.is_empty() {
                    b.insert("merger".into(), Value::String(merger));
                }
                let v = api.protect_branch(&o, &n, &branch, &Value::Object(b))?;
                println!("已保护分支 {}", branch);
                println!("{}", pretty(&v));
            }
            BranchAction::Unprotect { repo, branch } => {
                let (o, n) = split_repo(&repo)?;
                api.unprotect_branch(&o, &n, &branch)?;
                println!("已取消分支 {} 的保护", branch);
            }
            BranchAction::Rule(rc) => match rc.action {
                BranchRuleAction::List { repo } => {
                    let (o, n) = split_repo(&repo)?;
                    let v = api.branch_protect_rules(&o, &n, 0, 0)?;
                    print_list(&v, |it| {
                        format!(
                            "{} — 推送: {} / 合并: {}",
                            it["wildcard"].as_str().unwrap_or("?"),
                            it["pusher"].as_str().unwrap_or(""),
                            it["merger"].as_str().unwrap_or("")
                        )
                    });
                }
                BranchRuleAction::Create {
                    repo,
                    wildcard,
                    pusher,
                    merger,
                } => {
                    let (o, n) = split_repo(&repo)?;
                    api.create_branch_protect_rule(&o, &n, &wildcard, &pusher, &merger)?;
                    println!("已创建保护规则 {}", wildcard);
                }
                BranchRuleAction::Update {
                    repo,
                    wildcard,
                    pusher,
                    merger,
                } => {
                    if pusher.is_empty() && merger.is_empty() {
                        anyhow::bail!("至少要指定 --pusher 或 --merger");
                    }
                    let (o, n) = split_repo(&repo)?;
                    api.update_branch_protect_rule(&o, &n, &wildcard, &pusher, &merger)?;
                    println!("已更新保护规则 {}", wildcard);
                }
                BranchRuleAction::Delete {
                    repo,
                    wildcard,
                    yes,
                } => {
                    if !yes {
                        anyhow::bail!("删除保护规则不可撤销，确认请加 --yes");
                    }
                    let (o, n) = split_repo(&repo)?;
                    api.delete_branch_protect_rule(&o, &n, &wildcard)?;
                    println!("已删除保护规则 {}", wildcard);
                }
            },
        },

        Command::Commit(c) => match c.action {
            CommitAction::List {
                repo,
                sha,
                path,
                author,
                per_page,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut f: Vec<(&str, &str)> = Vec::new();
                if !sha.is_empty() {
                    f.push(("sha", &sha));
                }
                if !path.is_empty() {
                    f.push(("path", &path));
                }
                if !author.is_empty() {
                    f.push(("author", &author));
                }
                let v = api.commits(&o, &n, &f, 0, per_page)?;
                print_list(&v, |it| {
                    let s = it["sha"].as_str().unwrap_or("");
                    format!(
                        "{} {} — {}",
                        &s[..s.len().min(8)],
                        it["commit"]["message"]
                            .as_str()
                            .unwrap_or("")
                            .lines()
                            .next()
                            .unwrap_or(""),
                        it["commit"]["author"]["name"].as_str().unwrap_or("?")
                    )
                });
            }
            CommitAction::View { repo, sha } => {
                let (o, n) = split_repo(&repo)?;
                println!("{}", pretty(&api.commit(&o, &n, &sha)?));
            }
            CommitAction::Diff { repo, sha, patch } => {
                let (o, n) = split_repo(&repo)?;
                let text = if patch {
                    api.commit_patch(&o, &n, &sha)?
                } else {
                    api.commit_diff(&o, &n, &sha)?
                };
                println!("{}", text);
            }
            CommitAction::Compare { repo, base, head } => {
                let (o, n) = split_repo(&repo)?;
                println!("{}", pretty(&api.compare_commits(&o, &n, &base, &head)?));
            }
            CommitAction::Comment(cc) => match cc.action {
                CommitCommentAction::List { repo, sha } => {
                    let (o, n) = split_repo(&repo)?;
                    let v = api.commit_comments(&o, &n, &sha, 0, 0)?;
                    print_list(&v, |it| {
                        format!(
                            "#{} @{}: {}",
                            it["id"].as_u64().unwrap_or(0),
                            it["user"]["login"].as_str().unwrap_or("?"),
                            it["body"]
                                .as_str()
                                .unwrap_or("")
                                .lines()
                                .next()
                                .unwrap_or("")
                        )
                    });
                }
                CommitCommentAction::Add { repo, sha, body } => {
                    let (o, n) = split_repo(&repo)?;
                    let v = api.create_commit_comment(&o, &n, &sha, &body)?;
                    println!(
                        "已评论提交 {}（评论 id {}）",
                        sha,
                        v["id"].as_u64().unwrap_or(0)
                    );
                }
            },
        },

        Command::File(c) => match c.action {
            FileAction::List { repo, path, branch } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.contents(&o, &n, &path, &branch)?;
                if v.is_array() {
                    print_list(&v, |it| {
                        format!(
                            "{:<4} {}  ({} 字节)",
                            it["type"].as_str().unwrap_or("?"),
                            it["path"].as_str().unwrap_or("?"),
                            it["size"].as_u64().unwrap_or(0)
                        )
                    });
                } else {
                    println!("{}", pretty(&v));
                }
            }
            FileAction::Get {
                repo,
                path,
                branch,
                output,
            } => {
                let (o, n) = split_repo(&repo)?;
                let text = api.raw_file(&o, &n, &path, &branch)?;
                match output {
                    Some(dest) => {
                        std::fs::write(&dest, text.as_bytes())
                            .map_err(|e| anyhow!("写入 {} 失败: {}", dest, e))?;
                        println!("已写入 {}（{} 字节）", dest, text.len());
                    }
                    None => println!("{}", text),
                }
            }
            FileAction::Create {
                repo,
                path,
                from,
                message,
                branch,
            } => {
                let (o, n) = split_repo(&repo)?;
                let content = read_as_base64(&from)?;
                let v = api.create_file(&o, &n, &path, &message, &content, &branch)?;
                println!(
                    "已创建 {}（提交 {}）",
                    path,
                    v["commit"]["sha"].as_str().unwrap_or("")
                );
            }
            FileAction::Update {
                repo,
                path,
                from,
                message,
                branch,
            } => {
                let (o, n) = split_repo(&repo)?;
                // 覆盖写要带当前 blob sha 做乐观锁，先查一次。
                let cur = api.contents(&o, &n, &path, &branch)?;
                let sha = cur["sha"].as_str().ok_or_else(|| {
                    anyhow!("拿不到 {} 的当前 sha —— 该路径是目录还是不存在？", path)
                })?;
                let content = read_as_base64(&from)?;
                let v = api.update_file(&o, &n, &path, &message, &content, sha, &branch)?;
                println!(
                    "已更新 {}（提交 {}）",
                    path,
                    v["commit"]["sha"].as_str().unwrap_or("")
                );
            }
            FileAction::Delete {
                repo,
                path,
                message,
                branch,
                yes,
            } => {
                if !yes {
                    anyhow::bail!("删除文件不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                let cur = api.contents(&o, &n, &path, &branch)?;
                let sha = cur["sha"].as_str().ok_or_else(|| {
                    anyhow!("拿不到 {} 的当前 sha —— 该路径是目录还是不存在？", path)
                })?;
                api.delete_file(&o, &n, &path, &message, sha, &branch)?;
                println!("已删除 {}", path);
            }
        },

        Command::Tag(c) => match c.action {
            TagAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_tags(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} -> {}",
                        it["name"].as_str().unwrap_or("?"),
                        it["commit"]["sha"].as_str().unwrap_or("")
                    )
                });
            }
            TagAction::View { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_tag(&o, &n, &tag)?;
                println!("{}", pretty(&v));
            }
            TagAction::Create {
                repo,
                tag,
                refs,
                message,
            } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_tag(&o, &n, &refs, &tag, &message)?;
                println!(
                    "已创建标签 {}: {}",
                    v["name"].as_str().unwrap_or(&tag),
                    v["commit"]["url"].as_str().unwrap_or("")
                );
            }
            TagAction::Delete { repo, tag, yes } => {
                if !yes {
                    anyhow::bail!("删除标签不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_tag(&o, &n, &tag)?;
                println!("已删除标签 {}", tag);
            }
            TagAction::Protect { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                api.protect_tag(&o, &n, &tag)?;
                println!("已将标签 {} 设为受保护", tag);
            }
            TagAction::Unprotect { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                api.unprotect_tag(&o, &n, &tag)?;
                println!("已取消标签 {} 的保护", tag);
            }
            TagAction::Rules { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_protected_tags(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} (级别 {})",
                        it["name"].as_str().unwrap_or("?"),
                        it["create_access_level"].as_i64().unwrap_or(0)
                    )
                });
            }
            TagAction::RuleView { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_protected_tag(&o, &n, &tag)?;
                println!("{}", pretty(&v));
            }
            TagAction::RuleCreate {
                repo,
                tag,
                access_level,
            } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_protected_tag(&o, &n, &tag, access_level)?;
                println!(
                    "已创建受保护标签规则 {}",
                    v["name"].as_str().unwrap_or(&tag)
                );
            }
            TagAction::RuleUpdate {
                repo,
                tag,
                access_level,
            } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.update_protected_tag(&o, &n, &tag, access_level)?;
                println!(
                    "已更新受保护标签规则 {}",
                    v["name"].as_str().unwrap_or(&tag)
                );
            }
            TagAction::RuleDelete { repo, tag, yes } => {
                if !yes {
                    anyhow::bail!("删除受保护标签规则不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_protected_tag(&o, &n, &tag)?;
                println!("已删除受保护标签规则 {}", tag);
            }
        },

        Command::Hook(c) => match c.action {
            HookAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_hooks(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "#{} {} ({}) {}",
                        it["id"].as_i64().unwrap_or(0),
                        it["name"].as_str().unwrap_or("?"),
                        it["type"].as_str().unwrap_or(""),
                        it["config"]["url"].as_str().unwrap_or("")
                    )
                });
            }
            HookAction::View { repo, id } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_hook(&o, &n, id)?;
                println!("{}", pretty(&v));
            }
            HookAction::Create {
                repo,
                name,
                url,
                events,
                content_type,
                secret,
                active,
            } => {
                let (o, n) = split_repo(&repo)?;
                let evs: Vec<&str> = events
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut config = json!({ "url": url, "content_type": content_type });
                if !secret.is_empty() {
                    config["secret"] = json!(secret);
                }
                let body =
                    json!({ "name": name, "config": config, "events": evs, "active": active });
                let v = api.create_hook(&o, &n, &body)?;
                println!(
                    "已创建 Webhook #{} -> {}",
                    v["id"].as_i64().unwrap_or(0),
                    v["config"]["url"].as_str().unwrap_or(&url)
                );
            }
            HookAction::Update {
                repo,
                id,
                name,
                url,
                events,
                content_type,
                secret,
                active,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut body = json!({});
                if !name.is_empty() {
                    body["name"] = json!(name);
                }
                let mut config = json!({});
                let mut has_cfg = false;
                if !url.is_empty() {
                    config["url"] = json!(url);
                    has_cfg = true;
                }
                if !content_type.is_empty() {
                    config["content_type"] = json!(content_type);
                    has_cfg = true;
                }
                if !secret.is_empty() {
                    config["secret"] = json!(secret);
                    has_cfg = true;
                }
                if has_cfg {
                    body["config"] = config;
                }
                if !events.is_empty() {
                    let evs: Vec<&str> = events
                        .split(',')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .collect();
                    body["events"] = json!(evs);
                }
                if let Some(a) = active {
                    body["active"] = json!(a);
                }
                let v = api.update_hook(&o, &n, id, &body)?;
                println!("已更新 Webhook #{}", v["id"].as_i64().unwrap_or(id as i64));
            }
            HookAction::Delete { repo, id, yes } => {
                if !yes {
                    anyhow::bail!("删除 Webhook 不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_hook(&o, &n, id)?;
                println!("已删除 Webhook #{}", id);
            }
        },

        Command::Label(c) => match c.action {
            LabelAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_labels(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} (#{}) — {}",
                        it["name"].as_str().unwrap_or("?"),
                        it["color"].as_str().unwrap_or(""),
                        it["description"].as_str().unwrap_or("")
                    )
                });
            }
            LabelAction::View { repo, name } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_label(&o, &n, &name)?;
                println!("{}", pretty(&v));
            }
            LabelAction::Create {
                repo,
                name,
                color,
                description,
            } => {
                let (o, n) = split_repo(&repo)?;
                let body = json!({ "name": name, "color": color, "description": description });
                let v = api.create_label(&o, &n, &body)?;
                println!("已创建标签 {}", v["name"].as_str().unwrap_or(&name));
            }
            LabelAction::Update {
                repo,
                name,
                new_name,
                color,
                description,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut body = json!({});
                if !new_name.is_empty() {
                    body["new_name"] = json!(new_name);
                }
                if !color.is_empty() {
                    body["color"] = json!(color);
                }
                if !description.is_empty() {
                    body["description"] = json!(description);
                }
                let v = api.update_label(&o, &n, &name, &body)?;
                println!("已更新标签 {}", v["name"].as_str().unwrap_or(&name));
            }
            LabelAction::Delete { repo, name, yes } => {
                if !yes {
                    anyhow::bail!("删除标签不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_label(&o, &n, &name)?;
                println!("已删除标签 {}", name);
            }
        },

        Command::Milestone(c) => match c.action {
            MilestoneAction::List { repo, state } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_milestones(&o, &n, &state, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "#{} {} [{}] — {}",
                        it["number"].as_i64().unwrap_or(0),
                        it["title"].as_str().unwrap_or("?"),
                        it["state"].as_str().unwrap_or(""),
                        it["description"].as_str().unwrap_or("")
                    )
                });
            }
            MilestoneAction::View { repo, number } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_milestone(&o, &n, number)?;
                println!("{}", pretty(&v));
            }
            MilestoneAction::Create {
                repo,
                title,
                description,
                state,
                due_on,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut body = json!({ "title": title });
                if !description.is_empty() {
                    body["description"] = json!(description);
                }
                if !state.is_empty() {
                    body["state"] = json!(state);
                }
                if !due_on.is_empty() {
                    body["due_on"] = json!(due_on);
                }
                let v = api.create_milestone(&o, &n, &body)?;
                println!(
                    "已创建里程碑 #{} {}",
                    v["number"].as_i64().unwrap_or(0),
                    v["title"].as_str().unwrap_or(&title)
                );
            }
            MilestoneAction::Update {
                repo,
                number,
                title,
                description,
                state,
                due_on,
            } => {
                let (o, n) = split_repo(&repo)?;
                let mut body = json!({});
                if !title.is_empty() {
                    body["title"] = json!(title);
                }
                if !description.is_empty() {
                    body["description"] = json!(description);
                }
                if !state.is_empty() {
                    body["state"] = json!(state);
                }
                if !due_on.is_empty() {
                    body["due_on"] = json!(due_on);
                }
                let v = api.update_milestone(&o, &n, number, &body)?;
                println!(
                    "已更新里程碑 #{}",
                    v["number"].as_i64().unwrap_or(number as i64)
                );
            }
            MilestoneAction::Delete { repo, number, yes } => {
                if !yes {
                    anyhow::bail!("删除里程碑不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_milestone(&o, &n, number)?;
                println!("已删除里程碑 #{}", number);
            }
        },

        Command::Collaborator(c) => match c.action {
            CollaboratorAction::List { repo, affiliation } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_collaborators(&o, &n, &affiliation, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({})",
                        it["login"].as_str().unwrap_or("?"),
                        it["type"].as_str().unwrap_or("")
                    )
                });
            }
            CollaboratorAction::Add {
                repo,
                username,
                permission,
            } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.add_collaborator(&o, &n, &username, &permission)?;
                println!(
                    "已添加协作者 {}（权限 {}）",
                    v["login"].as_str().unwrap_or(&username),
                    permission
                );
            }
            CollaboratorAction::Remove {
                repo,
                username,
                yes,
            } => {
                if !yes {
                    anyhow::bail!("移除协作者不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.remove_collaborator(&o, &n, &username)?;
                println!("已移除协作者 {}", username);
            }
            CollaboratorAction::Permission { repo, username } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.collaborator_permission(&o, &n, &username)?;
                let perm = v["permission"].as_str().unwrap_or("?");
                let login = v["user"]["login"].as_str().unwrap_or(&username);
                println!("{} 的权限：{}", login, perm);
            }
        },

        Command::Org(c) => match c.action {
            OrgAction::List => {
                let v = api.list_orgs(0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({}) — {}",
                        it["login"].as_str().unwrap_or("?"),
                        it["name"].as_str().unwrap_or(""),
                        it["description"].as_str().unwrap_or("")
                    )
                });
            }
            OrgAction::View { org } => {
                let v = api.get_org(&org)?;
                println!("{}", pretty(&v));
            }
            OrgAction::Members { org, role } => {
                let v = api.list_org_members(&org, &role, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({})",
                        it["login"].as_str().unwrap_or("?"),
                        it["type"].as_str().unwrap_or("")
                    )
                });
            }
            OrgAction::Repos { org, r#type } => {
                let v = api.list_org_repos(&org, &r#type, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} {} — {}",
                        it["full_name"].as_str().unwrap_or("?"),
                        if it["private"].as_bool().unwrap_or(false) {
                            "[私有]"
                        } else {
                            ""
                        },
                        it["description"].as_str().unwrap_or("")
                    )
                });
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
            ReleaseAction::Create {
                repo,
                tag,
                name,
                body,
                target,
            } => {
                let (o, n) = split_repo(&repo)?;
                // 名称与正文在同一个编辑器里一次填完；名称留空则回退到 tag。
                let (rname, body) = if name.is_some() && !body.is_empty() {
                    (name.unwrap(), body)
                } else {
                    let iname = name.clone().unwrap_or_default();
                    edit_title_body(&iname, &body)?
                };
                let rname = if rname.trim().is_empty() {
                    tag.clone()
                } else {
                    rname
                };
                let v = api.create_release(&o, &n, &tag, &rname, &body, &target)?;
                println!(
                    "已创建发行版: {} {}",
                    v["tag_name"].as_str().unwrap_or(""),
                    v["html_url"].as_str().unwrap_or("")
                );
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
            SearchAction::Web {
                query,
                r#type,
                page,
                per_page,
                json,
            } => {
                let raw = crate::api::web_search(&query, &r#type, page, per_page)?;
                let d = crate::api::search_digest(&raw, &r#type);
                if json {
                    println!("{}", serde_json::to_string_pretty(&d)?);
                } else {
                    print_web_search(&d, &r#type);
                }
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
            println!(
                "网页端 token 已刷新并保存到 {}",
                config::config_path()?.display()
            );
        }

        Command::Install { system } => {
            run_install(system)?;
        }
        Command::Skill(c) => match c.action {
            gc_skill::SkillAction::Install { tools, all, force } => {
                gc_skill::run_skill_install(&tools, all, force)?;
            }
            gc_skill::SkillAction::List => {
                gc_skill::run_skill_list()?;
            }
        },

        Command::WebToken(c) => match c.action {
            WebTokenAction::Set => {
                config::prompt_and_save_web_tokens()?;
                println!("已保存网页端 token 到 {}", config::config_path()?.display());
            }
        },
        Command::Api(a) => {
            let token = config::load_token()?.unwrap_or_default();
            let api = ApiClient::new(
                config::api_base(),
                if token.is_empty() { None } else { Some(token) },
            )?;
            let v = api.raw(
                &a.method,
                &a.path,
                if a.body.is_empty() {
                    None
                } else {
                    Some(&a.body)
                },
            )?;
            println!("{}", pretty(&v));
        }
        Command::Tree(a) => gc_repo::cli::run_tree(&a.repo, &a.branch)?,
        Command::Read(a) => {
            gc_repo::cli::run_read(&a.repo, &a.file, &a.branch, a.output.as_deref())?
        }
        Command::Download(a) => {
            gc_repo::cli::run_download(&a.repo, &a.files, &a.branch, &a.output)?
        }
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
                    std::fs::write(&out, &bytes)
                        .map_err(|e| anyhow!("写入 {} 失败: {}", out, e))?;
                    println!("头像已下载: {} ({} 字节)", out, bytes.len());
                } else {
                    println!("{}", p.avatar);
                }
            }
            // ---- v5 接口（PAT），移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
            UserAction::Followers { username } => {
                let v = api.user_followers(&username, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({})",
                        it["login"].as_str().unwrap_or("?"),
                        it["name"].as_str().unwrap_or("")
                    )
                });
            }
            UserAction::Following { username } => {
                let v = api.user_following(&username, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({})",
                        it["login"].as_str().unwrap_or("?"),
                        it["name"].as_str().unwrap_or("")
                    )
                });
            }
            UserAction::Starred { username } => {
                let v = api.user_starred(&username, "", "", 0, 0)?;
                print_list(&v, |it| {
                    format!("{}", it["full_name"].as_str().unwrap_or("?"))
                });
            }
            UserAction::Subscriptions { username } => {
                let v = api.user_subscriptions(&username, "", "", 0, 0)?;
                print_list(&v, |it| {
                    format!("{}", it["full_name"].as_str().unwrap_or("?"))
                });
            }
            UserAction::Namespaces { mode } => {
                let v = api.user_namespaces(&mode, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({}) — {}",
                        it["path"].as_str().unwrap_or("?"),
                        it["type"].as_str().unwrap_or(""),
                        it["name"].as_str().unwrap_or("")
                    )
                });
            }
            UserAction::Namespace { path } => {
                let v = api.user_namespace(&path)?;
                println!("{}", pretty(&v));
            }
            UserAction::Keys => {
                let v = api.user_keys(0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "#{} {} — {}",
                        it["id"].as_i64().unwrap_or(0),
                        it["title"].as_str().unwrap_or(""),
                        it["key"].as_str().unwrap_or("")
                    )
                });
            }
            UserAction::Key { id } => {
                let v = api.user_key(id)?;
                println!("{}", pretty(&v));
            }
            UserAction::KeyAdd { title, key } => {
                let v = api.add_user_key(&title, &key)?;
                println!(
                    "已添加 SSH 公钥 #{}: {}",
                    v["id"].as_i64().unwrap_or(0),
                    v["title"].as_str().unwrap_or(&title)
                );
            }
            UserAction::KeyDelete { id, yes } => {
                if !yes {
                    anyhow::bail!("删除 SSH 公钥不可撤销，确认请加 --yes");
                }
                api.delete_user_key(id)?;
                println!("已删除 SSH 公钥 #{}", id);
            }
            UserAction::Issues {
                filter,
                state,
                labels,
                sort,
                direction,
                since,
            } => {
                let v =
                    api.user_issues(&filter, &state, &labels, &sort, &direction, &since, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "#{} {} [{}]",
                        it["number"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or(""),
                        it["state"].as_str().unwrap_or("")
                    )
                });
            }
            UserAction::Prs {
                state,
                scope,
                sort,
                direction,
                labels,
            } => {
                let v = api.user_prs(&state, &scope, &sort, &direction, &labels, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "#{} {} (仓库 #{})",
                        it["number"].as_i64().unwrap_or(0),
                        it["title"].as_str().unwrap_or(""),
                        it["repository_id"].as_i64().unwrap_or(0)
                    )
                });
            }
            UserAction::OrgMembership { org } => {
                let v = api.org_membership(&org)?;
                println!("{}", pretty(&v));
            }
            UserAction::OrgLeave { org, yes } => {
                if !yes {
                    anyhow::bail!("退出组织不可撤销，确认请加 --yes");
                }
                api.leave_org(&org)?;
                println!("已退出组织 {}", org);
            }
            // ---- v5 接口（PAT），Tier B 剩余端点，移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
            UserAction::Profile { username } => {
                let v = api.get_user(&username)?;
                println!("{}", pretty(&v));
            }
            UserAction::UpdateProfile {
                nickname,
                company,
                description,
                email,
                github_account,
                website,
                location,
            } => {
                let mut body = json!({});
                if !nickname.is_empty() {
                    body["nickname"] = json!(nickname);
                }
                if !company.is_empty() {
                    body["company"] = json!(company);
                }
                if !description.is_empty() {
                    body["description"] = json!(description);
                }
                if !email.is_empty() {
                    body["email"] = json!(email);
                }
                if !github_account.is_empty() {
                    body["github_account"] = json!(github_account);
                }
                if !website.is_empty() {
                    body["website"] = json!(website);
                }
                if !location.is_empty() {
                    body["location"] = json!(location);
                }
                let v = api.update_user(&body)?;
                println!("已更新资料: {}", v["login"].as_str().unwrap_or(""));
            }
            UserAction::Events { username, year } => {
                let v = api.user_events(&username, &year, "")?;
                println!("{}", pretty(&v));
            }
            // ---- v5 接口（PAT），移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
            UserAction::Emails => {
                let v = api.user_emails()?;
                print_list(&v, |it| {
                    format!(
                        "{} {}（{}）{}",
                        it["email"].as_str().unwrap_or("?"),
                        if it["primary"].as_bool().unwrap_or(false) {
                            "[主]"
                        } else {
                            ""
                        },
                        it["state"].as_str().unwrap_or(""),
                        if it["verified"].as_bool().unwrap_or(false) {
                            " 已验证"
                        } else {
                            ""
                        },
                    )
                });
            }
            UserAction::Orgs { username } => {
                let v = api.user_orgs_by_username(&username, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "{} ({}) — {}",
                        it["login"].as_str().unwrap_or("?"),
                        it["name"].as_str().unwrap_or(""),
                        it["description"].as_str().unwrap_or("")
                    )
                });
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
        // 在 run() 开头就已经分流走了，走不到这里。
        Command::Mcp(_) => unreachable!("mcp 子命令在加载凭据前已处理"),
    }

    Ok(())
}

/// `gc install` 实现：把当前运行的 gc 二进制复制到 PATH 目录。
/// 默认 ~/.local/bin（用户级，无需权限）；--system 装到 /usr/local/bin，
/// 非 root 时自动通过 sudo 执行复制。
fn run_install(system: bool) -> Result<()> {
    let src = std::env::current_exe().context("无法定位当前 gc 二进制")?;
    let (dest_dir, need_sudo) = if system {
        (std::path::PathBuf::from("/usr/local/bin"), true)
    } else {
        let home = dirs::home_dir().context("无法获取 HOME 目录")?;
        (home.join(".local/bin"), false)
    };

    // 目标目录不存在则创建（用户目录直接建；系统目录交给 sudo 段处理）
    if !dest_dir.exists() && !need_sudo {
        std::fs::create_dir_all(&dest_dir)
            .with_context(|| format!("创建 {} 失败", dest_dir.display()))?;
    }
    let dest = dest_dir.join("gc");

    // 已装旧版时提示覆盖
    if dest.exists() {
        println!("覆盖已存在的 {}", dest.display());
    }

    if need_sudo {
        // 系统级：非 root 时自动走 sudo cp，保留可执行权限
        #[cfg(unix)]
        let is_root = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata("/proc/self")
                .map(|m| m.uid() == 0)
                .unwrap_or(false)
        };
        #[cfg(not(unix))]
        let is_root = std::env::var("USER").map(|u| u == "root").unwrap_or(false);
        if !is_root {
            println!("--system 需要 root，自动执行 sudo ...");
        }
        // sudo -n 优先尝试免密；失败回退交互式 sudo
        let status = std::process::Command::new("sudo")
            .args([
                "cp",
                src.to_string_lossy().as_ref(),
                dest.to_string_lossy().as_ref(),
            ])
            .status()
            .context("执行 sudo 失败（未安装 sudo？）")?;
        if !status.success() {
            anyhow::bail!("sudo cp 失败");
        }
        let _ = std::process::Command::new("sudo")
            .args(["chmod", "755", dest.to_string_lossy().as_ref()])
            .status();
    } else {
        std::fs::copy(&src, &dest)
            .with_context(|| format!("复制 {} -> {} 失败", src.display(), dest.display()))?;
        // 确保可执行位
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&dest)?.permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&dest, perm)?;
        }
    }

    println!("已安装到 {}", dest.display());
    // PATH 提示
    let in_path = std::env::var("PATH")
        .map(|p| {
            p.split(':').any(|d| {
                std::path::Path::new(d).join("gc").exists() || d == dest_dir.to_string_lossy()
            })
        })
        .unwrap_or(false);
    if !in_path {
        println!(
            "注意：{} 不在 PATH 里，请把它加入 shell 配置（如 ~/.bashrc）",
            dest_dir.display()
        );
    }
    Ok(())
}
/// 打印列表：兼容直接数组、{ "data": [...] } 与 { "issues": [...] } 三种返回结构
/// 打印 `gc search web` 的精简结果（输入是 api::search_digest 的输出）。
fn print_web_search(d: &Value, kind: &str) {
    let items = d["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("(无结果)");
        return;
    }
    let s = |it: &Value, k: &str| it[k].as_str().unwrap_or("").to_string();
    for it in &items {
        match kind {
            "user" => {
                let (u, n) = (s(it, "username"), s(it, "nickname"));
                if n.is_empty() || n == u {
                    println!("{}", u);
                } else {
                    println!("{} ({})", u, n);
                }
            }
            "issue" => println!(
                "{}#{} [{}] {}",
                s(it, "repo"),
                it["iid"].as_u64().unwrap_or(0),
                s(it, "state"),
                s(it, "title")
            ),
            _ => {
                let desc = s(it, "description");
                let lang = s(it, "language");
                println!(
                    "{}  ★{}{}{}",
                    s(it, "repo"),
                    it["stars"].as_u64().unwrap_or(0),
                    if lang.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", lang)
                    },
                    if desc.is_empty() {
                        String::new()
                    } else {
                        format!("\n    {}", desc)
                    }
                );
            }
        }
    }
    println!(
        "\n共 {} 条，第 {}/{} 页",
        d["total"].as_u64().unwrap_or(0),
        d["page"].as_u64().unwrap_or(1),
        d["page_count"].as_u64().unwrap_or(0)
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use gc_repo::cli::{build_tree_string, is_text};

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
    fn test_parse_gitcode_repo_from_remote_urls() {
        assert_eq!(
            parse_gitcode_repo("https://gitcode.com/alice/demo.git"),
            Some("alice/demo".to_string())
        );
        assert_eq!(
            parse_gitcode_repo("https://gitcode.com/alice/demo"),
            Some("alice/demo".to_string())
        );
        assert_eq!(
            parse_gitcode_repo("git@gitcode.com:alice/demo.git"),
            Some("alice/demo".to_string())
        );
        // 非 gitcode 远程应返回 None
        assert_eq!(parse_gitcode_repo("git@github.com:alice/demo.git"), None);
        assert_eq!(parse_gitcode_repo("not a url"), None);
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
        assert_eq!(
            config::clone_url(&o, &n),
            "https://gitcode.com/ghhhJoshrjnrhr/gb.git"
        );
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
