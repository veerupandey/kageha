//! Read-only discovery of explicitly enabled local browser debugging ports.
//! Discovery grants no attachment consent and never launches or restarts a browser.
use std::{
    collections::BTreeMap,
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

fn parse(commands: &str) -> BTreeMap<String, String> {
    let mut endpoints = BTreeMap::new();
    for line in commands.lines() {
        let line = line.trim();
        let executable = line.split_once(" --").map_or(line, |(exe, _)| exe);
        let profile = if executable.ends_with("/Comet.app/Contents/MacOS/Comet") {
            "comet"
        } else if executable.ends_with("/Google Chrome.app/Contents/MacOS/Google Chrome") {
            "chrome"
        } else {
            continue;
        };
        let args = line.split_whitespace().collect::<Vec<_>>();
        let port = args
            .iter()
            .enumerate()
            .find_map(|(i, arg)| {
                arg.strip_prefix("--remote-debugging-port=")
                    .or_else(|| {
                        (*arg == "--remote-debugging-port")
                            .then(|| args.get(i + 1).copied())
                            .flatten()
                    })
                    .and_then(|v| v.parse::<u16>().ok())
            })
            .filter(|port| *port > 0);
        if let Some(port) = port {
            endpoints
                .entry(profile.into())
                .or_insert_with(|| format!("http://127.0.0.1:{port}"));
        }
    }
    endpoints
}

/// Only the current user's browser processes are inspected. Cache the inventory
/// so browser preview polling does not spawn a process for every state request.
pub fn discover() -> BTreeMap<String, String> {
    static CACHE: OnceLock<Mutex<Option<(Instant, BTreeMap<String, String>)>>> = OnceLock::new();
    let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() else {
        return BTreeMap::new();
    };
    if let Some((at, endpoints)) = &*cache {
        if at.elapsed() < Duration::from_secs(3) {
            return endpoints.clone();
        }
    }
    let endpoints = Command::new("/bin/ps")
        .args([
            "-u",
            &unsafe { libc::geteuid() }.to_string(),
            "-o",
            "command=",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success() && o.stdout.len() < 2 * 1024 * 1024)
        .map(|o| parse(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    *cache = Some((Instant::now(), endpoints.clone()));
    endpoints
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_branded_explicit_ports_without_guessing_default_ports() {
        let endpoints=parse("/Applications/Comet.app/Contents/MacOS/Comet --remote-debugging-port=9411 --foo\n/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --remote-debugging-port 9512 --foo\n/Applications/Other.app/Contents/MacOS/Other --remote-debugging-port=9222");
        assert_eq!(endpoints.get("comet").unwrap(), "http://127.0.0.1:9411");
        assert_eq!(endpoints.get("chrome").unwrap(), "http://127.0.0.1:9512");
        assert_eq!(endpoints.len(), 2);
    }
    #[test]
    fn normal_browsers_pipes_helpers_and_zero_ports_are_not_attachable() {
        assert!(parse("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --remote-debugging-pipe\n/Applications/Comet.app/Contents/MacOS/Comet --remote-debugging-port=0\n/Applications/Comet.app/Contents/Frameworks/Comet Helper --remote-debugging-port=1234\n/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --foo").is_empty());
    }
}
