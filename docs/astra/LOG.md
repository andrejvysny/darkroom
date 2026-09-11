# Astra call log

Every `/codex-darkroom` invocation, one line. This is how burn, session continuity and access mode
stay visible across sessions, and how a truncated run is picked up later.

- `Outcome` is `complete`, `PARTIAL` (truncated — see the FINDINGS id for which sections arrived), or
  `failed` (with the reason).
- `Access` is `sealed` or `repo-read`. A `repo-read` answer is the least reproducible kind, because
  its input was the tree at that moment rather than a saved packet.
- `Session` is the id from the run's stderr (`session id: …`) — the handle `codex exec resume` needs.
  `—` means the run was `--ephemeral`.
- `Tokens` is the `tokens used` figure from the stderr log.

| Date | Mode | Subsystem | Effort | Access | Pattern/Round | Question (Q1) | Outcome | Tokens | Session | Finding |
|---|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | | |
