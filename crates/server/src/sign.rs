// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Signaturen: HMAC-SHA256 für eigene Links (Euro-Office-Dateiabruf, Freigabe-
//! Schlüssel) und JWT (HS256) für den Document Server.

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Lädt den Server-Schlüssel aus `secret.key` oder legt ihn neu an.
pub async fn load_or_create_secret(data_dir: &Path) -> anyhow::Result<Vec<u8>> {
    let path = data_dir.join("secret.key");
    if let Ok(s) = tokio::fs::read_to_string(&path).await {
        if let Ok(b) = hex::decode(s.trim()) {
            if b.len() >= 32 {
                return Ok(b);
            }
        }
    }
    let mut b = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    tokio::fs::write(&path, hex::encode(&b)).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(b)
}

fn mac(key: &[u8], msg: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC akzeptiert jede Schlüssellänge");
    m.update(msg);
    m
}

pub fn sign(key: &[u8], msg: &str) -> String {
    hex::encode(mac(key, msg.as_bytes()).finalize().into_bytes())
}

pub fn verify(key: &[u8], msg: &str, sig: &str) -> bool {
    match hex::decode(sig) {
        Ok(s) => mac(key, msg.as_bytes()).verify_slice(&s).is_ok(),
        Err(_) => false,
    }
}

pub fn jwt_encode(secret: &str, payload: &serde_json::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let body = URL_SAFE_NO_PAD.encode(payload.to_string());
    let input = format!("{header}.{body}");
    let sig = URL_SAFE_NO_PAD.encode(mac(secret.as_bytes(), input.as_bytes()).finalize().into_bytes());
    format!("{input}.{sig}")
}

/// Prüft ein HS256-JWT und liefert die Nutzdaten.
pub fn jwt_decode(secret: &str, token: &str) -> Option<serde_json::Value> {
    let mut parts = token.trim().split('.');
    let (h, b, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    if header.get("alg")?.as_str()? != "HS256" {
        return None;
    }
    let sig = URL_SAFE_NO_PAD.decode(s).ok()?;
    mac(secret.as_bytes(), format!("{h}.{b}").as_bytes()).verify_slice(&sig).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(b).ok()?).ok()?;
    if let Some(exp) = payload.get("exp").and_then(|v| v.as_i64()) {
        if exp < crate::util::now() {
            return None;
        }
    }
    Some(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let p = serde_json::json!({"a": 1, "b": "x"});
        let t = jwt_encode("geheim", &p);
        assert_eq!(jwt_decode("geheim", &t), Some(p));
        assert_eq!(jwt_decode("falsch", &t), None);
        let s = sign(b"k", "msg");
        assert!(verify(b"k", "msg", &s));
        assert!(!verify(b"k", "msg2", &s));
    }
}
