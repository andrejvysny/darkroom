# `docs/astra/` — GPT-6-Astra numerics register

Produced by the `/codex-darkroom` skill (`.claude/skills/codex-darkroom/`), which sends an evidence
packet to GPT-6-Astra via the Codex CLI for the parts of Darkroom where a wrong algorithm ships a
wrong photograph — RAW colour science, develop shader math, tone operators, HDR merge and
deghosting, panorama geometry, denoise DSP, colour management, similarity metrics, and statistical
modelling.

The routing rule: **if a mistake produces the wrong pixel, involve Astra; if it produces broken
software, involve Fable.** Everything else — IPC, SQLite, import, UI, CI, wgpu plumbing — goes to
`codex-plan-review` / `codex-implementation-review` instead.

- **`FINDINGS.md`** — the distilled result of every call, under a stable id. Check it before packing
  a new question: if a finding already covers the ground, give Astra the id and ask it to attack the
  proposed fix rather than rediscover the problem.
- **`LOG.md`** — one line per call, including the session id that makes a run resumable and the
  token count from its stderr log.

Raw packets and raw Astra output are **not** committed — they live in the session scratchpad
(`<scratchpad>/astra/pack-*.md`, `out-*.md`). Only the distilled spec or findings land here.

Astra never writes code. In the default access mode it never reads this repository either; the
gated `repo-read` mode gives it read-only access for questions that provably span several crates,
and is logged as such. Claude packs the evidence, verifies the answer against source, runs the
supplied test vectors, and implements.
