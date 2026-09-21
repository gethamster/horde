# Jev integration across Horde’s development lifecycle

Status: Proposed implementation plan; no implementation or live benchmarks completed.

Research date: September 19, 2026. Repository: [gethamster/horde](https://github.com/gethamster/horde), inspected at commit `de239089928c7d0f1eb0b296a99afc140d076fe6`.

## Summary

Add Jev as a fast decision service alongside Horde’s coding models. Use it to select suitable models, review work products, choose bounded next actions, and reduce unnecessary context and model calls.

Start with routing and review. Extend into native execution, browser testing, and automatic delivery as each capability passes evaluation. Optimize verified completion time while preserving the existing quality baseline and configured spending limits.

Scope covers Horde’s work, including its resulting PRs. It excludes external PR intake, repository watchers, and a new general-purpose desktop agent.

## What the sources establish

| Source | Underlying mechanism | Application to Horde |
| --- | --- | --- |
| [Paolo’s PR review demo](https://x.com/redp314/status/2100585126652481915) | Fourteen typed checks evaluate a diff in one request; code combines probabilities into routing/verdicts and escalates uncertainty. The post does not link implementation source. | Batch review signals, then route findings to appropriate reviewers. Its reported cost and speed do not establish defect-detection quality. |
| [Sydney’s harness article](https://x.com/sydneyrunkle/status/2100754364545761643) and [LangChain implementation documentation](https://docs.langchain.com/oss/python/integrations/providers/typesafe) | Separate middleware chooses a model for a run and classifies selected tool calls before execution. | Add decision hooks at explicit lifecycle boundaries. Keep model routing separate from permission enforcement. |
| [ctatedev’s UI experiment](https://x.com/ctatedev/status/2101022101750571357) and [json-render composition source](https://github.com/vercel-labs/json-render/blob/main/packages/core/src/experimental-compose.ts) | Jev chooses among application-owned component recipes and valid placements; code validates the resulting structure. | Apply the constrained-catalog pattern to workflow templates, repair strategies, skills, and test suites. A new Horde UI is unnecessary. |
| [Jared Palmer’s Kev](https://github.com/jaredpalmer/kev) | Qwen backbone, LoRA, isolated question branches, and a trained pointer head return option probabilities without text decoding. Exposes a TypeSafe-compatible API. | First portability candidate. Qualify each checkpoint on Horde tasks; published results show meaningful domain-transfer limitations. |
| [OpenJev, now SemIf](https://github.com/TheoLeeCJ/SemIf) | Reads allowed answer-token logits from frozen open models; shared-prefix execution amortizes repeated state. | Useful baseline for local inference and batching. Its probabilities require workload-specific calibration. |
| [James Ward’s proposal](https://x.com/JamesWard/status/2100745626560680292) | Put classification around the outer loop and invoke generation as an inner operation. | Introduce a bounded native controller that delegates coding and difficult reasoning to existing executors. |
| [Bespoke Nimble](https://github.com/bespokelabsai/nimble) | Contrastive synthetic training examples, Qwen LoRA, and constrained candidate scoring. | Evaluate as another private inference backend. Its narrow published evaluation and shorter input limits prevent assuming interchangeability with Jev. |
| [Fast Jev compaction](https://github.com/tamaratran/fast-jev-compaction) | Scores whether each tool call and result remains useful; removes pairs or truncates results while retaining selected text verbatim. | Adapt selective pruning with durable retrieval and protected context. Removing content is still lossy, even when retained content stays verbatim. |
| [Browser Use Ultrafast](https://github.com/browser-use/jev-ultrafast) | Batches operation and compatible target choices over an indexed DOM observation; a generative model supplies text. Execution checks target freshness. | Fast browser-test execution over observed actions, with independent assertions. |
| [Computer-use post](https://x.com/0xidanlevin/status/2100937437325205568) and [WindTunnel source](https://github.com/nekuda-ai/WindTunnel/tree/main/experiments/jev) | Jev selects website tools; Mercury generates arguments. The benchmark checks resulting application state. | Prefer structured application tools when available. The reported 49/49 versus 25/49 result compares complete benchmark configurations, not an isolated model improvement. |

Jev’s documented interface is `POST /v1/systemone`, with `Choice`, `Score`, and `Noul` questions. It accepts text and structured text, not screenshots. Independent questions can share one request. Pin `jev-1.13.0` initially and enforce its documented request limits. [API reference](https://docs.typesafe.ai/api), [model specifications](https://docs.typesafe.ai/models)

TypeSafe documents weaknesses with indirection, irrelevant context, arithmetic, and adversarial content. Consequently, Horde should ask narrow questions and retain executable policy checks in Rust. [Documented limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13)

## Architecture and interfaces

### A dedicated decision service

- Implement a Rust `DecisionBackend` interface with a TypeSafe HTTP adapter. Jev cannot use Horde’s existing chat-completions executor directly.
- Give the service no worker token, shell access, or mutation tools. Runtime code constructs permitted choices and executes accepted decisions.
- Support typed questions, validated probability distributions, explicit abstention, bounded batching, cancellation, and per-purpose deadlines.
- Reuse HTTP connections. Retry transient failures within the decision deadline; decision retries must not consume coding-worker attempts.
- Pin backend, model version, question catalog, thresholds, and policy with each task. Keep authorization ceilings in operator-controlled configuration.
- Add task-scoped decision inspection through CLI/MCP and include decision summaries in existing metrics and events. Evaluation accepts registered purposes and evidence references; workers cannot submit replacement authorization policies.

### Durable evidence and compatibility

Persist every decision’s purpose, input hashes, context version, model identity, policy version, result, timing, usage, and applied outcome. Store larger evidence in Horde’s existing artifact store.

Cache only identical evidence/model/policy combinations. Revalidate changing facts, including capacity, claims, PR heads, and checks, before action.

Horde currently maps all agent roles in an explicitly selected task to one pinned capability. Preserve that contract: use separately selected child tasks for different coding/review models, and keep the decision service separately configured. [Execution selection](https://github.com/gethamster/horde/blob/de239089928c7d0f1eb0b296a99afc140d076fe6/src/execution_selection.rs#L382)

Use additive persistence changes. Existing tasks retain their pins; disabling the feature preserves current behavior. Decision-service failure falls back to an already authorized conventional path, while delivery gates hold.

## Implementation sequence

### 1. Establish measurement and ship routing in shadow mode

- Build versioned evaluation fixtures from representative Horde objectives, changes, failures, and outcomes. Separate calibration data from held-out evaluation.
- Ask atomic questions about task difficulty, ambiguity, risk, and required capabilities. Compute prices, quotas, deadlines, and capacity in code.
- Rank only the permitted, available runtime/capability pairs using observed completion outcomes and operator-provided guidance.
- Record proposed selections alongside existing selections before allowing automatic routing.
- Once qualified, select and pin the executor before dispatch. Explicit user selections remain authoritative; uncertain classifications use the configured conventional route.
- Extend metrics with decision overhead, routing errors, escalation rates, and total time/cost per verified result.

### 2. Review every relevant Horde work product

- Add review checkpoints for plans, completed patches, integration results, test failures, and final delivery evidence.
- Batch independent checks for scope adherence, missing requirements, suspicious changes, test adequacy, API compatibility, and security-sensitive areas.
- Build review inputs from the complete change manifest plus relevant source context. Track coverage explicitly; oversized or unsupported portions cannot silently count as reviewed.
- Use Jev to prioritize findings and select specialist review. Generative reviewers investigate causes, validate findings, and propose repairs.
- Bind every finding and review result to exact code and evidence hashes. Any subsequent edit invalidates affected review evidence.
- Require independent generative review before automatic delivery. Jev scores alone cannot establish correctness or security.
- Classify failures into configured recovery paths: gather evidence, repair, choose a stronger permitted model, or ask a durable question. Keep retry budgets and uncertain-effect reconciliation authoritative.

“Review everything” means coverage at meaningful boundaries, not a synchronous model request after every token or harmless coordination operation.

### 3. Add native context pruning and bounded loop decisions

Horde’s native conversation currently preserves an append-only serialized prefix. Compaction must create a new conversation epoch within the same attempt, preserving deadlines, ownership, and counters. [Native conversation representation](https://github.com/gethamster/horde/blob/de239089928c7d0f1eb0b296a99afc140d076fe6/src/native_protocol.rs#L9)

- Store and verify original tool content before pruning it from active context. Recover omitted content by artifact reference rather than repeating side-effectful commands.
- Protect mandatory instructions, authoritative context, unresolved questions, pending tool calls, recent failures, and mutation receipts.
- Score only eligible completed tool exchanges. Preserve call/result pairing and retain uncertain candidates.
- Compact only when expected context savings justify the request and lost prefix-cache benefit. If compaction fails, retain the valid conversation or stop explicitly when it cannot fit.
- Introduce a controller that chooses among registered operations such as retrieve context, execute a prepared check, delegate implementation, request review, or escalate.
- Let generative models produce code and open-ended arguments. Validate fully assembled tool calls before classification and execution; streamed tool intent is insufficient.
- Use the same catalog approach for skill selection, evidence retrieval, and choosing existing workflow/repair templates.

External Codex and Claude executors initially benefit at task and artifact boundaries. Internal-loop intervention requires a separately tested supported adapter; Horde must not rewrite their private session formats.

### 4. Accelerate browser testing

- Package the hybrid browser runner as a test command inside Horde’s existing disposable application environments.
- Prefer declared application/WebMCP tools; otherwise offer supported actions over an observed DOM snapshot.
- Batch operation and target questions when independent. Generate free-text arguments only when required.
- Validate action IDs, argument schemas, origin restrictions, target freshness, and execution budgets before acting.
- Treat a model’s `DONE` decision as a request to run independent assertions. Record application-state checks, traces, and screenshots against the tested commit.
- Unsupported controls or repeated stalls escalate to the configured existing browser executor.
- Keep screenshot interpretation with a vision-capable verifier. Jev can classify extracted textual evidence.

### 5. Enable bounded automatic delivery

Horde currently observes deployments triggered by a merge; it does not independently dispatch production releases. Its existing health check also does not verify deployment identity. [Delivery implementation](https://github.com/gethamster/horde/blob/de239089928c7d0f1eb0b296a99afc140d076fe6/src/delivery.rs#L34)

- Add an explicitly configured, root-only delivery policy naming allowed repositories, branches, environments, required checks, and review requirements.
- Permit automatic routine delivery only after deterministic eligibility checks and qualified Jev decisions. Security-sensitive changes, migrations, consequential API changes, uncertainty, and missing evidence escalate.
- Bind authorization to the exact PR head, integrated commit, context version, checks, and review artifacts. Revalidate immediately before merge.
- Preserve merge intent recording, exact-head matching, and external-state reconciliation.
- For push-triggered deployment, authorization must cover the combined merge-and-deploy effect.
- Add a separately triggerable release adapter where independent production gating is required. Pin the immutable release artifact and record dispatch identity before execution.
- Verify deployed version and application-specific smoke checks. Roll back only through a configured compatible action targeting a known-good release; otherwise hold and notify.

### 6. Qualify portable backends

Use the same Horde evaluation suite for hosted Jev, Kev, SemIf, and Nimble.

Start with Kev’s compatible endpoint; add adapters for the others’ differing schemas and limits. Record checkpoint, prompt, hardware, and precision. Qualify each backend separately for each purpose.

A backend change never inherits another backend’s thresholds or delivery authority automatically. Training a Horde-specific model remains outside this implementation; retained evaluation data can support that later.

## Validation and rollout defaults

- Default configuration is disabled. Enabled purposes progress through shadow, advisory, and automatic operation independently.
- Select thresholds on calibration data, then freeze them for held-out evaluation. Raw confidence is not a measured probability that shipping is safe.
- Promotion requires preserved baseline completion quality and critical-defect detection, with lower end-to-end verified-completion latency after decision overhead, retries, and escalations.
- Test malformed responses, missing answers, invalid probabilities, rate limits, outages, budget exhaustion, and oversized inputs.
- Test disallowed/stale model choices, changed context, lost claims, injected instructions, and incomplete tool arguments.
- Test compaction recovery, exact artifact retrieval, protected requirements, tool pairing, and interruption during epoch creation.
- Test browser false-success reports, stale targets, unsupported controls, timeouts, and environment cleanup.
- Test child delivery rejection, stale PR heads, missing checks, mismatched review evidence, duplicate dispatch prevention, and restart after every external-write boundary.
- Use temporary repositories and mocked external writes. Live comparisons are explicit opt-in runs with configured spending caps.
- Run `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`; cover new decision and policy code to the repository’s 80% requirement.

Research inspected source and documentation; it did not run paid model benchmarks or validate the posts’ performance claims.
