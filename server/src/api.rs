use crate::{
    engine::Engine,
    error::{Error, Result},
    ws,
};
use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        Path, State,
    },
    routing::{get, post},
    Json, Router,
};
use common::{
    seed::SeedConfig,
    signing::{unix_millis, Envelope},
    types::*,
};
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub struct App {
    engine: Mutex<Engine>,
    epoch: u64,
    started: Instant,
}
impl App {
    pub fn new(seed: SeedConfig, accept_secs: u64) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            engine: Mutex::new(Engine::new(seed, accept_secs)?),
            epoch: unix_millis(),
            started: Instant::now(),
        }))
    }
    pub fn now(&self) -> u64 {
        self.epoch + self.started.elapsed().as_millis() as u64
    }
    pub fn lock(&self) -> MutexGuard<'_, Engine> {
        let mut engine = self.engine.lock().expect("exchange state poisoned");
        engine.advance(self.now());
        engine
    }
}
pub fn router(app: Arc<App>) -> Router {
    // Weak ownership lets this task terminate when a test/server drops its app.
    let weak = Arc::downgrade(&app);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_millis(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let Some(app) = weak.upgrade() else {
                break;
            };
            drop(app.lock());
        }
    });
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/assets", get(assets))
        .route("/v1/balances", get(balances))
        .route("/v1/balances/:pubkey", get(balances_of))
        .route("/v1/rfqs/:id", get(rfq))
        .route("/v1/rfqs", post(submit))
        .route("/v1/orders", post(place))
        .route("/v1/orders/cancel", post(cancel))
        .route("/v1/accept", post(accept))
        .route("/v1/decline", post(decline))
        .route("/ws/makers", get(ws::makers))
        .route("/ws/rfq/:id", get(ws::taker))
        .fallback(|| async { Error::malformed("unknown endpoint") })
        .method_not_allowed_fallback(|| async { Error::malformed("unsupported HTTP method") })
        .with_state(app)
}
async fn assets(State(app): State<Arc<App>>) -> Json<AssetsResponse> {
    Json(app.lock().assets())
}
async fn balances(State(app): State<Arc<App>>) -> Json<BalancesResponse> {
    Json(app.lock().balances())
}
async fn balances_of(
    State(app): State<Arc<App>>,
    path: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<TraderBalances>> {
    let Path(key) = path.map_err(Error::malformed)?;
    let key = key.parse().map_err(Error::malformed)?;
    Ok(Json(app.lock().balances_of(key)?))
}
async fn rfq(
    State(app): State<Arc<App>>,
    path: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<RfqView>> {
    let Path(id) = path.map_err(Error::malformed)?;
    let id: Uuid = id.parse().map_err(Error::malformed)?;
    Ok(Json(app.lock().rfq(id)?.view.clone()))
}

// A single handler pattern guarantees the same authentication/error order.
macro_rules! action {
    ($name:ident, $payload:ty, $response:ty, $method:ident $(, $clock:ident)?) => {
        async fn $name(State(app): State<Arc<App>>, body: std::result::Result<Json<Envelope<$payload>>, JsonRejection>) -> Result<Json<$response>> {
            let Json(envelope) = body.map_err(Error::malformed)?;
            let mut engine = app.lock();
            let owner = engine.authenticate(&envelope)?;
            // Signature verification takes time; advance again immediately
            // before the action so authentication cannot stretch a deadline.
            let now = app.now();
            engine.advance(now);
            Ok(Json(engine.$method(owner,envelope.payload $(, { let $clock = now; $clock })?)?))
        }
    };
}
action!(submit, SubmitRfqPayload, SubmitRfqResponse, submit, now);
action!(place, PlaceOrderPayload, PlaceOrderResponse, place);
action!(cancel, CancelOrderPayload, CancelOrderResponse, cancel);
action!(accept, AcceptPayload, AcceptResponse, accept, now);
action!(decline, DeclinePayload, DeclineResponse, decline);
