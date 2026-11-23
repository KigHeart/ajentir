# AJENTIR Trading

**Status: initial domain foundation. No broker execution or profitability claims.**

## Product definition

AJENTIR Trading is a multi-agent financial-market application built on AJENTIR's independent Rust runtime. Agents research markets, formulate competing hypotheses, evaluate strategies, assess portfolio consequences and propose actions. The runtime coordinates their work; a separate authority boundary decides whether an action is permitted.

It is not a prompt-driven stock picker or a monolithic trading bot.

## Responsibilities

1. **Market data:** timestamped, source-identified observations with event time, receive time, provenance and correction history.
2. **Research agents:** analyse filings, news, fundamentals, market structure and macro conditions. Their outputs are evidence-bearing claims, not orders.
3. **Hypothesis agents:** propose falsifiable opportunities with horizon, universe, mechanism, invalidation conditions and calibrated confidence.
4. **Strategy evaluation:** point-in-time, walk-forward and out-of-sample testing; account for fees, spread, slippage, market impact, funding, inference and infrastructure costs.
5. **Portfolio agents:** evaluate concentration, correlation, liquidity, drawdown, exposure and allocation.
6. **Risk authority:** independently validate limits and permissions. Agent consensus never grants authority.
7. **Execution adapters:** eventually connect broker APIs behind explicit permits, idempotency, durable effect intents and reconciliation.
8. **Evaluation:** preserve decisions, orders, fills, fees and mark-to-market evidence. Report net returns, drawdown, benchmarks and uncertainty.

## Implemented first slice: aj-trading

- Validated currency, instrument, order and strategy identifiers.
- Integer-only money arithmetic for whole-unit limit orders.
- Immutable portfolio snapshots.
- Pure pre-trade risk evaluation: per-order notional, gross exposure, open-order count and long-position checks.

Not implemented: feeds, alpha models, broker adapters, fractional quantities, derivatives, FX conversion, fee/slippage models or order submission. Passing risk evaluation is not execution authorisation.

## Implementation sequence

- **T0 — Domain contracts:** complete initial crate; deterministic and side-effect free.
- **T1 — Market data:** normalized events, timestamps, provenance, replay and stale-data detection.
- **T2 — Research and hypotheses:** independent agents, typed evidence and competing explanations in AJENTIR state.
- **T3 — Strategy evaluation:** point-in-time datasets, walk-forward splits, realistic costs and reproducibility.
- **T4 — Portfolio and risk:** exposure aggregation, regime invalidation and hard risk authority.
- **T5 — Durable execution:** effect ledger, approvals, fencing, idempotency and broker reconciliation.
- **T6 — Paper trading:** live data, simulated fills, latency and failure testing.
- **T7 — Controlled live pilot:** only after independent review, explicit capital limits, kill switch and auditable broker integration.

## Non-negotiable rules

- No agent can grant itself trading authority.
- No model output is an order; it is a proposal requiring typed validation.
- No backtest is accepted without point-in-time data and realistic costs.
- No strategy is promoted on in-sample performance.
- No live execution before durable effect intent, idempotency, reconciliation, fencing and kill-switch semantics exist.
- Missing, stale, contradictory or unpriced data fails closed.
- Record rejected proposals as well as accepted ones.
- Volume and agent activity are not profitability.

## Open research questions

- How do independent agents avoid correlated evidence and consensus cascades?
- Which observations add information beyond standard quantitative features?
- How quickly does strategy validity decay across regimes?
- Can agents detect stale or insufficient data?
- Does agentic research improve net-of-cost out-of-sample returns over simpler baselines?
- What is each agent's marginal value after inference and infrastructure costs?
