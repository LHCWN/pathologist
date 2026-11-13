# Canonical system-header prelude

Status: experimental, opt-in with `--canonical-system-headers`. The follow-up
reduces complete-tree cost substantially, but does not establish parity with
default indexing. This document describes the final design and investigation;
the [evaluation report](EVAL_REPORT.md#canonical-header-reuse-finalization-2026-11-15)
holds the current measurements and validation results. Historical measurements
below describe earlier implementations, not the final performance claim.

## Motivation

Bare-tree `--system-includes` asks the host compiler for its default search
paths and then indexes the reached standard-library headers.  Those headers
are large and every translation unit reaches a similar closure, so the same
tokens are preprocessed, parsed, lowered and merged repeatedly.  On the
`services/abilitymgr` subset this was the dominant cost:

| Mode | Wall time | Peak RSS |
|---|---:|---:|
| Bare tree, no system includes | 4.5 s | - |
| `--system-includes` | 90.8 s | 580 MiB |
| `--system-includes --canonical-system-headers` | 21.5 s | ~300 MiB |

The target was to remove the per-translation-unit standard-library work while
keeping the existing project-header indexing behavior and dependency ownership
rules.

## Design

### Command line and API

- `trace analyze --canonical-system-headers` sets
  `PreprocessOptions::canonical_system_headers` (builder:
  `with_canonical_system_headers`).
- The mode is only built by the bare-tree (`build_program_inner`) path.  With a
  compilation database, shared command configuration, or link-target
  configured indexing the flag is ignored with a warning; each command has its
  own macro configuration and cannot share one prelude.
- `TRACE_CANONICAL_SYSTEM_HEADERS` is an internal experiment hook.  It is not a
  supported user-facing option.

### Preamble discovery

`canonical_system_preamble` scans every discovered source and project header
text (raw source cache when available, otherwise the file), collects the
spellings of `#include <...>` whose file exists under a compiler-default root,
sorts and deduplicates them, and writes:

```c
#include <algorithm>
#include <memory>
...
```

to an exclusively created `.trace-canonical-std-<random>.tmp` file in the target
root. Each indexing run owns a `NamedTempFile`, including cleanup on failure;
concurrent runs cannot truncate or remove each other's preambles. An existing
`.trace-canonical-std.tmp` file, symlink or hard link is never opened for writing
or removed. The extension avoids discovery as a source/header. The file-probe
epoch is advanced after writing so later `is_file` checks see the new file.
Before the prelude enters the final program, its private scratch path is mapped
to the stable synthetic IR identity `.trace-canonical-std.tmp`. Random names
therefore do not change exported file identities across runs or worker counts.

Spellings are deduplicated **before** searching the roots, including spellings
that do not resolve there. Each distinct spelling is probed once through the
shared directory-listing cache, rather than issuing filesystem probes for
every occurrence in every source/header.

The roots include every path reported by the compiler probe
(`project_compiler_paths`) and explicitly supplied
`compiler_system_include_paths`.  Explicit `-isystem`/`-idirafter` paths are
not added by this step; a project can place aliases there, so those headers
keep the existing per-consumer gate.

### One expansion per compiler-default header

Canonical mode uses a fixed configuration for compiler-default system paths:

- `PreprocessorState::canonical_system` recognizes a path under the system
  compiler-default roots that is not in `separate_header_paths`.
- `matching_variant` returns the first stored variant and ignores the macro
  dependency fingerprint for those paths.
- `publish_variant` is first-wins for them and returns variant 0 even when the
  one-variant journal budget is full.
- `ExpansionJournal` takes a canonical one-variant cap only under compiler-default
  roots. Project headers and explicit system roots retain the normal cap
  (`with_project_cap`/`cap_for`).
- `is_cacheable_header` relaxes the project-preamble and boundary gates for
  canonical bundles, including nested system headers and headers reached after
  project declarations.
- Completion refusals (`uncovered`, context-sensitive, conservative preamble)
  are not applied to canonical bundles.

A cache-priming pass records the macro closure and first standard variants
before discovery, separately from the inline preprocessing/lowering pass below.
Covered includes use the lazy closure; uncovered paths retain canonical variants.

#### Lazy standard macro closure

The indexer records the canonical preamble's
executed macro directives (`PreprocessOptions::record_macro_ops`, returned in
`PreprocessResult::macro_ops`). Initial compiler/command definitions are excluded.
The resulting language-specific closure is applied only when a consumer first
reaches a canonical standard header at a declaration boundary, never at TU startup.
Covered standard includes then contribute no per-consumer declaration records:
their IR is already in the final prelude. Replaying those records would repeatedly
copy standard-header dependency closures into project entries. Only files covered
by the matching language's lowered prelude can take this shortcut; unscanned
standard headers retain ordinary declaration units. Project entries which install
the closure retain its ordered operation log and mark that replay supplies the
same macro environment. Fingerprint checking
for project headers remains enabled. Incoming cold/primed state participates
in fingerprints and deferred journal validation, so a cached cold entry cannot
reset project overrides made after an earlier standard include. The state check
also prevents an entry built after installation from skipping installation in a
cold consumer, even when that header itself never reads a standard macro.

This intentionally exposes the complete canonical standard macro closure at
the first covered include, rather than the per-header portion. Conditional
project code after that include can therefore differ from the original
experimental canonical mode. It is part of the explicit canonical option; ordinary
`--system-includes` retains per-include expansion. Forced includes,
conditional recording/exploration, or project files reached by the preamble
retain the existing path; project guards must never enter this closure.

### Prelude lowering and merge

For each language present in the tree:

1. Build a `PreprocessOptions` with `separate_system_headers = false`,
   `inline_include_bodies = true`, no expansion cache, and enlarged limits
   (512 MiB output, 200M expanded tokens, 128 include depth, 256 expansions
   per file).
2. Preprocess the run's private preamble inline and lower it as one header
   unit with `lower_prepared_source(..., LoweringUnit::Header, ...)`.
3. Compact the resulting `UnitIndex`: merge descriptors, header-merge type
   compaction, drop flow ranges and global initializer ranges, and shrink the
   vectors.

The prelude is merged into the final `Program` **once**, after project headers
and translation units have been merged, using `merge_unit_symbols`.  Doing it
before those merges made every later merge consult a much larger dedup table.
Translation units do not clone the prelude.

Canonical consumers also use the existing bounded reusable-preamble planner
for project-only header lists. Ordinary system indexing admits lists that
contain dependency units; that gate alone would disable reuse once canonical
mode removes system units from the lists. Planning and lowering use the same
admission rule. The existing limit of sixteen snapshots and ordered header
identity remain unchanged; standard-library IR is still absent from them.
The [follow-up measurements](EVAL_REPORT.md#canonical-project-preamble-reuse-follow-up-2026-11-13)
show fewer symbol imports with unchanged normalized semantics, but no established
end-to-end subset speedup.

Project and external source headers keep their own PCH units. Only headers
under compiler-default roots actually covered by the matching language's
prelude are removed from the PCH set. Macro-spelled or otherwise unscanned
standard includes retain their declaration units. Dropping every
non-project file (including external source headers) lost about 8,000 project
call edges on the subset and was rejected.

### Project lookup and storage context

The authoritative sharing rules are in
[Shared system header bundles](PREPROCESSOR.md#shared-system-header-bundles).
Their consequences for this mode are:

- Class-member `using Base::method` imports already have IR metadata and do
  not require the whole header to stay inline. An include inside an existing
  class, namespace or unfinished declaration still stays inline, including a
  standalone-warmed fragment reached later in that scope.
- Global and namespace/linkage using-declarations need syntax at the original
  include position. A shared project header retains a `ConsumerContext`
  projection containing those declarations, namespace aliases and the original
  namespace/linkage wrappers. Inline namespace ownership and LineMap origins
  survive. This prevents an import after a function from changing that earlier
  function's lookup. Macro-expanded imports retain their original provenance.
- Weak pragmas, malformed or unclassified scopes, and TU-specific data keep
  the conservative inline fallback. File-static data and anonymous namespace
  dependencies propagate through live includes, cached replays and guard skips.
  Class static members and proven static function definitions do not by
  themselves require this fallback. The fallback preserves callbacks through
  file-static variables; declarations alone cannot supply their TU storage.
- Project headers with a projection import prerequisite types, their own
  callable declarations and global data declarations for body/result typing.
  They do not recursively import prerequisite function bodies. Literal name
  searches conservatively limit callable imports; constructors, destructors,
  operators and templated names remain candidates, as do names used by stored
  member initializers and default arguments. Substring matches may retain
  extra prototypes and never select a single overload.
- Parameters are imported only for functions retained in that declaration
  import. Filtering functions while retaining their parameters turns those
  parameters into ownerless entries and causes explosive growth through nested
  headers. The regression test checks this ownership boundary directly.
- An external header getter's return binding to an internal C++ function is
  retained during TU replay even when its duplicate body was already merged.
  The existing shared resolver and merger remain responsible for identity,
  linkage and target selection; the projection does not introduce a resolver.

The [shared-project-header approximation](ANALYSIS.md#shared-header-functions)
still applies to implicit incoming declarations. This is not an immutable base
symbol table or a complete C++ module implementation.

### Bookkeeping and cache lifetime

Warm runs use fresh local macro tables with normal compiler/CLI precedence.
The former accumulating warm table was written but never consumed by later
TUs; removing it avoids unnecessary macro-table locking without introducing a
union of project macros. Cached entries still replay their ordered directives.

Include-graph updates trust paths already interned in either canonical file
set and use a membership set for large edge lists. New edges retain input
append order; hash iteration never controls exported order. This avoids
quadratic duplicate checks when preprocessing reports transitive closures.

Header expansion retirement, orphan settlement, bounded source admission and
the sixteen-snapshot preamble limit retain their existing phase order. The
follow-up does not replace the custom preprocessor or weaken project macro
fingerprints to manufacture cache hits.

### Why the final program is merged late

The earlier design inserted the prelude into every translation unit's selected
header list.  That matched per-consumer standard-library typing more closely
(18,403 project direct edges against 18,428 for ordinary system indexing) but
cloned the prelude program for every unit.  On the subset it peaked near
405 MiB; on the full tree it was 478 s and 2,710 MiB.  The final merge removes
the per-unit clone and lowers the subset to 21.5 s / ~300 MiB, at the cost of
not having standard declarations available while a translation unit is
lowered.  Project smart-pointer typing can therefore move toward the no-system
result; the full-tree output has more project edges (765,893 vs 705,900), not
fewer.

### Deliberate precision loss

This mode is an explicit speed-for-precision trade, not a more precise model.
The accepted losses are:

- **One macro configuration for all translation units.** A standard header is
  expanded once, under the preamble's macro environment. Project macros that
  change a standard-library `#if` arm, a typedef spelling, an inline overload,
  or a declaration set are not represented per unit. This can change function
  signatures, add or remove declarations, and change which overload a call
  binds to.
- **No standard declarations during translation-unit lowering.** The prelude
  is merged into the final `Program` after project headers and TUs. Code that
  needs a standard type to infer a member call (notably
  `std::shared_ptr<T>::operator->` and related smart-pointer unwrapping) can
  lower differently or lose a member call. The subset gained project edges
  (18,428 -> 22,949) rather than losing them, but that is a movement toward the
  no-system result, not evidence of greater accuracy.
- **Conditional variants are collapsed.** The first stored expansion wins for
  compiler-default roots. `#if` arms, feature macros, and ABI/type differences
  selected by project defines are not explored; `--explore` cannot recover
  them for these paths.
- **The result is not semantically equivalent to ordinary
  `--system-includes`.** On the subset, functions changed from 17,149 to
  14,905, project direct edges from 18,428 to 22,949, and 154,019 normalized
  semantic rows were deterministic between canonical runs but not equal to the
  ordinary run. Treat this mode as a faster approximate graph, not as a
  replacement when system-header typing matters.
- **Configured indexing is unsupported.** Compilation database commands carry
  different macro configurations; the flag is ignored there rather than
  silently mixing configurations.
- **Default-mode parity is not established.** Complete-tree processing is
  substantially faster than the first canonical implementation, but still
  processes more declarations and macro context than default indexing. Use the
  current evaluation table rather than the historical timings below.

Use the mode when a bare-tree call graph is needed quickly and approximate
standard-library typing is acceptable. Do not use it when overload identity,
ABI-sensitive typedefs, conditional standard-library behavior, or per-command
configurations are part of the question. The existing
`--system-includes` mode remains the conservative choice.

### Compile-commands companion change

The same branch also improves the configured path. The include probe now
records compiler-default roots in
`PreprocessOptions::compiler_system_include_paths` (with
`with_compiler_system_include`) so compiler-default headers may be separated
after project declarations and a project-header preamble. Explicit `-isystem`
roots keep the declaration/preamble gate because a project can interpose
aliases there. Probe results are cached across build directories when the
compiler printed absolute search entries and environment include paths are
absolute too (see the [probe cache contract](PREPROCESSOR.md#compilation-database-options-62)). A bare driver name is resolved
once per run. A database with many `directory` values therefore no longer
starts the compiler once per directory.

Measured on release builds with one worker and synthetic projects:

| Case | Before | After | Result |
|---|---:|---:|---|
| `own_first`, 20 TU | 1.044 s | 0.206 s | settle 20 -> 1 TU; source-lower 0.428 -> 0.063 s |
| `direct`, 20 TU | 0.262 s | 0.209 s | unchanged output |
| `unique`, 20 TU | 0.516 s | 0.398 s | unchanged output |
| 120 command directories | 20.159 s | 0.564 s | all analysis tables and `database_identity` match |
| 240 command directories | 41.170 s | 0.739 s | all analysis tables and `database_identity` match |

This part is independent of canonical mode and is safe for configured
indexing. Regressions:
`compiler_default_system_headers_share_after_project_declarations`,
the absolute/relative probe tests, and the project-alias refusal test.

## Historical measurements before the reuse follow-up

All measurements below use Windows x64, MSYS2 MinGW-w64 GCC 15.2 headers and
`--jobs 8`.  Times are wall-clock observations, not repeated medians unless
stated.

### `services/abilitymgr` (236 TUs)

| Mode | Wall | Peak RSS | Functions | Project direct edges |
|---|---:|---:|---:|---:|
| No system includes | 4.5 s | - | 6,837 (DB) | 26,449 (DB) |
| `--system-includes` | 90.8 s | 580 MiB | 17,149 | 18,428 |
| Canonical, per-TU prelude | 23.3-31.5 s | ~405 MiB | 14,873 | 18,403 |
| Canonical, final merge | 21.5-26.7 s | 297-315 MiB | 14,905 | 22,949 |

Canonical final-merge phases: preprocessing 1.5-2.2 s, PCH 2.1-3.0 s over 100
units, TU parse 5.4-8.5 s, index 19-24 s.  Two runs exported 154,019 identical
normalized semantic rows.

### Complete `ability_ability_runtime` (3,456 TUs, 3,110 headers)

Master `2a51d50` was built in a clean worktree with a separate target directory.

| Mode | Wall | Peak RSS | Functions | Project direct edges |
|---|---:|---:|---:|---:|
| Master, no options | **114.3 s** | **1,877 MiB** | 85,445 | 705,900 |
| Canonical, final merge | 419-536 s | ~2,700 MiB | 93,579 | 765,893 |

Phase comparison:

| Phase | Master | Canonical |
|---|---:|---:|
| Preprocess | 9.9 s (discovery 7.6) | 61-128 s |
| PCH | 10.1 s / 2,524 units | 67-104 s / 1,238-1,375 units |
| TU parse | 37.0 s | 90-151 s |
| Index | 80.6 s | 370-489 s |

`TRACE_PROFILE_INDEX=1` explains the regression:

| Metric | Master | Canonical |
|---|---:|---:|
| `source-lower` | 117.8 s / 8,722 calls | 501.8 s / 5,797 calls |
| `syntax-parse` | 34.6 s | 144.6 s |
| `ast-lower` | 78.6 s | 336.4 s |
| `merge-symbols` | 345,784 calls / 100.5 s | 619,267 calls / 131.3 s |
| `merge-types` | 183,883 calls / 17.3 s | 5,288 calls / 1.1 s |

PCH text itself is small in canonical mode (2,183,803 bytes over 1,766 units,
largest 35,086 bytes).  The cost is therefore not the amount of system text
that is replayed.  It is the project/external header lowering and repeated
cache misses: canonical mode has fewer units but each is much more expensive.
Suppressing or skipping the replayed system text changed little (see below).

## Approaches tried and rejected

### Context relaxation for compiler-default roots

Allowing compiler-default standard headers to share after project declarations
reported 43-150 cache keys across configurations, reduced skipped
headers from 405 to 77, and parse to about 34 s (total about 59 s on the
subset).  It changed type spelling, so it was not kept as a general rule.  The
canonical mode now deliberately accepts a single configuration instead of
generalizing this relaxation to ordinary indexing.

### Declaration-signature dedup of PCH units

Deduplicating 402 units to 329 by declaration signature/text saved about
1-1.5% and regressed types (`size_t` became `int` instead of `long`).  Rejected.

### Larger variant budget plus context relaxation

`max_expansion_variants = 32` with context relaxation produced 1,407 units,
475 s, and 22 GB peak.  Rejected on both time and memory.

### Smaller index window / byte budget

Changing the index-window divisor from 32 to 8 with an 8 MiB cap gave about 8%
on a small run with no memory growth, but the 1/32 policy is validated on
Linux and needs full-corpus RSS before changing.  Reverted.

### Forced canonical preamble in every translation unit

A forced include of a generated standard-header preamble took 26 s to
preprocess, 116 s to parse, and 205 s total on the subset.  Rejected.

### One PCH unit per system header in a canonical batch

An earlier experiment lowering 359 canonical units at once failed with
`memory allocation of 27917287424 bytes failed` (27.9 GB).  Per-header units
were rejected for that experiment; the final design retains an aggregate
prelude. The allocation failure alone does not prove that every per-header
design is inherently unviable: a later, separate project-header experiment
produced the same allocation size because excluded functions' parameters were
still imported. See the follow-up diagnosis below.

### Per-translation-unit prelude insertion

Kept the prelude in each unit's selected headers.  It matched ordinary system
indexing more closely but cloned the prelude per unit: full tree 478 s / 2.7
GiB.  Replaced by the final merge.

### Suppress replayed system text in consuming units

Added a preprocessor option that truncates the output of a canonical system
header after processing it, preserving macro side effects.  On the full tree it
was still 535 s / 2.7 GiB; the output changed by a few dozen functions.  PCH
text was already only ~2.2 MiB, so this could not remove the dominant cost.

### Skip uncached canonical system expansion

Short-circuited any canonical system include to a cache lookup and emitted
nothing on a miss.  Discovery rose to 102.9 s, total 458 s, and output changed
(93,527 functions).  Rejected because it saved neither time nor the project
header work and weakened macro fidelity.

### Filter PCH down to discovered project files

The first canonical filter kept only `include_graph.project_files` in the PCH
set, dropping project-owned external headers.  The subset lost about 8,000
project call edges.  The final filter removes only compiler-default roots.

### Journal cap tuning

Using the normal variant cap in the canonical journal, instead of one, made no
visible difference on the subset and did not fix the full tree.  The
one-variant cap is now restricted to compiler-default paths; explicit system
roots and project headers retain the normal cap and fingerprint checks.

## Investigation findings and remaining work

The earlier whole-tree regression came mainly from project-header reuse and
lookup context, not the small amount of stored system text. A sampled fuzzer
TU shrank from about 391 KiB to 1.4 KiB in an intermediate lookup-projection
probe, and its second debug preprocessing pass fell from about 1.0 s to
0.16 s. This was diagnostic evidence, not a final release benchmark.

Specific findings from the follow-up:

- Project cache misses included real include-guard/macro differences. Ignoring
  those fingerprints changed project output and was rejected. The retained
  lazy standard closure stabilizes standard state while preserving project
  binding checks and the incoming cold/primed distinction.
- Restoring every standard header's guards, declaration identities and replay
  records into every consumer retained unnecessary dependency closures. Covered
  standard includes now use the late aggregate prelude instead. Unknown or
  macro-spelled standard includes continue through ordinary indexing.
- Treating every `using` as a consumer effect prevented class-header sharing.
  Dropping namespace imports entirely would change lookup order; the syntax
  projection preserves that order instead.
- Recursive callable/body imports consumed tens of GiB. Narrow declaration
  imports alone were insufficient until excluded parameters were dropped too.
  A diagnostic trap at one million variables identified the merge loop and
  the orphaned implicit `this` parameters. Disabling reusable preambles still
  failed, so snapshot count was not the cause. Temporary allocation traps,
  ancestry counters, cache-candidate logging and probe examples were removed.
- Ancestor lookup accounted for about 2.8 summed seconds across 2.26 million
  queries in a profile with hundreds of seconds of AST lowering. Replacing it
  was not justified by that profile; its eager profiling allocation was removed.
- Prototype narrowing must include implicit work. Default member initializer
  calls, inherited lifecycle candidates, static callback return bindings and
  external global receivers have dedicated regression coverage. File-static
  callback storage exposed a case requiring the inline fallback.
- Runs overlapping release builds or broad test compilation were useful for
  diagnosis but are not used to claim a paired end-to-end speedup. The current
  evaluation section distinguishes final runs, earlier stages and failures.

Further optimization is deliberately deferred. A borrowed immutable declaration
base with a mutable TU overlay would be a larger change if per-TU standard
typing must be retained. Another option is measuring preamble admission by
actual import cost. Neither is part of this implementation, and the flag stays
explicit.

### Original profile that motivated the follow-up

The full-tree regression is in project/external header processing, not in
system text:

- canonical PCH lowers 1,238-1,375 project/external headers in 67-104 s,
  against 2,524 headers in 10.1 s for master;
- `source-lower` is 501.8 s against 117.8 s;
- `merge-symbols` jumps from 345,784 to 619,267 calls;
- PCH system text is only ~2.2 MiB total.

The original investigation proposed the following directions. Project cache
stability and repeated imports are addressed above; the remaining architectural
ideas are not required for this change:

1. Make project-header cache hits stable under canonical mode.  The debug runs
   showed project headers as cacheable and apart, but canonical still lowered
   many project-header units with far more work than master.  Determine whether
   nested project headers are being held in parent entries or whether variant
   fingerprints differ between warm, discovery, and settle.
2. Avoid merging the prelude into the final program before all project dedup
   state is settled.  The current final merge is already late; further work
   should profile `merge-symbols` on the full tree after the project-header
   fix.
3. If per-unit standard typing must be retained, replace the clone with a
   shared immutable base program plus a copy-on-write overlay.  This is a
   larger refactor of `Program`/`SymbolTable`.
4. Do not enable canonical mode automatically for large trees until the full
   tree is at least competitive with master.  The CLI flag remains explicit
   and docs state the current limitation.

## Reproduction

Subset:

```bash
cargo build -p trace-cli --release
target/release/trace analyze \
  /path/to/ability_ability_runtime/services/abilitymgr \
  --system-includes --canonical-system-headers --jobs 8 \
  --timeout-secs 900 -o /tmp/abilitymgr-canonical.db
```

Full tree and master baseline:

```bash
# master without options in a clean worktree
git worktree add --detach /tmp/trace-master 2a51d50
CARGO_TARGET_DIR=/tmp/trace-master-target \
  cargo build --manifest-path /tmp/trace-master/Cargo.toml -p trace-cli --release

/tmp/trace-master-target/release/trace analyze \
  /path/to/ability_ability_runtime --jobs 8 --timeout-secs 3600 \
  -o /tmp/full-master.db

target/release/trace analyze /path/to/ability_ability_runtime \
  --system-includes --canonical-system-headers --jobs 8 \
  --timeout-secs 7200 -o /tmp/full-canonical.db
```

Normalized semantic comparison can be done with
`scripts/bench_system_includes.py` (`compare_semantics`), as used for the
154,019-row determinism check.

## File map

| Area | Location |
|---|---|
| CLI flag | `crates/trace-cli/src/main.rs` |
| Option/API | `crates/trace-preproc/src/options.rs` |
| Canonical cache and journal policy | `crates/trace-preproc/src/preprocessor.rs`, `crates/trace-preproc/src/journal.rs` |
| Consumer lookup projection and storage classification | `crates/trace-preproc/src/preprocessor/consumer_context.rs` |
| Preamble generation, lowering, PCH filter, final merge | `crates/trace-parse/src/lower.rs` |
| Declaration/parameter ownership and TU callback return replay | `crates/trace-parse/src/merge.rs` |
| Ordered include-graph membership updates | `crates/trace-parse/src/deps.rs` |
| Regression fixtures | `tests/fixtures/preproc/lookup_context/`, `tests/fixtures/preproc/member_imports/` |
| User docs | `README.md`, `docs/PREPROCESSOR.md`, `docs/EVAL_REPORT.md`, `CHANGELOG.md` |
