# Routing v1 fixture report

The routing v1 fixture set defines the offline contract for Horde's initial shadow integration. It separates one calibration example from held-out examples and records expected policy behavior rather than model accuracy claims.

The test suite verifies request and response validation, endpoint restrictions, retry and deadline behavior, operator-only configuration, durable interruption, cache scoping, inspection pagination, and metrics. The missing-guidance fixture requires a local abstention with no provider request. The security example supplies a known classification target for future opt-in evaluations.

These fixtures cannot justify active routing. Promotion requires a separately recorded evaluation on representative Horde tasks with frozen thresholds and held-out outcomes.

## Tuara integration smoke, September 22, 2026

The final smoke passed against revision `e8f5eba` with the score-legend
compatibility fix in this checkout. Task `1057f030-c09f-47e5-807b-483996edc54b`
used a local Codex worker and Jev model `XXXXTSJV130XXX` through
`https://tuara.com/router/v1/systemone`. The worker committed and integrated
`hello.txt` with exactly `hello` plus a newline, leaving the original checkout
untouched and the integrated worktree clean.

All five Jev requests succeeded on their first attempt: routing, plan review,
patch review, integration review, and final review. The plan review abstained;
the other decisions did not. Every decision remained advisory and unapplied.
Final, patch, and integration reviews reported complete coverage and fresh
evidence. The earlier plan review was no longer fresh after the commit changed.
Decision usage totaled 6,406 input tokens and 775 output tokens; provider cost
was not reported. The daemon stopped after saving the result, with delivery
disabled.

The live response exposed a Horde compatibility bug: score legends arrived as
objects such as `{"0":"routine","1":"moderate","2":"complex"}`, while Horde
accepted only arrays. The validator now accepts either representation and still
requires exact indices and labels. Probability and weighted-score checks remain
unchanged. Unit tests and the daemon smoke against a mock provider reproduced
the failure before the fix and passed afterward.

This run validates the small fixture's routing and advisory review flow. It does
not establish decision quality, active routing, browser actions, context pruning,
or automatic-delivery qualification. The successful run used Codex for coding;
Tuara's separate coding-provider failure below was not retested.

### Earlier attempts

Initial live validation against Horde revision `e8f5eba` did not pass. Requests reached
`https://tuara.com/router/v1/systemone` with the TypeSafe-compatible SystemOne
contract. After replacing a rejected credential, Tuara returned HTTP 400 with
`provider_protocol_mismatch`. Its error identified the selected upstream as
`https://inference.baseten.co/v1/chat/completions`. Both `jev-1.13.0` and the live
catalog identifier `XXXXTSJV130XXX` produced this response.

Two isolated daemon runs retained the failure evidence:

| Task | Worker result | Decision result |
| --- | --- | --- |
| `251b0d31-c862-4285-96b5-551b39163c6d` | Tuara worker using `XXXXQWQW38SXXX` failed with HTTP 404 | Routing and three reviews failed with `provider_unavailable` |
| `be0c7388-7b4c-43c2-bdaa-903f3c1fa6b0` | Local Codex worker committed and integrated `hello.txt` containing exactly `hello` plus a newline | Routing and four reviews failed with `provider_unavailable` |

Each failed decision recorded one provider attempt and remained unapplied. The
Codex run confirms that advisory failures allow ordinary execution and integration
to finish. The smoke harness still exited unsuccessfully because Jev did not
return a valid decision. No live decision-quality or cost conclusion follows from
these runs. Both daemons stopped after saving their evidence; delivery was disabled.

A subsequent retry against `/router/v1/systemone` failed with HTTP 502 for both
model identifiers. Tuara now identified `aiplatform.googleapis.com` as the provider
and returned `seller_protocol_mismatch`: `seller speaks anthropic_messages, not
decision`. The upstream selection changed, but a successful decision response
remained unavailable. The full daemon smoke was not repeated after this failed
preflight.

## Repeat the smoke

Build Horde and export an active Tuara inference key as `TUARA_API_KEY`, then run:

```sh
cargo build --locked
python3 scripts/live_smoke.py codex \
  --decision-base-url https://tuara.com/router \
  --decision-model XXXXTSJV130XXX \
  --decision-review
```

The decision base URL intentionally ends at `/router`: Horde appends
`/v1/systemone`. Use the Tuara catalog ID: a later probe returned HTTP 200 for
`XXXXTSJV130XXX`, while `jev-1.13.0` returned HTTP 402 `no_fill_at_limit`.
The Codex worker uses its existing local login. To test Tuara's
coding provider as well, replace `codex` with `tuara --model MODEL_ID`, selecting
an available coding model from Tuara's current catalog.

The harness creates a temporary repository and isolated administrator
configuration. It enables shadow decisions with a 30-second deadline, one attempt
per request, and at most 16 decisions per task. It waits for the final review and
requires successful provider responses for routing and final evidence. Local
abstentions cannot satisfy those checks. The printed evidence directory contains
`result.json`, `events.json`, `metrics.json`, `decisions.json`, `reviews.json`, and
`summary.json`, including when validation fails. Keep runtime databases and worker
transcripts outside the repository.

Offline verification passed formatting and Clippy checks, plus 692 Rust tests
with two ignored tests. The Rust suite required an empty `XDG_CONFIG_HOME` because
two default-setting tests otherwise read the installed user configuration. Nine
Python smoke tests passed, including a real daemon against mock chat and decision
servers, delayed final reviews, provider errors, and skipped-final-review rejection.
These checks supplement the successful live Jev smoke described above.
