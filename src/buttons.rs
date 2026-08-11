use std::thread;

use anyhow::{Context, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Gpio0, Gpio18, Input, PinDriver, Pull};
use esp_idf_hal::task::block_on;

use crate::commands::Command;
use crate::events::{AppEvent, EventSender};

const DEBOUNCE_MS: u32 = 30;

pub fn start(
    boot_pin: Gpio0<'static>,
    power_pin: Gpio18<'static>,
    sender: EventSender,
) -> Result<()> {
    let boot = PinDriver::input(boot_pin, Pull::Up)?;
    let power = PinDriver::input(power_pin, Pull::Up)?;
    let power_sender = sender.clone();
    thread::Builder::new()
        .name("button-boot".into())
        .stack_size(2048)
        .spawn(move || button_loop(boot, Command::NextScreen, sender))
        .context("starting BOOT interrupt task")?;
    thread::Builder::new()
        .name("button-power".into())
        .stack_size(2048)
        .spawn(move || button_loop(power, Command::PreviousScreen, power_sender))
        .context("starting PWR interrupt task")?;
    Ok(())
}

fn button_loop(mut pin: PinDriver<'static, Input>, command: Command, sender: EventSender) {
    loop {
        if let Err(error) = block_on(pin.wait_for_falling_edge()) {
            log::error!("Button interrupt failed: {error}");
            return;
        }
        // A one-shot settle delay is only armed after a GPIO edge. It consumes
        // no CPU between presses and avoids turning switch bounce into events.
        FreeRtos::delay_ms(DEBOUNCE_MS);
        if pin.is_low() && sender.send(AppEvent::Command(Ok(command.clone()))).is_err() {
            return;
        }
        if pin.is_low() && block_on(pin.wait_for_rising_edge()).is_err() {
            return;
        }
        FreeRtos::delay_ms(DEBOUNCE_MS);
    }
}
