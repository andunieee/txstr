#!/bin/bash
# the scripted session that record.sh films
source "$WORK/keys.env"
# words are typed out letter by letter, keys and hashes are pasted in one go
type_out() {
  printf '\e[1;32m~\e[0m $ '
  local words w i
  read -ra words <<< "$1"
  for w in "${words[@]}"; do
    ((i++)) && printf ' ' && sleep 0.035
    if [[ $w =~ ^(npub1|nsec1|note1)[a-z0-9]+$ || $w =~ ^[0-9a-f]{7,}$ ]]; then
      sleep 0.3; printf '%s' "$w"; sleep 0.3
    else
      for ((c = 0; c < ${#w}; c++)); do printf '%s' "${w:c:1}"; sleep 0.035; done
    fi
  done
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
run "txstr reply $manpage 'man grep is 2000 lines. ken was right'" 2
run "txstr thread $manpage" 4
