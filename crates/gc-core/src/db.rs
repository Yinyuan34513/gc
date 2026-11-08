// SPDX-License-Identifier: MIT
//! gc 的 SQLite 存储层：统一放在 gc 配置目录下的 `gc.db`。
//!
//! 三张表：
//! - `kv`              —— 通用键值（web token、登录元信息等）
//! - `search_cache`    —— 全量搜索的聚合结果缓存（按 查询词+类型+页数上限 键）
//! - `cookies`         —— 外置登录脚本回传的 Cookie（gc 自动去重合并写入）
//!
//! 全部走 WAL + busy_timeout，允许多个 gc 进程并发读写。

use anyhow::{Context as _, Result};
use rusqlite::Connection;
use serde_json::Value;

/// gc 数据库路径：`~/.config/gc/gc.db`（与 config.toml 同目录）。
pub fn db_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir()
        .context("无法定位用户配置目录")?
        .join("gc");
    std::fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    Ok(dir.join("gc.db"))
}

fn open() -> Result<Connection> {
    let path = db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("打开 SQLite 数据库 {} 失败", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS kv (
             key        TEXT PRIMARY KEY,
             value      TEXT NOT NULL,
             updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
         );
         CREATE TABLE IF NOT EXISTS search_cache (
             cache_key  TEXT PRIMARY KEY,
             kind       TEXT NOT NULL,
             query      TEXT NOT NULL,
             pages      INTEGER NOT NULL,
             payload    TEXT NOT NULL,
             updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
         );
         CREATE TABLE IF NOT EXISTS cookies (
             name       TEXT NOT NULL,
             domain     TEXT NOT NULL,
             value      TEXT NOT NULL,
             updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
             PRIMARY KEY (name, domain)
         );",
    )?;
    Ok(conn)
}

// ---- kv ----------------------------------------------------------------------

pub fn kv_set(key: &str, value: &str) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2,
             updated_at = datetime('now','localtime')",
        [key, value],
    )?;
    Ok(())
}

pub fn kv_get(key: &str) -> Result<Option<String>> {
    let conn = open()?;
    let mut stmt = conn.prepare("SELECT value FROM kv WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(rows.next()?.and_then(|r| r.get::<_, String>(0).ok()))
}

/// 按前缀列举 kv（返回 (去掉前缀的后缀, value)，按后缀排序）。
pub fn kv_list(prefix: &str) -> Result<Vec<(String, String)>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT key, value FROM kv WHERE key LIKE ?1 || '%' ORDER BY key",
    )?;
    let rows = stmt.query_map([prefix], |r| {
        let k: String = r.get(0)?;
        let v: String = r.get(1)?;
        Ok((k.strip_prefix(prefix).unwrap_or(&k).to_string(), v))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ---- 搜索缓存 ------------------------------------------------------------------

fn cache_key(query: &str, kind: &str, per_page: u32, max_pages: Option<u32>) -> String {
    format!("{}|{}|{}|{:?}", kind, query, per_page, max_pages)
}

/// 读缓存（TTL 秒内有效；ttl 传 0 表示永不过期）。
pub fn search_cache_get(
    query: &str,
    kind: &str,
    per_page: u32,
    max_pages: Option<u32>,
    ttl_secs: u64,
) -> Result<Option<Value>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT payload, updated_at FROM search_cache WHERE cache_key = ?1",
    )?;
    let mut rows = stmt.query([cache_key(query, kind, per_page, max_pages)])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let payload: String = row.get(0)?;
    let updated_at: String = row.get(1)?;
    if ttl_secs > 0 {
        // updated_at 是 localtime 字符串，用 SQLite 自己算更稳（本地时区语义一致）
        let age: f64 = conn.query_row(
            "SELECT CAST(julianday('now','localtime') - julianday(?1) AS REAL) * 86400.0",
            [&updated_at],
            |r| r.get(0),
        )?;
        if age > ttl_secs as f64 {
            return Ok(None);
        }
    }
    Ok(serde_json::from_str(&payload).ok())
}

pub fn search_cache_put(
    query: &str,
    kind: &str,
    per_page: u32,
    max_pages: Option<u32>,
    pages: u32,
    digest: &Value,
) -> Result<()> {
    let conn = open()?;
    conn.execute(
        "INSERT INTO search_cache(cache_key, kind, query, pages, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(cache_key) DO UPDATE SET pages = ?4, payload = ?5,
             updated_at = datetime('now','localtime')",
        rusqlite::params![
            cache_key(query, kind, per_page, max_pages),
            kind,
            query,
            pages,
            digest.to_string()
        ],
    )?;
    Ok(())
}

// ---- cookies（外置登录脚本回传，去重合并）----------------------------------------

/// 合并写入一批 Cookie：以 (name, domain) 为主键，新值直接覆盖旧值。
/// 返回 (写入条数, 覆盖旧值条数)。
pub fn cookie_merge(cookies: &[Value]) -> Result<(usize, usize)> {
    let conn = open()?;
    let mut written = 0usize;
    let mut replaced = 0usize;
    for c in cookies {
        let (Some(name), Some(domain)) =
            (c["name"].as_str(), c["domain"].as_str())
        else {
            continue;
        };
        let value = c["value"].as_str().unwrap_or("");
        if value.is_empty() {
            continue; // 空值 Cookie 不入库（清除语义另行处理）
        }
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cookies WHERE name = ?1 AND domain = ?2
                           AND value != ?3)",
            [name, domain, value],
            |r| r.get(0),
        )?;
        if exists {
            replaced += 1;
        } else if conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cookies WHERE name = ?1 AND domain = ?2
                           AND value = ?3)",
            [name, domain, value],
            |r| r.get(0),
        )? {
            // 同名同域同值：脚本重复回传，跳过
            continue;
        }
        conn.execute(
            "INSERT INTO cookies(name, domain, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(name, domain) DO UPDATE SET value = ?3,
                 updated_at = datetime('now','localtime')",
            [name, domain, value],
        )?;
        written += 1;
    }
    Ok((written, replaced))
}

/// 读取某个 domain 下全部 Cookie，输出为浏览器 Cookie JSON 数组。
pub fn cookies_for_domain(domain_suffix: &str) -> Result<Vec<Value>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT name, domain, value, updated_at FROM cookies
         WHERE ?1 = '' OR domain LIKE '%' || ?1
         ORDER BY domain, name",
    )?;
    let rows = stmt.query_map([domain_suffix], |r| {
        Ok(serde_json::json!({
            "name": r.get::<_, String>(0)?,
            "domain": r.get::<_, String>(1)?,
            "value": r.get::<_, String>(2)?,
            "updated_at": r.get::<_, String>(3)?,
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}
