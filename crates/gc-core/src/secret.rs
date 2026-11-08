// SPDX-License-Identifier: MIT
//! 敏感令牌的落盘加密层（AES-256-GCM）。
//!
//! `gitcode.toml` 里的 PAT / 网页端 JWT 以密文形式存储，格式：
//!
//! ```text
//! gcm1:<base64(nonce(12B) || ciphertext)>
//! ```
//!
//! 密钥来源（优先级从高到低）：
//! 1. 环境变量 `GC_TOKEN_KEY`：64 位 hex（32 字节），供 CI / 脚本场景注入；
//! 2. 密钥文件 `~/.config/gc/.secret-key`（32 字节随机数，0600），首次使用自动生成。
//!
//! 解密失败（密钥文件丢失 / 被篡改 / 密文损坏）直接报错——宁可让用户重新
//! `gc token set`，也绝不静默把错误数据当 token 用。环境变量 `GC_TOKEN_KEY`
//! 用于测试与 CI 注入确定密钥，避免测试污染真实密钥文件。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Context as _, Result};
use base64::Engine as _;
use std::path::PathBuf;

/// 密文前缀：带此前缀的字段视为 GCM 密文，否则原样透传（明文兼容）。
const PREFIX: &str = "gcm1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 密钥文件路径：`~/.config/gc/.secret-key`（可用 GC_SECRET_KEY_FILE 覆盖，测试用）。
fn key_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("GC_SECRET_KEY_FILE") {
        return Ok(PathBuf::from(p));
    }
    let dir = dirs::config_dir()
        .context("无法定位用户配置目录")?
        .join("gc");
    std::fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    Ok(dir.join(".secret-key"))
}

/// 加载（或首次生成）32 字节密钥。优先环境变量 `GC_TOKEN_KEY`（64 位 hex）。
fn ensure_key() -> Result<[u8; KEY_LEN]> {
    // 测试构建：统一用确定性密钥。否则并行测试会互相踩——
    // A 测试刚用密钥文件加密，B 测试设置 GC_TOKEN_KEY 后 A 再解密就失败。
    #[cfg(test)]
    {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| std::env::set_var("GC_TOKEN_KEY", "11".repeat(32)));
    }
    if let Ok(hex) = std::env::var("GC_TOKEN_KEY") {
        let hex = hex.trim();
        let bytes = hex::decode(hex)
            .map_err(|e| anyhow::anyhow!("GC_TOKEN_KEY 不是合法 hex: {e}"))?;
        if bytes.len() != KEY_LEN {
            anyhow::bail!("GC_TOKEN_KEY 需要 {KEY_LEN} 字节（64 位 hex），实际 {} 字节", bytes.len());
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    let path = key_path()?;
    if let Ok(data) = std::fs::read(&path) {
        if data.len() == KEY_LEN {
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&data);
            return Ok(key);
        }
        anyhow::bail!(
            "密钥文件 {} 长度异常（{} 字节，应为 {KEY_LEN}）：删除它可重新生成，但旧密文将无法解密",
            path.display(),
            data.len()
        );
    }
    // 首次生成：OS 熵源随机 32 字节，0600 权限
    let mut key = [0u8; KEY_LEN];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut key);
    std::fs::write(&path, key)
        .with_context(|| format!("写入密钥文件 {} 失败", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("收紧密钥文件权限失败: {}", path.display()))?;
    }
    Ok(key)
}

/// 加密明文（已是密文则原样返回，避免二次包裹）。
pub fn protect(plain: &str) -> Result<String> {
    if plain.starts_with(PREFIX) || plain.is_empty() {
        return Ok(plain.to_string());
    }
    let key = ensure_key()?;
    let cipher = Aes256Gcm::new((&key).into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, Payload { msg: plain.as_bytes(), aad: b"gc-token" })
        .map_err(|_| anyhow::anyhow!("AES-256-GCM 加密失败"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ct);
    Ok(format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(blob)))
}

/// 解密密文；无前缀的明文原样透传（兼容旧配置，下次落盘时自动加密）。
pub fn unprotect(stored: &str) -> Result<String> {
    let stored = stored.trim();
    if !stored.starts_with(PREFIX) {
        return Ok(stored.to_string());
    }
    let b64 = &stored[PREFIX.len()..];
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("token 密文 base64 解码失败: {e}"))?;
    if blob.len() <= NONCE_LEN {
        anyhow::bail!("token 密文过短，已损坏");
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let key = ensure_key()?;
    let cipher = Aes256Gcm::new((&key).into());
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: b"gc-token" })
        .map_err(|_| {
            anyhow::anyhow!(
                "token 解密失败（密钥文件丢失/被篡改，或密文损坏）：\
                 删除 {} 后重新 gc token set 即可恢复",
                key_path().map(|p| p.display().to_string()).unwrap_or_default()
            )
        })?;
    String::from_utf8(plain).context("token 解密结果不是合法 UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_test_key() {
        std::env::set_var("GC_TOKEN_KEY", &"11".repeat(32));
    }

    #[test]
    fn roundtrip_and_prefix_idempotent() {
        set_test_key();
        let enc = protect("pat-abc123").unwrap();
        assert!(enc.starts_with(PREFIX));
        assert_ne!(enc, "pat-abc123");
        assert_eq!(unprotect(&enc).unwrap(), "pat-abc123");
        // 已是密文再保护：原样返回
        assert_eq!(protect(&enc).unwrap(), enc);
    }

    #[test]
    fn plaintext_passthrough_for_legacy() {
        assert_eq!(unprotect("plain-legacy-token").unwrap(), "plain-legacy-token");
        assert_eq!(unprotect("  spaced  ").unwrap(), "spaced");
    }

    #[test]
    fn tamper_detected() {
        set_test_key();
        let mut enc = protect("pat-abc123").unwrap();
        // 篡改密文末字符（base64 字母表内循环）
        let last = enc.pop().unwrap();
        enc.push(if last == 'A' { 'B' } else { 'A' });
        assert!(unprotect(&enc).is_err(), "被篡改的密文必须解密失败");
    }

    #[test]
    fn nonce_uniqueness_same_plaintext_different_ciphertext() {
        set_test_key();
        let a = protect("same").unwrap();
        let b = protect("same").unwrap();
        assert_ne!(a, b, "随机 nonce 保证同明文异密文");
        assert_eq!(unprotect(&a).unwrap(), "same");
        assert_eq!(unprotect(&b).unwrap(), "same");
    }
}
