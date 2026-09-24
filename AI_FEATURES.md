# AI Agent Features

This document covers what the AI chat companion can actually _do_ beyond
plain conversation — tool calling, the native tools available to it, and
how each one behaves. For the high-level feature list and setup, see
[`README.md`](README.md); for building from source, see
[`DEVELOP.md`](DEVELOP.md).

## Tool calling

Tool calling — letting the model read files, run commands, or search the
web as part of answering you — is available for:

- the **local Ollama** provider, and
- **OpenAI and Anthropic API-key profiles**, when they use the provider's
  own endpoint (the default). An API-key profile pointed at a **custom
  endpoint** (OpenRouter, a local server, a proxy…) stays plain chat: many
  such servers reject tool definitions, and those profiles chat fine today,
  so they're left exactly as they were.

Claude Code and Codex (the subscription-auth providers) have their own,
separate native-tool system instead — see
[Claude Code / Codex own tools](#claude-code--codex-own-tools) below.

### What leaves your machine

With a cloud API-key profile, the **results of tool calls** — file
contents, command output, search results — are sent to the provider along
with the conversation, because the model needs them to continue. One
message can also trigger several requests (the loop stops after a fixed
number of steps), each billed to your API key. The provider's one-time
data disclosure says both; it was updated for this, so if you had already
accepted an API-key disclosure you'll be asked to review the new text once,
and chat with that profile waits until you do (Settings → AI). The local
Ollama provider doesn't send your conversation to a cloud model — though a
`web_search` it runs still sends the search query to a search service (see
[Web search](#web-search)).

### When a task needs more than these tools

In `mix` mode, a request that needs tools is escalated from the local model
to your default profile; if that's an API-key profile on its default
endpoint, it now runs the tool loop instead of just chatting. Fleet
Snowfluff's own tools can read, search and (with confirmation) run
commands, but they have **no tool for writing files** — a request to edit
code may be declined, and a Claude Code profile only writes files if you've
turned those tools on.

### How each call is permitted

When the model requests a tool call, it goes through a permission check
before running:

- **Auto** — runs immediately (read-only, no side effects: `web_search`,
  `get_system_context`, and file access inside your configured project
  folder).
- **Confirm** — a popup asks you to approve or deny it first. If a model
  turn requests several tool calls at once, they're all shown together in
  one popup, not one at a time. For read-only tools, you can check
  "remember for this session" so the same request doesn't ask again until
  you start a new chat; tools that can write files or run commands never
  offer that option.
- **Deny** — reported back to the model as "not permitted," never silently
  ignored.

## Native tools

| Tool                 | What it does                                                                                 |
| -------------------- | -------------------------------------------------------------------------------------------- |
| `web_search`         | Searches the web — see [below](#web-search) for the full fallback chain and its limitations. |
| `read_file`          | Reads a file's contents (first 20 KB only — see below).                                      |
| `list_directory`     | Lists a directory's contents.                                                                |
| `run_command`        | Runs a shell command.                                                                        |
| `get_system_context` | Reports the current date/time, OS, and CPU/memory/uptime.                                    |

`read_file`/`list_directory` are auto-allowed for any path inside your
configured **project folder** (Settings → AI); a path outside it — or any
path at all, if no project folder is set — asks for confirmation instead of
being refused outright.

`read_file` returns at most the first **20 KB** of a file and says so when it
has cut one off, so a huge file can't flood the chat, run up your bill, or
send more of your data to a cloud provider than the model needed.

`run_command` runs in your project folder as its working directory (it
isn't configurable per-request the way file access is, since a shell
command is a meaningfully higher risk than reading a file). It always asks
for confirmation, times out after **30 seconds**, and caps combined
output at **20 KB** — a command that hangs or produces a wall of output
doesn't stall or flood the chat.

`get_system_context` reports OS name, current UTC time, CPU core
count/usage, memory used/total, and uptime. It deliberately does **not**
read the active window title, your idle time, or clipboard content.

### Web search

`web_search` requires no account, API key, or login of any kind. It tries
up to three sources, in order, and returns the first one that actually has
results:

1. **A local [`ddgs`](https://pypi.org/project/ddgs/) server, if you're
   running one.** `ddgs` (the successor to `duckduckgo_search`) is a
   Python package with a bundled `ddgs api` command that runs a small local
   search server:

   ```
   pip install -U ddgs[api]
   ddgs api -d
   ```

   By default it listens on `127.0.0.1:4479` (`ddgs`'s own default port,
   not something you need to configure here) and aggregates results from
   several real search engines. **This step is entirely optional** — Fleet
   Snowfluff never installs or starts it for you, and never asks you to.
   If it isn't running, search simply moves on to the next source with no
   visible error; if you start it later, mid-session, it's picked up
   within about a minute of the next search. Running it is worth it mainly
   for **non-English queries**, which the next two sources handle poorly
   (see below).

2. **A public [SearXNG](https://searx.be) instance.** Real, ranked search
   results, but unofficial infrastructure Fleet Snowfluff doesn't control
   — public instances are increasingly protected by anti-bot challenges
   (CAPTCHAs, JavaScript proof-of-work) that can make this tier
   unreliable or unavailable for stretches of time, through no fault of
   the app.
3. **DuckDuckGo's Instant Answer API.** Official and always reachable
   without a key, but narrow by design — it only returns pre-built
   infobox/definition-style answers (mostly sourced from English
   Wikipedia), not general search results. It reliably answers a query
   like "rust programming language" but not an arbitrary question, and
   it's heavily biased toward English — this is the specific gap the
   `ddgs` tier above exists to fill.

If all three come back empty, you get an honest "no search results
available" instead of an error — a source being down degrades answer
quality for that message, not the tool itself.

## Claude Code / Codex own tools

Claude Code and Codex (the subscription-auth providers) don't use Fleet
Snowfluff's tool-calling/permission system above at all — each CLI owns
its own built-in tools (file access, running commands, its own web
search, etc.) directly.

For **Claude Code**, Fleet Snowfluff controls _which_ of those built-in
tools it's allowed to use via a per-tool allow-list in Settings → AI:
read-only tools (reading files, listing directories, web search) are
allowed by default, and anything that can write files or run commands is
denied by default — adjustable per tool. There's no live per-call
confirmation for these the way there is for Fleet Snowfluff's own tools
above; a denied tool is simply unavailable until you turn it on.

**Codex** has no equivalent per-tool control — it runs at a single, coarser
sandbox level (read-only) instead. This is a real, known gap, not an
oversight: Codex's own permission model is less granular than Claude
Code's, and Codex support in general is still experimental.

### Conversation memory and sessions

Both CLIs keep their own session, and Fleet Snowfluff resumes it from one
message to the next so the CLI doesn't have to be re-sent the whole
conversation each time. Your chat transcript is the source of truth; the
CLI's session is a cache of it:

- **Resumed session:** your new message is sent, plus any turns the CLI
  never saw — in `mix` mode another provider may have answered some in
  between. If it saw everything, only the new message is sent.
- **No session to resume** (the first message, a session the CLI has
  since dropped, or a message handed over from another provider in
  `mix` mode): the request also carries a short summary of the recent
  transcript, so the reply isn't produced without the earlier turns.
- **Restarting the app** picks the same session back up. The session
  references are stored in a small `.sessions.json` file next to that
  conversation's chat log, and start over with a new chat.

The CLI is always run in a fixed directory — your project directory if
you've set one, otherwise a `cli-workspace` folder in Fleet Snowfluff's
config directory — never wherever the app happened to be launched from.
This only decides where the CLI's relative paths point; it is **not** a
security boundary, and the per-tool allow-list above is still what
controls what Claude Code may do. Pressing Stop ends the CLI process
along with the reply.

Codex gets the same handling but, like the rest of Codex support, hasn't
been verified against a real subscription.
