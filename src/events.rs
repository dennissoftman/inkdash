use std::sync::mpsc::{self, Receiver, Sender};

use crate::buttons::ButtonEvent;
use crate::commands::CommandMessage;
use crate::location::WorkerUpdate as LocationWorkerUpdate;
use crate::notifications::Notification;
use crate::ota::WorkerEvent as OtaWorkerEvent;
use crate::weather::WorkerUpdate;

/// Every asynchronous producer feeds this single queue. The application task
/// blocks on it, reduces events into state, then renders at most once per batch.
#[derive(Debug)]
pub enum AppEvent {
    Button(ButtonEvent),
    Command(CommandMessage),
    ClockDue,
    WeatherDue,
    WeatherCompleted(WorkerUpdate),
    LocationCompleted(LocationWorkerUpdate),
    Notification(Notification),
    WifiChanged,
    ReconnectDue,
    NtpSynchronized,
    Ota(OtaWorkerEvent),
    OtaConfirmationExpired,
    OtaRestartDue,
}

pub type EventSender = Sender<AppEvent>;
pub type EventReceiver = Receiver<AppEvent>;

pub fn channel() -> (EventSender, EventReceiver) {
    mpsc::channel()
}
