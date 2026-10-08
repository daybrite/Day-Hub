//! Synthetic publication data for visually checking many repositories without HTTP or prefs.
//! Optional first argument: save a native window capture there and exit after one second.
use day::prelude::*;
use dayapp::{
    grid,
    model::{Project, Run, Status},
    res,
};

fn main() {
    day::launch(
        day::WindowOptions {
            locales: Some((res::locales::DEFAULT, res::locales::CATALOG)),
            title_fn: Some(|| res::str::grid_preview().format()),
            size: Size::new(660.0, 360.0),
            ..Default::default()
        },
        || {
            let statuses = [
                Status::Success,
                Status::Success,
                Status::Success,
                Status::Running,
                Status::Failure,
                Status::Queued,
                Status::Cancelled,
                Status::Skipped,
                Status::Unknown,
            ];
            let projects: Vec<_> = (0..70)
                .map(|index| Project {
                    repo: format!("fixture/repository-{index}"),
                    group: index / 24,
                    error: None,
                    runs: (0..6)
                        .map(|row| Run {
                            status: statuses[(index + row * 3) % statuses.len()],
                            ..Default::default()
                        })
                        .collect(),
                })
                .collect();
            let standard = grid::render(&projects, grid::Options::default());
            let compact = grid::render(
                &projects,
                grid::Options {
                    rows: 6,
                    column_width: 3,
                    ..grid::Options::default()
                },
            );
            let bounded = grid::render(
                &projects,
                grid::Options {
                    max_width: 80,
                    ..grid::Options::default()
                },
            );
            let image = standard.image.clone().unwrap();
            status_item("hublights-grid-fixture", move || {
                StatusItem::new()
                    .raster(image.clone())
                    .tooltip(res::str::grid_preview().format())
                    .menu(vec![menu_role(MenuRole::Quit)])
            });
            if let Some(path) = std::env::args().nth(1) {
                day::task(async move {
                    day::sleep(1000).await;
                    let png = day::window_image()
                        .capture()
                        .expect("native fixture capture");
                    std::fs::write(path, png).expect("write fixture capture");
                    day::quit();
                });
            }
            column((
                label(res::str::grid_preview()),
                canvas(move |d, size| standard.draw(d, size))
                    .height(32.0)
                    .grow_w(),
                label(res::str::grid_preview_count(0, 70)),
                canvas(move |d, size| compact.draw(d, size))
                    .height(32.0)
                    .grow_w(),
                label(res::str::grid_preview_count(0, 70)),
                canvas(move |d, size| bounded.draw(d, size))
                    .height(32.0)
                    .grow_w(),
                label(res::str::grid_preview_count(57, 13)),
                label(res::str::grid_help()),
            ))
            .spacing(8.0)
            .padding(20.0)
        },
    );
}
