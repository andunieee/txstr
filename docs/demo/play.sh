#!/bin/bash
# the scripted session that record.sh films
source "$WORK/keys.env"
type_out() {
  printf '\e[1;32m~\e[0m $ '
  local s=$1
  for ((i = 0; i < ${#s}; i++)); do printf '%s' "${s:i:1}"; sleep 0.035; done
  sleep 0.5
  echo
}
run() { type_out "$1"; eval "$1"; sleep "${2:-2}"; }

clear; sleep 0.6
run "txstr whoami" 1.5
run "txstr following" 1.5
run "txstr timeline -l 3" 3.5
run "txstr follow lain $lain_npub" 1.5
run "txstr view lain" 3
run "echo 'just joined. hi @ada, hi @lain' | txstr tweet" 2
run "txstr timeline -l 2" 3.5
