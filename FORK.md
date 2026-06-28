# Fork Ledger & Upstream Strategy

This is a **fork** of [`xintaofei/codeg`](https://github.com/xintaofei/codeg) (Apache-2.0).
Goal: carry our features while staying **easy to sync with upstream** for as long as possible.

## Remotes & sync

- `origin` = `jtalborough/codeg` (this fork)
- `upstream` = `xintaofei/codeg`
- Current base: **upstream v0.18.3**

**Sync routine (per upstream release):**
```bash
git fetch upstream
git merge upstream/main            # into fork main (use a worktree if the tree is dirty)
# resolve conflicts (rare unless upstream refactors a file we touched)
# CI: pnpm eslint . && pnpm test ; cargo test (3 binaries)
./scripts/deploy-codeg-server.sh   # rebuild + redeploy the box
```
Once an upstream PR of ours merges, **drop our copy of that commit** on the next sync so it doesn't duplicate.

## Principles (keep us mergeable)

1. **Localize + add, don't edit.** A new module/file beats editing a hot upstream file. (Proof: the whole chat layer survived the v0.18.0→0.18.3 jump — 99 upstream files — with **zero conflicts** because it lives in `chat_channel/`.)
2. **Centralize in the service layer.** Put behavior in `*_service.rs` / shared `_core` fns, not scattered across `acp/manager.rs` or `acp/lifecycle.rs` (the hottest files).
3. **One feature = one focused branch/commit.** Easy to cherry-pick upstream or drop later.
4. **Upstream everything non-controversial.** The smaller the private delta, the cheaper every sync.
5. **Gate core-touching features behind config/flags** and keep diffs minimal + additive.
6. **Sync every upstream release.** Don't let drift pile up.

## Ledger

### ✅ Contribute upstream (security / bugs / non-controversial)
| Item | Type | Status | Files / ref |
|---|---|---|---|
| Chat **sender-allowlist** (unauthenticated-RCE-via-chat fix) | security | **PR [#309](https://github.com/xintaofei/codeg/pull/309)** | `chat_channel/authz.rs` + gate |
| Image-in-chat error | bug | planned | conv `c35c504b` |
| Conversation **tab auto-naming** (falls back to first sentence) | bug/polish | planned | conv `bf28b2b9` |
| **Unread indicator** — clear `pending_review` on open (Option A) | feature (small) | planned | `conversation_service` + sidebar |
| Clean chat output (suppress per-tool/per-turn chrome) | polish | in fork | upstream candidate — may want an i18n pass + a config toggle first |

### 🔒 Keep in fork (opinionated / personal) — probe upstream before investing
| Item | Status | Notes |
|---|---|---|
| Channel-bound **personas** (1 channel = 1 agent, default folder/agent) | done | opinionated UX; float as a proposal, don't assume acceptance |
| Per-channel **auto_approve** | done | upstreamable with care (security-sensitive default) |
| **Persistent / auto-resume** chat sessions | done | upstreamable with care |
| **Per-project coupled workspaces** (terminals + tabs per project; per-project browser window w/ extensions + bookmarks) | planned (conv `657decd8`) | **big, core, opinionated** → fork unless maintainer wants it; touches `tab-context.tsx`, terminal mgr, composer (all upstream-churned) — highest merge risk |
| rai / cass persona configs + box deployment | personal | never upstream |

### 🛠 Infra (not upstreamed)
`scripts/deploy-codeg-server.sh`, box build pipeline, Tailscale `serve`, systemd user service, `FORK.md`, `.claude/agents/codeg-expert.md`.

## Risk legend
- **Low merge risk:** isolated subsystems — `chat_channel/`, new modules, service-layer helpers, DB migrations (append-only).
- **High merge risk:** `acp/manager.rs`, `acp/lifecycle.rs`, `contexts/*-context.tsx`, `components/conversations/sidebar-*`, the composer/session UI — upstream actively develops these. Touch additively, minimally, and prefer upstreaming.
