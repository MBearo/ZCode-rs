use crate::Engine;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc};
use zcode_cli_core_api::{RuntimeEvent, SessionRuntime};
use zcode_cli_protocol::CommandAck;

/// Shared runtime handle for the TUI and App Server.
///
/// `Engine` remains the sole owner of session facts. This handle only
/// serializes access and republishes command acknowledgements as frontend
/// events; it does not cache session, queue, model, or tool state.
pub struct CoreRuntime {
    engine: Arc<Mutex<Engine>>,
    events: broadcast::Sender<RuntimeEvent>,
}

impl CoreRuntime {
    pub fn new(engine: Engine) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            engine: Arc::new(Mutex::new(engine)),
            events,
        })
    }

    pub fn engine(&self) -> Arc<Mutex<Engine>> {
        Arc::clone(&self.engine)
    }

    fn command_event(command: &zcode_cli_protocol::Command, payload: Value) -> RuntimeEvent {
        RuntimeEvent {
            trace_id: command.command_id.clone(),
            session_id: command.session_id.clone().unwrap_or_default(),
            run_id: None,
            turn_id: None,
            sequence: 0,
            kind: "commandAck".into(),
            payload,
        }
    }
}

#[async_trait]
impl SessionRuntime for CoreRuntime {
    async fn dispatch(&self, command: zcode_cli_protocol::Command) -> anyhow::Result<CommandAck> {
        let event_command = command.clone();
        let payload = self.engine.lock().await.dispatch_command(command).await?;
        let ack: CommandAck = serde_json::from_value(payload.clone())?;
        let _ = self.events.send(Self::command_event(&event_command, payload.clone()));
        Ok(ack)
    }

    async fn query(&self, method: &str, params: &Value) -> anyhow::Result<Value> {
        Ok(self.engine.lock().await.query_method(method, params)?)
    }

    async fn subscribe(&self, session_id: &str) -> anyhow::Result<mpsc::Receiver<RuntimeEvent>> {
        let mut source = self.events.subscribe();
        let session_id = session_id.to_owned();
        let (sender, receiver) = mpsc::channel(64);
        tokio::spawn(async move {
            while let Ok(event) = source.recv().await {
                if event.session_id == session_id && sender.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(receiver)
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        // The long-running `serve` loop owns cancellation. A handle shutdown
        // is intentionally idempotent and only releases queued subscribers.
        let _ = self.events.send(RuntimeEvent {
            trace_id: "runtime-shutdown".into(),
            session_id: String::new(),
            run_id: None,
            turn_id: None,
            sequence: 0,
            kind: "runtimeShutdown".into(),
            payload: json!({}),
        });
        Ok(())
    }
}
