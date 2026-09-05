# 50-Persona Audit — Round 1

Date: 2026-09-06
Protocol: `Reese-max/autodev-ng/docs/portfolio-audit/2026-09-06-50-persona-audit.md`

> Fixed 50-persona model simulation plus repository evidence review; not 50 human participants.

## Round 1 result

Status: **P1 TRACKED (ISSUES DISABLED) — NOT CLEAN**

The Round 0 authorization-boundary finding remains present in the current README/configuration contract:

- the default Kiro ACP example uses `--trust-all-tools`;
- access is constrained by `allowed_channels`, but `allowed_users` is optional and an empty list means all users in the allowed channel may use the bot;
- follow-up messages inside a created thread no longer require an @mention;
- the broker can therefore bridge Discord users into a coding agent that has been configured to auto-trust tools.

This is an intentionally powerful architecture, but the default/example combination does not establish a strong per-user authorization boundary before tool-capable agent execution.

GitHub Issues are disabled in this repository, so no duplicate Issue is created; this report remains the repository-local tracker and the central portfolio report should continue tracking the P1.

## Fixed-persona impact

D03/C05/J04/J05 and multi-user J02 personas fail the least-privilege scenario when an allowed Discord channel contains users who were not intended to control a fully trusted coding agent.

## Required remediation

1. Make production examples/defaults fail closed on user authorization: require non-empty `allowed_users` when dangerous auto-trust modes are enabled, or require an equivalent role/approval gate.
2. Treat `--trust-all-tools` as an explicit high-risk opt-in rather than the ordinary quick-start default.
3. Add startup validation that rejects unsafe combinations (tool auto-trust + broad channel membership + no user allowlist).
4. Add per-session actor binding so thread follow-ups remain authorized to the initiating/approved users.
5. Add tests for unauthorized channel members, thread hijack/follow-up, permission escalation and agent restart.
6. Re-run the fixed personas on a production-equivalent broker and require two consecutive rounds without new P0/P1/P2.

## Runtime status

**Pending.** This round confirms the default configuration semantics from current documentation but did not connect a Discord server or execute an ACP coding agent.