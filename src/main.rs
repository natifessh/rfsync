use blake3::Hash;
use chrono::{DateTime, Utc};
use clap::{builder::Str, Parser};
use indicatif::{ProgressBar, ProgressStyle};
use ssh2::Session;
use walkdir::WalkDir;
use std::{
    env::Args, fs::{self, metadata, File}, io::{BufReader, Read, Write}, net::TcpStream, os::unix::thread, path::{Path, PathBuf}, time::{Duration, SystemTime, UNIX_EPOCH}
};
use fs_extra::{dir::{self, CopyOptions}, file::copy as copy_file};
use lazy_static::lazy_static;
lazy_static!{
    pub static ref PROGRESS_BAR: ProgressBar = {
        let progress_bar = ProgressBar::new(0);
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("[{msg}\n{elapsed_precise}] [{bar:40.green/blue}] {pos}/{len} ({eta}) "),
        );
        progress_bar
    };
}
fn bar_init(u:u64){
    PROGRESS_BAR.set_length(u);
    PROGRESS_BAR.set_draw_delta(u/1000);
    PROGRESS_BAR.set_position(0);
}
#[derive(Parser,Debug)]
#[command(version, about, long_about = None)]
pub struct Cmd{
    #[arg(short='s',long="source")]
    pub source:Option<String>,
    #[arg(short='d',long="destination")]
    pub destination:Option<String>,
    #[arg(short='w',long="watch")]
    pub  watch:Option<bool>,
    #[arg(short='r',long="remote")]
    pub remote:Option<bool>,
    #[arg(short='u',long="username", requires = "remote")]
    pub username:Option<String>,
    #[arg(short='p',long="password", requires = "remote")]
    pub password:Option<String>,
    #[arg(short='h',long="host", requires = "remote")]
    pub host:Option<String>
}
fn compute_lhash(path: &Path) -> std::io::Result<Hash> {
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
fn watcher<'a>(source:&'a str,backup:&'a str){
    let src_root = Path::new(source);
    let backup_root = Path::new(backup);
    let entries: Vec<_> = WalkDir::new(src_root)
    .into_iter()
    .filter_map(Result::ok)
    .filter(|e| e.file_type().is_file() || e.file_type().is_dir()) 
    .collect();

    let total_files = entries.len();
    bar_init(total_files as u64);
    for entry in WalkDir::new(src_root).into_iter().filter_map(Result::ok) {
        let src_path = entry.path();
       

        if entry.file_type().is_file() {
            let rel_path = relative_path(src_path, src_root);
            let dst_path = backup_root.join(&rel_path);
            PROGRESS_BAR.set_message(&format!("Backingup {:?}",rel_path));
            let should_copy = !dst_path.exists() || !compare_hash(src_path, &dst_path);

            if should_copy {
                
                if let Some(parent) = dst_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                }
                copy_file(src_path, &dst_path, &fs_extra::file::CopyOptions{
                    overwrite:true,

                    ..Default::default()
                })
                    .expect("Copy failed");
            } 
            PROGRESS_BAR.inc(1);
            /* let meta = fs::metadata(src_path).unwrap();
            let size = meta.len();
            let modified: DateTime<Utc> = meta.modified().unwrap_or(UNIX_EPOCH).into();
            println!("  Size: {} bytes", size);
            println!("  Modified: {}", modified.format("%Y-%m-%d %H:%M:%S")); */
        }
    }
    PROGRESS_BAR.finish_with_message(&format!("Completed backup {:?}",src_root));
    PROGRESS_BAR.finish();

}
fn write_to_remote(src:&str,host:&str  ,dest:&str,paswd:&str,usme:&str){
    let tcp=TcpStream::connect(host).expect("fail");
    let mut session=Session::new().expect("fail");
    let mtd=metadata(src).unwrap();
    session.set_tcp_stream(tcp);
    session.handshake().unwrap();
    session.userauth_password(usme,paswd).expect("auth failure");
    let mut remote_file=session.scp_send(Path::new(
        dest
    ), 0o644, mtd.len(), None).expect("failed file..");
    let mut local_file = File::open(src).unwrap();
    let mut buffer = [0u8; 8192];
    loop {
        let n = local_file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        remote_file.write_all(&buffer[..n]).unwrap();
    }

    remote_file.send_eof().unwrap();
    remote_file.wait_eof().unwrap();
    remote_file.close().unwrap();
    remote_file.wait_close().unwrap();
   

}
fn read_from_remote(src:&str,host:&'static str  ,dest:&str,paswd:&str,usme:&str){
    let mut tcp=TcpStream::connect(host).expect("conn failure");
    let mut session=Session::new().unwrap();
    session.set_tcp_stream(tcp);
    session.handshake().unwrap();
    session.userauth_agent(usme).unwrap();
    let(mut remote_file,stat)=session.scp_recv(Path::new(src)).unwrap();
    let mut contents=Vec::new();
    remote_file.read_to_end(&mut contents).unwrap();
    remote_file.send_eof().unwrap();
    remote_file.wait_eof().unwrap();
    remote_file.close().unwrap();
    remote_file.wait_close().unwrap();
}
fn compare_rhash(session:&Session,path:&Path)->std::io::Result<Hash>{
    let(mut remote_file,_stat)=session.scp_recv(&path).unwrap();
    let mut hasher=blake3::Hasher::new();
    let mut buffer=[0u8;8192];
    loop{
        let n=remote_file.read(&mut buffer)?;
        if n==0{
            break;
        }
        hasher.update(&buffer[..n]);
    }
    remote_file.send_eof().unwrap();
    remote_file.wait_eof().unwrap();
    remote_file.close().unwrap();
    remote_file.wait_close().unwrap();
    Ok(hasher.finalize())
}

fn main() {
     let args = Cmd::parse();
     if args.remote.unwrap(){
        match (args.host.unwrap(),args.username.unwrap(),args.password.unwrap(),args.source.unwrap(),args.destination.unwrap()) {
               (h,u,p,s,d)=>{
                let mut session=Session::new().unwrap();
                let tcp=TcpStream::connect(&h).unwrap();
                session.handshake().unwrap();
                session.set_tcp_stream(tcp);
                session.handshake().unwrap();
                session.userauth_password(&u, &p).unwrap();
                let dst=Path::new(&d);
                let src=Path::new(&s);
                let remote_hash=compare_rhash(&session, &dst).unwrap();
                let local_hash=compute_lhash(&src).unwrap();
                let should_copy=remote_hash!=local_hash;
                if should_copy{
                    write_to_remote(&s, &h,& d,&p,&u);
                }
               },

            _=>todo!()
            
        }

     }else{

    match (args.source, args.destination) {
        (Some(src), Some(bcu)) => {
            if args.watch.unwrap_or(false) {
                loop {
                    watcher(&src, &bcu);
                    std::thread::sleep(Duration::from_secs(10));
                }
            } else {
                watcher(&src, &bcu);
            }
        },
        _ => {
            eprintln!("Usag error: --source <path> and --backup <path> are required.");
            std::process::exit(1);
        }
    } 
     }
}
fn remote_watcher(session:&Session,path:&str)->Vec<String>{
    let mut channel=session.channel_session().unwrap();
    channel.exec(&format!("find {}",path)).unwrap();
    let mut s=String::new();
    channel.read_to_string(&mut s).unwrap();
    channel.send_eof().unwrap();
    channel.wait_close().unwrap();
    s.lines()
    .map(|line| line.trim().to_string())
    .filter(|line| !line.is_empty())
    .collect()
}