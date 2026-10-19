use anyhow::{anyhow, bail, Context, Result};
use natpmp::*;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{COOKIE, ORIGIN, REFERER, SET_COOKIE};
use reqwest::{StatusCode, Url};
use std::env;
use std::fmt::Display;
use std::net::Ipv4Addr;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Fixed network configuration.
const NATPMP_GATEWAY_IP: &str = "10.2.0.1";
const QBITTORRENT_HOST: &str = "127.0.0.1";
const DEFAULT_WEBUI_PORT: u16 = 8080;

// Mapping must be renewed before the gateway's 60-second expiration.
const NATPMP_MAPPING_LIFETIME: u32 = 60;
const MAPPING_RENEW_INTERVAL: Duration = Duration::from_secs(45);

// HTTP and retry configuration.
const QBITTORRENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const NATPMP_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const WEBUI_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(5);
const WEBUI_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);
const MAIN_LOOP_MAX_SLEEP: Duration = Duration::from_millis(250);

fn log(level: &str, message: impl Display) {
    let timestamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs();

    eprintln!("[{timestamp}] [{level}] {message}");
}

macro_rules! info {
    ($($arg:tt)*) => {
        log("INFO", format_args!($($arg)*))
    };
}

macro_rules! warn {
    ($($arg:tt)*) => {
        log("WARN", format_args!($($arg)*))
    };
}

macro_rules! error {
    ($($arg:tt)*) => {
        log("ERROR", format_args!($($arg)*))
    };
}

fn main() -> Result<()> {
    info!("Starting NAT-PMP port synchronization service");
    info!("NAT-PMP gateway: {NATPMP_GATEWAY_IP}");

    let mut client = QbittorrentClient::from_env()?;

    let gateway: Ipv4Addr = NATPMP_GATEWAY_IP
    .parse()
    .context("Invalid hardcoded NAT-PMP gateway IP")?;

    let mut natpmp = Natpmp::new_with(gateway)
    .map_err(|e| anyhow!("Failed to create NAT-PMP client: {e:?}"))?;

    retry_forever("NAT-PMP public address query", || {
        query_gateway(&mut natpmp)
    });

    let mut mappings = retry_forever("Initial port mapping", || {
        query_mappings(&mut natpmp, None)
    });

    let mut configured_port: Option<u16> = None;
    let mut webui_retry_delay = WEBUI_RETRY_INITIAL_DELAY;
    let mut next_webui_attempt = Instant::now();

    // Use a fixed 45-second schedule instead of deriving the interval
    // from the lifetime reported by the gateway.
    let mut renew_at = Instant::now() + MAPPING_RENEW_INTERVAL;

    loop {
        let now = Instant::now();
        let port = mappings.tcp.public_port();

        if configured_port != Some(port) && now >= next_webui_attempt {
            match client.set_listen_port(port) {
                Ok(()) => {
                    info!("qBittorrent listening port successfully set to {port}");

                    configured_port = Some(port);
                    webui_retry_delay = WEBUI_RETRY_INITIAL_DELAY;
                    next_webui_attempt = Instant::now();
                }
                Err(e) => {
                    error!("Failed to update qBittorrent listening port: {e:#}");
                    warn!(
                        "Retrying qBittorrent WebUI update in {} seconds",
                        webui_retry_delay.as_secs()
                    );

                    next_webui_attempt = Instant::now() + webui_retry_delay;
                    webui_retry_delay =
                    std::cmp::min(webui_retry_delay * 2, WEBUI_RETRY_MAX_DELAY);
                }
            }
        }

        if Instant::now() >= renew_at {
            let previous_port = mappings.tcp.public_port();

            info!(
                "Renewing TCP/UDP mappings; current external port: {previous_port}"
            );

            mappings = retry_forever("Port mapping renewal", || {
                query_mappings(&mut natpmp, Some(&mappings))
            });

            let new_port = mappings.tcp.public_port();

            if previous_port != new_port {
                warn!("External port changed: {previous_port} -> {new_port}");
            } else {
                info!("Port mapping renewed successfully on port {new_port}");
            }

            // Always verify qBittorrent after a successful NAT-PMP renewal.
            match client.get_listen_port() {
                Ok(actual_port) if actual_port == new_port => {
                    info!(
                        "Post-renewal verification passed: \
qBittorrent listen_port={actual_port}"
                    );

                    configured_port = Some(new_port);
                    webui_retry_delay = WEBUI_RETRY_INITIAL_DELAY;
                }

                Ok(actual_port) => {
                    warn!(
                        "Post-renewal port mismatch: NAT-PMP={new_port}, \
qBittorrent={actual_port}. Forcing an update."
                    );

                    configured_port = None;
                    next_webui_attempt = Instant::now();
                }

                Err(e) => {
                    error!(
                        "Could not verify qBittorrent listening port after renewal: {e:#}"
                    );
                    warn!(
                        "Forcing a qBittorrent listening-port update to {new_port}"
                    );

                    configured_port = None;
                    next_webui_attempt = Instant::now();
                }
            }

            // Schedule from the renewal completion time.
            renew_at = Instant::now() + MAPPING_RENEW_INTERVAL;
            continue;
        }

        let now = Instant::now();
        let mut wait = renew_at.saturating_duration_since(now);

        if configured_port != Some(port) {
            wait = std::cmp::min(
                wait,
                next_webui_attempt.saturating_duration_since(now),
            );
        }

        thread::sleep(std::cmp::min(wait, MAIN_LOOP_MAX_SLEEP));
    }
}

struct QbittorrentClient {
    client: Client,
    base_url: Url,
    username: Option<String>,
    password: Option<String>,
    sid: Option<String>,
    authentication_failed: bool,
}

impl QbittorrentClient {
    fn from_env() -> Result<Self> {
        let webui_port = env::var("WEBUI_PORT")
        .unwrap_or_else(|_| DEFAULT_WEBUI_PORT.to_string())
        .parse::<u16>()
        .context("Invalid WEBUI_PORT; expected a valid TCP port")?;

        let base_url = Url::parse(&format!(
            "http://{QBITTORRENT_HOST}:{webui_port}/"
        ))
        .context("Failed to construct qBittorrent WebUI URL")?;

        let username = optional_env("QBITTORRENT_WEBUI_USERNAME")?;
        let password = optional_env("QBITTORRENT_WEBUI_PASSWORD")?;

        if username.is_some() != password.is_some() {
            bail!(
                "Set both QBITTORRENT_WEBUI_USERNAME and \
QBITTORRENT_WEBUI_PASSWORD, or leave both unset"
            );
        }

        let client = Client::builder()
        .timeout(QBITTORRENT_REQUEST_TIMEOUT)
        .build()
        .context("Failed to create qBittorrent HTTP client")?;

        info!("qBittorrent WebUI endpoint: {base_url}");

        if username.is_some() {
            info!("qBittorrent WebUI authentication is enabled");
        } else {
            warn!("qBittorrent WebUI authentication is disabled");
        }

        Ok(Self {
            client,
            base_url,
            username,
            password,
            sid: None,
            authentication_failed: false,
        })
    }

    fn api_url(&self, path: &str) -> Result<Url> {
        self.base_url
        .join(path)
        .context("Failed to construct qBittorrent API URL")
    }

    fn with_headers(&self, request: RequestBuilder) -> RequestBuilder {
        let request = request
        .header(REFERER, self.base_url.as_str())
        .header(ORIGIN, self.base_url.origin().ascii_serialization());

        match self.sid.as_deref() {
            Some(sid) => request.header(COOKIE, sid),
            None => request,
        }
    }

    fn login(&mut self) -> Result<()> {
        if self.authentication_failed {
            bail!(
                "qBittorrent authentication previously failed; \
verify credentials and restart the service"
            );
        }

        let (Some(username), Some(password)) =
        (self.username.as_deref(), self.password.as_deref())
        else {
            return Ok(());
        };

        info!("Authenticating with qBittorrent WebUI");

        let url = self.api_url("api/v2/auth/login")?;

        let response = self.client
        .post(url)
        .header(REFERER, self.base_url.as_str())
        .header(ORIGIN, self.base_url.origin().ascii_serialization())
        .form(&[("username", username), ("password", password)])
        .send()
        .context("Failed to send qBittorrent login request")?;

        if response.status() == StatusCode::FORBIDDEN {
            self.authentication_failed = true;
            bail!(
                "qBittorrent returned HTTP 403 during login; \
check credentials and WebUI IP ban settings"
            );
        }

        let response = response
        .error_for_status()
        .context("qBittorrent login request failed")?;

        let sid = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie| {
            cookie
            .split(';')
            .next()
            .map(str::trim)
            .filter(|part| part.starts_with("SID="))
            .map(str::to_owned)
        });

        let body = response
        .text()
        .context("Failed to read qBittorrent login response")?;

        if body.trim() != "Ok." {
            self.authentication_failed = true;
            bail!(
                "qBittorrent login rejected; verify WebUI credentials"
            );
        }

        self.sid = Some(sid.ok_or_else(|| {
            self.authentication_failed = true;
            anyhow!("Login succeeded but qBittorrent returned no SID cookie")
        })?);

        info!("qBittorrent authentication successful");

        Ok(())
    }

    fn get_listen_port(&mut self) -> Result<u16> {
        if self.authentication_failed {
            bail!(
                "qBittorrent authentication previously failed; \
verify credentials and restart the service"
            );
        }

        if self.username.is_some() && self.sid.is_none() {
            self.login()?;
        }

        for attempt in 0..2 {
            let url = self.api_url("api/v2/app/preferences")?;

            let response = self
            .with_headers(self.client.get(url))
            .send()
            .context("Failed to request qBittorrent preferences")?;

            if matches!(
                response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) && self.username.is_some()
            && attempt == 0
            {
                warn!("qBittorrent session expired; re-authenticating");

                self.sid = None;
                self.login()?;
                continue;
            }

            let response = response
            .error_for_status()
            .context("qBittorrent rejected the preferences request")?;

            let preferences = response
            .text()
            .context("Failed to read qBittorrent preferences")?;

            let compact: String = preferences
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();

            let marker = r#""listen_port":"#;

            let value = compact
            .split_once(marker)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                anyhow!(
                    "qBittorrent preferences response has no listen_port field"
                )
            })?;

            let digits: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();

            let port = digits
            .parse::<u16>()
            .context("Invalid listen_port in qBittorrent preferences")?;

            return Ok(port);
        }

        bail!("Failed to read qBittorrent listening port after re-authentication")
    }

    fn set_listen_port(&mut self, port: u16) -> Result<()> {
        if self.authentication_failed {
            bail!(
                "qBittorrent authentication previously failed; \
verify credentials and restart the service"
            );
        }

        if self.username.is_some() && self.sid.is_none() {
            self.login()?;
        }

        let json = format!(r#"{{"listen_port":{port}}}"#);

        for attempt in 0..2 {
            let url = self.api_url("api/v2/app/setPreferences")?;

            let request = self.client
            .post(url)
            .form(&[("json", json.as_str())]);

            let response = self
            .with_headers(request)
            .send()
            .context("Failed to send qBittorrent setPreferences request")?;

            if matches!(
                response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) && self.username.is_some()
            && attempt == 0
            {
                warn!("qBittorrent session expired; re-authenticating");

                self.sid = None;
                self.login()?;
                continue;
            }

            response
            .error_for_status()
            .context("qBittorrent rejected the setPreferences request")?;

            self.verify_listen_port(port)?;

            return Ok(());
        }

        bail!("Failed to update listening port after re-authentication")
    }

    fn verify_listen_port(&mut self, expected_port: u16) -> Result<()> {
        let actual_port = self.get_listen_port()?;

        if actual_port != expected_port {
            bail!(
                "qBittorrent listening port mismatch: \
expected {expected_port}, actual {actual_port}"
            );
        }

        info!("Verified qBittorrent listening port: {actual_port}");

        Ok(())
    }
}

fn optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(anyhow!("Failed to read {name}: {error}")),
    }
}

struct MappingPair {
    tcp: MappingResponse,
    udp: MappingResponse,
}

fn query_gateway(n: &mut Natpmp) -> Result<GatewayResponse> {
    info!("Requesting NAT-PMP gateway public address");

    n.send_public_address_request()
    .map_err(|e| anyhow!("Failed to send public address request: {e:?}"))?;

    match read_natpmp_response(n, "public address query")? {
        Response::Gateway(response) => {
            info!(
                "Gateway public address: {}, epoch: {}",
                response.public_address(),
                  response.epoch()
            );

            Ok(response)
        }
        _ => bail!("Expected a NAT-PMP gateway response"),
    }
}

fn query_mappings(
    n: &mut Natpmp,
    previous: Option<&MappingPair>,
) -> Result<MappingPair> {
    let tcp = match previous {
        Some(previous) => query_port(
            n,
            Protocol::TCP,
            previous.tcp.private_port(),
                                     previous.tcp.public_port(),
        )?,
        None => query_port(n, Protocol::TCP, 0, 0)?,
    };

    if tcp.public_port() == 0 || tcp.lifetime().is_zero() {
        bail!("Gateway returned an invalid TCP mapping");
    }

    let udp = query_port(
        n,
        Protocol::UDP,
        tcp.private_port(),
                         tcp.public_port(),
    )?;

    if udp.public_port() == 0 || udp.lifetime().is_zero() {
        bail!("Gateway returned an invalid UDP mapping");
    }

    if udp.public_port() != tcp.public_port() {
        bail!(
            "Gateway returned mismatched ports: TCP={}, UDP={}",
            tcp.public_port(),
              udp.public_port()
        );
    }

    info!(
        "TCP mapping established: internal={}, external={}, lifetime={}s",
        tcp.private_port(),
          tcp.public_port(),
          tcp.lifetime().as_secs()
    );

    info!(
        "UDP mapping established: internal={}, external={}, lifetime={}s",
        udp.private_port(),
          udp.public_port(),
          udp.lifetime().as_secs()
    );

    Ok(MappingPair { tcp, udp })
}

fn query_port(
    n: &mut Natpmp,
    protocol: Protocol,
    internal: u16,
    external: u16,
) -> Result<MappingResponse> {
    let is_tcp = matches!(&protocol, Protocol::TCP);
    let name = if is_tcp { "TCP" } else { "UDP" };

    info!(
        "Requesting {name} mapping: internal={internal}, \
suggested_external={external}, lifetime={NATPMP_MAPPING_LIFETIME}s"
    );

    n.send_port_mapping_request(
        protocol,
        internal,
        external,
        NATPMP_MAPPING_LIFETIME,
    )
    .map_err(|e| anyhow!("Failed to send {name} mapping request: {e:?}"))?;

    match read_natpmp_response(n, &format!("{name} port mapping"))? {
        Response::TCP(response) if is_tcp => Ok(response),
        Response::UDP(response) if !is_tcp => Ok(response),
        _ => bail!("Expected a NAT-PMP {name} mapping response"),
    }
}

fn read_natpmp_response(
    n: &mut Natpmp,
    description: &str,
) -> Result<Response> {
    loop {
        let timeout = n
        .get_natpmp_request_timeout()
        .map_err(|e| {
            anyhow!("Failed to get NAT-PMP timeout for {description}: {e:?}")
        })?;

        if !timeout.is_zero() {
            thread::sleep(timeout);
        }

        match n.read_response_or_retry() {
            Ok(response) => return Ok(response),
            Err(Error::NATPMP_TRYAGAIN) => {
                warn!("NAT-PMP is retrying: {description}");
            }
            Err(error) => {
                return Err(anyhow!(
                    "NAT-PMP {description} failed: {error:?}"
                ));
            }
        }
    }
}

fn retry_forever<T, F>(operation: &str, mut request: F) -> T
where
F: FnMut() -> Result<T>,
{
    let mut delay = Duration::from_secs(1);

    loop {
        match request() {
            Ok(value) => {
                info!("{operation} succeeded");
                return value;
            }
            Err(e) => {
                error!("{operation} failed: {e:#}");
                warn!("Retrying {operation} in {} seconds", delay.as_secs());

                thread::sleep(delay);
                delay = std::cmp::min(delay * 2, NATPMP_RETRY_MAX_DELAY);
            }
        }
    }
}
