# CLAUDE.md

Guidance for Claude Code when working in this repo.

## Repo purpose

Personal automation scripts and pipelines. Each workflow lives in its own subdirectory with a `README.md`, `setup.sh`, and self-contained dependencies.

## Conventions

- **One directory per workflow.** Don't mix concerns across directories.
- **No shared lib directory** unless two or more workflows genuinely need the same code.
- **Python scripts** use `#!/usr/bin/env python3` and are marked executable.
- **Shell scripts** use `#!/usr/bin/env bash` with `set -euo pipefail`.
- **Config files** are YAML, named `config.yaml`, and kept in the workflow directory.
- **No comments** unless the why is non-obvious.

## Adding a new workflow

1. Create a new subdirectory (e.g. `my-workflow/`)
2. Add `README.md`, `config.yaml`, `setup.sh`, and the main script(s)
3. Update the table in the root `README.md`

## External dependencies

- `fswatch` — installed via Homebrew
- `anthropic` Python SDK — installed per-workflow via `pip3 install --break-system-packages`
- `pyyaml` — same
- Claude Code CLI (`claude`) — used for MCP tool access from scripts

## Secrets

`ANTHROPIC_API_KEY` must be set in the environment. Never commit it.
