txstr
=====

**txstr** is a decentralised, minimalist microblogging network for hackers.

So you want to get some thoughts out into the world while also following the
gibberish of a few people you actually find interesting? Instead of signing up
for yet another closed platform with an algorithm deciding what you see,
txstr gives you a nick, a keypair and a terminal. Your keypair is your
identity, your account; nobody hands it to you and nobody can take it away.
Your timeline is built from exactly the people you follow — no ranking, no
likes, no reposts, no ads, no "who to follow". You find people the old way:
someone hands you their key.

![demo](docs/demo.gif)

**tl;dr**: txstr is a CLI for a tiny, text-only social network with no
center, where your follow list is a file you own.

Features
--------

- A fast, single-binary command-line client, written in Rust.
- Talks to several independent servers at once, so no single one owns you.
- Your follow list is a plain TOML file, with nicks *you* chose. Read it,
  edit it, grep it, put it in your dotfiles.
- `@nick` mentions for the people you follow.
- Git-style short hashes on every note: `txstr reply a1b2c3d "same"`,
  `txstr thread a1b2c3d` to read the whole conversation.
- `txstr timeline -f` keeps watching for new notes, like `tail -f`.
- `txstr tweet` with no text opens `$EDITOR`.
- Plays well with your shell: `fortune | txstr tweet`,
  `txstr timeline | less`, `txstr view ken | grep -i rust`.

Getting started
---------------

    $ cargo install --locked --git https://github.com/andunieee/txstr
    $ txstr quickstart
    $ txstr whoami        # give the output to your friends
    $ txstr follow ken npub1…
    $ txstr tweet "hello world"
    $ txstr timeline

txstr needs Rust 1.91 or newer, and `--locked` matters: it builds against
the exact dependency versions in `Cargo.lock`.

Documentation
-------------

Check out the full documentation at: https://andunieee.github.io/txstr/

Hacking
-------

`cargo run --example devrelay` starts an in-memory server on
`ws://127.0.0.1:7777`. Point a throwaway config at it to try things without
talking to anyone. `docs/demo/record.sh` re-records the demo above against it.

Under the hood
--------------

txstr speaks [nostr](https://nostr.com), using the
[ritualistic](https://github.com/andunieee/ritualistic) library, so your notes
are readable from any other client and you can follow anyone there.

License
-------

txstr is released under the MIT License.
