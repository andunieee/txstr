#!/bin/bash
# re-records docs/demo.gif against a throwaway local relay (nak serve) full of made-up people.
# needs: asciinema, agg (https://github.com/asciinema/agg), nak (https://github.com/fiatjaf/nak), jq
set -euo pipefail
ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
export WORK=$(mktemp -d)
trap 'kill $SERVER 2>/dev/null; rm -rf "$WORK"' EXIT

cargo build -q --release --manifest-path "$ROOT/Cargo.toml" --bins
nak serve --port 7777 > /dev/null 2>&1 & SERVER=$!
sleep 1

R=ws://localhost:7777
now=$(date +%s)
for who in ghost ada ken lain; do
  sk=$(nak key generate)
  echo "${who}_sk=$sk"
  echo "${who}_npub=$(nak key public "$sk" | nak encode npub)"
done > "$WORK/keys.env"
source "$WORK/keys.env"

# prints the new note's id
post() { nak event --sec "$1" --ts $((now - $2)) -c "$3" "${@:4}" $R < /dev/null 2>/dev/null | jq -r .id; }
post "$lain_sk" 90000 "present day. present time." > /dev/null
post "$ada_sk"  18000 "wrote a forth interpreter in 400 lines this weekend. it's turtles all the way down." > /dev/null
grep_id=$(post "$ken_sk" 10800 'reminder that `grep -r` is still the best IDE feature ever shipped')
ken_pk=$(nak decode "$ken_npub")
manpage_id=$(post "$ada_sk" 7200 "nostr:$ken_npub the man page agrees with you" \
  -t "e=$grep_id;;root;$ken_pk" -p "$ken_pk")
# the short hash ghost reads off the timeline and replies to
echo "manpage=${manpage_id:0:7}" >> "$WORK/keys.env"
post "$ken_sk"   2400 "my homelab uptime is longer than most of my relationships" > /dev/null
post "$lain_sk"  1200 "everyone is connected. you just have to know who to follow." > /dev/null

mkdir -p "$WORK/config/txstr"
cat > "$WORK/config/txstr/config.toml" << CFG
nick = "ghost"
secret_key = "$ghost_sk"
servers = ["$R"]
timeout = 2

[following.ada]
npub = "$ada_npub"
servers = ["$R"]
last_seen = $now

[following.ken]
npub = "$ken_npub"
servers = ["$R"]
last_seen = $now
CFG

# pretend ghost's server list was already announced, so nothing leaves this machine
mkdir -p "$WORK/cache/txstr"
printf %s "$R" > "$WORK/cache/txstr/announced-$(nak key public "$ghost_sk")"

PATH="$ROOT/target/release:$PATH" XDG_CONFIG_HOME="$WORK/config" XDG_CACHE_HOME="$WORK/cache" \
  asciinema rec --overwrite --headless --window-size 96x26 --idle-time-limit 4 \
  -c "bash $ROOT/docs/demo/play.sh" "$WORK/demo.cast"
agg --theme monokai --font-size 16 --speed 1.1 --last-frame-duration 4 "$WORK/demo.cast" "$ROOT/docs/demo.gif"
echo "wrote $ROOT/docs/demo.gif"
