use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection, FollowRedirectsPolicy};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sys::{self, EspError};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config;
use crate::events::{AppEvent, EventSender};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const ENDPOINT_KEY: &str = "ota_endpoint";
const ENDPOINT_LENGTH_LIMIT: usize = 512;
const MANIFEST_RESPONSE_LIMIT: usize = 4096;
const DOWNLOAD_BUFFER_SIZE: usize = 4096;
const OTA_TASK_STACK_SIZE: usize = 20 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
    pub size: usize,
    digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckResult {
    UpToDate,
    Available(UpdateInfo),
}

#[derive(Debug)]
pub enum WorkerEvent {
    Checked(Result<CheckResult, String>),
    Progress {
        version: String,
        downloaded: usize,
        total: usize,
    },
    Finalizing {
        version: String,
    },
    Installed {
        version: String,
    },
    InstallFailed(String),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Screen {
    Hidden,
    Checking,
    Available { version: String, size: usize },
    Downloading { version: String, percent: u8 },
    Finalizing { version: String },
    Restarting { version: String },
    UpToDate,
    Failed { message: String },
}

impl Screen {
    pub const fn is_visible(&self) -> bool {
        !matches!(self, Self::Hidden)
    }

    pub const fn can_accept(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    pub const fn can_start_check(&self) -> bool {
        matches!(self, Self::Hidden | Self::UpToDate | Self::Failed { .. })
    }

    pub const fn can_cancel(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Available { .. } | Self::Downloading { .. }
        )
    }
}

enum Request {
    Check(String),
    Install(UpdateInfo),
}

pub struct Service {
    requests: SyncSender<Request>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointSource {
    Override,
    BuildDefault,
}

impl EndpointSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::BuildDefault => "build-default",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredEndpoint {
    pub url: String,
    pub source: EndpointSource,
}

pub struct EndpointStore {
    storage: EspDefaultNvs,
}

impl EndpointStore {
    pub fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
        let storage = EspNvs::new(partition, config::NVS_NAMESPACE, true)
            .context("opening OTA endpoint settings NVS")?;
        Ok(Self { storage })
    }

    pub fn effective(&self) -> Result<Option<ConfiguredEndpoint>> {
        if let Some(url) = self.load_override()? {
            validate_url(&url, "saved OTA endpoint")?;
            return Ok(Some(ConfiguredEndpoint {
                url,
                source: EndpointSource::Override,
            }));
        }
        let endpoint = option_env!("OTA_ENDPOINT")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|url| ConfiguredEndpoint {
                url: url.to_owned(),
                source: EndpointSource::BuildDefault,
            });
        if let Some(endpoint) = endpoint.as_ref() {
            validate_url(&endpoint.url, "build-time OTA endpoint")?;
        }
        Ok(endpoint)
    }

    pub fn set_override(&self, url: &str) -> Result<()> {
        validate_url(url, "OTA endpoint")?;
        if url.len() > ENDPOINT_LENGTH_LIMIT {
            bail!("OTA endpoint exceeds {ENDPOINT_LENGTH_LIMIT} bytes");
        }
        self.storage
            .set_str(ENDPOINT_KEY, url)
            .context("saving OTA endpoint override")
    }

    pub fn clear_override(&self) -> Result<()> {
        self.storage
            .remove(ENDPOINT_KEY)
            .map(|_| ())
            .context("clearing OTA endpoint override")
    }

    fn load_override(&self) -> Result<Option<String>> {
        let Some(length) = self.storage.str_len(ENDPOINT_KEY)? else {
            return Ok(None);
        };
        if length <= 1 || length > ENDPOINT_LENGTH_LIMIT + 1 {
            bail!("saved OTA endpoint has an invalid length");
        }
        let mut buffer = vec![0_u8; length];
        Ok(self
            .storage
            .get_str(ENDPOINT_KEY, &mut buffer)?
            .map(str::to_owned))
    }
}

impl Service {
    pub fn start(events: EventSender) -> Result<Self> {
        let (request_sender, requests) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        thread::Builder::new()
            .name("ota".into())
            .stack_size(OTA_TASK_STACK_SIZE)
            .spawn(move || worker_loop(requests, events, worker_cancel))
            .context("starting OTA worker")?;
        Ok(Self {
            requests: request_sender,
            cancel,
        })
    }

    pub fn check(&self, endpoint: String) -> bool {
        self.cancel.store(false, Ordering::Release);
        self.try_request(Request::Check(endpoint))
    }

    pub fn install(&self, update: UpdateInfo) -> bool {
        self.cancel.store(false, Ordering::Release);
        self.try_request(Request::Install(update))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    fn try_request(&self, request: Request) -> bool {
        match self.requests.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn worker_loop(requests: mpsc::Receiver<Request>, events: EventSender, cancel: Arc<AtomicBool>) {
    while let Ok(request) = requests.recv() {
        let event = match request {
            Request::Check(endpoint) => {
                let result = check_manifest(&endpoint).map_err(|error| format!("{error:#}"));
                if cancel.load(Ordering::Acquire) {
                    WorkerEvent::Cancelled
                } else {
                    WorkerEvent::Checked(result)
                }
            }
            Request::Install(update) => match install_update(&update, &events, cancel.as_ref()) {
                Ok(()) => WorkerEvent::Installed {
                    version: update.version,
                },
                Err(_) if cancel.load(Ordering::Acquire) => WorkerEvent::Cancelled,
                Err(error) => WorkerEvent::InstallFailed(format!("{error:#}")),
            },
        };
        if events.send(AppEvent::Ota(event)).is_err() {
            return;
        }
    }
}

fn check_manifest(endpoint: &str) -> Result<CheckResult> {
    validate_url(endpoint, "OTA endpoint")?;
    let manifest: UpdateManifest = get_manifest_json(endpoint)?;
    if manifest.schema_version != 1 {
        bail!(
            "OTA manifest schema {} is unsupported",
            manifest.schema_version
        );
    }
    let latest = parse_version(&manifest.version)?;
    let current = Version::parse(CURRENT_VERSION).context("parsing current firmware version")?;
    if latest <= current {
        return Ok(CheckResult::UpToDate);
    }

    validate_url(&manifest.firmware_url, "firmware URL")?;
    if manifest.size == 0 {
        bail!("OTA manifest declares an empty firmware image");
    }
    let digest = parse_digest(&manifest.sha256)?;
    Ok(CheckResult::Available(UpdateInfo {
        version: latest.to_string(),
        download_url: manifest.firmware_url,
        size: manifest.size,
        digest,
    }))
}

fn install_update(update: &UpdateInfo, events: &EventSender, cancel: &AtomicBool) -> Result<()> {
    let partition = unsafe { sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    if partition.is_null() {
        bail!("partition table has no inactive OTA application slot");
    }
    let partition_size = unsafe { (*partition).size as usize };
    if update.size > partition_size {
        bail!(
            "firmware is {} bytes but OTA slot holds only {} bytes",
            update.size,
            partition_size
        );
    }

    let configuration = Configuration {
        buffer_size: Some(DOWNLOAD_BUFFER_SIZE),
        timeout: Some(config::OTA_DOWNLOAD_TIMEOUT),
        follow_redirects_policy: FollowRedirectsPolicy::FollowGetHead,
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut connection =
        EspHttpConnection::new(&configuration).context("creating OTA HTTPS client")?;
    let user_agent = format!("inkdash/{CURRENT_VERSION}");
    connection
        .initiate_request(
            Method::Get,
            &update.download_url,
            &[
                ("accept", "application/octet-stream"),
                ("user-agent", user_agent.as_str()),
            ],
        )
        .context("starting firmware download")?;
    connection
        .initiate_response()
        .context("receiving firmware download")?;
    let status = connection.status();
    if !(200..300).contains(&status) {
        bail!("firmware download returned HTTP status {status}");
    }
    if let Some(length) = connection.header("content-length") {
        let length = length
            .parse::<usize>()
            .context("invalid firmware Content-Length")?;
        if length != update.size {
            bail!(
                "firmware Content-Length changed from {} to {length} bytes",
                update.size
            );
        }
    }
    if cancel.load(Ordering::Acquire) {
        bail!("OTA cancelled");
    }

    let mut session = OtaWriteSession::begin(partition, update.size)?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_usize;
    let mut last_reported_percent = u8::MAX;
    let mut buffer = [0_u8; DOWNLOAD_BUFFER_SIZE];
    loop {
        if cancel.load(Ordering::Acquire) {
            bail!("OTA cancelled");
        }
        let count = connection
            .read(&mut buffer)
            .context("reading firmware download")?;
        if count == 0 {
            break;
        }
        if downloaded + count > update.size {
            bail!("firmware download exceeded advertised size");
        }
        session.write(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        downloaded += count;

        let percent = ((downloaded as u64 * 100) / update.size as u64) as u8;
        let report_percent = (percent / 10) * 10;
        if report_percent != last_reported_percent {
            last_reported_percent = report_percent;
            let _ = events.send(AppEvent::Ota(WorkerEvent::Progress {
                version: update.version.clone(),
                downloaded,
                total: update.size,
            }));
        }
    }

    if downloaded != update.size {
        bail!(
            "firmware download ended at {downloaded} of {} bytes",
            update.size
        );
    }
    let actual_digest: [u8; 32] = hasher.finalize().into();
    if actual_digest != update.digest {
        bail!("firmware SHA-256 does not match the manifest digest");
    }
    if cancel.load(Ordering::Acquire) {
        bail!("OTA cancelled");
    }
    let _ = events.send(AppEvent::Ota(WorkerEvent::Finalizing {
        version: update.version.clone(),
    }));
    session.finish()?;
    if cancel.load(Ordering::Acquire) {
        bail!("OTA cancelled before selecting the new boot image");
    }
    EspError::convert(unsafe { sys::esp_ota_set_boot_partition(partition) })
        .context("selecting newly installed OTA image")?;
    Ok(())
}

struct OtaWriteSession {
    handle: Option<sys::esp_ota_handle_t>,
}

impl OtaWriteSession {
    fn begin(partition: *const sys::esp_partition_t, image_size: usize) -> Result<Self> {
        let mut handle = 0;
        EspError::convert(unsafe { sys::esp_ota_begin(partition, image_size, &mut handle) })
            .context("preparing inactive OTA slot")?;
        Ok(Self {
            handle: Some(handle),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let handle = self.handle.context("OTA writer is already finalized")?;
        EspError::convert(unsafe { sys::esp_ota_write(handle, bytes.as_ptr().cast(), bytes.len()) })
            .context("writing inactive OTA slot")
    }

    fn finish(&mut self) -> Result<()> {
        let handle = self
            .handle
            .take()
            .context("OTA writer is already finalized")?;
        EspError::convert(unsafe { sys::esp_ota_end(handle) })
            .context("validating installed OTA image")
    }
}

impl Drop for OtaWriteSession {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let result = unsafe { sys::esp_ota_abort(handle) };
            if result != sys::ESP_OK {
                log::warn!("Aborting incomplete OTA writer failed with ESP error {result}");
            }
        }
    }
}

pub fn confirm_running_image() -> Result<bool> {
    let partition = unsafe { sys::esp_ota_get_running_partition() };
    if partition.is_null() {
        bail!("could not identify running application partition");
    }
    let mut state = sys::esp_ota_img_states_t_ESP_OTA_IMG_UNDEFINED;
    let result = unsafe { sys::esp_ota_get_state_partition(partition, &mut state) };
    if result == sys::ESP_ERR_NOT_FOUND {
        return Ok(false);
    }
    EspError::convert(result).context("reading running OTA image state")?;
    if state != sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY {
        return Ok(false);
    }
    EspError::convert(unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() })
        .context("confirming running OTA image")?;
    Ok(true)
}

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}

fn get_manifest_json(url: &str) -> Result<UpdateManifest> {
    let configuration = Configuration {
        buffer_size: Some(2048),
        timeout: Some(config::OTA_MANIFEST_TIMEOUT),
        follow_redirects_policy: FollowRedirectsPolicy::FollowGetHead,
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut connection =
        EspHttpConnection::new(&configuration).context("creating manifest HTTPS client")?;
    let user_agent = format!("inkdash/{CURRENT_VERSION}");
    connection
        .initiate_request(
            Method::Get,
            url,
            &[
                ("accept", "application/json"),
                ("user-agent", user_agent.as_str()),
            ],
        )
        .context("requesting OTA manifest")?;
    connection
        .initiate_response()
        .context("receiving OTA manifest")?;
    let status = connection.status();
    if status == 404 {
        bail!("OTA manifest was not found");
    }
    if !(200..300).contains(&status) {
        bail!("OTA manifest request returned HTTP status {status}");
    }

    let mut body = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 1024];
    loop {
        let count = connection
            .read(&mut chunk)
            .context("reading OTA manifest response")?;
        if count == 0 {
            break;
        }
        if body.len() + count > MANIFEST_RESPONSE_LIMIT {
            bail!("OTA manifest exceeded {MANIFEST_RESPONSE_LIMIT} bytes");
        }
        body.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&body).context("decoding OTA manifest")
}

fn validate_url(url: &str, label: &str) -> Result<()> {
    if !url.starts_with("https://")
        || !url.bytes().all(|byte| byte.is_ascii_graphic())
        || url.contains(['"', '\\'])
    {
        bail!("{label} must be a valid HTTPS URL");
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<Version> {
    Version::parse(value.strip_prefix('v').unwrap_or(value))
        .with_context(|| format!("manifest version {value:?} is not semantic versioning"))
}

fn parse_digest(value: &str) -> Result<[u8; 32]> {
    let hex = value.strip_prefix("sha256:").unwrap_or(value);
    if hex.len() != 64 {
        bail!("manifest SHA-256 has the wrong length");
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).context("manifest SHA-256 is not ASCII")?;
        digest[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| anyhow!("manifest SHA-256 is not hexadecimal"))?;
    }
    Ok(digest)
}

#[derive(Deserialize)]
struct UpdateManifest {
    schema_version: u8,
    version: String,
    firmware_url: String,
    size: usize,
    sha256: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_https_endpoint() {
        assert!(super::validate_url("https://example.com/ota.json", "endpoint").is_ok());
        assert!(super::validate_url("http://example.com/ota.json", "endpoint").is_err());
    }

    #[test]
    fn parses_prefixed_version() {
        assert_eq!(
            super::parse_version("v1.2.3").unwrap(),
            semver::Version::new(1, 2, 3)
        );
    }

    #[test]
    fn parses_sha256_digest() {
        let value = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(super::parse_digest(&value).unwrap(), [0xab; 32]);
    }
}
