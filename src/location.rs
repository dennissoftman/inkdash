use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::EspDefaultNvs;
use serde::de::DeserializeOwned;
use serde::Deserialize;

const LATITUDE_KEY: &str = "loc_lat_e6";
const LONGITUDE_KEY: &str = "loc_lon_e6";
const IP_LOCATION_URL: &str = "https://ipwho.is/?fields=success,latitude,longitude,message";
const HTTP_RESPONSE_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub latitude: f32,
    pub longitude: f32,
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
    message: Option<String>,
}

impl LocationProvider for IpLocationProvider {
    fn locate(&mut self) -> Result<Location> {
        let response: IpLocationResponse = get_json(IP_LOCATION_URL)?;
        if !response.success {
            bail!(
                "IP geolocation rejected the request: {}",
                response.message.as_deref().unwrap_or("unknown error")
            );
        }
        let location = Location {
            latitude: response
                .latitude
                .context("IP geolocation response omitted latitude")? as f32,
            longitude: response
                .longitude
                .context("IP geolocation response omitted longitude")?
                as f32,
        };
        validate(location)?;
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
                "Using saved location {:.4}, {:.4}",
                location.latitude,
                location.longitude
            );
            return Ok(location);
        }

        let location = provider.locate().context("detecting location")?;
        self.save(location)?;
        log::info!(
            "Detected and saved location {:.4}, {:.4}",
            location.latitude,
            location.longitude
        );
        Ok(location)
    }

    fn load(&self) -> Result<Option<Location>> {
        let (Some(latitude), Some(longitude)) = (
            self.storage.get_i32(LATITUDE_KEY)?,
            self.storage.get_i32(LONGITUDE_KEY)?,
        ) else {
            return Ok(None);
        };
        let location = Location {
            latitude: latitude as f32 / 1_000_000.0,
            longitude: longitude as f32 / 1_000_000.0,
        };
        if let Err(error) = validate(location) {
            log::warn!("Discarding invalid saved location: {error:#}");
            self.storage.remove(LATITUDE_KEY)?;
            self.storage.remove(LONGITUDE_KEY)?;
            return Ok(None);
        }
        Ok(Some(location))
    }

    fn save(&self, location: Location) -> Result<()> {
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
        Ok(())
    }
}

fn validate(location: Location) -> Result<()> {
    if !location.latitude.is_finite()
        || !location.longitude.is_finite()
        || !(-90.0..=90.0).contains(&location.latitude)
        || !(-180.0..=180.0).contains(&location.longitude)
    {
        return Err(anyhow!("location coordinates are out of range"));
    }
    Ok(())
}

pub(crate) fn get_json<T>(url: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let configuration = Configuration {
        buffer_size: Some(1024),
        timeout: Some(Duration::from_secs(15)),
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
        if body.len() + count > HTTP_RESPONSE_LIMIT {
            bail!("HTTP response exceeded {HTTP_RESPONSE_LIMIT} bytes");
        }
        body.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&body).context("decoding JSON response")
}
