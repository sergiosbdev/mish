# Mish

**A fast, opinionated shell in Rust, with a native macOS terminal app built around it.**

[![Release](https://img.shields.io/github/v/release/sergiosbdev/mish?sort=semver)](https://github.com/sergiosbdev/mish/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform: macOS](https://img.shields.io/badge/platform-macOS%2013%2B-lightgrey.svg)](#requirements)

<!--
  Screenshot: take it from a clean demo session (demo user, generic hosts such as
  srv-a / web / db, a ~/code/project directory, no real servers, paths or vault entries).
-->
<!-- ![Mish](docs/screenshot.png) -->

## Contents

- [What is Mish](#what-is-mish)
- [Features](#features)
- [Download & install](#download--install)
- [Updating](#updating)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Configuration](#configuration)
- [Security & privacy](#security--privacy)
- [Building from source](#building-from-source)
- [License & credits](#license--credits)

## What is Mish

Mish is two things that work together:

- **`mish`**, a personal shell written in Rust. It handles everyday work itself (moving
  between directories, jumping to servers, listing files) and hands anything complex
  (pipes, globs, `$VAR`, shell builtins) to zsh or bash, so you never lose compatibility.
- **Mish.app**, a native macOS terminal (tao + wry + xterm.js) that runs `mish` in its own
  PTY, with tabs, split panes, search, an encrypted password vault, and sessions that keep
  running after you close the window.

It is built for a specific way of working: many terminals open at once, several AI coding
agents running side by side, and a steady flow of `ssh` sessions to remote servers.
`mish` also works on its own in any terminal (Terminal.app, iTerm2, over ssh).

> The interface is currently in Spanish. Menu labels below are quoted as they appear,
> followed by their English meaning.

## Features

### The shell

- **Smart delegation.** Pipes, redirections, globs, `&&`, `$VAR`, builtins like `source`,
  `set` or `pushd`, and commands not found in `PATH` run through a configurable delegate
  shell (`zsh` by default, `bash`, or any absolute path). Commands with a path (`./x`,
  `/usr/bin/x`) always run directly.
- **Multi-line paste.** A pasted block runs command by command, as if typed line by line
  (`cd` still moves mish). `\` continuations are joined; multi-line constructs (`for … done`,
  `if … fi`, heredocs, quotes spanning lines) stay together and go to the delegate shell.
- **Quick navigation.** `z <pattern>` jumps to the most frequently used matching directory
  (it learns as you go, per machine); `ir <name>` jumps to a bookmarked directory from your
  config. The welcome panel lists the folders you actually visited most recently on this
  machine (missing ones are skipped).
- **Servers.** Hosts are read from `~/.ssh/config`. `s <name>` connects, `s <name> 'cmd'`
  runs a remote command, `servers` lists them and `servers -p` checks which ones respond.
- **Completion and suggestions.** Tab completes commands, aliases, servers, bookmarks and
  paths; → accepts the inline suggestion from history; ↑ with text typed walks back through
  commands starting with it.
- **History search.** Ctrl+R searches history by whatever you remember: several words in any
  order, case-insensitive, or just their letters in order (`dcup` → `docker compose up`). Most
  recent first, no duplicates; ↑↓ or Ctrl+R/Ctrl+S to pick, Enter puts it on the line (it does
  not run it), Esc/Ctrl+G closes.
- **Live syntax highlighting.** Known commands in green, unknown in red; flags, strings,
  variables and existing paths are coloured as you type.
- **Informative prompt.** Git branch with staged / modified / untracked counts and
  ahead/behind arrows, last command duration, exit status and a clock on the right. Past
  commands collapse to a compact `❯ command` line.
- **Long-command notifications.** A macOS notification when a command takes longer than a
  threshold (30 s by default) and you are not looking at that tab.
- **Full-screen programs just work.** `vim`, `htop`, `ssh`, `claude` and friends get the
  real terminal, including Ctrl+C and Ctrl+Z; `jobs` and `fg` bring stopped jobs back.
- **Other niceties.** A visual `l` listing (folders first, icons, size, age), session
  aliases, `reload`, emacs or vi editing mode, four colour themes, and commands that start
  with a space stay out of history.

### Mish.app

- **Tabs and split panes.** Split right or down, move focus between panes with the
  keyboard, drag dividers to resize, double-click a divider to even it out.
- **Search** the scrollback of the focused pane with ⌘F.
- **Inherited working directory.** New tabs and panes open in the directory of the pane
  you are focused on.
- **Tinted ssh tabs.** Connecting to a server colours the tab and renames it after the
  host, and restores it when you disconnect. Colours are automatic or set per server.
- **Encrypted vault.** Store passwords and have Mish type them straight into the terminal
  (handy for `sudo` prompts and remote logins) or copy them to a clipboard that clears
  itself after 30 seconds. On Macs with Touch ID it can unlock with your fingerprint, with
  the master password as fallback.
- **Git panel.** ⌘⇧G opens a side panel for the repository of the focused pane: changed
  files grouped as staged, unstaged, untracked and conflicts, with the diff of the selected
  file; open pull requests (CI and review status) and issues via the GitHub CLI (`gh`,
  logged in); and a `git grep` search of the repo. Press Enter on a file or a search match to
  open it in your editor at that line, set with `[editor] gui = "code -g {file}:{line}"` in
  `config.toml` (VS Code is used if it is not set).
- **Persistent sessions.** Shells live in a small background daemon (`mish-daemon`).
  Quit the app with ⌘Q and your running programs (editors, agents, builds) keep going;
  reopen it and every tab reattaches with its scrollback. ⌘⌥Q quits and closes all sessions.
- **Tab restoration.** The tab and pane layout, with each pane's directory, is saved and
  restored, even after a reboot.
- **Claude status.** When Claude Code runs in a pane, the tab and the pane show whether it
  is working, waiting for your permission, done or failed, and the Dock badge counts the
  panes waiting for you; with the app closed, the daemon sends a macOS notification instead.
  Enable it once with `~/Applications/Mish.app/Contents/MacOS/mish claude-hooks install`
  (adds `mish hook` handlers to `~/.claude/settings.json`; `--dry-run` previews the change,
  `claude-hooks uninstall` removes only Mish's handlers).
- **Built-in help.** **Ayuda → Atajos de teclado** (Help → Keyboard Shortcuts, ⌘/) shows a
  filterable list of every shortcut.
- **Signed updates.** Check for new releases from the app menu or with `mish update`.

## Download & install

1. Download the latest `Mish-X.Y.Z-macos-*.zip` from
   **[Releases](https://github.com/sergiosbdev/mish/releases/latest)**.
2. Unzip it and drag **Mish.app** to **/Applications**.
3. Open it (see below for the first launch).
4. Optional: to use `mish` from other terminals, choose **Mish → Instalar comando mish en el
   terminal…** (Install mish command…). It links `mish` into `~/.local/bin` and tells you
   if that folder is not in your `PATH` yet.

### First launch

Mish is open source and is not signed with an Apple Developer ID or notarized, so macOS
Gatekeeper will refuse to open it the first time with a warning that it cannot verify the
developer. This is expected. To allow it, do one of the following:

- **Right-click** Mish.app in Finder → **Open** → **Open**. On recent macOS versions, if
  that is not offered, open **System Settings → Privacy & Security** and click
  **Open Anyway** next to the message about Mish.
- Or remove the quarantine flag from the terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Mish.app
  ```

You only need to do this once. Later updates are verified by Mish itself (see
[Updating](#updating)).

### Requirements

- macOS 13 Ventura or later.
- Release builds target Apple Silicon (`arm64`). Check the assets of each release for other
  architectures, or [build from source](#building-from-source) on an Intel Mac.

## Updating

Mish never updates itself behind your back. It only looks for a new version when you ask:

- In the app: **Mish → Buscar actualizaciones…** (Check for Updates…).
- From the shell: `mish update` (or `mish update --check` to only check).

When a newer release exists you see its release notes and decide whether to install it:
nothing is downloaded until you click **Actualizar** (Update). Inside Mish.app, `mish update`
just opens that same notice.
The download is verified before anything is touched: its **SHA-256** checksum must match
and its **Ed25519 signature** must be valid against the public key built into Mish. The
new app is then checked (code signature and version) and swapped in atomically, with a
rollback if any step fails. Thanks to the daemon, your sessions survive the restart.

## Keyboard shortcuts

| Area | Shortcut | Action |
|---|---|---|
| Tabs | ⌘T / ⌘N | New tab (in the focused pane's directory) |
| | ⌘W | Close the focused pane (and the tab if it was the last one) |
| | ⌘1 … ⌘9 | Go to tab 1 … 9 |
| | ⌘⇧] / ⌘⇧[ | Next / previous tab |
| | Middle click | Close a tab |
| Panes | ⌘D | Split: new pane to the right |
| | ⌘⇧D | Split: new pane below |
| | ⌘⌥ + arrows | Focus the neighbouring pane |
| Terminal | ⌘C / ⌘V / ⌘A | Copy / paste / select all |
| | ⌘K | Clear the terminal and its scrollback |
| | ⌘+ / ⌘− / ⌘0 | Bigger / smaller / default font size |
| | ⌥ + key | Type keyboard symbols such as `\| @ # \ ~ [ ] { }` |
| Search | ⌘F | Search the pane's scrollback |
| | Enter / ⇧Enter | Next / previous match |
| Vault | ⌘⇧P | Open or close the vault |
| | Enter / click | Type the secret into the terminal |
| | ⌥Enter / ⌥click | Copy the secret (clipboard clears after 30 s) |
| Git | ⌘⇧G | Open or close the git panel |
| | Enter | Open the file or match in the editor, or the PR / issue in the browser |
| Window | ⌘Enter / ⌃⌘F | Full screen |
| Sessions | ⌘Q | Quit (sessions keep running) |
| | ⌘⌥Q | Quit and close all sessions |
| Help | ⌘/ | Show all keyboard shortcuts |

The full, searchable list is always one keystroke away: **Ayuda → Atajos de teclado**
(Help → Keyboard Shortcuts, ⌘/).

## Configuration

Mish reads `~/.config/mish/config.toml`. It is created with sensible defaults on first run
and reloaded automatically when you save it (or with `reload`). Configuration keys are in
Spanish; here is a commented example:

```toml
# Shell used for whatever mish does not run itself (pipes, globs, $VAR, builtins...):
# "zsh" (default), "bash" or an absolute path. Must come before any [section].
delegate_shell = "zsh"

[prompt]
# Variables: {cwd} {git} {host} {user} {estado} (exit status) {tiempo} (duration) {vpn}
formato = "{cwd} {git}{tiempo}{estado}❯ "

[tema]                     # theme
nombre = "cobre"           # "cobre", "pizarra", "bosque" or "papel" (light)
bienvenida = true          # welcome panel with date, recent folders and server status
prompt_compacto = true     # collapse past commands to "❯ command"

[avisos]                   # notices
notificar_seg = 30         # notify when a command takes longer than this (0 = never)
vpn = true                 # show a warning in the prompt when the -vpn hosts don't respond

[editor]
modo = "emacs"             # "emacs" or "vi" (restart mish to apply)
gui = "code -g {file}:{line}"  # editor opened from the git panel ({file} and {line})

[alias]
ll = "ls -la"
gs = "git status"

[atajos]                   # named shortcuts: type the name to run the command
deploy-logs = "ssh web 'docker logs -f --tail 100 app'"

[dirs]                     # bookmarks for `ir <name>`
code    = "~/code"
project = "~/code/project"

[servidores]               # servers
prefer = "vpn"             # which variant `s` tries first: "vpn" or "publico"
tenir = true               # tint the tab and set its title when connecting

[servidores.colores]       # tab background per server (otherwise an automatic one)
web = "#2a1418"

[servidores.extra]         # servers that are not in ~/.ssh/config: name = "user@host"
db = "admin@192.0.2.10"
```

**Server variants.** If your `~/.ssh/config` defines the same machine twice, for example
`web-vpn` (reachable through a VPN) and `web-publico` (its public address), Mish groups them
as a single server `web`. `s web` tries the preferred variant first and falls back to the
other one if it does not connect.

## Security & privacy

- **Vault.** Secrets are stored in `~/.config/mish/vault.bin` (mode 0600, atomic writes),
  encrypted with **XChaCha20-Poly1305** using a key derived from your master password with
  **Argon2id** (64 MiB, 3 passes). The vault locks itself after 5 minutes of inactivity and
  when the app quits. Secrets stay in the native process: the web view that draws the UI
  only ever receives entry names and usernames. Decrypted data is zeroized from memory.
- **Touch ID (optional).** Enabling it stores the derived vault key (never the master
  password) in your login keychain, where only the Mish binary that created it can read it
  without a system prompt; Mish asks for your fingerprint (LocalAuthentication) before
  reading it. The stored key is discarded when it no longer opens the vault, when you
  create a new vault, or when the enrolled fingerprints change. Because Mish.app is signed
  ad hoc, the fingerprint check is enforced by the app rather than by a biometric keychain
  ACL, and after an update macOS may ask once for your login password to let the new build
  read the key.
- **Session daemon.** `mish-daemon` listens on a Unix socket in `~/.local/state/mish/`. The
  directory must be 0700 and owned by you, the socket is 0600, and every connection is
  checked with `getpeereid`: only processes running as your user can attach. Terminal
  contents are never written to its log.
- **Claude hooks.** `mish hook` forwards only the event name, notification type, tool name
  and Claude's short notification message (at most 200 characters) to the local daemon:
  never your prompt, tool input, transcript or working directory. It does nothing outside
  a Mish pane and never writes to the terminal.
- **No telemetry.** Mish does not collect or send usage data. It only goes online when you
  ask it to check for updates (the public GitHub Releases API), plus TCP probes to port 22
  of your own ssh hosts for the welcome panel and VPN warning, which you can turn off with
  `bienvenida = false` and `vpn = false`.
- **Signed updates.** Updates are verified with SHA-256 and an Ed25519 signature before
  installation, and never installed without your confirmation.

## Building from source

Requirements:

- macOS with the Xcode Command Line Tools (`xcode-select --install`).
- Rust stable (1.85 or later, edition 2024), e.g. via [rustup](https://rustup.rs).
- Node.js is optional: the build script uses `node --check` to syntax-check the app's
  JavaScript when it is available.

```sh
git clone https://github.com/sergiosbdev/mish.git
cd mish

app/build-app.sh           # build mish, mish-daemon and Mish.app and install to ~/Applications
app/build-app.sh --dist    # build a distributable Mish.app into dist/ without installing
```

To use only the shell: `cargo install --path .` installs `mish` into `~/.cargo/bin`.

### Repository layout

| Path | Contents |
|---|---|
| `src/` | `mish`, the shell |
| `gui/` | Mish.app: window, tabs, panes, vault and the xterm.js front end (`gui/assets/`) |
| `daemon/` | `mish-daemon`, which owns the PTYs so sessions outlive the app |
| `proto/` | Protocol shared by the daemon and its clients over the Unix socket |
| `updater/` | Release check, download, verification and installation |
| `app/` | Build and release scripts, app icon |

### Releasing

For maintainers:

```sh
app/release.sh X.Y.Z       # bump the version everywhere, commit and tag vX.Y.Z
git push origin vX.Y.Z     # pushing the tag triggers the release workflow
```

The GitHub Actions workflow builds the app on macOS, zips it, signs the archive with the
Ed25519 private key stored in the `MISH_UPDATE_KEY` repository secret, and uploads the zip,
its `.sha256` and its `.sig` to the release.

## License & credits

Mish is released under the [MIT License](LICENSE).

It stands on the shoulders of these projects:

| Project | Used for | License |
|---|---|---|
| [xterm.js](https://xtermjs.org) | Terminal rendering in Mish.app | MIT |
| [tao](https://github.com/tauri-apps/tao) | Native windowing and event loop | Apache-2.0 / MIT |
| [wry](https://github.com/tauri-apps/wry) | Native web view | Apache-2.0 / MIT |
| [muda](https://github.com/tauri-apps/muda) | Native menus | Apache-2.0 / MIT |
| [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty) | Pseudo-terminals | MIT |
| [reedline](https://github.com/nushell/reedline) | Line editor of the shell | MIT |
| [RustCrypto](https://github.com/RustCrypto) (`argon2`, `chacha20poly1305`) | Vault encryption | Apache-2.0 / MIT |
| [objc2](https://github.com/madsmtm/objc2) (`objc2-local-authentication`) | Touch ID for the vault | MIT / Zlib / Apache-2.0 |
| [security-framework](https://github.com/kornelski/rust-security-framework) | Keychain access | MIT / Apache-2.0 |
