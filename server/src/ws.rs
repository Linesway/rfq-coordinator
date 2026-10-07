use crate::{
    api::App,
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

pub async fn makers(
    State(app): State<Arc<App>>,
    ws: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Result<Response> {
    let ws = ws.map_err(Error::malformed)?;
    // Maker feed is live-only, with subscription registered before the upgrade.
    let rx = app.lock().makers.subscribe();
    Ok(ws
        .max_message_size(65536)
        .max_frame_size(65536)
        .on_upgrade(move |socket| maker_loop(socket, rx)))
}
pub async fn taker(
    State(app): State<Arc<App>>,
    path: std::result::Result<Path<String>, PathRejection>,
    ws: std::result::Result<
        WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Result<Response> {
    let Path(id) = path.map_err(Error::malformed)?;
    let id: Uuid = id.parse().map_err(Error::malformed)?;
    let ws = ws.map_err(Error::malformed)?;
    Ok(ws
        .max_message_size(65536)
        .max_frame_size(65536)
        .on_upgrade(move |socket| taker_loop(socket, app, id)))
}
async fn send<T: Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    let text = serde_json::to_string(value).expect("wire types serialize");
    matches!(
        timeout(IO_TIMEOUT, socket.send(Message::Text(text))).await,
        Ok(Ok(()))
    )
}
async fn incoming(
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
async fn maker_loop(mut socket: WebSocket, mut rx: broadcast::Receiver<MakerEvent>) {
    loop {
        tokio::select! {
            message = socket.recv() => if !incoming(&mut socket,message).await { break; },
            event = rx.recv() => match event {
                Ok(event) => if !send(&mut socket,&event).await { break; },
                Err(_) => break, // A lagging client must reconnect, never block exchange state.
            }
        }
    }
}
fn terminal(event: &TakerEvent) -> bool {
    matches!(
        event,
        TakerEvent::Settled { .. }
            | TakerEvent::Expired { .. }
            | TakerEvent::Declined { .. }
            | TakerEvent::Failed { .. }
    )
}
async fn taker_loop(mut socket: WebSocket, app: Arc<App>, id: Uuid) {
    let auth = timeout(IO_TIMEOUT, socket.recv()).await;
    let result = (|| {
        let text = match auth {
            Ok(Some(Ok(Message::Text(text)))) => text,
            _ => return Err(Error::malformed("first frame must be a signed subscribe")),
        };
        let e: Envelope<SubscribePayload> =
            serde_json::from_str(&text).map_err(Error::malformed)?;
        let mut engine = app.lock();
        let owner = engine.authenticate(&e)?;
        engine.advance(app.now());
        if e.payload.rfq_id != id {
            return Err(Error::malformed("subscribe RFQ does not match socket path"));
        }
        engine.subscribe(owner, id)
    })();
    let (snapshot, mut rx) = match result {
        Ok(v) => v,
        Err(e) => {
            send(
                &mut socket,
                &TakerEvent::Error {
                    message: format!("{}: {}", e.0, e.1),
                },
            )
            .await;
            return;
        }
    };
    for event in snapshot {
        if !send(&mut socket, &event).await || terminal(&event) {
            return;
        }
    }
    loop {
        tokio::select! {
            message = socket.recv() => if !incoming(&mut socket,message).await { break; },
            event = rx.recv() => match event {
                Ok(event) => if !send(&mut socket,&event).await || terminal(&event) { break; },
                Err(_) => break,
            }
        }
    }
}
