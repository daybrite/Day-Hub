//! Ordered menu configuration. Separators are first-class draggable rows, not a text syntax
//! users have to learn; the original text format remains the persistence/interchange format.
use crate::{hub, model, res};
use day::prelude::*;
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: u64,
    repository: Option<String>,
}
fn decode(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                None
            } else if line.chars().all(|c| c == '-') {
                Some(None)
            } else {
                Some(Some(line.to_string()))
            }
        })
        .enumerate()
        .map(|(id, repository)| Entry {
            id: id as u64,
            repository,
        })
        .collect()
}
fn encode(rows: &[Entry]) -> String {
    rows.iter()
        .map(|r| r.repository.as_deref().unwrap_or("---"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn move_row(rows: &mut Vec<Entry>, from: usize, to: usize) {
    if from < rows.len() && to < rows.len() {
        let row = rows.remove(from);
        rows.insert(to, row);
    }
}
pub fn repositories(draft: Signal<String>) -> impl Piece {
    let rows = Signal::new(decode(&draft.get()));
    let next = Signal::new(rows.with(|r| r.len()) as u64);
    let input = Signal::new(String::new());
    let filter = Signal::new(String::new());
    let message = Signal::new(String::new());
    day::reactive::watch(
        move || rows.get(),
        move |rows, _| {
            draft.set(encode(rows));
        },
    );
    let add = move |name: String| {
        let valid = model::parse_projects(&name).ok().filter(|p| p.len() == 1);
        let Some(projects) = valid else {
            message.set(res::str::invalid_repository("1").format());
            return;
        };
        let name = projects[0].repo.clone();
        if rows.with(|rows| {
            rows.iter().any(|r| {
                r.repository
                    .as_ref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&name))
            })
        }) {
            message.set(res::str::repository_duplicate().format());
            return;
        }
        let id = next.get();
        next.set(id + 1);
        rows.update(|r| {
            r.push(Entry {
                id,
                repository: Some(name),
            })
        });
        input.set(String::new());
        message.set(String::new());
    };
    column((
        label(res::str::monitored_repositories()).font(Font::Headline),
        list(items(move || rows.get(), |r: &Entry| r.id), move |slot| {
            row((
                label(move || {
                    slot.get()
                        .repository
                        .unwrap_or_else(|| res::str::separator().format())
                })
                .grow_w(),
                button(res::str::remove_row())
                    .id(format!(
                        "monitor-remove-{}",
                        slot.get()
                            .repository
                            .map(|r| r.replace('/', "~"))
                            .unwrap_or_else(|| format!("separator-{}", slot.get().id))
                    ))
                    .action(move || {
                        let id = slot.get().id;
                        rows.update(|r| r.retain(|v| v.id != id));
                    }),
            ))
            .spacing(10.0)
        })
        .reorderable(true)
        .on_reorder(move |from, to| rows.update(|r| move_row(r, from, to)))
        .id("monitor-rows")
        .height(280.0)
        .grow_w(),
        label(move || res::str::editor_count(rows.with(|r| r.len()) as i64).format())
            .id("monitor-row-count"),
        row((
            text_field(input)
                .placeholder(res::str::repository_address())
                .id("monitor-input")
                .grow_w(),
            button(res::str::add_repository())
                .id("monitor-add")
                .action(move || add(input.get())),
            button(res::str::add_separator())
                .id("monitor-separator")
                .action(move || {
                    let id = next.get();
                    next.set(id + 1);
                    rows.update(|r| {
                        r.push(Entry {
                            id,
                            repository: None,
                        })
                    });
                }),
        ))
        .spacing(10.0),
        label(move || message.get()).id("monitor-message"),
        label(res::str::known_repositories()).font(Font::Headline),
        text_field(filter)
            .placeholder(res::str::find_known_repository())
            .id("known-repository-search"),
        list(
            items(
                move || {
                    let query = filter.get().to_lowercase();
                    let configured = rows.get();
                    let mut repos = hub::current().map(|h| h.repos.get()).unwrap_or_default();
                    repos.retain(|r| {
                        r.full_name.to_lowercase().contains(&query)
                            && !configured.iter().any(|c| {
                                c.repository
                                    .as_ref()
                                    .is_some_and(|n| n.eq_ignore_ascii_case(&r.full_name))
                            })
                    });
                    repos.sort_by(|a, b| a.full_name.cmp(&b.full_name));
                    repos
                },
                |r: &hub::Repository| r.id,
            ),
            move |slot| {
                row((
                    label(move || slot.get().full_name).grow_w(),
                    button(res::str::add_repository())
                        .id(format!("known-add-{}", slot.get().id))
                        .action(move || add(slot.get().full_name)),
                ))
                .spacing(10.0)
            },
        )
        .height(180.0)
        .grow_w()
        .id("known-repositories"),
    ))
    .spacing(10.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repositories_and_separators_move_together_and_roundtrip() {
        let mut rows = decode("fixture/one\n---\nfixture/two");
        move_row(&mut rows, 1, 2);
        assert_eq!(encode(&rows), "fixture/one\nfixture/two\n---");
        move_row(&mut rows, 0, 1);
        assert_eq!(encode(&rows), "fixture/two\nfixture/one\n---");
        rows.remove(2);
        assert_eq!(
            model::parse_projects(&encode(&rows)).unwrap()[0].repo,
            "fixture/two"
        );
    }
}
