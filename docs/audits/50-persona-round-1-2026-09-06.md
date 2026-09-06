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

---

# Round 2 continuation — 2026-09-06

Audited default-branch SHA before this documentation update: `bec07084c6f94e67bff77fc4bb592951421dc8fa`.

Status: **SAME P1 REPRODUCIBLE / NO NEW P0/P1/P2 — NOT CLEAN**

The only default-branch change after the product SHA reviewed in Round 1 is the audit-documentation commit itself. The same fixed authorization personas were therefore re-run against the current contract and implementation evidence rather than treating documentation churn as a product fix.

## Re-run evidence

The unsafe combination is still the documented normal Kiro quick-start path:

- `README.md` still shows `allowed_users` commented out/optional and defines an empty user allowlist as allowing all users in an allowed channel.
- The same examples still launch Kiro as `kiro-cli acp --trust-all-tools`.
- The documented thread workflow still allows follow-up messages without another @mention.
- `src/config.rs` still deserializes `allowed_users` with `#[serde(default)]` to an empty vector; configuration loading contains no startup validation that rejects an empty user allowlist when the agent arguments enable tool auto-trust.

Accordingly D03, C05, J02, J04 and J05 still fail the same least-privilege/thread-hijack scenario. This is the same already-tracked P1, not a newly counted defect.

## Execution evidence check

GitHub Actions currently exposes only one `main` workflow run in this fork: `Release Charts` run `24399467914`, successful on product SHA `5269602933ed354c6888d7cf2e693e2f7d21e697` on 2026-04-14. That workflow is release-chart execution evidence only; it is **not** evidence that Discord authorization, ACP tool permissions, thread actor binding, or a production-equivalent broker runtime pass.

## CLEAN gate

Still **NOT CLEAN**. The P1 authorization boundary remains reproducible and there is no current production-equivalent runtime evidence for the affected scenarios. Two consecutive clean rounds cannot begin until the unsafe configuration/actor-binding path is fixed or explicitly redesigned with an equivalent authorization gate, followed by the same persona regression scenarios.