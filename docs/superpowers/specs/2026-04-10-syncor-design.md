# Syncor Design Spec

## Overview

Syncor is a cross-machine directory sync tool. It monitors local directories for changes, stores content using chkpt-core (content-addressed, compressed, chunked), and synchronizes via pluggable transport backends (Git first).

## Use Cases

- Dotfile sync across machines
- Project asset sharing
- General-purpose directory sync

## Architecture

```
syncor (workspace)
├── syncor-core/        # Core logic
│   ├── config/         # Global/project settings
│   ├── link/           # Link management (dir ↔ repo mapping)
│   ├── sync/           # Sync engine, conflict handling
│   ├── transport/      # SyncTransport trait + implementations
│   ├── watch/          # fsnotify watcher + poller
│   └── daemon/         # IPC server, worker queue, lifecycle management
├── syncor-cli/         # CLI binary (clap + dialoguer)
└── (chkpt-core)        # External: scan, hash, compress, chunk, save/restore
```

## chkpt-core Integration Strategy

Syncor uses chkpt-core's **lower-level modules directly** rather than the top-level `save()`/`restore()` functions, because:

- `save()`/`restore()` hardcode the store location to `~/.chkpt/stores/<project-id>/` via `StoreLayout::new()`, which derives project ID from the workspace path. This is unsuitable for syncor because:
  - Syncor needs stores inside the git transport workspace (`<link-id>/repo/stores/`)
  - Connected machines have different workspace paths, producing different project IDs

Note: `StoreLayout::from_home_dir()` exists but still appends `.chkpt/stores/<project_id>/` internally, and project_id is derived from the workspace path (which differs across machines). This makes it unsuitable for syncor's needs.

**Modules used directly:**
- `scanner`: parallel workspace scanning with ignore patterns
- `store::blob`: XXH3-128 hashing, content reading (mmap for large files)
- `store::pack`: `PackWriter::add_pre_compressed_bytes()` for writing, `PackSet::read()` / `PackSet::try_read()` for blob retrieval (handles locate + decompress internally)
- `store::tree`: directory tree encoding (bitcode + LZ4)
- `store::catalog`: SQLite catalog for snapshot metadata, blob index, manifest
- `index`: FileIndex for incremental change detection (skip unchanged files)
- `ops::lock`: file-based project locking

**NOT available from chkpt-core (syncor handles directly):**
- LZ4 compression: `compress_with_worker_context` is private in chkpt-core. Syncor uses `lz4_flex` directly.
- I/O locality ordering: `io_order` module is private. Syncor's save pipeline may be slightly slower than `chkpt save` for large workspaces. Acceptable trade-off for v1; can be optimized later by making `io_order` public in chkpt-core.

**Syncor manages:**
- Store path allocation (inside transport workspace)
- Orchestration of scan → hash → compress (lz4_flex) → pack → catalog flow
- Restore orchestration: catalog lookup → PackSet::read() → write to disk

Note: syncor stores are managed exclusively by syncor and are NOT shared with chkpt CLI. No `ProjectLock` from chkpt-core is needed; syncor uses its own per-link flock instead.

Note: blob hash format — `hash_content_bytes()` returns `[u8; 16]`, `hash_content()` returns 32-char hex string. `PackWriter::add_pre_compressed` takes hex string, `add_pre_compressed_bytes` takes `[u8; 16]`. `PackSet::read()`/`try_read()` take `&str` (hex). Catalog stores hashes as `[u8; 16]`. Use `blob::bytes_to_hex()` for conversion when needed.

This approach avoids forking chkpt-core while giving full control over store locations and sync semantics.

## Storage Layout

```
~/.config/syncor/
├── config.toml         # Global settings (poll interval, auth, etc.)
└── links.toml          # Link registry (dir ↔ repo mappings, authoritative for local state)

~/.local/share/syncor/
├── syncor.sock         # Unix domain socket (macOS/Linux)
├── daemon.log          # Daemon log
├── daemon.pid          # PID file for liveness checking
└── <link-id>/
    ├── repo/           # Git clone (transport workspace)
    └── state.db        # Local sync state (last sync, conflicts)
```

## Git Repo Structure (Remote)

```
<repo>/
├── syncor.toml             # Registered link list + metadata (authoritative for remote state)
└── stores/
    └── <link-name>/        # Per-link chkpt store
        ├── catalog.sqlite
        ├── trees/
        └── packs/
```

**Excluded from remote:**
- `index.bin`: machine-local cache of file metadata (mtime, inode). Meaningless on other machines, could cause incorrect cache hits. Each machine builds its own locally.
- `catalog.sqlite-wal`, `catalog.sqlite-shm`: SQLite WAL sidecar files. Before git commit, syncor runs `PRAGMA wal_checkpoint(TRUNCATE)` to flush all data into the main db file, ensuring no data is lost in uncommitted WAL pages.

## Catalog Merge Strategy

The catalog (`catalog.sqlite`) is shared across machines via git. Since multiple machines create snapshots independently, a **merge-on-pull** strategy is used instead of overwrite.

**Why merge works:**
- Snapshot IDs are UUIDv7 (time-ordered, globally unique) — no collisions
- blob_index entries are keyed by content hash (XXH3-128) — same content = same key, idempotent
- snapshot_files are linked to snapshot IDs — follow their parent snapshot

**Merge flow (on pull):**
1. `transport.pull()` brings the remote `catalog.sqlite` into the repo dir
2. Open remote catalog as attached database: `ATTACH '<repo>/stores/<link>/catalog.sqlite' AS remote`
3. Merge into local catalog:
   ```sql
   INSERT OR IGNORE INTO blob_index SELECT * FROM remote.blob_index;
   INSERT OR IGNORE INTO snapshots SELECT * FROM remote.snapshots;
   INSERT OR IGNORE INTO snapshot_files SELECT * FROM remote.snapshot_files;
   ```
4. Detach remote catalog
5. Run `PRAGMA wal_checkpoint(TRUNCATE)` on local catalog
6. Copy merged local catalog back to repo dir for next push

**On push:**
1. Run `PRAGMA wal_checkpoint(TRUNCATE)` on local catalog
2. Copy local catalog to repo dir
3. git add / commit / push

This ensures all machines see all snapshots while allowing independent local writes. The `last_synced_snapshot_id` in state.db references a snapshot ID that exists in all merged catalogs.

### links.toml vs syncor.toml Reconciliation

- `links.toml` (local) is authoritative for local link configuration (paths, settings)
- `syncor.toml` (remote) is authoritative for the link registry (what links exist in the repo)
- On `connect`: syncor reads remote `syncor.toml` to discover available links, then creates local entries in `links.toml`
- On `link`: syncor updates both local `links.toml` and remote `syncor.toml`
- Name conflicts: after any git merge of `syncor.toml`, syncor validates the merged result for duplicate link names. If duplicates are found, the merge is rejected and the user must choose a different name. (Git may auto-merge TOML additions without detecting semantic conflicts.)

## CLI Interface

```bash
# Link (register local dir for sync)
syncor link <dir>
syncor link <dir> --repo <repo> --name <name>

# Connect (pull remote dir to local)
syncor connect <repo> --dir <remote-name> --to <local-path>
syncor connect <repo>   # interactive mode

# Management
syncor status [dir]
syncor unlink <dir>
syncor disconnect <name>
syncor resolve <dir>     # interactive conflict resolution
syncor log <dir>         # shows sync event history from state.db (timestamp, action, result)

# Config
syncor config set <key> <value>

# Daemon
syncor daemon start
syncor daemon stop
syncor daemon status

# Manual (works without daemon)
syncor push [dir]
syncor pull [dir]
```

### Constraints

- **One dir, one link**: a directory can only be linked to one repo at a time. `syncor link /foo --repo B` when `/foo` is already linked to repo A will error. User must `syncor unlink /foo` first.
- **Link names are unique per repo**: enforced by semantic validation of `syncor.toml` after git merge.

## Ignore Configuration

Syncor respects chkpt-core's `.chkptignore` files placed in the synced directory root. These follow gitignore syntax and are processed by chkpt-core's scanner module. No separate syncor-specific ignore mechanism is needed.

## Transport Abstraction

```rust
trait SyncTransport {
    fn push(&self, link: &LinkInfo, store_path: &Path) -> Result<PushResult>;
    fn pull(&self, link: &LinkInfo, store_path: &Path) -> Result<PullResult>;
    fn list_remote_links(&self, repo: &str) -> Result<Vec<RemoteLinkInfo>>;
    fn has_remote_changes(&self, link: &LinkInfo) -> Result<bool>;
}

enum PushResult {
    Success { revision: String },
    Conflict { details: ConflictInfo },
}

enum PullResult {
    Success { revision: String },
    UpToDate,
    Conflict { details: ConflictInfo },
}
```

### GitTransport Implementation

- Maintains git clone at `~/.local/share/syncor/<link-id>/repo/`
- `push`: chkpt store files are written directly in the repo dir -> git add -> commit -> push
- `pull`: git fetch -> conflict check (revision comparison) -> git merge/pull
- `has_remote_changes`: `git rev-list` comparison (local HEAD vs remote HEAD)
- Auth: system git credentials first, fallback to config token

### Transport Error Handling

Git operations can fail in several ways. Error taxonomy:

| Error | Behavior |
|-------|----------|
| Network failure | Retry with exponential backoff (3 attempts, 5s/15s/45s). Log warning. |
| Push rejected (non-fast-forward) | Treat as conflict. Pull first, then retry push. |
| Auth failure | Log error, mark link as `error` state. No retry. |
| Repository corruption | Log error, mark link as `error` state. User must manually re-clone (unlink + re-link). |
| Rate limiting | Respect retry-after header. Back off. |

Daemon logs all transport errors to `daemon.log`.

## Initial Link (First Push)

When `syncor link <dir>` creates a brand-new link (no remote state yet):

1. Create entry in local `links.toml`
2. Initialize git repo (or use existing) via transport
3. Create store directory structure in repo (`stores/<link-name>/`)
4. Initialize empty catalog
5. Run first chkpt save (scan → hash → compress → pack → catalog)
6. Create/update `syncor.toml` in repo with new link entry
7. `transport.push()`
8. Set `last_synced_snapshot_id` = first snapshot ID in state.db
9. Register fsnotify watcher in daemon

## Initial Connect (Full Sync)

When `syncor connect <repo> --dir <name> --to <local-path>` runs:

1. Clone/pull the repo via transport
2. Read `syncor.toml` to discover available links
3. Merge remote catalog into local (see Catalog Merge Strategy)
4. If `<local-path>` is empty:
   - Full restore from remote catalog (all files from latest snapshot)
   - Set `last_synced_snapshot_id` = remote snapshot ID
5. If `<local-path>` has existing files:
   - Treat as merge with empty base (no previous sync point)
   - All local files are "locally added", all remote files are "remotely added"
   - Files present on both sides with same hash: no conflict
   - Files present on both sides with different hash: **conflict**
   - Set `last_synced_snapshot_id` = remote snapshot ID after resolution
6. Register poller in daemon (or start polling in foreground)

## Sync Engine

### Push Flow (fsnotify triggered)

1. File change detected (fsnotify)
2. Debounce (configurable, default 2s)
3. Acquire filesystem lock for this link
4. Scan workspace using chkpt-core scanner
5. Compare against local FileIndex to find changed files
6. Hash + compress changed files, write to pack (chkpt-core blob/pack modules)
7. Update catalog with new snapshot
8. Check `transport.has_remote_changes()`
   - No remote changes: `transport.push()`
   - Remote changes exist: pull first -> conflict check -> resolve -> push
9. Update state.db (last_local_snapshot, last_sync_at)
10. Release filesystem lock

### Pull Flow (polling triggered)

1. `transport.has_remote_changes()` check
2. If changed: acquire filesystem lock for this link
3. `transport.pull()`
4. Check local changes (scan + compare against last-synced snapshot)
   - No local changes: restore from remote catalog using chkpt-core pack/tree modules
   - Local changes exist: conflict check -> resolve -> restore
5. Update local FileIndex
6. Update state.db (last_remote_revision, last_sync_at)
7. Release filesystem lock

## Conflict Handling

### Detection (Three-Point Comparison)

Conflict detection uses three snapshots:

- **Base**: the last-synced snapshot (stored in `state.db` as `last_synced_snapshot_id`)
- **Local**: current local snapshot (from local catalog)
- **Remote**: latest remote snapshot (from pulled remote catalog)

For each file, compare its blob hash across all three points:

| Base | Local | Remote | Result |
|------|-------|--------|--------|
| A | A | A | No change |
| A | A | B | Remote changed — auto-apply |
| A | B | A | Local changed — auto-keep |
| A | B | B | Both changed to same — no conflict |
| A | B | C | **Conflict** — both sides changed differently |
| — | B | — | Local added — auto-keep |
| — | — | B | Remote added — auto-apply |
| — | B | C | **Conflict** — both sides added differently |
| A | — | A | Local deleted — auto-apply |
| A | A | — | Remote deleted — auto-apply |
| A | — | — | Both deleted — no conflict |
| A | B | — | **Conflict** — local changed, remote deleted |
| A | — | B | **Conflict** — local deleted, remote changed |

### Resolution (extensible via trait)

```rust
trait ConflictResolver {
    fn resolve(&self, conflict: &Conflict) -> Result<Resolution>;
}

enum Resolution {
    KeepLocal,
    KeepRemote,
    Merged(Vec<u8>),
    Skip,
}
```

**v1 scope**: InteractiveResolver - local/remote selection only.

**Future**: TextMergeResolver (3-way merge), CustomResolver (external merge tool), auto-resolve rules.

### State Tracking (state.db)

```sql
CREATE TABLE sync_state (
    link_id TEXT PRIMARY KEY,
    last_local_snapshot TEXT,
    last_remote_revision TEXT,
    last_synced_snapshot_id TEXT,  -- base for three-point comparison
    last_sync_at TEXT
);

CREATE TABLE conflicts (
    id INTEGER PRIMARY KEY,
    link_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    local_hash TEXT,    -- NULL if deleted locally
    remote_hash TEXT,   -- NULL if deleted remotely
    base_hash TEXT,     -- NULL if newly added
    detected_at TEXT NOT NULL
);

CREATE TABLE sync_log (
    id INTEGER PRIMARY KEY,
    link_id TEXT NOT NULL,
    action TEXT NOT NULL,     -- push, pull, conflict_resolved
    result TEXT NOT NULL,     -- success, error, conflict
    detail TEXT,
    created_at TEXT NOT NULL
);
```

## Daemon Architecture

```
syncor daemon
├── IPC Server (Unix socket / Windows named pipe)
│   └── Receives CLI commands via JSON protocol
├── Watcher Manager
│   └── Per-link fsnotify watcher
│   └── Change detected -> debounce -> enqueue sync job
├── Poller Manager
│   └── Per-connect polling timer
│   └── Timer fires -> has_remote_changes -> enqueue sync job
├── Sync Worker
│   └── Dequeue and process sequentially (per-link lock)
│   └── push / pull / conflict detection
└── State Manager
    └── state.db read/write, conflict state management
```

### IPC Protocol

JSON over Unix domain socket (newline-delimited):

```json
// Request
{"cmd": "link", "args": {"dir": "/path/to/dir", "repo": "my-repo", "name": "dotfiles"}}
{"cmd": "status", "args": {}}
{"cmd": "unlink", "args": {"dir": "/path/to/dir"}}

// Response
{"ok": true, "data": {...}}
{"ok": false, "error": "message"}
```

Commands: `link`, `connect`, `unlink`, `disconnect`, `status`, `push`, `pull`, `resolve`, `log`.

### Per-Link Filesystem Lock

The per-link lock is a **filesystem-level flock** on `~/.local/share/syncor/<link-id>/sync.lock`. Both the daemon and manual CLI commands (`syncor push`, `syncor pull`) acquire this lock before operating on a link. This prevents race conditions when both are running.

### Crash Recovery

**Daemon startup recovery:**
1. Check if `daemon.pid` exists
2. If PID file exists, check if process is alive (`kill -0`)
3. If process is dead: remove stale `daemon.pid` and `syncor.sock`, proceed with startup
4. If process is alive: report "daemon already running"

**Sync flow crash recovery:**
If the daemon crashes mid-sync (between catalog update and state.db update), the next sync cycle recovers:
1. On startup, for each link: compare local catalog's latest snapshot against `state.db.last_local_snapshot`
2. If catalog is ahead of state.db: the previous sync completed locally but state.db wasn't updated. Update state.db to match catalog, then check if transport push succeeded (compare local HEAD vs remote HEAD). If push succeeded, update `last_remote_revision`. If not, re-push.
3. Content-addressed storage makes re-pushes idempotent — pushing the same content twice is safe.

### Unresolved Conflicts Behavior

Links with unresolved conflicts in the `conflicts` table are **paused** — the daemon skips sync cycles for that link until `syncor resolve` clears all conflicts. `syncor status` shows these links as `conflict` state.

### Key Decisions

- **Per-link filesystem lock**: daemon and CLI both respect flock
- **Debounce**: configurable, default 2 seconds
- **Poll interval**: per-connect configurable, default 60 seconds
- **Works without daemon**: `syncor push` / `syncor pull` run directly (acquire same flock)
- **Logging**: `~/.local/share/syncor/daemon.log`

### Platform Support

- macOS: launchd + Unix domain socket (v1)
- Linux: systemd + Unix domain socket (v1)
- Windows: Windows Service + named pipe (future)

```rust
trait DaemonBackend {
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn status(&self) -> Result<DaemonStatus>;
    fn socket_path(&self) -> PathBuf;
}
```

## Crate Structure

```
syncor-core/
├── config.rs          # Config load/save (config.toml, links.toml)
├── link.rs            # LinkInfo, LinkState types
├── sync/
│   ├── engine.rs      # Sync orchestration (push/pull flow)
│   ├── conflict.rs    # ConflictResolver trait + detection (three-point)
│   └── state.rs       # state.db management (rusqlite)
├── transport/
│   ├── mod.rs         # SyncTransport trait
│   └── git.rs         # GitTransport implementation
├── watch/
│   ├── watcher.rs     # fsnotify wrapper + debounce
│   └── poller.rs      # Polling timer
└── daemon/
    ├── server.rs      # IPC server (JSON over unix socket)
    ├── worker.rs      # Sync job queue + per-link flock
    └── manager.rs     # Watcher/poller lifecycle management

syncor-cli/
└── main.rs            # clap CLI -> daemon IPC or direct execution
```

## Dependencies

| Crate | Purpose |
|-------|---------|
| chkpt-core | Storage engine (scanner, blob, pack, tree, catalog, index modules) |
| clap | CLI argument parsing |
| dialoguer | Interactive prompts |
| tokio | Async runtime, timers |
| notify | fsnotify file watching |
| rusqlite | state.db |
| git2 | GitTransport (libgit2 bindings) |
| serde + serde_json | IPC protocol, config |
| toml | Config file parsing |
| dirs | XDG path resolution |
| fs4 | Filesystem locking (per-link flock) |
| lz4_flex | LZ4 compression (chkpt-core's compression is private) |

## v1 Scope

- `link`, `connect`, `unlink`, `disconnect` commands
- `status`, `push`, `pull` commands
- `resolve` (interactive, local/remote choice only)
- `log` (sync event history from state.db)
- Daemon with fsnotify watcher + polling
- GitTransport (single backend)
- Conflict detection (three-point comparison) + InteractiveResolver
- macOS + Linux support

## Out of v1 Scope

- Windows support
- Text 3-way merge / external merge tools
- Auto-resolve rules
- Multiple transport backends
- Selective file sync (sync partial contents of a link)
