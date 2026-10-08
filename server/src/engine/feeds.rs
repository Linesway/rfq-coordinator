//! Quote events and reconnect snapshots for takers.

use super::*;

impl Engine {
    /// Create the snapshot and receiver under one lock so no update can be missed.
    pub fn subscribe(
        &self,
        owner: Pubkey,
        id: Uuid,
    ) -> Result<(Vec<TakerEvent>, broadcast::Receiver<TakerEvent>)> {
        let rfq = self.rfq(id)?;
        if rfq.owner != owner {
            return Err(Error::new(NOT_YOUR_RFQ));
        }
        let mut events = vec![TakerEvent::Subscribed {
            rfq_id: id,
            state: rfq.view.state,
            closes_at: rfq.view.closes_at,
        }];
        if rfq.no_quote {
            events.push(TakerEvent::NoQuote { rfq_id: id });
        }
        match rfq.view.state {
            RfqState::Open => {}
            RfqState::AwaitingAccept => events.push(self.quote(id)),
            RfqState::Settled => events.push(TakerEvent::Settled {
                rfq_id: id,
                trade: rfq.trade.clone().unwrap(),
            }),
            RfqState::Expired => events.push(TakerEvent::Expired { rfq_id: id }),
            RfqState::Declined => events.push(TakerEvent::Declined { rfq_id: id }),
            RfqState::Failed => events.push(TakerEvent::Failed { rfq_id: id }),
        }
        Ok((events, rfq.events.subscribe()))
    }

    pub(super) fn quote(&self, id: Uuid) -> TakerEvent {
        let rfq = &self.rfqs[&id];
        TakerEvent::Quote {
            rfq_id: id,
            bid: rfq.bid.map(|order_id| QuoteLevel {
                price: self.orders[&order_id].price,
            }),
            ask: rfq.ask.map(|order_id| QuoteLevel {
                price: self.orders[&order_id].price,
            }),
            size: rfq.view.size,
            accept_deadline: rfq.view.accept_deadline.unwrap(),
        }
    }
}
