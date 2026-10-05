//! where people's notes live. everyone can advertise the servers they write to
//! and read from in a kind 10002 list; we look those up on a few well-known
//! directories and remember the ones for people we follow.

use std::collections::HashMap;
use std::path::PathBuf;

use ritualistic::{Event, Kind, PubKey, Tag};

pub const LIST: Kind = Kind(10002);

/// where server lists are looked up and announced. never shown to the user.
pub const DIRECTORIES: [&str; 4] = [
    "purplepag.es",
    "nos.lol",
    "relay.primal.net",
    "indexer.coracle.social",
];

/// how many of someone's servers we talk to.
pub const PER_PERSON: usize = 3;

/// recheck someone's server list once we haven't seen a note from them in this long.
pub const STALE: u32 = 7 * 86_400;

#[derive(Debug, Default, Clone)]
pub struct ServerList {
    /// where they publish ("outbox"): read their notes from here.
    pub write: Vec<String>,
    /// where they look for mentions ("inbox"): send notes tagging them here.
    pub read: Vec<String>,
}

impl ServerList {
    pub fn from_event(event: &Event) -> Self {
        let mut list = Self::default();
        for tag in &event.tags.0 {
            let (Some("r"), Some(url)) = (tag.first().map(String::as_str), tag.get(1)) else {
                continue;
            };
            let Some(url) = clean(url) else { continue };
            let marker = tag.get(2).map(String::as_str).unwrap_or("");
            if (marker.is_empty() || marker == "write") && !list.write.contains(&url) {
                list.write.push(url.clone());
            }
            if (marker.is_empty() || marker == "read") && !list.read.contains(&url) {
                list.read.push(url);
            }
        }
        list
    }
}

/// the newest server list per author, out of whatever the directories sent.
pub fn newest(events: Vec<Event>) -> HashMap<PubKey, ServerList> {
    let mut best: HashMap<PubKey, Event> = HashMap::new();
    for event in events.into_iter().filter(|e| e.kind == LIST) {
        match best.get(&event.pubkey) {
            Some(prev) if prev.created_at.0 >= event.created_at.0 => {}
            _ => {
                best.insert(event.pubkey, event);
            }
        }
    }
    best.into_iter()
        .map(|(pk, event)| (pk, ServerList::from_event(&event)))
        .collect()
}

/// a server url the way we write it down: normalized, without the trailing slash.
pub fn clean(url: &str) -> Option<String> {
    let url = ritualistic::normalize_url(url.trim()).ok()?.to_string();
    Some(url.trim_end_matches('/').to_string())
}

/// the first few, which is how people tend to order them: most important first.
pub fn pick(servers: &[String]) -> Vec<String> {
    servers.iter().take(PER_PERSON).cloned().collect()
}

/// whether everything we read from is still somewhere they say they publish to.
pub fn still_listed(ours: &[String], theirs: &[String]) -> bool {
    ours.iter()
        .all(|url| clean(url).is_some_and(|url| theirs.contains(&url)))
}

/// our own list: every server is both read and write, so no marker.
pub fn tags(servers: &[String]) -> Vec<Tag> {
    servers
        .iter()
        .filter_map(|url| clean(url))
        .map(|url| vec!["r".into(), url])
        .collect()
}

/// we announce our server list whenever it differs from the last one we
/// announced, which is remembered here, per key.
fn announced_path(pk: &PubKey) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("txstr")
        .join(format!("announced-{}", pk.to_hex()))
}

fn fingerprint(servers: &[String]) -> String {
    let mut urls: Vec<String> = servers.iter().filter_map(|url| clean(url)).collect();
    urls.sort();
    urls.join("\n")
}

pub fn already_announced(pk: &PubKey, servers: &[String]) -> bool {
    std::fs::read_to_string(announced_path(pk)).is_ok_and(|old| old == fingerprint(servers))
}

pub fn remember_announced(pk: &PubKey, servers: &[String]) {
    let path = announced_path(pk);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, fingerprint(servers));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritualistic::{EventTemplate, SecretKey, Tags, Timestamp};

    fn list(sk: &SecretKey, at: u32, tags: Vec<Tag>) -> Event {
        EventTemplate {
            created_at: Timestamp(at),
            kind: LIST,
            tags: Tags(tags),
            content: String::new(),
        }
        .finalize(sk)
    }

    fn r(url: &str, marker: Option<&str>) -> Tag {
        let mut tag = vec!["r".into(), url.into()];
        tag.extend(marker.map(String::from));
        tag
    }

    #[test]
    fn markers() {
        let sk = SecretKey::generate();
        let parsed = ServerList::from_event(&list(
            &sk,
            1,
            vec![
                r("wss://both.example/", None),
                r("wss://out.example", Some("write")),
                r("wss://in.example", Some("read")),
                r("not a url at all", None),
            ],
        ));
        assert_eq!(parsed.write, ["wss://both.example", "wss://out.example"]);
        assert_eq!(parsed.read, ["wss://both.example", "wss://in.example"]);
    }

    #[test]
    fn newest_wins() {
        let sk = SecretKey::generate();
        let lists = newest(vec![
            list(&sk, 2, vec![r("wss://new.example", None)]),
            list(&sk, 1, vec![r("wss://old.example", None)]),
        ]);
        assert_eq!(lists[&sk.pubkey()].write, ["wss://new.example"]);
    }

    #[test]
    fn comparing() {
        let theirs = vec!["wss://a.example".to_string(), "wss://b.example".into()];
        assert!(still_listed(&["wss://b.example/".into()], &theirs));
        assert!(!still_listed(&["wss://c.example".into()], &theirs));
        assert_eq!(
            tags(&["soloco.nl".into()]),
            vec![vec!["r".to_string(), "wss://soloco.nl".into()]]
        );
    }
}
