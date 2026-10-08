use super::*;

pub struct OfferBook {
    offers: Vec<(Side, u64, usize, bool)>,
    pub preferred_side: Side,
}

pub fn all_three_offer_books(prices: [u64; 3]) -> Vec<OfferBook> {
    let mut books = Vec::new();
    for sides in 0..8 {
        for price_choices in 0..27u64 {
            for cancellations in 0..8 {
                let offers = (0..3)
                    .map(|index| {
                        let side = if sides & (1 << index) == 0 {
                            Side::Buy
                        } else {
                            Side::Sell
                        };
                        let price = prices[(price_choices / 3u64.pow(index) % 3) as usize];
                        let maker = if index == 1 { 2 } else { 1 };
                        let cancelled = cancellations & (1 << index) != 0;
                        (side, price, maker, cancelled)
                    })
                    .collect();
                books.push(OfferBook {
                    offers,
                    preferred_side: if price_choices.is_multiple_of(2) {
                        Side::Buy
                    } else {
                        Side::Sell
                    },
                });
            }
        }
    }
    books
}

#[derive(Clone, Copy)]
struct ModelOffer {
    arrival: usize,
    side: Side,
    price: u64,
    maker: Pubkey,
    id: Uuid,
}
pub struct ExpectedQuote {
    ask: Option<ModelOffer>,
    bid: Option<ModelOffer>,
}

pub struct BookTrial {
    engine: Engine,
    traders: [Pubkey; 3],
    initial: BTreeMap<String, u128>,
    request: Uuid,
    direction: Option<Side>,
    live_offers: Vec<ModelOffer>,
    request_lots: u64,
}

impl BookTrial {
    pub fn new(
        direction: Option<Side>,
        request_lots: u64,
        cash_each: u64,
        sol_lots_each: u64,
    ) -> Self {
        let (mut engine, traders) = world_with_funds(cash_each, sol_lots_each);
        let initial = ledger(&engine);
        let request = submit_sized(&mut engine, traders[0], direction, 0, request_lots, 1);
        Self {
            engine,
            traders,
            initial,
            request,
            direction,
            live_offers: vec![],
            request_lots,
        }
    }
    pub fn place_offers_and_cancel_selected(&mut self, book: &OfferBook, offer_lots: u64) {
        for (arrival, &(side, price, maker, cancelled)) in book.offers.iter().enumerate() {
            let maker = self.traders[maker];
            let offer = order_sized(
                &mut self.engine,
                maker,
                self.request,
                side,
                price,
                Some(arrival as u64),
                offer_lots,
            )
            .expect("generated offer should fit");
            if cancelled {
                self.engine
                    .cancel(
                        maker,
                        CancelOrderPayload {
                            rfq_id: self.request,
                            order_id: Some(offer.order_id),
                            client_order_id: None,
                        },
                    )
                    .expect("generated cancellation should work");
            } else {
                self.live_offers.push(ModelOffer {
                    arrival,
                    side,
                    price,
                    maker,
                    id: offer.order_id,
                });
            }
            audit(&self.engine, &self.initial);
        }
    }
    pub fn independently_choose_best_prices(&self) -> ExpectedQuote {
        let ask = self
            .live_offers
            .iter()
            .filter(|offer| offer.side == Side::Sell && self.direction != Some(Side::Sell))
            .min_by_key(|offer| (offer.price, offer.arrival))
            .copied();
        let bid = self
            .live_offers
            .iter()
            .filter(|offer| offer.side == Side::Buy && self.direction != Some(Side::Buy))
            .min_by_key(|offer| (std::cmp::Reverse(offer.price), offer.arrival))
            .copied();
        ExpectedQuote { ask, bid }
    }
    pub fn close_quoting(&mut self, at_ms: u64) {
        self.engine.advance(at_ms);
    }
    pub fn expect_quote(&self, expected: &ExpectedQuote) {
        assert_eq!(
            self.engine.rfqs[&self.request].ask,
            expected.ask.map(|offer| offer.id)
        );
        assert_eq!(
            self.engine.rfqs[&self.request].bid,
            expected.bid.map(|offer| offer.id)
        );
        audit(&self.engine, &self.initial);
    }
    pub fn accept_and_expect_exact_balances(
        &mut self,
        expected: &ExpectedQuote,
        preferred_side: Side,
    ) {
        let chosen = if preferred_side == Side::Buy {
            expected.ask.or(expected.bid)
        } else {
            expected.bid.or(expected.ask)
        };
        let Some(offer) = chosen else {
            assert_eq!(
                self.engine.rfqs[&self.request].view.state,
                RfqState::Expired
            );
            return;
        };
        let taker = self.traders[0];
        let side = offer.side.opposite();
        let (buyer, seller) = if side == Side::Buy {
            (taker, offer.maker)
        } else {
            (offer.maker, taker)
        };
        let mut balances: BTreeMap<_, _> = self
            .engine
            .traders
            .iter()
            .map(|(key, trader)| (*key, (trader.usd.total, trader.assets["SOL"].total)))
            .collect();
        let payment = ((self.request_lots as u128 * offer.price as u128) / 100) as u64;
        balances.get_mut(&buyer).unwrap().0 -= payment;
        balances.get_mut(&buyer).unwrap().1 += self.request_lots;
        balances.get_mut(&seller).unwrap().0 += payment;
        balances.get_mut(&seller).unwrap().1 -= self.request_lots;
        let trade = self
            .engine
            .accept(
                taker,
                AcceptPayload {
                    rfq_id: self.request,
                    side,
                },
                1001,
            )
            .expect("model-backed trade should settle")
            .trade;
        assert_eq!(
            (trade.size, trade.price, trade.notional_usd),
            (self.request_lots, offer.price, payment)
        );
        for (trader, expected) in balances {
            assert_eq!(
                (
                    self.engine.traders[&trader].usd.total,
                    self.engine.traders[&trader].assets["SOL"].total
                ),
                expected
            );
        }
    }
    pub fn expire_every_request(&mut self, at_ms: u64) {
        self.engine.advance(at_ms);
    }
    pub fn expect_no_reserved_funds(&self) {
        audit(&self.engine, &self.initial);
        assert!(self.engine.orders.values().all(|offer| !offer.active));
    }
}
