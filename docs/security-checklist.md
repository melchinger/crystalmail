# Security Checklist

## Trust boundaries

- Secrets stay out of the repository and are stored in the OS credential
  store. Never log credentials or include them in command-line arguments.
- Input is validated at every trust boundary before persistence or privileged
  actions.
- Privileged actions require explicit authorization checks.
- Destructive changes require review and rollback awareness.

## Tauri/WebView hardening

- Release builds do not enable the Tauri developer-tools feature.
- The production CSP is injected by Tauri; `dangerousDisableAssetCspModification`
  must remain disabled. JavaScript is limited to same-origin resources.
- Inline styles remain allowed because the React UI uses style attributes and
  the first-paint splash is inline CSS. Inline scripts are not allowed.
- Shell `open` permissions are limited to external links and mail links; no
  shell execute, spawn, or stdin permissions are granted.
- Remote mail images remain an explicit user-controlled feature. HTML mail is
  sanitized before it is rendered or quoted.

## Dependency and supply-chain controls

- `package-lock.json` and `src-tauri/Cargo.lock` are committed and CI uses the
  lockfiles.
- Dependabot tracks npm, Cargo, and GitHub Actions updates.
- The scheduled security workflow runs `npm audit` and RustSec advisory scans.
- CI actions should be upgraded deliberately and reviewed before changing
  their major version or permissions.

## Known follow-ups

- `sanitizeFragment()` is a deliberately small, DOMParser-based sanitizer.
  Keep adversarial coverage for SVG, `srcset`, `formaction`, dangerous URI
  schemes, event attributes, and CSS as the sanitizer evolves. Consider a
  maintained sanitizer library if the allowed HTML surface grows.
- Release artifacts are currently unsigned. Add platform signing only after
  the required credentials and verification process are available.
- Add focused regression tests for authentication/keyring access, HTML
  sanitization, mail parsing, migrations, and workflow execution as those
  areas gain more behavior.
