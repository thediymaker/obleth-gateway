# Security Policy

## Reporting a vulnerability

Public issues are fine for general bugs, hardening ideas, and non-sensitive
security discussions. For actual vulnerabilities or suspected vulnerabilities,
please use a private reporting channel.

Please report vulnerabilities privately using one of these channels:

- GitHub private vulnerability reporting for this repository.
- Email: support@obleth.com

Include, where possible:

- affected version or commit
- deployment mode or configuration details
- steps to reproduce
- proof of impact
- suggested mitigation, if you have one

You can expect an initial acknowledgement within 5 business days.

## Disclosure policy

- We will confirm whether the report is in scope.
- We will work on a fix and coordinate disclosure timing where appropriate.
- Please avoid public disclosure until a fix or mitigation is available.

## Scope

Examples of in-scope issues include:

- authentication or authorization bypass
- tenant isolation failures
- SSRF, credential exposure, or secret leakage
- request routing that crosses tenant boundaries
- remote code execution, privilege escalation, or data exfiltration

Examples typically out of scope unless they create a real exploit path:

- missing best-practice headers alone
- denial of service requiring unrealistic resources
- vulnerabilities only present in unsupported or heavily modified deployments