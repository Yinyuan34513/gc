#!/usr/bin/env bash
# gc 发版脚本：bump 版本 → 全量 sanity 检查 → 推送双远端 → 打 tag
# （tag 推送到 github 即触发 GitHub Actions 自动发布 crates.io，见 docs/release.md）
#
# 用法：
#   scripts/release.sh 0.1.5            # 正式发布
#   scripts/release.sh 0.1.5 --dry-run  # 只跑检查，不改动任何东西
#
# 前置检查覆盖（任何一条不过即中止，且此时尚未做任何改动）：
#   - 工作区干净、双远端 main 与本地 HEAD 同步
#   - tag 未被本地/远端占用（版本不可覆盖）
#   - cargo fmt --check 与 cargo test 全绿
#   - bump 后二进制 --version 与目标一致
#   - 推送后再次核对远端 main 的 SHA（防 push 静默失败——踩过：
#     push 失败会让 tag 推不上去、CI 不触发，而表面看不出异常）

set -euo pipefail

VERSION=""
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *)
      if [ -n "$VERSION" ]; then
        echo "错误：只能指定一个版本号（收到第二个: $arg）" >&2
        exit 1
      fi
      VERSION="$arg"
      ;;
  esac
done

if [ -z "$VERSION" ]; then
  echo "用法: $0 <版本号，如 0.1.5> [--dry-run]" >&2
  exit 1
fi
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "错误：版本号需为 X.Y.Z 格式，收到: $VERSION" >&2
  exit 1
fi

cd "$(git rev-parse --show-toplevel)"

step() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
die() { printf '\033[31m错误: %s\033[0m\n' "$1" >&2; exit 1; }

# ---------- 1. 前置 sanity 检查（不动任何东西） ----------
step "前置检查"

[ -z "$(git status --porcelain)" ] || die "工作区有未提交改动，先 commit 或 stash"
echo "✓ 工作区干净"

git fetch -q origin github
for remote in origin github; do
  git rev-parse --verify "$remote/main" >/dev/null 2>&1 ||
    die "远端 $remote 不存在（git remote -v 查看）"
  if [ "$(git rev-parse "$remote/main")" != "$(git rev-parse HEAD)" ]; then
    die "$remote/main 与本地 HEAD 不一致——先 git push $remote main 再发版"
  fi
  echo "✓ $remote/main 与本地同步"
done

if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  die "本地已存在 tag v$VERSION"
fi
if git ls-remote --exit-code --tags github "refs/tags/v$VERSION" >/dev/null 2>&1; then
  die "GitHub 已存在 tag v$VERSION——已发布版本不可覆盖，只能换新版本号"
fi
echo "✓ tag v$VERSION 未被占用"

cargo fmt -- --check || die "cargo fmt --check 未过，先 cargo fmt"
echo "✓ 格式检查通过"

cargo test --workspace -q || die "测试未过"
echo "✓ 测试通过"

CURRENT=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
if [ "$CURRENT" = "$VERSION" ]; then
  die "Cargo.toml 已是 $VERSION——若确需重发请先确认 tag 从未推送"
fi
echo "✓ 当前版本 $CURRENT → 目标 $VERSION"

if [ "$DRY_RUN" = "1" ]; then
  step "dry-run 完成：所有检查通过，未做任何改动"
  exit 0
fi

# ---------- 2. bump 版本（workspace.package + 9 个内部依赖，共 10 处） ----------
step "bump 版本到 $VERSION"
sed -i "s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml
sed -i "s/version = \"$CURRENT\" }/version = \"$VERSION\" }/g" Cargo.toml
LEFT=$(grep -c "\"$CURRENT\"" Cargo.toml || true)
[ "$LEFT" = "0" ] || die "Cargo.toml 还剩 $LEFT 处旧版本 $CURRENT，请人工检查"
COUNT=$(grep -c "\"$VERSION\"" Cargo.toml)
[ "$COUNT" -ge 10 ] || die "只替换了 $COUNT 处版本（应 ≥10）"
echo "✓ 替换 $COUNT 处"

# ---------- 3. 构建与版本校验 ----------
step "构建与版本校验"
cargo build -q
BIN_VER=$(./target/debug/gc --version | awk '{print $2}')
[ "$BIN_VER" = "$VERSION" ] || die "二进制版本 $BIN_VER != 目标 $VERSION"
echo "✓ gc --version = $BIN_VER"

# ---------- 4. 提交并推送双远端 ----------
step "提交并推送"
git add Cargo.toml Cargo.lock
git commit -q -m "chore: bump $VERSION"
git push -q origin main
git push -q github main
# 推送后再核对一次远端真实 SHA（防静默失败）
NEW_HEAD=$(git rev-parse HEAD)
for remote in origin github; do
  REMOTE_SHA=$(git ls-remote "$remote" refs/heads/main | cut -f1)
  if [ "$REMOTE_SHA" != "$NEW_HEAD" ]; then
    die "推送后 $remote/main（$REMOTE_SHA）≠ HEAD（$NEW_HEAD）——push 可能静默失败。tag 尚未创建，可手动 push 后重跑本脚本"
  fi
  echo "✓ $remote/main 已更新到 $NEW_HEAD"
done

# ---------- 5. 打 tag 并推送（触发 CI 自动发布） ----------
step "打 tag v$VERSION 并推送（触发自动发布）"
git tag "v$VERSION"
git push -q github "v$VERSION"
git push -q origin "v$VERSION"
echo "✓ tag v$VERSION 已推送到 github / origin"
echo
echo "CI 已触发。查看: https://github.com/Yinyuan34513/gc/actions"
RUN_ID=$(gh run list --repo Yinyuan34513/gc --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)
if [ -n "$RUN_ID" ]; then
  echo "跟进: gh run watch --repo Yinyuan34513/gc $RUN_ID"
fi
