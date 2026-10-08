use super::*;
use serde_json::Value;

pub fn subscription(request: Uuid) -> SubscribePayload {
    SubscribePayload { rfq_id: request }
}
pub fn decline_request(request: Uuid) -> DeclinePayload {
    DeclinePayload { rfq_id: request }
}
pub fn acceptance(request: Uuid, side: Side) -> AcceptPayload {
    AcceptPayload {
        rfq_id: request,
        side,
    }
}
pub fn cancellation(request: Uuid, order: Option<Uuid>, number: Option<u64>) -> CancelOrderPayload {
    CancelOrderPayload {
        rfq_id: request,
        order_id: order,
        client_order_id: number,
    }
}
pub fn sell_offer(request: Uuid, size: u64, price: u64) -> PlaceOrderPayload {
    PlaceOrderPayload {
        rfq_id: request,
        side: Side::Sell,
        price,
        size,
        client_order_id: None,
    }
}

pub fn changed_fields<T>(signed: &Envelope<T>, changes: &[(&str, Value)]) -> Vec<Envelope<T>>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    changes
        .iter()
        .map(|(field, value)| {
            let mut wire = serde_json::to_value(signed).expect("signed request should serialize");
            wire["payload"][*field] = value.clone();
            serde_json::from_value(wire).expect("changed field should retain the request type")
        })
        .collect()
}
