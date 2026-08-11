use anyhow::{anyhow, Context, Result};
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sys::{self, EspError};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use std::time::{Duration, Instant};

const NVS_NAMESPACE: &str = "dashboard";
const SSID_KEY: &str = "wifi_ssid";
const PASSWORD_KEY: &str = "wifi_pass";
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug)]
pub struct WifiCredentials {
    pub ssid: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct WifiStatus {
    pub configured_ssid: Option<String>,
    pub connected: bool,
    pub ip: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_dbm: i8,
    pub channel: u8,
    pub auth: String,
}

pub struct WifiManager {
    wifi: BlockingWifi<EspWifi<'static>>,
    storage: EspDefaultNvs,
    last_connected: bool,
    next_reconnect: Instant,
}

impl WifiManager {
    pub fn new(
        modem: Modem<'static>,
        system_loop: EspSystemEventLoop,
        nvs_partition: EspDefaultNvsPartition,
    ) -> Result<Self> {
        let storage = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)
            .context("opening dashboard NVS")?;
        let wifi = BlockingWifi::wrap(
            EspWifi::new(modem, system_loop.clone(), Some(nvs_partition))?,
            system_loop,
        )?;
        Ok(Self {
            wifi,
            storage,
            last_connected: false,
            next_reconnect: Instant::now(),
        })
    }

    pub fn connect_saved(&mut self) -> Result<bool> {
        let Some(credentials) = self.load()? else {
            return Ok(false);
        };
        self.connect(&credentials)?;
        Ok(true)
    }

    pub fn set_credentials(&mut self, credentials: WifiCredentials) -> Result<()> {
        self.storage
            .set_str(SSID_KEY, &credentials.ssid)
            .context("saving Wi-Fi SSID")?;
        self.storage
            .set_str(PASSWORD_KEY, &credentials.password)
            .context("saving Wi-Fi password")?;
        self.connect(&credentials)
    }

    pub fn clear(&mut self) -> Result<()> {
        if self.wifi.is_connected().unwrap_or(false) {
            self.wifi.disconnect().context("disconnecting Wi-Fi")?;
        }
        if self.wifi.is_started().unwrap_or(false) {
            self.wifi.stop().context("stopping Wi-Fi")?;
        }
        self.storage.remove(SSID_KEY)?;
        self.storage.remove(PASSWORD_KEY)?;
        self.last_connected = false;
        self.next_reconnect = Instant::now() + RECONNECT_INTERVAL;
        Ok(())
    }

    /// Observe link changes and periodically request a non-blocking reconnect.
    /// Calling this once per minute adds no meaningful idle wake-up cost because
    /// the dashboard already wakes at RTC minute boundaries.
    pub fn poll(&mut self) -> Result<bool> {
        let connected = self.wifi.is_connected().unwrap_or(false);
        let changed = connected != self.last_connected;
        let now = Instant::now();

        if connected {
            self.last_connected = true;
            self.next_reconnect = now + RECONNECT_INTERVAL;
            return Ok(changed);
        }

        if self.last_connected {
            self.next_reconnect = now;
        }
        self.last_connected = false;

        if now >= self.next_reconnect && self.load()?.is_some() {
            self.next_reconnect = now + RECONNECT_INTERVAL;
            self.wifi
                .wifi_mut()
                .connect()
                .context("requesting background Wi-Fi reconnect")?;
            log::info!("Requested background Wi-Fi reconnect");
        }
        Ok(changed)
    }

    pub fn status(&self) -> Result<WifiStatus> {
        let configured_ssid = self.load()?.map(|credentials| credentials.ssid);
        let connected = self.wifi.is_connected().unwrap_or(false);
        let ip = if connected {
            Some(self.wifi.wifi().sta_netif().get_ip_info()?.ip.to_string())
        } else {
            None
        };
        Ok(WifiStatus {
            configured_ssid,
            connected,
            ip,
        })
    }

    pub fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        if !self.wifi.is_started().unwrap_or(false) {
            self.wifi.start().context("starting Wi-Fi for scan")?;
            EspError::convert(unsafe {
                sys::esp_wifi_set_ps(sys::wifi_ps_type_t_WIFI_PS_MAX_MODEM)
            })
            .context("enabling Wi-Fi maximum modem power saving")?;
        }
        let mut networks: Vec<_> = self
            .wifi
            .scan()
            .context("scanning for Wi-Fi networks")?
            .into_iter()
            .map(|network| WifiNetwork {
                ssid: network.ssid.as_str().to_owned(),
                signal_dbm: network.signal_strength,
                channel: network.channel,
                auth: network
                    .auth_method
                    .map(|method| format!("{method:?}"))
                    .unwrap_or_else(|| "Unknown".to_owned()),
            })
            .collect();
        networks.sort_by_key(|network| std::cmp::Reverse(network.signal_dbm));
        Ok(networks)
    }

    fn connect(&mut self, credentials: &WifiCredentials) -> Result<()> {
        let configuration = Configuration::Client(ClientConfiguration {
            ssid: credentials
                .ssid
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("SSID is longer than 32 bytes"))?,
            password: credentials
                .password
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("password is longer than 64 bytes"))?,
            auth_method: if credentials.password.is_empty() {
                AuthMethod::None
            } else {
                // In station configuration ESP-IDF interprets this as the
                // minimum accepted authentication threshold. WPA2 therefore
                // accepts WPA2-only APs as well as stronger WPA3 variants.
                AuthMethod::WPA2Personal
            },
            ..Default::default()
        });

        if self.wifi.is_connected().unwrap_or(false) {
            self.wifi.disconnect().context("disconnecting old Wi-Fi")?;
        }
        self.wifi
            .set_configuration(&configuration)
            .context("configuring Wi-Fi")?;
        if !self.wifi.is_started().unwrap_or(false) {
            self.wifi.start().context("starting Wi-Fi")?;
        }
        EspError::convert(unsafe { sys::esp_wifi_set_ps(sys::wifi_ps_type_t_WIFI_PS_MAX_MODEM) })
            .context("enabling Wi-Fi maximum modem power saving")?;
        let result = self
            .wifi
            .connect()
            .context("connecting to Wi-Fi")
            .and_then(|()| {
                self.wifi
                    .wait_netif_up()
                    .context("waiting for a Wi-Fi address")
            });
        self.last_connected = self.wifi.is_connected().unwrap_or(false);
        self.next_reconnect = Instant::now() + RECONNECT_INTERVAL;
        result
    }

    fn load(&self) -> Result<Option<WifiCredentials>> {
        let Some(ssid_length) = self.storage.str_len(SSID_KEY)? else {
            return Ok(None);
        };
        let mut ssid_buffer = vec![0_u8; ssid_length];
        let ssid = self
            .storage
            .get_str(SSID_KEY, &mut ssid_buffer)?
            .context("Wi-Fi SSID disappeared from NVS")?
            .to_owned();

        let password = if let Some(password_length) = self.storage.str_len(PASSWORD_KEY)? {
            let mut password_buffer = vec![0_u8; password_length];
            self.storage
                .get_str(PASSWORD_KEY, &mut password_buffer)?
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        };
        Ok(Some(WifiCredentials { ssid, password }))
    }
}
