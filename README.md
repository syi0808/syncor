# syncor

Cross-machine directory sync powered by content-addressed storage.

syncor monitors local directories, compresses and deduplicates content via [chkpt-core](https://crates.io/crates/chkpt-core), and syncs through Git repositories. It detects conflicts with three-point comparison and lets you resolve them interactively.

## Install

```bash
cargo install syncor-cli
```

Requires `git` CLI on your PATH.

## Quick Start

**Machine A** &mdash; link a directory and push:

```bash
syncor link ~/dotfiles --repo https://github.com/you/sync.git --name dotfiles
# Registers the directory, takes an initial snapshot, and pushes to the repo.

# After making changes:
syncor push ~/dotfiles
```

**Machine B** &mdash; connect and pull:

```bash
syncor connect https://github.com/you/sync.git --dir dotfiles --to ~/dotfiles
# Clones the repo, restores all files to ~/dotfiles.

# Later, pull updates:
syncor pull ~/dotfiles
```

## Commands

| Command | Description |
|---------|-------------|
| `syncor link <dir>` | Register a directory for sync (push mode) |
| `syncor connect <repo>` | Connect to a remote link (pull mode) |
| `syncor push [dir]` | Push local changes to remote |
| `syncor pull [dir]` | Pull remote changes to local |
| `syncor status [dir]` | Show link status and last sync time |
| `syncor log <dir>` | Show sync history |
| `syncor resolve <dir>` | Interactively resolve conflicts |
| `syncor unlink <dir>` | Remove a link by directory |
| `syncor disconnect <name>` | Remove a link by name |
| `syncor config set <key> <val>` | Update configuration |
| `syncor daemon start\|stop\|status` | Manage the background daemon |

## How It Works

```
Machine A                          Git Remote                        Machine B
    |                                  |                                  |
    |  syncor link + push              |                                  |
    |  scan -> hash -> LZ4 -> pack --->|                                  |
    |                                  |<--- syncor connect + pull        |
    |                                  |     restore from packs           |
    |                                  |                                  |
    |  modify files + syncor push      |                                  |
    |  incremental save (only diffs)-->|                                  |
    |                                  |<--- syncor pull                  |
    |                                  |     three-point merge            |
    |                                  |     auto-apply or conflict       |
```

**Storage engine** &mdash; uses chkpt-core for:
- XXH3-128 content-addressed hashing
- LZ4 compression
- Pack file chunking (50 MB, stays under GitHub's 100 MB limit)
- Incremental saves (unchanged files are skipped via file index)
- SQLite catalog for snapshot metadata

**Conflict detection** &mdash; three-point comparison using base (last synced), local, and remote snapshots:

| Base | Local | Remote | Result |
|------|-------|--------|--------|
| A | A | B | Auto-apply remote |
| A | B | A | Keep local |
| A | B | C | **Conflict** |
| &mdash; | B | C | **Conflict** (both added differently) |
| A | &mdash; | B | **Conflict** (deleted vs modified) |
| A | A | &mdash; | Auto-delete local |

**Transport** &mdash; pluggable backend (Git first). Uses the `git` CLI for authentication, so your existing SSH keys and credential helpers work out of the box.

## Ignore Files

Place a `.chkptignore` file in your synced directory root (gitignore syntax):

```
*.env
node_modules/
.DS_Store
```

Ignored files are never synced, and restore will not delete them.

## Configuration

```bash
syncor config set debounce_secs 5          # fsnotify debounce (default: 2)
syncor config set default_poll_interval_secs 120  # poll interval (default: 60)
```

Config stored at `~/.config/syncor/config.toml`.

## Data Layout

```
~/.config/syncor/
  config.toml        # Global settings
  links.toml         # Link registry

~/.local/share/syncor/
  state.db           # Sync state, conflicts, log
  links/<link-id>/   # Per-link git clone + chkpt store
```

## License

MIT
