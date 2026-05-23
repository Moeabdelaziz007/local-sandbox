use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// AOF fsync policy — controls how often data is flushed to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsyncPolicy {
    /// fsync after every write. Safest, slowest.
    Always,
    /// fsync once per second (background). Good balance of safety and performance.
    #[default]
    EverySecond,
    /// Never fsync; let the OS decide. Fastest, least safe.
    No,
}

impl FsyncPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            FsyncPolicy::Always => "always",
            FsyncPolicy::EverySecond => "everysec",
            FsyncPolicy::No => "no",
        }
    }
}

/// Server configuration loaded from a `tinyredis.conf` file (or built from defaults).
#[derive(Debug, Clone)]
pub struct Config {
    /// IP address(es) to bind to.
    pub bind: String,
    /// TCP port to listen on.
    pub port: u16,
    /// Enable append-only file persistence.
    pub appendonly: bool,
    /// AOF file path.
    pub appendfilename: PathBuf,
    /// Memory limit in bytes (0 = no limit).
    pub maxmemory: u64,
    /// Eviction policy name (stored as-is; enforcement not yet implemented).
    pub maxmemory_policy: String,
    /// AOF fsync policy.
    pub appendfsync: FsyncPolicy,
    /// Required password for `AUTH`. `None` means authentication is disabled.
    pub requirepass: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: "127.0.0.1".to_string(),
            port: 6379,
            appendonly: true,
            appendfilename: PathBuf::from("tinyredis.aof"),
            maxmemory: 0,
            maxmemory_policy: "noeviction".to_string(),
            appendfsync: FsyncPolicy::EverySecond,
            requirepass: None,
        }
    }
}

impl Config {
    /// Parse a Redis-style config file.
    ///
    /// Format: `keyword value` per line; `#` starts a comment; blank lines ignored.
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            Error::Protocol(format!(
                "Failed to open config file '{}': {e}",
                path.display()
            ))
        })?;

        let mut cfg = Config::default();

        for (lineno, raw) in contents.lines().enumerate() {
            // Strip inline comments and surrounding whitespace.
            let line = match raw.find('#') {
                Some(pos) => &raw[..pos],
                None => raw,
            }
            .trim();

            if line.is_empty() {
                continue;
            }

            // Split into keyword + rest-of-line.
            let (kw, rest) = match line.find(char::is_whitespace) {
                None => (line, ""),
                Some(pos) => (&line[..pos], line[pos..].trim()),
            };

            match kw.to_ascii_lowercase().as_str() {
                "bind" => {
                    cfg.bind = rest.to_string();
                }
                "port" => {
                    cfg.port = rest.parse().map_err(|_| {
                        Error::Protocol(format!(
                            "Config error at line {}: invalid port '{rest}'",
                            lineno + 1
                        ))
                    })?;
                }
                "appendonly" => {
                    cfg.appendonly = match rest {
                        "yes" => true,
                        "no" => false,
                        _ => {
                            return Err(Error::Protocol(format!(
                                "Config error at line {}: expected 'yes' or 'no' for appendonly",
                                lineno + 1
                            )));
                        }
                    };
                }
                "appendfilename" => {
                    // Strip optional surrounding quotes.
                    let name = rest.trim_matches('"').trim_matches('\'');
                    cfg.appendfilename = PathBuf::from(name);
                }
                "maxmemory" => {
                    cfg.maxmemory = parse_memory_size(rest).map_err(|_| {
                        Error::Protocol(format!(
                            "Config error at line {}: invalid maxmemory '{rest}'",
                            lineno + 1
                        ))
                    })?;
                }
                "maxmemory-policy" => {
                    cfg.maxmemory_policy = rest.to_string();
                }
                "appendfsync" => {
                    cfg.appendfsync = match rest {
                        "always" => FsyncPolicy::Always,
                        "everysec" => FsyncPolicy::EverySecond,
                        "no" => FsyncPolicy::No,
                        _ => {
                            return Err(Error::Protocol(format!(
                                "Config error at line {}: invalid appendfsync '{rest}' \
                                 (expected always|everysec|no)",
                                lineno + 1
                            )));
                        }
                    };
                }
                "requirepass" => {
                    let pass = rest.trim_matches('"').trim_matches('\'');
                    cfg.requirepass = if pass.is_empty() {
                        None
                    } else {
                        Some(pass.to_string())
                    };
                }
                // Unknown directives are silently ignored for forward compatibility.
                _ => {}
            }
        }

        Ok(cfg)
    }
}

/// Parse Redis-style memory size strings: `0`, `100`, `100b`, `100kb`, `100mb`, `100gb`.
/// Uses 1024-based multipliers (kibibytes / mebibytes / gibibytes).
fn parse_memory_size(s: &str) -> std::result::Result<u64, ()> {
    let s = s.to_ascii_lowercase();
    let (digits, mult) = if let Some(n) = s.strip_suffix("gb") {
        (n, 1024 * 1024 * 1024u64)
    } else if let Some(n) = s.strip_suffix("mb") {
        (n, 1024 * 1024u64)
    } else if let Some(n) = s.strip_suffix("kb") {
        (n, 1024u64)
    } else if let Some(n) = s.strip_suffix('b') {
        (n, 1u64)
    } else {
        (s.as_str(), 1u64)
    };
    digits
        .trim()
        .parse::<u64>()
        .map(|n| n * mult)
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.bind, "127.0.0.1");
        assert_eq!(cfg.port, 6379);
        assert!(cfg.appendonly);
        assert_eq!(cfg.appendfilename, PathBuf::from("tinyredis.aof"));
        assert_eq!(cfg.maxmemory, 0);
    }

    #[test]
    fn parse_memory_size_plain() {
        assert_eq!(parse_memory_size("1024"), Ok(1024));
        assert_eq!(parse_memory_size("0"), Ok(0));
    }

    #[test]
    fn parse_memory_size_units() {
        assert_eq!(parse_memory_size("1kb"), Ok(1024));
        assert_eq!(parse_memory_size("2mb"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_memory_size("1gb"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_memory_size("512b"), Ok(512));
    }

    #[test]
    fn parse_memory_size_invalid() {
        assert!(parse_memory_size("abc").is_err());
        assert!(parse_memory_size("").is_err());
    }

    #[test]
    fn parse_file_basic() {
        let dir = std::env::temp_dir();
        let path = dir.join("tinyredis_cfg_test.conf");
        std::fs::write(
            &path,
            b"# comment\nbind 0.0.0.0\nport 6380\nappendonly no\nmaxmemory 100mb\n",
        )
        .unwrap();
        let cfg = Config::from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.bind, "0.0.0.0");
        assert_eq!(cfg.port, 6380);
        assert!(!cfg.appendonly);
        assert_eq!(cfg.maxmemory, 100 * 1024 * 1024);
    }

    #[test]
    fn parse_file_inline_comment() {
        let dir = std::env::temp_dir();
        let path = dir.join("tinyredis_cfg_inline.conf");
        std::fs::write(&path, b"port 7000 # custom port\n").unwrap();
        let cfg = Config::from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(cfg.port, 7000);
    }

    #[test]
    fn parse_file_quoted_appendfilename() {
        let dir = std::env::temp_dir();
        let path = dir.join("tinyredis_cfg_quoted.conf");
        std::fs::write(&path, b"appendfilename \"my.aof\"\n").unwrap();
        let cfg = Config::from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(cfg.appendfilename, PathBuf::from("my.aof"));
    }
}
