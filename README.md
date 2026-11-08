# rfsync
lightweight file synchronization tool (local and over ssh)

![rfsync](demo.gif)

## usage
```bash
git clone https://github.com/natifessh/rfsync.git
cd rfsync
cargo run --release -- -s your/source_dir -b your/dest_dir
```

| flag | meaning |
|------|---------|
| `-s, --source <dir>` | source directory |
| `-b, --backup <dest>` | local directory, or `user@host:/remote/path` for ssh |
| `-n, --dry-run` | show what would be copied/deleted, change nothing (creates no directories either) |
| `-w, --watch` | keep running; re-sync when files change (via `notify`) |
| `-v, --verbose` | progress bar (local) / per-file output (remote) |
| `-e, --exclude <glob>` | skip matching paths, repeatable. Matched against the full relative path and every path component, e.g. `-e target -e '*.log'` |
| `--delete` | remove files from the destination that are gone from the source (excluded paths are left alone) |
| `-c, --config <file>` | read settings from a TOML file (see below) |
| `--debounce-ms <n>` | quiet period used to batch file events in watch mode (default 500) |
| `--port <n>` | ssh port for remote destinations (default 22) |
| `-j, --jobs <n>` | parallel ssh connections for remote uploads (default 4) |
| `-i, --identity <keyfile>` | ssh private key file (default: ssh-agent) |
| `--ask-pass` | prompt for the ssh password, or the key passphrase when `-i` is given |

Examples:
```bash
rfsync -s ~/projects -b /mnt/usb/projects -e target -e node_modules --delete -n   # preview first
rfsync -s ~/projects -b /mnt/usb/projects -e target -e node_modules --delete
rfsync -s ~/docs -b me@server:/srv/backup/docs -w
rfsync -s ~/docs -b me@server:/srv/backup/docs -i ~/.ssh/id_ed25519 --ask-pass -j 8
```

### config file
Anything you pass on the command line overrides the file; `exclude` lists from both are combined.
```toml
# rfsync.toml
source = "/home/me/projects"
backup = "me@server:/srv/backup/projects"
exclude = ["target", "node_modules", "*.log"]
delete = true
watch = true
jobs = 8
identity = "/home/me/.ssh/id_ed25519"
# also: verbose, dry_run, port, debounce_ms, ask_pass
```
```bash
rfsync -c rfsync.toml
rfsync -c rfsync.toml -n      # same settings, but preview only
```
Unknown keys are rejected, so typos get caught. Passwords are never read from the config file; use `--ask-pass`.

### remote backup notes
- Auth order: `--identity` key file, else password with `--ask-pass`, else your running **ssh-agent**.
- The host must already be in `~/.ssh/known_hosts` (run `ssh user@host` once). Mismatched keys are refused.
- Files are compared by size + modification time; uploads preserve the local mtime.
- `--jobs N` opens N extra ssh connections to upload files in parallel (plus one for directories and cleanup), so keep it under your server's `MaxStartups`/`MaxSessions` limits.
- Local sync compares content with blake3 hashes (size is checked first).

## tests
```bash
cargo test
```

## future plans
- bandwidth limit / resume for large remote files
- `rfsync.toml` auto-discovery in the current directory
