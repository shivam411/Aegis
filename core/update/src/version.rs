//! Version parsing and comparison (`MAJOR.MINOR.PATCH[-PRE][+BUILD]`).

use std::cmp::Ordering;

/// A parsed `MAJOR.MINOR.PATCH[-PRE][+BUILD]`, with an optional leading `v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub core: (u64, u64, u64),
    pub pre: Vec<String>,
}

impl Version {
    /// A release, not a pre-release such as `1.0.0-rc.1`.
    pub fn is_stable(&self) -> bool {
        self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (a, b, c) = self.core;
        write!(f, "{}.{}.{}", a, b, c)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Parses `v1.2.3`, `V1.2.3`, `1.2.3-rc.1+build`. `None` for anything else.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let s = s.split('+').next()?;
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, pre.split('.').map(str::to_string).collect()),
        None => (s, Vec::new()),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = Version {
        core: (parts.next()??, parts.next()??, parts.next()??),
        pre,
    };
    parts.next().is_none().then_some(version)
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            // A release sorts after its pre-releases.
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What to do, given the latest tag and this build's version.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Plan {
    UpToDate,
    Upgrade,
    /// This build is newer than the latest release (a development build).
    Ahead,
}

pub fn plan(latest: &str, current: &str) -> anyhow::Result<Plan> {
    let l = parse_version(latest)
        .ok_or_else(|| anyhow::anyhow!("Cannot compare release tag '{}'", latest))?;
    let c = parse_version(current)
        .ok_or_else(|| anyhow::anyhow!("Cannot compare version '{}'", current))?;
    Ok(match l.cmp(&c) {
        Ordering::Greater => Plan::Upgrade,
        Ordering::Equal => Plan::UpToDate,
        Ordering::Less => Plan::Ahead,
    })
}

/// True when going from `from` to `to` crosses a major version (1.x to 2.x).
/// 0.x releases never count: the project is pre-1.0 and every 0.x release
/// migrates the database forward on start.
pub fn is_major_jump(from: &Version, to: &Version) -> bool {
    from.core.0 >= 1 && to.core.0 > from.core.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        let v = |s: &str| parse_version(s).unwrap();
        assert_eq!(v("v0.1.7").core, (0, 1, 7));
        assert_eq!(v("V0.1.5").core, (0, 1, 5));
        assert!(v("0.2.0") > v("0.1.10"));
        assert!(v("0.1.10") > v("0.1.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.9"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha") > v("1.0.0-1"));
        assert_eq!(v("1.2.3+build.5"), v("v1.2.3"));
        assert!(v("1.0.0").is_stable() && !v("1.0.0-rc.1").is_stable());
        assert_eq!(v("v1.2.3-rc.1").to_string(), "1.2.3-rc.1");
        for bad in ["", "1.2", "1.2.3.4", "x.y.z", "latest", "v", "1.2.-3"] {
            assert!(parse_version(bad).is_none(), "{}", bad);
        }
    }

    #[test]
    fn plans_upgrades() {
        assert_eq!(plan("v0.1.7", "0.1.0").unwrap(), Plan::Upgrade);
        assert_eq!(plan("v0.1.7", "0.1.7").unwrap(), Plan::UpToDate);
        assert_eq!(plan("v0.1.7", "0.2.0-dev").unwrap(), Plan::Ahead);
        assert_eq!(plan("v0.2.0", "0.2.0-rc.1").unwrap(), Plan::Upgrade);
        assert!(plan("nightly", "0.1.0").is_err());
    }

    #[test]
    fn detects_major_jumps() {
        let v = |s: &str| parse_version(s).unwrap();
        assert!(is_major_jump(&v("1.4.0"), &v("2.0.0")));
        assert!(!is_major_jump(&v("1.4.0"), &v("1.5.0")));
        assert!(!is_major_jump(&v("0.1.7"), &v("0.2.0")));
        assert!(!is_major_jump(&v("0.9.0"), &v("1.0.0")));
    }
}
