---
name: codex-darkroom
description: Delegates an explicitly-approved Darkroom numerics question to GPT-6-Astra via the Codex CLI — scoped ONLY to RAW colour science, GPU develop math, tone operators, HDR merge and deghosting, panorama geometry, denoise DSP, colour management, similarity metrics, and statistical modelling. Astra derives, diagnoses, attacks, and reviews; Claude packs the evidence, verifies the answer, and implements it. Use only when the user explicitly invokes it. For UI, IPC, SQLite, import, CI, or wgpu resource plumbing, use codex-plan-review / codex-implementation-review instead — this skill refuses and redirects. Trigger for any mention of these — colour matrix correctness, white balance / CAT, tone curve or base tone operator, gamut mapping, ICC or Display P3 output transform, 3D LUT interpolation, HDR merge weighting or deghosting, EV100, panorama bundle adjustment / RANSAC / homography / seam energy, denoise noise model or Bayer tiling, k-Sigma, dHash / pHash / NCC similarity thresholds, cross-validation or class weighting in core-suggest, a CPU result that disagrees with the GPU, numerical instability, wrong pixels, ask Astra, GPT-6-Astra, gpt-6-astra.
disable-model-invocation: true
---

# Darkroom Numerics Review (GPT-6-Astra)

Independent second opinion on Darkroom's numerically-critical code via the Codex CLI's
`gpt-6-astra`. Astra's job is to **derive, diagnose, attack, and review — never to implement**: it
returns a specification or a set of findings, and Claude verifies it and writes the code. Run only
when the user explicitly invokes `/codex-darkroom`.

For anything outside the scope gate below, use the generic `codex-plan-review` /
`codex-implementation-review` / `codex-brainstorm` skills — those route across the
`gpt-5.6-terra/sol/luna` family and are the right tool for ordinary review.

## Routing — who owns which kind of mistake

> **If a mistake produces the wrong pixel, involve Astra. If a mistake produces broken software,
> involve Fable.**

| Astra owns | Fable owns |
|---|---|
| Colour science, tone operators, gamut and output transforms | Implementation, cross-crate integration, refactors |
| HDR radiometry, merge weighting, deghost consistency | IPC, SQLite, import, thumbnails, caching, UI, CI |
| Panorama geometry, estimation, optimisation | wgpu resource lifecycle, bindings, uniform packing |
| Shader and CPU develop math | Literature *retrieval* and long autonomous runs |
| Denoise DSP, noise models, tiling and windowing | Visual artefact *characterisation* (`gpu-visual-qa`) |
| Similarity metrics, statistical modelling, evaluation | Turning an accepted spec into a landed diff |
| Turning retrieved literature into a derivation | Deciding whether to ship it |
| Final adversarial review of numerical code | Everything the scope gate lists as out of scope |

Bug routing, when the cause is not yet known:

| Symptom | Start with |
|---|---|
| Wrong pixels · wrong colours · panorama distortion · HDR ghosting · denoise artefacts · numerical instability · **a GPU result that disagrees with the CPU path** | **Astra** |
| Memory growth · state desync · Tauri lifecycle · import workflow · DB consistency · UI state · a regression that appeared after a refactor | **Fable** |
| Multi-layer, root cause unknown | Fable first; hand over to Astra the moment the fault localises onto the pixel path |

## Scope gate — check this before anything else

Full paths, invariants and quotable constants: `references/invariants.md`.

| Area | Paths |
|---|---|
| RAW colour path | `crates/core-raw/src/color.rs`, `develop.rs` (`map_3ch_to_rgb`, `reconstruct_clipped`), `heif.rs` (PQ EOTF + anchor), `display.rs` (sRGB OETF, `srgb_to_prophoto`), `pano.rs` / `hdr_dng.rs` (XYZ matrix inversion only) |
| GPU develop math | `crates/core-pipeline/src/develop.wgsl`, `curve.rs`, `base_curve_ref.rs`, `mask_prepass.wgsl`, `mask_refine.wgsl`, `brush_bake.wgsl`, and **`params.rs:603–1139`** — WB/Planckian locus, Bradford CAT, grading-RGB matrices, the ACR base-tone fit, the base LUT, crop autozoom, lens distortion. `params.rs:1140+` (std140 packing) and `backend.rs` are **out** |
| HDR merge | `crates/core-hdr/src/{lib.rs,warp.rs}` — EV₁₀₀, hat weighting, deghost consistency, the inverse warp; `core-raw/src/hdr_file.rs` numerics only. Alignment lives in `core-pano::align`, so an HDR question may legitimately reach into it |
| Panorama geometry | `crates/core-pano/src/{features,matching,ransac,graph,camera,bundle,wave,project,exposure,seam,blend,rectangle,crop,detect,align,rng}.rs` |
| Denoise and analysis DSP | `crates/core-analyze/src/denoise.rs` (Bayer pack, CFA phase, tiling, feather, k-Sigma, bilateral), `metrics.rs`, `presence.rs`, `face_aligner.rs` (Umeyama), and the decode geometry in `face_detector.rs` / `detector.rs` / `megadetector.rs`. **Not** the ORT/CoreML plumbing or `models.rs` |
| Colour management | Display P3 / AdobeRGB / ICC output transforms, gamut mapping, output sharpening, 3D LUT interpolation (`@binding(16)`, planned) |
| Statistical modelling | `crates/core-suggest/src/{fit,cv,metrics,weights,features,model}.rs` |
| Similarity metrics | `crates/core-dedup/src/lib.rs` — dHash, DCT pHash, NCC and edge-NCC, colour distance, the `accepted_pair` rule, medoid clustering |
| Library-side numerics | `crates/core-library/src/{face_cluster.rs,features.rs}` and the clustering math in `pano_detect.rs`. The rest of `core-library` is out |

Explicitly **out of scope** — refuse and redirect: React/UI, `src/lib/ipc.ts`, `commands.rs`
plumbing, SQLite schema and migrations, import/dedup workflow, Tauri lifecycle, thumbnails and
caching, ORT model loading and downloads, CI/release, logging, wgpu resource lifecycle in
`backend.rs`, std140 packing in `params.rs`.

When a target straddles both (a `params.rs` change that alters both packing and shader math), scope
the packet to the math alone and say the plumbing half is out of scope.

## The gate

**Hard conditions — never waived.** If one fails, do not write a packet.

1. The target is in the scope table.
2. The question is mathematics, not code structure or API design.
3. A wrong answer would be **silently** wrong — it compiles, the goldens pass, the image looks
   plausible.
4. **You wrote down your own best answer first.** This makes Astra's reply falsifiable and turns
   disagreement into signal rather than deference. It is the condition most likely to be skipped and
   the one that most improves the result.
5. No secret, token, `.env` content, or personal photo path appears in the packet.

**Soft condition — disclose, do not block.** Name the harness or golden you already ran and why it
did not settle the question (`references/evidence-harnesses.md`). A question that a harness *could*
eventually settle — after a corpus re-record, a recalibration, or hours of wall-clock — is still
worth asking in parallel. Say that is what you are doing.

Up to **8 questions** per packet, strictly ordered by value.

## Modes

Pick exactly one per run and state it in the approval summary. Full prompt scaffolds:
`references/prompts.md`.

| Mode | Use when | Output |
|---|---|---|
| `explore` | The design space is open and Claude has already done the literature pass. | 3–5 candidate algorithms with assumptions, complexity, numerical failure modes, and what would make each the right choice here. No verdict. |
| `derive` | The approach is chosen; the mathematics is not written. | A specification another engineer can implement without talking to Astra. |
| `diagnose` | Measured numbers show the output is wrong. | Ranked hypotheses, each with a **discriminating experiment**: the exact harness and the number that confirms or kills it. A hypothesis with no falsifying measurement is rejected. |
| `calibrate` | A constant has no provenance. | The experiment that measures it — estimator, input set, statistic, acceptance interval, and the named harness that produces the number. Never a guessed value. |
| `rebut` | Fable has attacked Astra's own spec. **Session-resumed**, so Astra still holds its derivation. | Per critique point: concede with a revised spec, or refute with the argument. Never a silent rewrite. |
| `verify` | An implementation exists. | Adversarial findings; each names which golden should have caught it and why it did not. |

`verify` also takes `SCOPE: diff | function | subsystem`. `subsystem` is a whole-crate audit
(`core-pano`, the RAW colour path, `core-suggest`) — run it at `max` or `ultra`, and expect the
excerpt budget and the run time to grow with it.

## Building the packet

The packet is the question. Astra cannot see the repository in the default access mode, so a fact
that is not in the packet does not exist. The main thread writes the judgment sections (GOAL, MODE,
INVARIANTS selection, MY OWN ANSWER, QUESTIONS, NON-GOALS); the `astra-packer` subagent fills the
mechanical ones — excerpts, constants, golden coverage, measured evidence — without dragging `cargo`
output through the main context.

```
GOAL:                 <one line — what Astra must derive, diagnose, calibrate, attack, or rebut>
MODE:                 explore | derive | diagnose | calibrate | rebut | verify
SCOPE:                <verify only: diff | function | subsystem>
SUBSYSTEM:            raw-colour | tone-operator | gpu-develop-math | hdr-merge | panorama-geometry |
                      denoise-dsp | colour-management | suggest-stats | dedup-metric | library-numerics
ACCESS:               sealed | repo-read
ROUND:                <n of m; session id when resuming>
WORKING SPACE:        <colour space, encoding (linear/log/OETF), white point and units of EVERY
                      quantity below. Default working space: linear wide-gamut ProPhoto,
                      scene-referred, D50, values >1.0 preserved.>
PIPELINE POSITION:    <where in the chain this sits — canonical order in references/invariants.md>
INVARIANTS:           <verbatim-quoted from references/invariants.md — never paraphrased from memory>
VERSIONING IMPACT:    <does the answer change developed pixels? current PROCESS_VERSION / DECODER_VERSION>
CURRENT MATH:         <verbatim excerpts, file:symbol — see the excerpt budget below>
MEASURED EVIDENCE:    <real numbers: exact command run, input file, output. Never "looks green">
IMAGES:               <each attached render: what it shows, which parameters, which measurement it
                      illustrates. An image never replaces the numbers.>
EXPECTED / REFERENCE: <what ACR/LR/dcraw/the spec gives for the same input, or "unknown">
LITERATURE:           <explore/derive: papers, standards and reference implementations Claude
                      retrieved, with citations. Astra's own web search stays disabled.>
EXISTING CONSTANTS:   <each with a provenance tag: derived | literature<cite> | calibrated<how> | unknown>
GOLDEN COVERAGE:      <which tests pin this, what they assert, and what they do NOT cover>
PRIOR FINDINGS:       <matching docs/astra/FINDINGS.md id, or "none found — checked">
MY OWN ANSWER:        <your best hypothesis, stated plainly — hard gate condition 4>
NON-GOALS:
QUESTIONS FOR ASTRA:  <numbered, ORDERED BY VALUE — most valuable first, up to 8>
```

Rules that carry unusual weight:

- **Order the questions by value.** Astra answers in order, so a run cut short still lands the
  answer you most needed. Q1 must be the one you would pay for alone.
- **Never invent a constant in the packet.** Quote it from source with `file:line`, or tag it
  `unknown`. If Astra asserts a different value, that is a finding to verify, not a fact to adopt.
- **Excerpt budget**: ≤150 lines for a bounded `diagnose`/`verify`/`calibrate`; ≤600 lines for
  `derive`, `explore`, or `verify SCOPE: subsystem`; none in `repo-read`, where Astra reads for
  itself. Exceeding the budget for a bounded question means the question is too broad — split it.
- Check `docs/astra/FINDINGS.md` before treating something as novel. If a finding already covers it,
  give Astra the id and ask it to attack the *proposed fix*, not rediscover the problem.

## Access modes

**`sealed` (default).** Astra runs from the scratchpad with no repository access. This is a
quality device, not a budget one: it forces Claude to state its own answer, keeps invariants quoted
rather than half-remembered, and makes the reply reproducible from an artefact that is logged.

**`repo-read` (gated).** Astra runs read-only inside the repository. Use it only when:

- two sealed rounds have failed on missing context, **or**
- the chain provably spans three or more crates (a develop question that reaches from `core-raw`
  through `core-pipeline` into export).

The packet is still sent, with the line *"the packet is the question; the repository is for
verification, not for rediscovery"*. Log it as `Access: repo-read` — its answers are the least
reproducible, because the inputs are the tree at that moment rather than a saved artefact.

`workspace-write`, `danger-full-access` and `--dangerously-bypass-*` stay forbidden in every mode.

**Images.** `-i <png>` attaches renders from `gpu-visual-qa` (Astra's `input_modalities` are text
and image). An image never replaces a number: every attached render still ships its patch means,
deltas and percentiles in `MEASURED EVIDENCE`. The image exists so Astra can see *where* an artefact
sits, not to be measured by eye.

## Effort routing

Model is fixed to `gpt-6-astra`; only effort varies. Default **`xhigh`**. `low` and `medium` are
never used — a question that cheap should not leave the main thread.

| Situation | Effort |
|---|---|
| `verify SCOPE: diff` inside one function · `calibrate` · a resumed delta round | `high` |
| **Default**: `derive` with a known reference · single-suspect `diagnose` · `explore` | `xhigh` |
| Novel algorithm · cross-stage `diagnose` · `verify SCOPE: subsystem` · a disputed finding · a regression that has survived multiple sessions | `max` |
| Whole-subsystem audit · multi-candidate `explore` where each candidate needs its own derivation · the final review of a change to the RAW colour path, panorama bundle/seam, HDR merge, or the base tone operator | `ultra` — **one explicit approval** |

- **A strong packet at `xhigh` beats a weak packet at `ultra`.** Escalate the packet before the effort.
- `ultra` is real: `~/.codex/models_cache.json` defines it for `gpt-6-astra` as "Maximum reasoning
  with automatic task delegation", with `multi_agent_version: v2` and sub-agents at `xhigh`. Say so
  when asking for it, and note that `--ignore-user-config` discards the local
  `[agents] max_threads = 6 / max_depth = 1`, so delegation breadth falls back to server defaults.
- A wrong effort string fails fast — no silent downgrade.

## Approval summary

Print this and wait for explicit approval before running:

```
Darkroom numerics review (GPT-6-Astra)
  Mode:          <explore | derive | diagnose | calibrate | rebut | verify[:scope]>
  Pattern:       <A | B | C | one-off>   Round: <n of m>
  Subsystem:     <…>
  In scope:      <path(s) matched against the scope table>
  Model:         gpt-6-astra          Effort: <high | xhigh | max | ultra*>
  Access:        <sealed | repo-read*>    Session: <new | resume <id>>
  Packet:        <path>  ~<n>k tokens  ·  <m> questions, value-ordered  ·  <k> images
  Prior finding: <FINDINGS.md id | none found — checked>
  My own answer: <one line — hard gate condition 4>
  Web:           disabled
  Output:        <explore: candidates | derive: specification | calibrate: experiment |
                  diagnose/verify/rebut: findings> — not code
  Purpose:       <one sentence>
```

`*ultra` and `*repo-read` each need explicit approval. Approving a **pattern** (below) approves its
calls as a unit; anything outside the pattern is a fresh approval. Never silently change model,
effort, access mode, or scope.

## Call patterns

Approved as a unit, not per call. Full commands and session mechanics: `references/workflows.md`.

- **A — standard numerics change**: `derive` → implement → `verify`. Two calls.
- **B — wrong pixels**: `diagnose` → Claude runs the discriminating experiment → `diagnose` round 2
  (resumed, carrying the measurement) → fix → `verify`. Three calls, session-backed.
- **C — high-risk** (RAW colour path · panorama bundle/seam · HDR merge and deghost · base tone
  operator): `explore` → `derive` → Fable critique via `reviewer-critical` → `rebut` → implement →
  `verify` at `max`. Four calls, session-backed. Recommended, not mandatory — say so if you shorten
  it to A.

A re-ask of the same question is legitimate only with a materially better packet or a higher effort,
and the approval summary must say what changed. Re-sending an unchanged packet hoping for a better
answer is not.

## Command

```bash
SCRATCH="<session scratchpad>/astra"; mkdir -p "$SCRATCH"; cd "$SCRATCH"
STAMP="$(date +%Y-%m-%d)-<subsystem>"
codex exec \
  -m "gpt-6-astra" \
  -c 'model_provider="openai"' \
  -c 'model_reasoning_effort="<EFFORT>"' \
  -c 'model_verbosity="high"' \
  -c 'web_search="disabled"' \
  -c 'approval_policy="never"' \
  -s read-only \
  --strict-config \
  --ignore-user-config \
  --skip-git-repo-check \
  -o "$SCRATCH/out-$STAMP.md" \
  - > "$SCRATCH/stdout-$STAMP.log" 2> "$SCRATCH/stderr-$STAMP.log" <<'CODEXEOF'
<PROMPT>
CODEXEOF
echo "exit=$?"
grep -m1 'session id:' "$SCRATCH/stderr-$STAMP.log"   # record it — this is the resume handle
```

- `<PROMPT>` = the packet, wrapped in the mode's scaffold from `references/prompts.md`.
- Run from the scratchpad, **never** from the repo, in `sealed` mode. `--skip-git-repo-check` is
  required because the scratchpad is not a git repo.
- **No `--ephemeral`**: the session is what makes `rebut` and resumed rounds possible. Add
  `--ephemeral` only for a deliberate one-shot with no follow-up.
- **Always capture stdout and stderr to files.** The stderr log carries both `session id:` and the
  `tokens used` figure; truncation detection depends on the output file.
- `repo-read` variant, image attachment, and `codex exec resume` (which takes **no** `-s` flag —
  sandbox goes through `-c sandbox_mode="read-only"`) are in `references/workflows.md`.
- Never use `workspace-write`, `danger-full-access`, or `--dangerously-bypass-approvals-and-sandbox`.
- Web search stays disabled — Claude does the literature retrieval, Astra derives from it.
- `max` and `ultra` runs can exceed the 10-minute Bash ceiling. Launch them with
  `run_in_background: true` and poll the output file.

## Detecting a truncated or failed run

Every mode's output contract requires the reply to end with the literal sentinel
`=== END OF ASTRA RESPONSE ===`, preceded by a `SECTIONS: <n>/<total>` line. Without the sentinel you
cannot distinguish "Astra answered briefly" from "Astra was cut off" — by a context limit, a
usage limit, or a transport failure.

| Signal | Meaning |
|---|---|
| exit 0 · output non-empty · ends with the sentinel | Complete |
| exit 0 · output non-empty · **no sentinel** | Truncated mid-generation |
| exit ≠ 0 · output non-empty | Killed by the CLI — read the stderr log |
| exit ≠ 0 · output missing or empty | Failed before producing anything |

The output file has **no trailing newline** after the sentinel, so test the last line, not the last
byte.

```bash
OUT="$SCRATCH/out-$STAMP.md"; ERR="$SCRATCH/stderr-$STAMP.log"
if   [ ! -f "$OUT" ]; then echo "STATE=missing"
elif [ ! -s "$OUT" ]; then echo "STATE=empty"
elif [ "$(tail -n1 "$OUT")" = "=== END OF ASTRA RESPONSE ===" ]; then echo "STATE=complete"
else                       echo "STATE=truncated"; fi
grep -niE 'usage limit|rate limit|quota|429|resets' "$ERR" | head
grep -A1 -i 'tokens used' "$ERR" | tail -2
```

**Salvage.** A truncated `diagnose`, `verify` or `explore` run is usually usable: items are emitted
independently and in value order, so the prefix that arrived is the part you most wanted. A
truncated **`derive` is not usable as a specification** — a half-finished derivation can be
internally consistent, read as complete, and still be wrong at exactly the step that never arrived.
Record it, never implement from it.

Either way, say plainly to the user that the response was partial and which sections are missing,
record the run `PARTIAL` in `docs/astra/LOG.md`, and continue it with a **resumed delta round**
(`references/workflows.md`) rather than re-sending the original packet.

## Proactive-offer duty

This skill never runs itself (`disable-model-invocation: true`), but staying silent about it is also
a failure. **Name the exact call — mode, effort, Q1, what it would cost — and wait for a yes or no**
whenever:

- a diff about to be committed touches a scope-table path;
- a `PROCESS_VERSION` bump is proposed;
- a plan introduces a new algorithm on the pixel path;
- a golden moved and the movement was not the intended one;
- a constant is about to land without `derived`, `literature`, or `calibrated` provenance;
- a CPU path and the GPU path disagree.

Offer, never run. One offer per issue — if it is declined, proceed without it and do not re-ask.

## Secrets

Never put secrets, tokens, keys, `.env` contents, or personal photo paths in the packet. Use
corpus-relative or anonymised paths (`tests/corpus/...`, `<library>/2026/...`). This applies to
`repo-read` runs too — Astra can read the tree, but the tree is not where those live.

## Failure policy

1. Report the exact error.
2. Retry at identical settings at most twice for a plainly transient failure (network, transport).
   **Never retry a usage limit** — stop, report any reset time verbatim, and resume later with a
   delta round.
3. Lower effort only for a confirmed cost/latency problem, and only with approval.
4. If `gpt-6-astra` is unavailable, stop and offer `codex-plan-review` / `codex-implementation-review`
   at `gpt-5.6-sol`/`xhigh` as an **explicitly-labelled** substitute — never silently substitute a
   different model under the Astra label.
5. Auth, missing-CLI, invalid-model, permission and config errors are not fixed by lowering effort —
   surface them.

## Integrating the result

Astra's output is a specification, an experiment, or a set of findings — **not code**. For each item:

1. Verify it against the actual source before acting; record basis `verified | inference | unknown`.
2. Check every constant against its stated provenance. An unsourced number is a defect, not an answer.
3. **Run the supplied synthetic test vectors before implementing anything.** They exist so the answer
   can be falsified locally.
4. Reject unsupported claims and say which, and why. Disagreeing with Astra is a normal outcome; a
   `rebut` round is how a disagreement gets settled, not a sign the call failed.

An accepted spec becomes the `DESIGN:` block of a delegation brief handed to
`astra-spec-implementer` (or the global `impl-critical`). Distil it into `docs/astra/FINDINGS.md`
under a stable id, and append the call to `docs/astra/LOG.md`. Never paste Astra's raw output as a
finished answer.

## Relationship to fable-orchestrator

Fable stays the main thread and the decision-maker. The Astra call is a Bash invocation, not an
agent — it costs the main context only the packet summary and the distilled result.

```
fable-explore / fable-plan  →  codex-darkroom (explore → derive)  →  PLAN.md carries the spec
   →  reviewer-critical (critique)  →  codex-darkroom (rebut)
   →  fable-run / astra-spec-implementer  →  gpu-visual-qa  →  codex-darkroom (verify)
```
