//! txstr: a tiny, decentralised microblogging client for the command line.
//! twtxt's spirit, nostr's plumbing.

mod config;
mod text;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ritualistic::{
    Event, EventTemplate, Filter, Kind, Network, Occurrence, PubKey, SecretKey,
    SubscriptionOptions, Tag, Tags, Timestamp,
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

    /// read what the people you follow have been saying
    Timeline {
        #[arg(short, long)]
        limit: Option<usize>,
        /// oldest first
        #[arg(short, long)]
        ascending: bool,
    },

    /// read a single feed, by nick or npub
    View {
        who: String,
        #[arg(short, long)]
        limit: Option<usize>,
        #[arg(short, long)]
        ascending: bool,
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
        Command::Tweet { text } => tweet(&config, text).await,
        Command::Timeline { limit, ascending } => {
            let mut authors: Vec<PubKey> = config.known().into_iter().map(|(_, pk)| pk).collect();
            authors.dedup();
            if authors.len() <= 1 {
                eprintln!("you're not following anyone yet. try `txstr follow <nick> <npub>`.");
            }
            show(&config, authors, limit, ascending).await
        }
        Command::View {
            who,
            limit,
            ascending,
        } => {
            let pk = config.resolve(&who)?;
            show(&config, vec![pk], limit, ascending).await
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

async fn tweet(config: &Config, words: Vec<String>) -> Result<()> {
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

    let (content, mentions) = text::expand_mentions(raw, &config.known());
    let event = EventTemplate {
        created_at: Timestamp::now(),
        kind: Kind(1),
        tags: Tags(mentions),
        content,
    }
    .finalize(&config.secret_key()?);

    let ok = publish(config, event).await?;
    println!("✓ posted to {ok} relay{}.", if ok == 1 { "" } else { "s" });
    Ok(())
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
) -> Result<()> {
    let limit = limit.unwrap_or(config.limit_timeline);
    let filter = Filter {
        kinds: Some(vec![Kind(1)]),
        authors: Some(authors),
        limit: Some(limit),
        ..Default::default()
    };

    let mut events = fetch(config, filter).await;
    events.sort_by_key(|e| std::cmp::Reverse(e.created_at.0));
    events.truncate(limit);
    if ascending {
        events.reverse();
    }

    let known = config.known();
    let now = Timestamp::now().0;
    for event in &events {
        println!(
            "➤ {} ({}):\n{}\n",
            bold(&text::display_name(&event.pubkey, &known)),
            dim(&text::time_ago(event.created_at.0, now)),
            text::collapse_mentions(&event.content, &known),
        );
    }
    Ok(())
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
