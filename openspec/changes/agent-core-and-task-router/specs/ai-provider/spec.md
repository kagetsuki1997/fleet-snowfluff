## ADDED Requirements

### Requirement: Provider tool-calling capability

A provider implementation MAY additionally support tool calling: accepting a set of tool definitions alongside a chat request and returning, for each turn, either further assistant text or a request to invoke one of the supplied tools. A provider that does not support tool calling SHALL continue to support ordinary chat unchanged. Whether a given provider profile supports tool calling SHALL be determined by its provider brand and auth method, not guessed at per request.

#### Scenario: A tool-calling-capable provider can be asked to call a tool

- **WHEN** the agent runtime sends a chat request with tool definitions to a provider profile that supports tool calling
- **THEN** the provider's response can include a request to invoke one of the supplied tools, distinguishable from ordinary assistant text

#### Scenario: A provider without tool-calling support is unaffected

- **WHEN** a provider profile that does not support tool calling is used for an ordinary chat request
- **THEN** it behaves exactly as before, with no missing capability or error related to tool calling

### Requirement: External CLI native-tool access control

For a subscription auth method backed by a CLI with its own native tool system (Claude Code, Codex), the application SHALL control which of that CLI's native tools it may use via an explicit, per-tool allow/deny list, rather than allowing all of them or blocking all of them uniformly. The default SHALL allow read-only, no-side-effect tools (including that CLI's own web search, when it is a no-cost first-party capability of the same subscription) and deny tools with file-write, shell-execution, or other side-effect capability; the user SHALL be able to change this per tool. The application SHALL NOT claim to support live, per-invocation approval of an individual native-tool call for these CLIs unless that has been verified to work.

#### Scenario: Read-only native tools are usable without extra configuration

- **WHEN** a subscription profile backed by a CLI with its own native tools is enabled
- **THEN** its read-only tools (file reading, directory listing, web search) are usable immediately, without the user changing any setting

#### Scenario: Side-effecting native tools stay off until explicitly allowed

- **WHEN** a subscription profile backed by a CLI with its own native tools is enabled and the user has not changed the default tool settings
- **THEN** tools capable of writing files or executing shell commands are not available to that CLI
