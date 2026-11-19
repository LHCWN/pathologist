# Program assignment graph and solver

PAG construction, the Andersen-style solver, field and array modeling, and interprocedural argument wiring. Part of the [analysis documentation](ANALYSIS.md).

## Program Assignment Graph (PAG)

### Node kinds

| `PagNodeKind` | Role |
|---------------|------|
| `Var(VarId)` | IR variable (local, param, global, synthetic temps) |
| `Loc(LocId)` | Abstract memory / function location |
| `CallTarget(CallSiteId)` | Synthetic node for indirect call resolution |

Every local, parameter and temporary gets a `Var` node. A global or static
gets its `Var` node and its location only when some flow constraint, return
flow or call site (callee variable, return destination or argument) names it
(`Pag::build_variables`). A header's declaration repeated into every
including unit, with no use in that unit, would otherwise be an isolated node
holding only its own address. Nodes are still created in variable order.
Anything that reaches such a global later, such as the by-name callee
fallback or an `&x` address, creates both on demand (`Pag::ensure_var_loc`).
Such globals get no `flow_nodes` row and no self points-to fact, and they
leave the minimal export's `variables` table, which lists what the flow
graph names; `--full-export` still lists every variable.

### PAG constraint kinds

| Kind | Semantics |
|------|-----------|
| `Copy` | `pts(dst) ⊇ pts(src)` |
| `AddrOf` | `pts(dst) ⊇ { loc }` |
| `Load` | for each `o ∈ pts(src)`: merge `memory_pts(o)` into `pts(dst)`; function locs copied directly |
| `Store` | for each `o ∈ pts(dst)`: merge `pts(src)` into `memory_pts(o)` and field summaries |
| `Gep` | field projection from base object locations (+ summary fallback) |
| `UnwrapPointer` | `pts(dst) ⊇ { o ∈ pts(src) \| o compatible with dst's pointee type }` (see [Smart-pointer unwrap](IR.md#smart-pointer-unwrap)) |

### Constraint representation and accessors

To reduce inline footprint during PAG analysis, `Constraint` stores field projection metadata in `field_info: Option<Box<FieldAccess>>` where `FieldAccess` holds `field: FieldId` and `field_name: Arc<str>`. This shrinks `Constraint` inline size from 48 B to 24 B (a 50% reduction).

- **Accessors**: Rather than accessing fields directly, consumers of `Constraint` use accessor methods:
  - `c.field() -> Option<FieldId>`: Returns the field ID for GEP constraints, or `None`.
  - `c.field_name() -> Option<&str>`: Returns the field name as a string slice, or `None`.
  - `c.field_name_arc() -> Option<Arc<str>>`: Returns a refcount clone of the field name `Arc<str>` without allocating string memory.
- **Migration**: Existing readers inspecting `c.field` or `c.field_name` must migrate to `c.field()` and `c.field_name()`.

### Post-solve flow-release lifecycle

Lowered IR flow constraints (`program.flow`) and function return flows (`program.fn_returns`) are consumed to construct the `Pag` (`Pag::build_flow_constraints`), during solver return-flow expansion, and to export source-level operation and return-call provenance. Source operation sites (`program.flow_origins`) retain constraint keys, origin vectors, and expression strings for that export. Calls retain their full source expression in `CallSiteDetails::call_expression`; return-assigning calls also retain their closest operation in `CallSiteDetails::return_operation`, before constraint deduplication. Complete expression text is shared as `Arc<str>` across origins, call expressions, and return operations; spans and occurrence records remain separate. The indexing-only `Program::source_expressions` pool expires in `release_merge_state` after the last merge, on the common CLI and C API indexing path. Origins and calls keep their references through export. CLI and C API retain all three collections until SQLite export succeeds, then call `program.release_flow()`:

- Drops all `FlowConstraint` entries in `program.flow` (reallocates to an empty vector).
- Drops all source provenance in `program.flow_origins`, including constraint keys, origin vectors, expression references, and map storage, and clears per-call expressions and return operation provenance. Also drops the expression pool for library callers that have not finalized merging. Shared text is freed when its last owner drops.
- Drops all `ReturnFlow` mappings in `program.fn_returns` (reallocates to an empty map).
- All symbol tables (`symbols`), call sites, functions, types, and the solved `Pag` (which retains its own compact `constraints` used by `export_flow_graph`) remain fully available for SQLite export, inspect queries, and C API callers. API consumers invoking analysis directly must retain these facts through provenance export; they may call `program.release_flow()` after their last consumer finishes. Releasing them immediately after solving is appropriate only when no provenance export follows. See [memory profile](MEMORY_PROFILE.md) and [evaluation report](EVAL_REPORT.md#struct-compaction-expansion-sharing-and-lifecycle-reclamation--2026-10-01).

### Abstract location kinds

| `LocKind` | Description |
|-----------|-------------|
| `Global` | External/global variable |
| `FileStatic` | File-scope `static` |
| `FnStatic` | Function-local `static` |
| `Local` | Parameter or stack local storage |
| `Heap` | Reserved for allocator summaries (stub) |
| `Field` | Specific field at a known parent object location |
| `FieldSummary` | Instance-insensitive merge of struct field `T.f` across instances in the same link target |
| `ArraySummary` | Unknown-index array element summary or modeled [container element cell](CXX.md#container-elements) |
| `Function` | Function entry address for indirect call targets |

### Lazy locations

**Global**, **file-scope `static`**, and **function-local `static`** variables receive `Loc` nodes eagerly at PAG build. Ordinary **locals** and **parameters** get locations **on demand** when referenced by `AddrOf`/`ensure_var_loc`.

## Solver

Worklist algorithm with **constraint adjacency index** (`SolverIndices`) for O(1) lookup of affected constraints per node.

### Work budget

Solving runs under a **deterministic pop budget** and optionally a **wall-clock budget**. The pop budget converts a divergent solve on a huge corpus into a partial result plus a visible warning instead of an hours-long hang; normal corpora converge far below it.

- **Derived default**: `800 000 + 6 × (PAG constraint count)` pops (`default_pops_budget`). The flat 800 000 floor is the historical default and covers the eval corpora (the largest, HDF, needs ~42k pops). The linear term lets mid-size and large trees finish their normal convergence instead of stopping under the flat cap at a partial result; the corpus measurements that sized it are in the [Solver work budget](EVAL_REPORT.md#solver-work-budget--2026-09-21-119) section of the evaluation report.
- **`--solve-budget-pops <N>`**: explicit budget; `N=0` means unlimited. Beats the derived default.
- **`TRACE_SOLVE_BUDGET_POPS=<n>`**: environment override (highest precedence; `=0` = unlimited); unparseable values fall back to the CLI/derived value. The eval harness pins this to `800_000` so eval corpora keep their exact exported databases.
- **`--solve-budget-secs <N>`**: optional wall-clock budget (`N=0` = no time limit). Checked every 10 000 pops, so a run can overshoot by the checkpoint interval plus the time of the one pop in flight; a run that converges in under 10 000 pops never checks the clock. A time cap is inherently **non-deterministic**: the stop point depends on machine load, so two runs of the same inputs may stop at different points.

**Partial results are recorded, never silent.** When a budget stops the solve before the worklist drains:

- the stderr progress line says the solve is partial, with the budget that stopped it;
- `analysis_run.options_json` gets `solver_partial: true`, `solver_pops`, `solve_budget_pops`, `solve_budget_secs`;
- an `analyze`-stage `warning` diagnostic (NULL `file_id`, line 0) is exported;
- `trace inspect` warns on opening such a database.

Points-to sets always grow monotonically, so a partial result is a monotone **prefix** of
the fixpoint: every recorded edge and flow is real, but flows that had not propagated yet
when the budget hit are absent. That cuts against may-analysis — a query asked "can this
pointer reach the sink?" can answer "no" when the true fixpoint would say yes, so do not
treat a truncated database as a sound may-answer. The truncation is always visible
(`solver_partial` + diagnostics), and the conservative response to a `partial` database is
to re-run with a higher budget rather than trust a "no".

### State

| Map | Role |
|-----|------|
| `pts` | PAG node → set of abstract locations |
| `memory_pts` | Object location → set of stored pointer values |
| `loc_nodes` | Reverse index: location → PAG nodes that must be requeued on store |

### Propagation highlights

**`Gep` with empty base points-to**

When `pts(base)` is empty (typical for pointer parameters with no incoming flow), fall back to **`FieldSummary`** for `(struct_type(base), field)` via `ensure_field_summary_for_var`. The summary is keyed by the struct's declaration identity (see "Field sensitivity"). This connects field stores through parameters to later field loads on unrelated instances (may-analysis). When link-target metadata is available, the summary also includes the target identity: separate link images cannot exchange field contents. Concrete field locations, heap locations, and nested summaries preserve that identity. Without target metadata the existing type-and-field summary cache is used.

The same fallback also fires when the base *has* pointees but none of them yielded a field cell — e.g. `void *` heap allocations or opaque summaries, where per-pointee `ensure_field_loc` synthesizes nothing. Without this, ops fields assigned through freshly-allocated objects starve every load site that reads them (observed as missing indirect-call edges for shared-obj style code).

**Stores to field summaries**

`apply_store` propagates into both concrete field locs and their `FieldSummary`, keeping summary memory in sync with instance stores.

A store is difference-propagated on both sides. A pointer that gains
targets writes the value's whole set into those new targets only. A value
that gains locations writes only those into every existing target, because
each target already holds the rest (`StoreSource::New`). The new locations
are taken in the value set's own iteration order, so each memory cell
receives them in the same order a whole-set write would insert them. The
cell iteration order that later loads follow is unchanged. Guards cannot
make the two writes differ: a slot guard is set once and never loosened,
and function arities are fixed before solving. A store a function
model adds mid-solve (`content_store`) never saw the values its sides
already held, and a side that has settled pops with an empty delta, which
fires nothing. Wiring therefore fires it once with both whole sets
(`apply_fn_model`); from then on it is difference-propagated like any
other store.

When an indirect callee resolves during solving, the new return constraints
replay the points-to sets their sources already hold. This includes settled
values captured by a lambda and uses the same worklist as later growth.

**Signature-guarded function-value propagation**

Wrong-type pointer casts put unrelated objects into a pointer's points-to; a store through such a pointer would otherwise write callback addresses into alien layouts, where later field loads surface them as bogus indirect-call targets. The solver therefore filters **function values only** (all non-function flow stays unfiltered, preserving soundness):

- A fn value may enter `memory_pts[cell]` / a summary cell only when the cell's declared type accepts it (`slot_guard_for`): `FnPtr` slots — a fn-pointer field (`FnPtr`) or variable (`Ptr(FnPtr)`) — with a recorded fixed signature require the same known parameter count.
  An empty parameter list is a known zero-argument signature and requires
  arity zero; a function with genuinely unknown arity remains admissible.
  Static members use their explicit parameter count, excluding any lowered
  receiver slot, when matching an ordinary callable slot.
  C++ callable declarators record their parameters; C callable descriptors
  remain coarse, even when their source spells a parameter list, and therefore
  impose no fixed-arity guard. A leading class-pointer parameter in an ordinary callback does not exempt the
  slot. A trailing `Unknown` parameter denotes an open variadic/pack signature,
  so it imposes no fixed-arity storage guard, including through pointer and
  array slots. Field-access filtering uses the same fixed/open arity policy:
  open callable fields admit every arity, while non-callable fields remain
  excluded from function-value projection. Explicit `MemberPointer` descriptors
  retain their values through field-cell loads/stores and check their signature
  at invocation; they do not use the GEP shortcut for ordinary function locations. Reinterpreting casts carry
  function values through separate abstract locations with the same `FnId`
  and link target, exempting only those values from storage arity checks;
  invoking such a value checks the cast's callable signature. Restoring the
  original signature therefore recovers a round trip through differently typed
  storage without weakening ordinary function locations' guards. Callable casts
  in array initializers use the same value lowering before copying into the
  array summary. Casts inside aggregate array elements use typed field stores;
  publishing an ordinary `ArrayFnMember` would lose reinterpretation before a
  restoring cast could read the element. Uncast function-address tables retain
  their existing per-array summary. An array takes its element's guard, a multi-dimensional one its leaf element's: a table of fn pointers keeps their arity, a table of untyped pointers (`void *ops[]`) or of scalars (`long v[4]`) stays writable as its element does, and an array of structs or scalar pointers rejects fn values; other concrete non-fn-pointer cells (`struct`, scalar-pointer, union) reject all fn values; unknown/untyped cells, `void *` included, stay writable (a dlsym-style `GetSymbol(name, void **out)` stores a function there).
- The same guard applies when `merge_memory_into` lifts cell contents into points-to sets — to **function values only**, exactly as on the store path: a load through a `NotFnPtr` cell (declared pointer, `struct`, `union`, an array of those) returns every non-function value stored there. (Before #127 the load applied the guard to every value, so such cells yielded nothing — e.g. `viaload = *pp` with `int **pp` lost `&g`.) The guard also applies when a `Gep` passes fn values from the base node's set into the field node — except registered `array_fn_members` table members, which always pass (see "Arrays and function-pointer tables").

Consequence: callbacks stored through correctly-typed ops assignments resolve exactly as before, while cross-signature leaks (e.g. a 2-param `AddService` callback surfacing at 4-param `Dispatch` sites) are cut. Documented imprecision: old-style casts that stash fn pointers in `void *`-typed cells then call them through typed loads still work (unknown cells accept everything), but calls through cells whose declared type is structurally wrong for the stored fn are no longer reported.

**Variable cells: a variable's value and its memory cell are one object**

A variable whose address is taken (`&v`, or `return &v`) has two representations in the solver: its PAG node (`pts[Var(v)]`, read and written by `Copy` — `x = v`, `v = y`) and its memory cell (`memory_pts[loc(v)]`, read by `Load` and written by `Store` through any pointer to `v`). The solver keeps them in step, in both directions, for every pointer-like variable (`var_is_pointer_like`: `is_pointer`, or a declared `Ptr`/`FnPtr` type, never an array) whose cell is the source of an `AddrOf` constraint. It is deliberately wider than `var_may_hold_pointee`, which parameter wiring uses and which leaves out `int *` / `void *`: an out-parameter of buffer type must still reach its direct reads.

- **Registration.** The first time a cell's address is taken (`seed_addr_of`, at solve start or when `expand_return_flows` adds `return &v` mid-solve), the cell gets its slot guard if it has none, and `register_synced_cell` records `node ↔ cell` and syncs what each side already holds — the address may be taken long after the variable got its value.
- **Node → cell.** When `pts[Var(v)]` gains locations, they are written into `memory_pts[loc(v)]`, filtered by the cell's slot guard exactly like a store (`FnFilter::admits`). The cell's loaders are requeued so pending loads see the growth.
- **Cell → node.** When a store grows `memory_pts[loc(v)]`, the cell is merged into `pts[Var(v)]` through the load path (`merge_memory_into_if_grown`, same guard).

Address-taken arrays of pointers or callbacks also synchronize their
index-insensitive element summary with their memory cell. Their own storage
location is excluded from node-to-cell synchronization: array decay still
names storage, while the summary's contents are element values. This lets
`&array[i]` carry initialized callbacks and lets a store through that address
reach subsequent element reads. The element slot's existing signature guard
continues to filter function values; struct arrays retain field-cell modeling.

Only a real address may reach a synced cell, so a name that denotes a *value* never stands for its own address. One rule, `seeds_own_location` / `var_is_pointer_like`, decides both places the solver used to assume it did:

- **The static-storage seed.** `solve` starts a global/static's node holding its own location only when its name denotes its storage (an aggregate, an array). A pointer static's name denotes only its value: seeding it would make `p = G; *p = &k;` write `k` into `G`'s cell, and syncing would turn that into `G = &k`. (The flow graph's `points_to` edges are a different thing — storage connectivity, drawn for every variable with a location; see `docs/SQLITE_SCHEMA.md`.)
- **The store's `self_loc`.** A store `*p = v` also writes `v`'s own location only when `v`'s name denotes its storage (`*p = arr`, an array decaying). For a pointer `v` it stores the value alone; `*out = &G` lowers to an explicit `AddrOfVar` temp (see "Argument flow"), so the address is a real, address-taken one.

Consequences: `get_buf(&q); x = q;` sees what `get_buf` stored (the out-parameter idiom), `v = &h; x = *(&v);` sees `h`, and `cb_t cb; get_cb(&cb); cb();` resolves the callback `get_cb` stored with `*out = handler`, `*out = &handler`, `*out = (cb_t)handler` or a qualified `*out = &ns::handler`. A function designator stored through a pointer lowers like a field store of one (`AddrOfFn` temp + `Store`; `fn_designator` for both paths), and a function-pointer variable's own cell gets the fn-pointer slot guard (see "Signature-guarded function-value propagation"). Aggregate variables (`struct`/`union` and non-pointer array objects) are not pointer-like and are not synced: their contents live in field cells and summaries; a table's elements are its cell, so `table[i]()` lowers as a load from `table` (which also passes through the functions its initializer placed), and a field array's elements are its field's cell: `s.ops[i] = fn` stores into the field, and every read of an element — a call `s.ops[i]()` / `p->ops[i]()`, an initializer or assignment `f = s.ops[i]`, an argument, or the source of `t.a[i] = s.b[j]` — loads from it (`field_table`, which also peels `s.m[i][j]`; `matrix[i][j]()` likewise loads from `matrix`).

Lazily created *field* cells get no slot guard: their recorded type can be another same-named field's (HDF's `UartDriverData` has a fn-pointer `config` and a nested `params.config` struct), and guarding them by it dropped real edges.

Before #127 the two representations were disjoint, and both idioms lost all flow. The pointer seed had also hidden a missed field-summary join, keyed by declaration since (see "Field sensitivity"); what that cost on the corpora and how it was recovered is in [EVAL_REPORT.md](EVAL_REPORT.md), "Pointer cells and `&x` arguments".

**Propagation without per-step allocation**

Copy propagation checks cast-specific destinations only when the PAG contains
reinterpretation nodes. That path reads copy indices in place and reuses
`Scratch::reinterpret_values`, filled in the source delta's order. The ordinary
copy path does not scan destinations for casts when the set is empty.

Every propagation step used to allocate. A fresh vector of newly-added
locations per propagate/merge call; a clone of a location's entire holder set
on every store; and — by far the largest — one signature-filtered copy of a
store's source points-to set per *target location it writes*. A profile of the
analyze phase on the biggest corpus attributed more time to the allocator than
to the set logic itself.

Three changes remove that overhead without changing a single propagated fact:

- Propagation steps take the buffer they need (`Scratch`) as an argument
  instead of allocating one, so a step that used to allocate now refills a
  buffer that already has capacity. The buffers live beside the solver state
  rather than in it, which is what lets the borrow checker reject two steps
  sharing one.
- A store filters its source set at most once per distinct slot guard
  (`StoreViews`), not once per target: which locations a guard admits depends
  only on the guard, and a source set carrying no function values at all needs
  no filtering for any target.
- Within one store, a shared `FieldSummary` cell is written once. Many of a
  store's targets are field cells of the same struct type and field, and they
  all write that one summary; since cell memory only grows, every write of it
  after the first inserts nothing. The `SUMMARY_MEM_CAP` check those writes
  would still perform is kept, and so is the requeue they would still trigger.
  A location is likewise requeued once per store — repeats are no-ops that
  still re-walk its holder set.

All three are order-preserving by construction: the buffers are filled in the
same sequence as the vectors they replace, filtering only ever drops elements,
and the skipped writes are provably empty. Memory cells, delta vectors and the
worklist therefore see the identical sequence of operations and the exported
database is byte-identical. Measurements are in the
[evaluation report](EVAL_REPORT.md#analyze-phase-performance--2026-09-21-117).

What is *not* done here: propagating only a store's newly-gained source
locations instead of its whole source set, and bitsets as the *storage* of
points-to sets, which would fix their iteration order by bit position. Both
would cut more, and both change the order locations enter cell memory, which
changes the partial result the default pop budget stops at. That is a
deliberate trade-off to make explicitly, not a free win. Bit mirrors that
only answer membership, with the sets kept as the storage, are done (below).

**Membership mirrors and per-load merges**

Hub sets make most of the solver's set operations hits: a load merges each
location a cell gained into a destination that usually holds it already, and
a store re-inserts its whole source into cells that usually hold it already.
The solver answers those without hashing, and still without changing the
order of anything:

- A points-to set or cell memory of at least `MIRROR_MIN` locations gets a
  bit mirror (`LocBits`). Bits index a dense numbering of just the
  locations mirrored sets hold, far fewer than all locations. On a large
  tree that numbering still runs to about a hundred thousand locations while
  a hub set holds a few hundred, so a mirror (`Mirror`) keeps only the
  512-number blocks its set reaches, found through a per-mirror directory,
  rather than one bit per number. The hash sets stay the storage and the
  iteration order. A mirror only ever says "held": a bit is set once its set
  holds the location, sets never shrink, and a clear bit falls back to the
  set. Every insert therefore happens as before, in the same order.
- A load steps over its source's new locations in one call per destination
  (`load_into`) rather than one per location. The destination's set, mirror
  and merged sizes are each looked up once. Only the destination's own
  indexing, delta and requeue are deferred to the end of the step, and no
  merge reads them, so they see the same sequence as before.
- A holder's load-source test is a bit per node (`SolverIndices::is_load_src`).
  Each location caches the filtered holders in the full holder set's own
  iteration order. Every successful holder insertion invalidates the cache,
  including non-load holders: a set resize can reorder the existing loaders.
  Growth of the load-source index also invalidates it. Empty results are
  cached, and the scratch-copy and requeue order remain unchanged.

On HDF these recover what the larger hub of #150/#151 costs; the exported
database, the `points_to` debug rows included, is byte-identical. Measurements
are in the [evaluation report](EVAL_REPORT.md#dereferenced-operands-and-template-parameter-bases--2026-09-25-151-150).

**String-literal memory cell exclusion and precise store requeuing**

String literals (`LocKind::StringLit`) interned for dynamic lookup (`dlsym`) represent immutable constants, never writable memory cells:
- `apply_store_to_targets` skips `LocKind::StringLit` targets, preventing stores through generic/cast pointer buffers or strings from treating constant string literals as memory cells.
- `load_into` skips `LocKind::StringLit` locations during cell merges, preventing dereferences of pointers holding string literals from attempting to read memory cells.
- In `apply_store_to_targets`, requeuing of instance field locations and field summaries is decoupled (`loc_changed` vs `summary_changed`). When an instance field grows without growing the summary (e.g. because the summary already holds the locations or hit `SUMMARY_MEM_CAP`), the summary's holders are not re-walked. Conversely, when only the summary grows, the instance field is not needlessly requeued.

Without this, ubiquitous string constants (such as `""` held by tens of thousands of variables across translation units) accumulated abstract locations, turning into massive false memory hubs whose stores re-triggered hundreds of millions of redundant loads across the entire program. Measurements are in the [evaluation report](EVAL_REPORT.md#solver-string-literal-memory-cell-exclusion-and-store-requeue-decoupling--2026-09-28).

**Indirect calls**

1. Each indirect call site gets a `CallTarget` node.
2. For field-path callees (`p->ops->fn`), lowering emits `Load`/`Copy` chain into a temp var; PAG connects `CallTarget` via `Copy` or `Load`.
3. When `pts(CallTarget)` gains a `Function` location, emit `CallGraphEdge` (resolution `indirect`), wire parameter `Copy` constraints, call `apply_call_summary`.

**Direct calls**

Direct-call consumers use `Program::callees_of`, which delegates to the
symbol table's target-aware resolver. It uses the lowering binding when
available and otherwise resolves by name in the calling TU. A file-`static`
definition shadows a same-name external definition in that scope. Internal
declarations also participate, so an initializer can bind a callback before
its definition is lowered. Header identity, visibility, and coalesced lookup
contexts follow [Shared header functions](ANALYSIS.md#shared-header-functions).

**Cross-TU direct-call recovery**

A plain call whose callee symbol is not visible in its TU is lowered with
`is_direct = false`. `CallSite::resolves_by_name` identifies recoverable calls
with no `callee_var` and a bare callee name. The same `Program::callees_of`
entry point resolves these through target-scoped candidate lookup. Lowering
also registers pointer-returning prototypes (e.g. `T *f(void);`) as functions,
so those declarations participate in lookup instead of creating phantom variables.

<a id="sound-handling-for-unresolved-member-field-calls"></a>
**Sound handling for unresolved member field calls.** Only bare identifiers
and qualified names may fall back to direct-call resolution or cross-TU name
recovery. A member field call (`obj->fn()` or `obj.fn()`) or array subscript
call (`arr[i]()`) whose field decomposition or receiver typing cannot be
resolved represents an unresolved indirect call (`is_direct = false`). It must
never fall back to matching an unrelated free function named `fn` in scope. AST
lowering retains the field path (`field_callee_text`, e.g. `"obj->fn"`), which
also prevents `finalize_extern_callees` from synthesizing bogus external
function symbols for field names. When an identifier matches a local variable
or parameter in the enclosing member method, local/parameter lookup takes
precedence over implicit member resolution, ensuring that shadowing callbacks
resolve their indirect call targets.

### Analyze options

```rust
pub struct AnalyzeOptions {
    pub retain_points_to: bool,  // CLI: --debug-points-to
    pub enable_ipc: bool,        // CLI: enabled unless --no-ipc
    // function models and solve budget omitted
}
```

When `retain_points_to` is false (default), points-to sets are discarded after solving to reduce memory.

## Field sensitivity

- Struct fields have distinct `FieldId` entries in `TypeTable`.
- `GepField` in IR becomes PAG `Gep` with field id.
- **`FieldSummary`** locations unify all instances of `struct T.field` for sound may-analysis (e.g. vtable writes through a parameter pointer visible at unrelated call sites).
- Unknown or non-struct base → GEP may no-op.
- **Struct identity is per-TypeDesc**: types intern by full `(tag, fields)` equality, so layouts of divergent copies diverge, and lowering must produce *identical* descs for the same logical struct in every TU. In particular, a typedef'd anonymous struct (`typedef struct { .. } Alias;`) takes `Alias` as its tag — per-unit `anon_N` counters would otherwise split one shared-header type into several TypeIds after merge.
- **Field summaries are keyed by declaration, not by desc**: nested aggregates are recorded structurally, so one declaration can still intern under several ids that differ only in how completely a nested member's tag was known — `IDriverLoader` reached through `HdfDriverLoader.super` vs named directly. `TypeTable::tag_identity` (from a descriptor: `tag_declaration`) maps a named, non-anonymous struct/union to its tag's richest id; the pointer lookups (`struct_type_for_loc`, `struct_type_from_type_id`) and `ensure_field_summary_loc` all use it, the latter remapping the field **by name** into that declaration, so a store through one snapshot meets a load through the other (#127 review: HDF's `driverLoader->GetDriver` / `->ReclaimDriver`), including a snapshot that lacks a member another configuration compiled in. A member the declaration's richest layout lacks keeps the snapshot's own summary; an anonymous aggregate (`anon_N`, a per-unit counter) is its own identity. The mapping is memoized per `(type, field)` (`Pag::declared_member`).
- **A location's struct type** (`struct_type_for_loc`) is the type its field positions refer to: a variable's own snapshot, or the tag's definition when the variable was declared with a forward-declared (empty) tag, as lowering numbers such fields (`struct_type_for_var`); through pointers and arrays, the pointee or element (an array element snapshot that was never separately interned falls back to its named tag declaration; an interned snapshot keeps its own positions); for a non-variable location (heap, field, table element) the same through its recorded type. A scalar has none. Variable-based summary lookup uses the same type resolver, including forward-declared tags, so it selects the same member as concrete field lookup.
- **The GEP field-name guard** rejects a pointee whose struct type lacks the GEP's named field at that position, and a pointee with no struct type at all, so a wrong-type pointer cannot pull fields out of an unrelated layout. Function values are not judged by it: a table's initializer places its functions directly in the table's points-to (`ArrayFnMember`), and they reach `table[i].func` through the table-member and arity rules instead (HDF's local `dispatchFunc[i].func(..)` tables).

Overlapping union views preserve storage identity without initializing inactive
members. Same-shape union members share a concrete field cell. For distinct
struct members of a union, fields share cells only within a prefix whose
declared types, offsets and sizes agree; names may differ. The first incompatible
field ends that prefix. Struct-view roots retain their own declared types so
the ordinary GEP name guard still applies. Canonical cells follow declaration
order, independently of which view is accessed first, and retain their original
link target. Export records each canonical field location once. A field's root
variable identifies its storage owner; further member lookup uses the field's
own declared type.

A recognized C++ pointer cast from known aggregate storage to its first
member's pointer type projects that member's cell when its offset is zero and
its type shape matches. For example, `reinterpret_cast<F *>(table)` for
`struct S { F first; F second; }; S table[1];` reads `first` through the cast,
without joining `second`. The cached projection precedes value-cast
normalization in initializers, returns, arguments and dereference/callee
consumers; these positions share the same member address. This is an
aggregate-to-first-member rule, not an
arbitrary cast or byte-offset alias model; see
[Conditional value limitations](IR.md#conditional-value-limitations).

## Arrays and function-pointer tables

- **Constant index**: treated conservatively (element refinement is future work).
- **Unknown subscript**: `ArraySummary` — all elements merged.
- **`ArrayFnMember`**: each initializer function is merged into the array var's points-to; any subscript call may target **any** listed function.
- **Nested initializer lists** (`{ {TYPE, Fn}, ... }`): element expressions are visited recursively, so arrays of structs with fn-ptr members feed `ArrayFnMember` facts into the table var. Element fn values flow through field loads on the array itself *and* through pointers to elements (`m = &arr[i]; m->fn()`), regardless of worklist order.
- **Field-designated members** (`[i] = { .fn = Fn }`): lowered as precise
  `GepField`+`Store` chains against the array var (index-insensitive, like
  runtime element stores), so a member only feeds loads of the field it was
  written to. Purely positional nested lists still use the merged
  `ArrayFnMember` blob. Mixed forms where positional and designated members
  coexist in one element list keep the designated precision; bare positional
  members of such lists are not separately parked (rare; sound direction).
- **Initializer-less array declarations** (tentative definitions such as
  `static struct Ops g_tbl[4];`) register the variable like any other global;
  runtime stores into elements then resolve normally.
- **Positional struct initializers** (`static struct Ops o = { Fn, ... };`):
  each bare value is mapped to its declared field by position and lowered as
  the same precise `GepField`+`Store` chain designated members use — function
  addresses included. Position counting treats designated and bare members
  uniformly (C's reset-after-designator subtlety is not modeled; rare).

## Member subobject addressing

`&outer.member` lowers to a gep-temp chain targeting the member's own abstract
location, typed by the member's declared struct — not to a flattened address of
the outer instance. Field loads through such pointers resolve fields against
the member's type (`dev->service = &inst.service; ... service->Dispatch`
resolves `Dispatch`, not same-index members of the outer struct). Arrays of
structs peel to their element type for field resolution (`arr[i].field`).

## Implicit this member variable access

This section is the authoritative description of when a bare name in a C++
member body designates an instance field of `this`. In an instance method or
other member body, an unqualified identifier naming an instance field is a
member access on `this`: `member_ = val;` is `this->member_ = val;`,
`m_in->pp` is `this->m_in->pp`, and `*m_pp` is `*this->m_pp`.

One rule (`implicit_this` in `lower.rs`) decides it, in the order C++ lookup
asks: a local, a parameter, or a `using ns::name;` in the body hides the
field; the field, when the member's class or a base declares it, hides a
namespace or global variable and a function of the same name. These
positions ask it — values, stores, `&member_`, calls (`cb_()`), returns,
the receiver of a member call (`plugin_.OnEvent()`), an argument of a direct
initialization (`T g(mu_);`), and the root of a field path or dereference
(including `f(*m_p)`) — and lower the field as the
one-field path `this->member_` (`field_path`), so one mechanism emits the
`GepField` in each of them. A bare field passed as a call argument
(`f(m_p)`) does not; see Limits below. A call through a
function pointer reached from one (`handler_->fn()`, `val_handler_.fn()`,
`nested_->handler->fn()`) is therefore a load through
`this->handler_->fn`, an indirect call with a `callee_var`, not a direct
call to an external `fn` (#146). A call through a
field whose path does not resolve stays unresolved, as the field's value
does; it never falls back to a function the field hides.

- **The field's class**: `class_ctx_field` searches the member's class, then
  its bases breadth first (`program.bases_of`). A function or static data
  member of the name declared on an intermediate class stops the search, as
  C++ name hiding does. The member's own class is its type even where
  `this`'s pointer type does not tag it as one (a union).
- **Inherited fields**: a field a base declares, at the root of any field
  path (`this->base_field_`, `d->base_field_`, `*d->get()`), is read through
  a summary receiver of the declaring class (`_recv`), so the access reaches
  the base's instance-insensitive field summary.
- **Member tables**: an element of a member array (`table_[i]`, as
  `this->table_[i]`) is the table's cell index-insensitively, as `s.table[i]`
  is (`field_table`): stores, reads, returns and calls through it
  (`table_[i]()`) go through that cell. Array types peel to their element in
  `peel_ptr_to_struct`, so `table_[i].val` resolves `val` on the element.
- **`this`**: the explicit `this` is its own value in values, returns and
  lvalues (`ctx.locals["this"]`); a lambda's captured `this` is the body's.

Limits:

- A bare instance field passed as a call argument (`f(m_p)` in a member body)
  passes the field's cell (`GEP(this, m_p)`), not its loaded value as
  `f(this->m_p)` does, and a global of the same name takes precedence there
  (`collect_call_args` asks `resolve_expr_var` before `addr_of_field_path`)
  (follow-up).

What a read, store or argument then does with the member's value is described
in [Dereferenced operands](IR.md#dereferenced-operands).

## Indirect call resolution patterns

Supported lowering patterns include:

| Pattern | Example |
|---------|---------|
| Direct fn ptr var | `fp()` |
| Single field | `obj.handler()` |
| Multi-hop field | `p->ops->setIpAddr()` |
| Mixed `.` / `->` | `subDev.subDevOps->setConfig()` |
| Designated init | `.handler = &Fn` |

### External callees

Plain-identifier calls that resolve to no definition under the analyzed root
are classified as `external`, not left as unresolved indirect sites. Two
sources feed this class: prototype-only declarations (the callee resolves
statically but has no body here), and synthesized entries for names that are
never declared in the tree at all (libc without tree headers, logging
backends referenced only inside macros — `finalize_extern_callees`). Edges to
bodyless functions never carry param wiring unless the prototype declares
formals; unresolved fn-pointer sites (`ptr_expr` shapes) remain the only
occupants of the "no target" indirect bucket.
| Static ops struct | `g_ops = { .fn = Fn }` + `memcpy`-style assign via `SbufInterfaceAssign` (field store from global init) |
| Call return | `p->field = Getter()` |

## Argument flow

When a call edge is created (direct or indirect), actuals are connected to callee formals:

- **Pointer variables** → PAG `Copy` from actual var node to formal var node, persistent for pointers whose pointees can hold callbacks (`var_may_hold_pointee`: fn pointers, pointers to fn-pointer slots such as `cb_t *out` or a table row, and pointers to aggregates or unknown types). Callback-pointer chains retain this persistent edge at every depth, including reference-return addresses that arrive through copies after initial parameter wiring.
- **Opaque pointer arguments** (`void *`) use the same persistent copies for
  actual/formal pairs whose pointees can carry callbacks: function-pointer slots,
  aggregates, unknown or erased types. A known scalar-buffer pointee on either
  side (`char *`, `int *`, other sized values) retains the existing snapshot
  behavior instead of adding a persistent copy. Both function values and addresses of their
  storage slots must propagate when they arrive after initial call wiring:
  `void *p = (void *)&cb; put(p)` must let `put` write into `cb` just as passing
  the cast directly does. Member-pointer slot pointees (`M *out`) use these
  copies too. The usual memory-cell guards still apply to subsequent stores.
- **Function identifiers** passed as fn-ptr args → `add_pts(formal, fn_loc)`
- **Parameter packs** (`Args &&...args`, `args...`): a pack formal takes every actual from its position on, and a pack expansion serves every formal from its position on ([Variadic parameter packs](#variadic-parameter-packs))
- **Dereferenced arguments** (`f(*x)`) pass what [Dereferenced operands](IR.md#dereferenced-operands) gives the parameter kind
- **Address of a plain variable** (`f(&x)`, through parentheses and casts — C `(T)` and C++ `static_cast` / `reinterpret_cast` / `const_cast` / `dynamic_cast`, one peeling rule, `peel_casts`, at every value entry point: stored values, assignments and initializers, return values, arguments and callback arguments) → lowering materializes a temp with `AddrOfVar { dst: temp, src: x }` (`addr_of_temp`) and records the temp as the actual, so the formal points to `x` (the out-parameter idiom: `get_buf(&q)` gives `out -> q`). Before #127 the actual was `x` itself, which handed the callee `x`'s *pointees* instead of its address. The same temp carries a stored `&x` (`*p = &x`, `.f = &x`). `&base.member` preserves the member’s own field address using the same GEP path as a stored address; unresolved paths and `&arr[i]` keep their base-variable handling (see [Documented imprecision](MODELS.md#documented-imprecision) under function models; a stored `(void *)&s.f` is the member's field address, and a cast `memcpy((void *)&s.f, ..)` argument follows the same field path), and so does `&r` for any C++ reference binding `r`, `auto &r` included (`reference_bindings`): the reference already holds its referent's address, so `&r` — as an argument, a stored value, `p = &r` or `return &r` — is `r`'s value (`names_reference_binding`). A qualified name (`&ns::var`, `&(ns::var)`) takes the same path once it resolves (see [Canonical variable identity](ANALYSIS.md#canonical-variable-identity)), a qualified reference binding included. The position is recorded in `CallSite::addr_of_args`: consumers that report the argument's *object* rather than its value — arg-flow rows (`actual_var`), `clears` terminators, and the `alias` / `mem_copy` models (`model_copy_side`) — name `x` through `Pag::argument_var` / `Pag::addressed_var`, so `memcpy_s(&dst, .., &src, ..)` still copies between the objects.

After fixpoint, `extract_arg_flow` records:

```
(call_site, arg_index, actual_var?, actual_fn?, formal_var)
```

Exactly one of `actual_var` or `actual_fn` is set per row. Only arguments that resolve to IR variables or function refs at the call site participate. `arg_index` is the callee's parameter position: for a C++ member function position 0 is the implicit `this`, and the first explicit argument is at 1.

Return-value flow affects **points-to** (what a call expression assigns), not arg-flow formals.

### Variadic parameter packs

Debug builds check that pack positions are sorted, unique and within the
parameter list before explicit-type lookup or argument wiring. Wiring also
checks that each recorded pack slot names a lowered pack variable.
During cross-TU registration, explicit-type lookup uses the declaration's
arity and pack count to account for the implicit receiver before the incoming
parameter IDs have been remapped.

A parameter pack (`Args &&...args`, `Args... args`) takes any number of
arguments, and a pack expansion (`args...`, `std::forward<Args>(args)...`)
passes any number. Neither is a position, so the analysis treats a pack as
**one variable that stands for every element** and wires it both ways. This
section is the authoritative statement of the rule; lowering
(`lower_parameters`, `collect_call_arg_nodes`, `mark_pack_actuals`,
`trace-parse/src/lower.rs`), the solver (`wire_actuals`, `recorded_position`,
`trace-analysis/src/solver.rs`), tests (`tests/fixtures/cpp_variadic_packs`,
`cpp_bind_packaged_task`) and reports link here. No new flow constraint kind
is added: the wiring is the ordinary actual-to-formal copy of
[Argument flow](#argument-flow).

- **The pack formal.** A C++ pack is lowered as a formal of its own, in
  declaration order, named without its `...`; C's `...` names nothing and is
  not a formal. The pack variable is in `Program::pack_vars`.
  `Function::pack_positions` records every explicit pack position (excluding `this`),
  and `explicit_arity`, in-class prototypes' counts and the
  explicit parameter types do **not** count packs: a call may pass none
  (`takes_argument_count`).
- **A pack formal takes every argument from its position on.** At a call, the
  pack formal at position `p` receives the actuals at `p`, `p + 1`, and every
  later position the site passes, as an ordinary persistent copy for each
  (`wire_actuals`, direct, indirect and `invoke` callbacks alike). A pack
  variable always counts as able to hold a pointee (`var_may_hold_pointee`),
  whatever its dependent element type lowered to (`Args &&...args` is an
  `int *`), so the copy does not depend on the order the sets grow in.
  Every declared pack receives that variadic wiring, including packs before
  another pack. Earlier packs may all be empty, so each formal's first
  possible actual position omits every preceding pack's synthetic slot.
- **Parameters after a pack.** A following fixed parameter has no fixed
  actual position: it may immediately follow an empty pack or move right by
  any number of elements. It receives every recorded actual from that first
  possible position on. Parameters before the pack retain their fixed
  bindings. Declaration signatures skip every pack wherever it occurs;
  argument-type filtering and ranking use only the known prefix before the first pack,
  retaining uncertain suffix matches. Member receivers remain at position zero.
- **An expansion serves every formal from its position on.** An argument that
  is a pack expansion records the value of its pattern at its position
  (`std::forward<Args>(args)` is the same value as `args`) and marks it a pack
  variable. A formal at that position or any later one takes it when nothing
  is written at its own position (`recorded_position`: a variable or a function
  written there is never shadowed), so `Target(first, rest...)` gives formal 0
  the `first` and every later formal the pack. A formal *before* the expansion
  never takes it. An argument written after an expansion starts at no known
  position (it is the expansion's count further on), so it is lowered like any
  other argument, nested calls included, and recorded at the expansion's
  position too, as a pack variable: it may reach any formal from there on.
- **Arity.** An expansion may be empty. At an `invoke` site
  ([Callback invocation](MODELS.md#callback-invocation-invoke)) only the positions before
  it count as passed, so a callable that takes nothing is still the one a
  `std::bind(f, args...)` runs when `args` is empty.
- **Arg-flow rows.** A row is the position the actual is recorded at and the
  formal it reached, so a pack formal has one row per later position that
  passes a variable or a function (a literal records none), and a formal
  reached through an expansion has the expansion's position. Rows are ordered
  as the other forwarded pairs are, by `(formal, position)`. When arguments
  follow an expansion, its position holds several values and the solver wires
  all of them; the row names the first.
- **Imprecision (may-analysis).** The pack is instance-insensitive: every
  element reaches every formal the pack is forwarded to, whatever its
  position within the pack, so `f(a, b)` through `g(Args... args) {
  h(args...); }` gives `h`'s first formal `b` as well as `a`. The same holds
  when the callable is itself in a pack (`std::thread(std::forward<Args>(args)...)`:
  every element may be the callable). Elements are not typed, so a scalar
  and a callback meet in one variable. A pack expanded inside a lambda's
  parameter list (`[](auto &&...args)`) is not lowered as a formal. A
  `sizeof...(args)` or a fold expression reads nothing.
- **Scope of the pack marking.** `Program::pack_vars` is global, not per call
  site: an argument written after an expansion that names a variable (`f(args...,
  local)`) marks that variable, so another call that passes it where no
  argument is written for a later formal (`g(local)` to a two-formal `g`) also
  gives it to that formal, and it always counts as able to hold a pointee. This
  only adds flows, so it over-approximates; it is not tracked per site.
  A dereferenced argument after an expansion (`*x`) is recorded at the
  expansion's position, so its by-reference or by-value treatment is read from
  that formal rather than its own.
- **Soundness and determinism.** Wiring only adds copies and points-to
  members, so the solver stays monotonic and flow-insensitive; the positions a
  pack formal takes are visited in increasing order, so the export is
  deterministic.
- **Cost.** A function that is not a pack function pays one flag test per
  formal; the rest is the cost of the extra copies, which on Camera is within
  the run-to-run variation of the analyze phase (`docs/EVAL_REPORT.md`).

### Flow-graph export

`export_flow_graph` (`trace-db/src/export.rs`) serializes the post-solve PAG
as `flow_nodes` / `flow_edges` for the `inspect dataflow` command:

- Constraint kinds map 1:1 to edge kinds `copy` / `addr_of` / `load` /
  `store` / `gep`.
- `points_to` edges are derived from the final var→location map.
- `call_arg` edges come from `extract_arg_flow` and are exported only when
  no stronger constraint already connects the actual/formal pair — this
  covers scalar (non-pointer) arguments that the solver does not persist as
  PAG constraints.
- The memory cells each load and store reaches are a table of their own,
  `flow_memory_access`; see [Memory access edges](#memory-access-edges).

#### Where a value moves

`flow_edges` rows carry no position. Where the statement behind an edge is
written is exported once, in `flow_origins`, keyed by the edge's
`(src_node, dst_node, kind)`; this section's rules apply to it, and the
recording, merging and text rules are in
[Source-level dataflow presentation](USAGE.md#source-level-dataflow-presentation).

- **One row per source site.** The same endpoint pair written at several
  statements keeps a row per statement, including two statements of one
  shared header body, a statement only an `--explore` variant compiles, and
  each element of a function-pointer table initializer. The copies of one
  header statement that several units contribute merge into one row.
- **Original-file position.** Sites follow the LineMap rule (AGENTS.md
  invariant 1): inside a macro expansion, the outermost invocation; inside a
  header body, the header. The operations reading a returned value
  (`return cb_;` loads the member) are at the value, as an initializer's
  are, and a `new` expression's allocation (the `addr_of` from its heap
  object) is at the expression. A field or element a call's argument reads
  or takes the address of (`sink(a->p, b->p)`, `take(&s.f)`) is at the
  argument, with the argument's text as its expression, not the call's: a
  position on one argument then names only that argument's access
  ([Value slice](USAGE.md#value-slice-inspect-slice)). Other arguments are the
  call's: its site and text.
- **Enclosing function.** It follows from the site by the edge-scope rule
  of the source-level view: the innermost defined function in the queried
  relation whose `[line_start, line_end]` in the site's file holds the line.
  When several definitions hold the line, the innermost one (every other one
  holding it encloses its range) owns it if the line is strictly inside it,
  not its first or last line: a statement on its own line in a local class's
  method is that definition's. A line held by a single definition is that
  definition's, its first and last lines included. Otherwise the site has
  none: outside every function (a file-scope initializer), and with no single
  innermost definition: a one-line method of a local class, the line such a
  method opens or closes on (the enclosing function's statement may share
  it), or two definitions written on one line (a macro that defines two
  functions). Lines are all the export records of a definition's extent, so
  these stay unresolved rather than guessed. The export adds no function
  column; the lookup uses `idx_functions_file_range`. Query:
  [Source sites of a value move](SQLITE_SCHEMA.md#source-sites-of-a-value-move).

  The public `functions` relation holds no lambda, so a lambda is not a
  definition there. A statement on its own line in a lambda body belongs to
  the visible function the lambda is written in, and so does a statement in a
  one-line lambda. A statement in a file-scope lambda has no enclosing
  function. `raw_functions` also holds the lambdas, so in the
  complete graph they count as definitions: a statement on its own line in a
  lambda body is that lambda's, and a one-line lambda or the line a lambda
  opens or closes on has none.
- **Lowered operations only.** Edges with no statement of their own have no
  rows: `points_to`, `call_arg`, `terminates`, `dlsym`, and parameter copies
  the solver wires at a resolved call. Calls are located by `call_sites`
  (`flow_calls`, `flow_return_calls`).
- **Memory accesses.** A memory access has no rows of its own: its sites
  are those of the `load` or `store` it was read off
  ([Memory access edges](#memory-access-edges)). A load or store without rows
  (a model's `content_store`) leaves its accesses without any.
- **Deterministic.** Rows are written in first-occurrence order of the merged
  constraints, so they are identical for every `--jobs` (AGENTS.md
  invariant 10).

#### Memory access edges

The constraint edges say how a pointer is used (`load q → x` is `x = *q`,
`store v → p` is `*p = v`), not which memory it reaches, so the graph leaves
a store and the load that reads the stored value unconnected. Once the solver
has converged, before the points-to sets are released, `memory_accesses`
(`trace-analysis/src/memory.rs`) lists for every `Load` and `Store`
constraint the cells its pointer may point to
(`AnalysisResult::memory_accesses`), or that it has none recorded. The export
writes them to `flow_memory_access`, additive to the v7 contract
([schema](SQLITE_SCHEMA.md#flow_memory_access)): per access, the `flow_edges`
id of its load or store and each cell, or one row without a cell when none is
recorded. `flow_edges` keeps only constraint edges, and the access's sites are
its load's or store's `flow_origins` rows
([Where a value moves](#where-a-value-moves)), not repeated for it. Read as
edges, they are `mem_write` from the stored value to each cell and `mem_read`
from each cell to the load's destination: the kinds a slice shows.

- **Which cells.** Every location in the pointer's points-to set except
  function and string-literal locations: a load through a function pointer
  yields the function itself, not memory. The pointer a field access yields
  (a `gep` destination) also holds the contents of the cells it designates,
  so that later uses of the field lvalue see stores recorded against the
  cell. Its accesses are instead the cells the `gep` designates, which the
  solver records as its `gep` step points the destination at them: the
  field's cell in each object the base points to, the field's summary, and
  the summary the solver falls back to for the base variable's declared type
  (its positional member, which need not carry the field's name; see "Field
  sensitivity"). A field holding another field's address (`s->p = &o->x`) is
  therefore not an access of `o->x` when `s->p` is read or written. A store
  into a field cell also writes the field's summary, as the solver does. A
  field cell lives in its parent's storage, so a local's field cell carries
  the local's function (`flow_nodes.fn_id`).
- **Fan-out cap.** An access keeps at most 16 cells (`MEMORY_ACCESS_CAP`).
  Past that, each instance cell is replaced by its field's summary, which
  every store into the cell also writes; an access still wider keeps no
  cell: it is recorded without one, as is an access whose pointer points at
  no memory, and a slice reports such a load's value as an `unrecorded_load`
  source. Counting stops as soon as the cells folding cannot remove pass the
  cap. Such a pointer's points-to
  set is too coarse to say which memory it touches (generic service and
  buffer pointers), and a slice through it would join unrelated objects.
- **A call through a pointer field reads it.** `cb_->OnError()`,
  `this->cb_->OnError()` and `obj->cb_->OnError()` read the pointer the
  field holds, and so does an explicit member operator through it
  (`cb_->operator()()`, `cb_->operator->()`), as `auto *cb = cb_; cb->OnError();` does. Lowering loads the
  field into a temporary at the call (`read_field_receiver`, `lower.rs`), so
  the read is a `load` and a memory read at the call's position: every such
  call reads it, each at its own position, so a slice can start at any of
  them. A receiver path lowering cannot resolve reads nothing and leaves no
  temporary. The temporary is not bound to the callee's `this`: a member
  call's receiver stays identity (`CallSite::receiver`), not value flow, so
  the call graph and every other points-to set are unchanged. A call through
  a variable (`p->f()`) or a dot call on a field object (`list_.size()`)
  reads nothing new.
- **Who follows them.** [`inspect slice`](USAGE.md#value-slice-inspect-slice) does.
  `inspect dataflow`, `dataflow_graph` and the C API's `trace_db_dataflow`
  read `flow_edges` and `flow_origins`, which hold none.
- **Deterministic.** The accesses are listed in constraint order, then by
  cell, and written sorted by load or store, then cell.

Measured cost: [Value slice: #205](EVAL_REPORT.md#value-slice-205--2026-10-09).
