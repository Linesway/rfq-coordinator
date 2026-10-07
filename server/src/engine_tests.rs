use super::*;
use common::{
    seed::{AssetDef, TraderSeed},
    Keypair,
};

fn world() -> (Engine, [Pubkey; 3]) {
    let keys = [1, 2, 3].map(|v| Keypair::from_seed([v; 32]).pubkey());
    let seed = SeedConfig {
        assets: BTreeMap::from([("SOL".into(), AssetDef { lot_decimals: 2 })]),
        traders: keys
            .iter()
            .map(|key| TraderSeed {
                pubkey: *key,
                name: None,
                usd: 1_000_000,
                assets: BTreeMap::from([("SOL".into(), 1000)]),
            })
            .collect(),
    };
    (Engine::new(seed, 1).unwrap(), keys)
}
fn submit(e: &mut Engine, owner: Pubkey, direction: Option<Side>, now: u64) -> Uuid {
    e.submit(
        owner,
        SubmitRfqPayload {
            asset: "SOL".into(),
            size: 100,
            ttl_secs: 1,
            direction,
        },
        now,
    )
    .unwrap()
    .rfq_id
}
fn order(
    e: &mut Engine,
    maker: Pubkey,
    id: Uuid,
    side: Side,
    price: u64,
    cid: Option<u64>,
) -> Result<PlaceOrderResponse> {
    e.place(
        maker,
        PlaceOrderPayload {
            rfq_id: id,
            side,
            price,
            size: 300,
            client_order_id: cid,
        },
    )
}
fn totals(e: &Engine) -> (u128, u128) {
    e.traders.values().fold((0, 0), |(usd, asset), t| {
        (
            usd + t.usd.total as u128,
            asset + t.assets["SOL"].total as u128,
        )
    })
}
fn invariants(e: &Engine, initial: (u128, u128)) {
    assert_eq!(
        totals(e),
        initial,
        "settlement must conserve every currency"
    );
    for (key, t) in &e.traders {
        assert!(t.usd.reserved <= t.usd.total);
        assert!(t.assets["SOL"].reserved <= t.assets["SOL"].total);
        let mut usd = 0u128;
        let mut asset = 0u128;
        for o in e.orders.values().filter(|o| o.active && o.maker == *key) {
            if o.side == Side::Buy {
                usd += o.amount as u128;
            } else {
                asset += o.amount as u128;
            }
        }
        for r in e.rfqs.values().filter(|r| r.owner == *key) {
            usd += r.taker_usd as u128;
            asset += r.taker_asset as u128;
        }
        assert_eq!(
            t.usd.reserved as u128, usd,
            "USD holds must match active commitments"
        );
        assert_eq!(
            t.assets["SOL"].reserved as u128, asset,
            "asset holds must match active commitments"
        );
    }
}

#[test]
fn best_price_then_arrival_and_exact_requested_size() {
    let (mut e, [t, a, b]) = world();
    let initial = totals(&e);
    let id = submit(&mut e, t, Some(Side::Buy), 0);
    let first = order(&mut e, a, id, Side::Sell, 900, Some(1))
        .unwrap()
        .order_id;
    order(&mut e, b, id, Side::Sell, 900, Some(1)).unwrap();
    order(&mut e, b, id, Side::Sell, 1000, Some(2)).unwrap();
    order(&mut e, a, id, Side::Buy, 2000, Some(2)).unwrap();
    e.advance(1000);
    assert_eq!(e.rfqs[&id].ask, Some(first));
    assert_eq!(e.rfqs[&id].bid, None);
    assert_eq!(e.balances_of(b).unwrap().assets["SOL"].reserved, 0);
    assert_eq!(e.balances_of(t).unwrap().usd.reserved, 900);
    assert_eq!(e.balances_of(a).unwrap().assets["SOL"].reserved, 100);
    let trade = e
        .accept(
            t,
            AcceptPayload {
                rfq_id: id,
                side: Side::Buy,
            },
            1500,
        )
        .unwrap()
        .trade;
    assert_eq!((trade.size, trade.notional_usd), (100, 900));
    assert_eq!(e.balances_of(t).unwrap().assets["SOL"].total, 1100);
    assert_eq!(e.balances_of(t).unwrap().usd.total, 999100);
    assert_eq!(
        e.accept(
            t,
            AcceptPayload {
                rfq_id: id,
                side: Side::Buy
            },
            1501
        )
        .unwrap_err()
        .0,
        NOT_AWAITING_ACCEPT
    );
    invariants(&e, initial);
}

#[test]
fn two_way_sell_chooses_highest_bid_and_releases_both_sides() {
    let (mut e, [t, a, b]) = world();
    let initial = totals(&e);
    let id = submit(&mut e, t, None, 0);
    order(&mut e, a, id, Side::Buy, 1100, None).unwrap();
    let best = order(&mut e, b, id, Side::Buy, 1200, None)
        .unwrap()
        .order_id;
    order(&mut e, a, id, Side::Sell, 1300, None).unwrap();
    e.advance(1000);
    assert_eq!(e.rfqs[&id].bid, Some(best));
    let balance = e.balances_of(t).unwrap();
    assert_eq!(
        (balance.usd.reserved, balance.assets["SOL"].reserved),
        (1300, 100)
    );
    e.accept(
        t,
        AcceptPayload {
            rfq_id: id,
            side: Side::Sell,
        },
        1100,
    )
    .unwrap();
    assert_eq!(e.balances_of(t).unwrap().usd.total, 1001200);
    invariants(&e, initial);
    assert!(e.orders.values().all(|o| !o.active));
}

#[test]
fn cancellation_global_client_ids_and_ownership() {
    let (mut e, [t, a, b]) = world();
    let id = submit(&mut e, t, None, 0);
    let other = submit(&mut e, t, None, 0);
    let oid = order(&mut e, a, id, Side::Sell, 1000, Some(7))
        .unwrap()
        .order_id;
    assert_eq!(
        order(&mut e, a, other, Side::Sell, 1000, Some(7))
            .unwrap_err()
            .0,
        DUPLICATE_CLIENT_ORDER_ID
    );
    let p = CancelOrderPayload {
        rfq_id: id,
        order_id: Some(oid),
        client_order_id: None,
    };
    assert_eq!(e.cancel(b, p.clone()).unwrap_err().0, NOT_YOUR_ORDER);
    e.cancel(a, p.clone()).unwrap();
    assert_eq!(e.cancel(a, p).unwrap_err().0, ORDER_NOT_FOUND);
    assert_eq!(e.balances_of(a).unwrap().assets["SOL"].reserved, 0);
    order(&mut e, a, other, Side::Sell, 1000, Some(7)).unwrap();
    e.cancel(
        a,
        CancelOrderPayload {
            rfq_id: other,
            order_id: None,
            client_order_id: Some(7),
        },
    )
    .unwrap();
}

#[test]
fn shared_inventory_cannot_be_overcommitted() {
    let (mut e, [t, a, _]) = world();
    let initial = totals(&e);
    e.traders
        .get_mut(&a)
        .unwrap()
        .assets
        .get_mut("SOL")
        .unwrap()
        .total = 100;
    let initial = (initial.0, initial.1 - 900);
    let one = submit(&mut e, t, None, 0);
    let two = submit(&mut e, t, None, 0);
    let oid = order(&mut e, a, one, Side::Sell, 1000, None)
        .unwrap()
        .order_id;
    assert_eq!(
        order(&mut e, a, two, Side::Sell, 1000, None).unwrap_err().0,
        INSUFFICIENT_BALANCE
    );
    e.cancel(
        a,
        CancelOrderPayload {
            rfq_id: one,
            order_id: Some(oid),
            client_order_id: None,
        },
    )
    .unwrap();
    order(&mut e, a, two, Side::Sell, 1000, None).unwrap();
    invariants(&e, initial);
}

#[test]
fn close_and_accept_deadlines_are_exclusive() {
    let (mut e, [t, a, _]) = world();
    let initial = totals(&e);
    let id = submit(&mut e, t, None, 0);
    let oid = order(&mut e, a, id, Side::Sell, 1000, None)
        .unwrap()
        .order_id;
    e.advance(1000);
    assert_eq!(
        order(&mut e, a, id, Side::Sell, 900, None).unwrap_err().0,
        RFQ_NOT_OPEN
    );
    assert_eq!(
        e.cancel(
            a,
            CancelOrderPayload {
                rfq_id: id,
                order_id: Some(oid),
                client_order_id: None
            }
        )
        .unwrap_err()
        .0,
        RFQ_NOT_OPEN
    );
    e.advance(2000);
    assert_eq!(
        e.accept(
            t,
            AcceptPayload {
                rfq_id: id,
                side: Side::Buy
            },
            2000
        )
        .unwrap_err()
        .0,
        NOT_AWAITING_ACCEPT
    );
    assert_eq!(e.rfqs[&id].view.state, RfqState::Expired);
    invariants(&e, initial);
}

#[test]
fn late_timer_does_not_extend_acceptance() {
    let (mut e, [t, a, _]) = world();
    let initial = totals(&e);
    let id = submit(&mut e, t, None, 0);
    order(&mut e, a, id, Side::Sell, 1000, None).unwrap();
    e.advance(9000);
    assert_eq!(e.rfqs[&id].view.accept_deadline, Some(2000));
    assert_eq!(e.rfqs[&id].view.state, RfqState::Expired);
    invariants(&e, initial);
}

#[test]
fn no_quote_reconnect_and_decline_cleanup() {
    let (mut e, [t, a, _]) = world();
    let initial = totals(&e);
    let empty = submit(&mut e, t, None, 0);
    e.advance(1000);
    let (events, _) = e.subscribe(t, empty).unwrap();
    assert!(matches!(
        events.as_slice(),
        [
            TakerEvent::Subscribed { .. },
            TakerEvent::NoQuote { .. },
            TakerEvent::Expired { .. }
        ]
    ));
    let id = submit(&mut e, t, None, 1000);
    order(&mut e, a, id, Side::Buy, 1000, None).unwrap();
    order(&mut e, a, id, Side::Sell, 1100, None).unwrap();
    e.advance(2000);
    let (events, _) = e.subscribe(t, id).unwrap();
    assert!(matches!(
        events.as_slice(),
        [TakerEvent::Subscribed { .. }, TakerEvent::Quote { .. }]
    ));
    e.decline(t, DeclinePayload { rfq_id: id }).unwrap();
    e.advance(3000);
    assert_eq!(e.rfqs[&id].view.state, RfqState::Declined);
    invariants(&e, initial);
}

#[test]
fn unaffordable_sides_are_omitted_and_taker_holds_compete() {
    let (mut e, [t, a, _]) = world();
    e.traders.get_mut(&t).unwrap().usd.total = 1000;
    e.traders
        .get_mut(&t)
        .unwrap()
        .assets
        .get_mut("SOL")
        .unwrap()
        .total = 0;
    let initial = totals(&e);
    let one = submit(&mut e, t, None, 0);
    let two = submit(&mut e, t, None, 0);
    for id in [one, two] {
        order(&mut e, a, id, Side::Buy, 900, None).unwrap();
        order(&mut e, a, id, Side::Sell, 1000, None).unwrap();
    }
    e.advance(1000);
    assert_eq!(e.rfqs[&one].view.state, RfqState::AwaitingAccept);
    assert!(e.rfqs[&one].bid.is_none());
    assert_eq!(e.rfqs[&two].view.state, RfqState::Expired);
    invariants(&e, initial);
}

#[test]
fn nonce_is_global_and_consumed_on_validly_signed_business_error() {
    let (mut e, _) = world();
    let key = Keypair::from_seed([1; 32]);
    let p = SubmitRfqPayload {
        asset: "MISSING".into(),
        direction: None,
        size: 100,
        ttl_secs: 1,
    };
    let env = Envelope::sign(&key, 0, p);
    let owner = e.authenticate(&env).unwrap();
    assert_eq!(
        e.submit(owner, env.payload.clone(), 0).unwrap_err().0,
        UNKNOWN_ASSET
    );
    let subscribe = Envelope::sign(
        &key,
        0,
        SubscribePayload {
            rfq_id: Uuid::new_v4(),
        },
    );
    assert_eq!(e.authenticate(&subscribe).unwrap_err().0, STALE_NONCE);
    let mut tampered = Envelope::sign(&key, 10, env.payload.clone());
    tampered.payload.size += 1;
    assert_eq!(e.authenticate(&tampered).unwrap_err().0, INVALID_SIGNATURE);
    assert!(e
        .authenticate(&Envelope::sign(&key, 1, env.payload))
        .is_ok());
}

#[test]
fn rounding_and_overflows_do_not_destroy_balances() {
    let (mut e, [t, a, _]) = world();
    let id = e
        .submit(
            t,
            SubmitRfqPayload {
                asset: "SOL".into(),
                direction: None,
                size: 3,
                ttl_secs: 1,
            },
            0,
        )
        .unwrap()
        .rfq_id;
    order(&mut e, a, id, Side::Sell, 101, None).unwrap();
    e.advance(1000);
    assert_eq!(
        e.accept(
            t,
            AcceptPayload {
                rfq_id: id,
                side: Side::Buy
            },
            1100
        )
        .unwrap()
        .trade
        .notional_usd,
        3
    );
    let huge = e
        .submit(
            t,
            SubmitRfqPayload {
                asset: "SOL".into(),
                direction: None,
                size: u64::MAX,
                ttl_secs: 1,
            },
            2000,
        )
        .unwrap()
        .rfq_id;
    let p = PlaceOrderPayload {
        rfq_id: huge,
        side: Side::Buy,
        price: u64::MAX,
        size: u64::MAX,
        client_order_id: None,
    };
    assert_eq!(e.place(a, p).unwrap_err().0, INVALID_SIZE);
    e.traders
        .get_mut(&t)
        .unwrap()
        .assets
        .get_mut("SOL")
        .unwrap()
        .total = u64::MAX;
    let initial = totals(&e);
    let id = submit(&mut e, t, Some(Side::Buy), 2000);
    order(&mut e, a, id, Side::Sell, 1000, None).unwrap();
    e.advance(3000);
    assert_eq!(
        e.accept(
            t,
            AcceptPayload {
                rfq_id: id,
                side: Side::Buy
            },
            3100
        )
        .unwrap_err()
        .0,
        INSUFFICIENT_BALANCE
    );
    assert_eq!(e.rfqs[&id].view.state, RfqState::Failed);
    invariants(&e, initial);
}

#[test]
fn malformed_seed_precision_is_rejected() {
    let seed = SeedConfig {
        assets: BTreeMap::from([("X".into(), AssetDef { lot_decimals: 39 })]),
        traders: vec![],
    };
    assert!(Engine::new(seed, 1).is_err());
}

#[test]
fn seeded_randomized_operations_preserve_money_and_exact_holds() {
    let (mut e, keys) = world();
    let initial = totals(&e);
    let mut rng = 0x726671u64;
    let mut ids = vec![];
    let mut now = 0;
    for _ in 0..1500 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n = rng >> 32;
        let key = keys[(n as usize / 7) % 3];
        now += n % 17;
        e.advance(now);
        if ids.is_empty() || n.is_multiple_of(7) {
            ids.push(submit(
                &mut e,
                key,
                if n.is_multiple_of(2) {
                    None
                } else {
                    Some(Side::Buy)
                },
                now,
            ));
        } else {
            let id = ids[(n as usize / 13) % ids.len()];
            match n % 7 {
                1..=3 => {
                    let _ = order(
                        &mut e,
                        key,
                        id,
                        if n.is_multiple_of(2) {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        900 + n % 300,
                        Some(n % 19),
                    );
                }
                4 => {
                    let _ = e.accept(
                        key,
                        AcceptPayload {
                            rfq_id: id,
                            side: if n.is_multiple_of(2) {
                                Side::Buy
                            } else {
                                Side::Sell
                            },
                        },
                        now,
                    );
                }
                5 => {
                    let _ = e.decline(key, DeclinePayload { rfq_id: id });
                }
                _ => {
                    let _ = e.cancel(
                        key,
                        CancelOrderPayload {
                            rfq_id: id,
                            order_id: None,
                            client_order_id: Some(n % 19),
                        },
                    );
                }
            }
        }
        invariants(&e, initial);
    }
    e.advance(now + 10_000);
    invariants(&e, initial);
    assert!(e
        .traders
        .values()
        .all(|t| t.usd.reserved == 0 && t.assets["SOL"].reserved == 0));
}

#[test]
fn lagging_broadcast_does_not_block_state_or_settlement() {
    let (mut e, [t, a, _]) = world();
    let mut slow = e.makers.subscribe();
    let initial = totals(&e);
    for _ in 0..1100 {
        submit(&mut e, t, None, 0);
    }
    let id = submit(&mut e, t, Some(Side::Buy), 0);
    order(&mut e, a, id, Side::Sell, 1000, None).unwrap();
    e.advance(1000);
    e.accept(
        t,
        AcceptPayload {
            rfq_id: id,
            side: Side::Buy,
        },
        1100,
    )
    .unwrap();
    assert!(matches!(
        slow.try_recv(),
        Err(broadcast::error::TryRecvError::Lagged(_))
    ));
    invariants(&e, initial);
}
