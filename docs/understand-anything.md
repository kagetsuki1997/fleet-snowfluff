# Exploring the codebase with Understand-Anything

[Understand-Anything](https://github.com/Egonex-AI/Understand-Anything) is a Claude Code
plugin that analyzes a repository into a **knowledge graph** (files, functions, classes, and
the imports between them), groups it into architectural **layers**, writes a guided **tour**,
and serves it all in an interactive **dashboard**. It is optional tooling for getting oriented
in this codebase; nothing in the build depends on it.

## Prerequisites

- Claude Code with the `understand-anything` plugin installed.
- Node.js 22 or newer and `pnpm` 10 or newer. The first `/understand` run builds the plugin's
  core package once, inside the plugin's own cache directory, not in this repository.

## Build the graph

Run this from the repository root inside Claude Code:

```text
/understand
```

It runs seven phases: scan, batching, per-file analysis (parallel subagents), a review of the
merged graph, architectural layers, a guided tour, and save. On this repository a run
dispatches around 20 subagents, so it takes several minutes and a real amount of model usage.
The first run asks you to confirm the ignore file before scanning.

| Option | Effect |
| --- | --- |
| `--full` | Rebuild everything and ignore any existing graph. |
| `--review` | Run the full LLM graph-reviewer instead of the quick deterministic check. |
| `--language <lang>` | Write summaries, tour and lessons in that language (for example `zh-TW`, `ja`, `ko`). The choice is remembered in `.ua/config.json`. |
| `--exclude "<globs>"` | Extra comma-separated patterns to leave out, gitignore syntax. |
| `--auto-update` / `--no-auto-update` | Turn automatic graph updates on commit on or off. |
| `<path>` | Analyze another directory instead of the current one. |

Re-running `/understand` after new commits does an **incremental** update: only files whose
structure changed are re-analyzed.

### Scope

Everything the analysis needs lives in `.ua/`. Which files are analyzed is controlled by
`.ua/.understandignore` (gitignore syntax). A scope that keeps the analysis small and focused
on the code is:

```gitignore
/*
!/crates/
!/ui/
ui/public/
**/memory/t2s_table.rs
```

That covers about 140 files. Without a scope the run also analyzes `openspec/`, `assets/`,
`docs/` and `locales/`, which roughly doubles the work and adds little for code navigation.

### What it writes

| File | Purpose |
| --- | --- |
| `.ua/knowledge-graph.json` | Nodes, edges, layers and the tour. |
| `.ua/code-lessons.json` | Per-file explanations shown in the dashboard's code viewer. |
| `.ua/fingerprints.json`, `.ua/meta.json` | Baseline for incremental updates. |
| `.ua/config.json`, `.ua/.understandignore` | Your language and scope settings. |

`.ua/` is generated output, about 1.3 MB, tied to the commit it was built from, and none of it
contains machine-specific paths. **This repository commits it**, so anyone can open the dashboard
without running an analysis. The checked-in files are `knowledge-graph.json`,
`code-lessons.json`, `fingerprints.json`, `meta.json`, `.understandignore` and
`intermediate/scan-result.json`; the last three plus the fingerprints are what let a later
`/understand` update incrementally. `.ua/.gitignore` keeps the scratch folders (`.trash-*/`,
`tmp/`) out, and the root `.gitignore` already ignores `config.json`, so your language setting
stays local.

Because it is committed, treat it as a snapshot rather than live data:

- It goes stale as soon as the code changes. Refresh it deliberately (for example when a
  release is cut) by running `/understand` and committing the result, not on every commit.
- Regenerating rewrites most of the JSON, so expect large, unreviewable diffs. Review the
  summary of what changed, not the diff.
- `meta.json` and the graph record the commit they describe. Keep that commit in `develop`'s
  history, or incremental updates cannot diff from it and fall back to a full rebuild.

## Open the dashboard

```text
/understand-dashboard
```

This starts a local server bound to `127.0.0.1` and opens your browser. It prints a line like:

```text
🔑  Dashboard URL: http://127.0.0.1:5173/?token=<token>
```

Use the whole URL, including `?token=`: the dashboard refuses to load the graph without it.
If port 5173 is taken, the next free port is used.

Two ways it can run:

1. **Prebuilt viewer (default).** Downloads a viewer pinned to the installed plugin version from
   the plugin's GitHub release and runs it with `npx`. Nothing is installed into this repository.
2. **Local build (fallback).** If the download fails (no network, no release asset), it installs
   and builds the plugin's own dashboard package and starts a Vite dev server.

The server keeps running in the background. Stop it with Ctrl+C if it is in a foreground
terminal; otherwise find the process with `lsof -i :5173` and stop it.

### What to look at

- **Tour**: a numbered walk through the app, from startup and pet motion, through rendering and
  the platform layer, to the AI providers, agent loop, memory, compaction and the MCP client.
- **Layers**: the architecture grouped by area, with named file groups inside the larger layers.
- **Code lessons**: opening a file from the tour shows an explanation split into line ranges.
  Files outside the tour get an overview only.
- **Search and node detail**: find a function or class and follow its `imports`, `exports` and
  `contains` edges.

## Other commands

| Command | Use it to |
| --- | --- |
| `/understand-chat` | Ask questions about the codebase using the graph. |
| `/understand-explain` | Get a deep-dive on one file, function or module. |
| `/understand-diff` | Explain what a diff or pull request touches and the risk it carries. |
| `/understand-onboard` | Generate an onboarding guide for someone new to the project. |
| `/understand-domain` | Extract business-domain flows into a separate domain graph. |

## Known limits on this repository

- **Few behavioural edges.** Most edges are `contains`, `exports` and `imports`. `calls` and
  `implements` edges are sparse, so the graph shows structure and dependencies far better than
  runtime call flow. Use it to find where to look, then read the code.
- **Uneven summaries.** Some function and class summaries are generic, and large files such as
  `ui/src/main.ts` and `agent_runtime.rs` were only partly read, so their summaries and lesson
  line ranges are best-effort. Re-run with `--review` if you need a stricter check.
- **Layers are heuristic.** They follow the crate layout and file names, not a verified
  dependency analysis.
- **It describes the files it saw.** After large changes, run `/understand` again; the graph is
  not updated live.
