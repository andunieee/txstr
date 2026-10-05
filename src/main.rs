//! txstr: a tiny, decentralised microblogging client for the command line.
//! twtxt's spirit, signed JSON plumbing.

mod config;
mod seen;
mod servers;
mod text;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ritualistic::{
    Event, EventTemplate, Filter, ID, Kind, Network, Occurrence, PubKey, SecretKey,
    SubscriptionOptions, Tag, TagQuery, Tags, Timestamp,
};

use config::{Config, Follow};
use servers::ServerList;

/// someone's key and the servers we read their notes from.
type Route = (PubKey, Vec<String>);

#[derive(Parser)]
#[command(version, about = "decentralised, minimalist microblogging for hackers")]
struct Cli {
    /// path to the config file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// set up your nick, key and servers
    Quickstart,

    /// post a note (reads stdin when piped, opens $EDITOR otherwise)
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
        return quickstart(&path).await;
    }

    let mut config = Config::load(&path)?;
    match cli.command {
        Command::Quickstart => unreachable!(),
        Command::Tweet { text } => tweet(&config, text, None).await,
        Command::Reply { hash, text } => {
            let (id, root_author) = seen::resolve(&hash)?;
            let servers = thread_servers(&config, id, root_author).await?;
            let parent = fetch_one(&config, &servers, id).await?;
            tweet(&config, text, Some(parent)).await
        }
        Command::Thread { hash } => {
            let (id, root_author) = seen::resolve(&hash)?;
            thread(&config, id, root_author).await
        }
        Command::Timeline {
            limit,
            ascending,
            follow,
        } => {
            let nicks: Vec<String> = config.following.keys().cloned().collect();
            if nicks.is_empty() {
                eprintln!("you're not following anyone yet. try `txstr follow <nick> <npub>`.");
            }
            refresh_servers(&mut config, &path, &nicks).await?;
            let mut routes: Vec<Route> = vec![(config.pubkey()?, config.servers.clone())];
            for follow in config.following.values() {
                if let Some(route) = route(&config, follow)
                    && !routes.iter().any(|(pk, _)| *pk == route.0)
                {
                    routes.push(route);
                }
            }
            show(&mut config, &path, routes, limit, ascending, follow).await
        }
        Command::View {
            who,
            limit,
            ascending,
            follow,
        } => {
            let pk = config.resolve(&who)?;
            let route = if pk == config.pubkey()? {
                (pk, config.servers.clone())
            } else if let Some(nick) = config.nick_of(&pk) {
                refresh_servers(&mut config, &path, std::slice::from_ref(&nick)).await?;
                route(&config, &config.following[&nick]).context("invalid key in follow list")?
            } else {
                let servers = server_lists(&config, vec![pk])
                    .await
                    .get(&pk)
                    .map(|list| servers::pick(&list.write))
                    .unwrap_or_default();
                if servers.is_empty() {
                    eprintln!("  … couldn't find where they publish, trying your servers");
                    (pk, config.servers.clone())
                } else {
                    (pk, servers)
                }
            };
            show(&mut config, &path, vec![route], limit, ascending, follow).await
        }
        Command::Follow { nick, npub } => {
            let pk: PubKey = npub
                .parse()
                .map_err(|err| anyhow::anyhow!("not a valid npub: {err:?}"))?;
            let nick = nick.trim_start_matches('@').to_string();
            if config.following.contains_key(&nick) {
                bail!("you're already following someone as '{nick}'");
            }
            let servers = server_lists(&config, vec![pk])
                .await
                .get(&pk)
                .map(|list| servers::pick(&list.write))
                .unwrap_or_default();
            config
                .following
                .insert(nick.clone(), Follow::new(pk.to_npub(), servers.clone()));
            config.save(&path)?;
            println!("✓ you're now following {nick}.");
            if servers.is_empty() {
                println!(
                    "  couldn't find where they publish, so you'll read them from your servers."
                );
            } else {
                println!("  reading them from {}.", servers.join(" "));
            }
            if config.publish_follows {
                publish_follows(&config, None)
                    .await
                    .context("saved locally, but couldn't publish your follow list")?;
            }
            Ok(())
        }
        Command::Unfollow { nick } => {
            let nick = nick.trim_start_matches('@');
            let Some(unfollowed) = config.following.remove(nick) else {
                bail!("you're not following anyone as '{nick}'");
            };
            config.save(&path)?;
            println!("✓ you've unfollowed {nick}.");
            if config.publish_follows {
                publish_follows(&config, unfollowed.npub.parse().ok())
                    .await
                    .context("saved locally, but couldn't publish your follow list")?;
            }
            Ok(())
        }
        Command::Following => {
            for (nick, follow) in &config.following {
                println!("➤ {} @ {}", bold(nick), follow.npub);
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
    let raw = if !words.is_empty() {
        words.join(" ")
    } else if std::io::stdin().is_terminal() {
        compose()?
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
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
    let root_author = text::root_author(&event);

    if let Err(err) = announce_servers(config, false).await {
        eprintln!("  ✗ couldn't announce your servers: {err:#}");
    }
    let urls = recipients(config, &event).await?;
    let ok = publish(config, &urls, event).await?;
    seen::remember([(id, root_author)]);
    println!(
        "✓ posted {} to {ok} server{}.",
        seen::short(&id),
        if ok == 1 { "" } else { "s" }
    );
    Ok(())
}

/// opens $VISUAL or $EDITOR on an empty file and returns whatever was written.
fn compose() -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("txstr-{}.txt", std::process::id()));
    std::fs::write(&path, "")?;

    // through the shell, so EDITOR="code --wait" works like it does for git
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .with_context(|| format!("couldn't run your editor ({editor})"));
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    if !status?.success() {
        bail!("{editor} exited with an error, not posting");
    }
    Ok(text?)
}

/// marked tags for a reply: point at the thread's root and at the
/// parent, and notify everyone who was already in the conversation.
fn reply_tags(parent: &Event, mut tags: Vec<Tag>) -> Vec<Tag> {
    let parent_hex = parent.id.to_hex();
    let parent_pk = parent.pubkey.to_hex();
    let (root, _) = text::thread_refs(&parent.tags.0);
    let root_author = text::root_author(parent).map(|pk| pk.to_hex());

    let mut thread = match root {
        Some(root) if root != parent.id => vec![
            vec![
                "e".into(),
                root.to_hex(),
                String::new(),
                "root".into(),
                root_author.clone().unwrap_or_default(),
            ],
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

    // the root's author too, so the reply lands in their inbox with the rest
    let mut people: Vec<String> = root_author.into_iter().collect();
    people.push(parent_pk);
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

/// our servers, plus a few of the inbox servers of everyone the note tags,
/// so it reaches them even if they don't read from where we write.
async fn recipients(config: &Config, event: &Event) -> Result<Vec<String>> {
    let me = config.pubkey()?;
    let mut tagged: Vec<PubKey> = Vec::new();
    for tag in &event.tags.0 {
        if let (Some("p"), Some(hex)) = (tag.first().map(String::as_str), tag.get(1))
            && let Ok(pk) = hex.parse::<PubKey>()
            && pk != me
            && !tagged.contains(&pk)
        {
            tagged.push(pk);
        }
    }

    let mut urls = config.servers.clone();
    if tagged.is_empty() {
        return Ok(urls);
    }
    let mut have: HashSet<String> = urls.iter().filter_map(|u| servers::clean(u)).collect();
    for list in server_lists(config, tagged).await.values() {
        for url in servers::pick(&list.read) {
            if have.insert(url.clone()) {
                urls.push(url);
            }
        }
    }
    Ok(urls)
}

/// sends an event to `urls`, reporting on each, returning how many accepted it.
async fn publish(config: &Config, urls: &[String], event: Event) -> Result<usize> {
    let ok = send(urls, event, config.timeout, true).await;
    if ok == 0 {
        bail!("no server accepted the event");
    }
    Ok(ok)
}

async fn send(urls: &[String], event: Event, timeout: u64, report: bool) -> usize {
    let mut network = Network::new();
    let mut results = network.publish_many(urls, event).await;
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);

    let mut ok = 0;
    let mut seen = 0;
    while seen < urls.len() {
        tokio::select! {
            result = results.recv() => match result {
                Some(result) => {
                    seen += 1;
                    match result.error {
                        None => {
                            ok += 1;
                            if report { eprintln!("  ✓ {}", result.relay_url) }
                        }
                        Some(err) if report => eprintln!("  ✗ {}: {err}", result.relay_url),
                        Some(_) => {}
                    }
                }
                None => break,
            },
            _ = &mut deadline => {
                if report { eprintln!("  … gave up waiting on the remaining servers") }
                break;
            }
        }
    }
    ok
}

/// publishes our server list (kind 10002), every server both read and write,
/// to our servers and the directories, so others know where to find us.
/// skipped when it's the same list we announced last time, unless forced.
async fn announce_servers(config: &Config, force: bool) -> Result<()> {
    let me = config.pubkey()?;
    if !force && servers::already_announced(&me, &config.servers) {
        return Ok(());
    }
    let event = EventTemplate {
        created_at: Timestamp::now(),
        kind: servers::LIST,
        tags: Tags(servers::tags(&config.servers)),
        content: String::new(),
    }
    .finalize(&config.secret_key()?);

    let mut urls = config.servers.clone();
    urls.extend(servers::DIRECTORIES.map(String::from));
    if send(&urls, event, config.timeout, false).await == 0 {
        bail!("no server accepted it");
    }
    servers::remember_announced(&me, &config.servers);
    println!("✓ announced your servers, so people can find your notes.");
    Ok(())
}

/// the newest server list of each of `pubkeys`, looked up on the directories.
async fn server_lists(config: &Config, pubkeys: Vec<PubKey>) -> HashMap<PubKey, ServerList> {
    let filter = Filter {
        kinds: Some(vec![servers::LIST]),
        authors: Some(pubkeys),
        ..Default::default()
    };
    let directories = servers::DIRECTORIES.map(String::from);
    servers::newest(fetch_from(&Network::new(), &directories, filter, config.timeout).await)
}

/// looks up the server lists of whoever among `nicks` we don't have servers
/// for, or haven't seen a note from in over a week, in case they've moved.
async fn refresh_servers(config: &mut Config, path: &Path, nicks: &[String]) -> Result<()> {
    let now = Timestamp::now().0;
    let due: Vec<(String, PubKey)> = nicks
        .iter()
        .filter_map(|nick| {
            let follow = config.following.get(nick)?;
            let stale = follow.servers.is_empty()
                || follow
                    .last_seen
                    .is_none_or(|t| now.saturating_sub(t) > servers::STALE);
            Some((nick.clone(), follow.npub.parse().ok().filter(|_| stale)?))
        })
        .collect();
    if due.is_empty() {
        return Ok(());
    }

    let lists = server_lists(config, due.iter().map(|(_, pk)| *pk).collect()).await;
    let mut changed = false;
    for (nick, pk) in due {
        let Some(list) = lists.get(&pk) else { continue };
        let fresh = servers::pick(&list.write);
        let Some(follow) = config.following.get_mut(&nick) else {
            continue;
        };
        if fresh.is_empty() {
            continue;
        }
        if follow.servers.is_empty() {
            eprintln!("  ✓ found where {nick} publishes: {}", fresh.join(" "));
            follow.servers = fresh;
            changed = true;
        } else if !servers::still_listed(&follow.servers, &list.write) {
            println!("➤ {nick} seems to have moved.");
            println!("  you read them from: {}", follow.servers.join(" "));
            println!("  they now publish to: {}", list.write.join(" "));
            if confirm(&format!(
                "➤ read {nick} from {} from now on?",
                fresh.join(" ")
            ))? {
                follow.servers = fresh;
            } else {
                // so we don't ask again until they've been quiet another week
                follow.last_seen = Some(now);
            }
            changed = true;
        }
    }
    if changed {
        config.save(path)?;
    }
    Ok(())
}

/// where to read someone we follow from: their servers, or ours if we don't know them yet.
fn route(config: &Config, follow: &Follow) -> Option<Route> {
    let pk = follow.npub.parse().ok()?;
    let servers = if follow.servers.is_empty() {
        config.servers.clone()
    } else {
        follow.servers.clone()
    };
    Some((pk, servers))
}

/// moves `last_seen` forward for everyone we follow in `newest`, which has
/// the time of the newest note we've got from each author.
fn bump_last_seen(config: &mut Config, newest: &HashMap<PubKey, u32>) -> bool {
    let mut changed = false;
    for follow in config.following.values_mut() {
        let Ok(pk) = follow.npub.parse::<PubKey>() else {
            continue;
        };
        let newest = newest.get(&pk).copied();
        if newest > follow.last_seen {
            follow.last_seen = newest;
            changed = true;
        }
    }
    changed
}

fn newest_by_author(newest: &mut HashMap<PubKey, u32>, event: &Event) {
    let at = newest.entry(event.pubkey).or_default();
    *at = (*at).max(event.created_at.0);
}

/// publishes the follow list as a kind 3 event. kind 3 is replaceable, so to
/// avoid wiping out follows made from other clients we start from the latest
/// one on the servers, drop whoever was just unfollowed, and lay ours on top.
async fn publish_follows(config: &Config, unfollowed: Option<PubKey>) -> Result<()> {
    let me = config.pubkey()?;
    let previous = fetch_from(
        &Network::new(),
        &config.servers,
        Filter {
            kinds: Some(vec![Kind(3)]),
            authors: Some(vec![me]),
            limit: Some(1),
            ..Default::default()
        },
        config.timeout,
    )
    .await
    .into_iter()
    .filter(|e| e.pubkey == me)
    .max_by_key(|e| e.created_at.0);

    // petnames are exactly what our nicks are
    let mut tags: Vec<Tag> = config
        .following
        .iter()
        .filter_map(|(nick, follow)| {
            let pk: PubKey = follow.npub.parse().ok()?;
            let hint = follow.servers.first().cloned().unwrap_or_default();
            Some(vec!["p".into(), pk.to_hex(), hint, nick.clone()])
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

    let ok = publish(config, &config.servers, event).await?;
    println!(
        "✓ follow list published to {ok} server{}.",
        if ok == 1 { "" } else { "s" }
    );
    Ok(())
}

/// prints the latest notes of each author, asking each one's own servers separately.
async fn show(
    config: &mut Config,
    path: &Path,
    routes: Vec<Route>,
    limit: Option<usize>,
    ascending: bool,
    follow: bool,
) -> Result<()> {
    let limit = limit.unwrap_or(config.limit_timeline);
    let network = Network::new();
    let mut requests = tokio::task::JoinSet::new();
    for (pk, urls) in routes.clone() {
        let filter = Filter {
            kinds: Some(vec![Kind(1)]),
            authors: Some(vec![pk]),
            limit: Some(limit),
            ..Default::default()
        };
        let (network, timeout) = (network.clone(), config.timeout);
        requests.spawn(async move {
            let mut events = fetch_from(&network, &urls, filter, timeout).await;
            events.retain(|e| e.pubkey == pk);
            events
        });
    }
    let mut events = Vec::new();
    while let Some(found) = requests.join_next().await {
        events.extend(found.unwrap_or_default());
    }

    let mut newest = HashMap::new();
    for event in &events {
        newest_by_author(&mut newest, event);
    }
    if bump_last_seen(config, &newest)
        && let Err(err) = config.save(path)
    {
        eprintln!("  ✗ couldn't save when you last saw everyone: {err:#}");
    }

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
        watch(config, path, network, routes, since, seen).await;
    }
    Ok(())
}

/// streams new notes until interrupted, one subscription per author.
async fn watch(
    config: &Config,
    path: &Path,
    network: Network,
    routes: Vec<Route>,
    since: Option<u32>,
    mut seen: HashSet<ID>,
) {
    let known = config.known();
    let since = since.unwrap_or(Timestamp::now().0);
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    for (pk, urls) in routes {
        let name = text::display_name(&pk, &known);
        tokio::spawn(tail(network.clone(), pk, urls, since, name, tx.clone()));
    }
    drop(tx);

    // last_seen is written down in batches, not once per note
    let mut newest = HashMap::new();
    let mut flush = tokio::time::interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break };
                if seen.insert(event.id) {
                    print_note(&event, &known, "");
                    seen::remember(hashes_shown(&event));
                    newest_by_author(&mut newest, &event);
                }
            }
            _ = flush.tick() => if !newest.is_empty() {
                save_last_seen(path, &std::mem::take(&mut newest));
            },
        }
    }
}

/// reloads the config before saving, so a long-running watch doesn't undo
/// whatever was changed in the meantime.
fn save_last_seen(path: &Path, newest: &HashMap<PubKey, u32>) {
    let saved = Config::load(path).and_then(|mut config| {
        if bump_last_seen(&mut config, newest) {
            config.save(path)?;
        }
        Ok(())
    });
    if let Err(err) = saved {
        eprintln!("  ✗ couldn't save when you last saw everyone: {err:#}");
    }
}

/// forwards one author's new notes, resubscribing if all their servers drop us.
async fn tail(
    network: Network,
    pk: PubKey,
    urls: Vec<String>,
    mut since: u32,
    name: String,
    tx: tokio::sync::mpsc::Sender<Event>,
) {
    loop {
        let filter = Filter {
            kinds: Some(vec![Kind(1)]),
            authors: Some(vec![pk]),
            since: Some(Timestamp(since)),
            ..Default::default()
        };
        let mut occurrences = network
            .subscribe(&urls, filter, SubscriptionOptions::default())
            .await;
        while let Some(occ) = occurrences.recv().await {
            match occ {
                Occurrence::Event(event, _) if event.pubkey == pk && event.verify_signature() => {
                    since = since.max(event.created_at.0);
                    if tx.send(*event).await.is_err() {
                        return;
                    }
                }
                Occurrence::Close => break,
                _ => {}
            }
        }
        eprintln!("  … lost every server for {name}, reconnecting in 30s");
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// a conversation lives on the inbox servers of whoever started it, since
/// every reply tags them. when we don't know who that is, we look the note up
/// where we usually read from and work it out.
async fn thread_servers(
    config: &Config,
    id: ID,
    root_author: Option<PubKey>,
) -> Result<Vec<String>> {
    let root_author = match root_author {
        Some(pk) => pk,
        None => {
            let everywhere = everywhere(config);
            let note = fetch_one(config, &everywhere, id).await?;
            match text::root_author(&note) {
                Some(pk) => pk,
                None => {
                    let root = text::thread_refs(&note.tags.0).0.unwrap_or(id);
                    fetch_one(config, &everywhere, root)
                        .await
                        .context("couldn't find who started this thread")?
                        .pubkey
                }
            }
        }
    };
    let inbox = server_lists(config, vec![root_author])
        .await
        .get(&root_author)
        .map(|list| servers::pick(&list.read))
        .unwrap_or_default();
    Ok(if inbox.is_empty() {
        config.servers.clone()
    } else {
        inbox
    })
}

/// prints the whole conversation `id` is part of, as an indented tree.
async fn thread(config: &Config, id: ID, root_author: Option<PubKey>) -> Result<()> {
    let servers = thread_servers(config, id, root_author).await?;
    let target = fetch_one(config, &servers, id).await?;
    let root = text::thread_refs(&target.tags.0).0.unwrap_or(id);

    let network = Network::new();
    let (by_id, replies) = tokio::join!(
        fetch_from(
            &network,
            &servers,
            Filter {
                ids: Some(vec![root]),
                ..Default::default()
            },
            config.timeout,
        ),
        fetch_from(
            &network,
            &servers,
            Filter {
                kinds: Some(vec![Kind(1)]),
                tags: Some(vec![TagQuery("e".into(), vec![root.to_hex()])]),
                ..Default::default()
            },
            config.timeout,
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

/// the note's own id and, for replies, its parent's: both get printed. they
/// share a thread, so they share its root's author.
fn hashes_shown(event: &Event) -> impl Iterator<Item = seen::Seen> {
    let root_author = text::root_author(event);
    [Some(event.id), text::thread_refs(&event.tags.0).1]
        .into_iter()
        .flatten()
        .map(move |id| (id, root_author))
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

/// fetches a single note by id from `urls`, or complains that none has it.
async fn fetch_one(config: &Config, urls: &[String], id: ID) -> Result<Event> {
    let filter = Filter {
        ids: Some(vec![id]),
        ..Default::default()
    };
    fetch_from(&Network::new(), urls, filter, config.timeout)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .with_context(|| format!("couldn't find note {} anywhere", seen::short(&id)))
}

/// our servers and those of everyone we follow: where the notes we show come from.
fn everywhere(config: &Config) -> Vec<String> {
    let mut have = HashSet::new();
    config
        .servers
        .iter()
        .chain(config.following.values().flat_map(|f| &f.servers))
        .filter(|url| servers::clean(url).is_some_and(|url| have.insert(url)))
        .cloned()
        .collect()
}

/// collects events until every server says EOSE or we run out of patience.
async fn fetch_from(
    network: &Network,
    urls: &[String],
    filter: Filter,
    timeout: u64,
) -> Vec<Event> {
    let mut occurrences = network
        .subscribe(urls, filter, SubscriptionOptions::default())
        .await;
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
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

async fn quickstart(path: &Path) -> Result<()> {
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

    let servers = ask(
        "➤ servers to talk to, space separated",
        &config.servers.join(" "),
    )?;
    config.servers = servers.split_whitespace().map(String::from).collect();

    config.publish_follows =
        confirm("➤ publish your follow list too, so other clients can see it?")?;

    config.save(path)?;
    println!("\n✓ created config file at {}", path.display());
    if let Err(err) = announce_servers(&config, true).await {
        eprintln!("✗ couldn't announce your servers yet: {err:#}");
    }
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
