# FAQ, imprecision and recommendations

Known limits of the analysis and practical guidance. Part of the [analysis documentation](ANALYSIS.md).

## Recommendations

- Prefer a source-backed analysis or `trace-merge` over an IPC fallback model when the framework's sources are available; use `--models` for implementations that genuinely cannot be indexed. See [Function models](MODELS.md) and the [source-backed workflow](../README.md#cross-project-analysis-with-framework-sources).
- Supply `--include`, `--dep` and `--compile-commands` when declarations, receiver types or build flags are missing. See [Dependency roots](USAGE.md#dependency-roots) and [Compilation databases](USAGE.md#compilation-databases-62).
- Use `--explore` for configuration-dependent code and read [its limits](USAGE.md#limits-of---explore) before relying on the extra facts.
- Treat `trace inspect slice` as an adviser query, not a race report; it shows evidence under a bounded walk. See [Value slice](USAGE.md#value-slice-inspect-slice).
- Enable `--sequencing` only when call order matters; it adds a sidecar graph and export cost. See [Call sequencing](USAGE.md#call-sequencing-control-flow-sidecar).
- Filter logging and boilerplate macros with `--ignore-macro` / `--ignore-logging` so they do not create call sites and temporaries. See [Noise macro filtering](USAGE.md#noise-macro-filtering---ignore-macro---ignore-logging-noise).
- Re-measure the pinned corpora with `scripts/eval_check.py` when a change moves counts; keep measurements in `docs/EVAL_REPORT.md`.

## Known imprecision

- All paths merged; no null-check refinement.
- Call order (`--sequencing`) is control-flow reachability, not path feasibility; see [Call sequencing](USAGE.md#call-sequencing-control-flow-sidecar), "Limits".
- `free` does not invalidate pointers.
- `FieldSummary` may connect unrelated struct instances. After a move
  from one object to another, both the old and new writes to a callback
  field may reach a call on the destination object.
- **Moved-from values.** The may-analysis preserves the source's
  possible pointees after `std::move`. A moved-from `std::unique_ptr`
  may still resolve calls on its former pointee. Ownership transfer
  and moved-from invalidation are outside the model.
- **Lazily created field cells carry no slot guard** (see
  [Variable cells](POINTERS.md#propagation-highlights)): their recorded type is not reliable enough to guard by.
- Multiple vtable/ops targets reported for one indirect site (may-analysis).
- **Casts of struct instances to another ops type** (`svc = (IOps *)&inst`):
  the whole instance flows into the target-typed slot, so field loads on it
  resolve against the *outer* layout plus its type-matched summary — sibling
  fields at colliding positional indexes can cross into such loads (observed
  as ~2% of Dispatch-site edges on HDF test drivers).
- **Signature-guarded propagation drops cross-signature fn values** (see
  [Signature-guarded function-value propagation](POINTERS.md#propagation-highlights)): calls through cells whose
  declared fn-pointer arity mismatches the stored function are not reported.
  Sites whose only reachable "targets" arrived via such wrong-type flow now
  report none (e.g. stub-side `super->X` calls behind an opaque IPC
  boundary in HDF — their baseline targets were cross-object pollution, not
  real resolutions).
- **IPC / process boundaries are only partly modeled**: OpenHarmony proxy
  operations connect to conventionally named stub handlers through synthetic
  `ipc` call edges, including reconstruction from separately indexed repositories;
  see [OpenHarmony IPC bridges](MODELS.md#openharmony-ipc-bridges). Opcode dispatch,
  parcel flow and `HdfRemoteServiceObtain`-style registrations remain unmodeled.
- **`memcpy` / `memmove`**: modeled through [function models](MODELS.md#function-models-configurable-summaries);
  unmodeled copier names remain invisible.
- Macro-generated identifiers may be skipped when classified as macro-like callees.
- Function pointer resolution is name/linkage based. **`dlsym` / `GetProcAddress`**: a string constant in the name argument (literal or a variable that receives one) is looked up among indexed functions of that exact name; missing symbols stay unresolved. `dlopen` that only runs `REGISTER` static constructors is the C3 factory path, not modeled here. The DSO handle is not used to restrict candidates. `sprintf` into a name buffer is unmodeled.

## Performance notes

Whole-program HDF-scale runs (~600 TUs, ~11k functions) target roughly:

| Phase | Typical |
|-------|---------|
| Index | ~25s (parallel preprocess + parse) |
| Analyze | ~0.3s |
| Export (minimal) | ~0.1s |

Key optimizations: solver adjacency index, `loc_nodes` reverse index, worklist dedup, lazy abstract locations, minimal SQLite export, skipped redundant header indexing.
