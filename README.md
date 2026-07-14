# Recif — Claude Profile Switcher

Recif lets you keep one master `~/.claude` directory and spin up lightweight,
isolated **profiles** (`~/.claude-enzyme`, `~/.claude-personal`, …) that share
the master's config via symlinks but authenticate as separate Claude accounts.

It decouples **account identity** (config-directory path → macOS Keychain slot)
from **config data** (symlinks into the master), so history, sessions, settings,
hooks, and plugins are shared across every profile while each profile logs in as
its own subscription. See [`PRD.md`](PRD.md) for the full design.

## How it works

Claude Code derives its Keychain credential slot from a hash of the
`CLAUDE_CONFIG_DIR` path string. A **real** directory whose contents are
symlinks into the master therefore gets its own credential slot while sharing
all state. Recif automates creating and maintaining those profile directories.

- Every top-level master entry is symlinked into each profile, **except** the
  denylist (`daemon/`, `.credentials.json`, `*.lock`, `*.db*`/`*.sqlite*`,
  `statsig/`, `.git*`).
- `daemon/` is always a **real** per-profile directory (never symlinked).
- A background **daemon** (via `launchd`) keeps every profile in sync with the
  master as files come and go.
- Profiles refuse to launch when the sync daemon is unhealthy (the launch gate),
  so you never land in an out-of-sync session unknowingly.

## Install

```bash
cargo build --release
install -m 755 target/release/recif ~/.local/bin/recif   # a dir on your PATH
codesign --force --sign - ~/.local/bin/recif             # macOS: re-sign after copy
```

> **macOS codesign gotcha.** `cargo` ad-hoc-signs the binary at its build
> location. Plain `cp`/`install` to another path invalidates that signature and,
> under the hardened runtime, macOS kills the copy on exec with `Killed: 9`
> (SIGKILL, exit 137) — including the launchd daemon, which would then
> crash-loop. Re-sign the installed copy with `codesign --force --sign -`.
> Install to a **stable** location (not the `target/` dir): the daemon plist
> records the binary's absolute path, so a `cargo clean` or moved repo would
> otherwise break the daemon. Re-run `recif doctor` after moving the binary to
> regenerate the plist.

## Usage

```bash
recif add <name> [--master ~/.claude] [--alias] [--description <s>] [--rc <path>]
recif remove <name> [--keep-daemon] [--yes]
recif list
recif doctor
```

- **add** — creates `~/.claude-<name>`, links the master, creates a real
  `daemon/`, records the profile in `~/.recif/config.toml`, installs+starts the
  daemon, and (with `--alias`) adds `alias claude-<name>="recif launch <name>"`
  to your shell rc. Idempotent.
- **remove** — safely unlinks each symlink (never following it into the master),
  removes the real `daemon/` (unless `--keep-daemon`), and drops the config entry
  and alias. The master and Keychain are untouched. Prompts for confirmation
  (`--yes` to skip).
- **list** — shows each profile's health, sync status, alias presence, and
  whether a Keychain slot exists, plus a daemon health line.
- **doctor** — checks the whole setup and auto-fixes what it can (reload the
  daemon, repair symlinks, recreate `daemon/`, remove leaked files). Missing
  Keychain slots are reported, never created (run `/login` once per profile).

### Launching a profile

The alias routes through the launch gate:

```bash
claude-enzyme            # = recif launch enzyme
claude-enzyme --resume   # args pass straight through to claude
```

When the daemon is healthy this `exec`s `claude` instantly. When it's down you
get a one-keystroke prompt to run `recif doctor`; declining aborts without
launching.

### First login per profile

Each profile has its own Keychain slot. Run the profile once and `/login`:

```bash
claude-enzyme    # then /login with that account
```

## Migrating an existing full-copy profile

If you already have a full-copy `~/.claude-enzyme`, convert it in place:

```bash
recif add enzyme
```

Files present in `~/.claude-enzyme` but not in the master are moved up to the
master and replaced with symlinks; files present in both let the **master win**.

## Configuration

`~/.recif/config.toml` (canonical absolute paths throughout):

```toml
version = 1
master = "/Users/you/.claude"

[daemon]
launchd_plist = "/Users/you/Library/LaunchAgents/com.recif.daemon.plist"
log_file = "/Users/you/.recif/daemon.log"

[[profiles]]
name = "enzyme"
description = "Enzyme work account"
created_at = "2026-07-07T00:00:00Z"
path = "/Users/you/.claude-enzyme"
```

## Uninstalling

```bash
recif uninstall              # stops+removes the daemon, aliases, profiles, ~/.recif
recif uninstall --keep-profiles   # same, but leave the profile dirs on disk
```

`uninstall` stops and unloads the launchd daemon, deletes its plist, removes
Recif-managed shell aliases, safely removes each profile directory (unlink only
— never following symlinks into the master), and deletes `~/.recif`. It **never**
touches the master `~/.claude` or Keychain credentials. It's idempotent and
best-effort: even a partially-installed setup can be cleaned. Finally, remove
the binary itself (`rm ~/.local/bin/recif`).

If you need to do it by hand (e.g. the binary is gone):

```bash
launchctl unload ~/Library/LaunchAgents/com.recif.daemon.plist
rm ~/Library/LaunchAgents/com.recif.daemon.plist
# remove the recif-managed alias lines (marked "# recif-managed") from ~/.zshrc or ~/.bashrc
rm -rf ~/.recif
```

> ⚠ **Never** `rm -rf` a profile directory by hand — a naive recursive delete
> follows the symlinks into the master and destroys shared data. Use
> `recif remove <name>` (or `recif uninstall`), which unlink the links only.

## Development

```bash
cargo test --features testutil   # unit + integration tests
RECIF_LIVE_WATCH=1 cargo test --features testutil --test reconcile_it live_watcher_smoke -- --nocapture
```

Tests never touch the real `~/.claude`: filesystem-touching logic runs against a
synthesized fake master in a `tempdir`, and system-touching code (launchctl,
exec, Keychain) sits behind traits so it's mockable and CI-safe.

## Status

Phase 2 (CLI v1): all four public commands, the hidden `daemon` and `launch`
entrypoints, launchd lifecycle, and shell-alias generation are implemented.
`history.jsonl` is treated as an ordinary shared symlink (advisory-lock
mitigation is deferred behind the Phase 1 append-test result).
