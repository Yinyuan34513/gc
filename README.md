# gc — GitCode (AtomGit) 命令行工具

`gc` 是一个用 Rust 写的 GitCode / AtomGit 命令行客户端，风格对标 GitHub 官方的 `gh`。
它封装了 GitCode 的 **v5 开放 API**（Gitee 兼容）以及网页端的 **v1 讨论接口**，让你不用打开浏览器就能管理仓库、Issue、PR、发行版和讨论。

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
```

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

# 以下命令的 [owner/name] 都可省略，省略时从当前目录的 git remote 推断
gc repo update [owner/name] [--name x] [--description "..."] [--homepage url] [--private|--public] [--default-branch main]
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

### 第三方声明

PR 全生命周期、分支 / 提交 / 文件、仓库治理（tag / hook / label / milestone /
collaborator / org）这几组能力的接口契约移植自
[ohos_gitcode_cli（`@oh-gc/cli`）](https://gitcode.com/guozejun/ohos_gitcode_cli)，
作者 guozejun，MIT 许可。按 MIT 要求保留的完整版权声明见
[`LICENSE.oh-gc`](LICENSE.oh-gc)，相关源码文件内也保留了逐处标注。
