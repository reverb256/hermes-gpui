# Security policy

## Supported versions

The latest `0.1.x` release and `main` receive fixes. Older tags are not
maintained.

## Reporting a vulnerability

Report privately through GitHub's **Report a vulnerability** flow:
https://github.com/reverb256/hermes-gpui/security/advisories/new

If that flow is unavailable for this private repository, contact
[@reverb256](https://github.com/reverb256) and ask for a private channel before
sending details.

Do not open a public issue, and do not include live credentials, cookies,
session tickets or private hostnames. If you must demonstrate a leak, use a
redacted value and describe the path instead.

Please include the affected version/commit, the host deployment shape (dashboard
auth on/off), reproduction steps, and the impact you believe it has.

## Scope

In scope:

- Credential handling: password login, `__Host-hermes_session_at`, WebSocket
  tickets, the redacted vault, and anything that could write secrets to logs or
  disk.
- RPC/REST parsing that could be driven by a malicious or compromised host.
- Release artifacts that do not match the tagged source.

Out of scope:

- Vulnerabilities in a Hermes host itself (report those upstream).
- Attacks that require an already-compromised desktop session or root.
- Denial of service from a host that never responds.

The client never calls the secret-reveal endpoint; plaintext credentials should
never enter this process beyond the initial login request.
