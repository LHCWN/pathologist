# Canonical system-header prelude

Status: experimental, opt-in with `--canonical-system-headers` (which implies
`--system-includes`). On the complete `ability_ability_runtime` tree it indexes
the host compiler's standard headers at about 3.5% over default-mode wall time
and the same peak memory, instead of 3.3 times the default time; the [evaluation report](EVAL_REPORT.md#canonical-prelude-at-default-mode-cost-2026-11-16)
holds the measurements. The [preprocessor contract](PREPROCESSOR.md#canonical-system-preamble)
is authoritative for the rules summarized here.

## Motivation

Bare-tree `--system-includes` asks the host compiler for its default search
paths and then indexes the reached standard-library headers. Those headers
are large and every translation unit reaches a similar closure, so the same
tokens are preprocessed, parsed, lowered and merged repeatedly. Ordinary
`--system-includes` takes 83-91 s on the 236-TU `services/abilitymgr` subset
against 7.6 s without it, and historically about 750 s and 6.5 GiB on the
complete tree.

The goal is the default mode's cost and project behavior, plus standard-library
macros and declarations: what the default mode would see if the standard
headers it cannot find were available once, in one configuration.

## Design

### Command line and API

- `trace analyze --canonical-system-headers` sets
  `PreprocessOptions::canonical_system_headers` (builder:
  `with_canonical_system_headers`) and turns on `--system-includes`. The
  library option discovers the compiler roots the same way unless the caller
  declared them with `with_compiler_system_include` and set no `compiler`.
- Only the bare-tree (`build_program_inner`) path builds a prelude. With a
  compilation database, shared command configuration, or link-target
  configured indexing the flag is ignored with a warning; each command has its
  own macro configuration and cannot share one prelude.

### Preamble discovery

Each language gets its own preamble. `canonical_prelude_sources` assigns every
discovered source and project header to the languages the indexer will
preprocess it as: translation units by extension, a header to every language
whose units reach it (C++ for C++ header spellings). A header the static
include graph does not reach may still be included through a macro by a unit
of either language, so it counts as C only in a tree without C++ units: a C++
prelude reading a C header is harmless, while a C prelude reading a C++
header would change every C unit. The C preamble also admits only `.h`
spellings, since C's standard and platform headers all use that extension and
a raw scan sees `#ifdef __cplusplus` arms too. A C prelude therefore never
reads a C++ library header, even when one root serves both languages.
`canonical_system_preamble` scans those files (raw source cache when
available, otherwise the file), collects the spellings of `#include <...>`,
deduplicates them, keeps the plain relative spellings that resolve inside the
language's compiler-default roots, and writes:

```c
#include <algorithm>
#include <memory>
...
```

to an exclusively created `.trace-canonical-std-<random>.tmp` file in the
system temporary directory. Indexing never writes into the analyzed tree, so a
run stopped by a signal or by the `--timeout-secs` watchdog cannot leave the
file there. Each indexing run owns a `NamedTempFile`, including cleanup on
failure; concurrent runs cannot truncate or remove each other's preambles,
and an existing file, symlink or hard link is never opened for writing or
removed. In the final program the scratch path has a stable synthetic
identity per language, `.trace-canonical-std-c.prelude` or
`.trace-canonical-std-cpp.prelude` under the language's first
compiler-default root, so random names never change exported file
identities, the two languages' preludes are distinct units, and the
identity has the dependency ownership of the headers it stands for. Each
distinct spelling is probed once through the shared directory-listing cache.

The scan reads raw lines, before preprocessing, so it over-approximates: an
include in an inactive `#if` arm, in a block comment or in a configuration the
tree never builds still enters the preamble. The prelude then lowers
declarations no unit includes, and the closure carries those headers' macros
and include guards into every unit, even one that never included them. A spelling such as
`<../project/own.h>` or an absolute path is never admitted: its first match
must stay inside the root it was found in, so a project header cannot become
a symbols-only prelude member. If the prelude run nevertheless reads a file
outside the compiler-default roots (a symlink out of a root, a relative
include climbing out), indexing falls back to ordinary `--system-includes`.

The roots are the language's compiler-default roots: the probe's search list
without explicit `-isystem`/`-idirafter` trees or directories the include
environment variables (`CPATH`, `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH`, …)
supplied (see `apply_probed_search`), followed by any
`compiler_system_include_paths` the caller supplied. Explicit and environment
roots are left out because a project can place aliases there. With
`--require-local-inputs`, the headers the prelude reads are validated like
any other input; the scratch preamble itself is the indexer's own file and is
not.

### One prelude run per language

For each language present in the tree, the preamble is preprocessed once,
inline (`inline_include_bodies`, no expansion cache, enlarged limits: 512 MiB
output, 200M expanded tokens, include depth 128, 256 expansions per file).
That run searches only the language's compiler-default roots, with no
project, explicit system, inferred or basename search and no forced includes,
so neither a project mock of a standard spelling nor an explicit system tree
enters the shared state. It produces three things:

1. **Declarations.** The output is lowered as one header `UnitIndex` and
   compacted (merge descriptors, header-merge type compaction, no flow or
   initializer ranges). Lowering runs on its own thread with the indexing
   stack size while the sequential warm pass proceeds; it needs only the
   dependency roots and path interning, so it uses a private graph instead of
   the growing project graph. The unit is merged into the final `Program`
   once, after project headers and translation units, so it never enlarges
   the dedup tables those merges consult. Its diagnostics (`#warning`s,
   missing nested includes) are imported with it, once: covered headers
   never run again, so the prelude is the only place they can report.
2. **Macro closure.** `PreprocessOptions::record_macro_ops` captures the
   directives the run executed, excluding the initial predefined, compiler
   and command-line definitions. `CanonicalSystemPrelude::new` keeps each
   name's last directive, in order.
3. **Covered headers.** The files the run read.

If the run hits a resource limit or fails, indexing warns and falls back to
ordinary `--system-includes` for the whole tree, rather than mixing a partial
prelude with per-consumer standard headers. Every limit (output size, token
budget, include depth, expansions per file) clears `PreprocessResult::complete`.
The run executes on its own thread with the indexing stack size, since its
include depth is twice the ordinary one and each level recurses: on the
caller's stack (1 MiB on a Windows main thread) the depth limit could
overflow before it falls back. Lowering finishes only after
indexing has started from the closure, so a lowering failure stops the
analysis with an error that suggests rerunning without the flag; a database
with standard macros but without their declarations would be neither mode.

### Every run starts from the closure

`PreprocessOptions::canonical_system_preludes` carries the per-language
preludes into indexing. Every preprocessing run of that language — warm,
discovery, settle, orphan headers and translation units — starts from the
closure, applied in the prelude run's own order: after the predefined,
compiler and command-line definitions (including compilation-command macro
operations) and the builtin fallback macros. A run seeded only by
predefined, compiler and `-D` definitions depends on nothing else, so its
starting table (with the fallback names) is built once per configuration
(`canonical_base_key`) and cloned; a run with compilation-command macros
applies the closure once its seed is complete: the C++ closure of `ability_ability_runtime`
has 7,835 bindings, and the warm pass alone has 2,451 sequential runs. The closure
is the run's baseline: it is not recorded in the deferred-publication history
or in any cache entry's operation log.

An `#include` that resolves to a covered header then contributes nothing to
the run: no text, no directives (they are already in the closure), no cache
entry, wherever it occurs — at file scope, inside `extern "C"`, or in the
middle of a project header. Its declarations are in the prelude. A standard
header that the prelude did not read, such as one reached only through a
macro-spelled include, is found through the same search paths and expanded
where it is included, under dependency ownership.

Indexing keeps `separate_system_headers` off. Standard headers therefore never
become expansions, system bundles, PCH units or parts of a translation unit's
text, and project headers are shared by exactly the default bare-tree rules.
The PCH set, header-unit planning, preamble reuse and expansion-cache variant
caps are the default ones.

### Why the closure is installed eagerly

An earlier design applied the closure lazily, at a unit's first standard
include at a declaration boundary, so project code before that include did not
see standard macros. That made each project-header cache entry depend on
whether its consumer was already "primed", required a cold/primed bit in macro
fingerprints and deferred-journal validation, and made entries that first
installed the closure carry its whole operation log for replay. Eager
installation gives every run of a configuration the same starting table, so
project-header fingerprints see stable bindings and none of that bookkeeping
is needed. The observable difference is confined to project code that tests
or redefines a standard macro before its first standard include; a real
compilation of such code would reach the standard header later and see its
definitions from then on.

## Precision

This mode is an explicit speed-for-precision trade relative to per-command
standard headers, not a more precise model:

- **One standard-library configuration.** Each standard header is
  preprocessed once, under the preamble's environment. Project macros that
  would change a standard-library `#if` arm, a typedef or a declaration set
  are not represented per unit, and `--explore` cannot vary them.
- **Standard macros from the first line.** See above.
- **No standard declarations while a unit is lowered.** The prelude joins the
  program after the translation units. Member typing that needs a standard
  type (for example `std::shared_ptr<T>::operator->`) follows the default
  mode's heuristics instead. Calls to standard functions resolve by name
  afterwards. The prelude holds each covered header's text, so a header's
  `static` functions belong to that header, and every file sees the covered
  headers (`SymbolTable::register_prelude_header`), as their macros reach every
  unit from the first line. A call
  to a header-static function therefore binds it in each unit; when another
  unit also defines an external function of the same name, the name lookup
  keeps both as may-candidates, where ordinary indexing binds the static one
  while lowering.
- **Project headers follow default sharing.** Using-declarations, file-static
  data and namespace context of a shared project header have the default
  mode's [shared-header approximation](ANALYSIS.md#shared-header-functions),
  not the ordinary `--system-includes` mode's inline fallbacks.
- **Configured indexing is unsupported.** The flag is ignored with
  compilation-database or link-target configuration.

Against default bare-tree indexing the mode only adds information: standard
macros expand (`assert`, `NULL`, integer limits, format macros), and calls to
standard functions bind to the prelude's declarations instead of external
placeholders. On the complete tree it keeps every distinct default-mode direct
call edge. The ordinary `--system-includes` mode is not a better reference for
the project call graph: on the `services/abilitymgr` subset it loses 3,121 of
the default mode's 8,506 distinct direct edges, mostly member calls through
`std::shared_ptr` whose standard-library typing it cannot follow.

## Investigation of the previous canonical design

The first complete design (`bcb4469` on `perf/canonical-system-prelude`)
took 517-524 s on the complete tree on Linux, 3.3 times the default mode's
155-157 s, with a 2.7 GiB peak against 1.7 GiB. It applied the closure lazily, kept per-variant canonical bundles in
the expansion cache with a one-variant journal cap, and replaced the inline
fallback for project headers with *consumer-context projections*: a project
header whose only consumer effects were using-declarations shared its IR and
replayed a syntax projection of those declarations at each include position.
Headers with file-static data, anonymous namespaces, weak pragmas or unclear
scopes stayed inline in every consumer.

Instrumenting each lowered unit's text and inline headers showed where the
time went:

- **Project headers stayed inline in most translation units.** Translation
  units held 541 MiB of preprocessed text in total. Headers such as
  `extension_ability_info.h`, `ability_info.h`, `configuration.h` and
  `hilog_tag_wrapper.h` were inline in 1,500-2,100 units each: the static-data
  fallback propagates to every cache frame that includes such a header, so
  one logging header with a `static` helper disqualified most of the tree.
  Summed `syntax-parse` and `ast-lower` were 401 s each, against 94 s and
  112 s for default indexing; `preamble-build` ran 3,247 times for 221 s,
  against 89 times for 3 s.
- **Standard headers were not the problem.** They were inline only 585 times
  across all units.
- **The sequential warm pass took 87 s**, against 6 s for default indexing.
- **Duplicate call sites.** A header both inline in a unit and imported
  through its preamble produced overload ties between two copies of the same
  function. The database had 97,310 overload-tie call sites against 6,719 for
  default indexing; every `TAG_LOG*` expansion produced two sites calling
  `GetTagInfoFromDomainId`. Most of that design's extra 94,000 direct call
  edges were these duplicates, not additional precision.

The redesign drops the projection, the static-data fallback, the lazy closure
and the canonical cache policy, and lets project headers follow the default
rules. The default mode already accepts their approximation; the canonical
mode's purpose is to add standard-library knowledge to it, not to change how
project headers are shared.

### Rejected or superseded in this redesign

- **Rebuilding the closure in every run.** Replaying its 7,835 bindings
  through the ordinary macro mutation path (journal records, hashing) made
  the sequential warm pass 12.5 s, and a bulk insertion 11.6 s, against 6.4 s
  for default indexing. Cloning one seeded table per configuration brings it
  to 8.3-9.2 s; the remainder is mostly the standard macros now expanding in
  project headers.
- **Lowering the prelude before the warm pass** added its 2.9 s to the
  critical path; it now overlaps the sequential warm pass. Lowering it with a
  private graph produced all 567,165 subset and 9,286,412 complete-tree
  analysis rows unchanged.
- **Reusable project-only preambles** (planned when canonical mode removed
  system units from preamble keys) are not used: with default sharing the
  default admission rule applies and preamble building costs what it does in
  default indexing.

## Compile-commands companion change

The include probe records compiler-default roots in
`PreprocessOptions::compiler_system_include_paths` (with
`with_compiler_system_include`), which the canonical prelude is built from, and
a bare driver name is resolved once per run. Two further parts of the first
version were withdrawn in review, so the rows below that measured them no
longer apply:

- Sharing compiler-default headers after project declarations and a
  project-header preamble in ordinary `--system-includes` indexing (the
  `own_first` row). On `services/abilitymgr` it changed 248 project
  signatures and lost 14 distinct direct call edges against master, with no
  speedup (88.9 s against 82.9 s on Linux). Ordinary indexing keeps master's
  gate for every system root again and exports master's rows exactly.
- Reusing a probe across build directories when the compiler printed only
  absolute paths (the 120/240-directory rows). A wrapper can pick an SDK by
  working directory and still print absolute paths, so every working directory
  keeps its own probe (see the
  [probe cache contract](PREPROCESSOR.md#compilation-database-options-62)).

Measured on release builds with one worker and synthetic projects:

| Case | Before | After | Result |
|---|---:|---:|---|
| `own_first`, 20 TU | 1.044 s | 0.206 s | settle 20 -> 1 TU; source-lower 0.428 -> 0.063 s |
| `direct`, 20 TU | 0.262 s | 0.209 s | unchanged output |
| `unique`, 20 TU | 0.516 s | 0.398 s | unchanged output |
| 120 command directories | 20.159 s | 0.564 s | all analysis tables and `database_identity` match |
| 240 command directories | 41.170 s | 0.739 s | all analysis tables and `database_identity` match |

Regressions: `compiler_default_system_headers_keep_the_declaration_gate`,
`each_command_directory_is_probed_for_its_own_answer` and the project-alias
refusal test.

## History

The sections below describe the previous canonical design and the experiments
behind it. Their measurements are Windows observations of that design, not of
the current one.

### Historical measurements before the reuse follow-up

All measurements below use Windows x64, MSYS2 MinGW-w64 GCC 15.2 headers and
`--jobs 8`.  Times are wall-clock observations, not repeated medians unless
stated.

#### `services/abilitymgr` (236 TUs)

| Mode | Wall | Peak RSS | Functions | Project direct edges |
|---|---:|---:|---:|---:|
| No system includes | 4.5 s | - | 6,837 (DB) | 26,449 (DB) |
| `--system-includes` | 90.8 s | 580 MiB | 17,149 | 18,428 |
| Canonical, per-TU prelude | 23.3-31.5 s | ~405 MiB | 14,873 | 18,403 |
| Canonical, final merge | 21.5-26.7 s | 297-315 MiB | 14,905 | 22,949 |

Canonical final-merge phases: preprocessing 1.5-2.2 s, PCH 2.1-3.0 s over 100
units, TU parse 5.4-8.5 s, index 19-24 s.  Two runs exported 154,019 identical
normalized semantic rows.

#### Complete `ability_ability_runtime` (3,456 TUs, 3,110 headers)

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

### Approaches tried and rejected before the redesign

#### Context relaxation for compiler-default roots

Allowing compiler-default standard headers to share after project declarations
reported 43-150 cache keys across configurations, reduced skipped
headers from 405 to 77, and parse to about 34 s (total about 59 s on the
subset).  It changed type spelling, so it was not kept as a general rule.  The
canonical mode now deliberately accepts a single configuration instead of
generalizing this relaxation to ordinary indexing.

#### Declaration-signature dedup of PCH units

Deduplicating 402 units to 329 by declaration signature/text saved about
1-1.5% and regressed types (`size_t` became `int` instead of `long`).  Rejected.

#### Larger variant budget plus context relaxation

`max_expansion_variants = 32` with context relaxation produced 1,407 units,
475 s, and 22 GB peak.  Rejected on both time and memory.

#### Smaller index window / byte budget

Changing the index-window divisor from 32 to 8 with an 8 MiB cap gave about 8%
on a small run with no memory growth, but the 1/32 policy is validated on
Linux and needs full-corpus RSS before changing.  Reverted.

#### Forced canonical preamble in every translation unit

A forced include of a generated standard-header preamble took 26 s to
preprocess, 116 s to parse, and 205 s total on the subset.  Rejected.

#### One PCH unit per system header in a canonical batch

An earlier experiment lowering 359 canonical units at once failed with
`memory allocation of 27917287424 bytes failed` (27.9 GB).  Per-header units
were rejected for that experiment; the final design retains an aggregate
prelude. The allocation failure alone does not prove that every per-header
design is inherently unviable: a later, separate project-header experiment
produced the same allocation size because excluded functions' parameters were
still imported. See the follow-up diagnosis below.

#### Per-translation-unit prelude insertion

Kept the prelude in each unit's selected headers.  It matched ordinary system
indexing more closely but cloned the prelude per unit: full tree 478 s / 2.7
GiB.  Replaced by the final merge.

#### Suppress replayed system text in consuming units

Added a preprocessor option that truncates the output of a canonical system
header after processing it, preserving macro side effects.  On the full tree it
was still 535 s / 2.7 GiB; the output changed by a few dozen functions.  PCH
text was already only ~2.2 MiB, so this could not remove the dominant cost.

#### Skip uncached canonical system expansion

Short-circuited any canonical system include to a cache lookup and emitted
nothing on a miss.  Discovery rose to 102.9 s, total 458 s, and output changed
(93,527 functions).  Rejected because it saved neither time nor the project
header work and weakened macro fidelity.

#### Filter PCH down to discovered project files

The first canonical filter kept only `include_graph.project_files` in the PCH
set, dropping project-owned external headers.  The subset lost about 8,000
project call edges.  The final filter removes only compiler-default roots.

#### Journal cap tuning

Using the normal variant cap in the canonical journal, instead of one, made no
visible difference on the subset and did not fix the full tree.  The
one-variant cap is now restricted to compiler-default paths; explicit system
roots and project headers retain the normal cap and fingerprint checks.

### Findings of the previous design

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

#### Original profile that motivated the follow-up

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

```bash
cargo build -p trace-cli --release
# default bare-tree indexing
target/release/trace analyze /path/to/ability_ability_runtime --jobs 8 \
  --timeout-secs 3600 -o /tmp/default.db
# canonical standard prelude
target/release/trace analyze /path/to/ability_ability_runtime \
  --canonical-system-headers --jobs 8 --timeout-secs 3600 -o /tmp/canonical.db
```

Run them sequentially and set `TMPDIR` to a volume with room for the
preprocessing spill. `TRACE_PROFILE_INDEX=1` prints the summed indexing
counters. Compare exact analysis rows with `compare_databases` from
`scripts/bench_system_includes.py` for worker-count determinism.

## File map

| Area | Location |
|---|---|
| CLI flag | `crates/trace-cli/src/main.rs` |
| Option/API, `CanonicalSystemPrelude` | `crates/trace-preproc/src/options.rs` |
| Closure seeding and covered-include skip | `crates/trace-preproc/src/preprocessor.rs` (`apply_canonical_prelude`, `canonical_base_key`, `process_file`) |
| Preamble generation, prelude run, lowering thread, final merge | `crates/trace-parse/src/lower.rs` |
| Regression tests | `canonical_prelude_seeds_every_run_and_skips_only_the_headers_it_read` (preprocessor), `canonical_system_tests` (`lower.rs`), `canonical_mode_lowers_project_headers_like_default_indexing` and `canonical_scratch_names_do_not_change_exported_analysis` (`system_header_units.rs`) |
| User docs | `README.md`, `docs/PREPROCESSOR.md`, `docs/EVAL_REPORT.md`, `CHANGELOG.md` |
