# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Active development |

Hermes-RS is in early development. Security fixes are applied to the latest commit on `main`.

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub issues.**

Instead, report them via:

- **GitHub Security Advisories**: [Report a vulnerability](../../security/advisories/new)
- **Email**: Send a PGP-encrypted email to the maintainers (if listed in MAINTAINERS)

You should receive a response within **48 hours**. If you don't, please follow up.

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Affected versions/commits
- Potential impact
- Suggested fix (if available)

### Disclosure Policy

- Vulnerabilities are disclosed via GitHub Security Advisories after a fix is released.
- We follow [coordinated disclosure](https://en.wikipedia.org/wiki/Coordinated_vulnerability_disclosure).
- Credit is given to the reporter unless they request anonymity.

## Security Considerations

### Tool Execution

Hermes-RS executes arbitrary shell commands via the `terminal` tool and arbitrary code via the `code_execution` tool. These tools:

- Run with the same privileges as the Hermes-RS process
- Do not sandbox or isolate execution
- Accept input from LLM-generated content, which may be manipulated

**Mitigation**: Run Hermes-RS in a sandboxed environment (container, VM) when processing untrusted input.

### API Keys

- API keys are read from environment variables or config files
- Never commit API keys to the repository
- Config files (`hermes.toml`) should be excluded from version control via `.gitignore`

### Supply Chain

- All dependencies are resolved from `crates.io`
- Use `cargo audit` to check for known vulnerabilities in dependencies:
  ```bash
  cargo install cargo-audit
  cargo audit
  ```
