//! a throwaway in-memory relay, for trying txstr without bothering anyone.
//!
//!     cargo run --example devrelay            # listens on ws://127.0.0.1:7777
//!     txstr -c dev.toml ...                   # with relays = ["ws://127.0.0.1:7777"]

use std::sync::Arc;

use ritualistic::server::{CustomRelay, RelayInternals, start};
use ritualistic::{Event, Filter};

#[derive(Default)]
struct Memory {
    events: Vec<Event>,
}

impl CustomRelay for Memory {
    fn handle_event(&mut self, event: &Event) -> Result<(), String> {
        if !event.verify_signature() {
            return Err("invalid: bad signature".into());
        }
        let kind = event.kind;
        if kind.is_replaceable() {
            if self.events.iter().any(|e| {
                e.kind == kind && e.pubkey == event.pubkey && e.created_at >= event.created_at
            }) {
                return Ok(());
            }
            self.events
                .retain(|e| !(e.kind == kind && e.pubkey == event.pubkey));
        }
        self.events.push(event.clone());
        Ok(())
    }

    fn handle_request(&mut self, filter: &Filter) -> Result<Vec<Event>, String> {
        let mut found: Vec<Event> = self
            .events
            .iter()
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        found.sort_by_key(|e| std::cmp::Reverse(e.created_at.0));
        found.truncate(filter.limit.unwrap_or(500));
        Ok(found)
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7777".into());
    let internals = Arc::new(RelayInternals {
        info: Default::default(),
        custom_relay: Box::new(tokio::sync::Mutex::new(Memory::default())),
    });
    eprintln!("devrelay listening on ws://{addr}");
    start(internals, addr.parse().expect("invalid address"))
        .await
        .expect("relay crashed");
}
