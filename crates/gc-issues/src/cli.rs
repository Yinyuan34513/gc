//! Issue 域：参数定义（IssueCmd/IssueAction）与 `Command::Issue` 分发。

use anyhow::{anyhow, Context as _};
use clap::{Parser, Subcommand};
use gc_core::api::{self, ApiClient};
use gc_core::ctx::{resolve_repo, web_access_token};
use gc_core::output::{
    edit_text, edit_title_body, pretty, print_list, print_structured, read_as_base64, render_md,
};
use serde_json::{json, Value};

#[derive(Parser)]
pub struct IssueCmd {
    #[command(subcommand)]
    pub action: IssueAction,
}
#[derive(Subcommand)]
pub enum IssueAction {
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
        #[arg(long, default_value = "")]
        body: String,
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
        #[arg(long)]
        body: String,
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

pub fn run(action: IssueAction, api: &ApiClient) -> anyhow::Result<()> {
    match action {
        IssueAction::List {
            repo,
            repo_pos,
            state,
            json,
            jq,
        } => {
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
        IssueAction::View {
            repo,
            number,
            json,
            jq,
        } => {
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
        IssueAction::Create {
            repo,
            title,
            body,
            file,
            attach,
            label,
            confidential,
        } => {
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
            let project_id = api.repo(&o, &n)?["id"].as_u64().unwrap_or(0);
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
            let v = crate::api::issue_create(
                &token,
                &o,
                &n,
                &title,
                &description,
                confidential,
                &label,
            )?;
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
            println!(
                "已回复 Issue #{}（讨论：{} 评论 id：{}）",
                issue, discussion_id, note_id
            );
        }
        IssueAction::Edit {
            repo,
            issue,
            title,
            body,
            file,
        } => {
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
            println!(
                "已编辑 Issue #{}：{}",
                v["iid"].as_u64().unwrap_or(issue),
                v["web_url"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}
