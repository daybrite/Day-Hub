use crate::{
    api::Error,
    hub::{Detail, Hub, Repository, Sort},
    model::{Run, Status},
    res,
};
use day::prelude::*;
use day_piece_charts::{bar, chart, value};
use day_piece_settings::Prefs;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn show_main() {
    if let Some(window) = day::initial_window() {
        window.set_visible(true);
        window.focus();
    } else {
        day::open_new_window();
    }
}
pub fn error_text(error: &Error) -> String {
    match error {
        Error::Preferences => res::str::save_failed().format(),
        Error::Network => res::str::api_network().format(),
        Error::Auth => res::str::auth_error().format(),
        Error::Forbidden => res::str::api_forbidden().format(),
        Error::Sso => res::str::sso_error().format(),
        Error::Invalid => res::str::api_invalid().format(),
        Error::Keychain => res::str::keychain_error().format(),
        Error::MissingClient => res::str::missing_client().format(),
        Error::Http(code) => res::str::api_http(code.to_string()).format(),
        Error::RateLimited(s) => res::str::api_retry(s.to_string()).format(),
    }
}
fn status(value: &Value) -> Status {
    match value["conclusion"]
        .as_str()
        .unwrap_or_else(|| value["status"].as_str().unwrap_or_default())
    {
        "success" => Status::Success,
        "failure" | "timed_out" | "action_required" | "startup_failure" => Status::Failure,
        "in_progress" => Status::Running,
        "queued" | "waiting" | "pending" | "requested" => Status::Queued,
        "cancelled" => Status::Cancelled,
        "skipped" | "neutral" => Status::Skipped,
        _ => Status::Unknown,
    }
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}
fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}
fn duration(value: &Value) -> u64 {
    let parse = |key| {
        value[key]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    };
    match (parse("run_started_at"), parse("updated_at")) {
        (Some(start), Some(end)) => (end - start).num_seconds().max(0) as u64,
        _ => 0,
    }
}
pub fn monitor_run(value: &Value) -> Run {
    let elapsed = duration(value);
    Run {
        path: url::Url::parse(&text(value, "html_url"))
            .ok()
            .map(|u| u.path().to_string())
            .unwrap_or_default(),
        title: text(value, "display_title"),
        status: status(value),
        started: text(value, "run_started_at"),
        attempt: value["run_attempt"]
            .as_u64()
            .and_then(|attempt| u32::try_from(attempt).ok())
            .filter(|attempt| *attempt > 0),
        duration: res::str::duration_hours_minutes(res::str::DurationHoursMinutes {
            hours: format!("{:02}", elapsed / 3600),
            minutes: format!("{:02}", elapsed / 60 % 60),
        })
        .format(),
    }
}
pub fn hidden_filter_menu(hub: Hub) -> MenuEntry {
    menu_item(if hub.show_hidden.get() {
        res::str::hide_hidden_repositories()
    } else {
        res::str::show_hidden_repositories()
    })
    .id(day::id!(toggle_hidden_repositories))
    .checked(hub.show_hidden.get())
    .action(move || hub.toggle_hidden_filter())
}
fn owner_context_menu(hub: Hub, owner: String) -> Vec<MenuEntry> {
    let hidden = hub.hidden.with(|h| h.owner(&owner));
    vec![
        menu_item(if hidden {
            res::str::restore_owner(owner.clone())
        } else {
            res::str::hide_owner(owner.clone())
        })
        .id(day::id!(toggle_hidden_owner))
        .action(move || hub.toggle_hidden(&owner, true)),
    ]
}
fn repository_context_menu(hub: Hub, name: String, owner: String) -> Vec<MenuEntry> {
    let favorite = hub.favorites.with(|set| set.contains(&name));
    let hidden = hub.hidden.with(|h| h.repository(&name));
    let favorite_name = name.clone();
    let mut menu = vec![
        menu_item(if favorite {
            res::str::unfavorite()
        } else {
            res::str::favorite()
        })
        .action(move || hub.favorite(&favorite_name)),
        menu_item(if hidden {
            res::str::restore_repository()
        } else {
            res::str::hide_repository()
        })
        .id(day::id!(toggle_hidden_repository))
        .action(move || hub.toggle_hidden(&name, false)),
    ];
    menu.extend(owner_context_menu(hub, owner));
    menu
}
#[derive(Clone)]
struct SidebarRow {
    key: String,
    title: String,
    section: Option<String>,
    name: String,
    owner: String,
    section_id: String,
}
fn visible_repositories(hub: Hub) -> Vec<Repository> {
    let filter = hub.search.get().to_lowercase();
    let archived = hub.archived.get();
    let mut repos: Vec<_> = hub
        .repos
        .get()
        .into_iter()
        .filter(|r| {
            hub.repository_visible(r)
                && (archived || !r.archived)
                && (r.full_name.to_lowercase().contains(&filter)
                    || r.description
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&filter))
        })
        .collect();
    sort_repositories(&mut repos, hub.sort.get());
    repos
}
fn rows(hub: Hub) -> Vec<SidebarRow> {
    let repos = visible_repositories(hub);
    let favorites = hub.favorites.get();
    let mut result = Vec::new();
    for r in repos.iter().filter(|r| favorites.contains(&r.full_name)) {
        result.push(SidebarRow {
            key: format!("favorite:{}", r.full_name.replace('/', "~")),
            title: r.full_name.clone(),
            section: result.is_empty().then(|| res::str::favorites().format()),
            name: r.full_name.clone(),
            owner: r.owner.login.clone(),
            section_id: "favorites".into(),
        });
    }
    let mut groups: BTreeMap<String, Vec<Repository>> = BTreeMap::new();
    for r in repos {
        groups.entry(r.owner.login.clone()).or_default().push(r);
    }
    for (owner, repos) in groups {
        for (index, r) in repos.into_iter().enumerate() {
            result.push(SidebarRow {
                key: format!("repo:{}", r.full_name.replace('/', "~")),
                title: r.name,
                section: (index == 0).then(|| owner.clone()),
                section_id: format!("owner:{}", owner.to_lowercase()),
                name: r.full_name,
                owner: owner.clone(),
            });
        }
    }
    result
}
fn sort_repositories(repos: &mut [Repository], order: Sort) {
    repos.sort_by(|a, b| {
        match order {
            Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            Sort::Stars => b.stargazers_count.cmp(&a.stargazers_count),
            Sort::Updated => b.updated_at.cmp(&a.updated_at),
        }
        .then_with(|| a.full_name.cmp(&b.full_name))
    });
}
pub fn window(hub: Hub, prefs: Prefs) -> impl Piece {
    let navigation = nav(hub.selected)
        .style(NavStyle::Sidebar)
        .sections(
            Sections::collapsible(hub.collapsed_sections)
                .on_change(move |_, _| hub.save_collapsed_sections()),
        )
        .item("account".to_string(), res::str::account(), move || {
            account(hub)
        })
        .items((
            // Desktop reaches Settings through its menu and window; without a menu bar the
            // settings page is a sidebar section (https://daybrite.dev/docs/reference/settings).
            prefs.nav_item(day_piece_settings::NAV_KEY.to_string()),
            nav_items(
                move || rows(hub),
                move |r| {
                    let mut row = item(r.key.clone(), r.title.clone()).context_menu(
                        repository_context_menu(hub, r.name.clone(), r.owner.clone()),
                    );
                    if hub.hidden.with(|h| h.owner(&r.owner)) {
                        row = row.badge(res::str::hidden_owner_badge());
                    } else if hub.hidden.with(|h| h.repository(&r.name)) {
                        row = row.badge(res::str::hidden_repository_badge());
                    }
                    if let Some(section) = &r.section {
                        row = row.section_id(r.section_id.clone(), section.clone());
                    }
                    row
                },
            ),
        ))
        .destination(move |key: &String| {
            repository_page(
                hub,
                key.split_once(':')
                    .map(|(_, n)| n)
                    .unwrap_or_default()
                    .replace('~', "/"),
            )
        })
        .retain_selection_when(move |key| {
            if key == "account" || key == day_piece_settings::NAV_KEY {
                return true;
            }
            key.split_once(':')
                .and_then(|(_, name)| hub.repository(&name.replace('~', "/")))
                .is_some_and(|r| hub.repository_visible(&r))
        })
        .id(day::id!(hub_nav));
    navigation
        .searchable(Search::new(hub.search).prompt(res::str::search_repositories()))
        .header(move || {
            column((
                picker(
                    hub.sort,
                    [
                        (Sort::Updated, res::str::sort_updated()),
                        (Sort::Name, res::str::sort_name()),
                        (Sort::Stars, res::str::sort_stars()),
                    ],
                )
                .id(day::id!(repository_sort))
                .grow_w(),
                labeled(res::str::show_archived(), toggle(hub.archived)),
                label(move || {
                    res::str::sidebar_repository_count(res::str::SidebarRepositoryCount {
                        total: hub.repos.with(|r| r.len()) as i64,
                        visible: visible_repositories(hub).len() as i64,
                    })
                    .format()
                })
                .id(day::id!(sidebar_repository_count)),
                label(move || {
                    let _ = hub.monitor.clock.get();
                    let wait = hub.api.get().map(|a| a.retry_in()).unwrap_or(0);
                    if wait > 0 {
                        res::str::api_retry(wait.to_string()).format()
                    } else {
                        hub.error.get().as_ref().map(error_text).unwrap_or_default()
                    }
                })
                .id(day::id!(hub_error)),
            ))
            .spacing(10.0)
            .padding(Space::M)
        })
}

fn account(hub: Hub) -> impl Piece {
    scroll(
        column((
            label(res::str::sign_in_title()).font(Font::Title),
            label(res::str::sign_in_help()),
            when(
                move || hub.login.get().is_empty(),
                move || {
                    column((
                        button(res::str::sign_in())
                            .id(day::id!(github_sign_in))
                            .enabled(move || !hub.busy.get())
                            .action(move || hub.sign_in()),
                        when(
                            move || hub.device.get().is_some(),
                            move || {
                                let copy_status = Signal::new(0u8);
                                column((
                                    label(res::str::login_code_help()),
                                    label(move || {
                                        hub.device
                                            .get()
                                            .map(|d| {
                                                res::str::login_code(
                                                    d.details.user_code().secret().clone(),
                                                )
                                                .format()
                                            })
                                            .unwrap_or_default()
                                    })
                                    .font(Font::Title)
                                    .selectable()
                                    .id(day::id!(github_device_code)),
                                    button(res::str::copy_code_open_login())
                                        .id(day::id!(github_copy_code_open))
                                        .action(move || {
                                            let Some(device) = hub.device.get() else {
                                                return;
                                            };
                                            if day::clipboard::set_text(
                                                device.details.user_code().secret(),
                                            ) {
                                                copy_status.set(1);
                                                // Only open GitHub's fixed device URL; no codes in URLs or logs.
                                                open_url("https://github.com/login/device");
                                            } else {
                                                copy_status.set(2);
                                            }
                                        }),
                                    label(move || match copy_status.get() {
                                        1 => res::str::login_code_copied().format(),
                                        2 => res::str::login_copy_failed().format(),
                                        _ => String::new(),
                                    })
                                    .id(day::id!(github_copy_status)),
                                    label(res::str::login_wait()),
                                    button(res::str::open_login())
                                        .id(day::id!(github_open_login))
                                        .action(|| open_url("https://github.com/login/device")),
                                    button(res::str::cancel_login())
                                        .id(day::id!(cancel_login))
                                        .action(move || hub.cancel_login()),
                                ))
                                .spacing(Space::M)
                            },
                        ),
                    ))
                    .spacing(Space::L)
                },
            )
            .otherwise(move || {
                column((
                    label(move || res::str::signed_in(hub.login.get()).format())
                        .font(Font::Headline)
                        .id(day::id!(github_account)),
                    label(move || {
                        res::str::repository_count(hub.repos.with(|r| r.len()) as i64).format()
                    })
                    .id(day::id!(repository_count)),
                    row((
                        button(res::str::refresh_repositories())
                            .enabled(move || !hub.busy.get())
                            .action(move || hub.reload()),
                        button(res::str::sign_out())
                            .id(day::id!(github_sign_out))
                            .action(move || hub.sign_out()),
                    ))
                    .spacing(Space::M),
                ))
                .spacing(Space::M)
            }),
            when(
                move || hub.busy.get() && hub.device.get().is_none(),
                || row((spinner(), label(res::str::loading_repositories()))).spacing(10.0),
            ),
            label(res::str::access_help()),
            button(res::str::manage_access()).action(|| {
                open_url(
                    "https://github.com/settings/connections/applications/Ov23li46VVDMelb0esrd",
                )
            }),
            label(res::str::favorites_help()),
            label(res::str::monitor_help()),
            button(res::str::settings()).action(|| {
                day_piece_settings::open();
            }),
        ))
        .spacing(20.0)
        .padding(28.0),
    )
    .grow()
}
fn repository_page(hub: Hub, name: String) -> AnyPiece {
    let Some(repo) = hub.repository(&name) else {
        return label(res::str::no_repositories()).any();
    };
    hub.clear_run();
    hub.load(name.clone(), false);
    let favorite_name = name.clone();
    let refresh_name = name.clone();
    let overview_name = name.clone();
    let actions_name = name.clone();
    let monitor_name = name.clone();
    let button_name = name.clone();
    let favorite = move || {
        if hub.favorites.with(|set| set.contains(&favorite_name)) {
            res::str::unfavorite().format()
        } else {
            res::str::favorite().format()
        }
    };
    let menu_name = name.clone();
    let menu_owner = repo.owner.login.clone();
    column((
        label(repo.full_name)
            .font(Font::Title)
            .id(day::id!(repository_title))
            .context_menu(move || {
                repository_context_menu(hub, menu_name.clone(), menu_owner.clone())
            }),
        label(repo.description.unwrap_or_default()),
        row((
            button(favorite)
                .id(day::id!(favorite_repository))
                .action(move || hub.favorite(&button_name)),
            button(res::str::open_repository()).action(move || open_url(&repo.html_url)),
            button(res::str::refresh_repository())
                .action(move || hub.load(refresh_name.clone(), true)),
            button(res::str::monitor_repository())
                .enabled(move || {
                    !hub.monitor
                        .projects
                        .with(|projects| projects.iter().any(|p| p.repo == monitor_name))
                })
                .action(move || {
                    // Appending to the saved list applies it: the monitor watches the setting.
                    let mut input = crate::REPOSITORIES.get_untracked();
                    if !input.is_empty() && !input.ends_with('\n') {
                        input.push('\n');
                    }
                    input.push_str(&name);
                    if crate::model::parse_projects(&input).is_ok() {
                        crate::REPOSITORIES.set(input);
                    }
                }),
        ))
        .spacing(10.0),
        nav(Signal::new("overview".to_string()))
            .style(NavStyle::Tabs)
            .item("overview".to_string(), res::str::overview(), move || {
                overview(hub, overview_name.clone())
            })
            .item("actions".to_string(), res::str::actions(), move || {
                actions(hub, actions_name.clone())
            })
            .id(day::id!(repository_tabs))
            .grow(),
    ))
    .spacing(Space::M)
    .padding(20.0)
    .grow()
    .any()
}
fn detail(hub: Hub, name: String) -> day::reactive::Memo<Detail> {
    day::reactive::Memo::new(move || {
        hub.details
            .with(|all| all.get(&name).cloned().unwrap_or_default())
    })
}
fn overview(hub: Hub, name: String) -> impl Piece {
    let data = detail(hub, name);
    let val = move || data.get().overview.unwrap_or_default();
    let languages = chart(move || {
        val()["languages"]["edges"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                bar(
                    value(res::str::language_axis().format(), text(&e["node"], "name")),
                    value(res::str::bytes_axis().format(), number(e, "size") as f64),
                )
            })
            .collect()
    })
    .height(220.0)
    .grow_w()
    .id(day::id!(language_chart));
    let commits = chart(move || {
        let mut days: BTreeMap<String, u64> = BTreeMap::new();
        for c in val()["defaultBranchRef"]["target"]["history"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let day = text(c, "committedDate")
                .chars()
                .take(10)
                .collect::<String>();
            *days.entry(day).or_default() += 1;
        }
        days.into_iter()
            .map(|(date, n)| {
                bar(
                    value(res::str::date_axis().format(), date),
                    value(res::str::commits_axis().format(), n as f64),
                )
            })
            .collect()
    })
    .height(230.0)
    .grow_w()
    .id(day::id!(commit_chart));
    scroll(
        column((
            label(move || {
                data.get()
                    .error
                    .as_ref()
                    .map(error_text)
                    .unwrap_or_default()
            }),
            when(
                move || data.get().overview.is_none() && data.get().error.is_none(),
                || label(res::str::loading_detail()),
            ),
            label(res::str::repository_stats()).font(Font::Headline),
            row((
                label(move || {
                    res::str::stars_value(number(&val(), "stargazerCount").to_string()).format()
                }),
                label(move || {
                    res::str::forks_value(number(&val(), "forkCount").to_string()).format()
                }),
                label(move || {
                    res::str::issues_value(number(&val()["issues"], "totalCount").to_string())
                        .format()
                }),
                label(move || {
                    res::str::pulls_value(number(&val()["pullRequests"], "totalCount").to_string())
                        .format()
                }),
            ))
            .spacing(18.0),
            label(move || {
                res::str::branch_value(text(&val()["defaultBranchRef"], "name")).format()
            }),
            label(move || res::str::updated_value(text(&val(), "updatedAt")).format()),
            label(move || {
                res::str::license_value(
                    val()["licenseInfo"]["name"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| res::str::not_specified().format()),
                )
                .format()
            }),
            label(res::str::languages()).font(Font::Headline),
            languages,
            label(res::str::commit_activity()).font(Font::Headline),
            label(res::str::commit_help()),
            commits,
        ))
        .spacing(14.0)
        .padding(Space::M),
    )
    .grow()
}
fn actions(hub: Hub, name: String) -> impl Piece {
    let data = detail(hub, name.clone());
    let load_name = name.clone();
    let raw = Signal::new(false);
    let duration_chart = chart(move || {
        data.get()
            .runs
            .iter()
            .filter(|r| r["status"] == "completed")
            .rev()
            .map(|r| {
                bar(
                    value(
                        res::str::run_axis().format(),
                        number(r, "run_number").to_string(),
                    ),
                    value(res::str::seconds_axis().format(), duration(r) as f64),
                )
            })
            .collect()
    })
    .height(170.0)
    .grow_w()
    .id(day::id!(run_chart));
    scroll(
        column((
            label(move || {
                data.get()
                    .error
                    .as_ref()
                    .map(error_text)
                    .unwrap_or_default()
            })
            .any(),
            label(res::str::run_history()).font(Font::Headline).any(),
            label(move || {
                let runs = data.get().runs;
                let completed = runs.iter().filter(|r| r["status"] == "completed").count();
                let success = runs.iter().filter(|r| r["conclusion"] == "success").count();
                if completed == 0 {
                    res::str::run_summary_pending(runs.len() as i64).format()
                } else {
                    res::str::run_summary(res::str::RunSummary {
                        count: runs.len() as i64,
                        rate: format!("{:.0}", success as f64 / completed as f64 * 100.0),
                    })
                    .format()
                }
            })
            .any(),
            label(res::str::run_duration_chart()).any(),
            duration_chart.any(),
            when(
                move || data.get().runs.is_empty(),
                || label(res::str::no_runs()),
            )
            .any(),
            each(
                items(move || data.get().runs, |r: &Value| number(r, "id")),
                move |slot| {
                    let name = name.clone();
                    column((
                        button(move || {
                            let r = slot.get();
                            res::str::run_row(res::str::RunRow {
                                branch: text(&r, "head_branch"),
                                number: number(&r, "run_number").to_string(),
                                status: crate::status_label(status(&r)),
                                title: text(&r, "display_title"),
                            })
                            .format()
                        })
                        .id_of(move || day::id!(run, number(&slot.get(), "id")))
                        .action(move || hub.load_run(name.clone(), number(&slot.get(), "id"))),
                        label(move || {
                            let r = slot.get();
                            res::str::run_context(res::str::RunContext {
                                attempt: number(&r, "run_attempt").to_string(),
                                date: text(&r, "created_at"),
                                event: text(&r, "event"),
                            })
                            .format()
                        }),
                    ))
                    .spacing(Space::XS)
                },
            )
            .any(),
            when(
                move || data.get().runs_next.is_some(),
                move || {
                    button(res::str::load_more_runs()).action({
                        let n = load_name.clone();
                        move || hub.more_runs(n.clone())
                    })
                },
            )
            .any(),
            divider().any(),
            label(res::str::run_details()).font(Font::Headline).any(),
            when(
                move || hub.run_busy.get(),
                || row((spinner(), label(res::str::loading_run()))),
            )
            .any(),
            when(
                move || hub.run_info.get().is_none() && !hub.run_busy.get(),
                || label(res::str::select_run()),
            )
            .any(),
            when(
                move || hub.run_info.get().is_some(),
                move || {
                    column((
                        button(res::str::open_run())
                            .action(move || {
                                if let Some(info) = hub.run_info.get() {
                                    open_url(&text(&info, "html_url"));
                                }
                            })
                            .any(),
                        label(res::str::jobs())
                            .font(Font::Headline)
                            .id(day::id!(run_jobs))
                            .any(),
                        each(
                            items(
                                move || {
                                    hub.run_info
                                        .get()
                                        .and_then(|v| v["jobs"].as_array().cloned())
                                        .unwrap_or_default()
                                },
                                |v: &Value| number(v, "id"),
                            ),
                            move |job| {
                                column((
                                    label(move || {
                                        res::str::job_row(res::str::JobRow {
                                            name: text(&job.get(), "name"),
                                            status: crate::status_label(status(&job.get())),
                                        })
                                        .format()
                                    })
                                    .font(Font::Headline),
                                    each(
                                        items(
                                            move || {
                                                job.get()["steps"]
                                                    .as_array()
                                                    .cloned()
                                                    .unwrap_or_default()
                                            },
                                            |v: &Value| number(v, "number"),
                                        ),
                                        move |step| {
                                            label(move || {
                                                res::str::step_row(res::str::StepRow {
                                                    name: text(&step.get(), "name"),
                                                    number: number(&step.get(), "number")
                                                        .to_string(),
                                                    status: crate::status_label(status(
                                                        &step.get(),
                                                    )),
                                                })
                                                .format()
                                            })
                                        },
                                    ),
                                ))
                                .spacing(6.0)
                            },
                        )
                        .any(),
                        label(res::str::artifacts()).font(Font::Headline).any(),
                        each(
                            items(
                                move || {
                                    hub.run_info
                                        .get()
                                        .and_then(|v| v["artifacts"].as_array().cloned())
                                        .unwrap_or_default()
                                },
                                |v: &Value| number(v, "id"),
                            ),
                            move |artifact| {
                                label(move || {
                                    res::str::artifact_row(res::str::ArtifactRow {
                                        bytes: number(&artifact.get(), "size_in_bytes").to_string(),
                                        date: text(&artifact.get(), "expires_at"),
                                        name: text(&artifact.get(), "name"),
                                    })
                                    .format()
                                })
                            },
                        )
                        .any(),
                        labeled(res::str::show_raw(), toggle(raw)).any(),
                        when(
                            move || raw.get(),
                            move || {
                                let json_text = Signal::new(
                                    serde_json::to_string_pretty(&hub.run_info.get())
                                        .unwrap_or_default(),
                                );
                                day::reactive::watch(
                                    move || hub.run_info.get(),
                                    move |v, _| {
                                        json_text.set(
                                            serde_json::to_string_pretty(v).unwrap_or_default(),
                                        );
                                    },
                                );
                                text_area(json_text)
                                    .editable(false)
                                    .min_lines(8)
                                    .max_lines(20)
                            },
                        )
                        .any(),
                    ))
                    .spacing(10.0)
                },
            )
            .any(),
        ))
        .spacing(Space::M)
        .padding(Space::M),
    )
    .grow()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monitor_retains_attempts_and_formats_exact_rerun_counts() {
        day::install_locales(res::locales::DEFAULT, res::locales::CATALOG);
        // Synthetic API responses, including absent and malformed attempt metadata.
        let mut value = serde_json::json!({
            "display_title": "Fixture workflow", "conclusion": "success",
            "html_url": "https://github.com/fixture/repo/actions/runs/1"
        });
        for (attempt, expected) in [(1, 0), (2, 1), (3, 2), (12, 11)] {
            value["run_attempt"] = serde_json::json!(attempt);
            let run = monitor_run(&value);
            assert_eq!(run.reruns(), Some(expected));
            let label = crate::run_menu_label(&run);
            let label: String = label
                .chars()
                .filter(|c| !matches!(c, '\u{2068}' | '\u{2069}'))
                .collect();
            let expected_label = match expected {
                0 => "Passed · Fixture workflow · 00:00".to_string(),
                1 => "Passed · Fixture workflow · 00:00 · 1 rerun".to_string(),
                n => format!("Passed · Fixture workflow · 00:00 · {n} reruns"),
            };
            assert_eq!(label, expected_label);
        }
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!("3"),
            serde_json::json!(u64::MAX),
        ] {
            value["run_attempt"] = invalid;
            let run = monitor_run(&value);
            assert_eq!(run.reruns(), None);
            assert!(!crate::run_menu_label(&run).contains("rerun"));
        }
        value.as_object_mut().unwrap().remove("run_attempt");
        assert_eq!(monitor_run(&value).attempt, None);
    }

    #[test]
    fn workflow_status_and_elapsed_time_use_api_fields() {
        let run = serde_json::json!({"status":"completed","conclusion":"timed_out","run_started_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:03:00Z"});
        assert_eq!(status(&run), Status::Failure);
        assert_eq!(duration(&run), 180);
    }
}
