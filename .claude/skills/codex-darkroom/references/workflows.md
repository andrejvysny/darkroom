# Workflows — patterns, sessions, access modes

Everything here was verified against `codex-cli 0.153.4` and `gpt-6-astra` on 2026-09-11 with
throwaway probe calls. Where a behaviour is verified, it says so; where it is not, it says that too.

---

## Session mechanics

`--ephemeral` is what suppresses the session. **Leave it off** and every run is resumable, which is
what makes `rebut` and multi-round `diagnose` possible.

Capturing the handle — no `--json` needed, the plain run prints it on **stderr**:

```bash
grep -m1 'session id:' "$SCRATCH/stderr-$STAMP.log"
# session id: 01a0907e-f083-7532-b848-223df3af3e48
```

With `--json` the same id arrives as the first event: `{"type":"thread.started","thread_id":"…"}`.
The final event carries usage: `{"type":"turn.completed","usage":{"input_tokens":…}}`. In the plain
(non-JSON) form, stderr ends with a `tokens used` line and the count on the line after it — that is
the figure `docs/astra/LOG.md` records.

Resuming:

```bash
codex exec resume "<SESSION_ID>" \
  -m "gpt-6-astra" \
  -c 'model_reasoning_effort="<EFFORT>"' \
  -c 'sandbox_mode="read-only"' \
  -c 'web_search="disabled"' \
  -c 'approval_policy="never"' \
  --strict-config --ignore-user-config --skip-git-repo-check \
  -o "$SCRATCH/out-$STAMP-r2.md" \
  - > "$SCRATCH/stdout-$STAMP-r2.log" 2> "$SCRATCH/stderr-$STAMP-r2.log" <<'CODEXEOF'
<DELTA PROMPT>
CODEXEOF
```

Three differences from `codex exec`, all verified by failing on them first:

- **`resume` rejects `-s/--sandbox`** (`error: unexpected argument '-s' found`). Set the sandbox with
  `-c 'sandbox_mode="read-only"'` instead.
- `resume` also has no `-C/--cd`, no `--add-dir`, no `--profile`. It inherits the original session's
  working root.
- `resume --last` picks the newest session in the current directory; pass the id explicitly whenever
  you have it, because "newest" is not stable across parallel work.

Astra genuinely retains the earlier turn — a probe asked it to quote its previous reply verbatim and
it did. A resumed round therefore sends **only the delta**, never the original packet.

### Delta round

```
This continues the analysis in this session. Do not restate, revise, or re-derive anything already
produced — it stands. Produce only what is asked below, then the SECTIONS line and the sentinel.

CONTINUE FROM: <first missing section, by name>          # after a truncated run
NEW EVIDENCE:  <the discriminating experiment you asked for, its exact command and its output>
CRITIQUE:      <the numbered critique points, verbatim>  # rebut mode
```

If the original run was `--ephemeral` (no session), fall back to the one-shot delta packet in
`prompts.md`, which quotes the already-produced sections back into a fresh call.

---

## Access mode: `repo-read`

Gated — two failed sealed rounds, or a chain that provably spans three or more crates. The packet is
still sent; the repository only lets Astra check what the packet asserts.

```bash
codex exec \
  -m "gpt-6-astra" \
  -c 'model_reasoning_effort="<EFFORT>"' \
  -c 'model_verbosity="high"' \
  -c 'web_search="disabled"' \
  -c 'approval_policy="never"' \
  -s read-only \
  -C "/Users/andrejvysny/workspace/darkroom" \
  --strict-config --ignore-user-config \
  -o "$SCRATCH/out-$STAMP.md" \
  - > "$SCRATCH/stdout-$STAMP.log" 2> "$SCRATCH/stderr-$STAMP.log" <<'CODEXEOF'
<PROMPT — repo-read header variant from prompts.md>
CODEXEOF
```

- `--skip-git-repo-check` drops out (the repo is a git repo).
- The excerpt budget drops out with it — but `INVARIANTS`, `MY OWN ANSWER`, `MEASURED EVIDENCE` and
  the value-ordered questions all stay. Those are quality rules, not budget rules.
- `-s read-only` covers the whole tree; the path list in the prompt is guidance, not enforcement.
  Say in the packet which paths are relevant and that everything else is noise.
- Log `Access: repo-read`. Its answers are the least reproducible of any mode, because the input was
  the tree at that moment rather than an artefact that was saved.

---

## Attaching images

Verified: `gpt-6-astra` reads attached PNGs precisely — a probe named an 8×8 checker in a 64×64 image
correctly from the pixels alone.

```bash
codex exec … -i "$SCRATCH/before.png" -i "$SCRATCH/after.png" … - <<'CODEXEOF'
```

- `gpu-visual-qa` writes the crops; each one still ships its numbers in `MEASURED EVIDENCE`, and the
  `IMAGES` block says what each render shows and at which parameters.
- Crop tight around the artefact and state the source rectangle in pixels — a downscaled full frame
  hides exactly the halo you are asking about.
- **If you pass the prompt as a positional argument instead of `-`, redirect stdin**
  (`… "prompt text" < /dev/null`) or the CLI blocks on "Reading additional input from stdin…".

---

## `--output-schema` (experimental here)

`codex exec --output-schema <FILE>` constrains the final message to a JSON Schema, which would make
`verify` findings machine-parsable straight into `FINDINGS.md`. It is **not yet used by this skill**:
the sibling `codex-implementation-review` records that it is unreliable on the native review path,
and it has not been tried on this one. If you try it, keep the sentinel requirement — a schema'd
reply still has to prove it was not truncated — and record the result in `LOG.md`.

---

## Pattern A — standard numerics change

Two calls. Use when the approach is already chosen and the risk is bounded.

1. **`derive`** at `xhigh`, sealed. Packet carries `LITERATURE` if Claude did a retrieval pass.
2. Claude runs the spec's synthetic vectors as a standalone test **before** touching production code.
   If they do not reproduce, stop — the spec is wrong.
3. `astra-spec-implementer` implements the spec as written.
4. **`verify SCOPE: diff`** at `high`, resumed in the same session so Astra reviews against its own
   specification rather than re-deriving it.

## Pattern B — wrong pixels

Three calls, session-backed. Use when the output is measurably wrong and the stage is unknown.

1. **`diagnose`** at `xhigh`, sealed. `MEASURED EVIDENCE` carries `gpu-visual-qa` numbers and, where
   it helps, attached crops. Astra returns ranked hypotheses, each with a discriminating experiment.
2. Claude runs the discriminating experiment for the top hypothesis — that is the point of it.
3. **`diagnose` round 2**, resumed, carrying only `NEW EVIDENCE`. Usually at `high`; at `max` if the
   experiment killed every hypothesis, because the chain is then wider than assumed.
4. Fix, then **`verify SCOPE: diff`**, resumed.

## Pattern C — high-risk

Four calls, session-backed. The four areas where a silent error ships a wrong photograph to every
user: the RAW colour path, panorama bundle adjustment and seam, HDR merge and deghosting, and the
base tone operator.

1. **`explore`** at `xhigh` (or `ultra` when each candidate needs its own derivation). Claude
   supplies `LITERATURE`; Astra returns candidates, not a verdict.
2. Claude picks the candidate — this is a Fable decision, not an Astra one — and calls **`derive`**
   at `max`, resumed.
3. **`reviewer-critical`** attacks the specification from the implementation side: missing cases,
   assumptions that do not hold in this codebase, invariants it would break, what the goldens would
   not catch. Its output becomes the numbered `CRITIQUE` block.
4. **`rebut`**, resumed at `high`–`xhigh`. Astra concedes with a revised spec or refutes with the
   argument. A concession that changes a constant must re-tag its provenance.
5. Implement; run the vectors first.
6. **`verify SCOPE: subsystem`** at `max`, resumed.

Recommended, not mandatory. Shortening C to A is a legitimate call — say so explicitly in the
approval summary so the reduced coverage is a decision and not an omission.

---

## The adversarial loop, in one picture

```
        Claude (Fable)                         Astra
   ----------------------------------------------------------
   literature retrieval        →      explore    (candidates)
   pick a candidate            →      derive     (specification)
   reviewer-critical attacks   →      rebut      (concede or refute)
   astra-spec-implementer      →      —
   gpu-visual-qa measures      →      verify     (findings)
   decide, implement, ship     →      —
```

Two independent forms of reasoning, each doing what it is better at. The decision at every step
stays with Claude: Astra never ships a line of code, and Claude never adopts a constant it cannot
source.
