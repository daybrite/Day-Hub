//! App-wide account and repository state; window instances share requests and caches.
use crate::{
    App,
    api::{Client, Error},
    auth,
};
use day::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    rc::Rc,
    time::Instant,
};

pub fn is_fixture() -> bool {
    std::env::var_os("DAY_HUB_FIXTURE").is_some()
}

pub const CLIENT_ID: &str = "Ov23li46VVDMelb0esrd";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Owner {
    pub login: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: Owner,
    pub html_url: String,
    pub updated_at: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub stargazers_count: u64,
    #[serde(default)]
    pub forks_count: u64,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Clone, Default, PartialEq)]
pub struct Detail {
    pub overview: Option<Value>,
    pub runs: Vec<Value>,
    pub runs_next: Option<String>,
    pub error: Option<Error>,
    pub loaded: Option<Instant>,
}
#[derive(Clone, Copy)]
pub struct Hub {
    pub api: Signal<Option<Rc<Client>>>,
    pub login: Signal<String>,
    pub repos: Signal<Vec<Repository>>,
    pub favorites: Signal<BTreeSet<String>>,
    pub hidden: Signal<crate::visibility::Hidden>,
    pub show_hidden: Signal<bool>,
    pub collapsed_sections: Signal<std::collections::HashSet<String>>,
    pub search: Signal<String>,
    pub sort: Signal<usize>,
    pub archived: Signal<bool>,
    pub busy: Signal<bool>,
    pub error: Signal<Option<Error>>,
    pub device: Signal<Option<auth::Device>>,
    pub details: Signal<HashMap<String, Detail>>,
    pub selected: Signal<String>,
    pub run_info: Signal<Option<Value>>,
    pub run_busy: Signal<bool>,
    epoch: Signal<u64>,
    auth_task: Signal<Option<day::TaskHandle>>,
    list_task: Signal<Option<day::TaskHandle>>,
    detail_task: Signal<Option<day::TaskHandle>>,
    run_task: Signal<Option<day::TaskHandle>>,
    pub monitor: App,
}
thread_local! { static CURRENT: std::cell::Cell<Option<Hub>> = const { std::cell::Cell::new(None) }; }
pub fn current() -> Option<Hub> {
    CURRENT.with(|h| h.get())
}
pub fn api() -> Option<Rc<Client>> {
    current().and_then(|h| h.api.get())
}
impl Hub {
    pub fn new(monitor: App) -> Self {
        let h = Self {
            api: Signal::global(None),
            login: Signal::global(String::new()),
            repos: Signal::global(vec![]),
            favorites: Signal::global(BTreeSet::new()),
            hidden: Signal::global(crate::visibility::Hidden::default()),
            show_hidden: Signal::global(false),
            collapsed_sections: Signal::global(std::collections::HashSet::new()),
            search: Signal::global(String::new()),
            sort: Signal::global(
                day::prefs::get("hub.sort")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            ),
            archived: Signal::global(day::prefs::get("hub.archived").as_deref() != Some("false")),
            busy: Signal::global(false),
            error: Signal::global(None),
            device: Signal::global(None),
            details: Signal::global(HashMap::new()),
            selected: Signal::global("account".into()),
            run_info: Signal::global(None),
            run_busy: Signal::global(false),
            epoch: Signal::global(0),
            auth_task: Signal::global(None),
            list_task: Signal::global(None),
            detail_task: Signal::global(None),
            run_task: Signal::global(None),
            monitor,
        };
        CURRENT.with(|slot| slot.set(Some(h)));
        day::reactive::watch(
            move || h.sort.get(),
            move |v, _| {
                day::prefs::set("hub.sort", &v.to_string());
            },
        );
        day::reactive::watch(
            move || h.archived.get(),
            move |v, _| {
                day::prefs::set("hub.archived", &v.to_string());
            },
        );
        // A fixture mode drives UI checks without reading the real Keychain or contacting GitHub.
        if is_fixture() {
            h.fixture();
        } else {
            match auth::restore() {
                Ok(Some(token)) => {
                    h.api.set(Some(Client::new(token)));
                    h.reload();
                }
                Ok(None) => {}
                Err(e) => h.error.set(Some(e)),
            }
        }
        h
    }
    pub fn sign_in(self) {
        self.cancel_login();
        self.busy.set(true);
        self.error.set(None);
        let epoch = self.epoch.get();
        let task = day::task(async move {
            let result = async {
                let device = auth::begin(CLIENT_ID).await?;
                if self.epoch.get() != epoch {
                    return Err(Error::Auth);
                }
                self.device.set(Some(device.clone()));
                // Keep the app in front until the user has seen and copied the code. The
                // account page explicitly opens the browser; polling can start meanwhile.
                let token = auth::finish(device).await?;
                if self.epoch.get() != epoch {
                    return Err(Error::Auth);
                }
                // Confirm the credential works before persisting or publishing a session.
                let client = Client::new(token.clone());
                let viewer = client
                    .graphql("query Viewer { viewer { login } }", "Viewer", json!({}))
                    .await?;
                if viewer["viewer"]["login"].as_str().is_none() {
                    return Err(Error::Invalid);
                }
                if self.epoch.get() != epoch {
                    return Err(Error::Auth);
                }
                auth::save(&token)?;
                Ok(client)
            }
            .await;
            if self.epoch.get() != epoch {
                return;
            }
            self.device.set(None);
            self.busy.set(false);
            match result {
                Ok(client) => {
                    self.api.set(Some(client));
                    self.monitor.cancel();
                    self.monitor.next.set(Instant::now());
                    self.reload();
                }
                Err(e) => self.error.set(Some(e)),
            }
        });
        self.auth_task.set(Some(task));
    }
    pub fn cancel_login(self) {
        self.epoch.set(self.epoch.get() + 1);
        if let Some(task) = self.auth_task.get() {
            task.abort();
        }
        self.auth_task.set(None);
        self.device.set(None);
        self.busy.set(false);
    }
    pub fn sign_out(self) {
        if !is_fixture()
            && let Err(e) = auth::forget()
        {
            self.error.set(Some(e));
            return;
        }
        self.cancel_login();
        for task in [self.list_task, self.detail_task, self.run_task] {
            if let Some(t) = task.get() {
                t.abort();
            }
            task.set(None);
        }
        self.monitor.cancel();
        self.monitor.projects.update(|projects| {
            for p in projects {
                p.runs.clear();
                p.error = None;
            }
        });
        self.api.set(None);
        self.login.set(String::new());
        self.repos.set(vec![]);
        self.details.set(HashMap::new());
        self.favorites.set(BTreeSet::new());
        self.hidden.set(Default::default());
        self.show_hidden.set(false);
        self.collapsed_sections.set(Default::default());
        self.run_info.set(None);
        self.run_busy.set(false);
        self.error.set(None);
        self.selected.set("account".into());
    }
    pub fn reload(self) {
        let Some(api) = self.api.get() else {
            return;
        };
        if let Some(t) = self.list_task.get() {
            t.abort();
        }
        self.busy.set(true);
        self.error.set(None);
        let epoch = self.epoch.get();
        self.list_task.set(Some(day::task(async move {
            let result = api.repositories().await;
            if self.epoch.get() != epoch {
                return;
            }
            self.busy.set(false);
            match result {
                Ok((login, values)) => match values
                    .into_iter()
                    .map(serde_json::from_value::<Repository>)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(mut repos) => {
                        repos.sort_by_key(|r| r.id);
                        repos.dedup_by_key(|r| r.id);
                        self.favorites.set(
                            day::prefs::get(&format!("hub.favorites.{login}"))
                                .and_then(|s| serde_json::from_str(&s).ok())
                                .unwrap_or_default(),
                        );
                        if self.login.get() != login {
                            self.collapsed_sections.set(
                                day::prefs::get(&format!("hub.collapsedSections.{login}"))
                                    .and_then(|s| serde_json::from_str(&s).ok())
                                    .unwrap_or_default(),
                            );
                            self.hidden.set(
                                day::prefs::get(&format!("hub.hidden.{login}"))
                                    .and_then(|s| serde_json::from_str(&s).ok())
                                    .unwrap_or_default(),
                            );
                            self.show_hidden.set(
                                day::prefs::get(&format!("hub.showHidden.{login}")).as_deref()
                                    == Some("true"),
                            );
                        }
                        self.login.set(login);
                        self.repos.set(repos);
                    }
                    Err(_) => self.error.set(Some(Error::Invalid)),
                },
                Err(e) => self.error.set(Some(e)),
            }
        })));
    }
    pub fn favorite(self, name: &str) {
        self.favorites.update(|set| {
            if !set.remove(name) {
                set.insert(name.into());
            }
        });
        if is_fixture() {
            return;
        }
        day::prefs::set(
            &format!("hub.favorites.{}", self.login.get()),
            &serde_json::to_string(&self.favorites.get()).unwrap(),
        );
    }
    pub fn save_collapsed_sections(self) {
        let login = self.login.get();
        if is_fixture() || login.is_empty() {
            return;
        }
        let value = serde_json::to_string(&self.collapsed_sections.get()).unwrap();
        if !day::prefs::set(&format!("hub.collapsedSections.{login}"), &value) {
            self.error.set(Some(Error::Preferences));
        }
    }
    fn save_visibility(self) {
        let login = self.login.get();
        if is_fixture() || login.is_empty() {
            return;
        }
        let hidden = serde_json::to_string(&self.hidden.get()).unwrap();
        if !(day::prefs::set(&format!("hub.hidden.{login}"), &hidden)
            && day::prefs::set(
                &format!("hub.showHidden.{login}"),
                &self.show_hidden.get().to_string(),
            ))
        {
            self.error.set(Some(Error::Preferences));
        }
    }
    pub fn toggle_hidden(self, name: &str, group: bool) {
        self.hidden.update(|hidden| {
            if group {
                hidden.toggle_owner(name);
            } else {
                hidden.toggle_repository(name);
            }
        });
        self.save_visibility();
    }
    pub fn toggle_hidden_filter(self) {
        self.show_hidden.set(!self.show_hidden.get());
        self.save_visibility();
    }
    pub fn repository_visible(self, repo: &Repository) -> bool {
        self.show_hidden.get()
            || !self
                .hidden
                .with(|h| h.contains(&repo.full_name, &repo.owner.login))
    }
    pub fn repository(self, name: &str) -> Option<Repository> {
        self.repos
            .with(|repos| repos.iter().find(|r| r.full_name == name).cloned())
    }
    pub fn clear_run(self) {
        if let Some(t) = self.run_task.get() {
            t.abort();
        }
        self.run_task.set(None);
        self.run_info.set(None);
        self.run_busy.set(false);
    }
    pub fn load(self, name: String, force: bool) {
        if !force
            && self.details.with(|all| {
                all.get(&name)
                    .and_then(|d| d.loaded)
                    .is_some_and(|t| t.elapsed().as_secs() < 120)
            })
        {
            return;
        }
        let Some(api) = self.api.get() else {
            return;
        };
        if let Some(t) = self.detail_task.get() {
            t.abort();
        }
        let epoch = self.epoch.get();
        self.details.update(|all| {
            all.entry(name.clone()).or_default().error = None;
        });
        self.detail_task.set(Some(day::task(async move {
            let Some((owner, repo)) = name.split_once('/') else {
                return;
            };
            let overview = api
                .graphql(OVERVIEW, "Overview", json!({"owner":owner,"name":repo}))
                .await;
            if self.epoch.get() != epoch {
                return;
            }
            self.details.update(|all| {
                let d = all.entry(name.clone()).or_default();
                match overview {
                    Ok(value) if !value["repository"].is_null() => {
                        d.overview = Some(value["repository"].clone())
                    }
                    Ok(_) => d.error = Some(Error::Forbidden),
                    Err(e) => d.error = Some(e),
                }
            });
            let runs = api
                .page(&format!(
                    "https://api.github.com/repos/{name}/actions/runs?per_page=50"
                ))
                .await;
            if self.epoch.get() != epoch {
                return;
            }
            self.details.update(|all| {
                let d = all.entry(name.clone()).or_default();
                match runs {
                    Ok((value, next)) => {
                        let Some(runs) = value["workflow_runs"].as_array() else {
                            d.error = Some(Error::Invalid);
                            return;
                        };
                        d.runs = runs.clone();
                        d.runs_next = next;
                        d.loaded = Some(Instant::now());
                    }
                    Err(e) => d.error = Some(e),
                }
            });
        })));
    }
    pub fn more_runs(self, name: String) {
        let Some(next) = self
            .details
            .with(|all| all.get(&name).and_then(|d| d.runs_next.clone()))
        else {
            return;
        };
        let Some(api) = self.api.get() else {
            return;
        };
        if let Some(t) = self.detail_task.get() {
            t.abort();
        }
        let epoch = self.epoch.get();
        self.detail_task.set(Some(day::task(async move {
            let result = api.page(&next).await;
            if self.epoch.get() != epoch {
                return;
            }
            self.details.update(|all| {
                let d = all.entry(name).or_default();
                match result {
                    Ok((value, next)) => {
                        let Some(runs) = value["workflow_runs"].as_array() else {
                            d.error = Some(Error::Invalid);
                            return;
                        };
                        d.runs.extend(runs.iter().cloned());
                        let mut seen = BTreeSet::new();
                        d.runs.retain(|r| seen.insert(r["id"].as_u64()));
                        d.runs_next = next;
                    }
                    Err(e) => d.error = Some(e),
                }
            });
        })));
    }
    pub fn load_run(self, name: String, id: u64) {
        if is_fixture() {
            // Synthetic publication data; this branch never touches GitHub or credentials.
            let mut run = self
                .details
                .with(|all| {
                    all.get(&name)
                        .and_then(|d| d.runs.iter().find(|r| r["id"] == id).cloned())
                })
                .unwrap_or_default();
            run["jobs"] = json!([{"id":200,"name":"macOS tests","status":"completed","conclusion":"success","runner_name":"synthetic-runner","steps":[{"number":1,"name":"Check out fixture","status":"completed","conclusion":"success"},{"number":2,"name":"Run synthetic tests","status":"completed","conclusion":"success"}]}]);
            run["artifacts"] = json!([{"id":300,"name":"fixture-results","size_in_bytes":2048,"expires_at":"2026-11-08T10:00:00Z"}]);
            self.run_info.set(Some(run));
            return;
        }
        let Some(api) = self.api.get() else {
            return;
        };
        if let Some(t) = self.run_task.get() {
            t.abort();
        }
        let epoch = self.epoch.get();
        self.run_busy.set(true);
        self.run_info.set(None);
        self.error.set(None);
        self.run_task.set(Some(day::task(async move {
            let result = async {
                let mut run = api.get(&format!("/repos/{name}/actions/runs/{id}")).await?;
                let jobs = api
                    .pages(
                        &format!("/repos/{name}/actions/runs/{id}/jobs?per_page=100"),
                        Some("jobs"),
                    )
                    .await?;
                run["jobs"] = json!(jobs);
                // Artifact access can be restricted separately. Keep the useful run/jobs data.
                match api
                    .pages(
                        &format!("/repos/{name}/actions/runs/{id}/artifacts?per_page=100"),
                        Some("artifacts"),
                    )
                    .await
                {
                    Ok(artifacts) => run["artifacts"] = json!(artifacts),
                    Err(e) => self.error.set(Some(e)),
                }
                Ok(run)
            }
            .await;
            if self.epoch.get() != epoch {
                return;
            }
            self.run_busy.set(false);
            match result {
                Ok(value) => self.run_info.set(Some(value)),
                Err(e) => self.error.set(Some(e)),
            }
        })));
    }
    fn fixture(self) {
        self.login.set("fixture-user".into());
        let data = json!([
            {"id":1,"name":"day","full_name":"fixture-org/day","owner":{"login":"fixture-org"},"html_url":"https://github.com/fixture-org/day","updated_at":"2026-10-08T10:00:00Z","description":"Synthetic repository used for interface validation.","stargazers_count":42,"forks_count":8,"language":"Rust"},
            {"id":2,"name":"charts","full_name":"fixture-user/charts","owner":{"login":"fixture-user"},"html_url":"https://github.com/fixture-user/charts","updated_at":"2026-10-07T10:00:00Z","private":true,"stargazers_count":5,"language":"Rust"}]);
        self.repos.set(serde_json::from_value(data).unwrap());
        self.favorites
            .set(BTreeSet::from(["fixture-org/day".into()]));
        self.details.update(|all|{all.insert("fixture-org/day".into(),Detail{overview:Some(json!({"description":"Synthetic repository used for interface validation.","stargazerCount":42,"forkCount":8,"diskUsage":1024,"updatedAt":"2026-10-08T10:00:00Z","licenseInfo":{"name":"Mozilla Public License 2.0"},"issues":{"totalCount":12},"pullRequests":{"totalCount":3},"watchers":{"totalCount":6},"languages":{"edges":[{"size":800,"node":{"name":"Rust"}},{"size":200,"node":{"name":"Swift"}}]},"defaultBranchRef":{"name":"main","target":{"history":{"nodes":[{"committedDate":"2026-10-08T10:00:00Z","additions":24,"deletions":8},{"committedDate":"2026-10-07T10:00:00Z","additions":42,"deletions":12}]}}}})),runs:vec![json!({"id":100,"display_title":"Build and test","name":"CI","status":"completed","conclusion":"success","head_branch":"main","event":"push","run_number":128,"run_attempt":1,"created_at":"2026-10-08T10:00:00Z","run_started_at":"2026-10-08T10:00:00Z","updated_at":"2026-10-08T10:03:00Z","html_url":"https://github.com/fixture-org/day/actions/runs/100"})],loaded:Some(Instant::now()),..Default::default()});});
    }
}
pub const OVERVIEW: &str = r#"query Overview($owner:String!,$name:String!) { repository(owner:$owner,name:$name) {
 nameWithOwner description url homepageUrl visibility isArchived isFork createdAt updatedAt pushedAt
 stargazerCount forkCount diskUsage licenseInfo { name spdxId } watchers { totalCount }
 issues(states:OPEN) { totalCount } pullRequests(states:OPEN) { totalCount }
 releases(first:1,orderBy:{field:CREATED_AT,direction:DESC}) { nodes { name tagName publishedAt url } }
 languages(first:20,orderBy:{field:SIZE,direction:DESC}) { edges { size node { name color } } }
 repositoryTopics(first:20) { nodes { topic { name } } }
 defaultBranchRef { name target { ... on Commit { history(first:100) { totalCount nodes { oid committedDate additions deletions messageHeadline author { name } } } } } }
 } rateLimit { cost remaining resetAt } }"#;

pub fn migrate_preferences() {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSString, NSUserDefaults};
        let defaults = NSUserDefaults::standardUserDefaults();
        if let Some(old) =
            defaults.persistentDomainForName(&NSString::from_str("dev.daybrite.hublights"))
        {
            for key in [
                "repositories",
                "updateInterval",
                "statusBadgeCount",
                "paused",
                "grid.rows",
                "grid.columnWidth",
                "grid.maxWidth",
            ] {
                if !day::prefs::contains(key)
                    && let Some(value) = old.objectForKey(&NSString::from_str(key))
                    && let Some(value) = value.downcast_ref::<NSString>()
                {
                    day::prefs::set(key, &value.to_string());
                }
            }
        }
    }
}
