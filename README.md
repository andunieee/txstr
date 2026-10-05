txstr
=====

txstr is a text-centric, minimalist social ecosystem for publishing thoughts
on the internet.

No algorithms. No feed manipulation. No likes, reposts, ads, or recommended
content. Your identity is a keypair, your account lives on your machine, and
your timeline contains only the people you explicitly follow.

![demo](docs/demo.gif)

Conceptually, txstr is a bit like RSS or twtxt: you get someone's identity,
you follow them, and you read their posts. No server needed to publish --
send signed notes to servers hosted by others and anyone can verify them.

Getting started
---------------

    $ cargo install --locked --git https://github.com/andunieee/txstr
    $ txstr quickstart
    $ txstr follow ghost npub1…
    $ txstr tweet "hello world"
    $ txstr timeline

Requires Rust 1.91+. Always use `--locked`. Run install again to update.

Documentation
-------------

Full docs (commands, config, how it works): https://andunieee.github.io/txstr/

License
-------

txstr is released under the MIT License. Notes are signed JSON (`kind 1`,
thread markers, mention tags, `kind 3` follows, `kind 10002` server lists) and stay readable from any standard client.
