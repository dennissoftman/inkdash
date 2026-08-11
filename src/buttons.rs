use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use esp_idf_hal::gpio::{Gpio0, Gpio18, Input, PinDriver, Pull};

use crate::commands::{Command, CommandSender};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEBOUNCE_SAMPLES: u8 = 3;

pub fn start(
    boot_pin: Gpio0<'static>,
    power_pin: Gpio18<'static>,
    sender: CommandSender,
) -> Result<()> {
    let boot = PinDriver::input(boot_pin, Pull::Up)?;
    let power = PinDriver::input(power_pin, Pull::Up)?;
    thread::Builder::new()
        .name("buttons".into())
        .stack_size(3072)
        .spawn(move || button_loop(boot, power, sender))
        .context("starting button input thread")?;
    Ok(())
}

fn button_loop(
    boot: PinDriver<'static, Input>,
    power: PinDriver<'static, Input>,
    sender: CommandSender,
) {
    let mut boot_state = DebouncedButton::new(boot.is_low());
    let mut power_state = DebouncedButton::new(power.is_low());
    loop {
        if boot_state.update(boot.is_low()) {
            let _ = sender.try_send(Ok(Command::NextScreen));
        }
        if power_state.update(power.is_low()) {
            let _ = sender.try_send(Ok(Command::PreviousScreen));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

struct DebouncedButton {
    stable_pressed: bool,
    candidate_pressed: bool,
    candidate_samples: u8,
}

impl DebouncedButton {
    const fn new(pressed: bool) -> Self {
        Self {
            stable_pressed: pressed,
            candidate_pressed: pressed,
            candidate_samples: 0,
        }
    }

    /// Returns true once for each debounced press. Releases are consumed only
    /// to re-arm the next press.
    fn update(&mut self, pressed: bool) -> bool {
        if pressed != self.candidate_pressed {
            self.candidate_pressed = pressed;
            self.candidate_samples = 1;
            return false;
        }
        if pressed == self.stable_pressed {
            self.candidate_samples = 0;
            return false;
        }
        self.candidate_samples = self.candidate_samples.saturating_add(1);
        if self.candidate_samples < DEBOUNCE_SAMPLES {
            return false;
        }
        self.stable_pressed = pressed;
        self.candidate_samples = 0;
        pressed
    }
}
