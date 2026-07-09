# RFQ coordinator recipes.

port := "4402"
server_url := "http://127.0.0.1:" + port

# List recipes.
default:
    @just --list

# Run YOUR server implementation.
server:
    cargo run --release -p server -- --seed seed.toml --port {{port}}

# Run one stock maker (tight | lazy | skew | sniper). Extra flags pass through.
maker strategy *flags:
    cargo run --release -p maker -- --server {{server_url}} --key keys/maker-{{strategy}}.json --strategy {{strategy}} {{flags}}

# Run all four stock makers against a running server (Ctrl-C stops them all).
makers *flags:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -p maker
    trap 'kill 0' EXIT
    for s in tight lazy skew sniper; do
        target/release/rfq-maker --server {{server_url}} --key keys/maker-$s.json --strategy $s {{flags}} &
    done
    wait

# Submit an RFQ as the taker, e.g. `just submit --asset SOL --size 2`.
submit *args:
    cargo run --release -p cli -- --server {{server_url}} --key keys/taker.json submit {{args}}

# Show everyone's balances.
balances:
    cargo run --release -p cli -- --server {{server_url}} balances

