use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Gpio0, Gpio18, Input, PinDriver, Pull};
use esp_idf_hal::task::block_on;

use crate::events::{AppEvent, EventSender};

const DEBOUNCE_MS: u32 = 30;
const BUTTON_TASK_STACK_SIZE: usize = 4096;
const BOTH_BUTTONS_LONG_PRESS: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonEvent {
    Boot,
    Power,
    CheckForUpdates,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ButtonId {
    Boot,
    Power,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawButtonEvent {
    Pressed(ButtonId),
    Released(ButtonId),
}

pub fn start(
    boot_pin: Gpio0<'static>,
    power_pin: Gpio18<'static>,
    sender: EventSender,
) -> Result<()> {
    let boot = PinDriver::input(boot_pin, Pull::Up)?;
    let power = PinDriver::input(power_pin, Pull::Up)?;
    let (raw_sender, raw_events) = mpsc::channel();
    let power_sender = raw_sender.clone();
    thread::Builder::new()
        .name("button-boot".into())
        .stack_size(BUTTON_TASK_STACK_SIZE)
        .spawn(move || button_loop(boot, ButtonId::Boot, raw_sender))
        .context("starting BOOT interrupt task")?;
    thread::Builder::new()
        .name("button-power".into())
        .stack_size(BUTTON_TASK_STACK_SIZE)
        .spawn(move || button_loop(power, ButtonId::Power, power_sender))
        .context("starting PWR interrupt task")?;
    thread::Builder::new()
        .name("button-gestures".into())
        .stack_size(BUTTON_TASK_STACK_SIZE)
        .spawn(move || gesture_loop(raw_events, sender))
        .context("starting button gesture task")?;
    Ok(())
}

fn button_loop(
    mut pin: PinDriver<'static, Input>,
    button: ButtonId,
    sender: mpsc::Sender<RawButtonEvent>,
) {
    loop {
        if let Err(error) = block_on(pin.wait_for_falling_edge()) {
            log::error!("Button interrupt failed: {error}");
            return;
        }
        // A one-shot settle delay is only armed after a GPIO edge. It consumes
        // no CPU between presses and avoids turning switch bounce into events.
        FreeRtos::delay_ms(DEBOUNCE_MS);
        if pin.is_low() {
            if sender.send(RawButtonEvent::Pressed(button)).is_err() {
                return;
            }
        } else {
            continue;
        }
        if block_on(pin.wait_for_rising_edge()).is_err() {
            return;
        }
        FreeRtos::delay_ms(DEBOUNCE_MS);
        if sender.send(RawButtonEvent::Released(button)).is_err() {
            return;
        }
    }
}

fn gesture_loop(events: mpsc::Receiver<RawButtonEvent>, sender: EventSender) {
    let mut boot_down = false;
    let mut power_down = false;
    let mut chord_started: Option<Instant> = None;
    let mut chord_consumed = false;

    loop {
        let event = match chord_started.filter(|_| !chord_consumed) {
            Some(started) => {
                let remaining = BOTH_BUTTONS_LONG_PRESS.saturating_sub(started.elapsed());
                match events.recv_timeout(remaining) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => {
                        if boot_down && power_down {
                            chord_consumed = true;
                            if sender
                                .send(AppEvent::Button(ButtonEvent::CheckForUpdates))
                                .is_err()
                            {
                                return;
                            }
                        }
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match events.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            },
        };

        let Some(event) = event else {
            continue;
        };
        let was_chord = chord_started.is_some();
        match event {
            RawButtonEvent::Pressed(ButtonId::Boot) => boot_down = true,
            RawButtonEvent::Pressed(ButtonId::Power) => power_down = true,
            RawButtonEvent::Released(ButtonId::Boot) => {
                boot_down = false;
                if !was_chord && sender.send(AppEvent::Button(ButtonEvent::Boot)).is_err() {
                    return;
                }
            }
            RawButtonEvent::Released(ButtonId::Power) => {
                power_down = false;
                if !was_chord && sender.send(AppEvent::Button(ButtonEvent::Power)).is_err() {
                    return;
                }
            }
        }

        if boot_down && power_down && chord_started.is_none() {
            chord_started = Some(Instant::now());
        } else if !boot_down && !power_down {
            chord_started = None;
            chord_consumed = false;
        }
    }
}
