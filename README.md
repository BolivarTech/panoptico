# Panoptico

[![License](https://img.shields.io/github/license/BolivarTech/panoptico?label=license)](#license)
[![Release](https://img.shields.io/github/v/release/BolivarTech/panoptico?display_name=tag&sort=semver)](https://github.com/BolivarTech/panoptico/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/BolivarTech/panoptico/ci.yml?branch=main&label=tests)](https://github.com/BolivarTech/panoptico/actions/workflows/ci.yml)
[![Last commit](https://img.shields.io/github/last-commit/BolivarTech/panoptico/main)](https://github.com/BolivarTech/panoptico/commits/main)
[![Downloads](https://img.shields.io/github/downloads/BolivarTech/panoptico/total)](https://github.com/BolivarTech/panoptico/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg?logo=rust)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2021-blue.svg)](https://doc.rust-lang.org/edition-guide/rust-2021/index.html)
[![Code style: rustfmt](https://img.shields.io/badge/code_style-rustfmt-brightgreen.svg)](https://github.com/rust-lang/rustfmt)
[![Lint: clippy](https://img.shields.io/badge/lint-clippy-blue.svg)](https://github.com/rust-lang/rust-clippy)
[![Powered by Claude](https://img.shields.io/badge/powered_by-Claude-D97757.svg)](https://www.anthropic.com/claude)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://makeapullrequest.com)

**Automated AI code review for Pull Requests, powered by Claude.**

Standalone Rust binary that integrates into CI/CD pipelines (Azure Pipelines, GitHub Actions, GitLab CI, etc.) to automate code review on Pull Requests. Extracts git diffs, splits them into batches, sends each batch to Claude for analysis via a map-reduce pipeline, validates findings against real diff data, and emits structured reports (human-readable or JSON). Native Azure DevOps integration posts inline PR comments; the JSON output is consumable by any other platform.

### Etymology

The name *Panoptico* is the Spanish form of **panopticon**, the institutional architecture conceived by English philosopher **Jeremy Bentham** in 1791: a single observer at the center can watch every cell without being seen, so the inmates self-discipline as if always observed. **Michel Foucault** later took the panopticon as the canonical metaphor for modern disciplinary power — visibility producing behavior. This tool inherits the idea, not the dystopia: every diff is examined as if a senior reviewer were present at every Pull Request, without anyone having to be.

---

## Table of Contents

- [Code Privacy](#code-privacy)
- [Features](#features)
- [Architecture](#architecture)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Usage](#usage)
  - [Test API Connection](#test-api-connection)
  - [Review a Pull Request](#review-a-pull-request)
  - [Using Claude Code as Backend](#using-claude-code-as-backend)
  - [Encrypt an API Key](#encrypt-an-api-key)
- [Configuration](#configuration)
  - [TOML File](#toml-file)
  - [Minimal Configuration](#minimal-configuration)
  - [Configuration Precedence](#configuration-precedence)
  - [Endpoint Resolution](#endpoint-resolution)
  - [API Key Resolution](#api-key-resolution-credential-sources)
  - [Environment Variables](#environment-variables)
- [Backends](#backends)
- [Parallelization Modes](#parallelization-modes)
- [CI/CD Integration](#cicd-integration)
- [CLI Reference](#cli-reference)
- [Development](#development)
- [Known Issues](#known-issues)
- [Roadmap](#roadmap)
- [Author](#author)
- [License](#license)

---

## Code Privacy

1. **Configurable endpoint** — You choose where your code goes. Panoptico supports private Azure AI Foundry deployments, the direct Anthropic API, AWS Bedrock, or local Claude Code (OAuth) — there is no hardcoded destination. Select the backend that matches your data governance requirements.

2. **Data minimization** — The tool processes diffs locally on the build agent and sends only the relevant hunk context to the AI model. Full source code never leaves the build agent. This limits the surface of code exposure compared to solutions that ingest entire files or repositories.

3. **Stateless processing** — Anthropic's APIs (direct, AWS Bedrock, Azure AI Foundry) process each request statelessly and contractually exclude API traffic from model training. Verify your chosen backend's specific data handling policy before adopting it for sensitive code.

4. **Encrypted credentials** — API keys can be stored as AES-256-GCM-SIV encrypted blobs (Argon2 KDF + Reed-Solomon error correction) in configuration files or pipeline secret variables, avoiding plaintext secrets in any environment.

---

## Features

- **Map-Reduce pipeline** -- splits diffs into atomic hunks, groups into batches, reviews each independently, then synthesizes a consolidated report
- **Multiple backends** -- Azure AI Foundry, direct Anthropic API, AWS Bedrock, and Claude Code CLI
- **Prompt caching** -- up to 90% cost savings on batches 2+ (Azure and Anthropic backends)
- **Parallel batch processing** -- three modes: sequential, hybrid (cache-optimized), and full parallel with configurable concurrency
- **Hallucination guard** -- validates all findings against the actual diff file set before output
- **Flexible configuration** -- TOML config file with full CLI override support and config generation commands
- **Cost tracking** -- token usage accumulation and estimated cost reporting per review session
- **Secure credential storage** -- encrypted API keys in TOML via Argon2 KDF + AES-256-GCM-SIV + Reed-Solomon error correction
- **Structured output** -- human-readable text or JSON, to stdout or file

---

## Architecture

```
Git Diff --> Hunk Parser --> Batch Builder --> Map (LLM) --> Reduce --> Validator --> Output
                                                |
                                       +--------+--------+
                                       v                  v
                               HTTP Backends        Claude Code CLI
                           (Azure, Anthropic,       (local subprocess)
                               Bedrock)
```

| Step | Description |
|------|-------------|
| **Git Diff** | Extract diff between `--base-ref` and `--target-ref` (default: HEAD), filtered by extensions |
| **Hunk Parser** | Split each file's diff into atomic hunks at `@@` markers; file header prepended to each |
| **Batch Builder** | Greedy grouping of hunks into batches respecting `--max-lines` (default: 500) |
| **Map** | Send each batch to Claude via the selected backend for independent review |
| **Reduce** | Synthesize batch reviews into a single consolidated report (skipped for single-batch reviews) |
| **Validator** | Remove findings referencing files not present in the diff (hallucination guard) |
| **Output** | Human-readable text or JSON (`--json`), to stdout or file (`--output`) |

---

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) 1.70+
- Git

### Build from Source

```bash
# GitHub:
git clone https://github.com/BolivarTech/panoptico.git

# Azure DevOps:
git clone https://dev.azure.com/<your-org>/<your-project>/_git/Panoptico

cd panoptico
cargo build --release
```

The binary will be at `target/release/panoptico` (`panoptico.exe` on Windows).

### Download Prebuilt Binaries

Each tagged release publishes prebuilt binaries on the [Releases page](https://github.com/BolivarTech/panoptico/releases) for the two platforms most common in DevOps pipelines:

| Platform | Asset | Compatibility |
|---|---|---|
| Windows x86_64 | `panoptico-vX.Y.Z-x86_64-pc-windows-msvc.zip` | Windows 10/11, Windows Server 2019+ |
| Linux x86_64 (Debian/Ubuntu) | `panoptico-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | Debian 11+, Ubuntu 20.04+, and other glibc-based distros with OpenSSL 3.x |

> [!NOTE]
> The Linux binary is built on Ubuntu (glibc + OpenSSL 3.x) — the target environment of most DevOps runners (Azure Pipelines, GitHub Actions, GitLab CI, Jenkins on Debian/Ubuntu agents). It is **not** compatible with musl-based distributions (Alpine, etc.); for those, build from source.

Each archive ships with a matching `.sha256` checksum file for integrity verification, and bundles `LICENSE-MIT`, `LICENSE-APACHE`, and `README.md` alongside the binary.

**Consuming from a pipeline:**

```bash
# Linux
VERSION="v1.1.0"
ASSET="panoptico-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
curl -fsSL "https://github.com/BolivarTech/panoptico/releases/download/${VERSION}/${ASSET}" -o "${ASSET}"
curl -fsSL "https://github.com/BolivarTech/panoptico/releases/download/${VERSION}/${ASSET}.sha256" | sha256sum -c -
tar -xzf "${ASSET}"
./panoptico-${VERSION}-x86_64-unknown-linux-gnu/panoptico --help
```

```powershell
# Windows
$Version = "v1.1.0"
$Asset = "panoptico-$Version-x86_64-pc-windows-msvc.zip"
Invoke-WebRequest "https://github.com/BolivarTech/panoptico/releases/download/$Version/$Asset" -OutFile $Asset
Expand-Archive $Asset -DestinationPath .
& ".\panoptico-$Version-x86_64-pc-windows-msvc\panoptico.exe" --help
```

### Generate Default Configuration

```bash
panoptico config init          # Creates panoptico.toml
panoptico config init-prompt   # Creates ai-prompt.txt (system prompt template)
```

> [!NOTE]
> The generated template is oriented toward local development (uses `claude-code` backend with OAuth authentication). For production or CI/CD pipelines, adjust the `backend`, `endpoint`, and `credential_source` settings to match your environment (e.g., `backend = "azure"` with `credential_source = "env"`).

---

## Quick Start

```bash
# 1. Set your API key
export AZURE_AI_API_KEY="your-api-key"
export AZURE_AI_ENDPOINT="https://your-resource.services.ai.azure.com/anthropic/"

# 2. Verify the connection
panoptico test --model claude-sonnet-4-5

# 3. Review changes (human-readable output)
panoptico review --base-ref origin/main

# 4. Review with JSON output saved to file
panoptico review --base-ref origin/main --json -o report.json
```

---

## Usage

### Test API Connection

Verify that the backend is reachable and the model responds.

```bash
# Using environment variables (AZURE_AI_ENDPOINT)
panoptico test --model claude-sonnet-4-5

# Explicit endpoint
panoptico test \
  --endpoint "https://your-resource.services.ai.azure.com/anthropic/" \
  --model claude-sonnet-4-5
```

### Review a Pull Request

```bash
# Basic review (human-readable output to stdout)
panoptico review --base-ref origin/main

# JSON output to stdout
panoptico review --base-ref origin/main --json

# Save output to a file
panoptico review --base-ref origin/main --json -o report.json

# Review a specific commit range
panoptico review --base-ref HEAD~3 --target-ref HEAD

# Review only C/C++ and Rust files
panoptico review \
  --base-ref origin/main \
  --extensions "*.c,*.cpp,*.h,*.rs"

# Use the Anthropic API directly
panoptico review \
  --base-ref origin/main \
  --backend anthropic \
  --model claude-sonnet-4-5-20250929

# Parallel review with prompt caching (hybrid mode)
panoptico review \
  --base-ref origin/develop \
  --parallel hybrid \
  --max-concurrent 4 \
  --cache \
  --cost-report

# Full parallel review for speed priority
panoptico review \
  --base-ref origin/main \
  --parallel full \
  --max-concurrent 8

# Use a custom TOML config file
panoptico -c team-config.toml review --base-ref origin/main

# Use custom system prompt and review instructions
panoptico review \
  --base-ref origin/main \
  --system-prompt ai-prompt.txt \
  --instructions review-instructions.md

# Review with smaller batch size for large diffs
panoptico review \
  --base-ref origin/main \
  --max-lines 300
```

### Using Claude Code as Backend

The Claude Code backend uses the locally installed `claude` CLI as a subprocess. It authenticates via OAuth (no API keys or endpoints required), making it ideal for local development and prompt iteration.

**Prerequisites:** [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) installed and authenticated (`claude` available in PATH).

```bash
# Basic review using Claude Code
panoptico review \
  --base-ref origin/main \
  --backend claude-code

# With a specific model and JSON output
panoptico review \
  --base-ref origin/main \
  --backend claude-code \
  --model claude-sonnet-4-5-20250929 \
  --json

# Full parallel with cost report
panoptico review \
  --base-ref origin/main \
  --backend claude-code \
  --parallel full \
  --max-concurrent 4 \
  --cost-report
```

**TOML configuration for Claude Code** (`panoptico.toml`):

```toml
[review]
model = "claude-sonnet-4-5-20250929"
backend = "claude-code"
max_lines_per_batch = 300
extensions = ["*.rs", "*.py", "*.ts"]
parallel = "full"
max_concurrent = 4

[review.cost]
report = true
```

> [!NOTE]
> Claude Code does not support prompt caching. Using `--parallel hybrid` with `--backend claude-code` is automatically promoted to `full` since the hybrid cache-write step provides no benefit.

### Encrypt an API Key

Store an encrypted API key in the TOML config file instead of using environment variables.

```bash
# 1. Generate the encrypted blob
panoptico config encrypt-key \
  --password "my-secure-password" \
  --api-key "sk-ant-..."

# 2. The command outputs a base64 string and updates panoptico.toml if present.
#    Otherwise, add manually:
#      [azure]
#      credential_source = "encrypted"
#      api_key_encrypted = "<paste-output-here>"

# 3. Pass the password at review time
panoptico review --base-ref origin/main --key-password "my-secure-password"

# Or read password from stdin (avoids shell history)
echo -n "my-secure-password" | panoptico review \
  --base-ref origin/main --key-password-stdin
```

---

## Configuration

### TOML File

Create an `panoptico.toml` file in the working directory (or specify a path with `-c` / `--config`).

```toml
[review]
model = "claude-sonnet-4-5"
fallback_model = "claude-haiku-4-5"
backend = "azure"                      # "azure" | "anthropic" | "aws-bedrock" | "claude-code"
max_lines_per_batch = 500
# system_prompt = "ai-prompt.txt"      # Path to custom system prompt file
# instructions = "review-instructions.md"
extensions = [
    "*.c", "*.cpp", "*.h", "*.hpp",
    "*.rs", "*.py",
    "*.js", "*.jsx", "*.ts", "*.tsx",
    "*.cs", "*.java", "*.kt", "*.go",
]
parallel = "hybrid"                    # "none" | "hybrid" | "full"
max_concurrent = 4

[review.cache]
enabled = true

[review.cost]
report = true

[azure]
endpoint = "https://your-resource.services.ai.azure.com/anthropic/"
credential_source = "env"              # "env" | "keyring" | "encrypted" | "vault"
# api_key_encrypted = ""               # Base64 blob (when credential_source = "encrypted")
# vault_url = ""                       # Key Vault URL (when credential_source = "vault")
# vault_secret_name = ""               # Secret name  (when credential_source = "vault")

[platform]
type = "azure-devops"
org_url = "https://dev.azure.com/YourOrg"
project = "YourProject"
```

### Minimal Configuration

Only the model name is required. Everything else uses sensible defaults.

```toml
[review]
model = "claude-sonnet-4-5"
```

### Configuration Precedence

Settings are resolved in this order (later overrides earlier):

1. **TOML file** (`panoptico.toml`) -- base defaults
2. **CLI flags** -- selective overrides via `Option<T>`
3. **Environment variables** -- for secrets and endpoint fallback

### Endpoint Resolution

The Azure AI Foundry endpoint (`AZURE_AI_ENDPOINT`) can be provided from any of these sources. The first one found is used:

| Priority | Source | Example |
|----------|--------|---------|
| 1 | CLI flag | `--endpoint "https://..."` |
| 2 | TOML config | `[azure] endpoint = "https://..."` |
| 3 | Environment variable | `AZURE_AI_ENDPOINT=https://...` |

If the endpoint is set in the TOML file, no environment variable is needed.

### API Key Resolution (Credential Sources)

The API key source is controlled by `credential_source` in the `[azure]` TOML section. Environment variables are only required when using the `"env"` source (the default).

| Source | TOML Value | Use Case | How It Works |
|--------|------------|----------|--------------|
| Environment | `"env"` | CI/CD pipelines (default) | Reads `AZURE_AI_API_KEY` env var; if `--key-password` is provided, decrypts the value as an encrypted blob |
| Encrypted | `"encrypted"` | Portable config | Decrypts AES-256-GCM-SIV blob from TOML with a password |
| Keyring | `"keyring"` | Developer local | OS credential store |
| Vault | `"vault"` | Enterprise | Azure Key Vault via managed identity |

**Using encrypted credentials (no env vars needed):**

```toml
[azure]
endpoint = "https://your-resource.services.ai.azure.com/anthropic/"
credential_source = "encrypted"
api_key_encrypted = "<base64-blob-from-encrypt-key-command>"
```

```bash
# Pass password at review time
panoptico review --base-ref origin/main --key-password "my-password"

# Or from stdin (avoids shell history)
echo -n "my-password" | panoptico review --base-ref origin/main --key-password-stdin
```

The encrypted source uses a hardened cryptographic pipeline: **Argon2** key derivation (brute-force resistant) produces both key and nonce from the password, **AES-256-GCM-SIV** provides nonce-misuse resistant authenticated encryption, and **Reed-Solomon** error correction recovers up to 16 corrupted bytes per block.

**Using encrypted env var (no TOML blob needed):**

You can also store the encrypted blob in the `AZURE_AI_API_KEY` environment variable instead of the TOML file. When `credential_source = "env"` and `--key-password` is provided, the env var value is treated as an encrypted blob and decrypted automatically:

```bash
# 1. Encrypt the key
panoptico config encrypt-key --password "my-password" --api-key "sk-ant-..."
# 2. Store the output blob in a secret pipeline variable (AZURE_AI_API_KEY)
# 3. Pass the password at review time
panoptico review --base-ref origin/main --key-password "my-password"
```

This is useful in CI/CD pipelines where the encrypted blob is stored as a secret variable, avoiding both plaintext keys and TOML file management.

### Environment Variables

These are only required when the corresponding setting is not provided via TOML or CLI flags.

| Variable | When Required | Description |
|----------|---------------|-------------|
| `AZURE_AI_ENDPOINT` | Only if not set in TOML or CLI | Azure AI Foundry endpoint URL |
| `AZURE_AI_API_KEY` | Only with `credential_source = "env"` (default) | Azure AI Foundry API key |
| `AZURE_DEVOPS_TOKEN` | PR comment posting | PAT or `$(System.AccessToken)` |

---

## Backends

| Backend | Flag | Auth | Model Names |
|---------|------|------|-------------|
| Azure AI Foundry | `--backend azure` | `x-api-key` header | Deployment names (`claude-sonnet-4-5`) |
| Direct Anthropic | `--backend anthropic` | `x-api-key` header | Versioned (`claude-sonnet-4-5-20250929`) |
| AWS Bedrock | `--backend aws-bedrock` | AWS Signature V4 | ARN/ID (`anthropic.claude-sonnet-4-5-v2`) |
| Claude Code CLI | `--backend claude-code` | OAuth (local) | Same as Anthropic |

All HTTP backends use the Anthropic Messages API body format with `tool_use` for structured JSON output.

---

## Parallelization Modes

| Mode | Flag | Behavior | Cache Benefit | Best For |
|------|------|----------|---------------|----------|
| Sequential | `--parallel none` | One batch at a time | Full (90% savings) | Small PRs, cost optimization |
| Hybrid | `--parallel hybrid` | Batch 1 first, rest in parallel | Partial | Balanced speed and cost |
| Full | `--parallel full` | All batches in parallel | None | Speed priority |

Concurrency is capped by `--max-concurrent` (default: 4) via `tokio::sync::Semaphore`.

> [!NOTE]
> Using `--backend claude-code` with `--parallel hybrid` is automatically promoted to `full` because the CLI does not support prompt caching.

---

## CI/CD Integration

### Azure Pipelines

Add a review step to your `azure-pipelines.yml`:

```yaml
- script: |
    panoptico review \
      --base-ref origin/$(System.PullRequest.TargetBranch) \
      --target-ref HEAD \
      --backend azure \
      --parallel hybrid \
      --cache \
      --cost-report
  displayName: 'AI Code Review'
  env:
    AZURE_AI_ENDPOINT: $(AZURE_AI_ENDPOINT)
    AZURE_AI_API_KEY: $(AZURE_AI_API_KEY)
    AZURE_DEVOPS_TOKEN: $(System.AccessToken)
```

For JSON output (e.g., for downstream tooling or artifact publishing):

```yaml
- script: |
    panoptico review \
      --base-ref origin/$(System.PullRequest.TargetBranch) \
      --json -o $(Build.ArtifactStagingDirectory)/review-report.json
  displayName: 'AI Code Review (JSON)'
  env:
    AZURE_AI_ENDPOINT: $(AZURE_AI_ENDPOINT)
    AZURE_AI_API_KEY: $(AZURE_AI_API_KEY)
```

---

## CLI Reference

### Global Options

| Option | Description | Default |
|--------|-------------|---------|
| `-c`, `--config` | Path to TOML configuration file | `panoptico.toml` |

### `panoptico test`

Test API connection and print diagnostics.

| Option | Description | Default |
|--------|-------------|---------|
| `--endpoint` | API endpoint URL | env `AZURE_AI_ENDPOINT` |
| `--model` | Model deployment name | config default |

### `panoptico review`

Review PR changes against a base branch.

| Option | Description | Default |
|--------|-------------|---------|
| `--base-ref` | Git reference to diff against | `origin/main` |
| `--target-ref` | Git reference to diff towards | `HEAD` |
| `--backend` | `azure`, `anthropic`, `aws-bedrock`, `claude-code` | `azure` |
| `--model` | Model deployment name | `claude-sonnet-4-5` |
| `--fallback-model` | Fallback model for rate-limit retries | -- |
| `--endpoint` | API endpoint URL | env `AZURE_AI_ENDPOINT` |
| `--extensions` | File patterns, comma-separated | all files |
| `--max-lines` | Maximum lines per review batch | `500` |
| `--system-prompt` | Path to custom system prompt file | built-in default |
| `--instructions` | Path to custom review instructions file | -- |
| `--parallel` | `none`, `hybrid`, `full` | `none` |
| `--max-concurrent` | Max parallel API calls | `4` |
| `--cache` / `--no-cache` | Enable/disable prompt caching | config default |
| `--cost-report` / `--no-cost-report` | Enable/disable cost report | config default |
| `--key-password` | Password to decrypt encrypted API key | -- |
| `--key-password-stdin` | Read decryption password from stdin | `false` |
| `--json` | Output raw JSON instead of human-readable text | `false` |
| `-o`, `--output` | Write output to a file instead of stdout | -- |

### `panoptico config`

| Subcommand | Description |
|------------|-------------|
| `init` | Generate a default `panoptico.toml` in the current directory |
| `init-prompt` | Generate a default `ai-prompt.txt` system prompt template |
| `encrypt-key` | Encrypt an API key for secure TOML storage |

**`encrypt-key` options:**

| Option | Description |
|--------|-------------|
| `--password` | Password for key derivation (Argon2) |
| `--api-key` | Plaintext API key to encrypt |

---

## Development

```bash
cargo build                          # Build debug
cargo build --release                # Build release (LTO + strip)
cargo test                           # Run all 309 unit + 14 doc-tests
cargo test config::tests             # Run tests for a single module
cargo test config::tests::from_file  # Run a single test by name prefix
cargo clippy -- -D warnings          # Lint (0 warnings)
cargo fmt --check                    # Format check
```

See [`docs/App_Implementation_Report.md`](docs/App_Implementation_Report.md) for the full implementation roadmap and module documentation.

---

## Known Issues

The AI review may generate false positives (flagging correct code as problematic). The current false positive rate is estimated at ~30%. A [Semantic Review Pipeline](docs/panoptico-semantic.md) is planned to reduce this to <10% through four complementary strategies:

| Strategy | Status | Description |
|----------|--------|-------------|
| **Confidence scoring** | Planned | LLM self-reports certainty per finding; low-confidence results are filtered by severity-specific thresholds |
| **Deterministic merge** | Planned | Replaces the LLM synthesis phase with programmatic deduplication, eliminating a source of hallucinated findings |
| **Line range validation** | Planned | Rejects findings that reference lines outside the actual changed range (±5 line margin) |
| **Semantic context** | Planned | Sends complete functions to the LLM instead of diff fragments, providing full context for more accurate analysis |

Currently, `panoptico.exe` mitigates false positives via a **hallucination guard** that removes findings referencing files not present in the diff, and through prompt tuning (`--system-prompt`, `--instructions`) and model selection (`--model`).

---

## Roadmap

Future versions will extend Panoptico's accuracy and depth of analysis through two complementary integrations.

### RAG-augmented review

Retrieval-Augmented Generation will let the reviewer pull additional context beyond the diff itself before analyzing a change. Planned sources:

- **Repository code graph** — definitions of called functions, types referenced in the hunk, and immediate callers, fetched from the working tree.
- **Project documentation** — `README.md`, `CLAUDE.md`, `docs/`, ADRs, and inline rustdoc/docstrings, indexed and retrieved by semantic similarity to the diff content.
- **Historical review findings** — past validated findings on related code, used as in-context examples to anchor judgment and reduce repeat false positives.

The expected effect is a meaningful reduction in context-blind hallucinations (e.g., flagging a function as missing when it is defined in a sibling module).

### Multi-perspective consensus via MAGI

The **MAGI methodology** — inspired by the MAGI supercomputers from *Neon Genesis Evangelion* — dispatches every query through three independent AI personas with distinct lenses:

- **Melchior** — scientist (rigor, evidence, formal correctness)
- **Balthasar** — pragmatist (maintainability, real-world tradeoffs, ergonomics)
- **Caspar** — adversarial (failure modes, edge cases, attacker mindset)

Their verdicts are reconciled through weight-based consensus voting, producing a single answer plus a quantifiable agreement signal.

**Production track record.** MAGI is already shipping as a [Claude Code plugin and Gemini CLI plugin](https://github.com/BolivarTech/magi), where it has delivered **exceptional performance in real-world use** — markedly reducing single-perspective bias on architectural reviews, design decisions, and code analysis. Users report sharper recommendations and far fewer overlooked edge cases than single-agent dispatch.

**Integration plan for Panoptico.** A future release will embed [magi-core](https://github.com/BolivarTech/magi-core) — the **native Rust implementation of MAGI**, LLM-agnostic by design — directly into the review pipeline. Each batch will be routed through Melchior / Balthasar / Caspar before a finding is emitted.

Expected benefits:

- **Lower false positive rate** — findings only one persona detects are demoted or dropped, reducing single-perspective noise.
- **Built-in confidence scoring** — agreement across personas becomes a measurable confidence signal attached to each finding.
- **LLM-agnostic dispatch** — `magi-core` abstracts the backend, so the three perspectives can use different models (e.g., one Claude Opus, two Claude Sonnet) for cost/quality balance.

Combined, RAG (context) and MAGI (consensus) target the two largest sources of remaining false positives: incomplete information and single-perspective bias.

---

## Author

Julian Bolivar

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
