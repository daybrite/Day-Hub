//! GitHub identifiers and publication metadata, independent of the UI and localization.
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Success,
    Failure,
    Running,
    Queued,
    Cancelled,
    Skipped,
    #[default]
    Unknown,
}

impl Status {
    pub fn from_github(label: &str) -> Self {
        let label = label.to_ascii_lowercase();
        if label.contains("completed successfully") {
            Self::Success
        } else if label.contains("failed")
            || label.contains("timed out")
            || label.contains("action required")
        {
            Self::Failure
        } else if label.contains("currently running") || label.contains("in progress") {
            Self::Running
        } else if label.contains("queued") || label.contains("waiting") || label.contains("pending")
        {
            Self::Queued
        } else if label.contains("cancelled") {
            Self::Cancelled
        } else if label.contains("skipped") {
            Self::Skipped
        } else {
            Self::Unknown
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub path: String,
    pub title: String,
    pub status: Status,
    pub started: String,
    pub duration: String,
    /// GitHub's attempt number (1 = initial run). None when the source does not report it.
    pub attempt: Option<u32>,
}

impl Run {
    pub fn reruns(&self) -> Option<u32> {
        self.attempt.and_then(|attempt| attempt.checked_sub(1))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Network,
    Http(u16),
    Page,
    Backoff(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub repo: String,
    pub group: usize,
    pub runs: Vec<Run>,
    pub error: Option<FetchError>,
}

impl Project {
    pub fn actions_url(&self) -> String {
        format!("https://github.com/{}/actions", self.repo)
    }
}

/// Strict identifiers prevent accidental URL query/path injection. First occurrence wins,
/// case-insensitively, preserving HubLights' comments and dash-separated groups.
pub fn parse_projects(text: &str) -> Result<Vec<Project>, usize> {
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    let mut group = 0;
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.bytes().all(|c| c == b'-') {
            if !projects.is_empty() {
                group += 1;
            }
            continue;
        }
        let Some((owner, repo)) = line.split_once('/') else {
            return Err(index + 1);
        };
        if owner.is_empty()
            || repo.is_empty()
            || !owner
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || !repo
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            || matches!(repo, "." | "..")
        {
            return Err(index + 1);
        }
        if seen.insert(line.to_ascii_lowercase()) {
            projects.push(Project {
                repo: line.into(),
                group,
                runs: Vec::new(),
                error: None,
            });
        }
    }
    Ok(projects)
}

pub fn reconcile(mut new: Vec<Project>, previous: &[Project]) -> Vec<Project> {
    for project in &mut new {
        if let Some(old) = previous
            .iter()
            .find(|p| p.repo.eq_ignore_ascii_case(&project.repo))
        {
            project.runs.clone_from(&old.runs);
            project.error.clone_from(&old.error);
        }
    }
    new
}

/// Stable preference identifiers keep future picker additions independent of their position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepositoryOrder {
    #[default]
    Recency,
    Alphabetical,
}

impl RepositoryOrder {
    pub const ALL: &[Self] = &[Self::Recency, Self::Alphabetical];

    pub fn key(self) -> &'static str {
        match self {
            Self::Recency => "recency",
            Self::Alphabetical => "alphabetical",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|order| order.key() == key)
            .unwrap_or_default()
    }
}

/// Keep explicit groups together, sorting inside each group. Missing/invalid timestamps
/// come last; names break recency ties so refresh completion order never affects the grid.
pub fn ordered(mut projects: Vec<Project>, order: RepositoryOrder) -> Vec<Project> {
    projects.sort_by_cached_key(|p| {
        let latest = match order {
            RepositoryOrder::Recency => p
                .runs
                .iter()
                .filter_map(|run| chrono::DateTime::parse_from_rfc3339(&run.started).ok())
                .max(),
            RepositoryOrder::Alphabetical => None,
        };
        (
            p.group,
            std::cmp::Reverse(latest),
            p.repo.to_ascii_lowercase(),
        )
    });
    projects
}

/// Latest completed pass/fail in each repository, ignoring pending/cancelled/skipped runs.
pub fn pass_rate(projects: &[Project]) -> Option<u32> {
    let completed: Vec<_> = projects
        .iter()
        .filter_map(|p| {
            p.runs
                .iter()
                .find(|r| matches!(r.status, Status::Success | Status::Failure))
        })
        .collect();
    (!completed.is_empty()).then(|| {
        (completed
            .iter()
            .filter(|r| r.status == Status::Success)
            .count()
            * 100
            / completed.len()) as u32
    })
}

/// Only links to the monitored repository's run pages may be opened from scraped markup.
pub fn valid_run_path(path: &str, repo: &str) -> bool {
    path.strip_prefix(&format!("/{repo}/actions/runs/"))
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_comments_and_duplicates() {
        let projects =
            parse_projects("---\n Acme/One # fixture\nacme/one\n---\n\nAcme/Two\n").unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|p| (p.repo.as_str(), p.group))
                .collect::<Vec<_>>(),
            [("Acme/One", 0), ("Acme/Two", 1)]
        );
        for bad in [
            "https://github.com/a/b",
            "a/b?x=y",
            "a/../b",
            "a/..",
            "a/",
            "/b",
            "a/b/c",
        ] {
            assert_eq!(parse_projects(bad), Err(1));
        }
    }

    #[test]
    fn ordering_uses_latest_run_preserves_groups_and_handles_missing_dates() {
        // Synthetic timestamps deliberately include offsets and out-of-order run history.
        let mut projects = parse_projects("fixture/z\nfixture/a\nfixture/b\nfixture/missing\nfixture/invalid\n---\nfixture/newest").unwrap();
        let dates: &[&[&str]] = &[
            &["2026-01-01T08:00:00-05:00"],
            &["2026-01-01T13:00:00Z"],
            &["2025-01-01T00:00:00Z", "2026-01-01T14:00:00Z"],
            &[],
            &["invalid fixture timestamp"],
            &["2027-01-01T00:00:00Z"],
        ];
        for (project, dates) in projects.iter_mut().zip(dates) {
            project.runs = dates
                .iter()
                .map(|date| Run {
                    started: (*date).into(),
                    ..Default::default()
                })
                .collect();
        }
        let names = |order| {
            ordered(projects.clone(), order)
                .into_iter()
                .map(|p| p.repo)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(RepositoryOrder::Recency),
            [
                "fixture/b",
                "fixture/a",
                "fixture/z",
                "fixture/invalid",
                "fixture/missing",
                "fixture/newest"
            ]
        );
        assert_eq!(
            names(RepositoryOrder::Alphabetical),
            [
                "fixture/a",
                "fixture/b",
                "fixture/invalid",
                "fixture/missing",
                "fixture/z",
                "fixture/newest"
            ]
        );
        assert_eq!(
            RepositoryOrder::from_key("unknown-future-order"),
            RepositoryOrder::Recency
        );
        for order in RepositoryOrder::ALL {
            assert_eq!(RepositoryOrder::from_key(order.key()), *order);
        }
    }

    #[test]
    fn edits_keep_results_by_identity() {
        let mut old = parse_projects("a/one\na/two\n---\na/three").unwrap();
        for (p, date) in old
            .iter_mut()
            .zip(["2026-01-01", "2026-03-01", "2026-04-01"])
        {
            p.runs.push(Run {
                started: date.into(),
                attempt: Some(3),
                status: Status::Success,
                ..Default::default()
            });
        }
        let sorted = ordered(old.clone(), RepositoryOrder::Alphabetical);
        assert_eq!(
            sorted.iter().map(|p| p.repo.as_str()).collect::<Vec<_>>(),
            ["a/one", "a/two", "a/three"]
        );
        let edited = reconcile(parse_projects("A/TWO\n---\na/new").unwrap(), &old);
        assert_eq!(edited[0].runs, old[1].runs);
        assert!(edited[1].runs.is_empty());
        assert_eq!(pass_rate(&[]), None);
        assert_eq!(pass_rate(&old), Some(100));
        old[1].runs[0].status = Status::Failure;
        old[0].runs.insert(
            0,
            Run {
                status: Status::Running,
                ..Default::default()
            },
        );
        assert_eq!(pass_rate(&old), Some(66));
    }
}
