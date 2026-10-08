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

/// Preserve the user's explicit menu/grid ordering; separators assign groups during parsing.
pub fn ordered(projects: Vec<Project>) -> Vec<Project> {
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
    fn edits_keep_results_by_identity_and_preserve_manual_order() {
        let mut old = parse_projects("a/one\na/two\n---\na/three").unwrap();
        for (p, date) in old
            .iter_mut()
            .zip(["2026-01-01", "2026-03-01", "2026-04-01"])
        {
            p.runs.push(Run {
                started: date.into(),
                status: Status::Success,
                ..Default::default()
            });
        }
        let sorted = ordered(old.clone());
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
