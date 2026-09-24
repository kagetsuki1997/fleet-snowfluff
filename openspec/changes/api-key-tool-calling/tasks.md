## 1. Message model

- [x] 1.1 Add `tool_call_id: Option<String>` to `Message` (serde default, skipped when `None`) and a `Message::tool_result(id, content)` constructor, keeping `Message::tool(content)`; verify with tests that every plain message serializes byte-identically to before and that an id round-trips. `Message.tool_call_id: Option<String>` (skipped when `None`) and `Message::tool_result(id, content)`; `Message::tool(content)` kept. 3 tests: no existing message serializes the field, the id round-trips, and a missing field deserializes to `None`.
- [x] 1.2 Have `AemeathAgentRuntime` set the call id on every tool-result message it pushes (including unknown-tool, malformed-argument, and denied-call results); verify with the existing loop tests extended to assert each result message carries the id of the call it answers. Every result path in `AemeathAgentRuntime` (executed, unknown tool, malformed arguments, denied by tier, denied at confirmation) now uses `tool_result(call.id, ..)`. The scripted-provider harness records what the model receives each round trip; one test drives all five paths and asserts each result carries its own call id.
- [x] 1.3 Verify Ollama's request is unchanged: add a test asserting its tool-path body never contains `tool_call_id`, stripping the field in its builder if the shared serialization would otherwise emit it. Ollama's tool-path builder now serializes `messages` then removes `tool_call_id`; a test with a real tool-call/tool-result exchange asserts the field never appears in the request while the result itself (`role: tool`, content) does.

- [x] 1.4 Cap `read_file`'s output at a fixed byte limit (matching `run_command`'s style: a constant plus a visible truncation marker), truncating on a character boundary; verify with tests for: a file under the cap returned unchanged, a file over the cap truncated with the marker, a multi-byte character straddling the limit not producing invalid text, and the existing file-tool tests still passing. `read_file` reads at most `MAX_READ_BYTES` (20 KiB, matching `run_command`) plus one byte to detect truncation — never the whole file — cuts on a character boundary, and appends a visible marker. Tests: under, exactly at, and over the cap; a multi-byte character straddling the cut (no U+FFFD); a non-text file and a missing file still error as before.

## 2. OpenAI-compatible tool calling

- [ ] 2.1 Add a tool-aware request builder: `tools` in OpenAI function format, and message mapping for assistant turns with `tool_calls` (arguments as a JSON string, `content` null when empty) and `tool` results with `tool_call_id`; leave `build_chat_body` untouched. Verify with tests against literal expected bodies, including a multi-call turn and a plain conversation with no tools.
- [ ] 2.2 Add the streaming accumulator: parse `delta.tool_calls[]` fragments keyed by `index`, concatenate argument pieces, and emit one complete `ToolCall` per index at `finish_reason` or end of stream (empty arguments become `{}`, unparsable arguments become a provider error). Verify with fixtures for: arguments split across many fragments, two calls in one turn, text followed by a call, an empty-argument call, and malformed argument JSON.
- [ ] 2.3 Implement `ToolCallingProvider` for `OpenAiCompatible` using 2.1 and 2.2 and the existing HTTP/SSE plumbing; verify `chat()` behavior and its existing tests are unchanged, and add an `#[ignore]`d live test that runs one tool round trip with a real key from the environment.

## 3. Anthropic tool calling

- [ ] 3.1 Add a tool-aware request builder: `tools` with `input_schema`, assistant turns as `text` plus `tool_use` blocks, and consecutive tool results merged into one user message of `tool_result` blocks; keep the plain-chat builder and its `Role::Tool` guard as they are. Verify with tests for: a single call, several calls in one turn merging into one user message, text plus call, and system prompt staying top-level.
- [ ] 3.2 Add streaming reassembly for `tool_use` blocks (`content_block_start` id/name, `input_json_delta` `partial_json` accumulation, `content_block_stop` emit; empty input becomes `{}`), beside the existing text extraction without changing what `ClaudeCodeCli` receives. Verify with fixtures for split JSON, two blocks, text then tool, and an `error` event mid-stream, and that the existing `anthropic_stream_event` tests pass unmodified.
- [ ] 3.3 Implement `ToolCallingProvider` for `Anthropic`; verify plain `chat()` is unchanged and add an `#[ignore]`d live test for one tool round trip with a real key.

## 4. Capability routing

- [ ] 4.1 Make `route_provider` return `ToolCapable` for `(OpenAi|Anthropic, ApiKey)` only when `base_url` is unset or equals the provider default (trailing slash trimmed); every other combination stays as it is. Verify by extending the existing dispatch tests: default and unset URL are tool-capable, a custom URL is plain chat, subscription profiles and Mock are plain chat, Ollama Local is still tool-capable.
- [ ] 4.2 Confirm `run_generation_with_tools` and the mix-mode fallback path work with a cloud tool-capable profile as `default_profile` (the confirmation popup, project-root scoping, streaming, and the logged final text); verify by reading the flow and with a manual run in `just dev` against a real key, recording the result.

## 5. Disclosure

- [ ] 5.1 Update `ai.disclosure.open_ai.api_key` and `ai.disclosure.anthropic.api_key` in `en`, `ja`, `ko`, `zh-Hans`, and `zh-Hant` to state that tool results are sent to the provider and that tool use may make several billed requests per message; verify with the repository's locale consistency checks and by reading each string.
- [ ] 5.2 Add a disclosure-version marker to `AiSettings` (defaulting to the pre-change version) and, on load, when below current, remove the OpenAI and Anthropic API-key entries from `acknowledged_disclosures` and raise the marker; verify with tests that it runs once, is idempotent, leaves subscription and Ollama/Mock entries alone, and treats a missing field like the old version.
- [ ] 5.3 Check how the settings UI and chat gate an enabled profile whose acknowledgement was cleared, and make the resulting state explicit and non-silent (the disclosure is shown again; chat behavior for that profile is defined); verify with a manual run and record the observed behavior.

## 6. Verification and docs

- [ ] 6.1 Run `cargo fmt`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace`; verify no new warnings and no regressions.
- [ ] 6.2 Manually verify against real keys, recording results here: an OpenAI and an Anthropic API-key profile each complete a multi-tool task; a custom-endpoint profile still chats normally with no tools sent; a `mix`-mode escalation to a cloud `default_profile` uses tools; a denied confirmation is reported back to the model.
- [ ] 6.3 Update `AI_FEATURES.md` to describe tool calling on API-key profiles, the default-endpoint condition, and that tool results are sent to the provider; verify the doc matches the shipped behavior.
