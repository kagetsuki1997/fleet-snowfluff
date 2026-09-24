## MODIFIED Requirements

### Requirement: Provider tool-calling capability

A provider implementation MAY additionally support tool calling: accepting a set of tool definitions alongside a chat request and returning, for each turn, either further assistant text or a request to invoke one of the supplied tools, and accepting the result of an invoked tool tied to the request it answers. A provider that does not support tool calling SHALL continue to support ordinary chat unchanged. Whether a given provider profile supports tool calling SHALL be determined by its provider brand, its auth method, and whether it targets the provider's own default endpoint, not guessed at per request. Local Ollama profiles, and OpenAI and Anthropic API-key profiles using the provider's default endpoint, SHALL support tool calling; an API-key profile configured with a custom endpoint SHALL NOT, and SHALL behave exactly as ordinary chat.

#### Scenario: A tool-calling-capable provider can be asked to call a tool

- **WHEN** the agent runtime sends a chat request with tool definitions to a provider profile that supports tool calling
- **THEN** the provider's response can include a request to invoke one of the supplied tools, distinguishable from ordinary assistant text

#### Scenario: A provider without tool-calling support is unaffected

- **WHEN** a provider profile that does not support tool calling is used for an ordinary chat request
- **THEN** it behaves exactly as before, with no missing capability or error related to tool calling

#### Scenario: An API-key profile on the default endpoint supports tools

- **WHEN** an OpenAI or Anthropic API-key profile that uses the provider's default endpoint is asked to handle a message that needs a tool
- **THEN** the model can request a tool call, the result is returned to it tied to that request, and its next response can use the result

#### Scenario: A custom endpoint stays ordinary chat

- **WHEN** an OpenAI or Anthropic API-key profile is configured with a custom endpoint and receives a chat message
- **THEN** no tool definitions are sent and the request behaves exactly as it did before this capability existed

#### Scenario: A tool call split across the response stream is reassembled

- **WHEN** a provider streams one tool call's arguments in several fragments
- **THEN** the agent runtime receives a single complete tool call with its full arguments once the model has finished describing it, never a partial one

#### Scenario: Several tool results from one turn are all returned

- **WHEN** the model requests more than one tool call in a single turn
- **THEN** every result is returned to the model, each tied to its own request

### Requirement: Cloud provider data disclosure

The first time a (provider, auth method) pair involving a cloud provider (OpenAI or Anthropic, via either API key or subscription) is enabled, the settings UI SHALL show a one-time disclosure describing what will happen for that specific auth method — naming the provider and, for subscription auth, that the CLI's already-logged-in account will be used to send messages, not a newly entered API key — and that pair SHALL NOT become usable until the disclosure is acknowledged. For an API-key pair, the disclosure SHALL also state that when the assistant uses tools, their results (such as file contents, command output, and search results) are sent to the provider along with the conversation, and that using tools can result in several requests, each billed to the API key, for a single message. This acknowledgment SHALL be persisted per (provider, auth method) pair, so enabling a different auth method for a provider whose other auth method was already acknowledged SHALL show its own disclosure. When the disclosure text for an API-key pair changes materially, an acknowledgement given for the earlier text SHALL NOT carry over: the updated disclosure SHALL be shown once more, and until it is acknowledged the profile SHALL NOT be used for chat (an already-enabled profile in that state SHALL show the disclosure with a way to acknowledge it, rather than only a status label). Selecting Ollama or Mock SHALL require no such disclosure.

#### Scenario: First cloud selection

- **WHEN** the user enables OpenAI via API key for the first time
- **THEN** a disclosure naming OpenAI and describing API-key usage, including that tool results are sent to the provider and that tool use may make several billed requests per message, is shown, and that profile does not become usable until acknowledged

#### Scenario: Disclosure does not repeat once acknowledged

- **WHEN** the user has previously acknowledged OpenAI via API key's current disclosure, and later re-enables OpenAI via API key after disabling and re-enabling it
- **THEN** no disclosure is shown for that (provider, auth method) pair the second time

#### Scenario: Switching auth method re-triggers disclosure

- **WHEN** the user has acknowledged Anthropic via API key's disclosure, then enables Anthropic via subscription for the first time
- **THEN** a separate disclosure describing subscription usage is shown, even though Anthropic via API key was already acknowledged

#### Scenario: Local provider needs no disclosure

- **WHEN** the user selects Ollama as a provider
- **THEN** no data-disclosure prompt is shown

#### Scenario: A profile awaiting re-acknowledgement is not used for chat

- **WHEN** an OpenAI or Anthropic API-key profile is enabled and is the default, but its acknowledgement was cleared because the disclosure text changed
- **THEN** chat is not sent to it, the chat window says the updated disclosure needs review, and the settings row shows that disclosure with a way to acknowledge it

#### Scenario: Chat becomes available again once the disclosure is acknowledged

- **WHEN** the chat window is open and blocked on a pending disclosure, and the user acknowledges it in settings
- **THEN** the chat window allows sending again without having to be closed and reopened

#### Scenario: An earlier acknowledgement does not cover the updated tool-result disclosure

- **WHEN** the user acknowledged an OpenAI or Anthropic API-key disclosure before it mentioned tool results, and then opens the application after this capability ships
- **THEN** the updated disclosure is shown for that pair once, and is not shown again after it is acknowledged
