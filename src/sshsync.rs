//! Remote backup over ssh using SFTP. Host keys are checked against
//! ~/.ssh/known_hosts. Files are compared by size + mtime (uploads preserve the
//! local mtime), so no remote hashing is needed. Uploads run on several
//! independent ssh connections in parallel.
use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufReader},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::UNIX_EPOCH,
};

use ssh2::{CheckResult, FileStat, KnownHostFileKind, Session, Sftp};
use walkdir::DirEntry;

use crate::lfsync::{collect, is_excluded, relative_path, Opts, Stats};

#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// how to authenticate: private key file (+ optional passphrase), password, or ssh-agent
#[derive(Debug, Default)]
pub struct Auth {
    pub identity: Option<PathBuf>,
    /// the key passphrase when `identity` is set, otherwise the ssh password
    pub secret: Option<String>,
}

/// parse `user@host:/path`; anything else is a local path
pub fn parse_target(s: &str, port: u16) -> Option<Target> {
    let (user, rest) = s.split_once('@')?;
    let (host, path) = rest.split_once(':')?;
    if user.is_empty() || host.is_empty() || path.is_empty() || user.contains('/') {
        return None;
    }
    Some(Target {
        user: user.into(),
        host: host.into(),
        port,
        path: path.into(),
    })
}

pub fn connect(t: &Target, auth: &Auth) -> Result<Session, String> {
    let tcp = TcpStream::connect((t.host.as_str(), t.port))
        .map_err(|e| format!("cannot connect to {}:{}: {}", t.host, t.port, e))?;
    let mut session = Session::new().map_err(|e| e.to_string())?;
    session.set_tcp_stream(tcp);
    session.handshake().map_err(|e| format!("ssh handshake failed: {}", e))?;
    verify_host(&session, t)?;

    let result = match (&auth.identity, &auth.secret) {
        (Some(key), pass) => session.userauth_pubkey_file(&t.user, None, key, pass.as_deref()),
        (None, Some(pw)) => session.userauth_password(&t.user, pw),
        (None, None) => session.userauth_agent(&t.user),
    };
    result.map_err(|e| format!("authentication failed for {}: {}", t.user, e))?;
    if !session.authenticated() {
        return Err("authentication failed".into());
    }
    Ok(session)
}

fn verify_host(session: &Session, t: &Target) -> Result<(), String> {
    let mut kh = session.known_hosts().map_err(|e| e.to_string())?;
    let file = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".ssh/known_hosts"))
        .ok_or("HOME is not set")?;
    kh.read_file(&file, KnownHostFileKind::OpenSSH)
        .map_err(|e| format!("cannot read {}: {}", file.display(), e))?;
    let (key, _) = session.host_key().ok_or("server sent no host key")?;
    match kh.check_port(&t.host, t.port, key) {
        CheckResult::Match => Ok(()),
        CheckResult::Mismatch => Err(format!("HOST KEY MISMATCH for {} - refusing to connect", t.host)),
        _ => Err(format!(
            "{} is not in known_hosts; run `ssh {}@{}` once to trust it",
            t.host, t.user, t.host
        )),
    }
}

fn mtime_secs(m: &std::fs::Metadata) -> Option<u64> {
    m.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// join a relative local path onto a remote dir using `/` separators
fn remote_join(base: &Path, rel: &Path) -> PathBuf {
    let mut p = base.to_path_buf();
    for c in rel.components() {
        p.push(c.as_os_str());
    }
    p
}

fn ensure_dir(sftp: &Sftp, path: &Path) -> Result<(), String> {
    let mut cur = PathBuf::new();
    for c in path.components() {
        cur.push(c.as_os_str());
        if sftp.stat(&cur).is_err() {
            sftp.mkdir(&cur, 0o755)
                .map_err(|e| format!("mkdir {}: {}", cur.display(), e))?;
        }
    }
    Ok(())
}

fn upload(sftp: &Sftp, local: &Path, remote: &Path, mtime: Option<u64>) -> Result<(), String> {
    {
        let mut out = sftp.create(remote).map_err(|e| e.to_string())?;
        let mut input = BufReader::new(File::open(local).map_err(|e| e.to_string())?);
        io::copy(&mut input, &mut out).map_err(|e| e.to_string())?;
    } // handle closed here
    if let Some(m) = mtime {
        let stat = FileStat { size: None, uid: None, gid: None, perm: None, atime: Some(m), mtime: Some(m) };
        sftp.setstat(remote, stat).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn delete_remote(
    sftp: &Sftp,
    root: &Path,
    cur: &Path,
    opts: &Opts,
    keep: &HashSet<PathBuf>,
    stats: &mut Stats,
) {
    let Ok(children) = sftp.readdir(cur) else { return };
    for (path, st) in children {
        let rel = relative_path(&path, root);
        if is_excluded(&opts.excludes, &rel) {
            continue;
        }
        let keep_it = keep.contains(&rel);
        if st.is_dir() {
            delete_remote(sftp, root, &path, opts, keep, stats);
            // directories are cleaned up but not counted (same as local sync)
            if !keep_it && !opts.dry_run {
                let _ = sftp.rmdir(&path);
            }
        } else if !keep_it {
            if opts.dry_run {
                stats.deleted += 1;
                println!("would delete {:?}", rel);
                continue;
            }
            match sftp.unlink(&path) {
                Ok(_) => {
                    stats.deleted += 1;
                    if opts.verbose {
                        println!("deleted {:?}", rel);
                    }
                }
                Err(e) => {
                    stats.failed += 1;
                    eprintln!("failed to delete {:?}: {}", rel, e);
                }
            }
        }
    }
}

/// upload one file if the remote copy is missing or differs
fn process_file(sftp: &Sftp, entry: &DirEntry, src_root: &Path, root: &Path, opts: &Opts, stats: &mut Stats) {
    let rel = relative_path(entry.path(), src_root);
    let remote = remote_join(root, &rel);
    let Ok(meta) = entry.metadata() else {
        stats.failed += 1;
        eprintln!("cannot read metadata of {:?}", rel);
        return;
    };
    let mtime = mtime_secs(&meta);
    let up_to_date = sftp
        .stat(&remote)
        .map(|r| r.size == Some(meta.len()) && r.mtime == mtime)
        .unwrap_or(false);
    if up_to_date {
        return;
    }
    if opts.dry_run {
        stats.modified += 1;
        println!("would upload {:?}", rel);
        return;
    }
    match upload(sftp, entry.path(), &remote, mtime) {
        Ok(_) => {
            stats.modified += 1;
            if opts.verbose {
                println!("uploaded {:?}", rel);
            }
        }
        Err(e) => {
            stats.failed += 1;
            eprintln!("failed to upload {:?}: {}", rel, e);
        }
    }
}

pub fn sync_remote(
    session: &Session,
    t: &Target,
    auth: &Auth,
    src: &str,
    opts: &Opts,
    jobs: usize,
) -> Result<Stats, String> {
    let src_root = Path::new(src);
    let root = Path::new(&t.path);
    let sftp = session.sftp().map_err(|e| e.to_string())?;
    if !opts.dry_run {
        ensure_dir(&sftp, root)?;
    }

    let entries = collect(src_root, &opts.excludes);
    let mut stats = Stats { checked: entries.len(), ..Default::default() };

    // directories first (sequential, cheap), so uploads never race on mkdir
    if !opts.dry_run {
        for e in entries.iter().filter(|e| e.file_type().is_dir()) {
            let remote = remote_join(root, &relative_path(e.path(), src_root));
            if let Err(err) = ensure_dir(&sftp, &remote) {
                stats.failed += 1;
                eprintln!("{}", err);
            }
        }
    }

    // files: N workers, each on its own connection, pulling from a shared counter
    let files: Vec<&DirEntry> = entries.iter().filter(|e| e.file_type().is_file()).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let merged = Mutex::new(Stats::default());
    let workers = jobs.min(files.len());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let sess = match connect(t, auth) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("worker connection failed: {}", e);
                        return;
                    }
                };
                let sftp = match sess.sftp() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("worker sftp failed: {}", e);
                        return;
                    }
                };
                let mut local = Stats::default();
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(entry) = files.get(i) else { break };
                    process_file(&sftp, entry, src_root, root, opts, &mut local);
                    done.fetch_add(1, Ordering::SeqCst);
                }
                let mut m = merged.lock().unwrap();
                m.modified += local.modified;
                m.failed += local.failed;
            });
        }
    });

    let m = merged.into_inner().unwrap();
    stats.modified += m.modified;
    stats.failed += m.failed;
    // files nobody got to (e.g. every worker failed to connect) count as failures
    stats.failed += files.len().saturating_sub(done.load(Ordering::SeqCst));

    if opts.delete {
        let keep: HashSet<PathBuf> = entries.iter().map(|e| relative_path(e.path(), src_root)).collect();
        delete_remote(&sftp, root, root, opts, &keep, &mut stats);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_targets() {
        let t = parse_target("nati@box.lan:/srv/backup", 2222).unwrap();
        assert_eq!(
            t,
            Target { user: "nati".into(), host: "box.lan".into(), port: 2222, path: "/srv/backup".into() }
        );
    }

    #[test]
    fn local_paths_are_not_remote() {
        assert!(parse_target("/home/me/backup", 22).is_none());
        assert!(parse_target("./a@b", 22).is_none());
        assert!(parse_target("backup", 22).is_none());
        assert!(parse_target("user@host:", 22).is_none());
    }

    #[test]
    fn remote_join_uses_forward_slashes() {
        let p = remote_join(Path::new("/srv/bk"), Path::new("a/b/c.txt"));
        assert_eq!(p, PathBuf::from("/srv/bk/a/b/c.txt"));
    }
}
