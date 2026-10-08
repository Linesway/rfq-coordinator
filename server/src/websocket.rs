//! Maker notifications and authenticated taker subscriptions.

use crate::{
    app::App,
    error::{Error, Result},
};
use axum::{
    extract::{
        rejection::PathRejection,
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::Response,
};
use common::{signing::Envelope, types::*};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tokio::{sync::broadcast, time::timeout};
use uuid::Uuid;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_BYTES: usize = 65_536;

pub async fn makers(
    State(app): State<Arc<App>>,
    upgrade: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Result<Response> {
    let upgrade = upgrade.map_err(Error::malformed)?;
    // Subscribe before the handshake so events during the upgrade are queued.
    let events = app.lock().makers.subscribe();
    Ok(upgrade
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| maker_loop(socket, events)))
}
pub async fn taker(
    State(app): State<Arc<App>>,
    path: std::result::Result<Path<String>, PathRejection>,
    upgrade: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Result<Response> {
    let Path(id) = path.map_err(Error::malformed)?;
    let id: Uuid = id.parse().map_err(Error::malformed)?;
    let upgrade = upgrade.map_err(Error::malformed)?;
    Ok(upgrade
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| taker_loop(socket, app, id)))
}

async fn maker_loop(mut socket: WebSocket, mut events: broadcast::Receiver<MakerEvent>) {
    loop {
        tokio::select! {
            message = socket.recv() => {
                if !handle_incoming(&mut socket, message).await {
                    break;
                }
            },
            event = events.recv() => match event {
                Ok(event) => {
                    if !send_event(&mut socket, &event).await {
                        break;
                    }
                },
                Err(_) => break, // Disconnect clients that fall behind.
            }
        }
    }
    close_socket(&mut socket).await;
}

async fn taker_loop(mut socket: WebSocket, app: Arc<App>, id: Uuid) {
    let first_frame = timeout(IO_TIMEOUT, socket.recv()).await;
    let result = (|| {
        let text = match first_frame {
            Ok(Some(Ok(Message::Text(text)))) => text,
            _ => return Err(Error::malformed("first frame must be a signed subscribe")),
        };
        let envelope: Envelope<SubscribePayload> =
            serde_json::from_str(&text).map_err(Error::malformed)?;
        let mut engine = app.lock();
        let owner = engine.authenticate(&envelope)?;
        engine.advance(app.now());
        if envelope.payload.rfq_id != id {
            return Err(Error::malformed("subscribe RFQ does not match socket path"));
        }
        engine.subscribe(owner, id)
    })();
    let (snapshot, mut events) = match result {
        Ok(subscription) => subscription,
        Err(error) => {
            send_event(
                &mut socket,
                &TakerEvent::Error {
                    message: format!("{}: {}", error.code, error.message),
                },
            )
            .await;
            close_socket(&mut socket).await;
            return;
        }
    };
    for event in snapshot {
        if !send_event(&mut socket, &event).await || is_terminal(&event) {
            close_socket(&mut socket).await;
            return;
        }
    }
    loop {
        tokio::select! {
            message = socket.recv() => {
                if !handle_incoming(&mut socket, message).await {
                    break;
                }
            },
            event = events.recv() => match event {
                Ok(event) => {
                    if !send_event(&mut socket, &event).await || is_terminal(&event) {
                        break;
                    }
                },
                Err(_) => break,
            }
        }
    }
    close_socket(&mut socket).await;
}

async fn send_event<T: Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    let text = serde_json::to_string(value).expect("wire types serialize");
    matches!(
        timeout(IO_TIMEOUT, socket.send(Message::Text(text))).await,
        Ok(Ok(()))
    )
}
async fn close_socket(socket: &mut WebSocket) {
    let _ = timeout(IO_TIMEOUT, socket.send(Message::Close(None))).await;
}

async fn handle_incoming(
    socket: &mut WebSocket,
    message: Option<std::result::Result<Message, axum::Error>>,
) -> bool {
    match message {
        None | Some(Err(_)) | Some(Ok(Message::Close(_))) => false,
        Some(Ok(Message::Ping(bytes))) => matches!(
            timeout(IO_TIMEOUT, socket.send(Message::Pong(bytes))).await,
            Ok(Ok(()))
        ),
        _ => true,
    }
}

fn is_terminal(event: &TakerEvent) -> bool {
    matches!(
        event,
        TakerEvent::Settled { .. }
            | TakerEvent::Expired { .. }
            | TakerEvent::Declined { .. }
            | TakerEvent::Failed { .. }
    )
}
