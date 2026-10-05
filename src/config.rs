use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ritualistic::{PubKey, SecretKey};
use serde::{Deserialize, Serialize};

pub const DEFAULT_SERVERS: [&str; 2] = ["wss://wheat.happytavern.co", "wss://soloco.nl"];

/// everything txstr knows about you lives in one small, hand-editable toml file.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub nick: String,

    /// nsec or hex. this file is created with 0600 permissions, keep it that way.
    pub secret_key: String,

    #[serde(default = "default_servers")]
    pub servers: Vec<String>,

    /// a server you run, managed with `txstr server`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_server: Option<String>,

    /// also publish your follow list as a kind 3 event whenever it changes.
    #[serde(default)]
    pub publish_follows: bool,

    #[serde(default = "default_limit")]
    pub limit_timeline: usize,

    /// warn before posting anything longer than this many characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_warning: Option<usize>,

    #[serde(default = "default_timeout")]
    pub timeout: u64,

    /// one table per nick. your follow list is yours: it stays in this file
    /// unless `publish_follows` is on.
    #[serde(default)]
    pub following: BTreeMap<String, Follow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "FollowEntry")]
pub struct Follow {
    pub npub: String,

    /// where we read their notes from, picked from the server list they
    /// publish. empty means we don't know yet and use yours.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<String>,

    /// when the newest note we've seen from them was posted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u32>,
}

impl Follow {
    pub fn new(npub: String, servers: Vec<String>) -> Self {
        Self {
            npub,
            servers,
            last_seen: None,
        }
    }
}

/// older configs had a bare `nick = "npub1…"` per follow, so accept both.
#[derive(Deserialize)]
#[serde(untagged)]
enum FollowEntry {
    Npub(String),
    Full {
        npub: String,
        #[serde(default)]
        servers: Vec<String>,
        #[serde(default)]
        last_seen: Option<u32>,
    },
}

impl From<FollowEntry> for Follow {
    fn from(entry: FollowEntry) -> Self {
        match entry {
            FollowEntry::Npub(npub) => Follow::new(npub, Vec::new()),
            FollowEntry::Full {
                npub,
                servers,
                last_seen,
            } => Follow {
                npub,
                servers,
                last_seen,
            },
        }
    }
}

fn default_servers() -> Vec<String> {
    DEFAULT_SERVERS.iter().map(|s| s.to_string()).collect()
}

fn default_limit() -> usize {
    20
}

fn default_timeout() -> u64 {
    10
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("txstr")
            .join("config.toml")
    }

    pub fn new(nick: String, secret_key: &SecretKey) -> Self {
        Self {
            nick,
            secret_key: secret_key.to_nsec(),
            servers: default_servers(),
            owned_server: None,
            publish_follows: false,
            limit_timeline: default_limit(),
            character_warning: Some(280),
            timeout: default_timeout(),
            following: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).with_context(|| {
            format!(
                "couldn't read {}; run `txstr quickstart` first",
                path.display()
            )
        })?;
        toml::from_str(&raw).with_context(|| format!("malformed config at {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let raw = toml::to_string_pretty(self)?;
        write_private(path, raw.as_bytes())
            .with_context(|| format!("couldn't write {}", path.display()))
    }

    pub fn secret_key(&self) -> Result<SecretKey> {
        self.secret_key
            .trim()
            .parse()
            .map_err(|err| anyhow::anyhow!("invalid secret_key in config: {err:?}"))
    }

    pub fn pubkey(&self) -> Result<PubKey> {
        Ok(self.secret_key()?.pubkey())
    }

    /// resolves a nick from the follow list, or parses an npub/nprofile/hex directly.
    pub fn resolve(&self, who: &str) -> Result<PubKey> {
        let who = who.trim_start_matches('@');
        if who == self.nick {
            return self.pubkey();
        }
        let raw = self
            .following
            .get(who)
            .map(|f| f.npub.as_str())
            .unwrap_or(who);
        match raw.parse() {
            Ok(pk) => Ok(pk),
            Err(_) if self.following.contains_key(who) => {
                bail!("'{who}' in your follow list has an invalid key: {raw}")
            }
            Err(_) => bail!("'{who}' is neither someone you follow nor a valid npub"),
        }
    }

    /// every (nick, pubkey) pair we know, including ourselves.
    pub fn known(&self) -> Vec<(String, PubKey)> {
        let mut known: Vec<(String, PubKey)> = self
            .following
            .iter()
            .filter_map(|(nick, follow)| Some((nick.clone(), follow.npub.parse().ok()?)))
            .collect();
        if let Ok(me) = self.pubkey() {
            known.push((self.nick.clone(), me));
        }
        known
    }

    /// the nick we follow `pk` under, if any.
    pub fn nick_of(&self, pk: &PubKey) -> Option<String> {
        self.following
            .iter()
            .find(|(_, follow)| follow.npub.parse::<PubKey>().ok().as_ref() == Some(pk))
            .map(|(nick, _)| nick.clone())
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // mode() only applies on creation, so tighten pre-existing files too
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_new_follows() {
        let config: Config = toml::from_str(
            r#"
            nick = "ghost"
            secret_key = "00"

            [following]
            ada = "npub1old"

            [following.ken]
            npub = "npub1new"
            servers = ["wss://a.example"]
            last_seen = 1700000000
            "#,
        )
        .unwrap();
        assert_eq!(config.following["ada"].npub, "npub1old");
        assert!(config.following["ada"].servers.is_empty());
        assert_eq!(config.following["ken"].servers, ["wss://a.example"]);
        assert_eq!(config.following["ken"].last_seen, Some(1700000000));

        let again: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(again.following["ken"].last_seen, Some(1700000000));
    }
}
