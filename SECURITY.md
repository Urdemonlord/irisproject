# Security Policy

## Reporting a vulnerability

**Do not open a public issue for a security vulnerability.**

Use GitHub's private vulnerability reporting: go to the repository's
**Security** tab → **Report a vulnerability**. Only the maintainers can see
your report, and you can discuss a fix privately before anything is disclosed.

Please include in your report:

- What the vulnerability is and where it is (file, endpoint, or configuration)
- Steps to reproduce it, or a proof of concept
- The impact you think it has (who is affected, what an attacker could do)
- Any suggested fix, if you have one

## What happens next

- We will acknowledge your report and aim to confirm the issue promptly.
- We will work on a fix and coordinate disclosure with you. Our goal is to
  ship a fix and publish an advisory within 90 days of confirmation for
  issues that affect a deployed instance.
- Please do not disclose the issue publicly until a fix is available, or
  until we agree on a disclosure date.

## Scope

IRIS is a self-hosted mail platform. The most useful reports cover the API
(authentication, sessions, CSRF, rate limits), the mail pipeline (JMAP,
attachments, HTML rendering), and the deployment configuration in this repo.
