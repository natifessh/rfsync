# rfsync
lightweight local file synchronization tool
![rfsync](demo.gif)
# usage
you can use this dummy tool by running:
```bash
git clone https://github.com/natifessh/rfsync.git
cd rfsync
cargo run -- -s your/source_dir -b your/dest_dir -w bool to make the backup run every 10s
```

future plans

-- remote backup using ssh/scp on remote machines

-- make the watcher logic with notify 

-- add the -v option
```
