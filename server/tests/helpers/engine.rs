macro_rules! engine_tests { ($($item:item)*) => { $($item)* }; }
macro_rules! client_tests {
    ($($item:item)*) => {};
}

use super::*;
use common::{
    seed::{AssetDef, TraderSeed},
    Keypair,
};

fn world_with_funds(cash_each: u64, sol_lots_each: u64) -> (Engine, [Pubkey; 3]) {
    let keys = [1, 2, 3].map(|v| Keypair::from_seed([v; 32]).pubkey());
    let seed = SeedConfig {
        assets: BTreeMap::from([("SOL".into(), AssetDef { lot_decimals: 2 })]),
        traders: keys
            .iter()
            .map(|key| TraderSeed {
                pubkey: *key,
                name: None,
                usd: cash_each,
                assets: BTreeMap::from([("SOL".into(), sol_lots_each)]),
            })
            .collect(),
    };
    (Engine::new(seed, 1).unwrap(), keys)
}

fn submit_sized(
    engine: &mut Engine,
    owner: Pubkey,
    direction: Option<Side>,
    now: u64,
    lots: u64,
    ttl_secs: u64,
) -> Uuid {
    engine
        .submit(
            owner,
            SubmitRfqPayload {
                asset: "SOL".into(),
                size: lots,
                ttl_secs,
                direction,
            },
            now,
        )
        .unwrap()
        .rfq_id
}

fn order_sized(
    engine: &mut Engine,
    maker: Pubkey,
    id: Uuid,
    side: Side,
    price: u64,
    client_order_id: Option<u64>,
    lots: u64,
) -> Result<PlaceOrderResponse> {
    engine.place(
        maker,
        PlaceOrderPayload {
            rfq_id: id,
            side,
            price,
            size: lots,
            client_order_id,
        },
    )
}
fn totals(engine: &Engine) -> (u128, u128) {
    engine
        .traders
        .values()
        .fold((0, 0), |(usd, asset), trader| {
            (
                usd + trader.usd.total as u128,
                asset + trader.assets["SOL"].total as u128,
            )
        })
}
fn invariants(engine: &Engine, initial: (u128, u128)) {
    assert_eq!(
        totals(engine),
        initial,
        "settlement must conserve every currency"
    );
    for (key, trader) in &engine.traders {
        assert!(trader.usd.reserved <= trader.usd.total);
        assert!(trader.assets["SOL"].reserved <= trader.assets["SOL"].total);
        let mut usd = 0u128;
        let mut asset = 0u128;
        for order in engine
            .orders
            .values()
            .filter(|order| order.active && order.maker == *key)
        {
            if order.side == Side::Buy {
                usd += order.reserved_amount as u128;
            } else {
                asset += order.reserved_amount as u128;
            }
        }
        for rfq in engine.rfqs.values().filter(|rfq| rfq.owner == *key) {
            usd += rfq.taker_reserved_usd as u128;
            asset += rfq.taker_reserved_asset as u128;
        }
        assert_eq!(
            trader.usd.reserved as u128, usd,
            "USD holds must match active commitments"
        );
        assert_eq!(
            trader.assets["SOL"].reserved as u128, asset,
            "asset holds must match active commitments"
        );
    }
}

fn ledger(engine: &Engine) -> BTreeMap<String, u128> {
    let mut result = BTreeMap::from([("cash".into(), 0)]);
    for trader in engine.traders.values() {
        *result.get_mut("cash").unwrap() += trader.usd.total as u128;
        for (symbol, balance) in &trader.assets {
            *result.entry(format!("asset:{symbol}")).or_default() += balance.total as u128;
        }
    }
    result
}

fn audit(engine: &Engine, initial: &BTreeMap<String, u128>) {
    assert_eq!(&ledger(engine), initial, "currency conservation");
    for (owner, trader) in &engine.traders {
        let mut cash = 0u128;
        let mut assets: BTreeMap<String, u128> = engine
            .assets
            .keys()
            .map(|symbol| (symbol.clone(), 0))
            .collect();
        for order in engine
            .orders
            .values()
            .filter(|order| order.active && order.maker == *owner)
        {
            let rfq = &engine.rfqs[&order.rfq];
            assert!(!rfq.view.state.is_terminal());
            if order.side == Side::Buy {
                cash += order.reserved_amount as u128;
            } else {
                *assets.get_mut(&rfq.view.asset).unwrap() += order.reserved_amount as u128;
            }
            if let Some(client_order_id) = order.client_order_id {
                assert_eq!(
                    engine.client_order_ids.get(&(*owner, client_order_id)),
                    Some(&order.id)
                );
            }
        }
        for rfq in engine.rfqs.values().filter(|rfq| rfq.owner == *owner) {
            cash += rfq.taker_reserved_usd as u128;
            *assets.get_mut(&rfq.view.asset).unwrap() += rfq.taker_reserved_asset as u128;
            if rfq.view.state.is_terminal() {
                assert_eq!((rfq.taker_reserved_usd, rfq.taker_reserved_asset), (0, 0));
            }
        }
        assert!(trader.usd.reserved <= trader.usd.total);
        assert_eq!(trader.usd.reserved as u128, cash);
        for (symbol, balance) in &trader.assets {
            assert!(balance.reserved <= balance.total);
            assert_eq!(balance.reserved as u128, assets[symbol]);
            assert_eq!(balance.available() + balance.reserved, balance.total);
        }
    }
    for ((owner, client_order_id), id) in &engine.client_order_ids {
        let order = &engine.orders[id];
        assert!(order.active);
        assert_eq!(
            (order.maker, order.client_order_id),
            (*owner, Some(*client_order_id))
        );
    }
}

fn snapshot(engine: &Engine) -> serde_json::Value {
    serde_json::to_value(engine.balances()).unwrap()
}

mod trade_inputs;
use trade_inputs::*;

mod named_traders;
use named_traders::*;

mod signature_examples;
use signature_examples::*;

mod generated_trades;
use generated_trades::*;
mod offer_books;
use offer_books::*;

#[path = "../requirements/r04_r05_r06_r07_r09_ask_for_prices_and_trade/scenarios.rs"]
mod r04_r05_r06_r07_r09_ask_for_prices_and_trade;

#[path = "../requirements/r08_clients_receive_updates/scenarios.rs"]
mod r08_clients_receive_updates;

#[path = "../requirements/r10_reject_forged_and_repeated_requests/scenarios.rs"]
mod r10_reject_forged_and_repeated_requests;

#[path = "../requirements/r11_cannot_promise_the_same_money_twice/scenarios.rs"]
mod r11_cannot_promise_the_same_money_twice;

#[path = "../requirements/r12_trades_move_money_without_losing_it/scenarios.rs"]
mod r12_trades_move_money_without_losing_it;

#[path = "../requirements/r13_reject_late_requests_and_free_funds/scenarios.rs"]
mod r13_reject_late_requests_and_free_funds;

#[path = "../requirements/r16_bad_requests_get_consistent_errors/scenarios.rs"]
mod r16_bad_requests_get_consistent_errors;
