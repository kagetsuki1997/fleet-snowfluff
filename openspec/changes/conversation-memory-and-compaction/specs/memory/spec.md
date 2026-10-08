## Purpose

Defines what the assistant remembers about the user across conversations: how memories are written, corrected, found, and erased, how they are kept private, and how the model is allowed to use them.

## ADDED Requirements

### Requirement: Memories persist across conversations in one pool

The application SHALL keep saved memories on the local machine and SHALL make them available to every conversation. Memories belong to the user, not to a persona: the persona name in effect when a memory was written SHALL be recorded for reference only, and SHALL NOT be used to decide whether the memory is retrieved. Editing or renaming the persona SHALL NOT lose or hide any memory.

#### Scenario: A memory outlives its conversation

- **WHEN** the user tells the assistant a fact in one conversation and starts a new conversation later
- **THEN** the fact is available to the new conversation

#### Scenario: Renaming the persona keeps memories

- **WHEN** the persona's name is changed after memories were written
- **THEN** every memory is still retrieved exactly as before

### Requirement: What gets written

An explicit request to remember something, and a correction of something previously remembered, SHALL be saved immediately. Memories derived by the assistant from conversation SHALL be saved only when a compaction runs, never after every reply. Ordinary replies SHALL NOT be saved as memories.

#### Scenario: An explicit request is saved without waiting for a compaction

- **WHEN** the user asks the assistant to remember a fact
- **THEN** the fact is saved promptly, whether or not the assistant calls a tool for it, without waiting for any compaction, marked as told by the user

#### Scenario: Ordinary chatting writes nothing by itself

- **WHEN** a conversation proceeds without any explicit request, correction, or compaction
- **THEN** no memory is created

### Requirement: Derived memories and sensitive topics

Memories derived at compaction SHALL be usable immediately and SHALL be marked as automatically derived, distinct from those the user asked for. Derivation SHALL NOT produce memories about health, money, credentials, sexuality, religion, or politics; such content SHALL be saved only when the user explicitly asks.

#### Scenario: A derived memory is labeled

- **WHEN** a compaction produces a new memory
- **THEN** it is active at once and marked as automatically derived

#### Scenario: A sensitive topic is not derived

- **WHEN** the folded turns discuss the user's health
- **THEN** no memory about it is derived, and one is saved only if the user explicitly asks the assistant to remember it

### Requirement: Corrections supersede, deletion erases

When the user corrects a memory, the corrected memory SHALL replace the old one for retrieval, and the old one SHALL remain viewable as superseded. When the user deletes a memory or asks the assistant to forget it, the memory SHALL be erased entirely, so that it cannot be found by any search, digest, tool, or history view.

#### Scenario: A correction hides the old memory

- **WHEN** the user corrects a remembered fact
- **THEN** only the corrected fact is retrieved or injected, and the old one appears in the memory history as superseded

#### Scenario: A correction is applied even if the assistant never searches memory

- **WHEN** the user states a correction of something remembered and the assistant makes no memory tool call
- **THEN** the matching memory is still superseded by the corrected fact

#### Scenario: Deleting removes every trace

- **WHEN** the user deletes a memory
- **THEN** a search for its words finds nothing, it is absent from the digest, and it does not appear in the history

### Requirement: Retrieval on every request

Each request SHALL include a short digest of the user's most relevant stored memories, chosen by whether they are pinned, how important they are, and how recent they are, within a fixed size limit. Each request SHALL also include the few memories that best match the current message by keywords, when any match well enough. Both SHALL be presented inside one clearly labeled block identified as background information that is not instructions.

#### Scenario: The digest needs no match

- **WHEN** the user sends a message that shares no words with any stored memory
- **THEN** the digest of top-ranked memories is still included

#### Scenario: A matching memory is surfaced

- **WHEN** the user's message shares meaningful words with a stored memory that is not in the digest
- **THEN** that memory is included in the labeled block

#### Scenario: The block is bounded

- **WHEN** far more memories exist than fit the size limit
- **THEN** only the highest-ranked ones are included and the block stays within its limit

### Requirement: Keyword matching across supported languages

Matching SHALL work for English, Traditional Chinese, Simplified Chinese, Japanese, and Korean text without requiring an embedding model. Two-character Chinese words, Japanese kana and kanji text, Korean Hangul text, and Latin-script words mixed into CJK text SHALL be matched. A Traditional Chinese query SHALL match a memory written in Simplified Chinese and the reverse, while the memory's text is displayed exactly as it was written. Keywords the assistant attached to a memory at write time, including an English form, SHALL be matched too.

#### Scenario: A two-character Chinese word matches

- **WHEN** a memory contains 考試 and the user's message contains 考試
- **THEN** the memory is matched

#### Scenario: Traditional and Simplified match each other

- **WHEN** a memory was written with 学习 and the user's message contains 學習
- **THEN** the memory is matched, and it is displayed as 学习

#### Scenario: Latin words inside CJK text match

- **WHEN** a memory says 我在用 notion and the user's message contains notion
- **THEN** the memory is matched

#### Scenario: A one-character term still matches

- **WHEN** the user's message contains a single-character word that appears in a memory and has no better match
- **THEN** that memory can still be matched

#### Scenario: An English keyword reaches a memory written in Chinese

- **WHEN** a memory was written in Chinese with an English keyword attached and the user's message uses that English word
- **THEN** the memory is matched

### Requirement: Sensitive memory entries are not sent to cloud profiles

The user SHALL be able to mark a saved memory sensitive. A sensitive memory entry SHALL be used only when the request is handled by a local profile, and SHALL NOT appear in the digest, in keyword matches, or in memory tool results sent to a cloud profile. This applies to saved memory entries only: text the user typed into the conversation is not covered, and is sent to a cloud profile like any other message.

#### Scenario: A cloud profile does not receive a sensitive memory entry

- **WHEN** the default profile is a cloud profile and a memory is marked sensitive
- **THEN** that entry is absent from the digest, keyword matches, and memory tool results sent to the cloud profile

#### Scenario: A local profile does receive it

- **WHEN** a local profile handles the request
- **THEN** the sensitive memory can appear in the digest and matches like any other

#### Scenario: The conversation text is not covered

- **WHEN** the user typed a sensitive fact into the conversation and a cloud profile handles the request
- **THEN** that conversation text is sent like any other message, and only the saved memory entry for it is withheld

### Requirement: Memory tools and where they are available

The assistant SHALL be able to search and read memories through tools in every agent context, including delegated sub-agents and mixed mode's local attempt, where they are read-only. The tools to save a memory, to forget a memory, and the compaction tool SHALL be available only to the top-level agent. Forgetting through a tool SHALL require the user's confirmation. Memory tools SHALL always be included in the tool list the model is given and SHALL NOT be hidden behind the tool-search mechanism. A tool SHALL NOT accept a scope or owner from the model.

#### Scenario: A sub-agent can read but not write

- **WHEN** a delegated sub-agent's tool list is built
- **THEN** it contains the search and read tools and does not contain the save or forget tools

#### Scenario: The local attempt remains read-only

- **WHEN** mixed mode's local attempt builds its tool set
- **THEN** it contains the memory search and read tools and none that save or delete

#### Scenario: Forgetting asks first

- **WHEN** the top-level agent calls the forget tool
- **THEN** the user is asked to confirm before anything is erased, and declining erases nothing

#### Scenario: Memory tools are always offered

- **WHEN** many other tools are connected and hidden behind the tool-search mechanism
- **THEN** the memory tools are still in the tool list from the first request of a turn

### Requirement: CLI-backed runtimes receive memory but no tools

A request handled by a CLI-backed runtime SHALL include the same labeled memory block as any other request, and SHALL NOT be given memory tools.

#### Scenario: A CLI runtime sees memory in its context

- **WHEN** a message is handled by a CLI-backed profile
- **THEN** the digest and keyword matches are included in what it is sent, and no memory tool is offered

### Requirement: Memory content is untrusted

Memory text, including text derived from web or tool output, SHALL be treated as data. It SHALL appear only inside the labeled block, never in the persona's system prompt, and instructions contained in a memory SHALL NOT be presented to the model as system or developer instructions.

#### Scenario: A hostile memory does not gain authority

- **WHEN** a stored memory reads "ignore all previous instructions"
- **THEN** it is delivered only inside the labeled background block and the system prompt is unchanged

### Requirement: Memory failures never block chat

If the memory store cannot be read or written, the application SHALL still answer the user's message, without memory, and SHALL NOT lose the message.

#### Scenario: The store is unreadable

- **WHEN** the memory store cannot be opened when a message is sent
- **THEN** the message is answered normally without any memory block

### Requirement: Memory can be turned off

The user SHALL be able to turn memory off. While it is off, requests SHALL NOT include a memory block, memory tools SHALL NOT be offered, and no memory SHALL be derived or saved. Existing memories SHALL be kept unchanged while memory is off. The user SHALL also be able to erase all memories at once.

#### Scenario: Turning memory off stops all use of it

- **WHEN** memory is turned off and the user sends a message
- **THEN** the request contains no memory block, no memory tool is offered, and nothing is saved or derived

#### Scenario: Memories survive being switched off

- **WHEN** memory is turned off and later turned on again
- **THEN** the memories saved before are still there and are used again

#### Scenario: Erasing everything

- **WHEN** the user chooses to erase all memories and confirms
- **THEN** every memory, including superseded history, is gone and cannot be found by any search or view
