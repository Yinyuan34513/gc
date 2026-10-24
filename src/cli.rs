use anyhow::{anyhow, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use serde_json::Value;
use serde_json::json;
use serde_json::Map;

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
    /// 认证（无子命令时检查当前 token 是否有效）
    Auth(AuthCmd),
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
    /// 安装 gc 二进制到 PATH（默认 ~/.local/bin；--system 装到 /usr/local/bin，需要时自动 sudo）
    Install {
        /// 安装到系统目录 /usr/local/bin（非 root 时自动用 sudo）
        #[arg(long)]
        system: bool,
    },
    /// 把 gc 的使用技能安装到 AI 编码工具（自动检测 Claude Code / Codex / OpenCode / CodeBuddy / Gemini CLI / AtomCode）
    #[command(name = "skill", alias = "extension", alias = "extsion")]
    Skill(SkillCmd),
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
    Compare { repo: String, base: String, head: String },
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
struct IssueCmd {
    #[command(subcommand)]
    action: IssueAction,
}
#[derive(Subcommand)]
enum IssueAction {
    /// 列出 Issue
    List {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        /// 仓库（位置参数，owner/name 或 URL）
        repo_pos: Option<String>,
        #[arg(long, default_value = "open")]
        state: String,
        /// 打印完整 JSON
        #[arg(long)]
        json: bool,
        /// 用 jq 表达式过滤输出（优先于 --json）
        #[arg(long)]
        jq: Option<String>,
    },
    /// 查看某个 Issue
    View {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 打印完整 JSON
        #[arg(long)]
        json: bool,
        /// 用 jq 表达式过滤输出（优先于 --json）
        #[arg(long)]
        jq: Option<String>,
    },
    /// 创建 Issue
    Create {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        /// 标题（省略则打开编辑器输入首行作为标题）
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")] body: String,
        /// 从 Markdown 文件读取正文（覆盖 --body）
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// 附件（可重复多次）
        #[arg(long, num_args = 0..)]
        attach: Vec<String>,
        /// 标签（可重复多次）
        #[arg(long, num_args = 0..)]
        label: Vec<String>,
        /// 私有 Issue
        #[arg(long)]
        confidential: bool,
    },
    /// 关闭 Issue（可同时指定多个编号）
    Close {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        #[arg(num_args = 1..)]
        issues: Vec<u64>,
    },
    /// 重新打开 Issue（可同时指定多个编号）
    Reopen {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        #[arg(num_args = 1..)]
        issues: Vec<u64>,
    },
    /// 回复（评论）某个 Issue
    Reply {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        issue: u64,
        #[arg(long)] body: String,
    },
    /// 编辑 Issue 的标题 / 描述
    Edit {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        issue: u64,
        /// 新标题（不填则不修改标题）
        #[arg(long)]
        title: Option<String>,
        /// 新描述（不填则若也未给 --file，则打开编辑器交互编辑当前描述）
        #[arg(long)]
        body: Option<String>,
        /// 从文件读取描述
        #[arg(long)]
        file: Option<String>,
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
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        /// 仓库（位置参数，owner/name 或 URL）
        repo_pos: Option<String>,
        #[arg(long, default_value = "open")]
        state: String,
        /// 走网页端接口（支持排序与统计）
        #[arg(long)]
        web: bool,
        /// 网页端排序字段：updated_at / merged_at / created_at（默认 updated_at）
        #[arg(long, default_value = "updated_at")]
        order_by: String,
        /// 网页端排序方向：asc / desc（默认 desc）
        #[arg(long, default_value = "desc")]
        sort: String,
        /// 网页端页码
        #[arg(long, default_value_t = 1)]
        page: u64,
        /// 网页端每页数量
        #[arg(long, default_value_t = 10)]
        per_page: u64,
        /// 只显示各状态 PR 的数量统计（all/opened/merged/closed）
        #[arg(long)]
        count: bool,
        /// 打印完整 JSON（跳过 --web 的分页包裹也原样输出）
        #[arg(long)]
        json: bool,
        /// 用 jq 表达式过滤输出（优先于 --json）
        #[arg(long)]
        jq: Option<String>,
    },
    /// 显示相关 PR 的状态（当前分支 / 我创建 / 指派给我 / 待我评审）
    Status {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
    },
    /// 在 git 中检出 PR（创建本地分支 pr/<编号>/<源分支>）
    Checkout {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 查看单个 PR 的 CI / 合并状态
    Checks {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 查看 PR 的改动
    Diff {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 仅显示改动文件列表
        #[arg(long)]
        name_only: bool,
    },
    /// 锁定 PR（讨论将被锁定）
    Lock {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 将 PR 标记为准备评审（退出草稿）
    Ready {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 解锁 PR（解除锁定）
    Unlock {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 查看某个 PR
    View {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 打印完整 JSON
        #[arg(long)]
        json: bool,
        /// 用 jq 表达式过滤输出（优先于 --json）
        #[arg(long)]
        jq: Option<String>,
    },
    /// 创建 Pull Request（创建前用网页端 v2 对比接口预览 diff）
    Create {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        /// 标题（省略则打开编辑器输入首行作为标题）
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")] body: String,
        /// 从 Markdown 文件读取正文（覆盖 --body）
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// 源分支（要合并的分支，对应 compare 的 from）
        #[arg(long)] head: String,
        /// 目标分支（合并到的分支，对应 compare 的 to）
        #[arg(long)] base: String,
        /// 跨仓库合并时的目标仓库 owner/name（默认与 repo 相同）
        #[arg(long)]
        target_repo: Option<String>,
        /// 同时打印每个文件的具体 diff 文本
        #[arg(long)]
        patch: bool,
    },

    // ---- 以下 PR 生命周期子命令移植自 @oh-gc/cli（MIT, Copyright (c) guozejun），
    // ---- 版权声明见 LICENSE.oh-gc。
    /// 合并 PR
    Merge {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 合并方式：merge / squash / rebase（默认由服务端决定）
        #[arg(long, default_value = "")]
        method: String,
        /// 合并提交标题
        #[arg(long, default_value = "")]
        title: String,
        /// 合并提交说明
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 关闭 PR（可同时指定多个编号）
    Close {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        #[arg(num_args = 1..)]
        numbers: Vec<u64>,
    },
    /// 重新打开 PR（可同时指定多个编号）
    Reopen {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        #[arg(num_args = 1..)]
        numbers: Vec<u64>,
    },
    /// 编辑 PR 的标题 / 正文 / 草稿状态
    Edit {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        /// 标记为草稿 / 取消草稿
        #[arg(long)]
        draft: Option<bool>,
    },
    /// 列出 PR 改动的文件
    Files {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 列出 PR 的提交
    Commits {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// PR 评论：列出 / 发表 / 编辑 / 删除
    Comment(PrCommentCmd),
    /// 评审人管理
    Reviewer(PrUserCmd),
    /// 测试人管理
    Tester(PrUserCmd),
    /// 指派人管理
    Assignee(PrUserCmd),
    /// 提交评审通过
    Review {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 绕过未满足的评审条件
        #[arg(long)]
        force: bool,
    },
    /// 提交测试通过
    Test {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(long)]
        force: bool,
    },
    /// 标签管理
    Label(PrLabelCmd),
    /// 关联 Issue 管理
    Link(PrLinkCmd),
    /// Cherry-pick 已合并的 PR（网页端接口，可同时创建新 MR）
    CherryPick {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// cherry-pick 到的目标分支（默认目标仓库默认分支）
        #[arg(long)]
        branch: Option<String>,
        /// cherry-pick 后自动创建新的 MR
        #[arg(long, default_value_t = true)]
        with_new_merge_request: bool,
    },
    /// Revert 已合并的 PR（网页端接口，可同时创建新 MR）
    Revert {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// revert 到的目标分支
        #[arg(long)]
        branch: Option<String>,
        /// revert 后自动创建新的 MR
        #[arg(long, default_value_t = true)]
        with_new_merge_request: bool,
    },
    /// PR 审批：approve（通过）/ reset（撤销）（网页端接口）
    Approval {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 操作：approve 或 reset
        #[arg(long)]
        action: String,
        /// 绕过未满足的审批条件
        #[arg(long)]
        force: bool,
    },
    /// 查看 PR 的修改历史
    History {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 查看 PR 的操作日志
    Logs {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
}

#[derive(Parser)]
struct PrCommentCmd {
    #[command(subcommand)]
    action: PrCommentAction,
}
#[derive(Subcommand)]
enum PrCommentAction {
    /// 列出 PR 的评论
    List {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 发表评论；给了 --path 与 --position 则为行级评论
    Add {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(long)]
        body: String,
        /// 从 Markdown 文件读取评论正文（覆盖 --body）
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// 行级评论的文件路径
        #[arg(long)]
        path: Option<String>,
        /// 行级评论的行号
        #[arg(long)]
        position: Option<u64>,
        /// 走网页端接口（POST /issuepr/api/v1/projects/.../merge_request/{iid}/notes）
        #[arg(long)]
        web: bool,
    },
    /// 在某条讨论线程下回复
    Reply {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 讨论线程 id
        discussion_id: String,
        #[arg(long)]
        body: String,
    },
    /// 编辑评论（comment_id 取自 list 的 id 字段）
    Edit {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        comment_id: u64,
        #[arg(long)]
        body: String,
    },
    /// 删除评论
    Delete {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        comment_id: u64,
        /// 确认删除
        #[arg(long)]
        yes: bool,
    },
}

/// 评审人 / 测试人 / 指派人共用的参数形状（都是逗号分隔的用户名）。
#[derive(Parser)]
struct PrUserCmd {
    #[command(subcommand)]
    action: PrUserAction,
}
#[derive(Subcommand)]
enum PrUserAction {
    /// 添加（默认覆盖现有名单，加 --append 改为追加）
    Add {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        /// 逗号分隔的用户名
        users: String,
        #[arg(long)]
        append: bool,
    },
    /// 移除（用户名留空表示全部移除，仅指派人支持）
    Remove {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(default_value = "")]
        users: String,
    },
}

#[derive(Parser)]
struct PrLabelCmd {
    #[command(subcommand)]
    action: PrLabelAction,
}
#[derive(Subcommand)]
enum PrLabelAction {
    /// 追加标签
    Add {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(num_args = 1..)]
        labels: Vec<String>,
    },
    /// 覆盖标签（传什么就只剩什么）
    Set {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(num_args = 1..)]
        labels: Vec<String>,
    },
    /// 移除标签
    Remove {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(num_args = 1..)]
        labels: Vec<String>,
    },
}

#[derive(Parser)]
struct PrLinkCmd {
    #[command(subcommand)]
    action: PrLinkAction,
}
#[derive(Subcommand)]
enum PrLinkAction {
    /// 列出已关联的 Issue
    List {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
    },
    /// 关联 Issue
    Add {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(num_args = 1..)]
        issues: Vec<u64>,
    },
    /// 取消关联
    Remove {
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        number: u64,
        #[arg(num_args = 1..)]
        issues: Vec<u64>,
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
        /// 发行版名称（省略则打开编辑器输入首行；留空则用 tag）
        #[arg(long)]
        name: Option<String>,
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
        let McpAction::Start { allow_destructive, max_concurrency } = c.action;
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
            Some(AuthAction::Login { web, with_token }) => {
                crate::auth::login(web, with_token)?;
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
                    println!("已注册凭据助手：{}（{}）", key, if local { "本仓库" } else { "全局" });
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
                let api = ApiClient::new(config::api_base(), if token.is_empty() { None } else { Some(token) })?;
                let me = api.authenticated_user()?;
                println!("token 有效，当前用户：{}", me["login"].as_str().unwrap_or("?"));
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

        Command::Repo(c) => match c.action {
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
            RepoAction::Grep { pattern, include, repo, owner, branch, ignore_case } => {
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
            RepoAction::Fork { repo, source, org, name, description, force, link_remote, private } => {
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
                        so, sn, fork_full
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
                    let sha = sb["commit"]["sha"].as_str().ok_or_else(|| {
                        anyhow!("拿不到源分支 {}/{} 的 HEAD", so, sn)
                    })?;
                    // 硬重置：受保护时自动解保护、重建并尽量恢复保护
                    api.hard_reset_branch(&namespace, &new_name, branch, sha)?;
                    println!("已强制同步已有 fork {} 的 {} 到 {}/{} 的 HEAD ({})", fork_full, branch, so, sn, sha);
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
                        println!("已将可见性设为：{}", pv["visibility"].as_str().unwrap_or("?"));
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
            RepoAction::Sync { repo, branch, source, force, yes, no_wait } => {
                // 1. 解析目标仓库：参数或本地 git 推断
                let target = match repo {
                    Some(r) => r,
                    None => detect_repo_from_cwd()
                        .ok_or_else(|| anyhow!("当前目录不是 gitcode 仓库（无法从 git remote 推断），请显式指定 <REPO> 或用 --source"))?,
                };
                let (o, n) = split_repo(&target)?;
                let info = api.repo(&o, &n)?;
                let branch = if branch.is_empty() {
                    info["default_branch"].as_str().unwrap_or("main").to_string()
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
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
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

            RepoAction::Update { repo, name, description, homepage, private, public, default_branch } => {
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
                println!("已更新 {}", v["full_name"].as_str().unwrap_or(&format!("{}/{}", o, n)));
                println!("{}", pretty(&v));
            }
            RepoAction::Transfer { new_owner, repo, team_ids, yes } => {
                let (o, n) = resolve_repo(repo)?;
                if !yes {
                    anyhow::bail!(
                        "转移会把 {}/{} 的归属交给 {}（原路径将失效、需对方接收），确认请加 --yes",
                        o, n, new_owner
                    );
                }
                let ids: Vec<u64> = team_ids
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<u64>().map_err(|_| anyhow!("--team-ids 里的 {} 不是数字", s)))
                    .collect::<Result<_>>()?;
                let v = api.transfer_repo(&o, &n, &new_owner, &ids)?;
                println!("已转移到 {}", v["full_name"].as_str().unwrap_or(&new_owner));
            }
            RepoAction::Forks { repo, sort, page, per_page } => {
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
            RepoAction::Contributors { repo, kind, stats, author, page, per_page } => {
                let (o, n) = resolve_repo(repo)?;
                if stats {
                    let v = api.contributor_statistics(&o, &n, &author, page, per_page)?;
                    print_list(&v, |it| {
                        // 汇总数在 overview 里，顶层只有 name/email 和按天拆分的 contributions
                        let s = if it["overview"].is_object() { &it["overview"] } else { it };
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
                            it["login"].as_str().or_else(|| it["name"].as_str()).unwrap_or("?"),
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
                        let mut rows: Vec<(&String, f64)> =
                            m.iter().map(|(k, v)| (k, v.as_f64().unwrap_or(0.0))).collect();
                        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
                        for (lang, pct) in rows {
                            println!("{:<20} {:>6.2}%", lang, pct);
                        }
                    }
                    _ => println!("{}", pretty(&v)),
                }
            }
            RepoAction::Events { repo, filter, author, before, after, page, per_page } => {
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
            RepoAction::Stats { repo, branch, author, since, until, only_self, downloads, direction } => {
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
                    let v = api.commit_statistics(&o, &n, &branch, &author, only_self, &since, &until)?;
                    // 返回体是 {commits: [...], statistics: [...], total}，要看的是 statistics
                    print_list(&v["statistics"], |it| {
                        format!(
                            "{:<24} +{:<7} -{:<7} {} commits",
                            it["nick_name"].as_str().or_else(|| it["user_name"].as_str()).unwrap_or("?"),
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
            RepoAction::Module { repo, wiki, issues, security, merge_request, fork, analysis, discussion } => {
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
            RepoAction::Archive { repo, open, password } => {
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
        },

        Command::Issue(c) => match c.action {
            IssueAction::List { repo, repo_pos, state, json, jq } => {
                let (o, n) = resolve_repo(repo.or(repo_pos))?;
                let token = web_access_token()?;
                let v = crate::api::issue_list(&token, &o, &n, &state)?;
                if print_structured(&v, json, &jq)? {
                    return Ok(());
                }
                print_list(&v, |it| {
                    format!(
                        "#{} [{}] {}",
                        it["iid"].as_u64().unwrap_or(0),
                        it["state"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
            IssueAction::View { repo, number, json, jq } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let v = crate::api::issue_view(&token, &o, &n, number)?;
                if !print_structured(&v, json, &jq)? {
                    println!(
                        "#{} [{}] {}",
                        number,
                        v["state"].as_str().unwrap_or("?"),
                        v["title"].as_str().unwrap_or("(无标题)")
                    );
                    if let Some(a) = v["author"]["username"]
                        .as_str()
                        .or_else(|| v["user"]["username"].as_str())
                    {
                        println!("作者: {}", a);
                    }
                    println!();
                    render_md(
                        v["description"]
                            .as_str()
                            .or_else(|| v["body"].as_str())
                            .unwrap_or(""),
                    );
                }
            }
            IssueAction::Create { repo, title, body, file, attach, label, confidential } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let body = match &file {
                    Some(f) => std::fs::read_to_string(f)
                        .with_context(|| format!("读取正文文件 {} 失败", f))?,
                    None => body,
                };
                // 标题与正文在同一个编辑器里一次填完（缺省时打开编辑器；都给了则跳过编辑器）。
                let (title, description) = if title.is_some() && !body.is_empty() {
                    (title.unwrap(), body)
                } else {
                    let it = title.clone().unwrap_or_default();
                    edit_title_body(&it, &body)?
                };
                if title.trim().is_empty() {
                    anyhow::bail!("标题不能为空（请在编辑器首行填写，或用 --title 指定）");
                }
                // 数值型 project id（附件链接 user-attachments/files/{id}/... 用数值 id）
                let project_id = api.repo(&o, &n)?
                    ["id"].as_u64().unwrap_or(0);
                // 构造描述：正文 + 附件链接（网页端把附件以 <a href> 形式嵌进 description）
                let mut description = description;
                for f in &attach {
                    let p = std::path::Path::new(f);
                    let meta = crate::api::issue_attachment_upload(&token, &o, &n, p)?;
                    let fname = meta["file_name"].as_str().unwrap_or("attachment");
                    // 链接形如 https://gitcode.com/user-attachments/files/{project_id}/{file}
                    // 注意：GitCode 可能把存储名改成内容哈希，若链接打不开请改用返回的元信息。
                    let link = format!(
                        "\n\n<a href=\"https://gitcode.com/user-attachments/files/{}/{}\" target=\"_blank\">{}</a>",
                        project_id, fname, fname
                    );
                    description.push_str(&link);
                    println!("已上传附件：{}（响应：{}）", fname, meta);
                }
                let v = crate::api::issue_create(&token, &o, &n, &title, &description, confidential, &label)?;
                let iid = v["iid"].as_u64().unwrap_or(0);
                let url = v["web_url"].as_str().unwrap_or("");
                println!("已创建 Issue: #{} {}", iid, url);
                if confidential {
                    println!("（该 Issue 为私有）");
                }
            }
            IssueAction::Close { repo, issues } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                for iid in &issues {
                    let v = crate::api::issue_close(&token, &o, &n, *iid)?;
                    let state = v["state"].as_str().unwrap_or("?");
                    let url = v["web_url"].as_str().unwrap_or("");
                    println!("已关闭 Issue #{}（状态：{}） {}", iid, state, url);
                }
            }
            IssueAction::Reopen { repo, issues } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                for iid in &issues {
                    let v = crate::api::issue_reopen(&token, &o, &n, *iid)?;
                    let state = v["state"].as_str().unwrap_or("?");
                    let url = v["web_url"].as_str().unwrap_or("");
                    println!("已重新打开 Issue #{}（状态：{}） {}", iid, state, url);
                }
            }
            IssueAction::Reply { repo, issue, body } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let body = if body.is_empty() {
                    edit_text("")?
                } else {
                    body
                };
                let v = crate::api::issue_reply(&token, &o, &n, issue, &body)?;
                let discussion_id = v["id"].as_str().unwrap_or("");
                let note_id = v["notes"]
                    .get(0)
                    .and_then(|n0| n0["id"].as_u64())
                    .unwrap_or(0);
                println!("已回复 Issue #{}（讨论：{} 评论 id：{}）", issue, discussion_id, note_id);
            }
            IssueAction::Edit { repo, issue, title, body, file } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let description = match (body, file) {
                    (Some(b), _) => Some(b),
                    (None, Some(f)) => Some(
                        std::fs::read_to_string(&f)
                            .map_err(|e| anyhow!("读取文件 {} 失败: {}", f, e))?,
                    ),
                    (None, None) => {
                        // 未给 --body / --file：打开编辑器，初始内容为当前描述
                        let cur = crate::api::issue_view(&token, &o, &n, issue)?;
                        let initial = cur["description"].as_str().unwrap_or("").to_string();
                        Some(edit_text(&initial)?)
                    }
                };
                let v = crate::api::issue_edit(
                    &token,
                    &o,
                    &n,
                    issue,
                    title.as_deref(),
                    description.as_deref(),
                )?;
                println!("已编辑 Issue #{}：{}", v["iid"].as_u64().unwrap_or(issue), v["web_url"].as_str().unwrap_or(""));
            }
        },

        Command::Pr(c) => match c.action {
            PrAction::List { repo, repo_pos, state, web, order_by, sort, page, per_page, count, json, jq } => {
                let (o, n) = resolve_repo(repo.or(repo_pos))?;
                if web || count {
                    let token = web_access_token()?;
                    if count {
                        // 网页端统计接口一次返回全部状态：{"all":n,"opened":n,"closed":n,"merged":n,"locked":n}
                        let v = crate::api::pr_list_count(&token, &o, &n, "all")?;
                        if !print_structured(&v, json, &jq)? {
                        let get = |k: &str| v[k].as_u64().unwrap_or(0);
                        println!("all: {}", get("all"));
                        println!("opened: {}", get("opened"));
                        println!("merged: {}", get("merged"));
                        println!("closed: {}", get("closed"));
                        if v.get("locked").is_some() {
                            println!("locked: {}", get("locked"));
                        }
                        }
                    } else {
                        let order = match order_by.as_str() {
                            "merged_at" | "created_at" | "updated_at" => order_by.clone(),
                            other => anyhow::bail!("order_by 只支持 updated_at / merged_at / created_at，收到：{}", other),
                        };
                        let v = crate::api::pr_list_web(&token, &o, &n, &state, &order, &sort, page, per_page)?;
                        if print_structured(&v, json, &jq)? {
                            return Ok(());
                        }
                        // 网页端分页包裹：{"page_num":..,"total":n,"content":[...]}
                        let total = v["total"].as_u64().unwrap_or(0);
                        let items = v.as_array().cloned().unwrap_or_else(|| {
                            v["content"]
                                .as_array()
                                .or_else(|| v["list"].as_array())
                                .or_else(|| v["data"].as_array())
                                .cloned()
                                .unwrap_or_default()
                        });
                        if total > 0 || !items.is_empty() {
                            println!("共 {} 条（第 {} 页，每页 {}）", total, page, per_page);
                        }
                        print_list(&Value::Array(items), |it| {
                            format!(
                                "#{} [{}] {}",
                                it["iid"].as_u64().or_else(|| it["number"].as_u64()).unwrap_or(0),
                                it["state"].as_str().unwrap_or("?"),
                                it["title"].as_str().unwrap_or("?")
                            )
                        });
                    }
                    return Ok(());
                }
                let v = api.pulls(&o, &n, &state)?;
                if print_structured(&v, json, &jq)? {
                    return Ok(());
                }
                print_list(&v, |it| {
                    format!(
                        "#{} [{}] {}",
                        it["number"].as_u64().unwrap_or(0),
                        it["state"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
            PrAction::Status { repo } => {
                let (o, n) = resolve_repo(repo)?;
                let me = api.authenticated_user()?;
                let login = me["login"].as_str().unwrap_or("").to_string();
                let open = api
                    .pulls_query(&o, &n, &[("state", "open")])?
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut any = false;
                let mut section = |label: &str, prs: Vec<Value>| -> Result<()> {
                    if prs.is_empty() {
                        return Ok(());
                    }
                    any = true;
                    println!("{}:", label);
                    for pr in &prs {
                        let num = pr["number"].as_i64().unwrap_or(0);
                        let title = pr["title"].as_str().unwrap_or("?");
                        let head = pr["head"]["ref"]
                            .as_str()
                            .or_else(|| pr["source_branch"].as_str())
                            .unwrap_or("?");
                        let base = pr["base"]["ref"]
                            .as_str()
                            .or_else(|| pr["target_branch"].as_str())
                            .unwrap_or("?");
                        let author = pr["user"]["login"].as_str().unwrap_or("?");
                        let draft = if pr["draft"].as_bool().unwrap_or(false) {
                            " [草稿]"
                        } else {
                            ""
                        };
                        println!("  #{} {}{} ({} → {}, by {})", num, title, draft, head, base, author);
                    }
                    println!();
                    Ok(())
                };
                if let Some(branch) = current_git_branch() {
                    let cur: Vec<Value> = open
                        .iter()
                        .filter(|p| {
                            p["head"]["ref"]
                                .as_str()
                                .or_else(|| p["source_branch"].as_str())
                                .unwrap_or("")
                                == branch
                        })
                        .cloned()
                        .collect();
                    section(&format!("当前分支 ({}) 的 PR", branch), cur)?;
                }
                section(
                    "我创建的 PR",
                    api.pulls_query(&o, &n, &[("state", "open"), ("author", &login)])?
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                )?;
                section(
                    "指派给我的 PR",
                    api.pulls_query(&o, &n, &[("state", "open"), ("assignee", &login)])?
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                )?;
                section(
                    "待我评审的 PR",
                    api.pulls_query(&o, &n, &[("state", "open"), ("reviewer", &login)])?
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                )?;
                if !any {
                    println!("没有找到相关的 PR。");
                }
            }
            PrAction::Checkout { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let pr = api.pull(&o, &n, number)?;
                let head = pr["head"]["ref"]
                    .as_str()
                    .or_else(|| pr["source_branch"].as_str())
                    .unwrap_or("")
                    .to_string();
                if head.is_empty() {
                    return Err(anyhow!("无法确定 PR #{} 的源分支", number));
                }
                let head_repo = pr["head"]["repo"]["full_path"].as_str().unwrap_or("");
                let same_repo = head_repo.is_empty() || head_repo == &format!("{}/{}", o, n);
                let source: String = if same_repo {
                    "origin".to_string()
                } else {
                    format!("https://gitcode.com/{}.git", head_repo)
                };
                git_run(&["fetch", &source, &head])?;
                let branch = format!("pr/{}/{}", number, head);
                git_run(&["checkout", "-B", &branch, "FETCH_HEAD"])?;
                println!(
                    "已检出 PR #{} → 本地分支 {}",
                    number,
                    branch
                );
            }
            PrAction::Checks { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let pr = api.pull(&o, &n, number)?;
                println!(
                    "#{} {}",
                    pr["number"].as_i64().unwrap_or(number as i64),
                    pr["title"].as_str().unwrap_or("")
                );
                let state = pr["state"].as_str().unwrap_or("?");
                let draft = pr["draft"].as_bool().unwrap_or(false);
                println!(
                    "状态: {}",
                    if draft {
                        format!("{} (草稿)", state)
                    } else {
                        state.to_string()
                    }
                );
                let ms = pr["merge_status"].as_str().unwrap_or("");
                if !ms.is_empty() && ms != "unchecked" {
                    println!("合并状态: {}", ms);
                }
                if let Some(err) = pr["merge_error"].as_str() {
                    if !err.is_empty() {
                        println!("合并错误: {}", err);
                    }
                }
                let combined = pr["pipeline_status_with_code_quality"].as_str().unwrap_or("");
                let pipeline = pr["pipeline_status"].as_str().unwrap_or("");
                let cq = pr["codequality_status"].as_str().unwrap_or("");
                if !combined.is_empty() {
                    println!("CI 状态: {}", combined);
                } else {
                    if !pipeline.is_empty() {
                        println!("流水线: {}", pipeline);
                    }
                    if !cq.is_empty() {
                        println!("代码质量: {}", cq);
                    }
                }
                for (label, key) in [
                    ("评审人", "approval_merge_request_reviewers"),
                    ("评审通过", "approval_merge_request_approvers"),
                    ("测试人", "approval_merge_request_testers"),
                ] {
                    if let Some(arr) = pr.get(key).and_then(|v| v.as_array()) {
                        if !arr.is_empty() {
                            let parts: Vec<String> = arr
                                .iter()
                                .map(|r| {
                                    let name = r["username"]
                                        .as_str()
                                        .or_else(|| r["login"].as_str())
                                        .unwrap_or("?");
                                    let st = r["state"].as_str().unwrap_or("");
                                    if st.is_empty() {
                                        name.to_string()
                                    } else {
                                        format!("{} ({})", name, st)
                                    }
                                })
                                .collect();
                            println!("{}: {}", label, parts.join(", "));
                        }
                    }
                }
                if let Some(gate) = pr["gate_check"].as_bool() {
                    println!("门禁检查: {}", if gate { "通过" } else { "未通过/未开启" });
                }
                if let Some(url) = pr["html_url"].as_str().or_else(|| pr["web_url"].as_str()) {
                    println!("链接: {}", url);
                }
            }
            PrAction::Diff { repo, number, name_only } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull_files(&o, &n, number)?;
                let files = v.as_array().cloned().unwrap_or_default();
                let mut added = 0i64;
                let mut removed = 0i64;
                for f in &files {
                    let filename = f["filename"].as_str().unwrap_or("?");
                    let status = f["status"].as_str().unwrap_or("");
                    let a = f["additions"].as_i64().unwrap_or(0);
                    let d = f["deletions"].as_i64().unwrap_or(0);
                    added += a;
                    removed += d;
                    if name_only {
                        let icon = match status {
                            "added" => "A",
                            "deleted" => "D",
                            "modified" => "M",
                            "renamed" => "R",
                            _ => "?",
                        };
                        println!("{} {}", icon, filename);
                        continue;
                    }
                    println!("diff --git a/{} b/{}", filename, filename);
                    println!("status: {} | +{} -{}", status, a, d);
                    match f["patch"]["diff"].as_str().or_else(|| f["diff"].as_str()) {
                        Some(txt) => println!("{}", txt),
                        None => {
                            if f["too_large"].as_bool().unwrap_or(false) {
                                println!("(文件过大，无法显示 diff)");
                            }
                        }
                    }
                    println!();
                }
                if !name_only {
                    println!("变更文件 ({}):  +{} -{}", files.len(), added, removed);
                    if let Ok(pr) = api.pull(&o, &n, number) {
                        if let Some(u) = pr["diff_url"].as_str() {
                            println!("在线查看: {}", u);
                        }
                    }
                }
            }
            PrAction::Lock { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let b = json!({ "state": "locked" });
                api.update_pull(&o, &n, number, &b)?;
                println!("已锁定 PR #{}", number);
            }
            PrAction::Ready { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let b = json!({ "draft": false });
                api.update_pull(&o, &n, number, &b)?;
                println!("PR #{} 已标记为准备评审", number);
            }
            PrAction::Unlock { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let b = json!({ "state": "open" });
                api.update_pull(&o, &n, number, &b)?;
                println!("已解锁 PR #{}", number);
            }
            PrAction::View { repo, number, json, jq } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull(&o, &n, number)?;
                if !print_structured(&v, json, &jq)? {
                    println!(
                        "#{} [{}] {}",
                        number,
                        v["state"].as_str().unwrap_or("?"),
                        v["title"].as_str().unwrap_or("(无标题)")
                    );
                    if let Some(a) = v["user"]["login"].as_str() {
                        println!("作者: {}", a);
                    }
                    if let Some(url) = v["html_url"].as_str() {
                        println!("链接: {}", url);
                    }
                    println!();
                    render_md(
                        v["body"].as_str().or_else(|| v["description"].as_str()).unwrap_or(""),
                    );
                }
            }
            PrAction::Create { repo, title, body, file, head, base, target_repo, patch } => {
                let (o, n) = resolve_repo(repo)?;
                let body = match &file {
                    Some(f) => std::fs::read_to_string(f)
                        .with_context(|| format!("读取正文文件 {} 失败", f))?,
                    None => body,
                };
                // 标题与正文在同一个编辑器里一次填完。
                let (title, body) = if title.is_some() && !body.is_empty() {
                    (title.unwrap(), body)
                } else {
                    let it = title.clone().unwrap_or_default();
                    edit_title_body(&it, &body)?
                };
                if title.trim().is_empty() {
                    anyhow::bail!("标题不能为空（请在编辑器首行填写，或用 --title 指定）");
                }
                // 用网页端 v2 对比接口预览 diff（compare = pr create 的核心）
                let token = web_access_token()?;
                let (to, tnn) = match &target_repo {
                    Some(t) => split_repo(t)?,
                    None => (o.clone(), n.clone()),
                };
                let cmp = crate::api::repo_compare(&token, &o, &n, &to, &tnn, &head, &base)?;
                print_compare(&cmp, patch);
                // 创建 PR（v5 开放 API）
                let v = api.create_pull(&o, &n, &title, &body, &head, &base)?;
                println!("已创建 PR: #{} {}", v["number"].as_u64().unwrap_or(0), v["html_url"].as_str().unwrap_or(""));
            }

            // ---- PR 生命周期（移植自 @oh-gc/cli，MIT, Copyright (c) guozejun）----
            PrAction::Merge { repo, number, method, title, description } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.merge_pull(&o, &n, number, &method, &title, &description)?;
                println!(
                    "已合并 PR #{}：{}",
                    number,
                    v["message"].as_str().unwrap_or("ok")
                );
                if let Some(sha) = v["sha"].as_str() {
                    println!("合并提交: {}", sha);
                }
            }
            PrAction::Close { repo, numbers } => {
                let (o, n) = resolve_repo(repo)?;
                for num in numbers {
                    api.close_pull(&o, &n, num)?;
                    println!("已关闭 PR #{}", num);
                }
            }
            PrAction::Reopen { repo, numbers } => {
                let (o, n) = resolve_repo(repo)?;
                for num in numbers {
                    api.reopen_pull(&o, &n, num)?;
                    println!("已重新打开 PR #{}", num);
                }
            }
            PrAction::Edit { repo, number, title, body, draft } => {
                let (o, n) = resolve_repo(repo)?;
                let mut b = serde_json::Map::new();
                if let Some(t) = &title {
                    b.insert("title".into(), Value::String(t.clone()));
                }
                if let Some(x) = &body {
                    b.insert("body".into(), Value::String(x.clone()));
                }
                if let Some(d) = draft {
                    b.insert("draft".into(), Value::Bool(d));
                }
                if b.is_empty() {
                    anyhow::bail!("至少要指定 --title / --body / --draft 之一");
                }
                let v = api.update_pull(&o, &n, number, &Value::Object(b))?;
                println!("已更新 PR #{}：{}", number, v["title"].as_str().unwrap_or(""));
            }
            PrAction::Files { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull_files(&o, &n, number)?;
                print_list(&v, |it| {
                    format!(
                        "{} [{}] +{} -{}",
                        it["filename"].as_str().unwrap_or("?"),
                        it["status"].as_str().unwrap_or("?"),
                        it["additions"].as_u64().unwrap_or(0),
                        it["deletions"].as_u64().unwrap_or(0)
                    )
                });
            }
            PrAction::Commits { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull_commits(&o, &n, number, 0, 0)?;
                print_list(&v, |it| {
                    let sha = it["sha"].as_str().unwrap_or("");
                    let msg = it["commit"]["message"].as_str().unwrap_or("");
                    format!("{} {}", &sha[..sha.len().min(8)], msg.lines().next().unwrap_or(""))
                });
            }
            PrAction::Comment(c) => match c.action {
                PrCommentAction::List { repo, number } => {
                    let (o, n) = resolve_repo(repo)?;
                    let v = api.pull_comments(&o, &n, number, 0, 0)?;
                    print_list(&v, |it| {
                        format!(
                            "#{} @{}: {}",
                            it["id"].as_u64().unwrap_or(0),
                            it["user"]["login"].as_str().unwrap_or("?"),
                            it["body"].as_str().unwrap_or("").lines().next().unwrap_or("")
                        )
                    });
                }
                PrCommentAction::Add { repo, number, body, file, path, position, web } => {
                    let (o, n) = resolve_repo(repo)?;
                    let body = match &file {
                        Some(f) => std::fs::read_to_string(f)
                            .with_context(|| format!("读取评论文件 {} 失败", f))?,
                        None => body,
                    };
                    let v = if web {
                        if path.is_some() || position.is_some() {
                            anyhow::bail!("--web 的普通评论接口不支持行级评论，请去掉 --path/--position 或不使用 --web");
                        }
                        let token = web_access_token()?;
                        crate::api::pr_note_create(&token, &o, &n, number, &body)?
                    } else {
                        api.create_pull_comment(
                            &o,
                            &n,
                            number,
                            &body,
                            path.as_deref(),
                            position,
                        )?
                    };
                    println!("已评论 PR #{}（评论 id {}）", number, v["id"].as_u64().unwrap_or(0));
                }
                PrCommentAction::Reply { repo, number, discussion_id, body } => {
                    let (o, n) = resolve_repo(repo)?;
                    let v = api.reply_pull_discussion(&o, &n, number, &discussion_id, &body)?;
                    println!("已回复讨论 {}（评论 id {}）", discussion_id, v["id"].as_u64().unwrap_or(0));
                }
                PrCommentAction::Edit { repo, comment_id, body } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.edit_pull_comment(&o, &n, comment_id, &body)?;
                    println!("已编辑评论 {}", comment_id);
                }
                PrCommentAction::Delete { repo, comment_id, yes } => {
                    if !yes {
                        anyhow::bail!("删除评论不可撤销，确认请加 --yes");
                    }
                    let (o, n) = resolve_repo(repo)?;
                    api.delete_pull_comment(&o, &n, comment_id)?;
                    println!("已删除评论 {}", comment_id);
                }
            },
            PrAction::Reviewer(c) => match c.action {
                PrUserAction::Add { repo, number, users, append } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.assign_pull_reviewers(&o, &n, number, &users, append)?;
                    println!("已{}评审人：{}", if append { "追加" } else { "设置" }, users);
                }
                PrUserAction::Remove { repo, number, users } => {
                    if users.trim().is_empty() {
                        anyhow::bail!("移除评审人必须指定用户名（逗号分隔）");
                    }
                    let (o, n) = resolve_repo(repo)?;
                    api.remove_pull_reviewers(&o, &n, number, &users)?;
                    println!("已移除评审人：{}", users);
                }
            },
            PrAction::Tester(c) => match c.action {
                PrUserAction::Add { repo, number, users, append } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.assign_pull_testers(&o, &n, number, &users, append)?;
                    println!("已{}测试人：{}", if append { "追加" } else { "设置" }, users);
                }
                PrUserAction::Remove { repo, number, users } => {
                    if users.trim().is_empty() {
                        anyhow::bail!("移除测试人必须指定用户名（逗号分隔）");
                    }
                    let (o, n) = resolve_repo(repo)?;
                    api.remove_pull_testers(&o, &n, number, &users)?;
                    println!("已移除测试人：{}", users);
                }
            },
            PrAction::Assignee(c) => match c.action {
                PrUserAction::Add { repo, number, users, append: _ } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.assign_pull_assignees(&o, &n, number, &users)?;
                    println!("已指派：{}", users);
                }
                PrUserAction::Remove { repo, number, users } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.remove_pull_assignees(&o, &n, number, &users)?;
                    if users.trim().is_empty() {
                        println!("已移除全部指派人");
                    } else {
                        println!("已移除指派人：{}", users);
                    }
                }
            },
            PrAction::Review { repo, number, force } => {
                let (o, n) = resolve_repo(repo)?;
                api.review_pull(&o, &n, number, force)?;
                println!("已对 PR #{} 提交评审通过", number);
            }
            PrAction::Test { repo, number, force } => {
                let (o, n) = resolve_repo(repo)?;
                api.test_pull(&o, &n, number, force)?;
                println!("已对 PR #{} 提交测试通过", number);
            }
            PrAction::CherryPick { repo, number, branch, with_new_merge_request } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let branch = match branch {
                    Some(b) => b,
                    None => {
                        // 未指定目标分支时，用 v5 查询目标仓库默认分支
                        let r = api.repo(&o, &n)?;
                        r["default_branch"].as_str().unwrap_or("main").to_string()
                    }
                };
                let v = crate::api::pr_cherry_pick(&token, &o, &n, number, &branch, with_new_merge_request)?;
                if let Some(mr) = v.as_object() {
                    let iid = mr.get("iid").and_then(|x| x.as_u64()).unwrap_or(0);
                    let url = mr.get("web_url").and_then(|x| x.as_str()).unwrap_or("");
                    if iid > 0 {
                        println!("已 cherry-pick PR #{} 到 {}，新建 MR: #{} {}", number, branch, iid, url);
                    } else {
                        println!("已 cherry-pick PR #{} 到 {}：{}", number, branch, v);
                    }
                } else {
                    println!("已 cherry-pick PR #{}：{}", number, v);
                }
            }
            PrAction::Revert { repo, number, branch, with_new_merge_request } => {
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                let branch = match branch {
                    Some(b) => b,
                    None => {
                        let r = api.repo(&o, &n)?;
                        r["default_branch"].as_str().unwrap_or("main").to_string()
                    }
                };
                let v = crate::api::pr_revert(&token, &o, &n, number, &branch, with_new_merge_request)?;
                if let Some(mr) = v.as_object() {
                    let iid = mr.get("iid").and_then(|x| x.as_u64()).unwrap_or(0);
                    let url = mr.get("web_url").and_then(|x| x.as_str()).unwrap_or("");
                    if iid > 0 {
                        println!("已 revert PR #{} 到 {}，新建 MR: #{} {}", number, branch, iid, url);
                    } else {
                        println!("已 revert PR #{} 到 {}：{}", number, branch, v);
                    }
                } else {
                    println!("已 revert PR #{}：{}", number, v);
                }
            }
            PrAction::Approval { repo, number, action, force } => {
                let action = action.to_lowercase();
                if action != "approve" && action != "reset" {
                    anyhow::bail!("action 只支持 approve（通过）或 reset（撤销）");
                }
                let (o, n) = resolve_repo(repo)?;
                let token = web_access_token()?;
                crate::api::pr_approval(&token, &o, &n, number, &action, force)?;
                println!("已{} PR #{} 的审批", if action == "approve" { "通过" } else { "撤销" }, number);
            }
            PrAction::Label(c) => match c.action {
                PrLabelAction::Add { repo, number, labels } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.add_pull_labels(&o, &n, number, &labels)?;
                    println!("已追加标签：{}", labels.join(", "));
                }
                PrLabelAction::Set { repo, number, labels } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.replace_pull_labels(&o, &n, number, &labels)?;
                    println!("标签已设为：{}", labels.join(", "));
                }
                PrLabelAction::Remove { repo, number, labels } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.remove_pull_labels(&o, &n, number, &labels.join(","))?;
                    println!("已移除标签：{}", labels.join(", "));
                }
            },
            PrAction::Link(c) => match c.action {
                PrLinkAction::List { repo, number } => {
                    let (o, n) = resolve_repo(repo)?;
                    let v = api.pull_linked_issues(&o, &n, number, 0, 0)?;
                    print_list(&v, |it| {
                        format!(
                            "#{} {}",
                            it["number"].as_str().map(str::to_string).unwrap_or_else(|| it["number"]
                                .as_u64()
                                .unwrap_or(0)
                                .to_string()),
                            it["title"].as_str().unwrap_or("?")
                        )
                    });
                }
                PrLinkAction::Add { repo, number, issues } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.link_pull_issues(&o, &n, number, &issues)?;
                    println!("已关联 Issue：{:?}", issues);
                }
                PrLinkAction::Remove { repo, number, issues } => {
                    let (o, n) = resolve_repo(repo)?;
                    api.unlink_pull_issues(&o, &n, number, &issues)?;
                    println!("已取消关联 Issue：{:?}", issues);
                }
            },
            PrAction::History { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull_modify_history(&o, &n, number)?;
                println!("{}", pretty(&v));
            }
            PrAction::Logs { repo, number } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.pull_operate_logs(&o, &n, number, 0, 0)?;
                print_list(&v, |it| {
                    format!(
                        "[{}] @{} {}",
                        it["created_at"].as_str().unwrap_or(""),
                        it["user"]["login"].as_str().unwrap_or("?"),
                        it["content"].as_str().unwrap_or(it["action"].as_str().unwrap_or(""))
                    )
                });
            }
        },

        // ---- 分支 / 提交 / 文件（移植自 @oh-gc/cli，MIT, Copyright (c) guozejun）----
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
                println!("已创建分支 {}（起点 {}）", v["name"].as_str().unwrap_or(&branch), from);
            }
            BranchAction::Delete { repo, branch, yes } => {
                if !yes {
                    anyhow::bail!("删除分支不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                api.delete_branch(&o, &n, &branch)?;
                println!("已删除分支 {}", branch);
            }
            BranchAction::Protect { repo, branch, pusher, merger } => {
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
                BranchRuleAction::Create { repo, wildcard, pusher, merger } => {
                    let (o, n) = split_repo(&repo)?;
                    api.create_branch_protect_rule(&o, &n, &wildcard, &pusher, &merger)?;
                    println!("已创建保护规则 {}", wildcard);
                }
                BranchRuleAction::Update { repo, wildcard, pusher, merger } => {
                    if pusher.is_empty() && merger.is_empty() {
                        anyhow::bail!("至少要指定 --pusher 或 --merger");
                    }
                    let (o, n) = split_repo(&repo)?;
                    api.update_branch_protect_rule(&o, &n, &wildcard, &pusher, &merger)?;
                    println!("已更新保护规则 {}", wildcard);
                }
                BranchRuleAction::Delete { repo, wildcard, yes } => {
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
            CommitAction::List { repo, sha, path, author, per_page } => {
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
                        it["commit"]["message"].as_str().unwrap_or("").lines().next().unwrap_or(""),
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
                            it["body"].as_str().unwrap_or("").lines().next().unwrap_or("")
                        )
                    });
                }
                CommitCommentAction::Add { repo, sha, body } => {
                    let (o, n) = split_repo(&repo)?;
                    let v = api.create_commit_comment(&o, &n, &sha, &body)?;
                    println!("已评论提交 {}（评论 id {}）", sha, v["id"].as_u64().unwrap_or(0));
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
            FileAction::Get { repo, path, branch, output } => {
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
            FileAction::Create { repo, path, from, message, branch } => {
                let (o, n) = split_repo(&repo)?;
                let content = read_as_base64(&from)?;
                let v = api.create_file(&o, &n, &path, &message, &content, &branch)?;
                println!("已创建 {}（提交 {}）", path, v["commit"]["sha"].as_str().unwrap_or(""));
            }
            FileAction::Update { repo, path, from, message, branch } => {
                let (o, n) = split_repo(&repo)?;
                // 覆盖写要带当前 blob sha 做乐观锁，先查一次。
                let cur = api.contents(&o, &n, &path, &branch)?;
                let sha = cur["sha"]
                    .as_str()
                    .ok_or_else(|| anyhow!("拿不到 {} 的当前 sha —— 该路径是目录还是不存在？", path))?;
                let content = read_as_base64(&from)?;
                let v = api.update_file(&o, &n, &path, &message, &content, sha, &branch)?;
                println!("已更新 {}（提交 {}）", path, v["commit"]["sha"].as_str().unwrap_or(""));
            }
            FileAction::Delete { repo, path, message, branch, yes } => {
                if !yes {
                    anyhow::bail!("删除文件不可撤销，确认请加 --yes");
                }
                let (o, n) = split_repo(&repo)?;
                let cur = api.contents(&o, &n, &path, &branch)?;
                let sha = cur["sha"]
                    .as_str()
                    .ok_or_else(|| anyhow!("拿不到 {} 的当前 sha —— 该路径是目录还是不存在？", path))?;
                api.delete_file(&o, &n, &path, &message, sha, &branch)?;
                println!("已删除 {}", path);
            }
        },

        Command::Tag(c) => match c.action {
            TagAction::List { repo } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.list_tags(&o, &n, 0, 0)?;
                print_list(&v, |it| {
                    format!("{} -> {}", it["name"].as_str().unwrap_or("?"), it["commit"]["sha"].as_str().unwrap_or(""))
                });
            }
            TagAction::View { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_tag(&o, &n, &tag)?;
                println!("{}", pretty(&v));
            }
            TagAction::Create { repo, tag, refs, message } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_tag(&o, &n, &refs, &tag, &message)?;
                println!("已创建标签 {}: {}", v["name"].as_str().unwrap_or(&tag), v["commit"]["url"].as_str().unwrap_or(""));
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
                    format!("{} (级别 {})", it["name"].as_str().unwrap_or("?"), it["create_access_level"].as_i64().unwrap_or(0))
                });
            }
            TagAction::RuleView { repo, tag } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_protected_tag(&o, &n, &tag)?;
                println!("{}", pretty(&v));
            }
            TagAction::RuleCreate { repo, tag, access_level } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.create_protected_tag(&o, &n, &tag, access_level)?;
                println!("已创建受保护标签规则 {}", v["name"].as_str().unwrap_or(&tag));
            }
            TagAction::RuleUpdate { repo, tag, access_level } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.update_protected_tag(&o, &n, &tag, access_level)?;
                println!("已更新受保护标签规则 {}", v["name"].as_str().unwrap_or(&tag));
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
                    format!("#{} {} ({}) {}", it["id"].as_i64().unwrap_or(0), it["name"].as_str().unwrap_or("?"), it["type"].as_str().unwrap_or(""), it["config"]["url"].as_str().unwrap_or(""))
                });
            }
            HookAction::View { repo, id } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_hook(&o, &n, id)?;
                println!("{}", pretty(&v));
            }
            HookAction::Create { repo, name, url, events, content_type, secret, active } => {
                let (o, n) = split_repo(&repo)?;
                let evs: Vec<&str> = events.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
                let mut config = json!({ "url": url, "content_type": content_type });
                if !secret.is_empty() {
                    config["secret"] = json!(secret);
                }
                let body = json!({ "name": name, "config": config, "events": evs, "active": active });
                let v = api.create_hook(&o, &n, &body)?;
                println!("已创建 Webhook #{} -> {}", v["id"].as_i64().unwrap_or(0), v["config"]["url"].as_str().unwrap_or(&url));
            }
            HookAction::Update { repo, id, name, url, events, content_type, secret, active } => {
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
                    let evs: Vec<&str> = events.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
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
                    format!("{} (#{}) — {}", it["name"].as_str().unwrap_or("?"), it["color"].as_str().unwrap_or(""), it["description"].as_str().unwrap_or(""))
                });
            }
            LabelAction::View { repo, name } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_label(&o, &n, &name)?;
                println!("{}", pretty(&v));
            }
            LabelAction::Create { repo, name, color, description } => {
                let (o, n) = split_repo(&repo)?;
                let body = json!({ "name": name, "color": color, "description": description });
                let v = api.create_label(&o, &n, &body)?;
                println!("已创建标签 {}", v["name"].as_str().unwrap_or(&name));
            }
            LabelAction::Update { repo, name, new_name, color, description } => {
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
                    format!("#{} {} [{}] — {}", it["number"].as_i64().unwrap_or(0), it["title"].as_str().unwrap_or("?"), it["state"].as_str().unwrap_or(""), it["description"].as_str().unwrap_or(""))
                });
            }
            MilestoneAction::View { repo, number } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.get_milestone(&o, &n, number)?;
                println!("{}", pretty(&v));
            }
            MilestoneAction::Create { repo, title, description, state, due_on } => {
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
                println!("已创建里程碑 #{} {}", v["number"].as_i64().unwrap_or(0), v["title"].as_str().unwrap_or(&title));
            }
            MilestoneAction::Update { repo, number, title, description, state, due_on } => {
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
                println!("已更新里程碑 #{}", v["number"].as_i64().unwrap_or(number as i64));
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
                    format!("{} ({})", it["login"].as_str().unwrap_or("?"), it["type"].as_str().unwrap_or(""))
                });
            }
            CollaboratorAction::Add { repo, username, permission } => {
                let (o, n) = split_repo(&repo)?;
                let v = api.add_collaborator(&o, &n, &username, &permission)?;
                println!("已添加协作者 {}（权限 {}）", v["login"].as_str().unwrap_or(&username), permission);
            }
            CollaboratorAction::Remove { repo, username, yes } => {
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
                    format!("{} ({}) — {}", it["login"].as_str().unwrap_or("?"), it["name"].as_str().unwrap_or(""), it["description"].as_str().unwrap_or(""))
                });
            }
            OrgAction::View { org } => {
                let v = api.get_org(&org)?;
                println!("{}", pretty(&v));
            }
            OrgAction::Members { org, role } => {
                let v = api.list_org_members(&org, &role, 0, 0)?;
                print_list(&v, |it| {
                    format!("{} ({})", it["login"].as_str().unwrap_or("?"), it["type"].as_str().unwrap_or(""))
                });
            }
            OrgAction::Repos { org, r#type } => {
                let v = api.list_org_repos(&org, &r#type, 0, 0)?;
                print_list(&v, |it| {
                    format!("{} {} — {}", it["full_name"].as_str().unwrap_or("?"), if it["private"].as_bool().unwrap_or(false) { "[私有]" } else { "" }, it["description"].as_str().unwrap_or(""))
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
            ReleaseAction::Create { repo, tag, name, body, target } => {
                let (o, n) = split_repo(&repo)?;
                // 名称与正文在同一个编辑器里一次填完；名称留空则回退到 tag。
                let (rname, body) = if name.is_some() && !body.is_empty() {
                    (name.unwrap(), body)
                } else {
                    let iname = name.clone().unwrap_or_default();
                    edit_title_body(&iname, &body)?
                };
                let rname = if rname.trim().is_empty() { tag.clone() } else { rname };
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
            println!("网页端 token 已刷新并保存到 {}", config::config_path()?.display());
        }

        Command::Install { system } => {
            run_install(system)?;
        }
        Command::Skill(c) => match c.action {
            SkillAction::Install { tools, all, force } => {
                run_skill_install(&tools, all, force)?;
            }
            SkillAction::List => {
                run_skill_list()?;
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
            let api = ApiClient::new(config::api_base(), if token.is_empty() { None } else { Some(token) })?;
            let v = api.raw(&a.method, &a.path, if a.body.is_empty() { None } else { Some(&a.body) })?;
            println!("{}", pretty(&v));
        }
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
            // ---- v5 接口（PAT），移植自 @oh-gc/cli（MIT, Copyright (c) guozejun）----
            UserAction::Followers { username } => {
                let v = api.user_followers(&username, 0, 0)?;
                print_list(&v, |it| format!("{} ({})", it["login"].as_str().unwrap_or("?"), it["name"].as_str().unwrap_or("")));
            }
            UserAction::Following { username } => {
                let v = api.user_following(&username, 0, 0)?;
                print_list(&v, |it| format!("{} ({})", it["login"].as_str().unwrap_or("?"), it["name"].as_str().unwrap_or("")));
            }
            UserAction::Starred { username } => {
                let v = api.user_starred(&username, "", "", 0, 0)?;
                print_list(&v, |it| format!("{}", it["full_name"].as_str().unwrap_or("?")));
            }
            UserAction::Subscriptions { username } => {
                let v = api.user_subscriptions(&username, "", "", 0, 0)?;
                print_list(&v, |it| format!("{}", it["full_name"].as_str().unwrap_or("?")));
            }
            UserAction::Namespaces { mode } => {
                let v = api.user_namespaces(&mode, 0, 0)?;
                print_list(&v, |it| format!("{} ({}) — {}", it["path"].as_str().unwrap_or("?"), it["type"].as_str().unwrap_or(""), it["name"].as_str().unwrap_or("")));
            }
            UserAction::Namespace { path } => {
                let v = api.user_namespace(&path)?;
                println!("{}", pretty(&v));
            }
            UserAction::Keys => {
                let v = api.user_keys(0, 0)?;
                print_list(&v, |it| format!("#{} {} — {}", it["id"].as_i64().unwrap_or(0), it["title"].as_str().unwrap_or(""), it["key"].as_str().unwrap_or("")));
            }
            UserAction::Key { id } => {
                let v = api.user_key(id)?;
                println!("{}", pretty(&v));
            }
            UserAction::KeyAdd { title, key } => {
                let v = api.add_user_key(&title, &key)?;
                println!("已添加 SSH 公钥 #{}: {}", v["id"].as_i64().unwrap_or(0), v["title"].as_str().unwrap_or(&title));
            }
            UserAction::KeyDelete { id, yes } => {
                if !yes {
                    anyhow::bail!("删除 SSH 公钥不可撤销，确认请加 --yes");
                }
                api.delete_user_key(id)?;
                println!("已删除 SSH 公钥 #{}", id);
            }
            UserAction::Issues { filter, state, labels, sort, direction, since } => {
                let v = api.user_issues(&filter, &state, &labels, &sort, &direction, &since, 0, 0)?;
                print_list(&v, |it| format!("#{} {} [{}]", it["number"].as_str().unwrap_or("?"), it["title"].as_str().unwrap_or(""), it["state"].as_str().unwrap_or("")));
            }
            UserAction::Prs { state, scope, sort, direction, labels } => {
                let v = api.user_prs(&state, &scope, &sort, &direction, &labels, 0, 0)?;
                print_list(&v, |it| format!("#{} {} (仓库 #{})", it["number"].as_i64().unwrap_or(0), it["title"].as_str().unwrap_or(""), it["repository_id"].as_i64().unwrap_or(0)));
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
            UserAction::UpdateProfile { nickname, company, description, email, github_account, website, location } => {
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
                        if it["primary"].as_bool().unwrap_or(false) { "[主]" } else { "" },
                        it["state"].as_str().unwrap_or(""),
                        if it["verified"].as_bool().unwrap_or(false) { " 已验证" } else { "" },
                    )
                });
            }
            UserAction::Orgs { username } => {
                let v = api.user_orgs_by_username(&username, 0, 0)?;
                print_list(&v, |it| format!("{} ({}) — {}", it["login"].as_str().unwrap_or("?"), it["name"].as_str().unwrap_or(""), it["description"].as_str().unwrap_or("")));
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
#[derive(Parser)]
struct SkillCmd {
    #[command(subcommand)]
    action: SkillAction,
}
#[derive(Subcommand)]
enum SkillAction {
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
fn skill_tools() -> Vec<(&'static str, &'static str, &'static str)> {
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
fn tool_standard_dir(name: &str) -> Option<std::path::PathBuf> {
    dirs::home_dir().and_then(|h| {
        skill_tools()
            .into_iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, sub, _)| h.join(sub))
    })
}

/// 检测某工具的根目录（opencode 额外探测 ~/.opencode）
fn detect_tool_dir(name: &str) -> Option<std::path::PathBuf> {
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
fn skill_md() -> String {
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
fn run_skill_install(tools: &[String], all: bool, force: bool) -> Result<()> {
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
        println!("共安装 {} 个工具，跳过 {} 个。", installed.len(), skipped.len());
        for (name, label, reason) in &skipped {
            println!("  ⏭ {}（{}）：{}", name, label, reason);
        }
    }
    Ok(())
}

/// `gc skill list`：展示检测状态与安装情况
fn run_skill_list() -> Result<()> {
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
                if up_to_date { "最新" } else { "旧版可更新" }
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
        std::fs::create_dir_all(&dest_dir).with_context(|| format!("创建 {} 失败", dest_dir.display()))?;
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
            std::fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
        };
        #[cfg(not(unix))]
        let is_root = std::env::var("USER").map(|u| u == "root").unwrap_or(false);
        if !is_root {
            println!("--system 需要 root，自动执行 sudo ...");
        }
        // sudo -n 优先尝试免密；失败回退交互式 sudo
        let status = std::process::Command::new("sudo")
            .args(["cp", src.to_string_lossy().as_ref(), dest.to_string_lossy().as_ref()])
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
        .map(|p| p.split(':').any(|d| std::path::Path::new(d).join("gc").exists() || d == dest_dir.to_string_lossy()))
        .unwrap_or(false);
    if !in_path {
        println!("注意：{} 不在 PATH 里，请把它加入 shell 配置（如 ~/.bashrc）", dest_dir.display());
    }
    Ok(())
}

/// `gc repo grep` 实现：列出仓库文件 → 按 --include 通配过滤 → rayon 并行下载到临时目录
/// → 用 grep crate（ripgrep 库版）做行级正则搜索 → 输出 file:line: text。
fn run_repo_grep(
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
    let files = crate::api::repo_file_list(&token, owner, name, branch)?;
    if files.is_empty() {
        anyhow::bail!("仓库 {}/{} 分支 {} 没有文件", owner, name, branch);
    }
    // --include 通配过滤（* 与 ?，大小写不敏感）
    let matched: Vec<&String> = files
        .iter()
        .filter(|f| {
            include.is_empty()
                || include
                    .iter()
                    .any(|pat| wildcard_match(f, pat))
        })
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
    let mut cached_shas: std::collections::HashMap<String, String> = std::fs::read_to_string(&sha_file)
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
        let results = crate::api::download_files(&token, owner, name, branch, &to_download, &files_dir)?;
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
    println!("（{} 个文件命中，缓存：{}）", files_hit, files_dir.display());
    Ok(())
}

/// 极简通配符匹配（支持 * 与 ?，大小写不敏感），用于 --include 过滤
fn wildcard_match(s: &str, pattern: &str) -> bool {
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
pub(crate) fn split_repo(s: &str) -> Result<(String, String)> {
    // 允许直接传入 URL（https://host/owner/repo 或 git@host:owner/repo）
    let s = parse_gitcode_repo(s).unwrap_or_else(|| s.to_string());
    let mut it = s.splitn(2, '/');
    let o = it.next().ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    let n = it.next().ok_or_else(|| anyhow::anyhow!("仓库格式应为 owner/name"))?;
    if o.is_empty() || n.is_empty() {
        anyhow::bail!("仓库格式应为 owner/name");
    }
    Ok((o.to_string(), n.to_string()))
}

/// gc 在当前仓库用哪个 remote 解析 owner/name。存在 git local config `gc.remote` 则用它，否则 origin。
///
/// 上游 @oh-gc/cli 是往仓库里写 `.gitcode` 文件记这个值；这里改用 git 自己的
/// local config，省掉一个需要进 .gitignore 的дот文件，也能直接 `git config gc.remote` 查改。
fn configured_remote() -> String {
    std::process::Command::new("git")
        .args(["config", "--get", "gc.remote"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "origin".to_string())
}

/// 从当前目录的 git remote 推断 owner/name（仅支持 gitcode.com）。
/// 在克隆仓库目录下直接 `gc repo sync` 不指定 <REPO> 时使用，与 gh 行为一致。
fn detect_repo_from_cwd() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", &configured_remote()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_gitcode_repo(&String::from_utf8_lossy(&out.stdout))
}

/// 当前所在 git 分支（非 git 仓库或游离 HEAD 时返回 None）
fn current_git_branch() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}

/// 运行 git 命令，失败时返回带 stderr 的错误
fn git_run(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| anyhow!("运行 git 失败: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!("git {} 失败: {}", args.join(" "), err));
    }
    Ok(())
}

/// 参数给了就用参数，否则从当前目录的 git remote 推断。
/// `pub(crate)` 以便 MCP 工具也能复用（与 CLI 行为一致）。
pub(crate) fn resolve_repo(repo: Option<String>) -> Result<(String, String)> {
    let r = repo.or_else(detect_repo_from_cwd).ok_or_else(|| {
        anyhow!(
            "当前目录不是 gitcode 仓库（无法从 git remote {} 推断），请显式指定 <REPO>",
            configured_remote()
        )
    })?;
    split_repo(&r)
}

fn parse_gitcode_repo(url: &str) -> Option<String> {
    let u = url.trim();
    // 支持 gitcode.com / atomgit.com 等任意主机的三种形式：
    // https://host/owner/repo(.git)、git@host:owner/repo(.git)、owner/repo
    let stripped = if let Some(p) = u.find("://") {
        // 去掉 scheme://host[:port]/，只留路径
        let rest = &u[p + 3..];
        match rest.find('/') {
            Some(i) => &rest[i + 1..],
            None => return None,
        }
    } else if let Some(r) = u.strip_prefix("git@") {
        match r.find(':') {
            Some(i) => &r[i + 1..],
            None => return None,
        }
    } else {
        u
    };
    let repo = stripped.strip_suffix(".git").unwrap_or(stripped);
    if repo.contains('/') {
        Some(repo.to_string())
    } else {
        None
    }
}

/// 读本地文件并编码成 base64（v5 的 contents 写接口只收 base64）。
fn read_as_base64(path: &str) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| anyhow!("读取 {} 失败: {}", path, e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// 结构化输出助手：给了 --jq 则执行 jq 表达式，给了 --json 则打印完整 JSON。
/// 返回 true 表示已输出（调用方跳过默认文本输出）。
fn print_structured(v: &Value, json: bool, jq: &Option<String>) -> Result<bool> {
    if let Some(f) = jq {
        print!("{}", run_jq(f, v)?);
        return Ok(true);
    }
    if json {
        println!("{}", pretty(v));
        return Ok(true);
    }
    Ok(false)
}

/// 拉取仓库单个文本文件（v5）：
/// - "readme" 走 /repos/{o}/{n}/readme（自动识别 README 文件名）
/// - 其他值走 /repos/{o}/{n}/contents/{path}
/// 返回 (路径/文件名, 解码后的 UTF-8 文本)；文件不存在返回 Ok(None)。
fn fetch_repo_text(
    api: &ApiClient,
    owner: &str,
    name: &str,
    what: &str,
) -> Result<Option<(String, String)>> {
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
    let file_name = v["name"].as_str().or_else(|| v["path"].as_str()).unwrap_or(what).to_string();
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

/// 用 termimad 把 Markdown 渲染到终端（空文本跳过）。
fn render_md(md: &str) {
    if md.trim().is_empty() {
        return;
    }
    let skin = termimad::MadSkin::default();
    skin.print_text(md);
}

/// 用 jaq 对 JSON 值执行 jq 表达式，返回每个输出一行（JSON 格式）。
fn run_jq(filter: &str, input: &Value) -> Result<String> {
    use jaq_core::load::{Arena, File, Loader};
    use jaq_core::{Compiler, Ctx, Vars};

    let program = File { code: filter, path: () };
    let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs());
    let funs = jaq_core::funs::<jaq_core::data::JustLut<jaq_json::Val>>()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|e| anyhow!("jq 表达式解析失败：{:?}", e))?;
    let compiled = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|e| anyhow!("jq 表达式编译失败：{:?}", e))?;
    // jaq-json 的 Val 与 serde_json::Value 之间没有直接转换，走 JSON 字符串
    let json_str = serde_json::to_string(input)?;
    let val = jaq_json::read::parse_single(json_str.as_bytes())
        .map_err(|e| anyhow!("输入解析失败：{}", e))?;
    let ctx = Ctx::<jaq_core::data::JustLut<jaq_json::Val>>::new(&compiled.lut, Vars::new([]));
    let mut out = String::new();
    for r in compiled.id.run((ctx, val)) {
        match r {
            Ok(v) => {
                let s = format!("{}", v);
                out.push_str(&s);
                out.push('\n');
            }
            Err(e) => return Err(anyhow!("jq 执行失败：{:?}", e)),
        }
    }
    Ok(out)
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

fn print_list(v: &Value, fmt: impl Fn(&Value) -> String) {
    let arr = match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o
            .get("issues")
            .or_else(|| o.get("data"))
            .or_else(|| o.get("events"))
            .and_then(|d| d.as_array()),
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

/// 打印 compare 接口的对比结果（commits + diffs + 行数统计）
fn print_compare(v: &Value, patch: bool) {
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
        let id = c["id"].as_str().unwrap_or("").chars().take(8).collect::<String>();
        let title = c["title"]
            .as_str()
            .or_else(|| c["message"].as_str())
            .unwrap_or("?");
        let author = c["author_name"].as_str().or_else(|| c["committer_name"].as_str()).unwrap_or("");
        println!("  {} {} {}", id, title, if author.is_empty() { String::new() } else { format!("({})", author) });
    }
    println!("变更文件 ({}):  +{} -{}", diffs.len(), added, removed);
    for d in diffs {
        let path = d["new_path"]
            .as_str()
            .or_else(|| d["old_path"].as_str())
            .unwrap_or("?");
        let new_file = d["new_file"].as_bool().unwrap_or(false);
        let deleted = d["deleted_file"].as_bool().unwrap_or(false);
        let tag = if new_file { "新增" } else if deleted { "删除" } else { "修改" };
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

/// 用内嵌的 libreditor 交互编辑文本（类似 git commit 的编辑器体验）。
///
/// 非交互（无 TTY，比如被管道调用）时回退到外部 `$EDITOR` / `$VISUAL`（回退 vi），
/// 保证脚本场景仍然可用。用于 `gc issue edit` / `create` / `reply`、`pr create`、
/// `release create` 等需要正文（以及标题）输入的场景。
fn edit_text(initial: &str) -> Result<String> {
    edit_text_external(initial)
}

/// 在**同一个**编辑器会话里同时编辑标题（首行）与正文（其余内容），
/// 一次 `:wq` 即可同时保存两者，避免「先编辑标题、再编辑正文」导致要两次 `:wq`。
///
/// 缓冲区布局：第一行是标题，其后（含紧跟的第一个换行）是正文。
fn edit_title_body(initial_title: &str, initial_body: &str) -> Result<(String, String)> {
    let initial = format!("{}\n{}", initial_title, initial_body);
    let text = edit_text(&initial)?;
    let mut parts = text.splitn(2, '\n');
    let title = parts.next().unwrap_or("").to_string();
    let body = parts.next().unwrap_or("").to_string();
    // 标题按行，去掉可能的 \r（CRLF）与首尾空白。
    let title = title.trim_end_matches('\r').trim().to_string();
    Ok((title, body))
}

/// 外部编辑器回退：写入临时文件，调用 `$EDITOR`，关闭后读回。
fn edit_text_external(initial: &str) -> Result<String> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("gc-edit-{}.md", std::process::id()));
    std::fs::write(&path, initial).map_err(|e| anyhow!("写入临时文件失败: {}", e))?;
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| anyhow!("启动编辑器 {} 失败: {}（可用 EDITOR 环境变量指定）", editor, e))?;
    if !status.success() {
        return Err(anyhow!("编辑器 {} 退出码异常", editor));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow!("读回临时文件失败: {}", e))?;
    let _ = std::fs::remove_file(&path);
    Ok(text)
}

/// 获取可用于讨论接口的 access token：
/// - 若同时配置了 refresh token，则先刷新（并落盘）再返回；
/// - 若只有 access token，则直接返回（不刷新，避免只带 access 调用会注销会话）；
/// - 刷新失败时回退到已有 access token。
pub(crate) fn web_access_token() -> Result<String> {
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
