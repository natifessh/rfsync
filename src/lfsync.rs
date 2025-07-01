pub mod lfsync{
    use std::{fs::File, io::{BufReader, Read}, path::{Path, PathBuf}, sync::{atomic::{AtomicUsize, Ordering}, Arc}};
    use blake3::Hash;
    use clap::Parser;
    use fs_extra::file::copy as copy_file;
    use indicatif::{ProgressBar, ProgressStyle};
    use lazy_static::lazy_static;
    use walkdir::WalkDir;
    use rayon::{iter::{IntoParallelRefIterator, ParallelIterator}, *};
    lazy_static!{
    pub static ref PROGRESS_BAR: ProgressBar = {
        let progress_bar = ProgressBar::new(0);
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("[{msg}\n{elapsed_precise}] [{bar:40.white/blue}] {pos}/{len} ({eta}) "),
        );
        progress_bar
    };
}
#[derive(Parser,Debug)]
#[command(version, about, long_about = None)]
pub struct Cmd{
    #[arg(short='s',long="source")]
    pub source:String,
    #[arg(short='b',long="backup")]
    pub destination:String,
    #[arg(short='w',long="watch")]
    pub  watch:Option<bool>,
}
pub fn bar_init(u:u64){
    PROGRESS_BAR.set_length(u);
    PROGRESS_BAR.set_draw_delta(u.max(1000)/100);
    PROGRESS_BAR.set_position(0);
}
pub fn compute_lhash(path: &Path) -> std::io::Result<Hash> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 8192];
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
    if !dst.exists()||!src.exists() {
        return false;
    }
    let h1 = compute_lhash(src).ok();
    let h2 = compute_lhash(dst).ok();
    match (h1, h2) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
fn relative_path(from: &Path, base: &Path) -> PathBuf {
    from.strip_prefix(base).unwrap_or(from).to_path_buf()
}
pub fn sync_local(src:&str,dst:&str){
    let src_root = Path::new(src);
    let mut modified_files=Arc::new(AtomicUsize::new(0));;
    let backup_root = Path::new(dst);
    let entries: Vec<_> = WalkDir::new(src_root)
    .into_iter()
    .filter_map(Result::ok)
    .filter(|e| e.file_type().is_file() || e.file_type().is_dir()) 
    .collect();
    let total_files = entries.len();
    bar_init(total_files as u64);
    entries.par_iter().for_each(|entry| {
        let src_path = entry.path();
        if entry.file_type().is_file(){
            let rel_path = relative_path(src_path, src_root);
            let dst_path = backup_root.join(&rel_path);
            PROGRESS_BAR.set_message(&format!("no change {:?}",rel_path));
            let should_copy = !dst_path.exists() || !compare_hash(src_path, &dst_path);
            if should_copy {
                if let Some(parent) = dst_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                }
                PROGRESS_BAR.set_message("overwriting file/dir");
                copy_file(src_path, &dst_path, &fs_extra::file::CopyOptions{
                    overwrite:true,

                    ..Default::default()
                })
                    .expect("Copy failed");
                modified_files.fetch_add(1, Ordering::Relaxed);
            } 
            PROGRESS_BAR.inc(1);
            /* let meta = fs::metadata(src_path).unwrap();
            let size = meta.len();
            let modified: DateTime<Utc> = meta.modified().unwrap_or(UNIX_EPOCH).into();
            println!("  Size: {} bytes", size);
            println!("  Modified: {}", modified.format("%Y-%m-%d %H:%M:%S")); */
        }else if entry.file_type().is_dir() { 
            let rel_path = relative_path(src_path, src_root);
            let dst_dir = backup_root.join(&rel_path);
            if !dst_dir.exists() {
                PROGRESS_BAR.set_message(&format!("Creating directory {:?}", rel_path));
                if let Err(e) = std::fs::create_dir_all(&dst_dir) {
                    eprintln!("Error creating directory {}: {}", dst_dir.display(), e);
                }
            }
            PROGRESS_BAR.inc(1); 
        }
    } 
);
    PROGRESS_BAR.finish_with_message(&format!("backup completed {:?} files checked {:?} files added/overwritten in {:?} ",entries.len(),&modified_files,backup_root,));
}
}
