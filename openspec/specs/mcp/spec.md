# Spec: mcp

## Purpose

Defines how Aemeath connects to, trusts, and uses third-party MCP (Model Context Protocol) servers as an additional source of tools, consuming them the way it already consumes its own native tools.

## Requirements

### Requirement: Connecting to an MCP server requires explicit confirmation, with no pre-vetted allowlist

The application SHALL allow a user to request connecting to an MCP server either by describing it in a chat message or by filling in a form in the settings window, without restricting the request to a pre-vetted or curated list of known servers. Before any connection attempt is made, the application SHALL show the user exactly what would be connected (the command and arguments for a local server, or the URL for a remote one) and SHALL NOT proceed without the user's explicit approval of that specific request.

#### Scenario: A chat-requested connection is confirmed before anything happens

- **WHEN** the model resolves a chat request to connect to a specific MCP server
- **THEN** the user sees the exact command or URL that would be connected before any process is spawned or any network connection is made, and declining prevents the connection entirely

#### Scenario: A vague request is still shown concretely before connecting

- **WHEN** the model resolves an underspecified chat request (e.g. "connect the GitHub MCP server") to a specific command it has inferred
- **THEN** the user sees that concrete, resolved command in the confirmation, not the original vague request

#### Scenario: A known local service can be connected without any allowlist match

- **WHEN** a user describes a custom, previously-unknown local MCP server by its exact command
- **THEN** the application does not refuse or require the server to match any pre-configured list before offering to connect it

### Requirement: Credential handling differs by transport

For a server reached over a local (stdio) transport, the application SHALL collect any required credential as a plain value supplied by the user and inject it into the spawned process's environment, without initiating any browser-based authorization flow. For a server reached over a remote (HTTP) transport that requires OAuth authorization, the application SHALL act as the OAuth client itself: opening the user's system browser to the authorization server's consent page, and capturing the resulting authorization code via a short-lived local redirect listener or a manual code-entry fallback. In both cases, the resulting credential SHALL be stored using the application's existing secret-storage mechanism.

#### Scenario: A stdio server's required credential is collected without a browser

- **WHEN** a local MCP server is confirmed for connection and requires a credential
- **THEN** the user is prompted for that value directly in the confirmation flow, and no browser window is opened

#### Scenario: An HTTP server's OAuth requirement is discovered, not pre-declared

- **WHEN** a remote MCP server is confirmed for connection and its first request is rejected as unauthorized
- **THEN** the application discovers the authorization requirement from that rejection and proceeds to the browser-based authorization flow, without the user having had to say upfront that authorization was needed

#### Scenario: Authorization does not block the rest of the conversation

- **WHEN** a remote server's authorization flow has been started but not yet completed
- **THEN** the user can continue using the application normally, and the server becomes usable once authorization completes, whenever that happens

#### Scenario: A failed or abandoned authorization attempt can be retried

- **WHEN** a started authorization flow times out without completing
- **THEN** the server's configuration remains in a not-yet-usable state, and a later attempt starts a fresh authorization flow rather than being permanently blocked

### Requirement: A connected server's tools are consumed the same way native tools are

Once a server is successfully connected, the application SHALL discover its available tools and make each one callable the same way a native tool is callable, subject to the same permission-tier enforcement. Every tool a given connected server exposes SHALL default to requiring confirmation before it runs, with no finer-grained per-tool default in this version. A newly connected server's tools SHALL become usable starting with the next message sent, not within the same turn that connected it.

#### Scenario: A connected server's tool call is subject to confirmation like any other

- **WHEN** the model calls a tool belonging to a connected MCP server
- **THEN** the user is asked to confirm that call the same way they would be for any other tool requiring confirmation

#### Scenario: A newly connected server is not usable within the same turn

- **WHEN** a server finishes connecting partway through handling a message
- **THEN** that same turn does not gain access to the newly connected server's tools; they are available starting with the next message

### Requirement: MCP server connections persist across messages and conversations

The application SHALL keep a successfully established connection to an MCP server alive for reuse across multiple tool calls, messages, and conversations, rather than re-establishing it for every call. A connection that no longer exists (after an application restart, or after it was lost) SHALL be re-established automatically the next time it is actually needed, not proactively monitored.

#### Scenario: One connection serves many tool calls

- **WHEN** the same MCP server's tools are called multiple times across different messages
- **THEN** the underlying connection is reused rather than re-established for each call

#### Scenario: A connection is restored on first use after a restart

- **WHEN** the application is restarted and a previously connected server's tool is called for the first time since
- **THEN** the application re-establishes the connection automatically before making that call

### Requirement: Removing a connected server deletes its credential locally

The application SHALL allow a user to remove a previously connected MCP server from the settings window. Removing a server SHALL delete any credential stored for it and SHALL make its tools unavailable immediately. Removal SHALL NOT attempt to contact the server's own authorization provider to revoke the credential remotely.

#### Scenario: Removing a server deletes its stored credential

- **WHEN** a user removes a connected server that required a stored credential
- **THEN** that credential is deleted from local storage as part of the removal

#### Scenario: A removed server's tools are no longer offered

- **WHEN** a server has been removed
- **THEN** its tools no longer appear in any subsequently built tool registry

### Requirement: The local-first mix-mode attempt never offers MCP-sourced tools

The read-only, auto-tier-only tool set available to mixed mode's local-first attempt SHALL NOT include `connect_mcp_server` or any tool sourced from a connected MCP server, regardless of configuration.

#### Scenario: A connected server's tools are absent from the local attempt's registry

- **WHEN** the local-first attempt in mixed mode builds its own tool registry
- **THEN** no MCP-sourced tool, and no tool for connecting to a new MCP server, is present in it
