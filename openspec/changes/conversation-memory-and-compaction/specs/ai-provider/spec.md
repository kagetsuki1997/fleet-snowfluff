## MODIFIED Requirements

### Requirement: Bounded conversation context

Each request SHALL include only a bounded amount of the most recent conversation, limited by a size budget measured by an estimate of the text's size, not only by a count of messages, and not the full session history. The budget SHALL be a fixed amount, reduced when the active model's context window is small enough that the fixed amount would not fit comfortably. Many short messages SHALL fit within the budget in greater number than a few long ones. A hard upper limit on the number of messages SHALL also apply. Turns that fall outside the budget SHALL NOT simply disappear from the model's view: they SHALL be represented as defined by the `context-compaction` capability.

#### Scenario: Long-running session

- **WHEN** a session has accumulated far more turns than the fixed context window
- **THEN** only the most recent turns within that window are sent as context on the next request

#### Scenario: A small context window shrinks the budget

- **WHEN** the active model's context window is small
- **THEN** the history sent is reduced so the persona prompt and the request still fit, rather than being silently trimmed by the model server

#### Scenario: Many short messages are all kept

- **WHEN** a session contains many very short messages that together fit within the size budget
- **THEN** all of them are sent as context, not just the most recent fixed number

#### Scenario: One very long message does not crowd out the rest unboundedly

- **WHEN** a session contains one very long message
- **THEN** the amount of history sent stays within the size budget and does not grow with that message

#### Scenario: Older turns are summarized, not lost

- **WHEN** a session has accumulated far more history than the budget
- **THEN** the turns outside the budget are represented by a summary in the request, in addition to the most recent turns

### Requirement: Cloud provider data disclosure

The first time a (provider, auth method) pair involving a cloud provider (OpenAI or Anthropic, via either API key or subscription) is enabled, the settings UI SHALL show a one-time disclosure describing what will happen for that specific auth method — naming the provider and, for subscription auth, that the CLI's already-logged-in account will be used to send messages, not a newly entered API key — and that pair SHALL NOT become usable until the disclosure is acknowledged. For an API-key pair, the disclosure SHALL also state that when the assistant uses tools, their results (such as file contents, command output, and search results) are sent to the provider along with the conversation, and that using tools can result in several requests, each billed to the API key, for a single message. This acknowledgment SHALL be persisted per (provider, auth method) pair, so enabling a different auth method for a provider whose other auth method was already acknowledged SHALL show its own disclosure. When the disclosure text for a cloud pair (API key or subscription) changes materially, an acknowledgement given for the earlier text SHALL NOT carry over: the updated disclosure SHALL be shown once more, and until it is acknowledged the profile SHALL NOT be used for chat (an already-enabled profile in that state SHALL show the disclosure with a way to acknowledge it, rather than only a status label). For every cloud pair, the disclosure SHALL also state that memories the assistant has saved about the user, and summaries of earlier parts of the conversation, may be sent to the provider as part of a request; that saved memories the user marked sensitive are not sent; and that this does not cover anything the user typed into the conversation itself, which is sent like any other message. Selecting Ollama or Mock SHALL require no such disclosure.

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

#### Scenario: The disclosure mentions memory and summaries, and the limit of the sensitive flag

- **WHEN** the disclosure is shown for any cloud (provider, auth method) pair
- **THEN** it states that saved memories and conversation summaries may be sent, that sensitive saved memories are not, and that text typed into the conversation is not covered by that exception

#### Scenario: An acknowledgement given before memory existed does not carry over

- **WHEN** the user acknowledged a cloud pair's disclosure (API key or subscription) that did not mention memory, and then opens the application after this capability ships
- **THEN** the updated disclosure is shown for that pair once, and is not shown again after it is acknowledged

#### Scenario: A subscription profile awaiting re-acknowledgement is not used for chat

- **WHEN** a subscription-backed profile is enabled and is the default, but its acknowledgement was cleared because the disclosure text changed
- **THEN** chat is not sent to it, the chat window says the updated disclosure needs review, and the settings row shows that disclosure with a way to acknowledge it

## ADDED Requirements

### Requirement: The persona prompt is never trimmed

Every request SHALL carry the persona's system prompt in full, unshortened, and as the system prompt, never inside a conversation turn. When a request would not fit the active model's context window, the application SHALL reduce other content first, in this order: older history, then the persona's few-shot examples, then the optional tool listings (the stubs for tools not yet loaded). It SHALL also lower the reply allowance on a small window instead of letting the reply overflow it. If the persona prompt, the user's current message, and a minimal reply allowance still do not fit, the application SHALL NOT send the request and SHALL tell the user that the model's context window is too small. For a CLI-backed profile, the persona SHALL be delivered through that CLI's own system-prompt mechanism, so that the CLI's own context management cannot summarize it away.

#### Scenario: History shrinks before the persona does

- **WHEN** a small context window cannot hold the persona prompt, the tools, and the usual amount of history
- **THEN** the history sent is reduced, and the persona prompt in the request is byte-identical to the one that fits comfortably

#### Scenario: Few-shot examples and tool listings go next

- **WHEN** the persona prompt and tool listings still leave no room for history even after it is reduced to nothing
- **THEN** the persona's few-shot examples are dropped first, then the stub tool listings, and the persona prompt is still sent whole

#### Scenario: The reply allowance shrinks on a small window

- **WHEN** the context window is small
- **THEN** the allowance reserved for the reply is reduced so that the prompt plus the allowance fits the window

#### Scenario: A window too small for the persona is reported, not worked around

- **WHEN** the persona prompt, the current message, and a minimal reply allowance cannot fit the context window
- **THEN** no request is sent, and the user is told the model's context window is too small, rather than the persona being cut

#### Scenario: A CLI profile keeps the persona out of its conversation

- **WHEN** a message is handled by a CLI-backed profile
- **THEN** the persona is passed through that CLI's system-prompt mechanism and does not appear in any conversation turn

