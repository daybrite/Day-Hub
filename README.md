# Day Hub

A native macOS GitHub workspace with a compact Actions status grid in the menu bar. Renamed
from Day-HubLights; existing monitored repositories, groups, refresh interval and grid preferences
are copied from `dev.daybrite.hublights` into `dev.daybrite.hub` on first use. The old settings
remain intact. The repository list imported from the original HubLights is retained.

## Run

```sh
day patch --local ../day
day launch -p macos-appkit
```

This checkout also patches `day-piece-charts` to the adjacent checkout in `.cargo/config.toml`.
The local Day changes provide native status images and reliable menu actions. Launch opens the
main window and starts the menu monitor. Closing the window leaves monitoring running; **Open
Day Hub** and **Settings** in the status menu reopen it. Quit explicitly to stop the app.

## Sign in and browse

Choose **Sign in with GitHub** to display the device code in Day Hub. It is not sent by email
or SMS. Choose **Copy code and open GitHub**, paste the code into GitHub’s device authorization
form, then approve Day Hub and return to the app. Day Hub stays in front until you explicitly
open GitHub, so the browser cannot hide the code before you see it. Keep Day Hub running while
it completes authorization automatically. The registered OAuth client ID is `Ov23li46VVDMelb0esrd`, with device flow enabled.
No client secret is embedded or required. Passwords, 2FA and passkeys stay on GitHub in the browser.
The `oauth2` crate implements device polling, slow-down responses and expiry. Cancel aborts polling;
a cancelled or superseded attempt cannot publish a session.

Credentials are JSON in the macOS Keychain entry with service `dev.daybrite.hub.github` and account
`github.com`, accessed using `keyring`. Preferences contain no token. Launch restores the credential;
sign-out deletes it and clears private repository/run data from memory. Sign-out does not revoke
GitHub's app grant; **Manage GitHub app access** opens that control. Refresh-token support is included
when the server issues expiring credentials. Token values and OAuth bodies are never logged.

Scopes are `repo`, `read:org`, and `read:user`. GitHub OAuth's `repo` scope includes write permission,
although Day Hub currently only reads data. Organization approval and SAML SSO policies still apply:
Day Hub lists every repository visible to the authorized token, not repositories an organization
withholds from OAuth apps. Being an organization owner does not automatically approve Day Hub.
Use **Manage GitHub app access** to grant access, or the organization’s **Settings → Third-party
Access → OAuth app policy → Review → Grant access**, then **Refresh repositories**. A missing
organization can be omitted from an otherwise successful repository inventory; fetching more pages
cannot bypass an organization’s OAuth restrictions. The inventory follows every REST pagination link, including owned,
collaborator and organization-member repositories, without the Search API's 1,000-result cap.

The sidebar groups repositories by owner, with a separate local Favorites section. Favorites do not
star repositories on GitHub. Search, archived inclusion and sorting (recently updated by default,
name, or stars) customize the list. Sorting and archived inclusion persist; favorites are per account.
Right-click a repository to **Hide repository**, or an owner’s folder row to **Hide group**.
Repository menus also offer the owner-group action. **View → Show hidden repositories** (also
in the status menu) reveals hidden entries with badges; right-click to **Unhide** them. Hidden
repositories, owner groups and the show-hidden toggle persist separately for each GitHub account.
An owner rule includes repositories discovered later; unhiding an owner preserves individually
hidden repositories. Favorites use the same visibility filter. Search and archived filters still
apply while showing hidden entries. These are sidebar visibility preferences; monitored repositories
remain configured independently in Settings.
Overview uses a batched GraphQL query for metadata, issue/PR counts, languages and the latest 100
default-branch commits. Charts show language code size and sampled commit activity. Actions shows
recent runs and durations, loads older pages on demand, and fetches all jobs/steps and artifact
metadata for a selected run. The complete returned run/job/artifact JSON is available for additional
runner and API fields; full logs open on GitHub. Reruns, cancellation, artifact downloads and other
GitHub mutations are not implemented.

## Configure the menu bar

Open **Settings** in the main sidebar or status menu. Add repositories from the searchable account
list or type `owner/repository`. Drag rows to reorder. **Add separator** creates a draggable group
boundary; **Remove** works on repositories and separators. **Save and refresh** applies the draft.
Leaving Settings without saving discards that draft. The plain-text `repositories` preference remains
compatible with `owner/repository` lines and `---` group separators.

Each repository is a grid column with its newest run at the top. Passed runs are green, failed red,
running orange, queued yellow, cancelled/skipped gray and missing/unknown brown. Refresh failures
outline retained results in pink. The menu also exposes textual statuses and direct Actions/run links.
Manual repository order is shared by the grid and menu. Grid size, history and polling interval are
configurable, with a live preview. Overflow repositories remain accessible through the menu.

Signed-in monitoring uses the API, including private repositories. Signed-out monitoring retains the
original public Actions HTML adapter, which reports parsing failures without erasing previous results.
GitHub's HTML is not a versioned API. Original HubLights age fading, change flash, global shortcut and
idle-time pause remain unported.

## Requests and diagnostics

The window and authenticated monitor share one serialized HTTP client. REST requests use ETags;
repository detail views cache for two minutes. Inventory is paginated, overview fields are batched,
and detailed jobs/artifacts load only when selected. The client honors `Retry-After`, rate-limit reset,
remaining quota and `X-Poll-Interval`; secondary limits without timing headers use exponential pauses
starting at 60 seconds. Transport/server failures retry with bounded exponential delays. Authentication
and permission failures do not retry. GitHub API redirects are refused and pagination is restricted to
`https://api.github.com` to keep bearer credentials on the expected origin. No response bodies are
cached on disk by the app's HTTP clients. The unauthenticated HTML adapter retains its own backoff.

Run `day launch -p macos-appkit` without `--detach` for default Info logs showing request paths,
HTTP statuses, sizes, parsing and retries. `DAY_LOG=warn` reduces output. OAuth payloads and tokens
are excluded. Repository names may appear in request URLs in logs.

## OAuth callback website and Desktop research

`website/` customizes Daysite with `/github/oauth`, matching the registered callback
`https://daybrite.github.io/Day-Hub/github/oauth`. Device flow itself does not use a redirect callback.
The static page explains this and offers `day-hub://open` to return to the app. It strips callback
parameters from browser history, sends no referrer and does not exchange or forward authorization
codes. Page text is generated from the same Fluent catalog as the app. The website must be published
separately before that URL is live.

GitHub Desktop was downloaded and studied at commit
`beabf9965389a01ed423b272dfa41416f46ec32a`: `app/src/lib/stores/sign-in-store.ts`,
`app/src/lib/oauth-token.ts`, and `app/src/lib/stores/token-store.ts`. Relevant patterns are external
browser authorization, rejecting stale/cancelled attempts, verifying the user before publishing the
session, and OS credential storage. Desktop's code-exchange flow uses its own application credentials;
Day Hub uses its own public device client and does not copy Desktop's client secret or tokens.

References:

- [GitHub OAuth authorization and device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps)
- [GitHub Desktop source](https://github.com/desktop/desktop/tree/beabf9965389a01ed423b272dfa41416f46ec32a/app/src/lib)
- [GitHub API best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
- [GitHub Actions workflow API](https://docs.github.com/en/rest/actions/workflow-runs)

GitHub's official Octokit clients are JavaScript; this native implementation uses maintained Rust
`oauth2`, `graphql_client`, and `keyring`, with Day's native HTTP transport.

## Validation

CI resolves dependencies from Git, without the machine-local `.cargo/config.toml` patches.
Keep the committed `Cargo.lock` Git-resolved: local patched builds remove the Day and charts
Git sources from it, which breaks CI's `cargo update -p day --precise ...` step. When refreshing
the lockfile, use a clean checkout without local patches and verify `cargo metadata --locked`.

The workflow runs the four synthetic-account scripts below with `DAY_HUB_FIXTURE=1`.
The live OAuth script is intentionally manual because it requires browser authorization.
Cargo features and native host projects cover the workflow's full target matrix. Fixture
checks exercise the UI without validating live OAuth or credential persistence on each OS.
The workflow runs all Day lint checks except `unknown-route`: repository destinations are
created dynamically by `nav.items()`, which the static route scanner cannot enumerate.
Navigation remains covered by the fixture scripts.

```sh
cargo test --offline --lib
cargo clippy --offline --all-targets -- -D warnings
cargo clippy --offline --no-default-features --features appkit --all-targets -- -D warnings
cargo fmt --all -- --check
day launch -p macos-appkit --env DAY_HUB_FIXTURE=1 --script dayscript/hub.yaml
day launch -p macos-appkit --env DAY_HUB_FIXTURE=1 --script dayscript/settings.yaml
day launch -p macos-appkit --env DAY_HUB_FIXTURE=1 --script dayscript/hidden.yaml
day launch -p macos-appkit --env DAY_HUB_FIXTURE=1 --script dayscript/sections.yaml
# Signed out, with network: initiates and cancels a real device grant.
day launch -p macos-appkit --script dayscript/login.yaml
```

Fixture mode uses explicitly synthetic repository/job/artifact data and skips Keychain restoration
and automatic requests. UI scripts keep menu edits in the draft and do not save monitoring preferences.
Unit tests cover pagination, ETag revalidation, foreign-origin rejection, partial GraphQL errors,
auth failures, retry timing, editor ordering/separators, workflow status/duration, HTML parsing and grid
rendering. Full authorization and organization visibility require an interactive GitHub login.
