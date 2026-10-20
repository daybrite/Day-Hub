//! Day Hub: a native GitHub workspace and compact Actions menu bar monitor.
use crate::clock::Instant;
use day::prelude::*;
use day_piece_settings::prelude::*;
use std::ops::RangeInclusive;
use std::time::Duration;

mod api;
mod auth;
mod clock;
mod editor;
pub mod github;
pub mod grid;
mod hub;
pub mod model;
mod ui;
mod visibility;
use model::{FetchError, Project, RepositoryOrder, Status};
day::resources!();
day::day_start!(options: main_window(), root);

pub fn main_window() -> day::WindowOptions {
    res::window_options()
        .size(1180.0, 800.0)
        .min_size(840.0, 600.0)
}

/// The GitHub repository the About links to.
const SOURCE_URL: &str = "https://github.com/daybrite/Day-Hub";

// Settings (https://daybrite.dev/docs/settings/): typed preferences the monitor reads
// reactively. Every write persists and applies at once; there is no draft and no Save button.
/// Monitored repositories: one `owner/repository` per line, `---` between groups.
pub static REPOSITORIES: Pref<String> = Pref::new("hub.repositories", String::new);
/// Minutes between automatic refreshes.
pub static REFRESH_MINUTES: Pref<i64> = Pref::new("hub.refresh_minutes", || 1);
pub const REFRESH_RANGE: RangeInclusive<i64> = 1..=60;
/// Recent runs listed per repository in the status menu, and lights in its title.
pub static BADGE_HISTORY: Pref<i64> = Pref::new("hub.badge_history", || 3);
pub const BADGE_RANGE: RangeInclusive<i64> = 1..=10;
/// Whether the status item shows the grid image or only the app title.
pub static GRID_VISIBLE: Pref<bool> = Pref::new("hub.grid.visible", || true);
/// The order of repositories inside each grid group.
pub static GRID_ORDER: Pref<RepositoryOrder> =
    Pref::new("hub.grid.order", || RepositoryOrder::Recency);
pub static GRID_ROWS: Pref<i64> = Pref::new("hub.grid.rows", || 3);
pub static GRID_COLUMN_WIDTH: Pref<i64> = Pref::new("hub.grid.column_width", || 6);
pub static GRID_MAX_WIDTH: Pref<i64> = Pref::new("hub.grid.max_width", || 480);
/// Automatic refresh paused from the status menu.
static PAUSED: Pref<bool> = Pref::new("hub.paused", || false);

/// The grid layout from the three steppers (tracked read). `grid::render` falls back to the
/// defaults should a stored value ever fall outside the ranges the steppers enforce.
fn grid_options() -> grid::Options {
    let dim = |v: i64| u32::try_from(v).unwrap_or(0);
    grid::Options {
        rows: dim(GRID_ROWS.get()),
        column_width: dim(GRID_COLUMN_WIDTH.get()),
        max_width: dim(GRID_MAX_WIDTH.get()),
    }
}

fn stepper_range(range: &RangeInclusive<u32>) -> RangeInclusive<i64> {
    i64::from(*range.start())..=i64::from(*range.end())
}

// Monitoring is intentionally app-wide: menu items, the first window and reopened settings
// share one store. Global signals outlive the closing of any settings window.
#[derive(Clone, Copy)]
struct App {
    projects: Signal<Vec<Project>>,
    grid: day::reactive::Memo<grid::Grid>,
    progress: Signal<Option<(usize, usize)>>,
    checked: Signal<Option<Instant>>,
    clock: Signal<Instant>,
    next: Signal<Instant>,
    backoff: Signal<Option<Instant>>,
    task: Signal<Option<day::TaskHandle>>,
}

impl App {
    fn new() -> Self {
        if hub::is_fixture() {
            // Synthetic monitor configuration: UI checks do not depend on the user's list.
            REPOSITORIES.set("fixture/one\n---\nfixture/two".to_string());
        }
        let projects = Signal::global(
            model::parse_projects(&REPOSITORIES.get_untracked()).unwrap_or_default(),
        );
        let root = day::reactive::Scope::root();
        let grid = root.enter(|| {
            day::reactive::Memo::new(move || {
                grid::render(
                    &model::ordered(projects.get(), GRID_ORDER.get()),
                    grid_options(),
                )
            })
        });
        let app = Self {
            projects,
            grid,
            progress: Signal::global(None),
            checked: Signal::global(None),
            clock: Signal::global(Instant::now()),
            next: Signal::global(Instant::now()),
            backoff: Signal::global(None),
            task: Signal::global(None),
        };
        // Settings apply live: an edited list keeps the results it already has and is fetched
        // on the timer's next tick; a new interval moves the next refresh without forcing one.
        root.enter(|| {
            REPOSITORIES.watch(move |text| {
                app.cancel();
                let projects = model::parse_projects(text).unwrap_or_default();
                app.projects
                    .set(model::reconcile(projects, &app.projects.get()));
                app.next.set(Instant::now());
            });
            REFRESH_MINUTES.watch(move |_| {
                let now = Instant::now();
                let due = app
                    .checked
                    .get_untracked()
                    .map_or(now, |at| at + app.interval());
                app.next.set(due.max(now));
            });
        });
        app
    }

    fn interval(self) -> Duration {
        let minutes = REFRESH_MINUTES
            .get_untracked()
            .clamp(*REFRESH_RANGE.start(), *REFRESH_RANGE.end());
        Duration::from_secs(minutes as u64 * 60)
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
        let projects = self.projects.get();
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
            self.next.set(Instant::now() + self.interval());
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
        res::str::summary(res::str::Summary {
            count: projects.len() as i64,
            rate,
        })
        .format()
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
            res::str::updating(res::str::Updating {
                done: done.to_string(),
                total: total.to_string(),
            })
            .format()
        } else if let Some(at) = self.checked.get() {
            res::str::updated(now.saturating_duration_since(at).as_secs().to_string()).format()
        } else {
            res::str::waiting().format()
        }
    }
}

pub fn root() -> impl Piece {
    // html5ever's debug tracing logs every tokenizer transition. Keep normal monitoring
    // quiet while preserving an explicit diagnostic level requested by the developer.
    if std::env::var_os("DAY_LOG").is_none() {
        day::set_log_level(day::log::LevelFilter::Info);
    }
    if hub::is_fixture() {
        // Scripted runs never touch the user's preferences: every setting lives in memory.
        day_piece_settings::install_store(day_piece_settings::MemoryStore::default());
    }
    let app = App::new();
    let hub = hub::Hub::new(app);
    let prefs = settings(app, hub);
    day::set_dock_visible(true);
    {
        let prefs = prefs.clone();
        day::register_new_window(move || ui::window(hub, prefs.clone()));
    }
    day::set_keep_running(day::KeepRunning::Always);
    day::on_open_url(|url| {
        if url == "day-hub://open" {
            ui::show_main();
            true
        } else {
            false
        }
    });
    app_menu(move || {
        vec![
            sub_menu(
                res::str::file_menu(),
                vec![
                    menu_item(res::str::open_hub()).action(ui::show_main),
                    menu_role(MenuRole::CloseWindow),
                    menu_separator(),
                    menu_role(MenuRole::Preferences),
                    menu_role(MenuRole::Quit),
                ],
            )
            .bar_role(MenuBarRole::File),
            sub_menu(res::str::view_menu(), vec![ui::hidden_filter_menu(hub)])
                .bar_role(MenuBarRole::View),
        ]
    });
    status_item("hublights", move || status(app));
    let timer = day::task(async move {
        let mut ticks = 0;
        loop {
            if !hub::is_fixture() && !PAUSED.get_untracked() && Instant::now() >= app.next.get() {
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
    ui::window(hub, prefs)
}

/// Declare and install Settings and About. On desktop this registers the app menu's Settings…
/// and About items and their windows; on the phones and the web the handle's `page()` is the
/// sidebar's Settings section (`ui::window`).
fn settings(app: App, hub: hub::Hub) -> Prefs {
    let order = rows::choice(
        &GRID_ORDER,
        res::str::menu_repository_order(),
        [
            (RepositoryOrder::Recency, res::str::order_recency()),
            (
                RepositoryOrder::Alphabetical,
                res::str::order_alphabetical(),
            ),
        ],
    )
    .id(day::id!(menu_repository_order));
    // The session is live as soon as a credential is restored (`api`); the fixture only
    // sets the login. Either marks the account as signed in.
    let signed_in = move || hub.api.get().is_some() || !hub.login.get().is_empty();
    Preferences::new()
        .about(About::from_manifest().link(day::tr("day-about-source"), SOURCE_URL))
        .group(general_group(res::locales::ALL))
        .group(
            Group::new("refresh", res::str::settings_refresh())
                .icon(Symbol::Refresh)
                .row(
                    rows::stepper(&REFRESH_MINUTES, res::str::refresh_minutes(), REFRESH_RANGE)
                        .id(day::id!(interval)),
                )
                .row(
                    rows::stepper(&BADGE_HISTORY, res::str::history_count(), BADGE_RANGE)
                        .id(day::id!(history_count)),
                ),
        )
        .group(
            Group::new("grid", res::str::settings_grid())
                .icon(res::vectors::prefs_grid)
                .section(
                    Section::new()
                        .row(
                            rows::toggle(&GRID_VISIBLE, res::str::show_grid())
                                .id(day::id!(show_grid)),
                        )
                        .row(order),
                )
                .section(
                    Section::new()
                        .title(res::str::grid_layout())
                        .footer(res::str::grid_help())
                        .row(
                            rows::stepper(
                                &GRID_ROWS,
                                res::str::grid_rows(),
                                stepper_range(&grid::ROWS),
                            )
                            .id(day::id!(grid_rows)),
                        )
                        .row(
                            rows::stepper(
                                &GRID_COLUMN_WIDTH,
                                res::str::grid_column_width(),
                                stepper_range(&grid::COLUMN_WIDTH),
                            )
                            .id(day::id!(grid_column_width)),
                        )
                        .row(
                            rows::stepper(
                                &GRID_MAX_WIDTH,
                                res::str::grid_max_width(),
                                stepper_range(&grid::MAX_WIDTH),
                            )
                            .id(day::id!(grid_max_width)),
                        )
                        .row(rows::custom(move || grid_preview(app))),
                ),
        )
        .group(
            Group::new("repositories", res::str::monitored_repositories())
                .icon(res::vectors::owner_group)
                .subtitle(move || {
                    res::str::monitored_count(app.projects.with(|p| p.len()) as i64).format()
                })
                .custom(editor::repositories),
        )
        .group(
            // "github", not "account": the main window's Account section already owns that
            // route segment, and `settings/account` would land there.
            Group::new("github", res::str::account())
                .icon(res::vectors::prefs_account)
                .row(
                    rows::info(res::str::account_user(), move || {
                        let login = hub.login.get();
                        if login.is_empty() {
                            res::str::not_signed_in().format()
                        } else {
                            login
                        }
                    })
                    .id(day::id!(account_user)),
                )
                .row(
                    rows::action(
                        move || {
                            if signed_in() {
                                res::str::sign_out().format()
                            } else {
                                res::str::sign_in().format()
                            }
                        },
                        move || {
                            if hub.api.get_untracked().is_some()
                                || !hub.login.get_untracked().is_empty()
                            {
                                hub.sign_out();
                            } else {
                                // The device code shows on the account page; bring it forward.
                                ui::show_main();
                                hub.selected.set("account".into());
                                hub.sign_in();
                            }
                        },
                    )
                    .id(day::id!(account_action)),
                ),
        )
        .window_size(640.0, 800.0)
        .install()
}

/// The menu bar grid as it is drawn right now, with the shown/overflow count under it.
fn grid_preview(app: App) -> impl Piece {
    column((
        canvas(move |draw, size| app.grid.with(|grid| grid.draw(draw, size)))
            .height(24.0)
            .grow_w()
            .id(day::id!(grid_preview))
            .a11y(|a| a.label(res::str::grid_preview())),
        label(move || {
            let grid = app.grid.get();
            if grid.image.is_none() {
                res::str::grid_preview_empty().format()
            } else {
                let shown = app.projects.with(|p| p.len().saturating_sub(grid.hidden));
                res::str::grid_preview_count(res::str::GridPreviewCount {
                    hidden: grid.hidden as i64,
                    shown: shown as i64,
                })
                .format()
            }
        })
        .font(Font::Footnote)
        .secondary()
        .id(day::id!(grid_preview_count)),
    ))
    .spacing(Space::S)
    .align(HAlign::Leading)
    .grow_w()
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

fn open_github_page(url: &str) {
    day::log::info!("Opening GitHub page from status menu: {url}");
    open_url(url);
}

// Format on the UI thread when building the menu, not while fetching run metadata.
fn run_menu_label(run: &model::Run) -> String {
    let label = res::str::run_label(res::str::RunLabel {
        duration: run.duration.clone(),
        status: status_label(run.status),
        title: run.title.clone(),
    })
    .format();
    match run.reruns() {
        Some(count) if count > 0 => res::str::run_label_reruns(res::str::RunLabelReruns {
            count: i64::from(count),
            run: label,
        })
        .format(),
        _ => label,
    }
}

fn status(app: App) -> StatusItem {
    let projects = model::ordered(app.projects.get(), GRID_ORDER.get());
    let history = usize::try_from(BADGE_HISTORY.get()).unwrap_or(0);
    let mut menu = vec![
        menu_item(res::str::open_hub()).action(ui::show_main),
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
        let mut runs =
            vec![menu_item(res::str::open_actions()).action(move || open_github_page(&url))];
        if let Some(error) = &project.error {
            runs.push(menu_item(error_text(error)).enabled(false));
        }
        if project.runs.is_empty() {
            runs.push(menu_item(res::str::no_runs()).enabled(false));
        }
        for run in project.runs.iter().take(history) {
            let url = format!("https://github.com{}", run.path);
            runs.push(menu_item(run_menu_label(run)).action(move || open_github_page(&url)));
        }
        let history = project
            .runs
            .iter()
            .take(history)
            .rev()
            .map(|r| light(r.status))
            .collect::<String>();
        menu.push(sub_menu(
            res::str::repository_label(res::str::RepositoryLabel {
                lights: history,
                repository: project.repo.clone(),
            }),
            runs,
        ));
    }
    if projects.is_empty() {
        menu.push(menu_item(res::str::empty()).enabled(false));
    }
    if let Some(hub) = hub::current() {
        menu.push(menu_separator());
        menu.push(ui::hidden_filter_menu(hub));
    }
    menu.extend([
        menu_separator(),
        menu_item(res::str::refresh())
            .id(day::id!(refresh))
            .enabled(
                app.progress.get().is_none()
                    && !projects.is_empty()
                    && app.backoff.get().is_none_or(|at| at <= Instant::now()),
            )
            .action(move || app.refresh(true)),
        menu_item(res::str::pause())
            .id(day::id!(pause))
            .checked(PAUSED.get())
            .action(|| PAUSED.set(!PAUSED.get_untracked())),
        menu_item(res::str::settings())
            .id(day::id!(settings))
            .action(|| {
                day_piece_settings::open();
            }),
        menu_separator(),
        menu_role(MenuRole::Quit),
    ]);
    let item = StatusItem::new()
        .tooltip(res::str::grid_tooltip(app.summary()))
        .menu(menu);
    if !GRID_VISIBLE.get() {
        return item.title(res::str::app_title()).tooltip(app.summary());
    }
    let grid = app.grid.get();
    if let Some(image) = grid.image {
        let item = item.raster(image);
        if grid.hidden > 0 {
            item.title(res::str::grid_overflow(grid.hidden as i64))
        } else {
            item
        }
    } else {
        item.title(res::str::app_title())
    }
}
