use anyhow::{bail, Context, Result};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().replace('T', " ");
        let (date, time) = normalized
            .split_once(' ')
            .context("expected YYYY-MM-DD HH:MM:SS")?;
        if time.contains(' ') {
            bail!("expected YYYY-MM-DD HH:MM:SS");
        }

        let mut date_parts = date.split('-');
        let mut time_parts = time.split(':');
        let value = Self {
            year: parse_u16(date_parts.next(), "year")?,
            month: parse_u8(date_parts.next(), "month")?,
            day: parse_u8(date_parts.next(), "day")?,
            hour: parse_u8(time_parts.next(), "hour")?,
            minute: parse_u8(time_parts.next(), "minute")?,
            second: parse_u8(time_parts.next(), "second")?,
        };

        if date_parts.next().is_some() || time_parts.next().is_some() {
            bail!("expected YYYY-MM-DD HH:MM:SS");
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if !(2000..=2099).contains(&self.year) {
            bail!("year must be between 2000 and 2099");
        }
        if !(1..=12).contains(&self.month) {
            bail!("month must be between 1 and 12");
        }
        let max_day = days_in_month(self.year, self.month);
        if self.day == 0 || self.day > max_day {
            bail!("day must be between 1 and {max_day}");
        }
        if self.hour > 23 || self.minute > 59 || self.second > 59 {
            bail!("time must be between 00:00:00 and 23:59:59");
        }
        Ok(())
    }

    /// PCF85063 weekday convention: Sunday = 0, Saturday = 6.
    pub fn weekday(&self) -> u8 {
        const OFFSETS: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut year = self.year;
        if self.month < 3 {
            year -= 1;
        }
        ((year + year / 4 - year / 100
            + year / 400
            + OFFSETS[self.month as usize - 1]
            + self.day as u16)
            % 7) as u8
    }

    pub fn short_date(&self) -> String {
        const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {} {}",
            WEEKDAYS[self.weekday() as usize],
            self.day,
            MONTHS[self.month as usize - 1]
        )
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

fn parse_u8(part: Option<&str>, name: &str) -> Result<u8> {
    part.with_context(|| format!("missing {name}"))?
        .parse::<u8>()
        .with_context(|| format!("invalid {name}"))
}

fn parse_u16(part: Option<&str>, name: &str) -> Result<u16> {
    part.with_context(|| format!("missing {name}"))?
        .parse::<u16>()
        .with_context(|| format!("invalid {name}"))
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 31,
    }
}

fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
