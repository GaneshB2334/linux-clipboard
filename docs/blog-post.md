# Blog post — ready to paste into Sanity Studio (Portfolio)

Studio → **Portfolio** → new Blog Post. Fields below map 1:1 to the form.

---

## Site
`Portfolio`

## Title
```
Clipboard History on Ubuntu: A Win+V Alternative for Linux
```

## Slug
```
clipboard-history-ubuntu-win-v-alternative-linux
```

## Excerpt
```
Windows has Win+V. Linux doesn't — so I built it. clipd is an open-source clipboard manager for Ubuntu with an instant popup, searchable history, and a free .deb download.
```

## Published At
Today's date.

## Reading Time
`6`

## Tags
`Linux` · `Ubuntu` · `Open Source` · `Rust` · `Productivity`

## SEO → Meta Title (60 char limit)
```
Clipboard History on Ubuntu: A Win+V Alternative
```

## SEO → Meta Description (160 char limit)
```
Free, open-source clipboard manager for Ubuntu and Debian. Instant popup, searchable history, pinned items. Download the .deb and get Win+V on Linux.
```

---

## Body

> Paste the sections below into the body editor. `##` → Heading 2, `###` → Heading 3.
> Set links as annotations on the highlighted text.

---

If you've moved to Linux from Windows, you've probably reached for **Win+V** on
reflex and got nothing. Windows keeps the last several things you copied and
lets you pick one from a popup. Linux keeps exactly one, and the moment you copy
something else, the previous one is gone.

That gap is small until it isn't — copying three values out of a config file one
at a time, losing a URL because you copied a command right after it.

So I built **clipd**: an open-source clipboard manager for Ubuntu and Debian with
the popup Windows should have shipped on Linux years ago.

**[→ Download the .deb](https://github.com/GaneshB2334/linux-clipboard/releases/latest/download/clipd_amd64.deb)**  ·  **[Source on GitHub](https://github.com/GaneshB2334/linux-clipboard)**

## What it does

Press the shortcut, start typing to filter, press Enter. That's the entire
interaction.

- **Searchable history.** Type to filter from the first keystroke — no clicking
  into a search box first.
- **Pinned items** for the things you paste constantly.
- **Every format is kept.** Copying styled text keeps both the rich and plain
  versions, so "paste as plain text" still works days later.
- **Passwords are dropped, not stored.** More on this below.
- **Images too**, alongside text, URLs, code snippets and colours.

## Install on Ubuntu or Debian

The quickest way:

```
curl -fsSL https://raw.githubusercontent.com/GaneshB2334/linux-clipboard/main/scripts/install.sh | bash
```

It detects your architecture and session type, downloads the matching
release, verifies its checksum, and starts clipd — nothing else to run.

Prefer to do it by hand? Grab the package from the link above, then:

```
sudo apt install ./clipd_amd64.deb
```

That's it — no logout, nothing else to configure. The popup opens with
`Ctrl+Alt+C` immediately, and paste works out of the box: it's injected
through a kernel-level virtual keyboard rather than a desktop-specific
extension, so there's no per-session setup step to wait on.

The popup opens with **Ctrl+Alt+C** out of the box.

### Getting actual Super+V

If you want the real Win+V muscle memory, add it yourself in
*Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts*:

```
Name:     Clipboard
Command:  clipctl toggle
Shortcut: Super+V
```

This step can't be automated, and the reason is interesting: GNOME reserves
**every** `Super`+key combination for itself. An application asking to bind one
is refused outright — the desktop's own shortcut settings are the only route.

## It has to be fast, or you won't use it

A clipboard manager sits between you and something you were already doing. If the
popup takes half a second, you stop reaching for it.

The popup opens in **9–11 milliseconds**. Not because of clever rendering, but
because it does no work when you open it: the window is built when clipd starts
and then hidden, with its list already filled in. Opening it just makes an
existing window visible.

The background process that watches your clipboard sits at about **5 MB of RAM
and no measurable CPU** while idle. Most clipboard tools poll — checking every
half second whether anything changed, forever. clipd instead asks the display
server to *tell* it when the clipboard changes, so it genuinely does nothing
between copies.

## Your passwords don't end up in it

A tool that records everything you copy will eventually record a password. That's
not hypothetical — it's the default behaviour of most clipboard history tools.

clipd handles this two ways:

**It listens for the hint.** KeePassXC, Bitwarden, 1Password, Firefox and Chromium
all tag copied credentials with a marker that says *this is a secret*. clipd
honours it: those copies are never written to disk.

**It recognises the shape of secrets.** Not everything comes from a password
manager — API keys pasted from a terminal don't carry any marker. So clipd also
detects things that look like credentials: JWTs, keys with known prefixes
(`sk-`, `ghp_`, `AKIA`), private key blocks, and card numbers that pass a Luhn
check.

Anything flagged either way is dropped, and never enters the search index.

## The Wayland problem

If you're on Ubuntu 24.04 or later, you're probably on Wayland — and Wayland
deliberately forbids one application from sending keystrokes to another. That's a
genuine security improvement, and it's also exactly what "paste this for you"
requires.

The official workaround is a permission prompt that leaves a **"remote access"
indicator** in your top bar for as long as the app runs. For a clipboard manager
that's wildly out of proportion — it looks like something is screen-sharing.

clipd ships a small GNOME Shell extension instead. Code running inside the
desktop shell can press Ctrl+V through the compositor's own virtual keyboard: no
permission prompt, no indicator. The extension does exactly one thing and reads
nothing.

If you'd rather not enable it, clipd still copies the item and tells you to press
Ctrl+V yourself.

## What it doesn't do yet

Being straight about the gaps:

- **KDE and other Wayland compositors aren't supported.** GNOME and X11 only.
- **No settings window.** The shortcut lives in a one-line config file.
- **Images are stored and pasted, but shown as a size rather than a thumbnail.**

It's version 0.1.0. It works, and I use it daily.

## Try it

**[Download the .deb](https://github.com/GaneshB2334/linux-clipboard/releases/latest/download/clipd_amd64.deb)** — free and open source (GPL-3.0).

The [source is on GitHub](https://github.com/GaneshB2334/linux-clipboard), issues
and pull requests welcome. If you build something with it or hit a bug, I'd like
to hear about it — I'm on
[LinkedIn](https://www.linkedin.com/in/ganeshbastapure/) and
[GitHub](https://github.com/GaneshB2334).

---

## FAQ (add these in the FAQ field)

**Q:** Is there a Win+V equivalent for Linux?
**A:** Not built in. Linux keeps only the most recent clipboard item, so you need a clipboard manager to get history. clipd provides a Win+V style popup on Ubuntu and Debian, though GNOME reserves Super+V so you bind the shortcut yourself in Settings.

**Q:** Does Ubuntu have clipboard history?
**A:** No. Ubuntu stores a single clipboard item and overwrites it every time you copy. Clipboard history requires a separate tool such as clipd.

**Q:** Will a clipboard manager store my passwords?
**A:** Most will. clipd drops them two ways: it honours the "this is a secret" marker that password managers set when copying, and it recognises credential-shaped text such as API keys and JWTs. Neither is written to disk or made searchable.

**Q:** Does clipd work on Wayland?
**A:** Yes. Copying and history work normally. Auto-paste needs the small GNOME Shell extension included in the package, because Wayland blocks applications from sending keystrokes to each other. Without it, clipd still copies and you press Ctrl+V.

**Q:** How do I install clipd on Ubuntu?
**A:** Run `curl -fsSL https://raw.githubusercontent.com/GaneshB2334/linux-clipboard/main/scripts/install.sh | bash`, or download the .deb and run `sudo apt install ./clipd_amd64.deb`. No logout needed — the popup opens with Ctrl+Alt+C immediately.
