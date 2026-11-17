## Purpose

Lets external MCP clients such as Claude Code read scheduled jobs and request new ones, without giving them any way to run something the user has not approved. It covers how the local server is enabled, reached, authenticated and limited.

## ADDED Requirements

### Requirement: The jobs MCP server is opt-in and local

The application SHALL provide an MCP server for jobs that is off by default and starts only when the user turns it on in Settings, the choice persisting across restarts. It SHALL listen only on the loopback address, SHALL reject any request whose Host is not the loopback address, and SHALL reject any request that carries an Origin that is not the loopback address, while accepting a request with no Origin, as non-browser clients send none. It SHALL use a configurable fixed port and, if that port is unavailable, SHALL fail to start with a clear message instead of choosing another. While the application is closed or the server is off, clients SHALL simply be unable to connect.

#### Scenario: Off by default

- **WHEN** the application is installed and the user has not enabled the server
- **THEN** no port is opened

#### Scenario: A web page cannot reach it

- **WHEN** a request arrives with an Origin that is not the loopback address
- **THEN** it is rejected

#### Scenario: A client with no Origin is accepted

- **WHEN** a command-line client sends a request with a valid token and no Origin header
- **THEN** the request is processed

#### Scenario: The port is already in use

- **WHEN** the user enables the server and the configured port is taken
- **THEN** the server does not start and the user is told why

### Requirement: Token authentication

Every request to the server SHALL carry a bearer token and requests without a valid token SHALL be rejected. The token SHALL be generated on first enable and kept in the application's secret storage, SHALL be hidden in Settings until the user reveals it, and SHALL be replaceable, the old token ceasing to work immediately. Settings SHALL show the command that registers the server with Claude Code, including the token on request, with a copy control.

#### Scenario: A request without the token

- **WHEN** a client calls the server without a valid bearer token
- **THEN** the call is rejected and nothing is read or changed

#### Scenario: Regenerating the token

- **WHEN** the user regenerates the token
- **THEN** a client still holding the old token is rejected from its next request

### Requirement: Exposed tools and pending requests

The server SHALL expose tools to list jobs, read a job's status, read its run history, read one run's output, request a new job, and request a trigger change. It SHALL NOT expose deleting a job or editing a job's tool allow list. A request for a new job or trigger change SHALL create a pending request that does nothing until the user approves it in Settings; a requested job SHALL have only the read-only default allow list. The application SHALL tell the user a request arrived through a status bubble, a badge on the Jobs tab, and an operating-system notification when in the background. Approving, editing before approving, and rejecting SHALL all be available per request, with the requesting client's name shown.

#### Scenario: A client requests a job

- **WHEN** Claude Code asks the server to create a job
- **THEN** a pending request appears in Settings and the job does not run

#### Scenario: A client cannot delete or widen

- **WHEN** a client looks for a way to delete a job or add a tool to its allow list
- **THEN** the server offers none

#### Scenario: Reading counts as cloud

- **WHEN** a client reads a run's output
- **THEN** the output is returned only if that run's own profile was cloud, and metadata is returned otherwise

### Requirement: Application-spawned sessions do not receive the server

Agent sessions the application itself starts SHALL NOT be given access to the jobs server.

#### Scenario: A delegated Claude Code session

- **WHEN** the application starts a Claude Code session to do work for the user
- **THEN** that session has no jobs tools
