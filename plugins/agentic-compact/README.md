# Agentic Compact Codex Plugin Package

This package contributes the `agentic-compact` skill. The MCP server is installed separately by the `agentic-compact install` command so its executable path and approval settings remain explicit in the user's `config.toml`.

The plugin manifest intentionally does not declare `mcpServers`; this avoids duplicate or ambiguous server definitions across Codex plugin and user configuration layers.
