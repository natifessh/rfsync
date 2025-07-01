mod lfsync;
use std::time::Duration;
use clap::Parser;
use lfsync::lfsync::*;
use crate::lfsync::lfsync::{ Cmd};
fn main() {
     let args = Cmd::parse();
         match (args.source, args.destination) {
        (src, dst) => {
            if args.watch.unwrap_or(false) {
                loop {
                    sync_local(&src, &dst);
                    std::thread::sleep(Duration::from_secs(10));
                }
            } else {
                sync_local(&src, &dst);
            }
        },
        _ => {
            eprintln!("Usag error: --source <path> and --backup <path> are required.");
            std::process::exit(1);
        }
    }  
    
}    
