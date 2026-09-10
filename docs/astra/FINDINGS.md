# Astra findings register

One entry per accepted result from a `/codex-darkroom` call. Ids are stable and never reused:
`A-1`, `A-2`, … Cite the id in later packets (`PRIOR FINDINGS:`) so a question is never paid for
twice.

**Status** is one of:

| Status | Meaning |
|---|---|
| `accepted` | Verified against source; the spec or finding stands |
| `accepted-partial` | Response was truncated by a usage limit; the sections listed arrived and were verified, the rest are missing |
| `rejected` | Verified against source and found wrong; kept so it is not re-asked |
| `open` | Accepted but not yet implemented |
| `implemented` | Landed; names the commit |

A **partial `derive` response is never `accepted`.** A half-finished derivation can read as complete
and be wrong at exactly the step that never arrived — record it `accepted-partial`, do not implement
from it, and resume with a delta packet.

## Entry template

```
## A-<n> — <one-line title>

- **Date**: YYYY-MM-DD
- **Mode / subsystem**: <derive|diagnose|verify> / <subsystem>
- **Effort**: <high|xhigh|max>
- **Status**: <accepted|accepted-partial|rejected|open|implemented>
- **Question asked**: <the Q1 that drove the call>
- **Result**: <2-6 lines — the specification, localisation, or finding>
- **Constants introduced**: <value · provenance tag · where the provenance came from>
- **Test vectors**: <where they live; whether they were run and what they returned>
- **Verification**: <what was checked against source, and what was rejected>
- **PROCESS_VERSION impact**: <yes/no + why>
- **Sections missing** (partial only): <which>
```

---

_No findings yet. The first planned call is a `verify` over `core-pano::bundle` / `seam`._
