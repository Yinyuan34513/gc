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
gc repo view owner/name          # 查看仓库详情
gc repo list                      # 列出当前用户的仓库
gc repo list owner               # 列出某用户的仓库
gc repo clone owner/name [dest]  # 克隆（内部调用 git clone）
gc repo create my-new-repo --description "..." --private   # 在当前用户名下建仓库
```

### Issue / PR `gc issue` / `gc pr`

```bash
gc issue list owner/name --state open
gc issue view owner/name 12
gc issue create owner/name --title "bug" --body "..."

gc pr list owner/name
gc pr view owner/name 3
gc pr create owner/name --title "fix" --head dev --base main
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

## 许可

自用工具，按你自己的需要随意改。
