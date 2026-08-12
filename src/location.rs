use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::config;
use crate::events::{AppEvent, EventSender};

const LATITUDE_KEY: &str = "loc_lat_e6";
const LONGITUDE_KEY: &str = "loc_lon_e6";
const CITY_KEY: &str = "loc_city";
const COUNTRY_CODE_KEY: &str = "loc_cc";
const TIMEZONE_ID_KEY: &str = "loc_tz";
const UTC_OFFSET_KEY: &str = "loc_utc_off";
const LOCATION_NAME_LIMIT: usize = 96;
const TIMEZONE_ID_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub latitude: f32,
    pub longitude: f32,
    pub city: String,
    pub country_code: String,
    pub timezone_id: String,
    pub utc_offset_seconds: i32,
}

/// A deliberately small seam for adding a BSSID or another location provider.
pub trait LocationProvider {
    fn locate(&mut self) -> Result<Location>;
}

pub struct IpLocationProvider;

#[derive(Deserialize)]
struct IpLocationResponse {
    success: bool,
    latitude: Option<f64>,
    longitude: Option<f64>,
    city: Option<String>,
    country_code: Option<String>,
    timezone: Option<IpTimezoneResponse>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct IpTimezoneResponse {
    id: Option<String>,
    offset: Option<i32>,
}

impl LocationProvider for IpLocationProvider {
    fn locate(&mut self) -> Result<Location> {
        let separator = if config::IP_LOCATION_ENDPOINT.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{separator}fields=success,latitude,longitude,city,country_code,timezone.id,timezone.offset,message",
            config::IP_LOCATION_ENDPOINT
        );
        let response: IpLocationResponse = get_json(&url)?;
        if !response.success {
            bail!(
                "IP geolocation rejected the request: {}",
                response.message.as_deref().unwrap_or("unknown error")
            );
        }
        let timezone = response
            .timezone
            .context("IP geolocation response omitted timezone")?;
        let location = Location {
            latitude: response
                .latitude
                .context("IP geolocation response omitted latitude")? as f32,
            longitude: response
                .longitude
                .context("IP geolocation response omitted longitude")?
                as f32,
            city: response
                .city
                .context("IP geolocation response omitted city")?
                .trim()
                .to_owned(),
            country_code: response
                .country_code
                .context("IP geolocation response omitted country code")?
                .trim()
                .to_ascii_uppercase(),
            timezone_id: timezone
                .id
                .context("IP geolocation response omitted timezone ID")?
                .trim()
                .to_owned(),
            utc_offset_seconds: timezone
                .offset
                .context("IP geolocation response omitted UTC offset")?,
        };
        validate(&location)?;
        Ok(location)
    }
}

pub struct LocationStore {
    storage: EspDefaultNvs,
}

impl LocationStore {
    pub fn new(storage: EspDefaultNvs) -> Self {
        Self { storage }
    }

    pub fn get_location<P>(&self, provider: &mut P) -> Result<Location>
    where
        P: LocationProvider,
    {
        if let Some(location) = self.load()? {
            log::info!(
                "Using saved location {}, {}",
                location.city,
                location.country_code
            );
            return Ok(location);
        }

        let location = provider.locate().context("detecting location")?;
        self.save(&location)?;
        log::info!(
            "Detected and saved location {}, {}",
            location.city,
            location.country_code
        );
        Ok(location)
    }

    pub fn refresh_location<P>(&self, provider: &mut P) -> Result<Location>
    where
        P: LocationProvider,
    {
        let location = provider.locate().context("refreshing location")?;
        self.save(&location)?;
        log::info!(
            "Refreshed location {}, {} ({}, UTC offset {:+})",
            location.city,
            location.country_code,
            location.timezone_id,
            location.utc_offset_seconds,
        );
        Ok(location)
    }

    fn load(&self) -> Result<Option<Location>> {
        let (
            Some(latitude),
            Some(longitude),
            Some(city),
            Some(country_code),
            Some(timezone_id),
            Some(utc_offset_seconds),
        ) = (
            self.storage.get_i32(LATITUDE_KEY)?,
            self.storage.get_i32(LONGITUDE_KEY)?,
            self.load_name(CITY_KEY, LOCATION_NAME_LIMIT)?,
            self.load_name(COUNTRY_CODE_KEY, 3)?,
            self.load_name(TIMEZONE_ID_KEY, TIMEZONE_ID_LIMIT)?,
            self.storage.get_i32(UTC_OFFSET_KEY)?,
        )
        else {
            return Ok(None);
        };
        let location = Location {
            latitude: latitude as f32 / 1_000_000.0,
            longitude: longitude as f32 / 1_000_000.0,
            city,
            country_code,
            timezone_id,
            utc_offset_seconds,
        };
        if let Err(error) = validate(&location) {
            log::warn!("Discarding invalid saved location: {error:#}");
            self.storage.remove(LATITUDE_KEY)?;
            self.storage.remove(LONGITUDE_KEY)?;
            self.storage.remove(CITY_KEY)?;
            self.storage.remove(COUNTRY_CODE_KEY)?;
            self.storage.remove(TIMEZONE_ID_KEY)?;
            self.storage.remove(UTC_OFFSET_KEY)?;
            return Ok(None);
        }
        Ok(Some(location))
    }

    fn save(&self, location: &Location) -> Result<()> {
        validate(location)?;
        self.storage
            .set_i32(
                LATITUDE_KEY,
                (location.latitude * 1_000_000.0).round() as i32,
            )
            .context("saving location latitude")?;
        self.storage
            .set_i32(
                LONGITUDE_KEY,
                (location.longitude * 1_000_000.0).round() as i32,
            )
            .context("saving location longitude")?;
        self.storage
            .set_str(CITY_KEY, &location.city)
            .context("saving location city")?;
        self.storage
            .set_str(COUNTRY_CODE_KEY, &location.country_code)
            .context("saving location country code")?;
        self.storage
            .set_str(TIMEZONE_ID_KEY, &location.timezone_id)
            .context("saving location timezone")?;
        self.storage
            .set_i32(UTC_OFFSET_KEY, location.utc_offset_seconds)
            .context("saving location UTC offset")?;
        Ok(())
    }

    fn load_name(&self, key: &str, limit: usize) -> Result<Option<String>> {
        let Some(length) = self.storage.str_len(key)? else {
            return Ok(None);
        };
        if length <= 1 || length > limit + 1 {
            log::warn!("Discarding invalid saved location name in {key}");
            self.storage.remove(key)?;
            return Ok(None);
        }
        let mut buffer = vec![0_u8; length];
        Ok(self.storage.get_str(key, &mut buffer)?.map(str::to_owned))
    }
}

fn validate(location: &Location) -> Result<()> {
    if !location.latitude.is_finite()
        || !location.longitude.is_finite()
        || !(-90.0..=90.0).contains(&location.latitude)
        || !(-180.0..=180.0).contains(&location.longitude)
    {
        return Err(anyhow!("location coordinates are out of range"));
    }
    if location.city.trim().is_empty()
        || location.city.len() > LOCATION_NAME_LIMIT
        || location.city.contains('\0')
        || !(2..=3).contains(&location.country_code.len())
        || !location
            .country_code
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic())
        || location.timezone_id.trim().is_empty()
        || location.timezone_id.len() > TIMEZONE_ID_LIMIT
        || location.timezone_id.contains('\0')
        || !(-18 * 60 * 60..=18 * 60 * 60).contains(&location.utc_offset_seconds)
    {
        return Err(anyhow!(
            "location city, country code, timezone, or UTC offset is invalid"
        ));
    }
    Ok(())
}

pub(crate) fn get_json<T>(url: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let configuration = Configuration {
        buffer_size: Some(1024),
        timeout: Some(config::HTTP_TIMEOUT),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut connection = EspHttpConnection::new(&configuration).context("creating HTTPS client")?;
    connection
        .initiate_request(Method::Get, url, &[("accept", "application/json")])
        .context("starting HTTPS request")?;
    connection
        .initiate_response()
        .context("receiving HTTPS response")?;
    let status = connection.status();
    if !(200..300).contains(&status) {
        bail!("HTTP request returned status {status}");
    }

    let mut body = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 512];
    loop {
        let count = connection.read(&mut chunk).context("reading HTTP body")?;
        if count == 0 {
            break;
        }
        if body.len() + count > config::HTTP_RESPONSE_LIMIT {
            bail!(
                "HTTP response exceeded {} bytes",
                config::HTTP_RESPONSE_LIMIT
            );
        }
        body.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&body).context("decoding JSON response")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Request {
    Cached,
    Refresh,
}

pub type WorkerUpdate = Result<Arc<Location>, String>;

pub struct LocationService {
    requests: SyncSender<Request>,
    latest: Option<Arc<Location>>,
    in_flight: Option<Request>,
}

impl LocationService {
    pub fn start(nvs_partition: EspDefaultNvsPartition, events: EventSender) -> Result<Self> {
        // One slot, like the weather worker: a lookup already on its way answers
        // for any caller waiting on one, and an unbounded queue would turn a
        // spell of network trouble into a backlog of identical HTTPS requests.
        let (request_sender, request_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("location".into())
            .stack_size(config::BACKGROUND_TASK_STACK_SIZE)
            .spawn(move || {
                let storage = match EspNvs::new(nvs_partition, config::NVS_NAMESPACE, true) {
                    Ok(storage) => storage,
                    Err(error) => {
                        let _ = events.send(AppEvent::LocationCompleted(Err(format!(
                            "opening location NVS: {error}"
                        ))));
                        return;
                    }
                };
                let location_store = LocationStore::new(storage);
                let mut provider = IpLocationProvider;
                while let Ok(request) = request_receiver.recv() {
                    let update = match request {
                        Request::Cached => location_store.get_location(&mut provider),
                        Request::Refresh => location_store.refresh_location(&mut provider),
                    }
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                    if events.send(AppEvent::LocationCompleted(update)).is_err() {
                        break;
                    }
                }
            })
            .context("starting location worker")?;

        Ok(Self {
            requests: request_sender,
            latest: None,
            in_flight: None,
        })
    }

    pub fn request(&mut self) -> bool {
        self.send(Request::Cached)
    }

    pub fn refresh(&mut self) -> bool {
        self.send(Request::Refresh)
    }

    /// Dispatches unless an equivalent lookup is already running. Returns whether
    /// a `LocationCompleted` event is now expected, so a caller that gets `false`
    /// knows to arm its own retry instead of waiting for one.
    fn send(&mut self, request: Request) -> bool {
        let answered_already = match (self.in_flight, request) {
            (None, _) => false,
            (Some(Request::Refresh), _) => true,
            (Some(Request::Cached), Request::Cached) => true,
            // A refresh is what tracks timezone and daylight-saving changes, so
            // it queues behind a cached lookup rather than being dropped.
            (Some(Request::Cached), Request::Refresh) => false,
        };
        if answered_already {
            return true;
        }
        match self.requests.try_send(request) {
            Ok(()) => {
                self.in_flight = Some(request);
                true
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn complete(&mut self, update: WorkerUpdate) {
        self.in_flight = None;
        match update {
            Ok(location) => {
                self.latest = Some(location);
            }
            Err(error) => {
                log::warn!("Location update failed; retaining last location: {error}");
            }
        }
    }

    pub fn latest(&self) -> Option<Arc<Location>> {
        self.latest.clone()
    }
}
