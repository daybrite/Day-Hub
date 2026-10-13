//! Compact, deterministic status pixels. No files or native handles: the app owns the image.
use crate::model::{Project, Status};
use day::StatusImage;
use day::prelude::*;

const SCALE: u32 = 2;
const HEIGHT: u32 = 18 * SCALE;
const GAP: u32 = 3 * SCALE;
const ERROR: u32 = 0xDE68CC;
// Dark marks contrast with every status fill, independent of the menu bar's appearance.
const RERUN: u32 = 0x111111;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub rows: u32,
    pub column_width: u32,
    pub max_width: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rows: 3,
            column_width: 6,
            max_width: 480,
        }
    }
}

impl Options {
    pub fn valid(self) -> bool {
        (1..=6).contains(&self.rows)
            && (2..=10).contains(&self.column_width)
            && (80..=600).contains(&self.max_width)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Cell {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub image: Option<StatusImage>,
    pub hidden: usize,
    cells: Vec<Cell>,
}

fn color(status: Status) -> u32 {
    match status {
        Status::Success => 0x30C66B,
        Status::Failure => 0xF24B4B,
        Status::Running => 0xFF942E,
        Status::Queued => 0xF3CA38,
        Status::Cancelled | Status::Skipped => 0x92969D,
        Status::Unknown => 0x947354,
    }
}

/// Inset marks leave both the status color and the refresh-error outline intact. Dimensions
/// are raster pixels at SCALE=2. Small cells use a corner mark instead of ambiguous counts.
fn rerun_marks(cells: &mut Vec<Cell>, cell: &Cell, reruns: u32) {
    if reruns == 0 {
        return;
    }
    let (count, tick_width, tick_height) = if cell.width >= 10 && cell.height >= 8 {
        (reruns.min(3), 2, 3)
    } else {
        (1, (cell.width - 2).min(2), (cell.height - 2).min(2))
    };
    let span = count * tick_width + count - 1;
    let start = cell.x + cell.width - 1 - span;
    for tick in 0..count {
        cells.push(Cell {
            x: start + tick * (tick_width + 1),
            y: cell.y + 1,
            width: tick_width,
            height: tick_height,
            color: RERUN,
        });
    }
}

/// Input follows the menu's order: groups in preference order, repositories sorted
/// within each group by the selected order. Latest run is always at the top. A half-point gutter separates cells.
/// Cap width in points, not repository count; the caller shows a localized overflow count.
pub fn render(projects: &[Project], options: Options) -> Grid {
    let options = if options.valid() {
        options
    } else {
        Options::default()
    };
    let column = options.column_width * SCALE;
    let mut cells = Vec::new();
    let mut width = 0;
    let mut group = None;
    let mut shown = 0;
    for project in projects {
        let gap = if group.is_some_and(|g| g != project.group) {
            GAP
        } else {
            0
        };
        if width + gap + column > options.max_width * SCALE {
            break;
        }
        if gap != 0 {
            cells.push(Cell {
                x: width + gap / 2,
                y: 2,
                width: 1,
                height: HEIGHT - 4,
                color: 0x92969D,
            });
        }
        width += gap;
        for row in 0..options.rows {
            let y = row * HEIGHT / options.rows;
            let bottom = (row + 1) * HEIGHT / options.rows;
            let run = project.runs.get(row as usize);
            let cell = Cell {
                x: width,
                y,
                width: column - 1,
                height: bottom - y - 1,
                color: color(run.map(|r| r.status).unwrap_or_default()),
            };
            cells.push(cell.clone());
            if let Some(reruns) = run.and_then(|r| r.reruns()) {
                rerun_marks(&mut cells, &cell, reruns);
            }
        }
        if project.error.is_some() {
            // An error outlines retained history; never turn a stale successful run into a
            // failed workflow. Its reason remains readable in the repository submenu.
            for (x, y, w, h) in [
                (width, 0, column - 1, 1),
                (width, HEIGHT - 2, column - 1, 1),
                (width, 0, 1, HEIGHT - 1),
                (width + column - 2, 0, 1, HEIGHT - 1),
            ] {
                cells.push(Cell {
                    x,
                    y,
                    width: w,
                    height: h,
                    color: ERROR,
                });
            }
        }
        width += column;
        group = Some(project.group);
        shown += 1;
    }
    let image = if width == 0 {
        None
    } else {
        let mut pixels = vec![0; (width * HEIGHT * 4) as usize];
        for cell in &cells {
            let rgba = [
                (cell.color >> 16) as u8,
                (cell.color >> 8) as u8,
                cell.color as u8,
                255,
            ];
            for y in cell.y..cell.y + cell.height {
                for x in cell.x..cell.x + cell.width {
                    let at = ((y * width + x) * 4) as usize;
                    pixels[at..at + 4].copy_from_slice(&rgba);
                }
            }
        }
        Some(
            StatusImage::rgba(width, HEIGHT, f64::from(SCALE), pixels)
                .expect("bounded grid dimensions"),
        )
    };
    Grid {
        image,
        hidden: projects.len() - shown,
        cells,
    }
}

impl Grid {
    /// Preview the very same rectangles used to produce the status image, at native size.
    pub fn draw(&self, draw: &mut Draw, size: Size) {
        let offset_y = ((size.height - 18.0) / 2.0).max(0.0);
        for c in &self.cells {
            draw.fill(
                Shape::Rect(Rect::new(
                    f64::from(c.x) / 2.0,
                    offset_y + f64::from(c.y) / 2.0,
                    f64::from(c.width) / 2.0,
                    f64::from(c.height) / 2.0,
                )),
                Color::hex(c.color),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FetchError, Run};

    fn fixture(group: usize, statuses: &[Status]) -> Project {
        Project {
            repo: "fixture/repo".into(),
            group,
            error: None,
            runs: statuses
                .iter()
                .map(|status| Run {
                    status: *status,
                    ..Default::default()
                })
                .collect(),
        }
    }
    fn pixel(grid: &Grid, x: usize, y: usize) -> &[u8] {
        let image = grid.image.as_ref().unwrap();
        let offset = (y * image.width() as usize + x) * 4;
        &image.pixels()[offset..offset + 4]
    }
    #[test]
    fn newest_at_top_with_transparent_gutters_and_missing_history() {
        let grid = render(
            &[fixture(0, &[Status::Failure, Status::Success])],
            Options::default(),
        );
        assert_eq!(grid.image.as_ref().unwrap().size(), Size::new(6.0, 18.0));
        assert_eq!(pixel(&grid, 1, 1), [242, 75, 75, 255]);
        assert_eq!(pixel(&grid, 1, 13), [48, 198, 107, 255]);
        assert_eq!(pixel(&grid, 1, 25), [148, 115, 84, 255]);
        assert_eq!(pixel(&grid, 11, 1)[3], 0);
        assert_eq!(pixel(&grid, 1, 11)[3], 0);
    }
    #[test]
    fn many_repositories_groups_and_bounded_overflow() {
        let mut projects = vec![fixture(0, &[Status::Success]); 70];
        for p in &mut projects[35..] {
            p.group = 1;
        }
        let grid = render(&projects, Options::default());
        assert_eq!(grid.hidden, 0);
        assert_eq!(grid.image.as_ref().unwrap().size(), Size::new(423.0, 18.0));
        assert_eq!(pixel(&grid, 420, 5)[3], 0); // gap, not another repository
        assert_eq!(pixel(&grid, 423, 5), [146, 150, 157, 255]); // group divider
        let bounded = render(
            &projects,
            Options {
                max_width: 80,
                ..Options::default()
            },
        );
        assert_eq!(bounded.hidden, 57);
        assert!(bounded.image.as_ref().unwrap().size().width <= 80.0);
        assert_eq!(grid, render(&projects, Options::default()));
    }
    #[test]
    fn reruns_use_capped_ticks_and_leave_initial_or_unknown_attempts_unmarked() {
        let mut project = fixture(0, &[Status::Success]);
        let baseline = render(&[project.clone()], Options::default());
        for attempt in [None, Some(0), Some(1)] {
            project.runs[0].attempt = attempt;
            assert_eq!(render(&[project.clone()], Options::default()), baseline);
        }
        for (attempt, count) in [(2, 1), (3, 2), (4, 3), (20, 3), (u32::MAX, 3)] {
            project.runs[0].attempt = Some(attempt);
            let grid = render(&[project.clone()], Options::default());
            assert_eq!(
                grid.cells.iter().filter(|c| c.color == RERUN).count(),
                count
            );
            // Tick ends inside the top-right corner; status fill and gutters survive.
            assert_eq!(pixel(&grid, 8, 1), [17, 17, 17, 255]);
            assert_eq!(pixel(&grid, 8, 4), [48, 198, 107, 255]);
            assert_eq!(pixel(&grid, 10, 1), [48, 198, 107, 255]);
            assert_eq!(pixel(&grid, 11, 1)[3], 0);
            assert_eq!(pixel(&grid, 8, 11)[3], 0);
            assert_eq!(pixel(&grid, 8, 13), [148, 115, 84, 255]);
        }
    }

    #[test]
    fn rerun_marks_fit_every_grid_size_and_preserve_errors_and_status_colors() {
        for status in [
            Status::Success,
            Status::Failure,
            Status::Running,
            Status::Queued,
            Status::Cancelled,
            Status::Skipped,
            Status::Unknown,
        ] {
            for rows in 1..=6 {
                for column_width in 2..=10 {
                    let mut project = fixture(0, &[status; 6]);
                    project.error = Some(FetchError::Network);
                    for run in &mut project.runs {
                        run.attempt = Some(5);
                    }
                    let grid = render(
                        &[project],
                        Options {
                            rows,
                            column_width,
                            ..Options::default()
                        },
                    );
                    let ticks_per_run = if column_width >= 6 && rows <= 4 { 3 } else { 1 };
                    let marks: Vec<_> = grid.cells.iter().filter(|c| c.color == RERUN).collect();
                    assert_eq!(marks.len(), (rows * ticks_per_run) as usize);
                    for mark in marks {
                        assert!(mark.x > 0 && mark.x + mark.width < column_width * SCALE - 1);
                        assert!(mark.y > 0 && mark.y + mark.height < HEIGHT - 1);
                    }
                    for row in 0..rows {
                        let y = row * HEIGHT / rows;
                        let bottom = (row + 1) * HEIGHT / rows;
                        // The bottom interior pixel retains the status even at minimum size.
                        let fill = color(status);
                        assert_eq!(
                            pixel(&grid, 1, (bottom - 3) as usize),
                            [(fill >> 16) as u8, (fill >> 8) as u8, fill as u8, 255]
                        );
                        assert_eq!(
                            pixel(&grid, (column_width * SCALE - 1) as usize, y as usize)[3],
                            0
                        );
                        assert_eq!(pixel(&grid, 1, (bottom - 1) as usize)[3], 0);
                    }
                    assert_eq!(pixel(&grid, 0, 0), [222, 104, 204, 255]);
                    assert_eq!(pixel(&grid, 1, 34), [222, 104, 204, 255]);
                }
            }
        }
    }

    #[test]
    fn errors_preserve_run_colors_and_empty_input_has_no_image() {
        let mut project = fixture(0, &[Status::Success]);
        project.error = Some(FetchError::Network);
        let grid = render(&[project], Options::default());
        assert_eq!(pixel(&grid, 2, 2), [48, 198, 107, 255]);
        assert_eq!(pixel(&grid, 0, 0), [222, 104, 204, 255]);
        assert!(render(&[], Options::default()).image.is_none());
        for rows in 1..=6 {
            let grid = render(
                &[fixture(0, &[Status::Running; 6])],
                Options {
                    rows,
                    ..Options::default()
                },
            );
            assert_eq!(pixel(&grid, 2, 34), [255, 148, 46, 255]);
        }
    }
}
