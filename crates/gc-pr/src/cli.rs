//! PR 域：参数定义（PrCmd/PrAction 及评论/评审人/标签/关联子命令）与 `Command::Pr` 分发。

use anyhow::{anyhow, Context as _};
use clap::{Parser, Subcommand};
use gc_core::api::{self, ApiClient};
use gc_core::ctx::{current_git_branch, git_run, resolve_repo, split_repo, web_access_token};
use gc_core::output::{edit_title_body, pretty, print_list, print_structured, render_md, run_jq};
use gc_repo::cli::print_compare;
use serde_json::{json, Value};

#[derive(Parser)]
pub struct PrCmd {
    #[command(subcommand)]
    pub action: PrAction,
}
#[derive(Subcommand)]
pub enum PrAction {
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
        #[arg(long, default_value = "")]
        body: String,
        /// 从 Markdown 文件读取正文（覆盖 --body）
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// 源分支（要合并的分支，对应 compare 的 from）
        #[arg(long)]
        head: String,
        /// 目标分支（合并到的分支，对应 compare 的 to）
        #[arg(long)]
        base: String,
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
pub struct PrCommentCmd {
    #[command(subcommand)]
    pub action: PrCommentAction,
}
#[derive(Subcommand)]
pub enum PrCommentAction {
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
pub struct PrUserCmd {
    #[command(subcommand)]
    pub action: PrUserAction,
}
#[derive(Subcommand)]
pub enum PrUserAction {
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
pub struct PrLabelCmd {
    #[command(subcommand)]
    pub action: PrLabelAction,
}
#[derive(Subcommand)]
pub enum PrLabelAction {
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
pub struct PrLinkCmd {
    #[command(subcommand)]
    pub action: PrLinkAction,
}
#[derive(Subcommand)]
pub enum PrLinkAction {
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

pub fn run(action: PrAction, api: &ApiClient) -> anyhow::Result<()> {
    match action {
        PrAction::List {
            repo,
            repo_pos,
            state,
            web,
            order_by,
            sort,
            page,
            per_page,
            count,
            json,
            jq,
        } => {
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
                        other => anyhow::bail!(
                            "order_by 只支持 updated_at / merged_at / created_at，收到：{}",
                            other
                        ),
                    };
                    let v = crate::api::pr_list_web(
                        &token, &o, &n, &state, &order, &sort, page, per_page,
                    )?;
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
                            it["iid"]
                                .as_u64()
                                .or_else(|| it["number"].as_u64())
                                .unwrap_or(0),
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
            let mut section = |label: &str, prs: Vec<Value>| -> anyhow::Result<()> {
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
                    println!(
                        "  #{} {}{} ({} → {}, by {})",
                        num, title, draft, head, base, author
                    );
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
            println!("已检出 PR #{} → 本地分支 {}", number, branch);
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
            let combined = pr["pipeline_status_with_code_quality"]
                .as_str()
                .unwrap_or("");
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
                println!(
                    "门禁检查: {}",
                    if gate {
                        "通过"
                    } else {
                        "未通过/未开启"
                    }
                );
            }
            if let Some(url) = pr["html_url"].as_str().or_else(|| pr["web_url"].as_str()) {
                println!("链接: {}", url);
            }
        }
        PrAction::Diff {
            repo,
            number,
            name_only,
        } => {
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
        PrAction::View {
            repo,
            number,
            json,
            jq,
        } => {
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
                    v["body"]
                        .as_str()
                        .or_else(|| v["description"].as_str())
                        .unwrap_or(""),
                );
            }
        }
        PrAction::Create {
            repo,
            title,
            body,
            file,
            head,
            base,
            target_repo,
            patch,
        } => {
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
            println!(
                "已创建 PR: #{} {}",
                v["number"].as_u64().unwrap_or(0),
                v["html_url"].as_str().unwrap_or("")
            );
        }

        // ---- PR 生命周期（移植自 @oh-gc/cli，MIT, Copyright (c) guozejun）----
        PrAction::Merge {
            repo,
            number,
            method,
            title,
            description,
        } => {
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
        PrAction::Edit {
            repo,
            number,
            title,
            body,
            draft,
        } => {
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
            println!(
                "已更新 PR #{}：{}",
                number,
                v["title"].as_str().unwrap_or("")
            );
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
                format!(
                    "{} {}",
                    &sha[..sha.len().min(8)],
                    msg.lines().next().unwrap_or("")
                )
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
                        it["body"]
                            .as_str()
                            .unwrap_or("")
                            .lines()
                            .next()
                            .unwrap_or("")
                    )
                });
            }
            PrCommentAction::Add {
                repo,
                number,
                body,
                file,
                path,
                position,
                web,
            } => {
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
                    api.create_pull_comment(&o, &n, number, &body, path.as_deref(), position)?
                };
                println!(
                    "已评论 PR #{}（评论 id {}）",
                    number,
                    v["id"].as_u64().unwrap_or(0)
                );
            }
            PrCommentAction::Reply {
                repo,
                number,
                discussion_id,
                body,
            } => {
                let (o, n) = resolve_repo(repo)?;
                let v = api.reply_pull_discussion(&o, &n, number, &discussion_id, &body)?;
                println!(
                    "已回复讨论 {}（评论 id {}）",
                    discussion_id,
                    v["id"].as_u64().unwrap_or(0)
                );
            }
            PrCommentAction::Edit {
                repo,
                comment_id,
                body,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.edit_pull_comment(&o, &n, comment_id, &body)?;
                println!("已编辑评论 {}", comment_id);
            }
            PrCommentAction::Delete {
                repo,
                comment_id,
                yes,
            } => {
                if !yes {
                    anyhow::bail!("删除评论不可撤销，确认请加 --yes");
                }
                let (o, n) = resolve_repo(repo)?;
                api.delete_pull_comment(&o, &n, comment_id)?;
                println!("已删除评论 {}", comment_id);
            }
        },
        PrAction::Reviewer(c) => match c.action {
            PrUserAction::Add {
                repo,
                number,
                users,
                append,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.assign_pull_reviewers(&o, &n, number, &users, append)?;
                println!(
                    "已{}评审人：{}",
                    if append { "追加" } else { "设置" },
                    users
                );
            }
            PrUserAction::Remove {
                repo,
                number,
                users,
            } => {
                if users.trim().is_empty() {
                    anyhow::bail!("移除评审人必须指定用户名（逗号分隔）");
                }
                let (o, n) = resolve_repo(repo)?;
                api.remove_pull_reviewers(&o, &n, number, &users)?;
                println!("已移除评审人：{}", users);
            }
        },
        PrAction::Tester(c) => match c.action {
            PrUserAction::Add {
                repo,
                number,
                users,
                append,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.assign_pull_testers(&o, &n, number, &users, append)?;
                println!(
                    "已{}测试人：{}",
                    if append { "追加" } else { "设置" },
                    users
                );
            }
            PrUserAction::Remove {
                repo,
                number,
                users,
            } => {
                if users.trim().is_empty() {
                    anyhow::bail!("移除测试人必须指定用户名（逗号分隔）");
                }
                let (o, n) = resolve_repo(repo)?;
                api.remove_pull_testers(&o, &n, number, &users)?;
                println!("已移除测试人：{}", users);
            }
        },
        PrAction::Assignee(c) => match c.action {
            PrUserAction::Add {
                repo,
                number,
                users,
                append: _,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.assign_pull_assignees(&o, &n, number, &users)?;
                println!("已指派：{}", users);
            }
            PrUserAction::Remove {
                repo,
                number,
                users,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.remove_pull_assignees(&o, &n, number, &users)?;
                if users.trim().is_empty() {
                    println!("已移除全部指派人");
                } else {
                    println!("已移除指派人：{}", users);
                }
            }
        },
        PrAction::Review {
            repo,
            number,
            force,
        } => {
            let (o, n) = resolve_repo(repo)?;
            api.review_pull(&o, &n, number, force)?;
            println!("已对 PR #{} 提交评审通过", number);
        }
        PrAction::Test {
            repo,
            number,
            force,
        } => {
            let (o, n) = resolve_repo(repo)?;
            api.test_pull(&o, &n, number, force)?;
            println!("已对 PR #{} 提交测试通过", number);
        }
        PrAction::CherryPick {
            repo,
            number,
            branch,
            with_new_merge_request,
        } => {
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
            let v = crate::api::pr_cherry_pick(
                &token,
                &o,
                &n,
                number,
                &branch,
                with_new_merge_request,
            )?;
            if let Some(mr) = v.as_object() {
                let iid = mr.get("iid").and_then(|x| x.as_u64()).unwrap_or(0);
                let url = mr.get("web_url").and_then(|x| x.as_str()).unwrap_or("");
                if iid > 0 {
                    println!(
                        "已 cherry-pick PR #{} 到 {}，新建 MR: #{} {}",
                        number, branch, iid, url
                    );
                } else {
                    println!("已 cherry-pick PR #{} 到 {}：{}", number, branch, v);
                }
            } else {
                println!("已 cherry-pick PR #{}：{}", number, v);
            }
        }
        PrAction::Revert {
            repo,
            number,
            branch,
            with_new_merge_request,
        } => {
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
                    println!(
                        "已 revert PR #{} 到 {}，新建 MR: #{} {}",
                        number, branch, iid, url
                    );
                } else {
                    println!("已 revert PR #{} 到 {}：{}", number, branch, v);
                }
            } else {
                println!("已 revert PR #{}：{}", number, v);
            }
        }
        PrAction::Approval {
            repo,
            number,
            action,
            force,
        } => {
            let action = action.to_lowercase();
            if action != "approve" && action != "reset" {
                anyhow::bail!("action 只支持 approve（通过）或 reset（撤销）");
            }
            let (o, n) = resolve_repo(repo)?;
            let token = web_access_token()?;
            crate::api::pr_approval(&token, &o, &n, number, &action, force)?;
            println!(
                "已{} PR #{} 的审批",
                if action == "approve" {
                    "通过"
                } else {
                    "撤销"
                },
                number
            );
        }
        PrAction::Label(c) => match c.action {
            PrLabelAction::Add {
                repo,
                number,
                labels,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.add_pull_labels(&o, &n, number, &labels)?;
                println!("已追加标签：{}", labels.join(", "));
            }
            PrLabelAction::Set {
                repo,
                number,
                labels,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.replace_pull_labels(&o, &n, number, &labels)?;
                println!("标签已设为：{}", labels.join(", "));
            }
            PrLabelAction::Remove {
                repo,
                number,
                labels,
            } => {
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
                        it["number"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| it["number"].as_u64().unwrap_or(0).to_string()),
                        it["title"].as_str().unwrap_or("?")
                    )
                });
            }
            PrLinkAction::Add {
                repo,
                number,
                issues,
            } => {
                let (o, n) = resolve_repo(repo)?;
                api.link_pull_issues(&o, &n, number, &issues)?;
                println!("已关联 Issue：{:?}", issues);
            }
            PrLinkAction::Remove {
                repo,
                number,
                issues,
            } => {
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
                    it["content"]
                        .as_str()
                        .unwrap_or(it["action"].as_str().unwrap_or(""))
                )
            });
        }
    }
    Ok(())
}
