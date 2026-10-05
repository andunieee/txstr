//! short hashes, git style. servers only look notes up by their full id, so we
//! remember every id we've printed and expand prefixes against that list. next
//! to each id goes the author of its thread's root, when known: their inbox
//! servers are where the conversation lives.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};
use ritualistic::{ID, PubKey};

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

/// a note we've shown, and who started the thread it's in.
pub type Seen = (ID, Option<PubKey>);

/// appends ids to the cache. failing to remember is never worth failing a command over.
pub fn remember(ids: impl IntoIterator<Item = Seen>) {
    let _ = try_remember(ids);
}

fn try_remember(ids: impl IntoIterator<Item = Seen>) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut lines: String = ids
        .into_iter()
        .map(|(id, root_author)| match root_author {
            Some(pk) => format!("{} {}\n", id.to_hex(), pk.to_hex()),
            None => id.to_hex() + "\n",
        })
        .collect();
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

/// turns a short hash, full hex id, note1… or nevent1… into an event id,
/// along with its thread's root author if we've seen it before.
pub fn resolve(reference: &str) -> Result<Seen> {
    let reference = reference.trim_matches(|c| c == '[' || c == ']' || c == '#');
    let known = std::fs::read_to_string(path()).unwrap_or_default();
    let full =
        ID::from_hex(reference)
            .ok()
            .or_else(|| match ritualistic::codes::decode(reference) {
                Ok(ritualistic::codes::DecodeResult::Event(pointer)) => Some(pointer.id),
                _ => None,
            });
    if let Some(id) = full {
        let root_author = expand(&id.to_hex(), &known).first().and_then(|(_, pk)| *pk);
        return Ok((id, root_author));
    }

    let prefix = reference.to_ascii_lowercase();
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{reference}' is not a note hash, hex id, note or nevent");
    }
    let found = expand(&prefix, &known);
    match found.as_slice() {
        [seen] => Ok(*seen),
        [] => bail!(
            "haven't seen any note starting with '{prefix}'. read it with timeline/view first, or use its full id"
        ),
        _ => bail!(
            "'{prefix}' is ambiguous, {} notes start with it. use more characters",
            found.len()
        ),
    }
}

fn expand(prefix: &str, known: &str) -> Vec<Seen> {
    let mut found: Vec<Seen> = Vec::new();
    for line in known.lines() {
        let mut words = line.split_whitespace();
        let Some(Ok(id)) = words
            .next()
            .filter(|hex| hex.starts_with(prefix))
            .map(ID::from_hex)
        else {
            continue;
        };
        let root_author = words.next().and_then(|hex| hex.parse().ok());
        match found.iter_mut().find(|(seen, _)| *seen == id) {
            Some(seen) => seen.1 = seen.1.or(root_author),
            None => found.push((id, root_author)),
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
        let pk = ritualistic::SecretKey::generate().pubkey();
        let known = format!("{a}\n{b}\n{a} {}\n", pk.to_hex());
        assert_eq!(
            expand("a1b2c", &known),
            vec![(ID::from_hex(&a).unwrap(), Some(pk))]
        );
        assert_eq!(expand("a1b2", &known).len(), 2);
        assert!(expand("ffff", &known).is_empty());
    }
}
