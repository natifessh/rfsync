mod lfsync;
mod sshsync;

use std::{
    path::Path,
    sync::mpsc::{channel, RecvTimeoutError},
    time::Duration,
};

use clap::Parser;
use lfsync::{build_excludes, is_excluded, relative_path, sync_local, validate_paths, Cmd, Opts};
use notify::{EventKind, RecursiveMode, Watcher};

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {}", msg);
    std::process::exit(1);
}

fn main() {
    let args = Cmd::parse();
    let opts = Opts {
        excludes: build_excludes(&args.exclude).unwrap_or_else(|e| fail(e)),
        delete: args.delete,
        verbose: args.verbose,
    };
    let remote = sshsync::parse_target(&args.destination, args.port);

    match &remote {
        None => validate_paths(&args.source, &args.destination).unwrap_or_else(|e| fail(e)),
        Some(_) if !Path::new(&args.source).is_dir() => {
            fail(format!("source '{}' is not an existing directory", args.source))
        }
        Some(_) => {}
    }

    // one sync pass; remote mode reconnects each time so a dropped link heals itself
    let run = || match &remote {
        None => sync_local(&args.source, &args.destination, &opts).report(),
        Some(t) => match sshsync::connect(t).and_then(|s| sshsync::sync_remote(&s, &args.source, &t.path, &opts)) {
            Ok(stats) => stats.report(),
            Err(e) => eprintln!("remote sync failed: {}", e),
        },
    };

    run();
    if !args.watch {
        return;
    }

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(tx).unwrap_or_else(|e| fail(e));
    watcher
        .watch(Path::new(&args.source), RecursiveMode::Recursive)
        .unwrap_or_else(|e| fail(e));
    println!("watching {} (ctrl-c to stop)", args.source);

    let src_root = Path::new(&args.source);
    let relevant = |ev: &notify::Event| {
        matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
            && ev
                .paths
                .iter()
                .any(|p| !is_excluded(&opts.excludes, &relative_path(p, src_root)))
    };

    while let Ok(first) = rx.recv() {
        let mut dirty = first.map(|e| relevant(&e)).unwrap_or(false);
        // batch a burst of events: keep draining until the source is quiet
        loop {
            match rx.recv_timeout(Duration::from_millis(args.debounce_ms)) {
                Ok(ev) => dirty |= ev.map(|e| relevant(&e)).unwrap_or(false),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if dirty {
            run();
        }
    }
}
