//! Maker offers, cancellations, and their reservations.

use super::*;

impl Engine {
    pub fn place(
        &mut self,
        maker: Pubkey,
        payload: PlaceOrderPayload,
    ) -> Result<PlaceOrderResponse> {
        let rfq = self.rfq(payload.rfq_id)?;
        if rfq.view.state != RfqState::Open {
            return Err(Error::new(RFQ_NOT_OPEN));
        }
        if rfq.owner == maker {
            return Err(Error::new(SELF_QUOTE_NOT_ALLOWED));
        }
        if payload.size == 0 {
            return Err(Error::new(INVALID_SIZE));
        }
        if payload.size < rfq.view.size {
            return Err(Error::new(ORDER_TOO_SMALL));
        }
        if payload.price == 0 {
            return Err(Error::malformed("price must be positive"));
        }
        if payload.client_order_id.is_some_and(|client_order_id| {
            self.client_order_ids
                .contains_key(&(maker, client_order_id))
        }) {
            return Err(Error::new(DUPLICATE_CLIENT_ORDER_ID));
        }
        let cash = self.notional(rfq, payload.price)?;
        let reserved_amount = if payload.side == Side::Buy {
            cash
        } else {
            rfq.view.size
        };
        let asset = rfq.view.asset.clone();
        let trader = self.traders.get_mut(&maker).unwrap();
        let balance = if payload.side == Side::Buy {
            &mut trader.usd
        } else {
            trader.assets.get_mut(&asset).unwrap()
        };
        if balance.available() < reserved_amount {
            return Err(Error::new(INSUFFICIENT_BALANCE));
        }
        balance.reserved += reserved_amount;
        self.sequence += 1;
        let id = Uuid::new_v4();
        self.orders.insert(
            id,
            Order {
                id,
                maker,
                rfq: payload.rfq_id,
                side: payload.side,
                price: payload.price,
                client_order_id: payload.client_order_id,
                reserved_amount,
                sequence: self.sequence,
                active: true,
            },
        );
        if let Some(client_order_id) = payload.client_order_id {
            self.client_order_ids.insert((maker, client_order_id), id);
        }
        self.rfqs.get_mut(&payload.rfq_id).unwrap().orders.push(id);
        Ok(PlaceOrderResponse { order_id: id })
    }

    pub fn cancel(
        &mut self,
        maker: Pubkey,
        payload: CancelOrderPayload,
    ) -> Result<CancelOrderResponse> {
        if payload.order_id.is_some() == payload.client_order_id.is_some() {
            return Err(Error::malformed("specify exactly one order identifier"));
        }
        let rfq = self.rfq(payload.rfq_id)?;
        if rfq.view.state != RfqState::Open {
            return Err(Error::new(RFQ_NOT_OPEN));
        }
        let asset = rfq.view.asset.clone();
        let id = payload
            .order_id
            .or_else(|| {
                self.client_order_ids
                    .get(&(maker, payload.client_order_id.unwrap()))
                    .copied()
            })
            .ok_or_else(|| Error::new(ORDER_NOT_FOUND))?;
        let order = self
            .orders
            .get(&id)
            .filter(|order| order.rfq == payload.rfq_id)
            .ok_or_else(|| Error::new(ORDER_NOT_FOUND))?;
        if order.maker != maker {
            return Err(Error::new(NOT_YOUR_ORDER));
        }
        if !order.active {
            return Err(Error::new(ORDER_NOT_FOUND));
        }
        self.release_order(id, &asset);
        Ok(CancelOrderResponse { order_id: id })
    }

    pub(super) fn release_order(&mut self, id: Uuid, asset: &str) {
        let order = self.orders.get_mut(&id).unwrap();
        if !order.active {
            return;
        }
        order.active = false;
        let trader = self.traders.get_mut(&order.maker).unwrap();
        let balance = if order.side == Side::Buy {
            &mut trader.usd
        } else {
            trader.assets.get_mut(asset).unwrap()
        };
        balance.reserved -= order.reserved_amount;
        if let Some(client_order_id) = order.client_order_id {
            self.client_order_ids
                .remove(&(order.maker, client_order_id));
        }
    }
}
