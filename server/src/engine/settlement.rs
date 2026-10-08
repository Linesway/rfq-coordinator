//! Taker decisions, atomic transfers, and reservation cleanup.

use super::*;

impl Engine {
    pub fn accept(
        &mut self,
        owner: Pubkey,
        payload: AcceptPayload,
        now: u64,
    ) -> Result<AcceptResponse> {
        let rfq = self.owned_awaiting(owner, payload.rfq_id)?.clone();
        let order_id = if payload.side == Side::Buy {
            rfq.ask
        } else {
            rfq.bid
        }
        .ok_or_else(|| Error::new(SIDE_UNAVAILABLE))?;
        let order = self.orders[&order_id].clone();
        let cash = self.notional(&rfq, order.price)?;
        let (buyer, seller) = if payload.side == Side::Buy {
            (owner, order.maker)
        } else {
            (order.maker, owner)
        };
        // Check receiving balances for overflow before moving funds.
        if self.traders[&buyer].assets[&rfq.view.asset]
            .total
            .checked_add(rfq.view.size)
            .is_none()
            || self.traders[&seller].usd.total.checked_add(cash).is_none()
        {
            self.finish(payload.rfq_id, RfqState::Failed, None);
            return Err(Error::new(INSUFFICIENT_BALANCE));
        }
        let trade = Trade {
            trade_id: Uuid::new_v4(),
            rfq_id: payload.rfq_id,
            asset: rfq.view.asset.clone(),
            taker_side: payload.side,
            price: order.price,
            size: rfq.view.size,
            notional_usd: cash,
            executed_at: now,
        };
        // All checks are complete. The App lock covers both transfers and the final state.
        self.release_rfq(&rfq);
        self.traders.get_mut(&buyer).unwrap().usd.total -= cash;
        self.traders.get_mut(&seller).unwrap().usd.total += cash;
        self.traders
            .get_mut(&seller)
            .unwrap()
            .assets
            .get_mut(&rfq.view.asset)
            .unwrap()
            .total -= rfq.view.size;
        self.traders
            .get_mut(&buyer)
            .unwrap()
            .assets
            .get_mut(&rfq.view.asset)
            .unwrap()
            .total += rfq.view.size;
        self.finish(payload.rfq_id, RfqState::Settled, Some(trade.clone()));
        Ok(AcceptResponse { trade })
    }

    pub fn decline(&mut self, owner: Pubkey, payload: DeclinePayload) -> Result<DeclineResponse> {
        self.owned_awaiting(owner, payload.rfq_id)?;
        self.finish(payload.rfq_id, RfqState::Declined, None);
        Ok(DeclineResponse {
            rfq_id: payload.rfq_id,
            state: RfqState::Declined,
        })
    }

    fn owned_awaiting(&self, owner: Pubkey, id: Uuid) -> Result<&Rfq> {
        let rfq = self.rfq(id)?;
        if rfq.owner != owner {
            return Err(Error::new(NOT_YOUR_RFQ));
        }
        if rfq.view.state != RfqState::AwaitingAccept {
            return Err(Error::new(NOT_AWAITING_ACCEPT));
        }
        Ok(rfq)
    }

    fn release_rfq(&mut self, rfq: &Rfq) {
        for order_id in &rfq.orders {
            self.release_order(*order_id, &rfq.view.asset);
        }
        let trader = self.traders.get_mut(&rfq.owner).unwrap();
        trader.usd.reserved -= rfq.taker_reserved_usd;
        trader.assets.get_mut(&rfq.view.asset).unwrap().reserved -= rfq.taker_reserved_asset;
        let live = self.rfqs.get_mut(&rfq.view.rfq_id).unwrap();
        live.taker_reserved_usd = 0;
        live.taker_reserved_asset = 0;
    }

    pub(super) fn finish(&mut self, id: Uuid, state: RfqState, trade: Option<Trade>) {
        let rfq = self.rfqs[&id].clone();
        if rfq.view.state.is_terminal() {
            return;
        }
        self.release_rfq(&rfq);
        let (outcome, event) = match state {
            RfqState::Settled => (
                RfqOutcome::Settled,
                TakerEvent::Settled {
                    rfq_id: id,
                    trade: trade.clone().unwrap(),
                },
            ),
            RfqState::Declined => (RfqOutcome::Declined, TakerEvent::Declined { rfq_id: id }),
            RfqState::Failed => (RfqOutcome::Failed, TakerEvent::Failed { rfq_id: id }),
            _ => (RfqOutcome::Expired, TakerEvent::Expired { rfq_id: id }),
        };
        let fills = trade
            .as_ref()
            .map(|trade| {
                let order_id = if trade.taker_side == Side::Buy {
                    rfq.ask
                } else {
                    rfq.bid
                }
                .unwrap();
                let order = &self.orders[&order_id];
                vec![Fill {
                    order_id: order.id,
                    maker_side: order.side,
                    price: order.price,
                    size: trade.size,
                }]
            })
            .unwrap_or_default();
        let live = self.rfqs.get_mut(&id).unwrap();
        live.view.state = state;
        live.trade = trade;
        let _ = live.events.send(event);
        let _ = self.makers.send(MakerEvent::RfqDone {
            rfq_id: id,
            outcome,
            fills,
        });
    }
}
