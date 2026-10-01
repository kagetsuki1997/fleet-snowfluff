## MODIFIED Requirements

### Requirement: Agent loop for tool-calling providers

For a provider profile that supports tool calling, the application SHALL run an agent loop: present the available native tools, allow the model to request one or more tool calls, execute permitted ones, return their results to the model, and repeat until the model produces a final answer or a limit is reached. For a provider profile that does not support tool calling, chat SHALL behave exactly as it does without an agent loop. The outcome of every tool call attempted during the loop SHALL remain available to the caller once the loop ends, not only the model's final reply text (see the `execution-log` capability for how that outcome is persisted and reused).

#### Scenario: A tool call result is used in the following reply

- **WHEN** the model requests a tool call and receives its result
- **THEN** the model's next response can incorporate that result before the user sees the final answer

#### Scenario: Iteration is bounded

- **WHEN** the model repeatedly requests tool calls without producing a final answer
- **THEN** the agent loop stops after a fixed maximum number of iterations rather than continuing indefinitely

#### Scenario: Tool call outcomes are available after the loop ends

- **WHEN** the agent loop produces a final answer after making one or more tool calls
- **THEN** the caller can determine which tools were called and whether each succeeded, failed, or was not permitted to run, without needing anything beyond what the loop itself returns
