# gc — GitCode (AtomGit) 命令行工具

`gc` 是一个用 Rust 写的 GitCode / AtomGit 命令行客户端，风格对标 GitHub 官方的 `gh`。
它封装了 GitCode 的 **v5 开放 API**（Gitee 兼容）以及网页端的 **v1 讨论接口**，让你不用打开浏览器就能管理仓库、Issue、PR、发行版和讨论。

> 与同类工具 gcl（jliu666/gitcode-cli）的全方位功能对比见
> [`docs/comparison-gcl.md`](docs/comparison-gcl.md)。

## 生成说明

本工具**完全由 CodeBuddy CLI 的 Hy3 模型自行生成**：全部代码实现由 Hy3 完成，仓库所有者仅提供提示词（prompt）与需求，不参与代码编写。

## 安装

```bash
cargo build --release
# 然后把 target/release/gc 放进 PATH
```

> 依赖：`anyhow`、`clap`、`reqwest`(blocking/rustls)、`serde`、`toml`、`dirs`、`rpassword`、`indicatif`。

## 配置

`gc` 把配置保存在 `~/.config/gitcode.toml`，token 以明文保存（请自行评估风险）。

```bash
# 录入个人访问令牌（PAT），会顺手验证一下
gc token set

# 检查当前 token 是否有效
gc auth

# 直接输出 PAT（明文，可管道使用）
export GIT_TOKEN=$(gc auth pal)

# 把 gc 注册为 git 凭据助手（全局，gitcode.com / atomgit.com）
gc auth git
```

### git 凭据自动配置

执行一次 `gc auth git` 之后，gc 会把自己注册为 git 的 credential helper
（写入全局配置 `credential.https://gitcode.com.helper` 和
`credential.https://atomgit.com.helper`）。之后所有针对这两个站点的
HTTPS `git push` / `git pull` / `git clone` 都会自动带上 gc 里保存的
PAT，**不需要再手动设置账号密码**，也不用把 token 明文写进
`~/.git-credentials`。

```bash
gc auth git            # 全局注册（默认）
gc auth git --local    # 只对当前仓库生效
```

只想装某一个仓库的话用 `--local`。换 token 后无需重新注册——
helper 每次实时读取 gc 的配置。

部分接口（讨论 Discussions）走的是网页端 v1 接口，需要浏览器的 JWT，而不是 PAT：

```bash
# 录入网页端 access / refresh token（从浏览器 Cookie 里拿）
gc web-token set

# 手动刷新网页端 JWT（只带 access 调刷新接口会注销会话，务必同时有 refresh）
gc refresh
```

也可用环境变量覆盖：

- `GITCODE_TOKEN` — 个人访问令牌
- `GITCODE_API_BASE` — API 根地址（默认 `https://api.gitcode.com`）
- `GITCODE_WEB_TOKEN` / `GITCODE_WEB_REFRESH_TOKEN` — 网页端 JWT
- `GITCODE_WEB_BASE` — 网页 API 根地址（默认 `https://web-api.gitcode.com`）

## 用法

不带任何参数运行会打印帮助（和 `gh` 一样）。

### 仓库 `gc repo`

```bash
gc repo view owner/name          # 查看仓库详情（精简摘要）
gc repo view owner/name --json    # 查看完整 JSON 元数据
gc repo list                      # 列出当前用户的仓库
gc repo list owner               # 列出某用户的仓库
gc repo clone owner/name [dest]                       # 克隆（内部调用 git clone）
gc repo clone owner/name --api [--path src] [--ref main] [dest]   # 不用 git，直接走网页端 v2 接口下载整个文件树（--path 只下某子目录）
gc repo create my-new-repo --description "..." --private   # 在当前用户名下建仓库
gc repo fork [owner/name] [--source owner/name] [--name new-name] [--description "..."] [--link-remote] [--private]   # Fork 到当前用户命名空间（网页端 v2 接口）；省略源仓库时从 git remote origin 推断
gc repo fork owner/name --org myorg [--name new-name]           # Fork 进组织命名空间（v5 接口，--description 不生效）
gc repo fork owner/name --force                                 # 已 fork 过时，把已有 fork 的默认分支硬重置到源仓库 HEAD（丢弃额外提交）
gc repo sync [owner/name] [--branch main] [--no-wait]           # 同步 fork 仓库与其上游（--no-wait 只提交不轮询）；省略仓库时从 git remote origin 推断
gc repo sync owner/name --force --yes [--source owner/name]     # 上游历史被重写导致快进失败时，硬重置目标分支到源 HEAD（受保护分支会自动解保护并在重建后恢复）
gc repo set owner/name <private|public>                         # 设置仓库可见性（v5 开放 API）
gc repo star owner/name [--unstar]                             # 给仓库点 Star（加 --unstar 取消）
gc repo tree owner/name [--ref main]   # 列出文件树（网页端 v2 接口）
gc repo read owner/name <file> [--ref main] [--output path]   # 读取文件内容（网页端 v2 接口）
gc repo download owner/name <file1> <file2> ... [--ref main] [--output dir]   # 多线程流式下载多个文件
gc repo delete owner/name --yes                                # 删除仓库（危险操作，必须加 --yes 确认）

gc repo update [owner/name] [--name x] [--description "..."] [--homepage url] [--private|--public] [--default-branch main]
gc repo tag [TAGS]... [--repo owner/name]     # 设置仓库标签（tag_list，整体替换；不给标签则清空）。需网页端 JWT（gc web-token set）
gc repo transfer <new-owner> [--repo owner/name] [--team-ids 1,2] --yes   # 转移归属（危险操作）
gc repo forks [owner/name] [--sort newest|oldest|stargazers]   # 列出该仓库的 fork
gc repo contributors [owner/name] [--type committers|authors] [--stats] [--author alice]
gc repo languages [owner/name]                                 # 语言构成（服务端直接给百分比）
gc repo events [owner/name] [--filter push|merged|issue|...] [--author alice] [--after 2026-01-01]
gc repo stats [owner/name] --branch main [--author alice] [--since 2026-01-01] [--only-self]
gc repo stats [owner/name] --downloads [--since ...] [--until ...] [--direction desc]
gc repo settings [owner/name]                                  # 查看高级设置
gc repo settings [owner/name] --disable-fork true --branch-regex '^feat/'   # 带参数则改为更新
gc repo module [owner/name] --wiki false --issues true         # 开关功能模块
gc repo roles [owner/name]                                     # 列出自定义角色
gc repo transition [owner/name] [--mode 1|2]                   # 权限继承模式：1 继承上级、2 独立管理
gc repo archive [owner/name] [--open] [--password ...]         # 归档/取消归档（服务端要求验密码，省略则交互输入）
gc repo get-remote                                             # 查看 gc 用哪个 git remote 推断 owner/name
gc repo set-remote upstream                                    # 改用 upstream（写入 git local config 的 gc.remote）
```

> `gc repo tree` 走的是网页端 v2 文件列表接口（`/api/v2/projects/{owner%2Fname}/repository/file-list`），
> 需要网页端 JWT（`gc web-token set`），且 `ref_name` 必填。它返回扁平路径列表，`gc` 在本地组装成 ASCII 树。

> ⚠ **`gc repo delete` 是危险操作**：不加 `--yes` 只会打印「即将删除」并拒绝执行；加 `--yes` 才会真正删除且**不可恢复**。请务必确认仓库名无误。

### Issue / PR `gc issue` / `gc pr`

`<REPO>` 改为可选的 `--repo owner/name` 参数：在 gitcode 仓库目录下可省略，自动从当前目录的 git remote 推断（与 gh 行为一致）。

```bash
# 在克隆的 gitcode 仓库目录里直接运行，自动推断 owner/name
gc issue list
gc issue view 12
gc pr list
# 显式指定仓库（两种写法等价）
gc issue list --repo owner/name --state open
gc pr view --repo owner/name 3

# 通过网页端 v2 接口创建（description 字段，返回 #iid 与 web_url；不指定 --body 默认为空）
gc issue create --title "bug" --body "..."
# 带附件 / 标签 / 私有
gc issue create --title "bug" --body "..." --attach ./log.txt --label bug --confidential
# 关闭（可一次多个编号）
gc issue close 1 2 3
# 重新打开（可一次多个编号）
gc issue reopen 1 2 3
# 回复（评论）某个 Issue
gc issue reply 1 --body "已修复，请验证"

# 创建前用网页端 v2 对比接口预览 diff（compare = pr create 的核心）
gc pr create --title "fix" --head dev --base main
# 跨仓库合并 / 打印每个文件具体 diff
gc pr create --title "fix" --head dev --base main --target-repo other/repo --patch
```

### 发行版 `gc release`

```bash
gc release list owner/name
gc release view owner/name v1.0.0
gc release create owner/name v1.0.0 --name "正式版" --target main
gc release upload owner/name v1.0.0 ./dist/app.tar.gz   # 上传附件（带进度条）
```

### 搜索 `gc search`

```bash
gc search repos "关键词"
gc search users "用户名"
gc search issues "关键词"
```

### 用户 `gc user`（网页端 v1 接口）

```bash
gc user view <username>                    # 查看用户资料（网页端 v1 接口）
gc user avatar <username> [--output path]  # 获取 / 下载用户头像（不指定 --output 则只打印 URL）
```

### Wiki `gc wiki`（网页端 v2 接口）

```bash
gc wiki create owner/name <标题> --content "Markdown 内容" [--message "提交说明"]
gc wiki create owner/name <标题> --file ./page.md        # 内容从文件读取
```

### 讨论 `gc discussion`（网页端 v1 接口）

讨论接口需要网页端 JWT（`gc web-token set`），且每次操作前会自动尝试刷新 token。

```bash
# source_id 取仓库页面的 page-repo-id（数字）；
# category_id 取创建页 URL /discussions/create/<hash> 里的 hash
gc discussion create \
  --source_id 10544417 \
  --source_type 2 \
  --category_id d3cc2a904e8a4bf8a4a965d3ad37908f \
  --title "标题" \
  --content "正文（Markdown）"

# id 取讨论页 URL /discussions/<hash> 里的 hash（不是数字 id）
gc discussion delete cd921cf8f66f441cb07cbea7135e10ca
```

## MCP 服务器（给 AI 客户端用）

`gc` 内置一个 MCP（Model Context Protocol）服务器，把上面的能力包装成 AI 客户端（如
Claude Desktop、各种 IDE 插件）可调用的工具。传输层是 **stdio 上的换行分隔 JSON-RPC 2.0**，
日志走 stderr（stdout 是协议通道，任何多余输出都会破坏会话）。

```bash
# 启动服务器（默认不开破坏性工具，最大并发 4）
gc mcp start

# 暴露破坏性工具（删仓库 / 改可见性 / 删讨论 / 删评论等），给「敢放手让 AI 改东西」的场景
gc mcp start --allow-destructive

# 调并发数（1 表示完全串行）
gc mcp start --max-concurrency 2
```

客户端配置示例（stdio 方式，AI 客户端会在需要时拉起这个进程）：

```json
{
  "mcpServers": {
    "gitcode": {
      "command": "gc",
      "args": ["mcp", "start"]
    }
  }
}
```

> 鉴权与环境变量与 CLI 完全一致（见上文「配置」）：需要 PAT（`GITCODE_TOKEN` 或
> `gc token set`），讨论 / Issue / 文件读取 / Wiki / 标签 等接口还需要网页端 JWT
>（`GITCODE_WEB_TOKEN` 或 `gc web-token set`）。`repo` 参数在大部分工具里都可省略，
> 省略时从当前目录的 git remote 推断。

### 工具一览

下表按类别列出全部工具。带 `★` 的是破坏性（默认不注册，需 `--allow-destructive`）工具。

**认证 / 用户**
- `gc_auth_status` — 校验 PAT 是否有效并返回当前用户
- `gc_refresh_web_token` — 刷新网页端 JWT（鉴权失败时用它再重试）
- `gc_user_view` *(网页端)* — 查看用户公开资料
- `gc_user_profile` *(v5)* — 查看用户资料
- `gc_user_update` — 更新当前用户资料
- `gc_user_followers` / `gc_user_following` — 粉丝 / 关注
- `gc_user_starred` / `gc_user_subscriptions` — 收藏的仓库 / 订阅的仓库
- `gc_user_namespaces` / `gc_user_namespace` — 命名空间
- `gc_user_keys` / `gc_user_key` / `gc_user_key_add` / `gc_user_key_delete` ★ — SSH 公钥
- `gc_user_issues` / `gc_user_prs` — 我的 Issue / PR
- `gc_user_org_membership` / `gc_user_org_leave` ★ — 组织成员资格 / 退出组织
- `gc_user_orgs` — 用户所属组织
- `gc_user_events` — 用户动态
- `gc_user_emails` — 我的邮箱

**仓库**
- `gc_repo_view` / `gc_repo_list` — 查看 / 列出仓库
- `gc_repo_tree` *(网页端)* / `gc_repo_read` *(网页端)* — 文件树 / 读文件
- `gc_repo_create` — 建仓库
- `gc_repo_fork` *(网页端)* — Fork
- `gc_repo_update` — 改仓库属性（名 / 描述 / 主页 / 可见性 / 默认分支）
- `gc_repo_set_tags` *(网页端)* — 设置仓库标签（tag_list，整体替换；传 `[]` 清空；会保留名/描述/主页不被覆盖）
- `gc_repo_transfer` ★ — 转移归属
- `gc_repo_forks` / `gc_repo_contributors` / `gc_repo_languages` / `gc_repo_events` / `gc_repo_stats`
- `gc_repo_settings` / `gc_repo_settings_update` — 高级设置查看 / 更新
- `gc_repo_module` — 开关功能模块（wiki / Issue / 安全 / PR / fork / 分析 / 讨论）
- `gc_repo_roles` / `gc_repo_transition` — 自定义角色 / 权限继承模式
- `gc_repo_archive` — 归档 / 取消归档
- `gc_repo_star` — 加星 / 取消加星
- `gc_repo_delete` ★ / `gc_repo_set_visibility` ★ — 删仓库 / 改可见性
- `gc_repo_sync` — 硬重置式同步 fork

**Issue / PR / 讨论**
- `gc_issue_list` / `gc_issue_view` / `gc_issue_create` / `gc_issue_reply` / `gc_issue_edit` / `gc_issue_close` / `gc_issue_reopen`
- `gc_pr_list` / `gc_pr_view` / `gc_pr_create` / `gc_pr_files` / `gc_pr_commits` / `gc_pr_comments` / `gc_pr_comment` / `gc_pr_comment_reply` / `gc_pr_comment_edit` / `gc_pr_comment_delete` ★
- `gc_pr_review` / `gc_pr_test` / `gc_pr_reviewers` / `gc_pr_testers` / `gc_pr_assignees` / `gc_pr_option_reviewers` / `gc_pr_labels` / `gc_pr_linked_issues` / `gc_pr_link_issues`
- `gc_pr_close` / `gc_pr_reopen` / `gc_pr_history` / `gc_pr_logs` / `gc_pr_edit` / `gc_pr_merge` ★
- `gc_discussion_create` / `gc_discussion_delete` ★

**分支 / 提交 / 文件**
- `gc_branch_list` / `gc_branch_view` / `gc_branch_create` / `gc_branch_protect` / `gc_branch_rules` / `gc_branch_delete` ★
- `gc_commit_list` / `gc_commit_view` / `gc_commit_diff` / `gc_commit_compare` / `gc_commit_comment`
- `gc_file_list` / `gc_file_get` / `gc_file_write` / `gc_file_delete` ★

**标签（tag）/ Webhook / Label / 里程碑 / 协作者 / 组织**
- `gc_tag_list` / `gc_tag_view` / `gc_tag_create` / `gc_tag_delete` ★ / `gc_tag_protect` / `gc_tag_unprotect`
- `gc_tag_protected_list` / `gc_tag_protected_view` / `gc_tag_protected_create` / `gc_tag_protected_update` / `gc_tag_protected_delete` ★
- `gc_hook_list` / `gc_hook_view` / `gc_hook_create` / `gc_hook_update` / `gc_hook_delete` ★
- `gc_label_list` / `gc_label_view` / `gc_label_create` / `gc_label_update` / `gc_label_delete` ★
- `gc_milestone_list` / `gc_milestone_view` / `gc_milestone_create` / `gc_milestone_update` / `gc_milestone_delete` ★
- `gc_collaborator_list` / `gc_collaborator_add` / `gc_collaborator_remove` ★ / `gc_collaborator_permission`
- `gc_org_list` / `gc_org_view` / `gc_org_members` / `gc_org_repos`

**发行版 / 搜索 / Wiki**
- `gc_release_list` / `gc_release_view` / `gc_release_create`
- `gc_search_repos` / `gc_search_users` / `gc_search_issues` / `gc_search_web` *(网页端)*
- `gc_wiki_create` *(网页端)*

## 实现说明（踩过的坑）

- **v5 开放 API 才用 PAT**：`/api/v5/*` 走 `access_token` 查询参数或 `PRIVATE-TOKEN` 头。
- **讨论走网页 v1 接口**：`/api/v1/discuss` 有 WAF，PAT 会被拦（HTTP 418），必须用浏览器 JWT + 浏览器请求头。
- **创建讨论用 JSON 而非 form**：字段是 snake_case（`title` / `md_content` / `source_id` / `source_type` / `category_id`）。
- **删除讨论用 hash 不是数字 id**：hash 取自讨论页 URL。
- **刷新 token 必须同时带 access + refresh**：只带 access 调 `GET /uc/api/v1/user/oauth/token` 会让服务端注销当前会话。
- **大文件走 blob 接口会被限流**：`repo read` / `repo download` 用的是网页端 `/repository/files`，服务端对超大文件（如几十 MB 的 rar）会返回 `is_limited: true` 且 `content` 为空——此时读到 0 字节是正常的，这类文件请改用 `gc repo clone` 拿完整内容。
- **`file_path` / `ref` 会做百分号编码**：`&`、中文、空格等特殊字符会被编码后放进 query，避免 `&` 被当成参数分隔符导致 404（实测裸 `&` 会返回 "file not found"。）

## 许可

自用工具，按你自己的需要随意改。

### 许可证

本项目（`gc`）以 **MIT 许可证** 发布，完整文本见 [`LICENSE`](LICENSE)。

MIT 是极宽松的 permissive 许可，与 BSD、Apache-2.0、ISC 等同样互相兼容；
本项目也完全兼容其所包含的 MIT 代码（见下方第三方声明），可自由使用、修改与再分发。

### 第三方声明

PR 全生命周期、分支 / 提交 / 文件、仓库治理（tag / hook / label / milestone /
collaborator / org）这几组能力的接口契约移植自
[ohos_gitcode_cli（`@oh-gc/cli`）](https://gitcode.com/guozejun/ohos_gitcode_cli)，
作者 guozejun，MIT 许可。按 MIT 要求保留的完整版权声明见
[`LICENSE.oh-gc`](LICENSE.oh-gc)，相关源码文件内也保留了逐处标注。
