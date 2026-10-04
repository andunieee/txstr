#!/bin/bash
# re-records docs/demo.gif against a throwaway local relay full of made-up people.
# needs: asciinema, agg (https://github.com/asciinema/agg), nak (https://github.com/fiatjaf/nak)
set -euo pipefail
ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
export WORK=$(mktemp -d)
trap 'kill $RELAY 2>/dev/null; rm -rf "$WORK"' EXIT

cargo build -q --release --manifest-path "$ROOT/Cargo.toml" --bins --examples
"$ROOT/target/release/examples/devrelay" 2>/dev/null & RELAY=$!
sleep 0.5

R=ws://127.0.0.1:7777
now=$(date +%s)
for who in ghost ada ken lain; do
  sk=$(nak key generate)
  echo "${who}_sk=$sk"
  echo "${who}_npub=$(nak key public "$sk" | nak encode npub)"
done > "$WORK/keys.env"
source "$WORK/keys.env"

post() { nak event -q --sec "$1" --ts $((now - $2)) -c "$3" "${@:4}" $R < /dev/null > /dev/null; }
post "$lain_sk" 90000 "present day. present time."
post "$ada_sk"  18000 "wrote a forth interpreter in 400 lines this weekend. it's turtles all the way down."
post "$ken_sk"  10800 'reminder that `grep -r` is still the best IDE feature ever shipped'
post "$ada_sk"   7200 "nostr:$ken_npub the man page agrees with you" -p "$(nak decode "$ken_npub")"
post "$ken_sk"   2400 "my homelab uptime is longer than most of my relationships"
post "$lain_sk"  1200 "everyone is connected. you just have to know who to follow."

mkdir -p "$WORK/config/txstr"
cat > "$WORK/config/txstr/config.toml" << CFG
nick = "ghost"
secret_key = "$ghost_sk"
relays = ["$R"]
timeout = 2

[following]
ada = "$ada_npub"
ken = "$ken_npub"
CFG

PATH="$ROOT/target/release:$PATH" XDG_CONFIG_HOME="$WORK/config" \
  asciinema rec --overwrite --headless --window-size 96x26 --idle-time-limit 4 \
  -c "bash $ROOT/docs/demo/play.sh" "$WORK/demo.cast"
agg --theme monokai --font-size 16 --speed 1.1 --last-frame-duration 4 "$WORK/demo.cast" "$ROOT/docs/demo.gif"
echo "wrote $ROOT/docs/demo.gif"
