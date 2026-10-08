//! HTTP routes, request decoding, and signed action dispatch.

use crate::{
    app::App,
    engine::Engine,
    error::{Error, Result},
    websocket,
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
    signing::{Envelope, SignablePayload},
    types::*,
    Pubkey,
};
use std::sync::Arc;
use uuid::Uuid;

pub fn router(app: Arc<App>) -> Router {
    crate::app::start_deadline_timer(&app);
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
        .route("/ws/makers", get(websocket::makers))
        .route("/ws/rfq/:id", get(websocket::taker))
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

// All signed actions use the same validation order and hold one lock through mutation.
fn execute_action<Payload: SignablePayload, Response>(
    app: &App,
    body: std::result::Result<Json<Envelope<Payload>>, JsonRejection>,
    action: impl FnOnce(&mut Engine, Pubkey, Payload, u64) -> Result<Response>,
) -> Result<Json<Response>> {
    let Json(envelope) = body.map_err(Error::malformed)?;
    let mut engine = app.lock();
    let owner = engine.authenticate(&envelope)?;
    // The deadline may have passed while verifying the signature.
    let now = app.now();
    engine.advance(now);
    Ok(Json(action(&mut engine, owner, envelope.payload, now)?))
}

async fn submit(
    State(app): State<Arc<App>>,
    body: std::result::Result<Json<Envelope<SubmitRfqPayload>>, JsonRejection>,
) -> Result<Json<SubmitRfqResponse>> {
    execute_action(&app, body, |engine, owner, payload, now| {
        engine.submit(owner, payload, now)
    })
}

async fn place(
    State(app): State<Arc<App>>,
    body: std::result::Result<Json<Envelope<PlaceOrderPayload>>, JsonRejection>,
) -> Result<Json<PlaceOrderResponse>> {
    execute_action(&app, body, |engine, owner, payload, _now| {
        engine.place(owner, payload)
    })
}

async fn cancel(
    State(app): State<Arc<App>>,
    body: std::result::Result<Json<Envelope<CancelOrderPayload>>, JsonRejection>,
) -> Result<Json<CancelOrderResponse>> {
    execute_action(&app, body, |engine, owner, payload, _now| {
        engine.cancel(owner, payload)
    })
}

async fn accept(
    State(app): State<Arc<App>>,
    body: std::result::Result<Json<Envelope<AcceptPayload>>, JsonRejection>,
) -> Result<Json<AcceptResponse>> {
    execute_action(&app, body, |engine, owner, payload, now| {
        engine.accept(owner, payload, now)
    })
}

async fn decline(
    State(app): State<Arc<App>>,
    body: std::result::Result<Json<Envelope<DeclinePayload>>, JsonRejection>,
) -> Result<Json<DeclineResponse>> {
    execute_action(&app, body, |engine, owner, payload, _now| {
        engine.decline(owner, payload)
    })
}
