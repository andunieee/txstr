//! turning `@nick` into `nostr:npub…` on the way out, and back again on the way in,
//! and working out where a note sits in a thread.

use ritualistic::{ID, PubKey, Tag};

/// expands `@nick` for people in your follow list into nip-27 references,
/// returning the new content and the `p` tags to go with it.
pub fn expand_mentions(content: &str, known: &[(String, PubKey)]) -> (String, Vec<Tag>) {
    let mut out = String::with_capacity(content.len());
    let mut tags: Vec<Tag> = Vec::new();
    let mut rest = content;

    while let Some(at) = rest.find('@') {
        let preceded_by_word = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let len = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.'))
            .unwrap_or(after.len());
        // a trailing dot is punctuation, not part of the nick
        let nick = after[..len].trim_end_matches('.');

        match known.iter().find(|(n, _)| n == nick) {
            Some((_, pk)) if !preceded_by_word && !nick.is_empty() => {
                out.push_str("nostr:");
                out.push_str(&pk.to_npub());
                let hex = pk.to_hex();
                if !tags.iter().any(|t| t.get(1) == Some(&hex)) {
                    tags.push(vec!["p".into(), hex]);
                }
                rest = &after[nick.len()..];
            }
            _ => {
                out.push('@');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, tags)
}

/// renders `nostr:npub…`/`nostr:nprofile…` references as `@nick` when we know them.
pub fn collapse_mentions(content: &str, known: &[(String, PubKey)]) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find("nostr:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "nostr:".len()..];
        let len = after
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        let code = &after[..len];

        match code.parse::<PubKey>() {
            Ok(pk) => {
                out.push('@');
                out.push_str(&display_name(&pk, known));
            }
            Err(_) => out.push_str(&rest[start..start + "nostr:".len() + len]),
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

pub fn display_name(pk: &PubKey, known: &[(String, PubKey)]) -> String {
    known
        .iter()
        .find(|(_, k)| k == pk)
        .map(|(nick, _)| nick.clone())
        .unwrap_or_else(|| short_npub(pk))
}

pub fn short_npub(pk: &PubKey) -> String {
    let npub = pk.to_npub();
    format!("{}…{}", &npub[..9], &npub[npub.len() - 4..])
}

/// the (root, parent) a note replies to, per nip-10. marked `e` tags win; older
/// clients just list them in order, first being the root and last the parent.
pub fn thread_refs(tags: &[Tag]) -> (Option<ID>, Option<ID>) {
    let e_tags: Vec<&Tag> = tags
        .iter()
        .filter(|t| t.first().map(String::as_str) == Some("e") && t.len() >= 2)
        .collect();
    let id = |t: &Tag| ID::from_hex(&t[1]).ok();
    let marked = |marker: &str| {
        e_tags
            .iter()
            .find(|t| t.get(3).map(String::as_str) == Some(marker))
            .and_then(|t| id(t))
    };

    let (root, parent) = (marked("root"), marked("reply"));
    if root.is_some() || parent.is_some() {
        // a reply straight to the root only carries the root marker
        return (root.or(parent), parent.or(root));
    }
    // positional: skip "mention" tags, which aren't part of the thread
    let positional: Vec<ID> = e_tags
        .iter()
        .filter(|t| t.get(3).is_none_or(|m| m.is_empty()))
        .filter_map(|t| id(t))
        .collect();
    (positional.first().copied(), positional.last().copied())
}

/// "3 minutes ago", the way twtxt says it.
pub fn time_ago(then: u32, now: u32) -> String {
    let delta = now.saturating_sub(then) as u64;
    let (n, unit) = match delta {
        0..=59 => return "just now".into(),
        60..=3599 => (delta / 60, "minute"),
        3600..=86_399 => (delta / 3600, "hour"),
        86_400..=2_591_999 => (delta / 86_400, "day"),
        2_592_000..=31_535_999 => (delta / 2_592_000, "month"),
        _ => (delta / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> (String, PubKey) {
        ("alice".into(), ritualistic::SecretKey::generate().pubkey())
    }

    #[test]
    fn mentions_round_trip() {
        let known = vec![alice()];
        let (expanded, tags) = expand_mentions("hey @alice, and @bob. mail@alice.com", &known);
        assert!(expanded.starts_with("hey nostr:npub1"));
        assert!(expanded.contains(", and @bob. mail@alice.com"));
        assert_eq!(tags.len(), 1);
        assert_eq!(
            collapse_mentions(&expanded, &known),
            "hey @alice, and @bob. mail@alice.com"
        );
    }

    #[test]
    fn mention_at_end_of_sentence() {
        let known = vec![alice()];
        let (expanded, _) = expand_mentions("thanks @alice.", &known);
        assert!(expanded.ends_with('.'));
        assert_eq!(collapse_mentions(&expanded, &known), "thanks @alice.");
    }

    #[test]
    fn unknown_references_are_shortened() {
        let pk = ritualistic::SecretKey::generate().pubkey();
        let shown = collapse_mentions(&format!("cc nostr:{}", pk.to_npub()), &[]);
        assert_eq!(shown, format!("cc @{}", short_npub(&pk)));
    }

    fn e(id: &ID, marker: &str) -> Tag {
        vec!["e".into(), id.to_hex(), String::new(), marker.into()]
    }

    #[test]
    fn thread_references() {
        let (r, p) = (ID([1; 32]), ID([2; 32]));
        assert_eq!(thread_refs(&[]), (None, None));
        assert_eq!(thread_refs(&[e(&r, "root")]), (Some(r), Some(r)));
        assert_eq!(
            thread_refs(&[e(&p, "reply"), e(&r, "root")]),
            (Some(r), Some(p))
        );
        assert_eq!(thread_refs(&[e(&r, ""), e(&p, "")]), (Some(r), Some(p)));
        assert_eq!(thread_refs(&[e(&r, "mention")]), (None, None));
    }

    #[test]
    fn humane_times() {
        assert_eq!(time_ago(100, 100), "just now");
        assert_eq!(time_ago(0, 60), "1 minute ago");
        assert_eq!(time_ago(0, 7200), "2 hours ago");
    }
}
