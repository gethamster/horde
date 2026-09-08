# Native provider contract

Horde's native executor speaks an OpenAI-compatible chat protocol. Its internal
kind is currently `tuara`; a configured base URL can point to a local server.
The contract below applies within one native invocation. An invocation starts a
new conversation when a step starts or retries.

## Request history and prefix reuse

Horde serializes each message once when it enters the conversation and retains
those bytes. Subsequent requests append messages without rewriting earlier ones.
Tool definitions are also serialized once per invocation in a fixed order.
Assistant content and tool-argument strings are not parsed and rewritten on each
request. A null assistant content field is normalized to an empty string once,
before the message is stored.

New questions, mailbox notices, tool results, and completion reminders go at the
tail. No changing timestamp, turn counter, or budget notice is inserted before
existing messages. The JSON request envelope grows as messages are appended;
byte preservation applies to existing message objects and the tool definition
array, not to the entire HTTP body being a prefix of the next HTTP body.

A provider must also render the unchanged history consistently through its chat
template for the tokenized prompt prefix to match. Horde cannot guarantee that
server-side template behavior or a particular cache speedup. Different invocations
may have different context and tools and therefore start a new prefix.

## Completing a step

The initial step prompt tells the worker to finish with a JSON object containing
`result`, `accepted`, and `artifacts`, without a tool call. Code changes must be
committed and required checks must pass before acceptance. A worker that cannot
finish should explain why with `accepted=false`.

`max_identical_tool_calls` defaults to 3. After that many consecutive calls have
identical arguments and unchanged results, Horde appends a completion reminder.
If the worker requests the same call again, Horde stops before executing it and
holds the task as `blocked`. The attempt fails and file claims remain held; this
is not successful completion. Inspect the evidence before resuming. Set the limit
to 0 to disable this guard. A different call or changed result resets the count.

## Provider request options

Native providers accept an optional `extra_body` table. Fields are merged at the
top level of every model request. The provider decides which sampling and template
options it supports. For example:

```toml
[providers.local]
kind = "tuara"
auth_mode = "api"
base_url = "http://127.0.0.1:8122/v1"
api_key_env = "LOCAL_MODEL_KEY"
model = "auto"
stream = true

[providers.local.extra_body]
temperature = 0.2
top_p = 0.9
repetition_penalty = 1.1

[providers.local.extra_body.chat_template_kwargs]
enable_thinking = false

[executors.worker]
provider = "local"
max_tokens = 2048
```

`model`, `messages`, `tools`, `stream`, `max_tokens`, `max_price`, and `n` are
reserved in `extra_body`. Use the dedicated setting where one exists. In
particular, the existing per-role `max_tokens` setting takes effect normally.
These options do not modify requests made internally by external CLI harnesses.
Never put a key value in `extra_body`; use the provider's key environment variable.

With `model = "auto"`, Horde requires exactly one entry with a nonempty ID from
`/models`. Zero entries or multiple entries produce an error listing the catalog IDs and
asking for an explicit choice. Horde resolves the ID once per invocation and records it in an
`executor.model_resolved` event. A path-shaped model ID works without copying it
into every configuration. Explicit IDs continue to require an exact catalog match.

A provider with `model = "auto"`, an endpoint, and a key-variable name selects the
native executor when `kind` is omitted. Named providers and the built-in default
can both use only these three fields:

```toml
[providers.default]
base_url = "http://127.0.0.1:8122/v1"
api_key_env = "LOCAL_MODEL_KEY"
model = "auto"
```

`horde doctor` reports the selected ID under `resolved_models.default.model`.

## Streaming and probes

Set `stream = true` on a native provider to decode SSE as it arrives. The default
remains false for existing provider configurations. The parser assembles tool calls
by index across network chunks and preserves content and reasoning fields.
`executor.progress` emits `first_token` when output starts and `tool_intent` when
a registered tool name becomes available. Arguments and text fragments are not
included in those events. A complete `[DONE]` marker is required before any tool
from that turn executes. Interrupted or oversized responses fail the attempt.

Run `horde doctor --probe --provider local` to verify the catalog and request a
small streamed `ping` call using the executor's parser. Omitting `--provider` probes
`default`. `--probe-tuara` remains an alias for probing the `native` role. Ordinary
`doctor` prints configuration and resolves native `auto` catalogs without making
a chat-completions request. Use `doctor --provider NAME` to inspect one catalog. A probe consumes model
capacity and must be requested explicitly.

## Events and metrics

Each `model_response` progress event includes the turn number, latency, resolved
model, provider usage, and extra response fields. Attempts retain the same per-turn
records under `usage.requests`; `horde metrics` exposes them under `turns` alongside
token totals. Unknown usage fields, including prefix reuse and speculation counts,
are retained without assuming their names or units. Standard prompt, completion,
and cached-token fields contribute to existing totals. Missing costs remain unknown.

Some servers only emit streaming usage when requested. If supported, set
`extra_body.stream_options = { include_usage = true }`. Horde does not send this
field automatically because some compatible servers reject it.

`tool.completed` includes `result_summary` and `result_truncated` for successful
calls, alongside the existing error fields. The summary is limited to 2,048 UTF-8
bytes. A successful call means the tool returned normally; a command's nonzero exit
code may still appear in its result. These summaries do not shorten the result
sent back to the model. Telemetry strings are redacted before encoding; unavailable
application secrets cause diagnostic content to be withheld. Usage and extra-field
objects each have a 64 KiB diagnostic limit. No assistant transcript is copied into
the usage record.
