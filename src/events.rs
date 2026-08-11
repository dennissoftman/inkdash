use std::sync::mpsc::{self, Receiver, Sender};

use crate::commands::CommandMessage;
use crate::notifications::Notification;
use crate::weather::WorkerUpdate;

/// Every asynchronous producer feeds this single queue. The application task
/// blocks on it, reduces events into state, then renders at most once per batch.
#[derive(Debug)]
pub enum AppEvent {
    Command(CommandMessage),
    ClockDue,
    WeatherDue,
    WeatherCompleted(WorkerUpdate),
    Notification(Notification),
    WifiChanged,
    ReconnectDue,
}

pub type EventSender = Sender<AppEvent>;
pub type EventReceiver = Receiver<AppEvent>;

pub fn channel() -> (EventSender, EventReceiver) {
    mpsc::channel()
}
