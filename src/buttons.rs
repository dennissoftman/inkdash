use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Gpio0, Gpio18, Input, PinDriver, Pull};
use esp_idf_hal::task::block_on;
use esp_idf_svc::sys::{self, EspError};

use crate::config;
use crate::events::{AppEvent, EventSender};

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
    arm_light_sleep_wakeup(&boot)?;
    arm_light_sleep_wakeup(&power)?;
    let (raw_sender, raw_events) = mpsc::channel();
    let power_sender = raw_sender.clone();
    thread::Builder::new()
        .name("button-boot".into())
        .stack_size(config::INPUT_TASK_STACK_SIZE)
        .spawn(move || button_loop(boot, ButtonId::Boot, raw_sender))
        .context("starting BOOT interrupt task")?;
    thread::Builder::new()
        .name("button-power".into())
        .stack_size(config::INPUT_TASK_STACK_SIZE)
        .spawn(move || button_loop(power, ButtonId::Power, power_sender))
        .context("starting PWR interrupt task")?;
    thread::Builder::new()
        .name("button-gestures".into())
        .stack_size(config::INPUT_TASK_STACK_SIZE)
        .spawn(move || gesture_loop(raw_events, sender))
        .context("starting button gesture task")?;
    Ok(())
}

/// Register a button as a light-sleep wake source.
///
/// `gpio_wakeup_enable` rejects edge triggers and rewrites the pin's interrupt
/// type, which is why both waits below are level based: a press has to be able
/// to wake a sleeping CPU, and an edge cannot. The driver disables the
/// interrupt inside its own handler, so a held button cannot spin the ISR.
fn arm_light_sleep_wakeup(pin: &PinDriver<'static, Input>) -> Result<()> {
    // Active low through a pull-up, so the pressed state is the wake level.
    EspError::convert(unsafe {
        sys::gpio_wakeup_enable(pin.pin().into(), sys::gpio_int_type_t_GPIO_INTR_LOW_LEVEL)
    })
    .with_context(|| format!("enabling GPIO{} light-sleep wakeup", pin.pin()))?;
    EspError::convert(unsafe { sys::esp_sleep_enable_gpio_wakeup() })
        .context("enabling GPIO as a light-sleep wake source")?;
    Ok(())
}

fn button_loop(
    mut pin: PinDriver<'static, Input>,
    button: ButtonId,
    sender: mpsc::Sender<RawButtonEvent>,
) {
    loop {
        if let Err(error) = block_on(pin.wait_for_low()) {
            log::error!("Button interrupt failed: {error}");
            return;
        }
        // A one-shot settle delay is only armed after the pin reads pressed. It
        // consumes no CPU between presses and avoids turning switch bounce into
        // events.
        FreeRtos::delay_ms(config::BUTTON_DEBOUNCE_MS);
        if pin.is_low() {
            if sender.send(RawButtonEvent::Pressed(button)).is_err() {
                return;
            }
        } else {
            continue;
        }
        if block_on(pin.wait_for_high()).is_err() {
            return;
        }
        FreeRtos::delay_ms(config::BUTTON_DEBOUNCE_MS);
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
                let remaining = config::BUTTON_LONG_PRESS.saturating_sub(started.elapsed());
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
