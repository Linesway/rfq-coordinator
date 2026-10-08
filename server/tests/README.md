# Read the scenarios; inspect helpers when needed

Every requirement folder has exactly one Rust file: **`scenarios.rs`**. It shows each test's moves, input amounts, prices, deadlines, and expected results. Generated tests show their case tables, ranges, and random seeds there too. Each test uses ordinary Rust functions with named traders, visible inputs, and direct assertions. Request construction, socket handling, and threading live under **`helpers/`**.

| Requirements | Open this scenario file |
| --- | --- |
| R1, R2 | [Request formats and errors](requirements/r01_r02_requests_use_the_expected_format/scenarios.rs) |
| R3, R20 | [The actual CLI and all four bots](requirements/r03_r20_cli_and_bots_work_offline/scenarios.rs) |
| R4, R5, R6, R7, R9 | [Asking for prices and completing trades](requirements/r04_r05_r06_r07_r09_ask_for_prices_and_trade/scenarios.rs) |
| R8 | [Clients receiving updates](requirements/r08_clients_receive_updates/scenarios.rs) |
| R10 | [Forged and repeated requests](requirements/r10_reject_forged_and_repeated_requests/scenarios.rs) |
| R11 | [Not promising the same funds twice](requirements/r11_cannot_promise_the_same_money_twice/scenarios.rs) |
| R12 | [Money and assets moving correctly](requirements/r12_trades_move_money_without_losing_it/scenarios.rs) |
| R13 | [Deadlines and releasing funds](requirements/r13_reject_late_requests_and_free_funds/scenarios.rs) |
| R14 | [Requests arriving together](requirements/r14_simultaneous_requests_stay_correct/scenarios.rs) |
| R15 | [Clients falling behind](requirements/r15_slow_clients_do_not_block_trades/scenarios.rs) |
| R16 | [Which error a bad request receives](requirements/r16_bad_requests_get_consistent_errors/scenarios.rs) |
| R17, R18, R19 | [Building and starting the server](requirements/r17_r18_r19_server_builds_and_starts/scenarios.rs) |

For example, `bob.offer_to_sell(request, sol(2), dollars_per_sol(150))` shows who offers what and at which price. `.expect_rejected("insufficient_balance")` checks the actual server error. The scenario creates the edge case itself; a helper performs one action or checks one result.

`sol(2)` means 2 whole SOL. `lots("SOL", 2)` means 2 smallest units; the live-server fixtures use 100 lots per SOL. `dollars(150)` means $150, and `micro_usd(1000)` means $0.001. The `engine_tests!` and `client_tests!` blocks only select which test harness runs the ordinary functions inside them.

## Run everything

From the repository root, with Rust, PowerShell, and Python 3 installed:

```powershell
./server/tests/run-tests.ps1
```

The command checks the layout and requirement map, formatting, Clippy, all tests in debug and release, real CLI/bot trades, and five deliberately broken server variants. It stops on failure.

## Run one requirement

```powershell
./server/tests/run-tests.ps1 -Requirement 10
```

Use any number from 1 to 20. Related requirements share a folder and run together. The runner rejects missing folders, extra Rust files in a requirement folder, missing/unregistered tests, and scenario macros or request plumbing added back into scenarios. Simple assertions belong in the scenarios.

Rust tests, including the actual CLI/bot scenario, also run directly:

```powershell
cargo test -p server
```

## How the helpers check results

- Engine stories use a controlled clock and check total funds and exact reservations after each move.
- HTTP/WebSocket stories run against a real local listener. Binary startup and stock-client stories launch the actual executables.
- The raw-JSON trade helper builds its own signing strings rather than using the shared message builder.
- The offer-book helper checks 5,184 cases against a separate winner and balance calculation. Generated histories check 16,000 multi-asset moves plus another 1,500-move history.
- Race helpers retain the original barriers, simultaneous requests, allowed outcomes, and balance checks.
- The CLI/bot helper starts the server, all four offline bots, and the shipped CLI. It checks expiry, directed and two-way buys/sells, every currency total, and released reservations. It stops its processes even when a check fails.

## Check that the tests catch broken code

```powershell
python ./server/tests/check-tests-catch-bugs.py
```

This changes a temporary source copy, never the working source. Each selected test must pass before the change and fail afterward. Compilation errors and zero tests do not count.

The five changes deliberately allow replay, pick the wrong price, reuse reserved funds, omit the seller's payment, or allow a request at its deadline. Results go to `target/test-sensitivity/`; stock-client logs go to `target/stock-clients-*/`.

## Limits

Finite tests are not a mathematical proof. The slow-client test forces queue overflow before checking the socket; it does not exhaustively test OS network buffers. Offline trades use local sample prices but do not simulate an outbound-blocking firewall. No hidden evaluator or exhaustive scheduler was available.
