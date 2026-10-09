//! GitHub's public-client device grant. Passwords stay in the system browser; tokens live
//! only in memory and the OS credential store. Never log OAuth payloads or token values.
use crate::api::Error;
use oauth2::{
    ClientId, DeviceAuthorizationUrl, Scope, StandardDeviceAuthorizationResponse, TokenResponse,
    TokenUrl, basic::BasicClient,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

pub fn now() -> u64 {
    crate::clock::unix_seconds()
}
fn entry() -> Result<keyring::Entry, Error> {
    keyring::Entry::new("dev.daybrite.hub.github", "github.com").map_err(|_| Error::Keychain)
}
pub fn restore() -> Result<Option<Credential>, Error> {
    match entry()?.get_password() {
        Ok(secret) => {
            let token: Credential = serde_json::from_str(&secret).map_err(|_| Error::Keychain)?;
            if token.client_id != crate::hub::CLIENT_ID
                || token.access_token.is_empty()
                || token.access_token.chars().any(char::is_whitespace)
            {
                return Err(Error::Auth);
            }
            Ok(Some(token))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(Error::Keychain),
    }
}
pub fn save(value: &Credential) -> Result<(), Error> {
    entry()?
        .set_password(&serde_json::to_string(value).map_err(|_| Error::Keychain)?)
        .map_err(|_| Error::Keychain)
}
pub fn forget() -> Result<(), Error> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(Error::Keychain),
    }
}

// oauth2 owns the protocol, interval, slow_down and expiration logic. Day owns the transport.
// GitHub returns OAuth errors with HTTP 200; normalize that to OAuth's standard error status.
async fn transport(request: oauth2::HttpRequest) -> Result<oauth2::HttpResponse, std::io::Error> {
    use day_part_http::{Cache, Client, Redirects, Request};
    let mut req = Request::post(request.uri().to_string(), request.body().clone())
        .header("Accept", "application/json")
        .header("User-Agent", "Day-Hub")
        .timeout_total(Duration::from_secs(30));
    for (name, value) in request.headers() {
        req = req.header(name.as_str(), value.to_str().unwrap_or_default());
    }
    let client = Client::builder()
        .redirects(Redirects::Never)
        .cache(Cache::Off)
        .build();
    day::info!("GitHub OAuth request: {}", request.uri().path());
    let response = client
        .fetch_limited_future(req, 128 * 1024)
        .await
        .map_err(|_| std::io::Error::other("OAuth transport failed"))?;
    day::info!(
        "GitHub OAuth response: HTTP {} (payload omitted)",
        response.status
    );
    let is_error = serde_json::from_slice::<serde_json::Value>(&response.body)
        .ok()
        .is_some_and(|v| v.get("error").is_some());
    let mut result =
        oauth2::http::Response::builder().status(if is_error && response.status == 200 {
            400
        } else {
            response.status
        });
    for (name, value) in response.headers {
        result = result.header(name, value);
    }
    result
        .body(response.body)
        .map_err(|_| std::io::Error::other("OAuth response invalid"))
}

#[derive(Clone)]
pub struct Device {
    pub client_id: String,
    pub details: StandardDeviceAuthorizationResponse,
}
pub async fn begin(client_id: &str) -> Result<Device, Error> {
    if client_id.trim().is_empty() {
        return Err(Error::MissingClient);
    }
    let client = BasicClient::new(ClientId::new(client_id.trim().into()))
        .set_device_authorization_url(
            DeviceAuthorizationUrl::new("https://github.com/login/device/code".into()).unwrap(),
        );
    let details = client
        .exchange_device_code()
        .add_scope(Scope::new("repo".into()))
        .add_scope(Scope::new("read:org".into()))
        .add_scope(Scope::new("read:user".into()))
        .request_async(&transport)
        .await
        .map_err(|_| Error::Auth)?;
    day::info!("GitHub OAuth device response parsed; awaiting browser authorization");
    Ok(Device {
        client_id: client_id.trim().into(),
        details,
    })
}
pub async fn finish(device: Device) -> Result<Credential, Error> {
    let client = BasicClient::new(ClientId::new(device.client_id.clone())).set_token_uri(
        TokenUrl::new("https://github.com/login/oauth/access_token".into()).unwrap(),
    );
    let token = client
        .exchange_device_access_token(&device.details)
        .request_async(
            &transport,
            |duration| day::sleep(duration.as_millis().min(u32::MAX as u128) as u32),
            None,
        )
        .await
        .map_err(|_| Error::Auth)?;
    Ok(Credential {
        client_id: device.client_id,
        access_token: token.access_token().secret().clone(),
        refresh_token: token.refresh_token().map(|t| t.secret().clone()),
        expires_at: token.expires_in().map(|d| now() + d.as_secs()),
    })
}
pub async fn refresh(old: &Credential) -> Result<Credential, Error> {
    let refresh = old.refresh_token.as_ref().ok_or(Error::Auth)?;
    let client = BasicClient::new(ClientId::new(old.client_id.clone())).set_token_uri(
        TokenUrl::new("https://github.com/login/oauth/access_token".into()).unwrap(),
    );
    // Refresh tokens may be single-use: never automatically replay this request.
    let token = client
        .exchange_refresh_token(&oauth2::RefreshToken::new(refresh.clone()))
        .request_async(&transport)
        .await
        .map_err(|_| Error::Auth)?;
    Ok(Credential {
        client_id: old.client_id.clone(),
        access_token: token.access_token().secret().clone(),
        refresh_token: Some(token.refresh_token().ok_or(Error::Auth)?.secret().clone()),
        expires_at: token.expires_in().map(|d| now() + d.as_secs()),
    })
}
