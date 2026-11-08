mod config;
mod lfsync;
mod sshsync;

use std::{
    path::Path,
    sync::mpsc::{channel, RecvTimeoutError},
    time::Duration,
};

use clap::Parser;
use config::{load_config, resolve, Cmd, Config};
use lfsync::{build_excludes, is_excluded, relative_path, sync_local, validate_paths, Opts};
use notify::{EventKind, RecursiveMode, Watcher};

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {}", msg);
    std::process::exit(1);
}

fn main() {
    let cmd = Cmd::parse();
    let cfg = match &cmd.config {
        Some(path) => load_config(path).unwrap_or_else(|e| fail(e)),
        None => Config::default(),
    };
    let s = resolve(cmd, cfg).unwrap_or_else(|e| fail(e));

    let opts = Opts {
        excludes: build_excludes(&s.exclude).unwrap_or_else(|e| fail(e)),
        delete: s.delete,
        verbose: s.verbose,
        dry_run: s.dry_run,
    };
    let remote = sshsync::parse_target(&s.destination, s.port);

    match &remote {
        None => validate_paths(&s.source, &s.destination, !s.dry_run).unwrap_or_else(|e| fail(e)),
        Some(_) if !Path::new(&s.source).is_dir() => {
            fail(format!("source '{}' is not an existing directory", s.source))
        }
        Some(_) => {}
    }

    // ask for the secret once up front so watch mode can reconnect without prompting
    let mut auth = sshsync::Auth { identity: s.identity.clone(), secret: None };
    if let (Some(t), true) = (&remote, s.ask_pass) {
        let what = if s.identity.is_some() { "Key passphrase" } else { "Password" };
        let secret = rpassword::prompt_password(format!("{} for {}@{}: ", what, t.user, t.host))
            .unwrap_or_else(|e| fail(format!("cannot read password: {}", e)));
        auth.secret = Some(secret);
    }

    // one sync pass; remote mode reconnects each time so a dropped link heals itself
    let run = || match &remote {
        None => sync_local(&s.source, &s.destination, &opts).report(s.dry_run),
        Some(t) => {
            let result = sshsync::connect(t, &auth)
                .and_then(|sess| sshsync::sync_remote(&sess, t, &auth, &s.source, &opts, s.jobs));
            match result {
                Ok(stats) => stats.report(s.dry_run),
                Err(e) => eprintln!("remote sync failed: {}", e),
            }
        }
    };

    run();
    if !s.watch {
        return;
    }

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(tx).unwrap_or_else(|e| fail(e));
    watcher
        .watch(Path::new(&s.source), RecursiveMode::Recursive)
        .unwrap_or_else(|e| fail(e));
    println!("watching {} (ctrl-c to stop)", s.source);

    let src_root = Path::new(&s.source);
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
            match rx.recv_timeout(Duration::from_millis(s.debounce_ms)) {
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
