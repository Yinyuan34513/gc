# 发布流程（crates.io）

发布由 GitHub Actions 自动完成：**打 tag 即发布**。

仓库：[github.com/Yinyuan34513/gc](https://github.com/Yinyuan34513/gc)（镜像，CI 在这里跑）
主仓：[atomgit.com/ghhhJoshrjnrhr/gc](https://atomgit.com/ghhhJoshrjnrhr/gc)（origin）

## 步骤

```bash
# 1. 改版本号：根 Cargo.toml 的 [workspace.package] version +
#    同文件「内部 crate」段里 9 个 gc-* 依赖的 version（共 10 处）
#    例如 sed -i 's/0.1.3/0.1.4/g' Cargo.toml

# 2. 提交并推到两个远端
git push origin main   # atomgit
git push github main   # GitHub（CI 所在）

# 3. 打 tag（必须与 workspace 版本一致，v 前缀）并推送
git tag v0.1.4
git push github v0.1.4
```

推送 tag 后，`Publish to crates.io` workflow 自动执行：

1. `cargo fmt --check` + `cargo test --workspace`（失败则不发布）
2. 校验 tag 与 `Cargo.toml` 版本一致
3. 按依赖拓扑顺序发布 10 个 crate：
   `gc-core → gc-search / gc-grep / gc-skill / gc-issues → gc-repo → gc-mcp → gc-pr → gc-cli → gitcode-gc`

## 手动触发（不发布）

GitHub 仓库 → Actions → Publish to crates.io → Run workflow → 勾选 `dry_run`，
只跑 `cargo publish --dry-run` 验证打包，不落库。

## 凭据

- workflow 使用 repo secret `CARGO_REGISTRY_TOKEN`（crates.io token，
  需要该账号下全部 10 个 crate 的 publish 权限；本地 `cargo login` 用的是同一个 token）。
- token 只存在于 GitHub Secrets 与本地 `~/.cargo/credentials.toml`，不要提交进仓库。

## 注意事项

- **版本不可覆盖**：已发布的版本号不能重用，只能发新版本号。
- crates.io 对「新 crate」有约 1 个/10 分钟的限速；发新版本（update）不受此限，
  所以 CI 串行发布 + 每个之间 `sleep 5` 只是等索引同步，不需要额外退避。
- 主包名是 `gitcode-gc`（`gc` 已被占用），目录仍是 `crates/gc`。
