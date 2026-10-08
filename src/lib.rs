//! Day Hub: a native GitHub workspace and compact Actions menu bar monitor.
use day::prelude::*;
use std::time::{Duration, Instant};

mod api;
mod auth;
mod editor;
pub mod github;
pub mod grid;
mod hub;
pub mod model;
mod ui;
mod visibility;
use model::{FetchError, Project, Status};
day::resources!();
day::day_start!(options: main_window(), root);

pub fn main_window() -> day::WindowOptions {
    day::WindowOptions {
        title_fn: Some(|| res::str::app_title().format()),
        size: Size::new(1180.0, 800.0),
        min_size: Some(Size::new(840.0, 600.0)),
        ..window()
    }
}

pub fn window() -> day::WindowOptions {
    day::WindowOptions {
        locales: Some((res::locales::DEFAULT, res::locales::CATALOG)),
        title_fn: Some(|| res::str::settings_title().format()),
        size: Size::new(640.0, 620.0),
        min_size: Some(Size::new(500.0, 500.0)),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    }
}

// Monitoring is intentionally app-wide: menu items, the first window and reopened settings
// share one store. Global signals outlive the closing of any settings window.
#[derive(Clone, Copy)]
struct App {
    projects: Signal<Vec<Project>>,
    repositories: Signal<String>,
    interval: Signal<u64>,
    history: Signal<usize>,
    grid_options: Signal<grid::Options>,
    grid: day::reactive::Memo<grid::Grid>,
    paused: Signal<bool>,
    progress: Signal<Option<(usize, usize)>>,
    checked: Signal<Option<Instant>>,
    clock: Signal<Instant>,
    next: Signal<Instant>,
    backoff: Signal<Option<Instant>>,
    task: Signal<Option<day::TaskHandle>>,
}

impl App {
    fn new() -> Self {
        let repositories = if hub::is_fixture() {
            // Synthetic monitor configuration: UI checks do not depend on the user's list.
            "fixture/one\n---\nfixture/two".to_string()
        } else {
            day::prefs::get("repositories").unwrap_or_default()
        };
        let projects = Signal::global(model::parse_projects(&repositories).unwrap_or_default());
        let grid_options = Signal::global(grid::Options {
            rows: saved_number("grid.rows", 3, 1, 6) as u32,
            column_width: saved_number("grid.columnWidth", 6, 2, 10) as u32,
            max_width: saved_number("grid.maxWidth", 480, 80, 600) as u32,
        });
        let grid = day::reactive::Scope::root().enter(|| {
            day::reactive::Memo::new(move || {
                grid::render(&model::ordered(projects.get()), grid_options.get())
            })
        });
        Self {
            projects,
            grid_options,
            grid,
            repositories: Signal::global(repositories),
            interval: Signal::global(saved_number("updateInterval", 60, 15, 3600)),
            history: Signal::global(saved_number("statusBadgeCount", 3, 1, 10) as usize),
            paused: Signal::global(day::prefs::get("paused").as_deref() == Some("true")),
            progress: Signal::global(None),
            checked: Signal::global(None),
            clock: Signal::global(Instant::now()),
            next: Signal::global(Instant::now()),
            backoff: Signal::global(None),
            task: Signal::global(None),
        }
    }

    fn refresh(self, force: bool) {
        if self.progress.get().is_some() || self.projects.with(|p| p.is_empty()) {
            return;
        }
        let now = Instant::now();
        if self.backoff.get().is_some_and(|until| until > now) {
            return;
        }
        self.backoff.set(None);
        let projects = model::ordered(self.projects.get());
        let total = projects.len();
        day::info!("Starting refresh of {total} repositories (force={force})");
        self.progress.set(Some((0, total)));
        let handle = day::task(async move {
            let started = Instant::now();
            let mut succeeded = 0;
            let mut failed = 0;
            for (index, project) in projects.into_iter().enumerate() {
                let result = if let Some(api) = hub::api() {
                    match api
                        .get(&format!("/repos/{}/actions/runs?per_page=10", project.repo))
                        .await
                    {
                        Ok(value) => value["workflow_runs"]
                            .as_array()
                            .map(|runs| runs.iter().map(ui::monitor_run).collect())
                            .ok_or(FetchError::Page),
                        Err(api::Error::RateLimited(s)) => Err(FetchError::Backoff(s)),
                        Err(api::Error::Http(s)) => Err(FetchError::Http(s)),
                        Err(api::Error::Auth) => Err(FetchError::Http(401)),
                        Err(api::Error::Forbidden | api::Error::Sso) => Err(FetchError::Http(403)),
                        Err(_) => Err(FetchError::Network),
                    }
                } else {
                    github::fetch(&project.repo, force).await
                };
                if result.is_ok() {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                let retry = match &result {
                    Err(FetchError::Backoff(seconds)) => Some(*seconds),
                    _ => None,
                };
                self.projects.update(|projects| {
                    // Results are applied by identity, never by a list position.
                    if let Some(current) = projects
                        .iter_mut()
                        .find(|p| p.repo.eq_ignore_ascii_case(&project.repo))
                    {
                        match result {
                            Ok(runs) => {
                                current.runs = runs;
                                current.error = None;
                            }
                            Err(error) => current.error = Some(error),
                        }
                    }
                });
                self.progress.set(Some((index + 1, total)));
                if let Some(seconds) = retry {
                    self.backoff
                        .set(Some(Instant::now() + Duration::from_secs(seconds)));
                    break;
                }
            }
            self.checked.set(Some(Instant::now()));
            self.next
                .set(Instant::now() + Duration::from_secs(self.interval.get()));
            self.progress.set(None);
            day::info!(
                "Refresh finished in {} ms: {succeeded} succeeded, {failed} failed, {} deferred",
                started.elapsed().as_millis(),
                total - succeeded - failed,
            );
        });
        self.task.set(Some(handle));
    }

    fn cancel(self) {
        if let Some((done, total)) = self.progress.get() {
            day::info!("Cancelling refresh after {done} of {total} repositories");
        }
        if let Some(task) = self.task.get() {
            task.abort();
        }
        self.task.set(None);
        self.progress.set(None);
    }

    fn summary(self) -> String {
        let projects = self.projects.get();
        let rate = model::pass_rate(&projects)
            .map(|rate| res::str::pass_rate(rate.to_string()).format())
            .unwrap_or_else(|| res::str::no_completed().format());
        res::str::summary(projects.len() as i64, rate).format()
    }

    fn update_text(self) -> String {
        let now = self.clock.get();
        if let Some(until) = self.backoff.get().filter(|until| *until > now) {
            return res::str::retry_later(
                until.saturating_duration_since(now).as_secs().to_string(),
            )
            .format();
        }
        if let Some((done, total)) = self.progress.get() {
            res::str::updating(done.to_string(), total.to_string()).format()
        } else if let Some(at) = self.checked.get() {
            res::str::updated(now.saturating_duration_since(at).as_secs().to_string()).format()
        } else {
            res::str::waiting().format()
        }
    }
}

fn saved_number(key: &str, default: u64, min: u64, max: u64) -> u64 {
    day::prefs::get(key)
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| (min..=max).contains(v))
        .unwrap_or(default)
}

pub fn root() -> impl Piece {
    // html5ever's debug tracing logs every tokenizer transition. Keep normal monitoring
    // quiet while preserving an explicit diagnostic level requested by the developer.
    if std::env::var_os("DAY_LOG").is_none() {
        day::set_log_level(day::log::LevelFilter::Info);
    }
    hub::migrate_preferences();
    let app = App::new();
    let hub = hub::Hub::new(app);
    day::set_dock_visible(true);
    day::register_new_window(move || ui::window(hub));
    day::set_keep_running(day::KeepRunning::Always);
    day::on_open_url(|url| {
        if url == "day-hub://open" {
            ui::show_main();
            true
        } else {
            false
        }
    });
    app_menu_reactive(move || {
        vec![
            sub_menu(
                res::str::app_title().format(),
                vec![
                    menu_item(res::str::open_hub().format()).action(ui::show_main),
                    menu_role(MenuRole::Preferences).action(ui::show_settings),
                    menu_role(MenuRole::Quit),
                ],
            ),
            sub_menu(
                res::str::view_menu().format(),
                vec![ui::hidden_filter_menu(hub)],
            ),
        ]
    });
    status_item("hublights", move || status(app));
    let timer = day::task(async move {
        let mut ticks = 0;
        loop {
            if std::env::var_os("DAY_HUB_FIXTURE").is_none()
                && !app.paused.get()
                && Instant::now() >= app.next.get()
            {
                app.refresh(false);
            }
            day::sleep(1000).await;
            ticks += 1;
            if ticks % 10 == 0 {
                app.clock.set(Instant::now());
            }
        }
    });
    day::on_lifecycle(day::Lifecycle::WillTerminate, move || {
        timer.abort();
        app.cancel();
    });
    ui::window(hub)
}

fn status_label(status: Status) -> String {
    match status {
        Status::Success => res::str::status_success(),
        Status::Failure => res::str::status_failure(),
        Status::Running => res::str::status_running(),
        Status::Queued => res::str::status_queued(),
        Status::Cancelled => res::str::status_cancelled(),
        Status::Skipped => res::str::status_skipped(),
        Status::Unknown => res::str::status_unknown(),
    }
    .format()
}

fn light(status: Status) -> String {
    match status {
        Status::Success => res::str::light_success(),
        Status::Failure => res::str::light_failure(),
        Status::Running => res::str::light_running(),
        Status::Queued => res::str::light_queued(),
        Status::Cancelled => res::str::light_cancelled(),
        Status::Skipped => res::str::light_skipped(),
        Status::Unknown => res::str::light_unknown(),
    }
    .format()
}

fn error_text(error: &FetchError) -> String {
    match error {
        FetchError::Network => res::str::network_error().format(),
        FetchError::Http(code) => res::str::http_error(code.to_string()).format(),
        FetchError::Page => res::str::page_error().format(),
        FetchError::Backoff(seconds) => res::str::retry_later(seconds.to_string()).format(),
    }
}

fn show_settings() {
    ui::show_settings();
}

fn open_github_page(url: &str) {
    day::log::info!("Opening GitHub page from status menu: {url}");
    open_url(url);
}

fn status(app: App) -> StatusItem {
    let projects = model::ordered(app.projects.get());
    let mut menu = vec![
        menu_item(res::str::open_hub().format()).action(ui::show_main),
        menu_item(app.summary()).enabled(false),
        menu_item(app.update_text()).enabled(false),
        menu_separator(),
    ];
    let mut group = None;
    for project in &projects {
        if group.is_some_and(|g| g != project.group) {
            menu.push(menu_separator());
        }
        group = Some(project.group);
        let url = project.actions_url();
        let mut runs = vec![
            menu_item(res::str::open_actions().format()).action(move || open_github_page(&url)),
        ];
        if let Some(error) = &project.error {
            runs.push(menu_item(error_text(error)).enabled(false));
        }
        if project.runs.is_empty() {
            runs.push(menu_item(res::str::no_runs().format()).enabled(false));
        }
        for run in project.runs.iter().take(app.history.get()) {
            let url = format!("https://github.com{}", run.path);
            runs.push(
                menu_item(
                    res::str::run_label(
                        run.duration.clone(),
                        status_label(run.status),
                        run.title.clone(),
                    )
                    .format(),
                )
                .action(move || open_github_page(&url)),
            );
        }
        let history = project
            .runs
            .iter()
            .take(app.history.get())
            .rev()
            .map(|r| light(r.status))
            .collect::<String>();
        menu.push(sub_menu(
            res::str::repository_label(history, project.repo.clone()).format(),
            runs,
        ));
    }
    if projects.is_empty() {
        menu.push(menu_item(res::str::empty().format()).enabled(false));
    }
    if let Some(hub) = hub::current() {
        menu.push(menu_separator());
        menu.push(ui::hidden_filter_menu(hub));
    }
    menu.extend([
        menu_separator(),
        menu_item(res::str::refresh().format())
            .id("refresh")
            .enabled(
                app.progress.get().is_none()
                    && !projects.is_empty()
                    && app.backoff.get().is_none_or(|at| at <= Instant::now()),
            )
            .action(move || app.refresh(true)),
        menu_item(res::str::pause().format())
            .id("pause")
            .checked(app.paused.get())
            .action(move || {
                let paused = !app.paused.get();
                app.paused.set(paused);
                day::prefs::set("paused", if paused { "true" } else { "false" });
            }),
        menu_item(res::str::settings().format())
            .id("settings")
            .action(show_settings),
        menu_separator(),
        menu_role(MenuRole::Quit),
    ]);
    let grid = app.grid.get();
    let item = StatusItem::new()
        .tooltip(res::str::grid_tooltip(app.summary()).format())
        .menu(menu);
    if let Some(image) = grid.image {
        let item = item.raster(image);
        if grid.hidden > 0 {
            item.title(res::str::grid_overflow(grid.hidden as i64).format())
        } else {
            item
        }
    } else {
        item.title(res::str::app_title().format())
    }
}

fn settings(app: App) -> impl Piece {
    let repositories = Signal::new(app.repositories.get());
    let interval = Signal::new(app.interval.get().to_string());
    let history = Signal::new(app.history.get().to_string());
    let options = app.grid_options.get();
    let rows = Signal::new(options.rows.to_string());
    let column_width = Signal::new(options.column_width.to_string());
    let max_width = Signal::new(options.max_width.to_string());
    let message = Signal::new(String::new());
    let draft_options = move || {
        let options = grid::Options {
            rows: rows.get().trim().parse().ok()?,
            column_width: column_width.get().trim().parse().ok()?,
            max_width: max_width.get().trim().parse().ok()?,
        };
        options.valid().then_some(options)
    };
    let preview = day::reactive::Memo::new(move || {
        grid::render(
            &model::reconcile(
                model::parse_projects(&repositories.get()).unwrap_or_default(),
                &app.projects.get(),
            ),
            draft_options().unwrap_or(app.grid_options.get()),
        )
    });
    scroll(
        form((
            section((
                label(res::str::monitor_editor_help()),
                editor::repositories(repositories),
            ))
            .title(res::str::repositories()),
            section((
                labeled(res::str::interval(), text_field(interval).id("interval")),
                labeled(
                    res::str::history_count(),
                    text_field(history).id("history-count"),
                ),
            ))
            .title(res::str::refresh_settings()),
            section((
                labeled(res::str::grid_rows(), text_field(rows).id("grid-rows")),
                labeled(
                    res::str::grid_column_width(),
                    text_field(column_width).id("grid-column-width"),
                ),
                labeled(
                    res::str::grid_max_width(),
                    text_field(max_width).id("grid-max-width"),
                ),
                label(res::str::grid_help()),
                canvas(move |draw, size| preview.with(|grid| grid.draw(draw, size)))
                    .height(24.0)
                    .grow_w()
                    .id("grid-preview")
                    .a11y(|a| a.label(res::str::grid_preview())),
                label(move || {
                    let grid = preview.get();
                    if grid.image.is_none() {
                        res::str::grid_preview_empty().format()
                    } else {
                        res::str::grid_preview_count(
                            grid.hidden as i64,
                            model::parse_projects(&repositories.get())
                                .map(|p| p.len().saturating_sub(grid.hidden))
                                .unwrap_or(0) as i64,
                        )
                        .format()
                    }
                })
                .id("grid-preview-count"),
            ))
            .title(res::str::grid_settings()),
            section((
                button(res::str::save()).id("save").action(move || {
                    let projects = match model::parse_projects(&repositories.get()) {
                        Ok(projects) => projects,
                        Err(line) => {
                            message.set(res::str::invalid_repository(line.to_string()).format());
                            return;
                        }
                    };
                    let (Ok(seconds @ 15..=3600), Ok(count @ 1..=10)) = (
                        interval.get().trim().parse::<u64>(),
                        history.get().trim().parse::<usize>(),
                    ) else {
                        message.set(res::str::invalid_settings().format());
                        return;
                    };
                    let Some(options) = draft_options() else {
                        message.set(res::str::invalid_grid().format());
                        return;
                    };
                    if !(day::prefs::set("repositories", &repositories.get())
                        && day::prefs::set("updateInterval", &seconds.to_string())
                        && day::prefs::set("statusBadgeCount", &count.to_string())
                        && day::prefs::set("grid.rows", &options.rows.to_string())
                        && day::prefs::set("grid.columnWidth", &options.column_width.to_string())
                        && day::prefs::set("grid.maxWidth", &options.max_width.to_string()))
                    {
                        message.set(res::str::save_failed().format());
                        return;
                    }
                    app.cancel();
                    app.projects
                        .set(model::reconcile(projects, &app.projects.get()));
                    app.repositories.set(repositories.get());
                    app.interval.set(seconds);
                    app.history.set(count);
                    app.grid_options.set(options);
                    app.next.set(Instant::now());
                    message.set(res::str::saved().format());
                    app.refresh(true);
                }),
                label(move || message.get()).id("settings-message"),
                label(move || app.summary()).id("summary"),
                label(move || app.update_text()).id("update-status"),
            )),
        ))
        .padding(20.0),
    )
}
