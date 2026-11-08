use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Deserialize;

/// command line arguments. Anything also settable in the config file is optional here;
/// values given on the command line win over the config file.
#[derive(Parser, Debug, Default)]
#[command(version, about, long_about = None)]
pub struct Cmd {
    /// local source directory
    #[arg(short = 's', long = "source")]
    pub source: Option<String>,
    /// destination: a local directory, or user@host:/remote/path for ssh (sftp) backup
    #[arg(short = 'b', long = "backup")]
    pub destination: Option<String>,
    /// keep running and re-sync whenever the source changes
    #[arg(short = 'w', long = "watch")]
    pub watch: bool,
    /// show a progress bar (local) / per-file output (remote)
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
    /// show what would be copied/deleted without changing anything
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// glob pattern to skip, matched against every path component (repeatable), e.g. -e target -e '*.log'
    #[arg(short = 'e', long = "exclude")]
    pub exclude: Vec<String>,
    /// delete files in the destination that no longer exist in the source
    #[arg(long = "delete")]
    pub delete: bool,
    /// quiet period in ms to batch file events in watch mode [default: 500]
    #[arg(long = "debounce-ms")]
    pub debounce_ms: Option<u64>,
    /// ssh port for remote destinations [default: 22]
    #[arg(long = "port")]
    pub port: Option<u16>,
    /// number of parallel ssh connections for remote uploads [default: 4]
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<usize>,
    /// private key file for ssh auth (default: use ssh-agent)
    #[arg(short = 'i', long = "identity")]
    pub identity: Option<PathBuf>,
    /// prompt for the ssh password (or the key passphrase when --identity is used)
    #[arg(long = "ask-pass")]
    pub ask_pass: bool,
    /// read settings from a TOML file
    #[arg(short = 'c', long = "config")]
    pub config: Option<PathBuf>,
}

/// settings that can live in a config file (rfsync.toml)
#[derive(Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub source: Option<String>,
    pub backup: Option<String>,
    pub watch: Option<bool>,
    pub verbose: Option<bool>,
    pub dry_run: Option<bool>,
    pub exclude: Option<Vec<String>>,
    pub delete: Option<bool>,
    pub debounce_ms: Option<u64>,
    pub port: Option<u16>,
    pub jobs: Option<usize>,
    pub identity: Option<PathBuf>,
    pub ask_pass: Option<bool>,
}

/// the final, fully-resolved settings
#[derive(Debug, PartialEq)]
pub struct Settings {
    pub source: String,
    pub destination: String,
    pub watch: bool,
    pub verbose: bool,
    pub dry_run: bool,
    pub exclude: Vec<String>,
    pub delete: bool,
    pub debounce_ms: u64,
    pub port: u16,
    pub jobs: usize,
    pub identity: Option<PathBuf>,
    pub ask_pass: bool,
}

pub fn load_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read config '{}': {}", path.display(), e))?;
    toml::from_str(&text).map_err(|e| format!("bad config '{}': {}", path.display(), e))
}

/// merge CLI args over the config file. Flags are on if either side turned them on;
/// exclude patterns from both are combined.
pub fn resolve(cmd: Cmd, cfg: Config) -> Result<Settings, String> {
    let source = cmd
        .source
        .or(cfg.source)
        .ok_or("no source given: use --source or set `source` in the config file")?;
    let destination = cmd
        .destination
        .or(cfg.backup)
        .ok_or("no destination given: use --backup or set `backup` in the config file")?;
    let jobs = cmd.jobs.or(cfg.jobs).unwrap_or(4);
    if jobs == 0 {
        return Err("jobs must be at least 1".into());
    }
    let mut exclude = cfg.exclude.unwrap_or_default();
    exclude.extend(cmd.exclude);

    Ok(Settings {
        source,
        destination,
        watch: cmd.watch || cfg.watch.unwrap_or(false),
        verbose: cmd.verbose || cfg.verbose.unwrap_or(false),
        dry_run: cmd.dry_run || cfg.dry_run.unwrap_or(false),
        exclude,
        delete: cmd.delete || cfg.delete.unwrap_or(false),
        debounce_ms: cmd.debounce_ms.or(cfg.debounce_ms).unwrap_or(500),
        port: cmd.port.or(cfg.port).unwrap_or(22),
        jobs,
        identity: cmd.identity.or(cfg.identity),
        ask_pass: cmd.ask_pass || cfg.ask_pass.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(src: Option<&str>, dst: Option<&str>) -> Cmd {
        Cmd {
            source: src.map(Into::into),
            destination: dst.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_apply() {
        let s = resolve(cmd(Some("a"), Some("b")), Config::default()).unwrap();
        assert_eq!((s.port, s.jobs, s.debounce_ms), (22, 4, 500));
        assert!(!s.delete && !s.watch && !s.dry_run);
    }

    #[test]
    fn config_fills_gaps_and_cli_wins() {
        let cfg: Config = toml::from_str(
            r#"
            source = "/cfg/src"
            backup = "me@host:/bk"
            exclude = ["target"]
            delete = true
            port = 2200
            jobs = 8
            "#,
        )
        .unwrap();
        let mut c = cmd(Some("/cli/src"), None);
        c.exclude = vec!["*.log".into()];
        c.port = Some(2222);
        let s = resolve(c, cfg).unwrap();
        assert_eq!(s.source, "/cli/src"); // CLI wins
        assert_eq!(s.destination, "me@host:/bk"); // from config
        assert_eq!(s.port, 2222);
        assert_eq!(s.jobs, 8);
        assert!(s.delete);
        assert_eq!(s.exclude, vec!["target", "*.log"]); // combined
    }

    #[test]
    fn missing_paths_and_zero_jobs_are_errors() {
        assert!(resolve(cmd(None, Some("b")), Config::default()).is_err());
        assert!(resolve(cmd(Some("a"), None), Config::default()).is_err());
        let mut c = cmd(Some("a"), Some("b"));
        c.jobs = Some(0);
        assert!(resolve(c, Config::default()).is_err());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        assert!(toml::from_str::<Config>("sourse = \"typo\"").is_err());
    }

    #[test]
    fn loads_config_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rfsync.toml");
        std::fs::write(&p, "source = \"a\"\nbackup = \"b\"\n").unwrap();
        let cfg = load_config(&p).unwrap();
        assert_eq!(cfg.source.as_deref(), Some("a"));
        assert!(load_config(&dir.path().join("nope.toml")).is_err());
    }
}
