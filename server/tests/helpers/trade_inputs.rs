use common::types::Side;

pub fn dollars(amount: u64) -> u64 {
    amount
        .checked_mul(1_000_000)
        .expect("dollar amount must fit")
}

#[derive(Clone, Copy, Debug)]
pub struct Coins {
    pub asset: &'static str,
    pub amount: u64,
    pub whole_coins: bool,
}

pub fn sol(amount: u64) -> Coins {
    Coins {
        asset: "SOL",
        amount,
        whole_coins: true,
    }
}
pub fn lots(asset: &'static str, amount: u64) -> Coins {
    Coins {
        asset,
        amount,
        whole_coins: false,
    }
}
pub fn dollars_per_sol(amount: u64) -> u64 {
    dollars(amount)
}
pub fn micro_usd(amount: u64) -> u64 {
    amount
}

#[derive(Clone, Copy)]
pub struct PriceRequest {
    pub coins: Coins,
    pub direction: Option<Side>,
    pub ttl_secs: u64,
}

pub fn buy(coins: Coins) -> PriceRequest {
    PriceRequest {
        coins,
        direction: Some(Side::Buy),
        ttl_secs: 1,
    }
}
pub fn sell(coins: Coins) -> PriceRequest {
    PriceRequest {
        coins,
        direction: Some(Side::Sell),
        ttl_secs: 1,
    }
}
pub fn both_prices(coins: Coins) -> PriceRequest {
    PriceRequest {
        coins,
        direction: None,
        ttl_secs: 1,
    }
}
impl PriceRequest {
    pub fn quoting_for(mut self, seconds: u64) -> Self {
        self.ttl_secs = seconds;
        self
    }
}
