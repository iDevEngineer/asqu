# Agent instructions

Rules for any AI tool (or person) changing Asqu.

- Use only AGENTS.md for agent instructions; don't add a CLAUDE.md (Claude Code reads AGENTS.md only when no CLAUDE.md exists).
- Tool-agnostic like git: no tool-specific options unless every tool is covered the same way.
- This repo is public. Never commit secrets, keys, account IDs or private server details. Release keys and tokens live in the Keychain or in GitHub secrets, never in files.
- Plain English in every message, help text and page.
- History is never quietly lost: anything that removes stored data is explained first, can be previewed, and never touches kept Timeline entries.
- An AI can never approve for the person (high-risk keeps, check commands, deletions).
- Every changed piece of logic gets a test; tests must pass on Mac, Linux and Windows.
- Tests never run Asqu in the repository's own folder: always a throwaway one.
- The Asqu view uses only the colour tokens on :root, and reuses existing classes before adding new ones. A new class never reuses an existing class name for a different meaning.
