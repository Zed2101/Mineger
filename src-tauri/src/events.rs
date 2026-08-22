// src-tauri/src/events.rs
//
// Bus eventi interno: tutto ciò che il backend emette al frontend locale
// (`server-output`, `server-status`, `backup-progress`) passa anche di qui,
// così l'host remoto può inoltrarlo ai client collegati via WebSocket.

use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Event {
    pub name: &'static str,
    pub payload: Value,
}

const CAPACITY: usize = 2048;

static TX: OnceLock<broadcast::Sender<Event>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<Event> {
    TX.get_or_init(|| broadcast::channel(CAPACITY).0)
}

/// Pubblica un evento. Senza ricevitori attivi è un no-op.
pub fn publish(name: &'static str, payload: Value) {
    let _ = sender().send(Event { name, payload });
}

pub fn subscribe() -> broadcast::Receiver<Event> {
    sender().subscribe()
}
