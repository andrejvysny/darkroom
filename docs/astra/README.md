# `docs/astra/` — GPT-6-Astra numerics register

Produced by the `/codex-darkroom` skill (`.claude/skills/codex-darkroom/`), which sends a sealed
evidence packet to GPT-6-Astra via the Codex CLI for the parts of Darkroom where a wrong algorithm
ships a wrong photograph — RAW colour science, develop shader math, HDR merge, panorama geometry,
denoise DSP, colour management.

- **`FINDINGS.md`** — the distilled result of every call, under a stable id. Check it before packing
  a new question: if a finding already covers the ground, give Astra the id and ask it to attack the
  proposed fix rather than rediscover the problem.
- **`LOG.md`** — one line per call. Astra runs on a limited ChatGPT Plus allowance; this is how the
  burn stays visible.

Raw packets and raw Astra output are **not** committed — they live in the session scratchpad
(`<scratchpad>/astra/pack-*.md`, `out-*.md`). Only the distilled spec or findings land here.

Astra never writes code and never reads this repository. Claude packs the evidence, verifies the
answer against source, runs the supplied test vectors, and implements.
