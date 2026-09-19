# RyGent

A cross-platform desktop workspace for running AI coding agents in isolated, per-project environments.

**RyGent** lets you run multiple AI coding sessions side by side, with each project having its own workspace, terminal, agent process, environment, credentials, and configuration.

Built with **Tauri 2 + Rust + React + TypeScript + Vite**.

> RyGent is the workspace. It does not depend on VS Code or another editor.

## Features

- 🗂️ **Project-based workspaces**
  - Create, edit, reopen, and remove workspaces
  - Each workspace points to its own project directory
  - Workspace configuration is persisted locally

- 🧩 **Multiple isolated sessions**
  - Run multiple AI coding agents simultaneously
  - Each session has its own PTY, process, environment, and configuration
  - Sessions do not share credentials or agent configuration

- 🤖 **Supported agents**
  - **Claude Code** and **OpenAI Codex CLI**
  - Pick the agent per workspace; the chosen one is shown in the session UI
  - Agent integration is abstracted behind a Rust `AgentAdapter` interface
  - Additional agents can be added without redesigning the workspace architecture

- 💻 **Real terminal**
  - Full interactive PTY-based terminal
  - xterm.js rendering
  - ANSI output and interactive applications
  - Copy, paste, scrolling, and keyboard interaction
  - Windows ConPTY and Unix PTY support through `portable-pty`

- 🔐 **Secure credentials**
  - Provider credentials are stored in the operating system's secure credential store
  - API keys are not stored in SQLite
  - Credentials are injected into the agent process only when a session starts
  - The React frontend does not receive provider secrets

- 🔌 **Custom AI providers**
  - Anthropic-compatible endpoints
  - Custom base URLs
  - Custom models
  - Optional model context-window configuration
  - Additional environment variables per provider

- 💾 **Persistent configuration**
  - SQLite for workspace and provider metadata
  - OS-native secure storage for credentials
  - Open tabs and active workspace are restored between application launches

- 🛡️ **Process isolation**
  - Each session runs independently
  - Windows Job Objects provide an OS-level backstop against orphaned processes
  - Session shutdown uses bounded graceful and forced termination paths

## Architecture

RyGent keeps the UI and native capabilities clearly separated.

```text
┌─────────────────────────────────────────────┐
│                 RyGent UI                   │
│          React + TypeScript + Vite          │
├─────────────────────────────────────────────┤
│              Tauri Command Layer            │
├─────────────────────────────────────────────┤
│                 Rust Core                   │
│                                             │
│  Workspaces    Sessions    Providers        │
│  Persistence   PTY         Secrets          │
│  Process       Agents      Lifecycle        │
├─────────────────────────────────────────────┤
│                                             │
│  SQLite    OS Keyring    Claude Code/Codex  │
│                                             │
└─────────────────────────────────────────────┘
```

The React layer is primarily responsible for presentation and application state.

Native capabilities such as:

- process management
- PTY handling
- filesystem validation
- SQLite persistence
- credential storage
- agent execution
- lifecycle management

are implemented in Rust.

Each supported agent is a module under `src-tauri/src/agents/` implementing the `AgentAdapter` trait: which executable it looks for, which environment a session receives, and which arguments it launches with. A single registry decides what the UI is offered, what a stored workspace may name, and which adapter a session gets, so adding an agent means one new module plus one registry entry — the session, PTY and process code is shared and unchanged.

## Requirements

### Node.js

Node.js **22.x LTS** is recommended.

Check your installation:

```bash
node --version
npm --version
```

RyGent uses Vite 8, which requires Node.js 20.19+ or 22.12+.

### Rust

Install Rust using `rustup`:

https://rustup.rs

Verify:

```bash
rustc --version
cargo --version
```

### Platform build tools

#### Windows

Install Visual Studio Build Tools with:

- Desktop development with C++
- MSVC
- Windows SDK

You can also install it with:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

#### macOS

Install Xcode Command Line Tools:

```bash
xcode-select --install
```

#### Linux

Install the system dependencies required by Tauri 2 for your distribution.

See the official Tauri prerequisites:

https://v2.tauri.app/start/prerequisites/

### Agents

An agent CLI must be installed and available on `PATH`. RyGent ships two adapters:

| Agent | Executable | Install |
| --- | --- | --- |
| Claude Code | `claude` | https://code.claude.com/docs/en/overview |
| OpenAI Codex CLI | `codex` | https://developers.openai.com/codex/cli/ |

Verify either one:

```bash
claude --version
codex --version
```

Restart RyGent after installing an agent so the application receives the updated `PATH`. Settings → **About / Storage** lists every agent this build implements with the path it resolved, or `not found on PATH`.

You do not have to install both. A workspace that names a missing agent still saves; starting a session then reports the same message instead of failing silently.

### Windows WebView2

Windows builds use WebView2.

Windows 11 and current Windows 10 installations normally include WebView2. If it is missing, install the Microsoft Edge WebView2 Runtime.

## Getting Started

Clone the repository:

```bash
git clone https://github.com/Ryan-PG/RyGent.git
cd RyGent
```

Install frontend dependencies:

```bash
npm install
```

Run the frontend only:

```bash
npm run dev
```

This starts Vite on:

```text
http://localhost:1420
```

For the complete desktop application:

```bash
npm run tauri dev
```

## Build

Build the frontend:

```bash
npm run build
```

Run the frontend tests:

```bash
npm test
```

Build the desktop application:

```bash
npm run tauri build
```

Tauri will build the frontend first and then create the platform-specific application bundles.

On Windows, the generated installers are located under:

```text
src-tauri/target/release/bundle/
```

Typically:

```text
src-tauri/target/release/bundle/
├── msi/
│   └── *.msi
└── nsis/
    └── *.exe
```

## Using RyGent

### 1. Configure a Provider

Open the **Providers** panel and create a provider profile.

A provider contains:

| Field                       | Description                                  |
| --------------------------- | -------------------------------------------- |
| Name                        | Display name for the provider                |
| Base URL                    | API endpoint for the agent's protocol        |
| API Key                     | Provider credential                          |
| Model                       | Default model identifier                     |
| Max Context Tokens          | Optional window size (Claude Code only)      |
| Extra Environment Variables | Optional provider-specific variables         |

The API key is stored in the operating system's secure credential storage.

### 2. Create a Workspace

Click **+ New Workspace**.

Configure:

- Workspace name
- Project directory
- Agent (**Claude Code** or **Codex**)
- Provider
- Model

RyGent validates the project directory and provider before creating the workspace. If the chosen agent is not installed, the form says so but still saves the workspace.

### 3. Start a Session

Open the workspace and press **Start**.

RyGent creates an isolated session containing:

```text
Project directory
       +
Selected agent process (Claude Code or Codex)
       +
Dedicated PTY
       +
Provider environment
       +
Session-specific configuration directory
```

Each agent uses its own configuration directory (Claude Code: `CLAUDE_CONFIG_DIR`, Codex: `CODEX_HOME`), so no two sessions share state.

The session runs independently from other workspaces.

### 4. Run Multiple Sessions

You can open multiple workspaces simultaneously.

For example:

```text
┌────────────────────────────────────────────────────┐
│ Project A │ Project B │ Project C │                 │
├────────────────────────────────────────────────────┤
│                                                    │
│ Claude Code session                                │
│                                                    │
│ > Analyze the authentication system                │
│                                                    │
└────────────────────────────────────────────────────┘
```

Each session maintains its own:

- process
- terminal
- working directory
- environment
- credentials
- agent configuration

Sessions of different agents coexist in the same window, each with its own PTY and process.

## Provider Credentials

RyGent stores provider credentials separately from normal application data.

### Storage

| Data                | Storage                  |
| ------------------- | ------------------------ |
| Provider metadata   | SQLite                   |
| Workspace metadata  | SQLite                   |
| UI preferences      | SQLite                   |
| API keys            | OS-native secure storage |
| Session environment | In memory                |

The exact secure storage mechanism depends on the operating system:

- Windows Credential Manager
- macOS Keychain
- Linux Secret Service

### Authentication

A provider profile supplies the credential for whichever agent the workspace runs. RyGent exposes it under the variable that agent documents:

| Agent | Variable | Values also sent |
| --- | --- | --- |
| Claude Code | `ANTHROPIC_AUTH_TOKEN` | `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`, `CLAUDE_CONFIG_DIR` |
| Codex | `OPENAI_API_KEY` | `OPENAI_BASE_URL`, `CODEX_HOME` |

Claude Code uses `ANTHROPIC_AUTH_TOKEN` as:

```text
Authorization: Bearer <token>
```

RyGent does not automatically expose the stored credential as `ANTHROPIC_API_KEY`.

For gateways that specifically require an API-key header, `ANTHROPIC_API_KEY` can be supplied through the provider's additional environment variables.

> Extra environment variables are stored as provider metadata. Do not use them for secrets unless you understand the storage implications.

### Model

The model comes from the provider profile or the workspace override:

- Claude Code receives it as `ANTHROPIC_MODEL`.
- Codex receives it as the `--model` flag, which outranks its own config file — Codex has no documented environment variable for the model.

## Model Context Window

Some Anthropic-compatible providers expose models that Claude Code does not recognize in its built-in model catalog.

For such models, RyGent provides:

```text
Max Context Tokens
```

For example:

```text
200000
```

The value is passed to the session as:

```text
CLAUDE_CODE_MAX_CONTEXT_TOKENS
```

Leave the field empty if you want Claude Code to use its own default behavior.

The correct value should match the actual context window supported by the provider's model.

> This setting applies to Claude Code only. Codex has no equivalent variable, so RyGent does not forward a context window to a Codex session rather than risk an unknown configuration key.

## Terminal

RyGent uses xterm.js with a real PTY-backed session.

### Keyboard shortcuts

| Shortcut                   | Action                                  |
| -------------------------- | --------------------------------------- |
| `Ctrl+C` with selection    | Copy selection                          |
| `Ctrl+C` without selection | Send interrupt to the agent             |
| `Ctrl+Shift+C`             | Copy selection                          |
| `Ctrl+Insert`              | Copy selection                          |
| `Ctrl+V`                   | Native paste                            |
| `Ctrl+Shift+V`             | Paste through RyGent clipboard handling |
| `Shift+PageUp`             | Scroll up                               |
| `Shift+PageDown`           | Scroll down                             |
| `Ctrl+Shift+Home`          | Scroll to top                           |
| `Ctrl+Shift+End`           | Scroll to bottom                        |

The terminal maintains up to 10,000 lines of scrollback.

Right-clicking the terminal provides:

- Copy
- Copy all
- Paste
- Select all
- Scroll to top
- Scroll to bottom
- Clear

### Alternate screen

Full-screen terminal applications such as editors and pagers may use the terminal's alternate screen buffer.

In that mode, scrolling is controlled by the application itself rather than RyGent's local scrollback.

## Security

RyGent is designed to keep provider credentials outside the frontend and normal application database.

### Credential isolation

Provider API keys:

- are stored in the OS credential manager
- are not stored in SQLite
- are not sent to the React frontend
- are not written to application logs
- are injected into the relevant agent process only

### Session isolation

Each session receives its own environment.

For example:

```text
Session A (Claude Code)
├── Project A
├── Provider A
├── Credential A
├── CLAUDE_CONFIG_DIR A
└── claude process A

Session B (Codex)
├── Project B
├── Provider B
├── Credential B
├── CODEX_HOME B
└── codex process B
```

Starting or stopping one session does not modify another session's environment.

### Process cleanup

RyGent attempts graceful shutdown first and uses forced termination when necessary.

On Windows, sessions use a Windows Job Object with kill-on-close behavior to reduce the risk of orphaned agent processes when the application itself is terminated unexpectedly.

## Project Structure

```text
.
├── README.md
├── package.json
├── vite.config.ts
├── vitest.config.ts
├── index.html
├── src/
│   ├── App.tsx
│   ├── components/
│   ├── services/
│   ├── stores/
│   ├── test/
│   ├── types/
│   ├── styles.css
│   └── main.tsx
└── src-tauri/
    ├── Cargo.toml
    ├── capabilities/
    ├── tauri.conf.json
    └── src/
        ├── agents/
        ├── commands/
        ├── persistence/
        ├── process/
        ├── providers/
        ├── pty/
        ├── secrets/
        ├── sessions/
        ├── workspaces/
        ├── lib.rs
        └── main.rs
```

## Development

Useful commands:

```bash
# Install dependencies
npm install

# Start Vite
npm run dev

# Build frontend
npm run build

# Run tests
npm test

# Start Tauri development environment
npm run tauri dev

# Build desktop application
npm run tauri build
```

For Rust development:

```bash
cd src-tauri

cargo check
cargo test
```

A plain `cargo build` may require the frontend `dist/` directory to exist first. The Tauri commands handle the frontend build automatically through the configured `beforeBuildCommand`.

## Troubleshooting

### An agent CLI is not found

Check the one the workspace runs:

```bash
claude --version
codex --version
```

If it works in your terminal but not in RyGent, restart the application so it receives the current `PATH`.

### `cargo` or `rustup` is not found on Windows

Rust normally installs Cargo under:

```text
%USERPROFILE%\.cargo\bin
```

Open a new terminal after installing Rust.

For PowerShell, you can temporarily update the current session:

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
```

Then verify:

```powershell
cargo --version
```

### Tauri build fails because `dist` does not exist

Run:

```bash
npm run build
```

Or use:

```bash
npm run tauri build
```

The Tauri build command runs the frontend build automatically.

### Windows application window is blank

Make sure WebView2 is installed.

The application requires the Microsoft Edge WebView2 Runtime.

### Port 1420 is already in use

Stop the process using port `1420`, then run:

```bash
npm run tauri dev
```

### Provider authentication fails

Check:

- Base URL
- API key
- Model name
- Provider compatibility
- Max Context Tokens
- Additional environment variables

RyGent sends the stored credential as:

```text
Authorization: Bearer <token>
```

If the provider specifically requires `X-Api-Key`, configure:

```text
ANTHROPIC_API_KEY
```

through the provider's extra environment variables.

### Terminal output is missing after switching tabs

Background sessions continue running, but output produced while a terminal is not visible is not currently replayed when returning to that tab.

The session itself remains active.

### `Ctrl+C` behaves differently depending on selection

This is intentional:

```text
Ctrl+C + selected text
    → copy

Ctrl+C + no selection
    → interrupt agent
```

Use `Ctrl+Shift+C` when you always want copy behavior.

## Contributing

Contributions are welcome.

Before submitting a change:

```bash
npm install
npm run build
npm test
cargo check
cargo test
```

Use **Conventional Commits** for commit messages:

```text
feat: add provider management
fix: prevent orphaned sessions
docs: improve installation guide
refactor: simplify session manager
test: add workspace lifecycle tests
chore: update dependencies
```

Release versions are managed automatically using **Release Please** and Semantic Versioning.

Examples:

```text
fix: ...       → patch release
feat: ...      → minor release
feat!: ...     → major release
```

## Releases

RyGent uses GitHub Actions for automated releases.

The release flow is:

```text
Conventional Commit
        ↓
       main
        ↓
   Release Please
        ↓
    Release PR
        ↓
   Merge Release PR
        ↓
    Git tag / Release
        ↓
  Platform build
        ↓
   Release artifacts
```

Windows builds produce installers such as:

```text
.msi
.exe
```

Additional platforms can be added to the release pipeline as their build environments are verified.

## License

RyGent is free and open-source software licensed under the
**GNU Affero General Public License v3.0 (AGPL-3.0)**.

Copyright (C) 2026 Ryan Heida.

See [LICENSE](LICENSE) for the complete license text.
