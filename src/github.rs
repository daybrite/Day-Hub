//! Tokenize public Actions HTML with html5ever (including entities and SVG namespaces).
//! Never execute scripts or reuse a browser's authenticated session.
use crate::model::{FetchError, Run, Status, valid_run_path};
use html5ever::tokenizer::{
    BufferQueue, StartTag, Token, TokenSink, TokenSinkResult, Tokenizer, states::RawKind,
};
use std::cell::RefCell;
use std::time::Instant;

#[derive(Default)]
struct Page {
    found: bool,
    depth: usize,
    row_depth: Option<usize>,
    run: Run,
    runs: Vec<Run>,
    duration_pending: bool,
    duration_span: bool,
    invalid: bool,
}

struct Sink<'a> {
    repo: &'a str,
    page: RefCell<Page>,
}

impl TokenSink for Sink<'_> {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut p = self.page.borrow_mut();
        if let Token::CharacterTokens(text) = &token {
            if p.duration_span {
                p.run.duration.push_str(text);
            }
            return TokenSinkResult::Continue;
        }
        let Token::TagToken(tag) = token else {
            return TokenSinkResult::Continue;
        };
        let start = tag.kind == StartTag;
        let name = tag.name.as_ref();
        if start {
            match name {
                "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
                "style" => return TokenSinkResult::RawData(RawKind::Rawtext),
                "title" | "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
                _ => {}
            }
        }
        let attr = |key: &str| {
            tag.attrs
                .iter()
                .find(|a| a.name.local.as_ref() == key)
                .map(|a| a.value.as_ref())
                .unwrap_or("")
        };
        if name == "div" {
            if start {
                if attr("id") == "partial-actions-workflow-runs" {
                    p.found = true;
                    p.depth = 1;
                } else if p.depth > 0 {
                    p.depth += 1;
                    if p.depth == 2 && attr("id").starts_with("check_suite_") {
                        p.row_depth = Some(p.depth);
                        p.run = Run::default();
                    }
                }
            } else if p.depth > 0 {
                if p.row_depth == Some(p.depth) {
                    let mut run = std::mem::take(&mut p.run);
                    if run.path.is_empty() || run.title.is_empty() || run.started.is_empty() {
                        p.invalid = true;
                    } else {
                        run.duration = run
                            .duration
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        p.runs.push(run);
                    }
                    p.row_depth = None;
                    p.duration_pending = false;
                    p.duration_span = false;
                }
                p.depth -= 1;
            }
        }
        if p.row_depth.is_some() {
            if start
                && name == "a"
                && !attr("aria-label").is_empty()
                && valid_run_path(attr("href"), self.repo)
            {
                p.run.path = attr("href").into();
                // The title is publication-supplied text, not an app-owned translation.
                p.run.title = attr("aria-label").into();
            }
            if start && name == "svg" && !attr("aria-label").is_empty() {
                let label = attr("aria-label");
                if label == "Run duration" {
                    p.duration_pending = true;
                } else if p.run.status == Status::Unknown {
                    p.run.status = Status::from_github(label);
                }
            }
            if start && name == "relative-time" && p.run.started.is_empty() {
                p.run.started = attr("datetime").into();
            }
            if start && name == "span" && p.duration_pending {
                p.duration_pending = false;
                p.duration_span = true;
            } else if !start && name == "span" {
                p.duration_span = false;
            }
        }
        TokenSinkResult::Continue
    }
}

pub fn parse(html: &str, repo: &str) -> Result<Vec<Run>, FetchError> {
    // Developer diagnostics, separate from the app's localized UI messages. Do not log
    // response bodies or workflow titles: repository, counts and timings are sufficient.
    let started = Instant::now();
    day::info!("{repo}: parsing Actions HTML ({} bytes)", html.len());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let tokenizer = Tokenizer::new(
        Sink {
            repo,
            page: RefCell::new(Page::default()),
        },
        Default::default(),
    );
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let page = tokenizer.sink.page.into_inner();
    if !page.found || page.invalid || page.depth != 0 {
        day::warn!(
            "{repo}: parse failed after {} ms (run_container={}, invalid_row={}, unclosed_divs={}); retaining previous results",
            started.elapsed().as_millis(),
            page.found,
            page.invalid,
            page.depth,
        );
        Err(FetchError::Page)
    } else {
        day::info!(
            "{repo}: parsed {} runs in {} ms (latest={:?}, unknown_statuses={})",
            page.runs.len(),
            started.elapsed().as_millis(),
            page.runs.first().map(|run| run.status),
            page.runs
                .iter()
                .filter(|run| run.status == Status::Unknown)
                .count(),
        );
        Ok(page.runs)
    }
}

pub async fn fetch(repo: &str, force: bool) -> Result<Vec<Run>, FetchError> {
    let mut request = day_part_http::Request::get(format!("https://github.com/{repo}/actions"))
        .header("Accept", "text/html")
        .header("Accept-Language", "en")
        .timeout(std::time::Duration::from_secs(30))
        .timeout_total(std::time::Duration::from_secs(45));
    if force {
        request = request.header("Cache-Control", "no-cache");
    }
    let started = Instant::now();
    day::info!("{repo}: GET https://github.com/{repo}/actions (force={force})");
    let response = day_part_http::fetch_future(request)
        .await
        .map_err(|error| {
            day::warn!(
                "{repo}: request failed after {} ms: {error}; retaining previous results",
                started.elapsed().as_millis(),
            );
            FetchError::Network
        })?;
    day::info!(
        "{repo}: HTTP {} received {} bytes in {} ms",
        response.status,
        response.body.len(),
        started.elapsed().as_millis(),
    );
    if matches!(response.status, 403 | 429) {
        let delay = response
            .header("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300)
            .clamp(15, 86400);
        day::warn!(
            "{repo}: HTTP {}; pausing requests for {delay} seconds and retaining previous results",
            response.status,
        );
        return Err(FetchError::Backoff(delay));
    }
    if response.status != 200 {
        day::warn!(
            "{repo}: unexpected HTTP {}; retaining previous results",
            response.status
        );
        return Err(FetchError::Http(response.status));
    }
    parse(&response.text(), repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Synthetic fixture following the public GitHub markup, no bundled application asset.
    fn fixture(status: &str) -> String {
        format!(
            r#"<div id="partial-actions-workflow-runs"><div id="check_suite_1"><div>
<a aria-label="Fixture &amp; entity" href="/fixture/repo/actions/runs/123"><svg xmlns="http://www.w3.org/2000/svg" aria-label="{status}" viewBox="0 0 16 16"></svg></a>
<relative-time datetime="2026-10-07T12:00:00Z"></relative-time>
<span><svg aria-label="Run duration"></svg><span> 1m 2s </span></span>
</div></div></div>"#
        )
    }
    #[test]
    fn html_statuses_entities_and_duration() {
        for (label, status) in [
            ("completed successfully", Status::Success),
            ("failed", Status::Failure),
            ("currently running", Status::Running),
            ("queued", Status::Queued),
            ("waiting", Status::Queued),
            ("cancelled", Status::Cancelled),
            ("skipped", Status::Skipped),
            ("future state", Status::Unknown),
        ] {
            let runs = parse(&fixture(label), "fixture/repo").unwrap();
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].title, "Fixture & entity");
            assert_eq!(runs[0].status, status);
            assert_eq!(runs[0].duration, "1m 2s");
        }
    }
    #[test]
    fn distinguish_empty_from_unreadable_and_reject_foreign_links() {
        assert_eq!(
            parse(
                "<div id='partial-actions-workflow-runs'></div>",
                "fixture/repo"
            ),
            Ok(vec![])
        );
        for html in [
            "<html>Sign in</html>".into(),
            fixture("failed").replace("/fixture/repo/actions/runs/123", "https://evil.example/"),
            fixture("failed").replace("<relative-time", "<other-element"),
            fixture("failed").trim_end_matches("</div>").into(),
        ] {
            assert_eq!(parse(&html, "fixture/repo"), Err(FetchError::Page));
        }
    }
    #[test]
    #[ignore = "optional live-page smoke check; HUB_LIGHTS_HTML points to a downloaded public page"]
    fn downloaded_page() {
        let html = std::fs::read_to_string(std::env::var("HUB_LIGHTS_HTML").unwrap()).unwrap();
        let runs = parse(&html, "daybrite/day").unwrap();
        assert!(!runs.is_empty());
        assert!(runs.iter().all(|r| valid_run_path(&r.path, "daybrite/day")));
        assert!(runs.iter().any(|r| !r.duration.is_empty()));
    }
}
