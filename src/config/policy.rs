use std::{collections::HashSet, fmt};

use chrono::{Duration, Utc};
use enum_kinds::EnumKind;
use log::debug;
use starlark::{starlark_simple_value, starlark_type, values::StarlarkValue};
use url::Url;

use crate::Torrent;

/// Conditions for matching a torrent for a policy on a transmission instance.
#[derive(PartialEq, Clone, Default)]
pub struct Condition {
    /// The tracker URL hostnames (only the host, not the path or
    /// port) that the policy should apply to.
    pub trackers: HashSet<String>,

    /// The number of files that must be present in a torrent for the
    /// policy to match. If None, any number of files matches.
    pub min_file_count: Option<i32>,

    /// The maximum number of files that may be present in a torrent
    /// for the policy to match. If None, any number of files matches.
    pub max_file_count: Option<i32>,

    /// The minimum amount of time that a torrent must have been
    /// seeding for, to qualify for deletion.
    ///
    /// Even if the [`max_ratio`] requirement isn't met, the torrent
    /// won't be deleted unless it's been seeding this long.
    pub min_seeding_time: Option<Duration>,

    /// The ratio at which a torrent qualifies for deletion, even if
    /// it has been seeded for less than [`max_seeding_time`].
    pub max_ratio: Option<f64>,

    /// The duration at which a torrent qualifies for deletion.
    pub max_seeding_time: Option<Duration>,
}

#[derive(PartialEq, Copy, Clone, Debug, EnumKind)]
#[enum_kind(ConditionMatchKind)]
pub enum ConditionMatch {
    /// Preconditions (not seeding, trackers, number of files) don't match.
    PreconditionsMismatch,

    /// Preconditions met, but did not match.
    None,

    /// Matches based on ratio
    Ratio(f64),

    /// Matches based on seed time
    SeedTime(Duration),
}

impl fmt::Display for ConditionMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ConditionMatch::*;
        match self {
            PreconditionsMismatch => write!(f, "PreconditionsMismatch"),
            None => write!(f, "None"),
            Ratio(r) => write!(f, "Ratio({})", r),
            SeedTime(d) => write!(f, "SeedTime({})", d),
        }
    }
}

impl ConditionMatch {
    pub fn is_match(&self) -> bool {
        self != &ConditionMatch::None && self != &ConditionMatch::PreconditionsMismatch
    }

    pub fn failed_with_precondition(&self) -> bool {
        self == &ConditionMatch::PreconditionsMismatch
    }

    pub fn is_real_mismatch(&self) -> bool {
        self == &ConditionMatch::None
    }
}

impl Condition {
    pub fn sanity_check(self) -> anyhow::Result<Self> {
        if vec![
            self.min_seeding_time.map(|_| true),
            self.max_ratio.map(|_| true),
            self.max_seeding_time.map(|_| true),
        ]
        .iter()
        .all(Option::is_none)
        {
            anyhow::bail!("Set at least one of min_seeding_time, max_seeding_time, max_ratio - otherwise this deletes all a tracker's torrents immediately.");
        }
        Ok(self)
    }

    /// Returns true of the condition matches a given torrent.
    pub fn matches_torrent(&self, t: &Torrent) -> ConditionMatch {
        if t.status != crate::Status::Seeding {
            debug!("Torrent {:?} is not seeding, bailing", t);
            return ConditionMatch::PreconditionsMismatch;
        }

        if !t
            .trackers
            .iter()
            .filter_map(Url::host_str)
            .any(|tracker_host| self.trackers.contains(tracker_host))
        {
            debug!(
                "Torrent {:?} does not have matching trackers, expected {:?}",
                t, self.trackers
            );
            return ConditionMatch::PreconditionsMismatch;
        }

        let file_count = t.num_files as i32;
        match (self.min_file_count, self.max_file_count) {
            (Some(min), Some(max)) if file_count < min || file_count > max => {
                debug!(
                    "Torrent {:?} doesn't have the right number of files: {}",
                    t, file_count
                );
                return ConditionMatch::PreconditionsMismatch;
            }
            (None, Some(max)) if file_count > max => {
                debug!(
                    "Torrent {:?} doesn't have the right number of files: {}",
                    t, file_count
                );
                return ConditionMatch::PreconditionsMismatch;
            }
            (Some(min), None) if file_count < min => {
                debug!(
                    "Torrent {:?} doesn't have the right number of files: {}",
                    t, file_count
                );
                return ConditionMatch::PreconditionsMismatch;
            }
            (_, _) => {}
        }

        if let Some(done_date) = t.done_date {
            let seed_time = Utc::now() - done_date;
            match (self.min_seeding_time, self.max_seeding_time) {
                // Short-circuiting criteria:
                (Some(min), Some(max)) if min < seed_time && seed_time >= max => {
                    debug!("Torrent {:?} matches seed time requirements", t);
                    return ConditionMatch::SeedTime(seed_time);
                }
                (None, Some(max)) if seed_time > max => {
                    debug!("Torrent {:?} seeded longer than necessary", t);
                    return ConditionMatch::SeedTime(seed_time);
                }

                // Exclusion criteria:
                (Some(min), _) if seed_time < min => {
                    debug!("Torrent {:?} doesn't yet meet min_seeding_time", t);
                    return ConditionMatch::None;
                }
                (_, _) => {}
            }
        }

        if let Some(max_ratio) = self.max_ratio {
            if t.upload_ratio as f64 >= max_ratio {
                debug!("Torrent {:?} doesn't have good enough ratio yet", t);
                return ConditionMatch::Ratio(t.upload_ratio as f64);
            }
        }

        ConditionMatch::None
    }
}

impl fmt::Debug for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "When:[{:?}", self.trackers)?;
        if let Some(min_file_count) = self.min_file_count {
            write!(f, " {}<f", min_file_count)?;
            if let Some(max_file_count) = self.max_file_count {
                write!(f, "<={}", max_file_count)?;
            }
        } else if let Some(max_file_count) = self.max_file_count {
            write!(f, " f<={}", max_file_count)?;
        }
        if let Some(min_seeding_time) = self.min_seeding_time {
            write!(f, " {}>t", min_seeding_time)?;
            if let Some(max_seeding_time) = self.max_seeding_time {
                write!(f, "<={}", max_seeding_time)?;
            }
        } else if let Some(max_seeding_time) = self.max_seeding_time {
            write!(f, " t<={}", max_seeding_time)?;
        }
        if let Some(max_ratio) = self.max_ratio {
            write!(f, " r<{}", max_ratio)?;
        }
        write!(f, "]")
    }
}

starlark_simple_value!(Condition);
impl<'v> StarlarkValue<'v> for Condition {
    starlark_type!("condition");
}

/// Specifies a condition for torrents that can be deleted.
#[derive(PartialEq, Clone)]
pub struct DeletePolicy {
    /// The condition under which to match
    pub match_when: Condition,
    pub delete_data: bool,
}

impl fmt::Debug for DeletePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl fmt::Display for DeletePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DeletePolicy:[{}, delete_data:{}]",
            self.match_when, self.delete_data
        )
    }
}

starlark_simple_value!(DeletePolicy);
impl<'v> StarlarkValue<'v> for DeletePolicy {
    starlark_type!("delete_policy");
}

#[cfg(test)]
mod test {
    use super::*;
    use test_case::test_case;

    fn init_logger() {
        let _ = env_logger::builder()
            // Include all events in tests
            .filter_level(log::LevelFilter::max())
            // Ensure events are captured by `cargo test`
            .is_test(true)
            // Ignore errors initializing the logger if tests race to configure it
            .try_init();
    }

    // Should never delete younglings:
    #[test_case("1 min", 0.0, ConditionMatchKind::None; "young torrent at unmet ratio")]
    #[test_case("1 min", 7.0, ConditionMatchKind::None; "young torrent at exceeded ratio")]
    // If they're older, we can delete if ratio is met:
    #[test_case("6 hrs", 1.1, ConditionMatchKind::Ratio; "medium and ratio exceeded")]
    #[test_case("6 hrs", 0.9, ConditionMatchKind::None; "medium and ratio not met")]
    // Any that are really old are fair game:
    #[test_case("12 days", 0.9, ConditionMatchKind::SeedTime; "when seeding long enough at unmet ratio")]
    #[test_case("12 days", 1.5, ConditionMatchKind::SeedTime; "when seeding long enough at exceeded ratio")]
    fn condition_seed_time(time: &str, upload_ratio: f32, matches: ConditionMatchKind) {
        init_logger();
        let time = Duration::from_std(parse_duration::parse(time).unwrap()).unwrap();
        let condition = Condition {
            trackers: vec!["tracker".to_string()].into_iter().collect(),
            max_ratio: Some(1.0),
            min_seeding_time: Some(Duration::minutes(60)),
            max_seeding_time: Some(Duration::days(2)),
            ..Default::default()
        };
        let t = Torrent {
            id: 1,
            hash: "abcd".to_string(),
            name: "testcase".to_string(),
            done_date: Some(Utc::now() - time),
            error: crate::Error::Ok,
            error_string: "".to_string(),
            upload_ratio,
            status: crate::Status::Seeding,
            num_files: 1,
            trackers: vec![Url::parse("https://tracker:8080/announce").unwrap()],
        };
        assert_eq!(
            ConditionMatchKind::from(condition.matches_torrent(&t)),
            matches
        );
    }

    #[test_case(1, true; "single-file torrent")]
    #[test_case(2, false; "within range: 2")]
    #[test_case(3, false; "within range: 3")]
    #[test_case(4, false; "within range: 4")]
    #[test_case(5, true; "out of range: 5")]
    fn condition_num_files(num_files: usize, matches: bool) {
        init_logger();
        let condition = Condition {
            trackers: vec!["tracker".to_string()].into_iter().collect(),
            max_ratio: Some(1.0),
            min_seeding_time: Some(Duration::minutes(60)),
            max_seeding_time: Some(Duration::days(2)),
            min_file_count: Some(2),
            max_file_count: Some(4),
            ..Default::default()
        };
        let t = Torrent {
            id: 1,
            hash: "abcd".to_string(),
            name: "testcase".to_string(),
            done_date: Some(Utc::now() - Duration::days(12)),
            error: crate::Error::Ok,
            error_string: "".to_string(),
            upload_ratio: 2.0,
            status: crate::Status::Seeding,
            num_files,
            trackers: vec![Url::parse("https://tracker:8080/announce").unwrap()],
        };
        assert_eq!(
            condition.matches_torrent(&t).failed_with_precondition(),
            matches
        );
    }
}
