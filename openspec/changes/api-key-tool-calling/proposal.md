## Why

Aemeath's tool-calling agent loop only runs for Ollama. An OpenAI or Anthropic API-key profile is plain chat, so a user whose `default_profile` is one of those gets no native tools even though `mix` mode escalates tool-needing requests to it by design. Stage 3 deferred these two providers as a fast-follow because each has its own streaming tool-call wire format; Stage 4's remaining useful work is closing that gap so an escalated task lands somewhere that can actually use the tools.

## What Changes

- Extend the conversation message model so a tool result can be tied to the call it answers (`tool_call_id`), which both OpenAI and Anthropic require and Ollama ignores.
- Implement tool calling for `OpenAiCompatible` (request `tools`, `tool_calls` on assistant turns, `tool` role results, and reassembly of fragmented `delta.tool_calls[].function.arguments` in the stream) and for `Anthropic` (request `tools`, `tool_use` content blocks with `input_json_delta` reassembly, and `tool_result` blocks carried in a user turn), each behind the existing `ToolCallingProvider` trait, sequenced OpenAI first, Anthropic second, in this one change.
- A profile is tool-capable only when its provider brand and auth method support it **and** its endpoint is the provider's own default. An API-key profile with a custom `base_url` (OpenRouter, vLLM, LM Studio, proxies) stays plain chat exactly as today, so existing users of such endpoints are not broken by a request field their server may reject.
- Tool results — file contents, command output, search results — are now sent to a cloud provider when one of these profiles runs the agent loop. Permission tiers are unchanged (a read inside the project directory is still automatic, consistent with Claude Code's read tools). The cloud-provider disclosure text for the two API-key profiles is updated to say this, and existing acknowledgements for those two profiles are cleared once so the updated disclosure is shown again.
- Bound `read_file`'s output the way `run_command`'s already is (it currently returns a whole file with no cap, which was tolerable when the content stayed local but is unbounded cost, context overflow risk, and data leaving the machine once the loop talks to a cloud API). This also applies to Ollama.
- Tool use can multiply requests billed to an API key (up to the loop's iteration limit, each resending the conversation), so the disclosure for the two API-key profiles also says so.
- Explicitly not in this change: tool use for custom endpoints (a per-profile opt-in is a possible later addition), a file-writing native tool, letting `mix` mode's local attempt run the tool loop, loop-level escalation, and capability-aware routing between profiles. The known limit that `ESCALATE` lands on `default_profile` even when that profile cannot perform the task is recorded in the design, not fixed here.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `agent-runtime`: "Native tool registry" gains a bound on the size of what the read-file tool returns.
- `ai-provider`: "Provider tool-calling capability" now includes OpenAI and Anthropic API-key profiles on their default endpoints, and states that a profile's capability depends on its endpoint as well as brand and auth method. "Cloud provider data disclosure" now covers tool results and re-presents the disclosure for the two API-key profiles once.

## Impact

- `crates/fleet-snowfluff-ai/src/message.rs`: `Message.tool_call_id` (omitted from serialization when absent).
- `crates/fleet-snowfluff-ai/src/providers/openai.rs`, `providers/anthropic.rs`, `providers/anthropic_stream_event.rs`: tool request builders, message mapping, and streaming reassembly; both implement `ToolCallingProvider`.
- `crates/fleet-snowfluff-ai/src/agent_runtime.rs`: tool-result messages carry the call id.
- `crates/fleet-snowfluff-ai/src/native_tools/file_tools.rs`: `read_file` output cap with a truncation marker.
- `crates/fleet-snowfluff/src/ai_commands.rs`: `route_provider` returns `ToolCapable` for the two API-key profiles on default endpoints.
- `crates/fleet-snowfluff-ai/src/settings.rs`: a disclosure-version marker driving the one-time re-acknowledgement.
- `locales/{en,ja,ko,zh-Hans,zh-Hant}.json`: updated `ai.disclosure.open_ai.api_key` and `ai.disclosure.anthropic.api_key` text.
- No new dependencies. `AiProvider::chat()` is unchanged; Ollama's wire format must remain byte-identical.
