# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Reporting a Vulnerability

If you discover a security vulnerability in QuadCD, please report it responsibly
using [GitHub Security Advisories](https://github.com/jokujossai/quadcd/security/advisories/new).

**Please do not open a public issue for security vulnerabilities.**

## What to Expect

- **Credit** in the release notes for responsibly disclosed vulnerabilities, unless you prefer to remain anonymous

## Scope

QuadCD is a systemd generator and deployment tool. Security issues of particular
interest include:

- Path traversal or arbitrary file writes during unit generation
- Environment variable injection or unintended variable substitution
- Privilege escalation when running in system mode
- Supply chain risks in dependencies
