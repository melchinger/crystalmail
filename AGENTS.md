# AGENTS.md

This file is the canonical instruction source for `crystalmail`.

## Project Snapshot
- Repository type: `rust`
- Primary language/runtime: `rust`
- Default package manager: `cargo`
- Selected stack modules: tauri, react, sqlite
- Goal: produce clean, secure, maintainable software with professional repository hygiene.

## Commands
- `install`: `cargo fetch`
- `test`: `cargo test`

## Stack Commands
- `desktop-dev`: `cargo tauri dev`
- `desktop-build`: `cargo tauri build`

## Architecture Boundaries
- Respect a layered structure with clear separation between domain logic, application flow, infrastructure, and presentation or delivery code.
- Do not place business logic in views, templates, pages, or transport adapters.
- Prefer small, focused files and changes. Refactor files before they become large or multi-purpose.
- Keep data validation close to input boundaries and use typed DTOs or schemas when data crosses layers.

## Code Style
- Prefer descriptive names, small functions, and early returns over deeply nested control flow.
- Avoid `any`, untyped dictionaries, and hidden side effects.
- Reuse existing utilities and patterns before introducing a new abstraction.
- Keep tests close to behavior and document non-obvious decisions in the repository docs.

## Security Rules
- Never hard-code secrets, tokens, passwords, or production credentials.
- Commit only placeholders in `.env.example`; real values must stay outside version control.
- Validate all external input on the server or trusted boundary before persistence or privileged actions.
- Use least privilege for infrastructure, APIs, service accounts, and third-party integrations.
- Ask for confirmation before destructive or high-risk operations such as dropping data, resetting history, or changing deployment credentials.

## Accessibility and UX
- Use semantic structure and accessible defaults for interactive elements.
- Ensure loading, empty, and error states are explicit and actionable.
- Avoid color-only status communication and preserve visible focus states.

## Testing and Review Workflow
- Make the smallest meaningful change, then run the most specific validation available.
- Add or update tests when behavior changes and there is an established adjacent test pattern.
- Stop and surface blockers instead of masking them with temporary hacks.
- Keep generated or derived files synchronized with their source templates.

## Operational Boundaries
- Do not invent infrastructure, credentials, or external systems that are not documented in the repository.
- Prefer data-driven configuration over scattered conditional logic.
- Preserve backward compatibility unless the change explicitly introduces a migration plan.

## Anti-Patterns
- Do not mix transport, persistence, and UI concerns in one file.
- Do not bypass validation to “make it work”.
- Do not add hidden global state, silent catch-all error handling, or unreviewed code generation outputs.

## Stack Overlay
### Profile: Desktop App
- Design for local UX, offline resilience, and explicit privilege boundaries around OS integration.

### Runtime: Rust
- Keep unsafe platform access isolated and clearly documented.
- Preserve explicit ownership and thin adapter boundaries around OS integrations.

### Module: Tauri
- Keep the Rust core authoritative for filesystem, OS, and privileged operations.
- Treat Tauri commands as adapter boundaries, not as a home for business logic.

### Module: React
- Keep UI components focused on presentation and interaction state.
- Push IO and business logic into services or command adapters.

### Module: SQLite
- Treat SQLite as a topology choice, not an excuse to couple domain logic to the database.
- Keep upgrade paths to PostgreSQL or MariaDB viable through adapter boundaries.

### Policy: Security Baseline
- Preserve input validation, secret hygiene, and least-privilege defaults across the stack.

### Policy: CI
- Keep the most relevant validation running in CI and preserve deterministic setup commands.
