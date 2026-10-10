//! Where a pairing's token is kept: the system keyring, or a private file
//! in the data folder.

use std::path::PathBuf;

use anyhow::Context;

use super::{Config, Tokens};
use crate::keystore;

fn token_key(cfg: &Config, device: &str) -> String {
    format!("{}:{device}", cfg.world)
}

fn token_file(cfg: &Config, device: &str) -> PathBuf {
    cfg.dir.join(format!("{}-{device}.token", cfg.world))
}

pub(super) async fn save_token(cfg: &Config, device: &str, token: &str) -> anyhow::Result<()> {
    if cfg.tokens == Tokens::Keyring {
        let (key, token2) = (token_key(cfg, device), token.to_owned());
        let saved = tokio::task::spawn_blocking(move || keystore::save(&key, &token2)).await?;
        match saved {
            Ok(_) => return Ok(()),
            Err(why) => tracing::warn!(%why, "no keyring; keeping the token in a private file"),
        }
    }
    write_private(&token_file(cfg, device), token)
}

pub(super) async fn load_token(cfg: &Config, device: &str) -> Option<String> {
    if cfg.tokens == Tokens::Keyring {
        let key = token_key(cfg, device);
        if let Ok(Some(token)) = tokio::task::spawn_blocking(move || keystore::load(&key)).await {
            return Some(token);
        }
    }
    std::fs::read_to_string(token_file(cfg, device))
        .ok()
        .map(|t| t.trim().to_owned())
}

pub(super) async fn forget_token(cfg: &Config, device: &str) {
    if cfg.tokens == Tokens::Keyring {
        let key = token_key(cfg, device);
        let _ = tokio::task::spawn_blocking(move || keystore::forget(&key)).await;
    }
    let _ = std::fs::remove_file(token_file(cfg, device));
}

fn write_private(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}
