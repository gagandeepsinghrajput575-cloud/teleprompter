//! API key retrieval.
//!
//! Keys are read from the environment first (development convenience) and from the
//! macOS Keychain otherwise. They are never written to disk by this application and
//! never included in logs — only a masked form is ever printed.

use std::process::Command;

const KEYCHAIN_ACCOUNT: &str = "stealthprompter";

/// Reads a secret by name, e.g. `DEEPGRAM_API_KEY`.
pub fn get(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    keychain_get(name)
}

fn keychain_get(name: &str) -> Option<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            name,
            "-w",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let secret = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if secret.is_empty() {
        None
    } else {
        Some(secret)
    }
}

/// Describes a secret without revealing it: enough to tell two keys apart in a log.
pub fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return format!("len={n}");
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[n - 4..].iter().collect();
    format!("{head}…{tail} (len={n})")
}

/// A key plus whether it was found, safe to send to the UI.
pub fn status(name: &str) -> KeyStatus {
    match get(name) {
        Some(secret) => KeyStatus {
            name: name.to_string(),
            present: true,
            masked: mask(&secret),
        },
        None => KeyStatus {
            name: name.to_string(),
            present: false,
            masked: String::new(),
        },
    }
}

#[derive(serde::Serialize)]
pub struct KeyStatus {
    pub name: String,
    pub present: bool,
    pub masked: String,
}

#[cfg(test)]
mod tests {
    use super::mask;

    #[test]
    fn mask_never_reveals_the_middle() {
        // Synthetic value with the same shape as a real key. Never use a live credential
        // in a test: this repo is public.
        let fake = "sk_0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijwxyz";
        let masked = mask(fake);
        assert!(masked.starts_with("sk_0"), "got {masked}");
        assert!(masked.contains("wxyz"), "got {masked}");
        assert!(!masked.contains("abcdefghij"), "middle leaked: {masked}");
        assert_ne!(masked, fake);
    }

    #[test]
    fn short_secrets_report_only_length() {
        assert_eq!(mask("abc123"), "len=6");
    }
}
