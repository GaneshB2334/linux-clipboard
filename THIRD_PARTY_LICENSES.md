# Third-party notices

clipd is distributed under GPL-3.0-or-later. Its dependencies remain under
their own licenses; this file records the bundled data and notable runtime
components used by the product.

## Emoji data

The Emoji tab uses the pinned `emojibase-data` package. It is MIT-licensed and
contains Unicode/CLDR-derived names, keywords, categories, and skin-tone
variants.

Project: <https://github.com/milesj/emojibase>

## Rust and JavaScript dependencies

The Rust crates and JavaScript packages are installed from their package
registries and retain the licenses declared by their respective authors. A
release build must include the dependency lockfiles so the exact dependency
versions remain reproducible.

## Runtime components

`wl-clipboard`, GNOME Shell, GTK/WebKitGTK, SQLite, and X11 libraries are
system components. clipd communicates with them through their public command
or protocol interfaces and does not relicense them.
