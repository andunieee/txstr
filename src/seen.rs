//! short hashes, git style. servers only look notes up by their full id, so we
//! remember every id we've printed and expand prefixes against that list.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};
use ritualistic::ID;

/// how many hex characters of an id we print.
pub const SHORT: usize = 7;

/// once the file grows past this many ids, keep only the newest half.
const KEEP: usize = 10_000;

pub fn short(id: &ID) -> String {
    id.to_hex()[..SHORT].to_string()
}

fn path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("txstr")
        .join("seen")
}

/// appends ids to the cache. failing to remember is never worth failing a command over.
pub fn remember(ids: impl IntoIterator<Item = ID>) {
    let _ = try_remember(ids);
}

fn try_remember(ids: impl IntoIterator<Item = ID>) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut lines: String = ids.into_iter().map(|id| id.to_hex() + "\n").collect();
    if lines.is_empty() {
        return Ok(());
    }

    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let count = old.lines().count();
    if count >= KEEP {
        let tail: Vec<&str> = old.lines().skip(count - KEEP / 2).collect();
        lines = tail.join("\n") + "\n" + &lines;
        return std::fs::write(&path, lines);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(lines.as_bytes())
}

/// turns a short hash, full hex id, note1… or nevent1… into an event id.
pub fn resolve(reference: &str) -> Result<ID> {
    let reference = reference.trim_matches(|c| c == '[' || c == ']' || c == '#');
    if let Ok(id) = ID::from_hex(reference) {
        return Ok(id);
    }
    if let Ok(ritualistic::codes::DecodeResult::Event(pointer)) =
        ritualistic::codes::decode(reference)
    {
        return Ok(pointer.id);
    }

    let prefix = reference.to_ascii_lowercase();
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{reference}' is not a note hash, hex id, note or nevent");
    }
    let known = std::fs::read_to_string(path()).unwrap_or_default();
    let found = expand(&prefix, &known);
    match found.as_slice() {
        [id] => Ok(*id),
        [] => bail!(
            "haven't seen any note starting with '{prefix}'. read it with timeline/view first, or use its full id"
        ),
        _ => bail!(
            "'{prefix}' is ambiguous, {} notes start with it. use more characters",
            found.len()
        ),
    }
}

fn expand(prefix: &str, known: &str) -> Vec<ID> {
    let mut found: Vec<ID> = Vec::new();
    for line in known.lines() {
        if line.starts_with(prefix)
            && let Ok(id) = ID::from_hex(line)
            && !found.contains(&id)
        {
            found.push(id);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        let a = "a1b2c3d".to_string() + &"0".repeat(57);
        let b = "a1b2fff".to_string() + &"0".repeat(57);
        let known = format!("{a}\n{b}\n{a}\n");
        assert_eq!(expand("a1b2c", &known), vec![ID::from_hex(&a).unwrap()]);
        assert_eq!(expand("a1b2", &known).len(), 2);
        assert!(expand("ffff", &known).is_empty());
    }
}
