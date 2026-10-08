"""Verify selected tests detect deliberate bugs in an isolated source copy."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
LOGS = ROOT / "target" / "test-sensitivity"
# Mutated binaries must never replace the normal workspace's test executables.
BUILD_DIRECTORY = LOGS / "build"


@dataclass(frozen=True)
class Mutation:
    name: str
    source: str
    before: str
    after: str
    test: str


CASES = [
    Mutation(
        "repeated_nonce_is_accepted", "engine.rs",
        "envelope.nonce <= last_nonce", "envelope.nonce < last_nonce",
        "nonce_is_global_and_consumed_on_validly_signed_business_error",
    ),
    Mutation(
        "most_expensive_ask_wins", "engine/windows.rs",
        "order.price < previous.price", "order.price > previous.price",
        "cheapest_seller_wins_and_equal_prices_use_the_first_offer",
    ),
    Mutation(
        "reserved_money_can_be_spent_again", "engine.rs",
        "self.total - self.reserved", "self.total",
        "maker_cannot_offer_the_same_sol_twice",
    ),
    Mutation(
        "seller_does_not_receive_payment", "engine/settlement.rs",
        "self.traders.get_mut(&seller).unwrap().usd.total += cash;",
        "self.traders.get_mut(&seller).unwrap().usd.total += 0;",
        "cheapest_seller_wins_and_equal_prices_use_the_first_offer",
    ),
    Mutation(
        "requests_at_deadline_are_allowed", "engine/windows.rs",
        "if deadline > now {", "if deadline >= now {",
        "an_action_exactly_at_its_deadline_is_too_late",
    ),
]


def copy_workspace(destination):
    for name in ["Cargo.toml", "Cargo.lock"]:
        shutil.copy2(ROOT / name, destination / name)
    for name in ["common", "cli", "maker", "server"]:
        shutil.copytree(
            ROOT / name, destination / name,
            ignore=shutil.ignore_patterns("target", ".git"),
        )


def run_test(workspace, mutation, *, expect_failure):
    name = mutation.name if expect_failure else mutation.name + "_baseline"
    log = LOGS / f"{name}.log"
    result = subprocess.run(
        ["cargo", "test", "--offline", "-p", "server", "--lib", mutation.test,
         "--", "--nocapture"],
        cwd=workspace,
        env={**os.environ, "CARGO_TARGET_DIR": str(BUILD_DIRECTORY)},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=300,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    log.write_text(result.stdout, encoding="utf-8")
    expected_summary = (
        "test result: FAILED. 0 passed; 1 failed;" if expect_failure
        else "test result: ok. 1 passed; 0 failed;"
    )
    if (result.returncode != 0) != expect_failure or expected_summary not in result.stdout:
        outcome = "failing" if expect_failure else "passing"
        raise RuntimeError(f"{name}: expected one {outcome} test; see {log}")


def check_mutation(workspace, mutation):
    source = workspace / "server" / "src" / mutation.source
    original = source.read_text(encoding="utf-8")
    if original.count(mutation.before) != 1:
        raise RuntimeError(f"{mutation.name}: update the exact mutation target")
    run_test(workspace, mutation, expect_failure=False)
    try:
        source.write_text(original.replace(mutation.before, mutation.after), encoding="utf-8")
        run_test(workspace, mutation, expect_failure=True)
    finally:
        source.write_text(original, encoding="utf-8")


def main():
    LOGS.mkdir(parents=True, exist_ok=True)
    report = {"status": "running", "caught": []}
    report_path = LOGS / "results.json"
    report_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    try:
        with tempfile.TemporaryDirectory(prefix="rfq-test-sensitivity-") as directory:
            workspace = Path(directory)
            copy_workspace(workspace)
            for mutation in CASES:
                check_mutation(workspace, mutation)
                report["caught"].append(mutation.name)
                print(f"CAUGHT: {mutation.name}", flush=True)
        report["status"] = "passed"
    except BaseException:
        report["status"] = "failed"
        raise
    finally:
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"PASS: all {len(CASES)} deliberate bugs were caught. Logs: {LOGS}")


if __name__ == "__main__":
    main()
