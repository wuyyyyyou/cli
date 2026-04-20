# AGENTS.md

## Project Overview

This repository is a Rust wrapper around the upstream [`googleworkspace/cli`](https://github.com/googleworkspace/cli) project.

The core `gws` CLI is still the same dynamic Google Workspace client: it fetches Google Discovery documents at runtime and builds commands dynamically. This repository adds an Anna/Executa adapter layer so the CLI can be called safely by the ANNA agent through a JSON-RPC over stdio plugin protocol.

When working in this repo, treat it as a **wrapper/adaptation project first**, not a fork that should diverge from upstream `gws` behavior without a clear reason.

> [!IMPORTANT]
> **Default implementation strategy**: prefer reusing the embedded `gws` runtime and existing library/CLI code. Do not re-implement Google Workspace API behavior in the Executa wrapper unless the protocol boundary requires it.

> [!IMPORTANT]
> **Protocol source of truth**: Anna/Executa behavior must follow:
> `.docs/protocol-spec.zh-CN.md`
>
> If local wrapper behavior and generic assumptions conflict, follow the protocol spec and the actual implementation in `crates/google-workspace-cli/src/gws_executa.rs`.

> [!NOTE]
> **Package manager**: use `pnpm` instead of `npm` for Node.js package management in this repository.

## Primary Goals

This repository has two deliverables:

1. `gws`
   - The upstream-style CLI for humans and agents
2. `gws-executa`
   - A single-binary Anna Executa plugin that exposes `gws` through JSON-RPC over stdio

The wrapper is successful only if both remain coherent:

- `gws` keeps its native CLI behavior
- `gws-executa` translates Anna protocol requests into isolated embedded `gws` executions
- protocol responses remain machine-safe for ANNA consumption

## Build & Test

For normal Rust work:

```bash
cargo build
cargo test
cargo clippy -- -D warnings
```

For the Anna wrapper specifically:

```bash
cargo build -p google-workspace-cli --bin gws-executa
cargo test -p google-workspace-cli gws_executa
```

If you need manual protocol verification, see:

- `crates/google-workspace-cli/EXECUTA.md`

> [!IMPORTANT]
> Prefer targeted tests around the wrapper (`gws_executa.rs`) when changing protocol handling, credential forwarding, file transport, stdout/stderr behavior, or request validation.

## Changesets

Every PR must include a changeset file at `.changeset/<descriptive-name>.md`:

```markdown
---
"@googleworkspace/cli": patch
---

Brief description of the change
```

Use:

- `patch` for fixes, docs, chores, and wrapper-only adjustments
- `minor` for new user-facing features or new helper capabilities
- `major` for breaking CLI or protocol changes

## Architecture

### Upstream core

The base CLI still uses the upstream two-phase parsing model:

1. Parse argv to identify the Google service
2. Fetch the Discovery document
3. Build the dynamic `clap::Command` tree
4. Re-parse and execute

Do not replace this with handwritten per-API command trees.

### Anna wrapper layer

`gws-executa` is the adaptation layer for ANNA.

It:

- reads JSON-RPC 2.0 requests from `stdin`
- writes protocol responses to `stdout`
- writes logs/debug output to `stderr`
- exposes `describe`, `health`, and `invoke`
- currently exposes a single tool: `run_gws`
- launches the same binary in an internal mode (`__anna_run_gws_internal`) to execute embedded `gws`
- forwards credentials into the isolated child process via environment variables
- always uses file transport for `invoke` responses

### Protocol invariants

These are non-negotiable when modifying the wrapper:

1. `stdout` must contain protocol JSON only
2. each protocol message must be a single line
3. logs/debug output belong on `stderr`, never `stdout`
4. the wrapper must speak JSON-RPC 2.0 over stdio
5. manifest/tool schemas must stay compatible with the Anna/Executa protocol
6. array parameters must declare `items` type information

If you accidentally print human-readable text to `stdout` in wrapper mode, you are breaking the protocol.

### File transport

The current wrapper design uses file transport for `invoke`:

- `describe` and `health` may return directly on `stdout`
- `invoke` returns a JSON pointer containing `__file_transport`
- the actual response payload is written to a temp JSON file
- `arguments.cwd` may override the temp file directory
- otherwise the plugin binary directory is used

When changing transport behavior, update both:

- `crates/google-workspace-cli/src/gws_executa.rs`
- `crates/google-workspace-cli/EXECUTA.md`

## Workspace Layout

| Path | Purpose |
| --- | --- |
| `crates/google-workspace/` | Publishable library with Discovery models, shared client, validation, and service registry |
| `crates/google-workspace-cli/` | Main CLI crate containing `gws`, auth flows, helpers, and `gws-executa` |

### Important files

#### Upstream/shared behavior

| File | Purpose |
| --- | --- |
| `crates/google-workspace/src/discovery.rs` | Discovery document fetch/cache models |
| `crates/google-workspace/src/services.rs` | service alias to API name/version mapping |
| `crates/google-workspace/src/validate.rs` | shared validation helpers |
| `crates/google-workspace/src/client.rs` | shared HTTP client logic |

#### Native CLI behavior

| File | Purpose |
| --- | --- |
| `crates/google-workspace-cli/src/main.rs` | native `gws` entrypoint |
| `crates/google-workspace-cli/src/commands.rs` | dynamic clap command builder |
| `crates/google-workspace-cli/src/executor.rs` | HTTP request execution |
| `crates/google-workspace-cli/src/auth.rs` | credential loading |
| `crates/google-workspace-cli/src/auth_commands.rs` | `gws auth` workflows |
| `crates/google-workspace-cli/src/lib.rs` | shared CLI runtime wiring |

#### Anna wrapper behavior

| File | Purpose |
| --- | --- |
| `crates/google-workspace-cli/src/gws_executa.rs` | Executa protocol server, manifest, invoke handling, file transport |
| `crates/google-workspace-cli/EXECUTA.md` | manual testing guide for `gws-executa` |
| `.docs/protocol-spec.zh-CN.md` | Anna/Executa protocol specification |

## Development Rules

### 1. Reuse upstream `gws` behavior

When the request is "support another Google API" or "change CLI behavior", prefer modifying:

- `crates/google-workspace/`
- shared CLI logic in `crates/google-workspace-cli/src/`

Only change the wrapper when the problem is specifically about:

- protocol compliance
- ANNA tool schema
- credential injection
- isolated execution
- file transport
- machine-readable output boundaries

### 2. Do not bypass the wrapper isolation model

`gws-executa` intentionally runs the current executable in an internal mode and strips/controls environment variables before invoking embedded `gws`.

Preserve these properties:

- isolated child environment
- explicit credential forwarding
- predictable config directory behavior
- no accidental inheritance of unrelated local auth state unless explicitly intended

### 3. Treat ANNA inputs as untrusted

This repository is explicitly designed for agent invocation. Continue to validate:

- `argv` entries
- `cwd`
- project/resource identifiers
- file paths
- text fields that flow into URLs, filesystem paths, or subprocess arguments

Reject:

- control characters
- reserved internal arguments
- malformed JSON-RPC payloads
- invalid directory arguments

### 4. Preserve machine-readable output

For wrapper work:

- never add plain `println!` output in RPC mode unless it is the actual JSON-RPC payload
- prefer structured JSON objects for success/error data
- include enough error context for ANNA to diagnose failures without scraping prose

### 5. Keep tool schemas explicit

When adding or modifying tools in the manifest:

- keep parameter names stable unless there is a strong migration reason
- provide `items` for array parameters
- keep credentials in `credentials`, not in tool parameters
- ensure descriptions are clear for both humans and LLM tool selection

## Input Validation & URL Safety

The upstream validation rules still apply. In particular:

1. file paths must use shared validation helpers where applicable
2. user values embedded in URL path segments must be percent-encoded
3. query parameters should use reqwest `.query()`
4. resource identifiers should be validated before interpolation
5. new validation should include both allow and reject-path tests

Relevant files:

- `crates/google-workspace/src/validate.rs`
- `crates/google-workspace-cli/src/validate.rs`
- `crates/google-workspace-cli/src/helpers/mod.rs`

## Credentials

For Anna/Executa integration, the wrapper currently supports credentials through `context.credentials`:

- `GOOGLE_ACCESS_TOKEN`
  - forwarded internally as `GOOGLE_WORKSPACE_CLI_TOKEN`
- `GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE`
  - forwarded as-is

The wrapper may also fall back to matching environment variables for local development, but protocol-facing integrations should prefer `context.credentials`.

Do not expose secret values in:

- stdout payload summaries
- debug logs
- test snapshots
- docs examples unless explicitly redacted/fake

## Common Change Patterns

### Add or modify a Google API surface

Usually change:

- `crates/google-workspace/src/services.rs`
- `crates/google-workspace/src/discovery.rs`
- possibly CLI helpers/docs if there is extra wrapper behavior

Usually do **not** add a new Google API-specific Rust crate.

### Add or modify Anna tool behavior

Usually change:

- `crates/google-workspace-cli/src/gws_executa.rs`
- `crates/google-workspace-cli/EXECUTA.md`
- relevant tests in `gws_executa.rs`

### Add a new helper command

Follow the existing helper guidance:

- do not wrap a single API call that Discovery already exposes
- helpers should provide orchestration, translation, or multi-step value
- protocol changes should not be mixed unnecessarily with helper logic

## Review Checklist

Before finishing a change, verify:

1. Did the change preserve upstream `gws` behavior unless wrapper adaptation required otherwise?
2. Did `gws-executa` remain JSON-RPC clean on `stdout`?
3. Are logs still on `stderr` only?
4. Are tool schemas and credential declarations still protocol-compliant?
5. Did we keep array parameter `items` declarations where needed?
6. Did we preserve input validation for agent-controlled inputs?
7. If transport behavior changed, did we update `EXECUTA.md` and related tests?

## Useful Commands

```bash
# Build native CLI
cargo build -p google-workspace-cli --bin gws

# Build Anna wrapper
cargo build -p google-workspace-cli --bin gws-executa

# Run wrapper-focused tests
cargo test -p google-workspace-cli gws_executa

# Manual protocol smoke test
echo '{"jsonrpc":"2.0","method":"describe","id":1}' | target/debug/gws-executa
```

## References

- Upstream project: `https://github.com/googleworkspace/cli`
- Anna protocol spec: `.docs/protocol-spec.zh-CN.md`
- Wrapper manual test guide: `crates/google-workspace-cli/EXECUTA.md`
