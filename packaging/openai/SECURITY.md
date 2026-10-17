# Security policy

Security fixes target the latest released version. Upgrade before checking whether a report still reproduces.

## Reporting a vulnerability

Email leeguoooo@gmail.com with the affected version, platform, reproduction steps and expected impact. Do not publish credentials, personal data or exploit details in a public issue. Prefer a minimal reproduction using synthetic data. We will assess the report and coordinate fixes and disclosure; there is no guaranteed response time or bounty program.

## Boundaries

The browser extension and CLI can act inside logged-in browser sessions. Keep agent access restricted to trusted local processes, review sensitive actions, and keep session ownership boundaries intact. Site-provided content is untrusted; browser control is not a sandbox for arbitrary scripts.
