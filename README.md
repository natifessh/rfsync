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
| `-w, --watch` | keep running; re-sync when files change (via `notify`) |
| `-v, --verbose` | progress bar (local) / per-file output (remote) |
| `-e, --exclude <glob>` | skip matching paths, repeatable. Matched against the full relative path and every path component, e.g. `-e target -e '*.log'` |
| `--delete` | remove files from the destination that are gone from the source (excluded paths are left alone) |
| `--debounce-ms <n>` | quiet period used to batch file events in watch mode (default 500) |
| `--port <n>` | ssh port for remote destinations (default 22) |

Examples:
```bash
rfsync -s ~/projects -b /mnt/usb/projects -e target -e node_modules --delete
rfsync -s ~/docs -b me@server:/srv/backup/docs -w
```

### remote backup notes
- Authentication uses your running **ssh-agent** (no passwords on the command line).
- The host must already be in `~/.ssh/known_hosts` (run `ssh user@host` once). Mismatched keys are refused.
- Files are compared by size + modification time; uploads preserve the local mtime.
- Local sync compares content with blake3 hashes (size is checked first).

## tests
```bash
cargo test
```

## future plans
- parallel remote uploads
- password / key-file auth for remote
