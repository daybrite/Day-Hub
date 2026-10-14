//! One serialized GitHub connection, shared by the window and menu monitor. Responses are
//! bounded and conditional REST requests reuse ETags. No credentials are written to logs.
use crate::auth::{self, Credential};
use day_part_http::{Cache, Client as Http, Redirects, Request, Response};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::{Duration, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Network,
    Http(u16),
    Auth,
    Forbidden,
    Sso,
    RateLimited(u64),
    Invalid,
    Keychain,
    MissingClient,
    Preferences,
}
#[derive(Clone)]
struct Cached {
    etag: String,
    value: Value,
    next: Option<String>,
}
struct RefreshFailure {
    error: Error,
    // A rejected credential needs sign-in; temporary failures expire automatically.
    retry_at: Option<u64>,
}
pub struct Client {
    credential: RefCell<Credential>,
    http: Http,
    gate: async_lock::Mutex<()>,
    blocked_until: Cell<u64>,
    refresh_failure: RefCell<Option<RefreshFailure>>,
    poll_after: RefCell<HashMap<String, u64>>,
    cache: RefCell<HashMap<String, Cached>>,
    pub remaining: Cell<Option<u64>>,
}
impl Client {
    pub fn new(credential: Credential) -> Rc<Self> {
        Rc::new(Self {
            credential: RefCell::new(credential),
            http: Http::builder()
                .cache(Cache::Off)
                .redirects(Redirects::Never)
                .build(),
            gate: async_lock::Mutex::new(()),
            blocked_until: Cell::new(0),
            refresh_failure: RefCell::new(None),
            poll_after: RefCell::new(HashMap::new()),
            cache: RefCell::new(HashMap::new()),
            remaining: Cell::new(None),
        })
    }
    // Called with the request gate held. Keep persistence injectable so recovery tests
    // exercise the real OAuth/HTTP pipeline without accessing the user's Keychain.
    async fn refresh_credential<F: std::future::Future<Output = Result<(), Error>>>(
        &self,
        now: impl Fn() -> u64,
        save: impl FnOnce(Credential) -> F,
    ) -> Result<(), Error> {
        let old = self.credential.borrow().clone();
        if !old.expires_at.is_some_and(|at| at <= now() + 60) {
            return Ok(());
        }
        if let Some(failure) = self.refresh_failure.borrow().as_ref()
            && failure.retry_at.is_none_or(|at| now() < at)
        {
            match failure.retry_at {
                Some(at) => day::info!(
                    "GitHub credential refresh deferred: {:?}; retry in {} seconds",
                    failure.error,
                    at.saturating_sub(now())
                ),
                None => day::info!("GitHub credential refresh blocked: sign-in required"),
            }
            return Err(failure.error.clone());
        }
        let fresh = match auth::refresh(&old, &self.http).await {
            Ok(fresh) => fresh,
            Err(error) => {
                let retry_at = (error != Error::Auth).then(|| now().saturating_add(30));
                day::warn!(
                    "GitHub credential refresh failed: {:?}; {}",
                    error,
                    if retry_at.is_some() {
                        "will retry after 30 seconds"
                    } else {
                        "sign-in required"
                    }
                );
                *self.refresh_failure.borrow_mut() = Some(RefreshFailure {
                    error: error.clone(),
                    retry_at,
                });
                return Err(error);
            }
        };
        self.refresh_failure.borrow_mut().take();
        // The old refresh token may already be consumed. Store the rotated credential
        // before awaiting persistence, including if the request is canceled during save.
        *self.credential.borrow_mut() = fresh.clone();
        save(fresh).await
    }
    async fn request(
        &self,
        url: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(Value, Option<String>), Error> {
        if !safe_api_url(url) {
            return Err(Error::Invalid);
        }
        let _guard = self.gate.lock().await;
        for attempt in 0..4u32 {
            let until = self
                .blocked_until
                .get()
                .max(self.poll_after.borrow().get(url).copied().unwrap_or(0));
            while auth::now() < until {
                day::sleep((until.saturating_sub(auth::now()).min(30) * 1000) as u32).await;
            }
            self.refresh_credential(auth::now, |saved| async move {
                day::spawn_blocking(move || auth::save(&saved))
                    .await
                    .map_err(|_| Error::Keychain)?
            })
            .await?;
            let mut req = match &body {
                Some(b) => Request::post(url, b.clone()).header("Content-Type", "application/json"),
                None => Request::get(url),
            }
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Day-Hub")
            .bearer(&self.credential.borrow().access_token)
            .timeout_total(Duration::from_secs(30));
            if body.is_none()
                && let Some(cached) = self.cache.borrow().get(url)
            {
                req = req.header("If-None-Match", &cached.etag);
            }
            day::info!(
                "GitHub API {} {} (attempt {})",
                if body.is_some() { "POST" } else { "GET" },
                url,
                attempt + 1
            );
            let response = match self.http.fetch_limited_future(req, 16 * 1024 * 1024).await {
                Ok(r) => r,
                Err(_) if attempt < 3 => {
                    day::sleep(1000 << attempt).await;
                    continue;
                }
                Err(_) => return Err(Error::Network),
            };
            day::info!(
                "GitHub API HTTP {} ({} bytes)",
                response.status,
                response.body.len()
            );
            self.remaining
                .set(header(&response, "x-ratelimit-remaining").and_then(|s| s.parse().ok()));
            if let Some(seconds) =
                header(&response, "x-poll-interval").and_then(|s| s.parse::<u64>().ok())
            {
                self.poll_after
                    .borrow_mut()
                    .insert(url.into(), auth::now().saturating_add(seconds));
            }
            let value: Value = if response.status == 304 {
                Value::Null
            } else {
                serde_json::from_slice(&response.body).unwrap_or(Value::Null)
            };
            let limited = response.status == 429
                || ((500..=599).contains(&response.status)
                    && header(&response, "retry-after").is_some())
                || (response.status == 403
                    && (header(&response, "retry-after").is_some()
                        || self.remaining.get() == Some(0)
                        || value["message"]
                            .as_str()
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .contains("rate limit")))
                || value["errors"]
                    .as_array()
                    .is_some_and(|errors| errors.iter().any(|e| e["type"] == "RATE_LIMITED"));
            if limited {
                let wait = retry_delay(&response, auth::now(), attempt);
                self.blocked_until.set(auth::now().saturating_add(wait));
                day::warn!("GitHub rate limit: pausing requests for {wait} seconds");
                if attempt == 3 {
                    return Err(Error::RateLimited(wait));
                }
                continue;
            }
            if self.remaining.get() == Some(0)
                && let Some(reset) =
                    header(&response, "x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok())
            {
                self.blocked_until.set(reset.saturating_add(1));
            }
            match response.status {
                304 => {
                    return self
                        .cache
                        .borrow()
                        .get(url)
                        .map(|c| (c.value.clone(), c.next.clone()))
                        .ok_or(Error::Invalid);
                }
                401 => return Err(Error::Auth),
                403 if header(&response, "x-github-sso").is_some() => return Err(Error::Sso),
                403 => return Err(Error::Forbidden),
                500..=599 if attempt < 3 => {
                    day::sleep(1000 << attempt).await;
                    continue;
                }
                200..=299 => {}
                status => return Err(Error::Http(status)),
            }
            if value.is_null() {
                return Err(Error::Invalid);
            }
            let next = next_link(header(&response, "link"));
            if body.is_none()
                && let Some(etag) = header(&response, "etag")
            {
                self.cache.borrow_mut().insert(
                    url.into(),
                    Cached {
                        etag: etag.into(),
                        value: value.clone(),
                        next: next.clone(),
                    },
                );
            }
            day::info!("GitHub API response parsed");
            return Ok((value, next));
        }
        Err(Error::Network)
    }
    pub async fn page(&self, url: &str) -> Result<(Value, Option<String>), Error> {
        self.request(url, None).await
    }
    pub fn retry_in(&self) -> u64 {
        self.blocked_until.get().saturating_sub(auth::now())
    }
    pub async fn get(&self, path: &str) -> Result<Value, Error> {
        self.request(&format!("https://api.github.com{path}"), None)
            .await
            .map(|r| r.0)
    }
    pub async fn pages(&self, path: &str, field: Option<&str>) -> Result<Vec<Value>, Error> {
        let mut next = Some(format!("https://api.github.com{path}"));
        let mut seen = std::collections::HashSet::new();
        let mut rows = Vec::new();
        while let Some(url) = next {
            if !seen.insert(url.clone()) {
                return Err(Error::Invalid);
            }
            let (value, link) = self.request(&url, None).await?;
            let array = field
                .map(|key| &value[key])
                .unwrap_or(&value)
                .as_array()
                .ok_or(Error::Invalid)?;
            rows.extend(array.iter().cloned());
            next = link;
        }
        Ok(rows)
    }
    pub async fn graphql(
        &self,
        query: &'static str,
        operation: &'static str,
        variables: Value,
    ) -> Result<Value, Error> {
        let body = graphql_client::QueryBody {
            query,
            operation_name: operation,
            variables,
        };
        let (value, _) = self
            .request(
                "https://api.github.com/graphql",
                Some(serde_json::to_vec(&body).map_err(|_| Error::Invalid)?),
            )
            .await?;
        let response: graphql_client::Response<Value> =
            serde_json::from_value(value).map_err(|_| Error::Invalid)?;
        if response.errors.is_some_and(|e| !e.is_empty()) {
            return Err(Error::Forbidden);
        }
        response.data.ok_or(Error::Invalid)
    }
    pub async fn repositories(&self) -> Result<(String, Vec<Value>), Error> {
        // REST's authenticated-user inventory includes owner, collaborator and organization
        // membership access. Follow every Link page; no search API's 1,000-result ceiling.
        let viewer = self
            .graphql(
                "query Viewer { viewer { login } rateLimit { remaining resetAt } }",
                "Viewer",
                json!({}),
            )
            .await?;
        let repositories=self.pages("/user/repos?per_page=100&sort=full_name&direction=asc&affiliation=owner,collaborator,organization_member",None).await?;
        Ok((
            viewer["viewer"]["login"]
                .as_str()
                .ok_or(Error::Invalid)?
                .into(),
            repositories,
        ))
    }
}
fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
pub fn safe_api_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("api.github.com")
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none()
    })
}
fn next_link(value: Option<&str>) -> Option<String> {
    value?.split(',').find_map(|part| {
        let (url, rel) = part.trim().split_once(';')?;
        (rel.contains("rel=\"next\"")).then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}
fn retry_delay(response: &Response, now: u64, attempt: u32) -> u64 {
    let retry = header(response, "retry-after")
        .and_then(|v| {
            v.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(v)
                    .ok()
                    .and_then(|d| d.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs().saturating_sub(now))
            })
        })
        .unwrap_or(0);
    let reset = if header(response, "x-ratelimit-remaining") == Some("0") {
        header(response, "x-ratelimit-reset")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v.saturating_sub(now) + 1)
            .unwrap_or(0)
    } else {
        0
    };
    retry.max(reset).max(if retry == 0 && reset == 0 {
        60u64 << attempt
    } else {
        1
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_limits_respect_later_header_and_backoff() {
        let r = Response::new(
            403,
            vec![
                ("Retry-After".into(), "12".into()),
                ("X-RateLimit-Remaining".into(), "0".into()),
                ("X-RateLimit-Reset".into(), "140".into()),
            ],
            vec![],
        );
        assert_eq!(retry_delay(&r, 100, 0), 41);
        assert_eq!(
            retry_delay(&Response::new(429, vec![], vec![]), 100, 2),
            240
        );
    }
    #[test]
    fn pagination_never_sends_token_to_foreign_origin() {
        assert!(safe_api_url("https://api.github.com/user/repos?page=2"));
        for value in [
            "https://api.github.com.evil.test/x",
            "http://api.github.com/x",
            "https://evil.test/x",
            "https://a@api.github.com/x",
        ] {
            assert!(!safe_api_url(value));
        }
        assert_eq!(
            next_link(Some(
                "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=8>; rel=\"last\""
            )),
            Some("https://api.github.com/x?page=2".into())
        );
    }
    use day_part_http::{
        Capabilities, Head, HttpError,
        transport::{Answer, Event, Events, Prepared, QuestionId, Transfer, Transport},
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    // Synthetic server replies, passed through Day's real HTTP client pipeline. No network,
    // real credentials, Keychain access, or wall-clock sleeps are used by these tests.
    #[derive(Clone, Default)]
    struct Scripted {
        responses: Arc<Mutex<VecDeque<ScriptedResponse>>>,
        requests: Arc<Mutex<Vec<Prepared>>>,
    }
    enum ScriptedResponse {
        Response(Box<Response>),
        Failure(HttpError),
        Pending,
    }
    struct Reply {
        events: Events,
        body: Mutex<Option<Vec<u8>>>,
    }
    impl Transfer for Reply {
        fn demand(&self, _: u32) {
            let body = self.body.lock().unwrap().take();
            if let Some(body) = body {
                if !body.is_empty() {
                    (self.events)(Event::Chunk(body));
                }
                (self.events)(Event::End);
            }
        }
        fn answer(&self, _: QuestionId, _: Answer) {}
        fn cancel(&self) {
            self.body.lock().unwrap().take();
        }
    }
    impl Transport for Scripted {
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn start(&self, request: Prepared, events: Events) -> Arc<dyn Transfer> {
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra request");
            let url = request.url.clone();
            self.requests.lock().unwrap().push(request);
            let response = match response {
                ScriptedResponse::Response(response) => response,
                other => {
                    if let ScriptedResponse::Failure(error) = other {
                        events(Event::Failed(error));
                    }
                    return Arc::new(Reply {
                        events,
                        body: Mutex::new(None),
                    });
                }
            };
            let head = Head {
                status: response.status,
                headers: response.headers,
                url,
                expected_length: Some(response.body.len() as u64),
            };
            let reply = Arc::new(Reply {
                events: events.clone(),
                body: Mutex::new(Some(response.body)),
            });
            events(Event::Head(head));
            reply
        }
    }
    fn fixture_client(responses: Vec<Response>) -> (Rc<Client>, Scripted) {
        let scripted = Scripted {
            responses: Arc::new(Mutex::new(
                responses
                    .into_iter()
                    .map(|response| ScriptedResponse::Response(Box::new(response)))
                    .collect(),
            )),
            ..Default::default()
        };
        let mut client = Client::new(Credential {
            client_id: "fixture-client".into(),
            access_token: "synthetic-token".into(),
            refresh_token: None,
            expires_at: None,
        });
        Rc::get_mut(&mut client).unwrap().http = Http::builder()
            .cache(Cache::Off)
            .redirects(Redirects::Never)
            .transport(scripted.clone())
            .build();
        (client, scripted)
    }
    fn ready<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        for _ in 0..100 {
            if let std::task::Poll::Ready(result) = f.as_mut().poll(&mut context) {
                return result;
            }
            std::thread::yield_now();
        }
        panic!("synthetic response unexpectedly pending");
    }
    #[test]
    fn all_pages_and_etag_revalidation_preserve_inventory() {
        let first = Response::new(
            200,
            vec![
                ("etag".into(), "fixture-etag".into()),
                (
                    "link".into(),
                    "<https://api.github.com/user/repos?page=2>; rel=\"next\"".into(),
                ),
            ],
            br#"[{"id":1}]"#.to_vec(),
        );
        let second = Response::new(200, vec![], br#"[{"id":2}]"#.to_vec());
        let (client, wire) = fixture_client(vec![
            first,
            second.clone(),
            Response::new(304, vec![], vec![]),
            second,
        ]);
        assert_eq!(
            ready(client.pages("/user/repos", None)).unwrap(),
            vec![json!({"id":1}), json!({"id":2})]
        );
        assert_eq!(ready(client.pages("/user/repos", None)).unwrap().len(), 2);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(
            requests[2]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("if-none-match") && v == "fixture-etag")
        );
        assert!(requests.iter().all(|r| r.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("authorization") && v == "Bearer synthetic-token"
        })));
    }
    #[test]
    fn untrusted_pagination_and_partial_graphql_are_rejected() {
        let (client, wire) = fixture_client(vec![Response::new(
            200,
            vec![(
                "link".into(),
                "<https://outside.invalid/repos>; rel=\"next\"".into(),
            )],
            b"[]".to_vec(),
        )]);
        assert_eq!(
            ready(client.pages("/user/repos", None)),
            Err(Error::Invalid)
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        let (client, _) = fixture_client(vec![Response::new(200,vec![],br#"{"data":{"viewer":{"login":"fixture"}},"errors":[{"message":"synthetic denied field"}]}"#.to_vec())]);
        assert_eq!(
            ready(client.graphql("query Viewer { viewer { login } }", "Viewer", json!({}))),
            Err(Error::Forbidden)
        );
    }
    #[test]
    fn expired_authorization_is_not_retried() {
        let (client, wire) = fixture_client(vec![Response::new(401, vec![], b"{}".to_vec())]);
        assert_eq!(ready(client.get("/user")), Err(Error::Auth));
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    fn expiring_client(responses: Vec<ScriptedResponse>) -> (Rc<Client>, Scripted) {
        let (client, wire) = fixture_client(vec![]);
        *wire.responses.lock().unwrap() = responses.into();
        let mut credential = client.credential.borrow_mut();
        credential.expires_at = Some(0);
        credential.refresh_token = Some("synthetic-refresh-token".into());
        drop(credential);
        (client, wire)
    }
    fn oauth_response(status: u16, body: &[u8]) -> ScriptedResponse {
        ScriptedResponse::Response(Box::new(Response::new(
            status,
            vec![("Content-Type".into(), "application/json".into())],
            body.to_vec(),
        )))
    }
    fn rotated_token() -> ScriptedResponse {
        oauth_response(200, br#"{"access_token":"synthetic-new-token","refresh_token":"synthetic-new-refresh","token_type":"bearer","expires_in":28800}"#)
    }
    #[test]
    fn offline_token_refresh_recovers_on_the_same_client() {
        let (client, wire) = expiring_client(vec![
            ScriptedResponse::Failure(HttpError::Connect),
            rotated_token(),
            oauth_response(200, br#"{"workflow_runs":[]}"#),
        ]);
        let saved = RefCell::new(None);
        let save = |credential| {
            *saved.borrow_mut() = Some(credential);
            std::future::ready(Ok(()))
        };
        assert_eq!(
            ready(client.refresh_credential(|| 100, save)),
            Err(Error::Network)
        );
        // All other repositories share the cooldown instead of sending another 54
        // refresh requests while offline. The error stays a network error, not Auth.
        for _ in 0..54 {
            assert_eq!(
                ready(client.refresh_credential(|| 129, save)),
                Err(Error::Network)
            );
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert!(saved.borrow().is_none());
        assert_eq!(ready(client.refresh_credential(|| 130, save)), Ok(()));
        assert_eq!(
            saved.borrow().as_ref().unwrap().access_token,
            "synthetic-new-token"
        );
        assert_eq!(
            ready(client.get("/repos/fixture/repo/actions/runs")).unwrap(),
            json!({"workflow_runs":[]})
        );
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[2]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("authorization")
                    && v == "Bearer synthetic-new-token")
        );
    }
    #[test]
    fn server_errors_do_not_permanently_reject_a_refresh_token() {
        for failure in [
            oauth_response(503, b"synthetic unavailable response"),
            oauth_response(200, br#"{"error":"temporarily_unavailable"}"#),
            oauth_response(200, b"synthetic malformed response"),
        ] {
            let (client, wire) = expiring_client(vec![failure, rotated_token()]);
            assert_eq!(
                ready(client.refresh_credential(|| 100, |_| async { Ok(()) })),
                Err(Error::Invalid)
            );
            assert_eq!(
                ready(client.refresh_credential(|| 130, |_| async { Ok(()) })),
                Ok(())
            );
            assert_eq!(wire.requests.lock().unwrap().len(), 2);
        }
    }
    #[test]
    fn rejected_refresh_tokens_require_sign_in_without_replaying() {
        for body in [
            br#"{"error":"bad_refresh_token"}"#.as_slice(),
            br#"{"error":"invalid_grant"}"#.as_slice(),
        ] {
            let (client, wire) = expiring_client(vec![oauth_response(200, body)]);
            for now in [100, 130, 1_000_000] {
                assert_eq!(
                    ready(client.refresh_credential(|| now, |_| async { Ok(()) })),
                    Err(Error::Auth)
                );
            }
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
    #[test]
    fn canceling_refresh_does_not_latch_an_authentication_failure() {
        use std::future::Future;
        let (client, wire) = expiring_client(vec![ScriptedResponse::Pending, rotated_token()]);
        {
            let mut pending =
                std::pin::pin!(client.refresh_credential(|| 100, |_| async { Ok(()) }));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(pending.as_mut().poll(&mut context).is_pending());
        }
        assert_eq!(
            ready(client.refresh_credential(|| 130, |_| async { Ok(()) })),
            Ok(())
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn keychain_failure_or_cancellation_keeps_rotated_credentials() {
        use std::future::Future;
        for cancel in [false, true] {
            let (client, wire) = expiring_client(vec![rotated_token()]);
            if cancel {
                let mut pending =
                    std::pin::pin!(client.refresh_credential(|| 100, |_| std::future::pending()));
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(pending.as_mut().poll(&mut context).is_pending());
            } else {
                assert_eq!(
                    ready(client.refresh_credential(|| 100, |_| async { Err(Error::Keychain) })),
                    Err(Error::Keychain)
                );
            }
            assert_eq!(
                client.credential.borrow().access_token,
                "synthetic-new-token"
            );
            assert_eq!(
                client.credential.borrow().refresh_token.as_deref(),
                Some("synthetic-new-refresh")
            );
            assert_eq!(
                ready(
                    client
                        .refresh_credential(auth::now, |_| async { panic!("unnecessary refresh") })
                ),
                Ok(())
            );
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}
