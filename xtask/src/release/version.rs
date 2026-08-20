//! Version parsing and derivation for the release log.
//!
//! Deliberately accepts only the two shapes this project releases — `X.Y.Z` and
//! `X.Y.Z-rc.N` — and rejects everything else rather than guessing. Versions decide
//! ordering in an append-only log, so a version we cannot compare exactly is one we must
//! not accept at all.

use std::{cmp::Ordering, fmt, str::FromStr};

use eyre::eyre::{bail, eyre};

/// Which part of the version a release advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bump {
    /// Next release candidate: continues an open rc series, or opens one on the next patch.
    Rc,
    /// Next patch release.
    Patch,
    /// Next minor release.
    Minor,
    /// Next major release.
    Major,
}

/// A released version: `X.Y.Z`, optionally an `-rc.N` prerelease of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    /// Major component.
    pub major: u64,
    /// Minor component.
    pub minor: u64,
    /// Patch component.
    pub patch: u64,
    /// Release-candidate number, when this is a prerelease of the triple above.
    pub rc: Option<u64>,
}

impl Version {
    /// A stable `X.Y.Z`.
    pub const fn stable(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            rc: None,
        }
    }

    /// True when this is an `-rc.N` prerelease.
    pub const fn is_rc(&self) -> bool {
        self.rc.is_some()
    }

    /// The stable version this rc is a candidate for (itself, when already stable).
    pub const fn base(&self) -> Self {
        Self::stable(self.major, self.minor, self.patch)
    }

    /// Derives the next version from the latest stable and any open rc series.
    ///
    /// `latest_rc` is only consulted for [`Bump::Rc`], and only when it is a candidate for
    /// something newer than `latest_stable` — an rc that has already been superseded by a
    /// stable release must not be continued.
    pub fn next(bump: Bump, latest_stable: Option<Self>, latest_rc: Option<Self>) -> Self {
        let stable = latest_stable.unwrap_or(Self::stable(0, 0, 0));
        match bump {
            Bump::Rc => match latest_rc {
                Some(rc) if rc.is_rc() && rc.base() > stable => Self {
                    rc: Some(rc.rc.unwrap_or(0) + 1),
                    ..rc
                },
                _ => Self {
                    patch: stable.patch + 1,
                    rc: Some(1),
                    ..stable
                },
            },
            Bump::Patch => Self {
                patch: stable.patch + 1,
                ..stable
            },
            Bump::Minor => Self {
                minor: stable.minor + 1,
                patch: 0,
                ..stable
            },
            Bump::Major => Self {
                major: stable.major + 1,
                minor: 0,
                patch: 0,
                rc: None,
            },
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            major,
            minor,
            patch,
            rc,
        } = self;
        write!(f, "{major}.{minor}.{patch}")?;
        if let Some(rc) = rc {
            write!(f, "-rc.{rc}")?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (triple, rc) = match s.split_once("-rc.") {
            Some((triple, n)) => {
                let n: u64 = n
                    .parse()
                    .map_err(|_| eyre!("version {s:?}: rc number must be an integer"))?;
                (triple, Some(n))
            }
            None => {
                if s.contains('-') || s.contains('+') {
                    bail!("version {s:?}: only `X.Y.Z` and `X.Y.Z-rc.N` are supported");
                }
                (s, None)
            }
        };

        let parts: Vec<&str> = triple.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            bail!("version {s:?}: expected three dot-separated components");
        };
        let parse = |part: &str, name: &str| -> eyre::Result<u64> {
            if part.is_empty() || (part.len() > 1 && part.starts_with('0')) {
                bail!("version {s:?}: {name} must not be empty or zero-padded");
            }
            part.parse()
                .map_err(|_| eyre!("version {s:?}: {name} must be an integer"))
        };

        Ok(Self {
            major: parse(major, "major")?,
            minor: parse(minor, "minor")?,
            patch: parse(patch, "patch")?,
            rc,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            // A prerelease sorts before the release it is a candidate for, and rc numbers
            // compare numerically — `rc.10` is newer than `rc.2`, which string ordering
            // would get backwards.
            .then_with(|| match (self.rc, other.rc) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_displays_both_supported_shapes() {
        for s in ["1.0.0", "1.0.0-rc.1", "0.0.0", "10.20.30-rc.44"] {
            assert_eq!(v(s).to_string(), s);
        }
    }

    #[test]
    fn rejects_shapes_we_cannot_order() {
        for s in [
            "1.0",
            "1.0.0.0",
            "1.0.0-beta.1",
            "1.0.0-rc",
            "1.0.0-rc.x",
            "v1.0.0",
            "1.0.0+build",
            "01.0.0",
            "",
        ] {
            assert!(s.parse::<Version>().is_err(), "should reject {s:?}");
        }
    }

    /// String ordering puts `rc.10` before `rc.2`; the log's monotonicity depends on this
    /// being numeric.
    #[test]
    fn rc_numbers_order_numerically() {
        assert!(v("1.0.0-rc.2") < v("1.0.0-rc.10"));
    }

    /// A release candidate precedes the release it is a candidate for.
    #[test]
    fn rc_precedes_its_stable_release() {
        assert!(v("1.0.0-rc.9") < v("1.0.0"));
        assert!(v("1.0.0") < v("1.0.1-rc.1"));
    }

    #[test]
    fn continues_an_open_rc_series() {
        let next = Version::next(Bump::Rc, Some(v("1.0.0")), Some(v("1.1.0-rc.2")));
        assert_eq!(next, v("1.1.0-rc.3"));
    }

    /// An rc already superseded by a stable release must not be continued — doing so would
    /// mint a version that sorts below the newest entry.
    #[test]
    fn ignores_rc_already_superseded_by_stable() {
        let next = Version::next(Bump::Rc, Some(v("1.1.0")), Some(v("1.1.0-rc.2")));
        assert_eq!(next, v("1.1.1-rc.1"));
    }

    #[test]
    fn opens_a_new_rc_series_when_none_is_open() {
        assert_eq!(
            Version::next(Bump::Rc, Some(v("1.0.0")), None),
            v("1.0.1-rc.1")
        );
    }

    #[test]
    fn stable_bumps_come_off_latest_stable_and_drop_any_rc() {
        let stable = Some(v("1.2.3"));
        let rc = Some(v("2.0.0-rc.7"));
        assert_eq!(Version::next(Bump::Patch, stable, rc), v("1.2.4"));
        assert_eq!(Version::next(Bump::Minor, stable, rc), v("1.3.0"));
        assert_eq!(Version::next(Bump::Major, stable, rc), v("2.0.0"));
    }

    #[test]
    fn first_release_starts_from_zero() {
        assert_eq!(Version::next(Bump::Rc, None, None), v("0.0.1-rc.1"));
        assert_eq!(Version::next(Bump::Minor, None, None), v("0.1.0"));
    }

    /// Every derived version must sort strictly after what it builds on, whatever the input.
    #[test]
    fn derived_versions_are_always_monotonic() {
        let cases = [
            (Some(v("1.0.0")), Some(v("1.1.0-rc.2"))),
            (Some(v("1.1.0")), Some(v("1.1.0-rc.2"))),
            (Some(v("0.0.0")), None),
            (None, None),
        ];
        for (stable, rc) in cases {
            for bump in [Bump::Rc, Bump::Patch, Bump::Minor, Bump::Major] {
                let next = Version::next(bump, stable, rc);
                if let Some(stable) = stable {
                    assert!(next > stable, "{next} !> {stable} for {bump:?}");
                }
            }
        }
    }
}
