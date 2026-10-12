# Spec: context-compaction

## Purpose

Defines how a conversation that has outgrown what each request can carry is condensed instead of silently truncated, so the assistant keeps the gist of earlier turns without sending the whole history.

## Requirements

### Requirement: Older turns are summarized, not silently dropped

Each request carries a bounded amount of the most recent conversation (see the `ai-provider` capability). When turns fall outside that amount, the application SHALL fold them into a summary that is included with later requests in place of the dropped turns. Turns that have fallen outside the bound but are not yet covered by a summary SHALL continue to be sent, up to a fixed upper limit, so nothing is missing while a summary is pending. The persisted session history SHALL NOT be modified or shortened by this: the full transcript remains available as it was written.

#### Scenario: Turns leaving the bound are represented once summarized

- **WHEN** a session has more history than the bound and a summary has been produced
- **THEN** the request contains the most recent turns plus a summary covering the turns outside the bound, rather than only the recent turns

#### Scenario: Pending turns are still sent

- **WHEN** turns have fallen outside the bound but no summary covers them yet
- **THEN** they are still included in the next request, up to the upper limit, so the model does not lose them while a summary is pending

#### Scenario: The session history is untouched

- **WHEN** older turns have been folded into a summary
- **THEN** the persisted session still contains every turn exactly as it was written, and the chat window still shows them

### Requirement: Compaction triggers

The application SHALL compact in batches: it SHALL create or extend the summary once enough uncovered turns have built up and the conversation has been quiet for a short time, and SHALL do so once when a conversation becomes idle or is closed. It SHALL NOT make a summarization request after every reply, and SHALL NOT start one while a reply is being generated. It SHALL additionally compact when the estimated size of an agent run's request approaches the active model's context window. A model-requested compaction SHALL be honored as described under the compaction tool requirement.

#### Scenario: One extra exchange does not trigger a summary

- **WHEN** a conversation goes just past the history bound by a single exchange
- **THEN** no summarization request is made yet

#### Scenario: Built-up turns are summarized after a quiet moment

- **WHEN** enough uncovered turns have built up and the user has stopped sending messages for the short quiet period
- **THEN** one summary covering them is produced

#### Scenario: An idle conversation is summarized once

- **WHEN** a conversation with unsummarized turns goes idle or its chat window is closed
- **THEN** a summary covering those turns is produced once, without waiting for another message

#### Scenario: Compaction never competes with a reply

- **WHEN** the user sends a message while a compaction is waiting to start
- **THEN** the message is answered first, and the compaction does not start while that reply is being generated

#### Scenario: A long agent run near its limit is compacted

- **WHEN** an agent run's accumulated request grows close to the model's context window
- **THEN** compaction runs before the request that would exceed it

### Requirement: The summary is background data, not instructions

The summary SHALL be presented to the model inside a clearly labeled block identified as background information that is not instructions, and SHALL NOT be placed in or merged into the persona's system prompt. The summary SHALL be written as neutral notes and SHALL NOT be written in the persona's voice.

#### Scenario: The persona prompt is unchanged by compaction

- **WHEN** a summary exists for a conversation
- **THEN** the persona's system prompt is identical to what it would be with no summary, and the summary appears only in its own labeled block

#### Scenario: Instruction-like text in the summary is not privileged

- **WHEN** the summary contains text phrased as an instruction
- **THEN** it appears only inside the labeled background block and is not presented to the model as a system or developer instruction

### Requirement: The summary is in the conversation's language and omits sensitive topics

The summary SHALL be written in the language the user is using in the conversation. Because a summary persists and is sent with later requests, it SHALL NOT retain content about health, medication, money or debt, credentials, sexuality, religion, or politics. The application SHALL instruct the model to leave such content out, and SHALL additionally screen the finished summary with a deterministic check for such topics in every supported language, removing any sentence the check matches, because the model cannot be relied on to follow the instruction.

#### Scenario: A Traditional Chinese conversation gets a Traditional Chinese summary

- **WHEN** the conversation is in Traditional Chinese
- **THEN** the summary is written in Traditional Chinese, not in English

#### Scenario: A sentence about medication is removed

- **WHEN** the model's summary contains a sentence about the user's medication
- **THEN** that sentence is absent from the stored summary

#### Scenario: Surrounding content is kept

- **WHEN** a summary contains one sentence the screen removes and other sentences about unrelated topics
- **THEN** the other sentences remain

### Requirement: A failed compaction leaves the conversation intact

If producing a summary fails or returns nothing, the application SHALL leave the existing summary and window unchanged, SHALL NOT show the user an error for it, and SHALL still answer the user's message. It SHALL try again at the next trigger.

#### Scenario: The summarizing model is unavailable

- **WHEN** a compaction is triggered but the model that would write the summary cannot be reached
- **THEN** the user's message is answered normally, the previous summary stays in place, and no error is shown for the failed compaction

#### Scenario: An empty summary is discarded

- **WHEN** the summarizing model returns empty text
- **THEN** the existing summary is kept and nothing is replaced

### Requirement: Which model writes the summary

Summaries SHALL be written by an enabled local profile when one exists; otherwise by the default profile. A CLI-backed profile MAY be used. When it is, the summary request SHALL use its own fresh session and SHALL NOT read, reuse, or change the session that the user's chat with that profile continues.

#### Scenario: A local profile is preferred

- **WHEN** a local profile is enabled and the default profile is a cloud profile
- **THEN** the summary is written by the local profile and the conversation text is not sent to the cloud profile for it

#### Scenario: Falling back to the default profile

- **WHEN** no local profile is enabled
- **THEN** the summary is written by the default profile

#### Scenario: A CLI profile's chat session is not disturbed

- **WHEN** the default profile is CLI-backed and a summary is written with it
- **THEN** the session that the user's chat continues with that profile is unchanged afterwards

### Requirement: The model can request compaction

The assistant SHALL be able to request compaction through a tool, optionally giving a hint about what to preserve. The tool SHALL be available only to the top-level agent, not to delegated sub-agents or to the read-only local attempt in mixed mode. A request that fails SHALL leave the conversation as it was and SHALL be reported to the model as not done.

#### Scenario: A requested compaction runs with its hint

- **WHEN** the top-level agent calls the compaction tool with a hint to preserve
- **THEN** a summary is produced that is asked to preserve what the hint names

#### Scenario: The tool is absent where it must not be

- **WHEN** the tool list is built for a delegated sub-agent or for mixed mode's local attempt
- **THEN** the compaction tool is not in it

#### Scenario: A failed request is reported

- **WHEN** a model-requested compaction fails
- **THEN** the conversation is unchanged and the tool result tells the model the compaction was not done
