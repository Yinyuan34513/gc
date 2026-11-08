#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""gc_web_login.py — gc 的外置浏览器登录脚本（Playwright）。

被 gc 通过 PTY 拉起。流程：
1. 打开 GitCode 登录页（可用环境变量 GC_LOGIN_URL 覆盖，默认 gitcode.com/login，
   该页支持 AtomGit 扫码/账号登录）；
2. 挂响应钩子抓取登录成功请求（access_token / oauth / sso 相关响应）；
3. 轮询 Cookie，检测到 GITCODE_ACCESS_TOKEN 即判定登录成功；
4. 立刻刷新页面，并在刷新后的短时间内快速采样 Cookie 签名，
   连续多次一致即视为稳定（新签发的会话 Cookie 此时仍在滚动）；
5. 把全量 Cookie 以 JSON 输出到 stdout（用哨兵标记包裹，日志全走 stderr）。

输出协议（stdout）：
  ...普通进度日志（stderr）...
  ---GC_COOKIES_BEGIN---
  {"cookies":[...], "captured_from":"...", "page_url":"..."}
  ---GC_COOKIES_END---
gc 读取两个哨兵行之间的 JSON，去重合并写入 SQLite。
"""

import json
import os
import sys
import time

from playwright.sync_api import sync_playwright

LOGIN_URL = os.environ.get("GC_LOGIN_URL", "https://gitcode.com/login")
ACCESS_COOKIE = "GITCODE_ACCESS_TOKEN"
REFRESH_COOKIE = "GITCODE_REFRESH_TOKEN"
STABLE_NEED = 3          # 连续 N 次签名一致才算稳定
STABLE_INTERVAL = 0.4    # 采样间隔（秒）
LOGIN_WAIT = 300         # 等人工登录的最长秒数
FAST_GRAB_WINDOW = 30    # 刷新后快速抓取窗口（秒）

BEGIN = "---GC_COOKIES_BEGIN---"
END = "---GC_COOKIES_END---"


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def cookie_sig(cookies) -> str:
    """按名字排序拼签名，滚动中的 Cookie 会得到不同签名。"""
    return "|".join(
        f"{c['name']}={c['value']}"
        for c in sorted(cookies, key=lambda c: (c.get("domain", ""), c["name"]))
    )


def find_login_cookie(cookies):
    for c in cookies:
        if c.get("name") in (ACCESS_COOKIE, REFRESH_COOKIE) and c.get("value"):
            return c
    return None


def main() -> int:
    log(f"[gc-login] 打开登录页: {LOGIN_URL}")
    captured_from = ""
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=False)
        context = browser.new_context()
        page = context.new_page()

        # 挂响应钩子：抓登录成功请求（access_token / oauth / sso）
        hits = []

        def on_response(resp):
            url = resp.url
            if any(k in url for k in (
                    "access_token", "oauth", "sso", "login/callback",
                    "session")) and resp.status < 400:
                hits.append(url)

        page.on("response", on_response)

        page.goto(LOGIN_URL, wait_until="domcontentloaded", timeout=60000)
        log("[gc-login] 请在浏览器里完成登录（扫码 / AtomGit 账号）。")

        # 1) 等人工登录：轮询登录态 Cookie
        deadline = time.time() + LOGIN_WAIT
        login_cookie = None
        while time.time() < deadline:
            cookies = context.cookies()
            login_cookie = find_login_cookie(cookies)
            if login_cookie:
                log(f"[gc-login] ✓ 检测到登录 Cookie: {login_cookie['name']}（值已隐藏）")
                break
            time.sleep(0.5)
        if not login_cookie:
            log("[gc-login] 等待超时：未检测到登录态 Cookie，退出。")
            browser.close()
            return 1

        # 2) 刷新页面，触发服务端签发完整会话 Cookie
        log("[gc-login] 登录成功，刷新页面并快速抓取稳定 Cookie ...")
        try:
            page.reload(wait_until="domcontentloaded", timeout=30000)
        except Exception as e:
            log(f"[gc-login] 刷新提示（继续采样）: {e}")

        # 3) 快速采样：等签名稳定
        deadline = time.time() + FAST_GRAB_WINDOW
        sig, stable, latest = "", 0, context.cookies()
        while time.time() < deadline and stable < STABLE_NEED:
            time.sleep(STABLE_INTERVAL)
            cur = context.cookies()
            if not find_login_cookie(cur):
                log("[gc-login] 采样中登录态消失，放弃。")
                browser.close()
                return 1
            latest = cur
            cur_sig = cookie_sig(cur)
            if cur_sig == sig:
                stable += 1
            else:
                sig, stable = cur_sig, 0
        if stable < STABLE_NEED:
            log("[gc-login] 观察窗口内 Cookie 未完全稳定，输出当前快照（可能略偏早）。")

        # 4) 抓到的登录成功请求（供 gc 日志参考）
        captured_from = "; ".join(hits[-3:]) if hits else login_cookie["name"]
        log(f"[gc-login] 登录成功请求线索: {captured_from}")

        # 5) 全量输出（哨兵包裹，stdout 只放这一段）
        out = {
            "cookies": [
                {"name": c["name"], "value": c["value"],
                 "domain": c.get("domain", ""), "path": c.get("path", "/"),
                 "expires": c.get("expires", -1),
                 "http_only": c.get("httpOnly", False),
                 "secure": c.get("secure", False)}
                for c in latest
            ],
            "captured_from": captured_from,
            "page_url": page.url,
        }
        browser.close()
        print(BEGIN)
        print(json.dumps(out, ensure_ascii=False))
        print(END)
    log(f"[gc-login] 完成：共输出 {len(out['cookies'])} 条 Cookie。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
