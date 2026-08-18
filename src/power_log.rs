//! Diagnostic ring buffer of battery-node samples.
//!
//! External power is reported from ESP-IDF's USB SOF monitor, which only a USB
//! host drives; a wall adapter supplies VBUS and never sends a SOF packet. The
//! evidence for a charge therefore has to be collected from the battery node
//! while the console is unplugged, so samples are buffered in RAM and dumped
//! once the board is back on a host.

use std::time::Duration;

use esp_idf_svc::sys::esp_timer_get_time;

/// Fast enough to resolve the step in terminal voltage when charge current
/// starts, which a one-minute dashboard cadence would smear away.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// 30 minutes at the sampling interval: long enough to cover an unplug, a
/// stretch on the adapter, and the walk back to the host.
const CAPACITY: usize = 360;

#[derive(Clone, Copy, Default)]
struct Sample {
    uptime_s: u32,
    millivolts: u16,
    usb_host: bool,
    eco: bool,
}

pub struct PowerLog {
    samples: [Sample; CAPACITY],
    next: usize,
    len: usize,
}

impl PowerLog {
    pub const fn new() -> Self {
        Self {
            samples: [Sample {
                uptime_s: 0,
                millivolts: 0,
                usb_host: false,
                eco: false,
            }; CAPACITY],
            next: 0,
            len: 0,
        }
    }

    pub fn record(&mut self, millivolts: f32, usb_host: bool, eco: bool) {
        let uptime_s = (unsafe { esp_timer_get_time() } / 1_000_000).max(0) as u32;
        self.samples[self.next] = Sample {
            uptime_s,
            millivolts: millivolts.round().clamp(0.0, f32::from(u16::MAX)) as u16,
            usb_host,
            eco,
        };
        self.next = (self.next + 1) % CAPACITY;
        self.len = (self.len + 1).min(CAPACITY);
    }

    pub fn clear(&mut self) {
        self.next = 0;
        self.len = 0;
    }

    /// Print oldest to newest, one sample per line, so a host-side script can
    /// read the dump without knowing where the ring happened to wrap.
    pub fn print(&self) {
        println!(
            "OK POWER LOG samples={} interval={}s",
            self.len,
            SAMPLE_INTERVAL.as_secs()
        );
        let oldest = (self.next + CAPACITY - self.len) % CAPACITY;
        for offset in 0..self.len {
            let sample = self.samples[(oldest + offset) % CAPACITY];
            println!(
                "t={} mv={} usb={} eco={}",
                sample.uptime_s,
                sample.millivolts,
                u8::from(sample.usb_host),
                u8::from(sample.eco)
            );
        }
        println!("OK POWER LOG end");
    }
}
