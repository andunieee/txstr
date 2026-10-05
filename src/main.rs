//! txstr: a tiny, decentralised microblogging client for the command line.
//! twtxt's spirit, nostr's plumbing.

mod config;
mod seen;
mod text;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ritualistic::{
    Event, EventTemplate, Filter, ID, Kind, Network, Occurrence, PubKey, SecretKey,
    SubscriptionOptions, Tag, TagQuery, Tags, Timestamp,
};

use config::Config;

#[derive(Parser)]
#[command(
    version,
    about = "decentralised, minimalist microblogging for hackers, over nostr"
)]
struct Cli {
    /// path to the config file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// set up your nick, key and relays
    Quickstart,

    /// post a note (reads stdin when no text is given)
    #[command(alias = "post")]
    Tweet { text: Vec<String> },

    /// reply to a note, by its short hash
    Reply { hash: String, text: Vec<String> },

    /// show the whole conversation a note belongs to
    Thread { hash: String },

    /// read what the people you follow have been saying
    Timeline {
        #[arg(short, long)]
        limit: Option<usize>,
        /// oldest first
        #[arg(short, long)]
        ascending: bool,
        /// keep watching for new notes, like tail -f
        #[arg(short, long)]
        follow: bool,
    },

    /// read a single feed, by nick or npub
    View {
        who: String,
        #[arg(short, long)]
        limit: Option<usize>,
        #[arg(short, long)]
        ascending: bool,
        #[arg(short, long)]
        follow: bool,
    },

    /// follow someone under a nick of your choosing
    Follow { nick: String, npub: String },

    /// stop following someone
    Unfollow { nick: String },

    /// list who you follow
    Following,

    /// print your nick and npub, to share with friends
    Whoami,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(Config::default_path);

    if let Command::Quickstart = cli.command {
        return quickstart(&path);
    }

    let mut config = Config::load(&path)?;
    match cli.command {
        Command::Quickstart => unreachable!(),
        Command::Tweet { text } => tweet(&config, text, None).await,
        Command::Reply { hash, text } => {
            let id = seen::resolve(&hash)?;
            let parent = fetch_one(&config, id).await?;
            tweet(&config, text, Some(parent)).await
        }
        Command::Thread { hash } => thread(&config, seen::resolve(&hash)?).await,
        Command::Timeline {
            limit,
            ascending,
            follow,
        } => {
            let mut authors: Vec<PubKey> = config.known().into_iter().map(|(_, pk)| pk).collect();
            authors.dedup();
            if authors.len() <= 1 {
                eprintln!("you're not following anyone yet. try `txstr follow <nick> <npub>`.");
            }
            show(&config, authors, limit, ascending, follow).await
        }
        Command::View {
            who,
            limit,
            ascending,
            follow,
        } => {
            let pk = config.resolve(&who)?;
            show(&config, vec![pk], limit, ascending, follow).await
        }
        Command::Follow { nick, npub } => {
            let pk: PubKey = npub
                .parse()
                .map_err(|err| anyhow::anyhow!("not a valid npub: {err:?}"))?;
            let nick = nick.trim_start_matches('@').to_string();
            if config.following.contains_key(&nick) {
                bail!("you're already following someone as '{nick}'");
            }
            config.following.insert(nick.clone(), pk.to_npub());
            config.save(&path)?;
            println!("✓ you're now following {nick}.");
            if config.publish_follows {
                publish_follows(&config, None)
                    .await
                    .context("saved locally, but couldn't publish your follow list")?;
            }
            Ok(())
        }
        Command::Unfollow { nick } => {
            let nick = nick.trim_start_matches('@');
            let Some(npub) = config.following.remove(nick) else {
                bail!("you're not following anyone as '{nick}'");
            };
            config.save(&path)?;
            println!("✓ you've unfollowed {nick}.");
            if config.publish_follows {
                publish_follows(&config, npub.parse().ok())
                    .await
                    .context("saved locally, but couldn't publish your follow list")?;
            }
            Ok(())
        }
        Command::Following => {
            for (nick, npub) in &config.following {
                println!("➤ {} @ {npub}", bold(nick));
            }
            Ok(())
        }
        Command::Whoami => {
            println!("{} {}", bold(&config.nick), config.pubkey()?.to_npub());
            Ok(())
        }
    }
}

async fn tweet(config: &Config, words: Vec<String>, parent: Option<Event>) -> Result<()> {
    let raw = if words.is_empty() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        words.join(" ")
    };
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("nothing to say?");
    }

    if let Some(max) = config.character_warning {
        let count = raw.chars().count();
        if count > max
            && !confirm(&format!(
                "✗ that's {count} characters, more than {max}. post anyway?"
            ))?
        {
            return Ok(());
        }
    }

    let (content, mut tags) = text::expand_mentions(raw, &config.known());
    if let Some(parent) = &parent {
        tags = reply_tags(parent, tags);
    }
    let event = EventTemplate {
        created_at: Timestamp::now(),
        kind: Kind(1),
        tags: Tags(tags),
        content,
    }
    .finalize(&config.secret_key()?);
    let id = event.id;

    let ok = publish(config, event).await?;
    seen::remember([id]);
    println!(
        "✓ posted {} to {ok} relay{}.",
        seen::short(&id),
        if ok == 1 { "" } else { "s" }
    );
    Ok(())
}

/// nip-10 marked tags for a reply: point at the thread's root and at the
/// parent, and notify everyone who was already in the conversation.
fn reply_tags(parent: &Event, mut tags: Vec<Tag>) -> Vec<Tag> {
    let parent_hex = parent.id.to_hex();
    let parent_pk = parent.pubkey.to_hex();
    let (root, _) = text::thread_refs(&parent.tags.0);

    let mut thread = match root {
        Some(root) if root != parent.id => vec![
            vec!["e".into(), root.to_hex(), String::new(), "root".into()],
            vec![
                "e".into(),
                parent_hex,
                String::new(),
                "reply".into(),
                parent_pk.clone(),
            ],
        ],
        _ => vec![vec![
            "e".into(),
            parent_hex,
            String::new(),
            "root".into(),
            parent_pk.clone(),
        ]],
    };

    let mut people = vec![parent_pk];
    people.extend(
        parent
            .tags
            .0
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some("p"))
            .filter_map(|t| t.get(1).cloned()),
    );
    for hex in people {
        if !tags
            .iter()
            .any(|t| t.first().map(String::as_str) == Some("p") && t.get(1) == Some(&hex))
        {
            tags.push(vec!["p".into(), hex]);
        }
    }
    thread.append(&mut tags);
    thread
}

/// sends an event to all configured relays, returning how many accepted it.
async fn publish(config: &Config, event: Event) -> Result<usize> {
    let mut network = Network::new();
    let mut results = network.publish_many(&config.relays, event).await;
    let deadline = tokio::time::sleep(Duration::from_secs(config.timeout));
    tokio::pin!(deadline);

    let mut ok = 0;
    let mut seen = 0;
    while seen < config.relays.len() {
        tokio::select! {
            result = results.recv() => match result {
                Some(result) => {
                    seen += 1;
                    match result.error {
                        None => { ok += 1; eprintln!("  ✓ {}", result.relay_url) }
                        Some(err) => eprintln!("  ✗ {}: {err}", result.relay_url),
                    }
                }
                None => break,
            },
            _ = &mut deadline => {
                eprintln!("  … gave up waiting on the remaining relays");
                break;
            }
        }
    }

    if ok == 0 {
        bail!("no relay accepted the event");
    }
    Ok(ok)
}

/// publishes the follow list as a kind 3 event. kind 3 is replaceable, so to
/// avoid wiping out follows made from other clients we start from the latest
/// one on the relays, drop whoever was just unfollowed, and lay ours on top.
async fn publish_follows(config: &Config, unfollowed: Option<PubKey>) -> Result<()> {
    let me = config.pubkey()?;
    let previous = fetch(
        config,
        Filter {
            kinds: Some(vec![Kind(3)]),
            authors: Some(vec![me]),
            limit: Some(1),
            ..Default::default()
        },
    )
    .await
    .into_iter()
    .filter(|e| e.pubkey == me)
    .max_by_key(|e| e.created_at.0);

    // nip-02 petnames are exactly what our nicks are
    let mut tags: Vec<Tag> = config
        .following
        .iter()
        .filter_map(|(nick, npub)| {
            let pk: PubKey = npub.parse().ok()?;
            Some(vec!["p".into(), pk.to_hex(), String::new(), nick.clone()])
        })
        .collect();
    let ours: Vec<String> = tags.iter().map(|t| t[1].clone()).collect();
    let dropped = unfollowed.map(|pk| pk.to_hex());

    let mut created_at = Timestamp::now();
    let mut content = String::new();
    if let Some(previous) = previous {
        for tag in previous.tags.0 {
            let keep = match (tag.first().map(String::as_str), tag.get(1)) {
                (Some("p"), Some(hex)) => !ours.contains(hex) && dropped.as_ref() != Some(hex),
                _ => true,
            };
            if keep {
                tags.push(tag);
            }
        }
        content = previous.content;
        // replaceable events are ordered by created_at, so we must be newer
        created_at = Timestamp(created_at.0.max(previous.created_at.0 + 1));
    }

    let event = EventTemplate {
        created_at,
        kind: Kind(3),
        tags: Tags(tags),
        content,
    }
    .finalize(&config.secret_key()?);

    let ok = publish(config, event).await?;
    println!(
        "✓ follow list published to {ok} relay{}.",
        if ok == 1 { "" } else { "s" }
    );
    Ok(())
}

async fn show(
    config: &Config,
    authors: Vec<PubKey>,
    limit: Option<usize>,
    ascending: bool,
    follow: bool,
) -> Result<()> {
    let limit = limit.unwrap_or(config.limit_timeline);
    let filter = Filter {
        kinds: Some(vec![Kind(1)]),
        authors: Some(authors.clone()),
        limit: Some(limit),
        ..Default::default()
    };

    let mut events = fetch(config, filter).await;
    events.sort_by_key(|e| std::cmp::Reverse(e.created_at.0));
    events.truncate(limit);
    // like tail -f, new notes show up at the bottom, so the history goes oldest first too
    if ascending || follow {
        events.reverse();
    }

    let known = config.known();
    for event in &events {
        print_note(event, &known, "");
    }
    seen::remember(events.iter().flat_map(hashes_shown));

    if follow {
        let since = events.iter().map(|e| e.created_at.0).max();
        let seen = events.iter().map(|e| e.id).collect();
        watch(config, authors, since, seen).await;
    }
    Ok(())
}

/// streams new notes until interrupted, resubscribing if every relay drops us.
async fn watch(config: &Config, authors: Vec<PubKey>, since: Option<u32>, mut seen: HashSet<ID>) {
    let known = config.known();
    let mut since = since.unwrap_or(Timestamp::now().0);
    let network = Network::new();
    loop {
        let filter = Filter {
            kinds: Some(vec![Kind(1)]),
            authors: Some(authors.clone()),
            since: Some(Timestamp(since)),
            ..Default::default()
        };
        let mut occurrences = network
            .subscribe(&config.relays, filter, SubscriptionOptions::default())
            .await;
        while let Some(occ) = occurrences.recv().await {
            match occ {
                Occurrence::Event(event, _)
                    if event.verify_signature() && seen.insert(event.id) =>
                {
                    since = since.max(event.created_at.0);
                    print_note(&event, &known, "");
                    seen::remember(hashes_shown(&event));
                }
                Occurrence::Close => break,
                _ => {}
            }
        }
        eprintln!("  … lost every relay, reconnecting in 30s");
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// prints the whole conversation `id` is part of, as an indented tree.
async fn thread(config: &Config, id: ID) -> Result<()> {
    let target = fetch_one(config, id).await?;
    let root = text::thread_refs(&target.tags.0).0.unwrap_or(id);

    let (by_id, replies) = tokio::join!(
        fetch(
            config,
            Filter {
                ids: Some(vec![root]),
                ..Default::default()
            },
        ),
        fetch(
            config,
            Filter {
                kinds: Some(vec![Kind(1)]),
                tags: Some(vec![TagQuery("e".into(), vec![root.to_hex()])]),
                ..Default::default()
            },
        ),
    );

    let mut notes: HashMap<ID, Event> = HashMap::new();
    for event in by_id.into_iter().chain(replies).chain([target]) {
        if event.kind == Kind(1) {
            notes.insert(event.id, event);
        }
    }

    // anything whose parent we couldn't find hangs off the top level
    let mut children: HashMap<Option<ID>, Vec<&Event>> = HashMap::new();
    for event in notes.values() {
        let parent = text::thread_refs(&event.tags.0)
            .1
            .filter(|p| notes.contains_key(p));
        children.entry(parent).or_default().push(event);
    }
    for list in children.values_mut() {
        list.sort_by_key(|e| e.created_at.0);
    }

    let known = config.known();
    let mut stack: Vec<(&Event, usize)> = children
        .get(&None)
        .map(|top| top.iter().rev().map(|e| (*e, 0)).collect())
        .unwrap_or_default();
    while let Some((event, depth)) = stack.pop() {
        print_note(event, &known, &"    ".repeat(depth));
        if let Some(kids) = children.get(&Some(event.id)) {
            stack.extend(kids.iter().rev().map(|e| (*e, depth + 1)));
        }
    }
    seen::remember(notes.values().flat_map(hashes_shown));
    Ok(())
}

/// the note's own id and, for replies, its parent's: both get printed.
fn hashes_shown(event: &Event) -> impl Iterator<Item = ID> {
    [Some(event.id), text::thread_refs(&event.tags.0).1]
        .into_iter()
        .flatten()
}

fn print_note(event: &Event, known: &[(String, PubKey)], indent: &str) {
    let mut header = format!(
        "{indent}➤ {} ({}) {}",
        bold(&text::display_name(&event.pubkey, known)),
        dim(&text::time_ago(event.created_at.0, Timestamp::now().0)),
        dim(&format!("[{}]", seen::short(&event.id))),
    );
    if let (_, Some(parent)) = text::thread_refs(&event.tags.0) {
        header += &dim(&format!(" ↪ [{}]", seen::short(&parent)));
    }
    let content = text::collapse_mentions(&event.content, known);
    println!("{header}:");
    for line in content.lines() {
        println!("{indent}{line}");
    }
    println!();
}

/// fetches a single note by id, or complains that no relay has it.
async fn fetch_one(config: &Config, id: ID) -> Result<Event> {
    let filter = Filter {
        ids: Some(vec![id]),
        ..Default::default()
    };
    fetch(config, filter)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .with_context(|| format!("none of your relays has note {}", seen::short(&id)))
}

/// collects events until every relay says EOSE or we run out of patience.
async fn fetch(config: &Config, filter: Filter) -> Vec<Event> {
    let network = Network::new();
    let mut occurrences = network
        .subscribe(&config.relays, filter, SubscriptionOptions::default())
        .await;
    let deadline = tokio::time::sleep(Duration::from_secs(config.timeout));
    tokio::pin!(deadline);

    let mut events = Vec::new();
    loop {
        tokio::select! {
            occ = occurrences.recv() => match occ {
                Some(Occurrence::Event(event, _)) if event.verify_signature() => events.push(*event),
                Some(Occurrence::Event(..)) => {}
                _ => break,
            },
            _ = &mut deadline => break,
        }
    }
    events
}

fn quickstart(path: &std::path::Path) -> Result<()> {
    println!("txstr - quickstart");
    println!("==================\n");
    println!("this wizard will set up txstr for you.\n");

    if path.exists()
        && !confirm(&format!(
            "✗ {} already exists. overwrite it?",
            path.display()
        ))?
    {
        return Ok(());
    }

    let default_nick = std::env::var("USER").unwrap_or_else(|_| "anon".into());
    let nick = ask("➤ please enter your desired nick", &default_nick)?;

    let key = ask(
        "➤ paste an existing nsec, or leave empty to generate a fresh key",
        "",
    )?;
    let secret_key = if key.is_empty() {
        SecretKey::generate()
    } else {
        key.parse()
            .map_err(|err| anyhow::anyhow!("that doesn't look like a valid nsec: {err:?}"))?
    };

    let mut config = Config::new(nick, &secret_key);

    let relays = ask(
        "➤ relays to talk to, space separated",
        &config.relays.join(" "),
    )?;
    config.relays = relays.split_whitespace().map(String::from).collect();

    config.publish_follows =
        confirm("➤ publish your follow list too, so other clients can see it?")?;

    config.save(path)?;
    println!("\n✓ created config file at {}", path.display());
    println!("✓ you are {} — tell your friends:", bold(&config.nick));
    println!(
        "  txstr follow {} {}",
        config.nick,
        secret_key.pubkey().to_npub()
    );
    Ok(())
}

fn ask(prompt: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() {
        default.into()
    } else {
        line.into()
    })
}

fn confirm(prompt: &str) -> Result<bool> {
    Ok(ask(&format!("{prompt} (y/N)"), "")?.eq_ignore_ascii_case("y"))
}

fn bold(s: &str) -> String {
    style(s, "1")
}

fn dim(s: &str) -> String {
    style(s, "2")
}

fn style(s: &str, code: &str) -> String {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
