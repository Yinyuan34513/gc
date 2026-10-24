# gc vs gcl 全方位对比

> 对比对象：**gc**（本项目）与 **gcl**（[jliu666/gitcode-cli](https://atomgit.com/jliu666/gitcode-cli)）。
> 两者都是 Rust 编写、对标 GitHub CLI (`gh`) 的 GitCode 命令行工具。
> 更新日期：2026-10-25。

## 一、工程规模

| 指标 | gc | gcl |
|---|---|---|
| 代码量 | ~15,900 行（25 文件） | 4,267 行（13 文件） |
| 测试 | 78（67 单元 + 11 MCP stdio 集成） | 47 |
| 运行模型 | blocking reqwest + rustls（编译稳、无 tokio 运行时） | async reqwest 0.13 + tokio |
| 代码组织 | Cargo workspace 按域拆 10 个 crate（gc-core 公共层 / gc-cli 命令行层 / gc-repo、gc-pr、gc-issues、gc-grep、gc-search、gc-skill 域层 / gc-mcp / gc 主包） | commands/ 按域拆分（单 crate） |
| 许可 | MIT（+ oh-gc 移植部分单独 LICENSE） | MIT |
| 分发 | **crates.io 已发布**：`cargo install gitcode-gc`（主包 + 9 个库 crate，0.1.1） | 未发布 |

## 二、认证与凭据

| 能力 | gc | gcl |
|---|:-:|:-:|
| PAT 登录 | ✅（浏览器抓取 / `--with-token` / 交互输入） | ✅ |
| 网页端 JWT + 自动刷新 | ✅ 独有（讨论/Issue v2 接口依赖） | ❌ 仅 PAT |
| **git 凭据自动配置** | ✅ `gc auth git`：注册为 gitcode.com / atomgit.com 的 credential helper，HTTPS push/pull/clone 全免密 | ❌ |
| 直接输出 token | ✅ `gc auth pal`（可管道：`export GIT_TOKEN=$(gc auth pal)`） | ❌ |
| 错误信息脱敏 token | ✅ `access_token=***` | ❌ |
| **OAuth broker 扫码登录** | ✅ `gc auth login --qr [--broker URL]`（终端 Unicode 二维码 + 轮询授权 + 换 PAT；broker 地址取 `--broker` > `GITCODE_BROKER`，契约与 lming112/gitcode-cli 兼容，client_secret 由 broker 持有） | ❌ |

## 三、命令覆盖面

| 能力 | gc | gcl |
|---|:-:|:-:|
| 仓库 CRUD / fork / star / 协作者 / 分支 / Hook / Wiki / Key / 保护分支规则 | ✅ | ⚠️ 仅 view/README |
| Issue 全生命周期（网页端 v2 创建/关闭/评论/附件/标签/里程碑/编辑） | ✅ | ⚠️ list/view/评论 |
| PR 生命周期（create/edit/merge/close/reopen/ready/diff/files/commits） | ✅ | ✅ |
| PR cherry-pick / revert | ✅ 网页端接口 | ❌ |
| PR 审批 approve / reset | ✅ 网页端接口 | ⚠️ v5 review --approve |
| PR 行级审查评论 | ✅ `pr comment add --path <文件> --position <行号>`（v5 diff 评论通道） | ✅ `pr comment --path --line` |
| PR checkout / checks | ✅ | ✅ |
| Release / Tag | ✅ | ❌ |
| 讨论 Discussions（v1 网页接口） | ✅ | ❌ |
| 组织 / 成员 / 用户资料 / 粉丝关注 | ✅ | ❌ |
| 搜索 / Compare / 统计 / 事件流 | ✅ | ❌ |
| 仓库内容搜索 | ✅ `repo grep`（ripgrep 库 + SHA 稀疏缓存） | ❌ |
| 裸 API 透传 | ✅ `gc api -m/-p/-b` | ❌ |
| 自我安装 | ✅ `gc install [--system]`（非 root 自动 sudo） | ❌ |
| AI 集成 | ✅ 内置 MCP server（运行时协议）+ `gc skill install`（自动检测 Claude Code / Codex / OpenCode / CodeBuddy / Gemini CLI / AtomCode 六个工具写入技能文件，`skill list` 查看状态，顶层别名 `extension`） | ✅ `gcl skill add`（静态技能文件，支持 --all） |

## 四、输出与体验

| 维度 | gc | gcl |
|---|---|---|
| 结构化输出 | ✅ `--json` / `--jq`（jaq 引擎，完整 jq 语法，覆盖 pr/issue/repo list 与 view） | ✅ `--json "字段"` + `--jq` |
| Markdown 终端渲染 | ✅ termimad（issue/pr view 正文、repo view 的 README） | ✅ termimad |
| 仓库详情展示 | ✅ 摘要 + README 渲染 + LICENSE 原文 | ⚠️ 摘要 |
| PR 列表排序与统计 | ✅ `--web --order-by updated_at/merged_at/created_at`、`--count` 一次返回全状态 | ⚠️ state 过滤 |
| 仓库参数形式 | ✅ 位置参数 / `--repo` / `-R` / 完整 URL / git@ 形式 / remote 自动推断 | ⚠️ `-R owner/repo` + remote 推断 |
| 从文件读正文 | ✅ `--file`（issue create / pr create / pr comment add） | ❌ |
| 行级评论 | ✅ `--path` + `--position`（行级走 v5 diff 评论，`--web` 普通评论不支持行级会明确报错） | ✅ `--path` + `--line` |
| 扫码登录二维码 | ✅ 终端 Unicode 半块渲染，URL 超长自动降级为纯链接 | ❌ |

## 五、可靠性与性能

| 风险点 | gc | gcl |
|---|---|---|
| 请求网络抖动 | ✅ 自动重试 3 次（500ms→1s→2s 指数退避），服务端错误不重试 | ❌ |
| 大批量下载并发 | ✅ rayon 全局线程池（核数上限） | 无下载功能 |
| 大仓库搜索 | ✅ SHA 对比稀疏缓存：只拉 `--include` 命中文件，未变化文件零流量（缓存于 `~/.cache/gc/grep/`） | — |
| token 泄漏面 | ✅ 错误信息脱敏；凭据 helper 不落 `~/.git-credentials` 明文 | 仅 PAT 明文配置文件 |

## 六、结论

- **gc 已全面领先**：认证四通道（浏览器抓取 / PAT / git 凭据助手 / broker 扫码）、git 凭据免密、Release/讨论/组织、cherry-pick/revert/审批、PR 行级评论、裸 API 透传、`--json`/`--jq`、termimad、稀疏缓存 grep、请求重试、MCP + skill 双 AI 集成、crates.io 一键安装——gcl 此前的差异化优势（结构化输出、Markdown 渲染、PR 工作流细节、行级评论）已全部追平或反超。
- **gcl 剩余差异仅在工程风格**：async tokio 运行时 vs gc 的 blocking reqwest；单 crate commands/ 拆分 vs gc 的 workspace 多 crate——两者都已按域拆分，属路线差异而非能力差距。
- 仍值得吸收的小点：`gcl skill add` 的静态技能文件思路与 gc 的 MCP 定位互补（gc 的 `skill install` 已覆盖同类场景）。

## 附：本次更新（2026-10-25）

- gc 补齐 **PR 行级审查评论**（`pr comment add --path/--position`），gcl 最后两张功能性牌清零；
- gc 重构为 **Cargo workspace 10 crate**，“代码组织”从劣势转为对等；
- 新增 **OAuth broker 扫码登录**（`gc auth login --qr`）；
- gc 主包以 **gitcode-gc** 上架 crates.io（`gc` 包名已被占用），支持 `cargo install gitcode-gc`。
