---
name: codex-darkroom
description: Delegates an explicitly-approved Darkroom numerics question to GPT-6-Astra via the Codex CLI — scoped ONLY to RAW colour science, GPU develop math, tone operators, HDR merge, panorama geometry, denoise DSP, colour management, and statistical modelling. Astra derives, diagnoses, and attacks; it never writes code and never reads the repo. Claude packs the evidence, implements the spec, and verifies it. Use only when the user explicitly invokes it. For UI, IPC, SQLite, import, CI, or wgpu resource plumbing, use codex-plan-review / codex-implementation-review instead — this skill refuses and redirects. Trigger for any mention of these — colour matrix correctness, white balance / CAT, tone curve or base tone operator, gamut mapping, ICC or Display P3 output transform, 3D LUT interpolation, HDR merge weighting or deghosting, EV100, panorama bundle adjustment / RANSAC / homography / seam energy, denoise noise model or Bayer tiling, numerical instability, wrong pixels, ask Astra, GPT-6-Astra, gpt-6-astra.
disable-model-invocation: true
---

# Darkroom Numerics Review (GPT-6-Astra)

Independent second opinion on Darkroom's numerically-critical code via the Codex CLI's
`gpt-6-astra`. Codex runs **read-only from a neutral directory with no access to this repository** —
the packet you build is its entire world. Astra's job is to **derive, diagnose, and attack, never to
implement**: it returns a specification or a set of findings, and Claude verifies and writes the
code. Run only when the user explicitly invokes `/codex-darkroom`.

For anything outside the scope gate below, use the generic `codex-plan-review` /
`codex-implementation-review` / `codex-brainstorm` skills — those route across the
`gpt-5.6-terra/sol/luna` family and are the right, cheaper tool for ordinary review.

## Why this skill exists, not the generic codex-* skills

Astra runs on a **ChatGPT Plus** allowance; Claude Fable runs on Max 20. The two budgets are not
comparable, and every rule here follows from that asymmetry: **spend Claude generously so that Astra
is cheap.**

This skill concentrates the scarce budget on the ~10% of Darkroom work where a subtly wrong
algorithm ships a subtly wrong photograph — code that compiles, passes `param_effects`, and looks
plausible while being mathematically incorrect. Two consequences:

1. **Hard scope gate.** Out of scope means refuse and redirect, not "run anyway at a lower bar".
2. **Sealed packet, not repo grazing.** Access is prompt-only. Claude compresses the question into a
   dense, numerically-grounded packet *before* calling Codex. Letting Astra explore would cost 5–20×
   the tokens rediscovering what Claude already knows. When a packet turns out to be insufficient,
   the correct Astra behaviour is to say so and stop — not to ask for the repo.

## The gate — five conditions, all must hold

Confirm every one before writing a packet. If any fails, do not spend the quota.

1. A wrong answer here would be **silently** wrong — it compiles, the goldens pass, the image looks
   plausible.
2. **No existing golden, harness, or reference settles it.** You checked — name what you ran. See
   `references/evidence-harnesses.md`.
3. The question is mathematics, not code structure or API design.
4. It fits in ≤5 numbered questions with real measured numbers attached.
5. **You wrote down your own best answer first.** This makes Astra's reply falsifiable and turns
   disagreement into signal rather than deference.

Condition 5 is the one most likely to be skipped and the one that most improves the result.

## Scope gate — check this before anything else

Full paths, invariants and quotable constants: `references/invariants.md`.

| Area | Paths |
|---|---|
| RAW colour path | `crates/core-raw/src/color.rs`, `develop.rs` (`map_3ch_to_rgb`, highlight reconstruction), `heif.rs` (PQ EOTF + anchor) |
| GPU develop math | `crates/core-pipeline/src/develop.wgsl`, `curve.rs`, `base_curve_ref.rs` (math only, **not** `params.rs` plumbing) |
| HDR merge | `crates/core-hdr/src/lib.rs`, `warp.rs`; `core-raw/src/hdr_file.rs` numerics only |
| Panorama geometry | `crates/core-pano/src/{ransac,bundle,camera,project,seam,blend,exposure,rectangle,wave,rng}.rs` |
| Denoise DSP | `crates/core-analyze/src/denoise.rs` — Bayer pack, CFA phase, tiling, feather, k-Sigma. **Not** the ORT/CoreML plumbing |
| Colour management | Display P3 / AdobeRGB / ICC output transforms, gamut mapping, output sharpening, 3D LUT interpolation (`@binding(16)`, planned) |
| Statistical modelling | `crates/core-suggest/src/{fit,cv,metrics,weights}.rs` |
| Similarity metrics | `crates/core-dedup` — dHash / threshold math only |

Explicitly **out of scope** — refuse and redirect: React/UI, `src/lib/ipc.ts`, `commands.rs`
plumbing, SQLite schema and migrations, import/dedup workflow, Tauri lifecycle, thumbnails and
caching, ORT model loading, CI/release, logging, wgpu resource lifecycle in `backend.rs`.

When a target straddles both (e.g. a `params.rs` change that also alters shader math), scope the
packet to the math alone and say the plumbing half is out of scope.

## Modes

Pick exactly one per run and state it in the approval summary. Full prompt scaffolds:
`references/prompts.md`.

- **`derive`** — design or derive a transform/algorithm. Output is a specification.
- **`diagnose`** — given measured numbers and the transform chain, prove *which stage* is
  mathematically wrong. Output is ranked hypotheses, each carrying a **discriminating experiment**:
  the exact harness to run and the number that would confirm or kill it. A hypothesis with no
  falsifying measurement is rejected.
- **`verify`** — adversarial review of a finished numerical diff against the design it implements.
  Each finding names which golden should have caught it and why it did not.

## Building the packet

The packet is what keeps Astra's token usage low — this is the entire point of the skill. The main
thread writes the judgment sections (GOAL, MODE, INVARIANTS selection, MY OWN ANSWER, QUESTIONS,
NON-GOALS). Delegate the mechanical sections to the `astra-packer` subagent, which fills excerpts,
constants, golden coverage and measured evidence without dragging `cargo` output through the main
context.

```
GOAL:                 <one line — what Astra must derive, diagnose, or attack>
MODE:                 derive | diagnose | verify
SUBSYSTEM:            raw-colour | tone-operator | gpu-develop-math | hdr-merge |
                      panorama-geometry | denoise-dsp | colour-management | suggest-stats | dedup-metric
WORKING SPACE:        <colour space, encoding (linear/log/OETF), white point and units of EVERY
                      quantity below. Default working space: linear wide-gamut ProPhoto,
                      scene-referred, D50, values >1.0 preserved.>
PIPELINE POSITION:    <where in the chain this sits — canonical order in references/invariants.md>
INVARIANTS:           <verbatim-quoted from references/invariants.md — never paraphrased from memory>
VERSIONING IMPACT:    <does the answer change developed pixels? current PROCESS_VERSION / DECODER_VERSION>
CURRENT MATH:         <verbatim excerpts, file:symbol, ≤150 lines total across all excerpts>
MEASURED EVIDENCE:    <real numbers: exact command run, input file, output. Never "looks green">
EXPECTED / REFERENCE: <what ACR/LR/dcraw/the spec gives for the same input, or "unknown">
EXISTING CONSTANTS:   <each with a provenance tag: derived | literature<cite> | calibrated<how> | unknown>
GOLDEN COVERAGE:      <which tests pin this, what they assert, what they do NOT cover>
PRIOR FINDINGS:       <matching docs/astra/FINDINGS.md id, or "none found — checked">
MY OWN ANSWER:        <your best hypothesis, stated plainly — gate condition 5>
NON-GOALS:
QUESTIONS FOR ASTRA:  <numbered, ORDERED BY VALUE — most valuable first>
```

Rules that carry unusual weight:

- **Order the questions by value.** Astra answers in order, so a run truncated by a usage limit still
  lands the answer you most needed. Q1 must be the one you would pay for alone.
- **Never invent a constant in the packet.** Quote it from source with `file:line`, or tag it
  `unknown`. If Astra asserts a different value, that is a finding to verify, not a fact to adopt.
- **Excerpt budget ≤150 lines total.** If you exceed it, the question is too broad — split it.
- Check `docs/astra/FINDINGS.md` before treating something as novel. If a finding already covers it,
  give Astra the id and ask it to attack the *proposed fix*, not rediscover the problem.

## Effort routing

Model is fixed to `gpt-6-astra`; only effort varies. Default **`high`**.

| Situation | Effort |
|---|---|
| `verify` a bounded diff inside one function | `high` |
| `diagnose` with strong evidence and a single suspect stage | `high` |
| `derive` a transform with a known reference (P3/ICC matrices, LUT interpolation) | `xhigh` |
| Cross-stage `diagnose` (whole colour chain, HDR weighting × deghost interaction) | `xhigh` |
| Novel algorithm (seam energy, deghost consistency, noise model, bundle reparameterisation) | `xhigh`, `max` on approval |
| Disputed finding, or a regression that has survived multiple sessions | `max`, explicit approval every time |

- **A strong packet at `high` beats a weak packet at `max`.** Escalate the packet before the effort.
- `max` requires explicit approval for the added cost every time — never default to it.
- `ultra` is listed as a valid effort for `gpt-6-astra` in the local Codex CLI config, but it is
  **unverified** whether it engages genuine multi-agent delegation for this model. Treat `max` as the
  practical ceiling; if `ultra` is requested, say it is unverified before running it.
- A wrong effort string fails fast — no silent downgrade.

## Approval summary

Print this and wait for explicit approval before running:

```
Darkroom numerics review (GPT-6-Astra)
  Mode:          <derive | diagnose | verify>
  Subsystem:     <…>
  In scope:      <path(s) matched against the scope table>
  Model:         gpt-6-astra          Effort: <high | xhigh | max*>
  Access:        sealed packet (no repo access)
  Packet:        <path>  ~<n>k tokens  ·  <m> questions, value-ordered
  Prior finding: <FINDINGS.md id | none found — checked>
  My own answer: <one line — gate condition 5>
  Web:           disabled
  Output:        <derive: specification | diagnose/verify: attack findings> — not code
  Purpose:       <one sentence>
```

`*max` needs explicit approval. Never silently change model, effort, access mode, or scope.

## Command

```bash
SCRATCH="<session scratchpad>/astra"; mkdir -p "$SCRATCH"; cd "$SCRATCH"
STAMP="$(date +%Y-%m-%d)-<subsystem>"
codex exec \
  -m "gpt-6-astra" \
  -c 'model_provider="openai"' \
  -c 'model_reasoning_effort="<EFFORT>"' \
  -c 'web_search="disabled"' \
  -c 'approval_policy="never"' \
  -s read-only \
  --ephemeral \
  --strict-config \
  --ignore-user-config \
  --skip-git-repo-check \
  -o "$SCRATCH/out-$STAMP.md" \
  - > "$SCRATCH/stdout-$STAMP.log" 2> "$SCRATCH/stderr-$STAMP.log" <<'CODEXEOF'
<PROMPT>
CODEXEOF
echo "exit=$?"
```

- `<PROMPT>` = the packet, wrapped in the mode's scaffold from `references/prompts.md`.
- Run from the scratchpad, **never** from the repo — this is sealed-packet mode. `--skip-git-repo-check`
  is required because the scratchpad is not a git repo.
- **Always capture stdout and stderr to files.** Truncation detection (below) depends on it.
- Never use `-C`/`--cd` into the repo, `workspace-write`, `danger-full-access`, or
  `--dangerously-bypass-approvals-and-sandbox`.
- Web search stays disabled — Claude does the literature retrieval, Astra only derives.
- `xhigh` and `max` runs can exceed the 10-minute Bash ceiling. Launch them with
  `run_in_background: true` and poll the output file.

## Usage limits and partial responses

A Plus allowance can be exhausted mid-call. Truncation is an expected outcome with a defined
procedure, not an error to retry blindly.

### Detect

Every mode's output contract requires the reply to end with the literal sentinel
`=== END OF ASTRA RESPONSE ===`, preceded by a `SECTIONS: <n>/<total>` line. Without the sentinel you
cannot distinguish "Astra answered briefly" from "Astra was cut off".

| Signal | Meaning |
|---|---|
| exit 0 · output non-empty · ends with the sentinel | Complete |
| exit 0 · output non-empty · **no sentinel** | Truncated mid-generation |
| exit ≠ 0 · output non-empty | Truncated, killed by the CLI — read the stderr log |
| exit ≠ 0 · output missing or empty | Failed before producing anything |

Grep the stderr log for `usage limit`, `rate limit`, `quota`, `429`, `resets`. Report any reset time
verbatim. `--json` streams JSONL events and is the fallback diagnostic when the cause is unclear; it
is noisier, so do not use it by default.

Verified against a real `codex exec` run: the output file has **no trailing newline** after the
sentinel, so test the last line, not the last byte. The stderr log also carries a `tokens used`
count — record it in `docs/astra/LOG.md`, it is the only direct read on quota burn.

```bash
OUT="$SCRATCH/out-$STAMP.md"; ERR="$SCRATCH/stderr-$STAMP.log"
if   [ ! -f "$OUT" ]; then echo "STATE=missing"
elif [ ! -s "$OUT" ]; then echo "STATE=empty"
elif [ "$(tail -n1 "$OUT")" = "=== END OF ASTRA RESPONSE ===" ]; then echo "STATE=complete"
else                       echo "STATE=truncated"; fi
grep -niE 'usage limit|rate limit|quota|429|resets' "$ERR" | head
grep -A1 -i 'tokens used' "$ERR" | tail -2
```

### Never auto-retry a quota failure

A retry costs the same quota and usually meets the same wall. Stop and report. This extends the
one-shot rule. Retry once at identical settings only for a plainly transient failure (network,
transport), never for a limit.

### Salvage — the rule that matters most

- A truncated **`diagnose`** or **`verify`** run is usually **usable**. Findings are emitted
  independently and in value order, so the prefix that arrived is the part you most wanted. Verify
  the arrived findings normally; the missing tail is missing coverage, not a wrong answer.
- A truncated **`derive`** run is **not usable as a specification**. A half-finished derivation can be
  internally consistent, read as complete, and still be wrong at exactly the step that never
  arrived. **Never implement from a partial `derive` output.** Record it; do not act on it.

In every case, say plainly to the user that the response was partial and which sections are missing.

### Record

Append to `docs/astra/LOG.md` with outcome `PARTIAL`, and to `docs/astra/FINDINGS.md` under a new id
marked `PARTIAL`, listing which sections arrived and which are missing — so the next packet starts
where this one stopped instead of rediscovering it.

### Resume when quota returns — delta packet, not the original

`--ephemeral` means there is no session to resume, by design. Send a fresh, much smaller packet:

```
GOAL:            <verbatim from the original packet>
MODE:            <same>
INVARIANTS:      <short form — only those the missing sections need>
ALREADY ANSWERED: <the sections Astra produced, quoted verbatim>
CONTINUE FROM:   <first missing section>
Do not restate, revise, or re-derive the sections above. Produce only the missing ones,
then the SECTIONS line and the sentinel.
```

Typically a fraction of the original cost. This is why the sentinel and `SECTIONS: n/total` header
are mandatory — they are how you know where it stopped.

### Degrade deliberately when quota is tight

Ask the single highest-value question at `high` rather than five questions at `xhigh`. Combined with
value-ordered questions, a truncated run then degrades gracefully instead of failing.

## Multiple runs

One primary call per question. A second is justified only with a genuinely different angle — a
`derive` before implementing, then a separate `verify` after — never the same packet re-sent hoping
for a better answer. Each additional run needs approval. Gaps in an answer are closed by Claude from
the repo, or converted into a calibration experiment runnable with the existing harnesses.

## Secrets

Never put secrets, tokens, keys, `.env` contents, or personal photo paths in the packet. Use
corpus-relative or anonymised paths (`tests/corpus/...`, `<library>/2026/...`).

## Failure policy

1. Report the exact error.
2. Retry once at identical settings only for a likely-transient failure — never for a usage limit.
3. Lower effort only for a confirmed cost/quota/latency issue, and only with approval.
4. If `gpt-6-astra` is unavailable, stop and offer `codex-plan-review` /
   `codex-implementation-review` at `gpt-5.6-sol`/`xhigh` as an **explicitly-labelled** substitute —
   never silently substitute a different model under the Astra label.
5. Auth, missing-CLI, invalid-model, permission and config errors are not fixed by lowering effort —
   surface them.

## Integrating the result

Astra's output is a specification or a set of findings — **not code**. For each item:

1. Verify it against the actual source before acting; record basis `verified | inference | unknown`.
2. Check every constant against its stated provenance. An unsourced number is a defect, not an answer.
3. **Run the supplied synthetic test vectors before implementing anything.** They exist so the answer
   can be falsified locally at zero further quota cost.
4. Reject unsupported claims and say which, and why.

An accepted spec becomes the `DESIGN:` block of a delegation brief handed to
`astra-spec-implementer` (or the global `impl-critical`). Distil it into `docs/astra/FINDINGS.md`
under a stable id, and append the call to `docs/astra/LOG.md`. Never paste Astra's raw output as a
finished answer.

## Relationship to fable-orchestrator

Fable stays the main thread and the decision-maker. The Astra call is a single Bash invocation, not
an agent — it costs the main context only the packet summary and the distilled result.

```
fable-explore / fable-plan  →  codex-darkroom (derive)  →  PLAN.md carries the spec
   →  fable-run / astra-spec-implementer  →  gpu-visual-qa  →  codex-darkroom (verify, optional)
```
