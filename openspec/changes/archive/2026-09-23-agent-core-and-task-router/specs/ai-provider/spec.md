## MODIFIED Requirements

### Requirement: Subscription auth via the provider's own CLI

For a provider profile using subscription auth, the application SHALL detect and use an already-authenticated installation of that provider's official CLI (`claude` for Anthropic, `codex` for OpenAI) rather than implementing its own OAuth flow. The application SHALL NOT require the user to enter an OAuth token manually. Detecting whether that CLI binary exists SHALL NOT be limited to the application process's own inherited `PATH` environment variable — the application SHALL also check common per-platform install locations and, on platforms where a GUI-launched process does not inherit an interactively-configured `PATH` (for example, an app launched from a desktop/dock icon rather than a terminal), the user's own shell environment, so that a CLI installed via a package manager or version manager is still found regardless of how the application was launched.

#### Scenario: CLI already logged in

- **WHEN** the user enables Anthropic via subscription and the `claude` CLI is installed and already authenticated
- **THEN** chat requests succeed using that CLI's credential, with no additional login step shown

#### Scenario: CLI not installed

- **WHEN** the user enables a subscription auth method and the corresponding CLI binary cannot be found
- **THEN** the profile shows a runtime-unavailable state naming the missing CLI, and no chat request is attempted

#### Scenario: CLI installed but not logged in

- **WHEN** the user enables a subscription auth method and the CLI is installed but not authenticated
- **THEN** the application attempts to trigger that CLI's own login flow (which opens a browser); if the login flow cannot be completed this way, the settings UI instead instructs the user to run the CLI's login command themselves and then re-check status

#### Scenario: CLI found even when launched outside a terminal

- **WHEN** the user enables a subscription auth method and the corresponding CLI is installed somewhere not on the minimal `PATH` a GUI-launched process inherits (for example, installed via Homebrew, a Node version manager, or Nix)
- **THEN** the application still finds and uses it, without the user needing to launch the application from a terminal or manually edit its `PATH`

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
