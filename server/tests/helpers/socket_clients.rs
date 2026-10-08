use super::*;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

pub struct SocketClient {
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}
impl TestServer {
    pub async fn taker_socket(&self, request: Uuid) -> SocketClient {
        self.socket(&format!("/ws/rfq/{request}")).await
    }
    pub async fn maker_socket(&self) -> SocketClient {
        self.socket("/ws/makers").await
    }
    async fn socket(&self, path: &str) -> SocketClient {
        let url = format!("{}{path}", self.world.url.replace("http://", "ws://"));
        let (socket, _) = connect_async(url).await.expect("socket should connect");
        SocketClient { socket }
    }
    pub fn burst_without_reading(&self, trader: &LiveTrader, count: usize, request: PriceRequest) {
        let mut engine = self.world.app.lock();
        for _ in 0..count {
            engine
                .submit(
                    trader.client.pubkey(),
                    request_payload(request),
                    self.world.app.now(),
                )
                .expect("burst request should succeed");
        }
    }
}
impl SocketClient {
    pub async fn send_json(&mut self, message: &Value) {
        self.send(Message::Text(message.to_string())).await;
    }
    pub async fn send(&mut self, message: Message) {
        self.socket.send(message).await.expect("frame should send");
    }
    async fn next(&mut self, within_secs: u64) -> Message {
        timeout(Duration::from_secs(within_secs), self.socket.next())
            .await
            .expect("frame should arrive before timeout")
            .expect("socket should still be open")
            .expect("valid WebSocket frame")
    }
    pub async fn expect_subscribed(&mut self, request: Uuid) {
        let frame = self.next(2).await;
        let event: TakerEvent = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert!(matches!(event, TakerEvent::Subscribed { rfq_id, .. } if rfq_id == request));
    }
    pub async fn expect_error(&mut self, code: &str, within_secs: u64) {
        let frame = self.next(within_secs).await;
        let event: TakerEvent = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        match event {
            TakerEvent::Error { message } => {
                assert!(message.starts_with(&format!("{code}:")), "{message}")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }
    pub async fn expect_closed(&mut self) {
        let frame = timeout(Duration::from_secs(2), self.socket.next())
            .await
            .expect("socket should close promptly");
        assert!(
            matches!(frame, None | Some(Ok(Message::Close(_)))),
            "unexpected frame: {frame:?}"
        );
    }
    pub async fn next_json(&mut self) -> Value {
        serde_json::from_str(self.next(2).await.to_text().unwrap()).unwrap()
    }
    pub async fn expect_pong(&mut self, bytes: &[u8]) {
        assert_eq!(self.next(2).await, Message::Pong(bytes.to_vec()));
    }
}
