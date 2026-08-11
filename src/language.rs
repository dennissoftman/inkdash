use anyhow::{Context, Result};
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};

const NVS_NAMESPACE: &str = "dashboard";
const LANGUAGE_KEY: &str = "ui_lang";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum Language {
    #[default]
    English = 0,
    Russian = 1,
}

pub struct Translations {
    pub indoor: &'static str,
    pub weather: &'static str,
    pub no_data: &'static str,
    pub now: &'static str,
    pub rain: &'static str,
    pub average: &'static str,
    pub low: &'static str,
    pub high: &'static str,
    pub forecast: &'static str,
    pub today: &'static str,
    pub tomorrow: &'static str,
    pub wind: &'static str,
    pub location: &'static str,
    pub no_wifi: &'static str,
    pub humidity_short: &'static str,
    pub rain_short: &'static str,
    pub high_short: &'static str,
    pub low_short: &'static str,
    pub kilometers_per_hour: &'static str,
}

const ENGLISH: Translations = Translations {
    indoor: "INDOOR",
    weather: "WEATHER",
    no_data: "NO DATA",
    now: "NOW",
    rain: "RAIN",
    average: "AVERAGE",
    low: "LOW",
    high: "HIGH",
    forecast: "FORECAST",
    today: "TODAY",
    tomorrow: "TOMORROW",
    wind: "WIND",
    location: "LOCATION",
    no_wifi: "No WiFi",
    humidity_short: "RH",
    rain_short: "R",
    high_short: "H",
    low_short: "L",
    kilometers_per_hour: "KM/H",
};

const RUSSIAN: Translations = Translations {
    indoor: "ВНУТРИ",
    weather: "ПОГОДА",
    no_data: "НЕТ ДАННЫХ",
    now: "СЕЙЧАС",
    rain: "ДОЖДЬ",
    average: "СРЕДНЯЯ",
    low: "МИН",
    high: "МАКС",
    forecast: "ПРОГНОЗ",
    today: "СЕГОДНЯ",
    tomorrow: "ЗАВТРА",
    wind: "ВЕТЕР",
    location: "МЕСТО",
    no_wifi: "Нет WiFi",
    humidity_short: "ВЛ",
    rain_short: "Д",
    high_short: "МАКС",
    low_short: "МИН",
    kilometers_per_hour: "КМ/Ч",
};

impl Language {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Self::English),
            "ru" | "russian" => Some(Self::Russian),
            _ => None,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
        }
    }

    pub const fn translations(self) -> &'static Translations {
        match self {
            Self::English => &ENGLISH,
            Self::Russian => &RUSSIAN,
        }
    }

    pub const fn condition(self, weather_code: u16) -> &'static str {
        match (self, weather_code) {
            (Self::English, 0) => "CLEAR",
            (Self::English, 1..=3) => "CLOUDY",
            (Self::English, 45 | 48) => "FOG",
            (Self::English, 51..=57) => "DRIZZLE",
            (Self::English, 61..=67) => "RAIN",
            (Self::English, 71..=77) => "SNOW",
            (Self::English, 80..=82) => "SHOWERS",
            (Self::English, 85 | 86) => "SNOW SHWR",
            (Self::English, 95..=99) => "STORM",
            (Self::English, _) => "UNKNOWN",
            (Self::Russian, 0) => "ЯСНО",
            (Self::Russian, 1..=3) => "ОБЛАЧНО",
            (Self::Russian, 45 | 48) => "ТУМАН",
            (Self::Russian, 51..=57) => "МОРОСЬ",
            (Self::Russian, 61..=67) => "ДОЖДЬ",
            (Self::Russian, 71..=77) => "СНЕГ",
            (Self::Russian, 80..=82) => "ЛИВНИ",
            (Self::Russian, 85 | 86) => "СНЕГОПАД",
            (Self::Russian, 95..=99) => "ГРОЗА",
            (Self::Russian, _) => "НЕИЗВЕСТНО",
        }
    }

    pub fn short_date(self, weekday: u8, day: u8, month: u8) -> String {
        const ENGLISH_WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const ENGLISH_MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        const RUSSIAN_WEEKDAYS: [&str; 7] = ["Вс", "Пн", "Вт", "Ср", "Чт", "Пт", "Сб"];
        const RUSSIAN_MONTHS: [&str; 12] = [
            "Янв", "Фев", "Мар", "Апр", "Май", "Июн", "Июл", "Авг", "Сен", "Окт", "Ноя", "Дек",
        ];
        let (weekdays, months) = match self {
            Self::English => (&ENGLISH_WEEKDAYS, &ENGLISH_MONTHS),
            Self::Russian => (&RUSSIAN_WEEKDAYS, &RUSSIAN_MONTHS),
        };
        format!(
            "{} {} {}",
            weekdays[weekday as usize],
            day,
            months[month as usize - 1]
        )
    }

    const fn from_stored(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::English),
            1 => Some(Self::Russian),
            _ => None,
        }
    }
}

pub struct LanguageStore {
    storage: EspDefaultNvs,
}

impl LanguageStore {
    pub fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
        let storage =
            EspNvs::new(partition, NVS_NAMESPACE, true).context("opening language settings NVS")?;
        Ok(Self { storage })
    }

    pub fn load(&self) -> Result<Language> {
        let Some(value) = self.storage.get_u8(LANGUAGE_KEY)? else {
            return Ok(Language::default());
        };
        Ok(Language::from_stored(value).unwrap_or_else(|| {
            log::warn!("Ignoring unsupported stored language value {value}");
            Language::default()
        }))
    }

    pub fn save(&self, language: Language) -> Result<()> {
        self.storage
            .set_u8(LANGUAGE_KEY, language as u8)
            .context("saving display language")
    }
}
