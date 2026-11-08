use std::{
    collections::HashSet,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use blake3::Hash;
use fs_extra::file::copy as copy_file;
use globset::{Glob, GlobSet, GlobSetBuilder};
use indicatif::{ProgressBar, ProgressStyle};
use lazy_static::lazy_static;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use walkdir::WalkDir;

lazy_static! {
    pub static ref PROGRESS_BAR: ProgressBar = {
        let progress_bar = ProgressBar::new(0);
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("[{msg}\n{elapsed_precise}] [{bar:40.white/blue}] {pos}/{len} ({eta}) "),
        );
        progress_bar
    };
}

/// options shared by local and remote sync
pub struct Opts {
    pub excludes: GlobSet,
    pub delete: bool,
    pub verbose: bool,
    pub dry_run: bool,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub checked: usize,
    pub modified: usize,
    pub failed: usize,
    pub deleted: usize,
}

impl Stats {
    pub fn report(&self, dry_run: bool) {
        if dry_run {
            println!(
                "🔎 Dry run: {} checked, {} would be copied, {} would be deleted, {} failed.",
                self.checked, self.modified, self.deleted, self.failed
            );
        } else {
            println!(
                "✅ Backup completed. {} checked, {} modified, {} deleted, {} failed.",
                self.checked, self.modified, self.deleted, self.failed
            );
        }
    }
}

pub fn build_excludes(patterns: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).map_err(|e| format!("bad exclude pattern '{}': {}", p, e))?);
    }
    b.build().map_err(|e| e.to_string())
}

/// a path is excluded if the whole relative path or any single component matches
pub fn is_excluded(set: &GlobSet, rel: &Path) -> bool {
    !set.is_empty()
        && (set.is_match(rel) || rel.components().any(|c| set.is_match(c.as_os_str())))
}

/// initialize progress bar
pub fn bar_init(u: u64) {
    PROGRESS_BAR.set_length(u);
    PROGRESS_BAR.set_draw_delta(u.max(1000) / 100);
    PROGRESS_BAR.set_position(0);
}

/// compute file's hash using blake3
pub fn compute_lhash(path: &Path) -> std::io::Result<Hash> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize())
}

fn compare_hash(src: &Path, dst: &Path) -> bool {
    // different sizes (or missing files) can never match, so skip hashing
    match (std::fs::metadata(src), std::fs::metadata(dst)) {
        (Ok(a), Ok(b)) if a.len() == b.len() => {}
        _ => return false,
    }
    match (compute_lhash(src).ok(), compute_lhash(dst).ok()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

pub fn relative_path(from: &Path, base: &Path) -> PathBuf {
    from.strip_prefix(base).unwrap_or(from).to_path_buf()
}

/// all files and directories under `root` (including root), minus excluded subtrees
pub fn collect(root: &Path, excludes: &GlobSet) -> Vec<walkdir::DirEntry> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_excluded(excludes, &relative_path(e.path(), root)))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() || e.file_type().is_dir())
        .collect()
}

/// canonicalize a path that may not exist yet: resolve the deepest existing
/// ancestor and re-append the missing components
fn resolve_path(p: &Path) -> std::io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        match cur.canonicalize() {
            Ok(mut out) => {
                out.extend(missing.iter().rev());
                return Ok(out);
            }
            Err(e) => match (cur.file_name().map(|n| n.to_owned()), cur.parent()) {
                (Some(name), Some(parent)) => {
                    missing.push(name);
                    cur = if parent.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        parent.to_path_buf()
                    };
                }
                _ => return Err(e),
            },
        }
    }
}

/// check the source exists and the destination is not inside it; create the
/// destination when `create` is set (never in a dry run)
pub fn validate_paths(src: &str, dst: &str, create: bool) -> Result<(), String> {
    let src_root = Path::new(src);
    if !src_root.is_dir() {
        return Err(format!("source '{}' is not an existing directory", src));
    }
    let s = src_root.canonicalize().map_err(|e| e.to_string())?;
    let d = resolve_path(Path::new(dst)).map_err(|e| format!("bad destination '{}': {}", dst, e))?;
    if d.starts_with(&s) {
        return Err("destination must not be inside the source directory".into());
    }
    if create {
        std::fs::create_dir_all(dst).map_err(|e| format!("cannot create '{}': {}", dst, e))?;
    }
    Ok(())
}

/// remove everything in `dst_root` that is not in `keep` (and not excluded)
fn delete_extraneous(
    dst_root: &Path,
    excludes: &GlobSet,
    keep: &HashSet<PathBuf>,
    verbose: bool,
    dry_run: bool,
    stats: &mut Stats,
) {
    // contents_first so files go before the directories that hold them
    for e in WalkDir::new(dst_root)
        .contents_first(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        let rel = relative_path(e.path(), dst_root);
        if rel.as_os_str().is_empty() || keep.contains(&rel) || is_excluded(excludes, &rel) {
            continue;
        }
        if e.file_type().is_dir() {
            // directories are cleaned up but not counted; one may legitimately
            // stay behind if it still holds excluded files
            if !dry_run {
                let _ = std::fs::remove_dir(e.path());
            }
        } else if dry_run {
            stats.deleted += 1;
            println!("would delete {:?}", rel);
        } else {
            match std::fs::remove_file(e.path()) {
                Ok(_) => {
                    stats.deleted += 1;
                    if verbose {
                        println!("deleted {:?}", rel);
                    }
                }
                Err(err) => {
                    stats.failed += 1;
                    eprintln!("failed to delete {:?}: {}", rel, err);
                }
            }
        }
    }
}

/// copy files to dst when they are new or their hashes differ
pub fn sync_local(src: &str, dst: &str, opts: &Opts) -> Stats {
    let src_root = Path::new(src);
    let backup_root = Path::new(dst);
    let modified = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    let entries = collect(src_root, &opts.excludes);
    let show_bar = opts.verbose && !opts.dry_run;
    if show_bar {
        bar_init(entries.len() as u64);
    }

    entries.par_iter().for_each(|entry| {
        let src_path = entry.path();
        let rel_path = relative_path(src_path, src_root);
        let dst_path = backup_root.join(&rel_path);

        if entry.file_type().is_file() {
            let should_copy = !compare_hash(src_path, &dst_path);
            if show_bar {
                PROGRESS_BAR.set_message(&format!(
                    "{} {:?}",
                    if should_copy { "Copying" } else { "No change" },
                    rel_path
                ));
            }
            if should_copy && opts.dry_run {
                modified.fetch_add(1, Ordering::Relaxed);
                println!("would copy {:?}", rel_path);
            } else if should_copy {
                let result = match dst_path.parent() {
                    Some(parent) => std::fs::create_dir_all(parent).map_err(|e| e.to_string()),
                    None => Ok(()),
                }
                .and_then(|_| {
                    copy_file(
                        src_path,
                        &dst_path,
                        &fs_extra::file::CopyOptions {
                            overwrite: true,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| e.to_string())
                });
                match result {
                    Ok(_) => {
                        modified.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        eprintln!("failed to copy {:?}: {}", rel_path, e);
                    }
                }
            }
        } else if opts.dry_run {
            // directories are created implicitly; nothing to report
        } else if let Err(e) = std::fs::create_dir_all(&dst_path) {
            failed.fetch_add(1, Ordering::Relaxed);
            eprintln!("failed to create {:?}: {}", rel_path, e);
        }
        if show_bar {
            PROGRESS_BAR.inc(1);
        }
    });

    let mut stats = Stats {
        checked: entries.len(),
        modified: modified.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
        deleted: 0,
    };

    if opts.delete {
        let keep: HashSet<PathBuf> = entries
            .iter()
            .map(|e| relative_path(e.path(), src_root))
            .collect();
        delete_extraneous(backup_root, &opts.excludes, &keep, opts.verbose, opts.dry_run, &mut stats);
    }

    if show_bar {
        PROGRESS_BAR.finish_with_message(&format!(
            "Backup completed. Checked {}, modified {}, deleted {}, failed {}.",
            stats.checked, stats.modified, stats.deleted, stats.failed
        ));
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn opts(excludes: &[&str], delete: bool) -> Opts {
        let pats: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();
        Opts {
            excludes: build_excludes(&pats).unwrap(),
            delete,
            verbose: false,
            dry_run: false,
        }
    }

    fn dry(excludes: &[&str], delete: bool) -> Opts {
        Opts { dry_run: true, ..opts(excludes, delete) }
    }

    fn run(s: &Path, d: &Path, o: &Opts) -> Stats {
        sync_local(s.to_str().unwrap(), d.to_str().unwrap(), o)
    }

    #[test]
    fn copies_then_is_idempotent() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(s.join("a")).unwrap();
        fs::write(s.join("a/f"), "hi").unwrap();
        fs::write(s.join("g"), "yo").unwrap();

        let first = run(&s, &d, &opts(&[], false));
        assert_eq!(first.modified, 2);
        assert_eq!(fs::read_to_string(d.join("a/f")).unwrap(), "hi");

        let second = run(&s, &d, &opts(&[], false));
        assert_eq!(second.modified, 0);
        assert_eq!(second.failed, 0);
    }

    #[test]
    fn detects_changed_content_with_same_size() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("f"), "aaaa").unwrap();
        run(&s, &d, &opts(&[], false));
        fs::write(s.join("f"), "bbbb").unwrap();
        assert_eq!(run(&s, &d, &opts(&[], false)).modified, 1);
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "bbbb");
    }

    #[test]
    fn exclude_skips_files_and_dirs() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(s.join("target/deep")).unwrap();
        fs::write(s.join("target/deep/x"), "x").unwrap();
        fs::write(s.join("keep.txt"), "k").unwrap();
        fs::write(s.join("noise.log"), "n").unwrap();

        run(&s, &d, &opts(&["target", "*.log"], false));
        assert!(d.join("keep.txt").exists());
        assert!(!d.join("target").exists());
        assert!(!d.join("noise.log").exists());
    }

    #[test]
    fn delete_mirrors_removals_but_spares_excluded() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("a"), "a").unwrap();
        fs::write(s.join("b"), "b").unwrap();
        let o = opts(&["*.log"], true);
        run(&s, &d, &o);
        fs::write(d.join("keep.log"), "mine").unwrap(); // excluded, must survive
        fs::create_dir_all(d.join("olddir")).unwrap();
        fs::write(d.join("olddir/z"), "z").unwrap();
        fs::remove_file(s.join("b")).unwrap();

        let st = run(&s, &d, &o);
        assert!(d.join("a").exists());
        assert!(!d.join("b").exists());
        assert!(!d.join("olddir").exists());
        assert!(d.join("keep.log").exists());
        assert_eq!(st.deleted, 2); // b and olddir/z (directories are not counted)
    }

    #[test]
    fn without_delete_flag_extras_stay() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(&s).unwrap();
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("extra"), "e").unwrap();
        run(&s, &d, &opts(&[], false));
        assert!(d.join("extra").exists());
    }

    #[test]
    fn validate_rejects_bad_paths() {
        let t = tempdir().unwrap();
        let s = t.path().join("s");
        fs::create_dir_all(&s).unwrap();
        let inside = s.join("x");
        assert!(validate_paths(s.to_str().unwrap(), inside.to_str().unwrap(), true).is_err());
        assert!(!inside.exists());
        assert!(validate_paths("/definitely/not/here", t.path().to_str().unwrap(), true).is_err());
        assert!(validate_paths(s.to_str().unwrap(), t.path().join("ok").to_str().unwrap(), true).is_ok());
    }

    #[test]
    fn validate_without_create_leaves_no_trace() {
        let t = tempdir().unwrap();
        let s = t.path().join("s");
        fs::create_dir_all(&s).unwrap();
        let new_dst = t.path().join("a/b/c");
        assert!(validate_paths(s.to_str().unwrap(), new_dst.to_str().unwrap(), false).is_ok());
        assert!(!t.path().join("a").exists());
        // still catches nested-in-source destinations that don't exist yet
        let nested = s.join("x/y");
        assert!(validate_paths(s.to_str().unwrap(), nested.to_str().unwrap(), false).is_err());
        assert!(!s.join("x").exists());
    }

    #[test]
    fn dry_run_changes_nothing_but_reports() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("new"), "n").unwrap();
        fs::write(s.join("same"), "s").unwrap();
        run(&s, &d, &opts(&[], false));
        fs::write(s.join("new2"), "n2").unwrap();
        fs::write(d.join("stale"), "x").unwrap();
        fs::remove_file(d.join("same")).unwrap();
        fs::write(d.join("same"), "DIFFERENT").unwrap();

        let st = run(&s, &d, &dry(&[], true));
        assert_eq!(st.modified, 2); // new2 + same
        assert_eq!(st.deleted, 1); // stale
        assert!(!d.join("new2").exists());
        assert!(d.join("stale").exists());
        assert_eq!(fs::read_to_string(d.join("same")).unwrap(), "DIFFERENT");
    }

    #[test]
    fn dry_run_does_not_create_destination() {
        let t = tempdir().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        fs::create_dir_all(s.join("sub")).unwrap();
        fs::write(s.join("sub/f"), "f").unwrap();
        let st = run(&s, &d, &dry(&[], false));
        assert_eq!(st.modified, 1);
        assert!(!d.exists());
    }

    #[test]
    fn bad_glob_is_an_error() {
        assert!(build_excludes(&["[".to_string()]).is_err());
    }
}
