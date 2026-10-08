//! RFQ submission, deadline ordering, and winner selection.

use super::*;

impl Engine {
    pub fn submit(
        &mut self,
        owner: Pubkey,
        payload: SubmitRfqPayload,
        now: u64,
    ) -> Result<SubmitRfqResponse> {
        if !self.assets.contains_key(&payload.asset) {
            return Err(Error::new(UNKNOWN_ASSET));
        }
        if payload.size == 0 {
            return Err(Error::new(INVALID_SIZE));
        }
        if !(1..=29).contains(&payload.ttl_secs) {
            return Err(Error::new(INVALID_TTL));
        }
        let closes_at = now
            .checked_add(payload.ttl_secs * 1000)
            .ok_or_else(|| Error::new(INVALID_TTL))?;
        closes_at
            .checked_add(self.accept_window_ms)
            .ok_or_else(|| Error::new(INVALID_TTL))?;
        let id = Uuid::new_v4();
        self.rfqs.insert(
            id,
            Rfq {
                view: RfqView {
                    rfq_id: id,
                    asset: payload.asset.clone(),
                    size: payload.size,
                    state: RfqState::Open,
                    created_at: now,
                    closes_at,
                    accept_deadline: None,
                },
                owner,
                direction: payload.direction,
                orders: vec![],
                bid: None,
                ask: None,
                taker_reserved_usd: 0,
                taker_reserved_asset: 0,
                no_quote: false,
                trade: None,
                events: broadcast::channel(32).0,
            },
        );
        self.schedule(id, closes_at);
        let _ = self.makers.send(MakerEvent::RfqOpened {
            rfq_id: id,
            asset: payload.asset,
            size: payload.size,
            direction: payload.direction,
            created_at: now,
            closes_at,
        });
        Ok(SubmitRfqResponse {
            rfq_id: id,
            created_at: now,
            closes_at,
        })
    }

    /// Apply every due deadline before processing the next action.
    pub fn advance(&mut self, now: u64) {
        while let Some((&(deadline, sequence), &id)) = self.deadlines.first_key_value() {
            if deadline > now {
                break;
            }
            self.deadlines.remove(&(deadline, sequence));
            match self.rfqs[&id].view.state {
                RfqState::Open => self.close(id),
                RfqState::AwaitingAccept => self.finish(id, RfqState::Expired, None),
                _ => {}
            }
        }
    }

    fn close(&mut self, id: Uuid) {
        let rfq = self.rfqs[&id].clone();
        let _ = self.makers.send(MakerEvent::RfqWindowClosed { rfq_id: id });
        let (mut bid, mut ask) = self.select_winners(&rfq);
        // A two-way quote must reserve enough for either choice.
        let cash = ask
            .map(|order_id| self.notional(&rfq, self.orders[&order_id].price).unwrap())
            .unwrap_or(0);
        let trader = self.traders.get_mut(&rfq.owner).unwrap();
        if trader.usd.available() < cash {
            ask = None;
        }
        if trader.assets[&rfq.view.asset].available() < rfq.view.size {
            bid = None;
        }
        let taker_reserved_usd = if ask.is_some() { cash } else { 0 };
        let taker_reserved_asset = if bid.is_some() { rfq.view.size } else { 0 };
        trader.usd.reserved += taker_reserved_usd;
        trader.assets.get_mut(&rfq.view.asset).unwrap().reserved += taker_reserved_asset;
        for order_id in &rfq.orders {
            if Some(*order_id) != bid && Some(*order_id) != ask {
                self.release_order(*order_id, &rfq.view.asset);
            }
        }
        let live = self.rfqs.get_mut(&id).unwrap();
        live.bid = bid;
        live.ask = ask;
        live.taker_reserved_usd = taker_reserved_usd;
        live.taker_reserved_asset = taker_reserved_asset;
        if bid.is_none() && ask.is_none() {
            live.no_quote = true;
            let _ = live.events.send(TakerEvent::NoQuote { rfq_id: id });
            self.finish(id, RfqState::Expired, None);
        } else {
            live.view.state = RfqState::AwaitingAccept;
            let deadline = rfq.view.closes_at + self.accept_window_ms;
            live.view.accept_deadline = Some(deadline);
            let event = self.quote(id);
            let _ = self.rfqs[&id].events.send(event);
            self.schedule(id, deadline);
        }
    }

    // A maker bid buys from the taker; a maker ask sells to the taker.
    fn select_winners(&self, rfq: &Rfq) -> (Option<Uuid>, Option<Uuid>) {
        let mut bid: Option<Uuid> = None;
        let mut ask: Option<Uuid> = None;
        for order_id in &rfq.orders {
            let order = &self.orders[order_id];
            if !order.active
                || rfq
                    .direction
                    .is_some_and(|direction| direction == order.side)
            {
                continue;
            }
            let winner = if order.side == Side::Buy {
                &mut bid
            } else {
                &mut ask
            };
            let better = winner.is_none_or(|previous_id| {
                let previous = &self.orders[&previous_id];
                (if order.side == Side::Buy {
                    order.price > previous.price
                } else {
                    order.price < previous.price
                }) || (order.price == previous.price && order.sequence < previous.sequence)
            });
            if better {
                *winner = Some(*order_id);
            }
        }
        (bid, ask)
    }

    fn schedule(&mut self, id: Uuid, at: u64) {
        self.sequence += 1;
        self.deadlines.insert((at, self.sequence), id);
    }
}
