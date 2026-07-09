# RFQ Coordinator

A Rust request-for-quote trading simulator with signed requests, competing market makers, reserved balances, and atomic settlement.

The workspace contains the coordinator (`server`), shared protocol and signing code (`common`), a trading client (`cli`), and market-maker strategies (`maker`).

## Run

Install stable Rust and PowerShell. From the repository root, use three terminals:

```powershell
./play.ps1 server
./play.ps1 makers
./play.ps1 buy 2 SOL
```

The makers use local prices by default. Add `-Live` to fetch public market prices. Trading uses simulated balances only.

```powershell
./play.ps1 sell 2 SOL
./play.ps1 quote 2 SOL
./play.ps1 balances
```

The checked-in keys and seed balances are public demonstration fixtures, not production credentials or real funds. Generate separate keys for other environments.

## Test

```powershell
cargo test -p server
./scripts/test.ps1
```

See `server/tests/README.md` for scenario coverage and the full validation workflow.
