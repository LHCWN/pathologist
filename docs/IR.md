# Intermediate representation (IR)

How lowering records values, returns, conditions, casts and smart-pointer unwraps as IR facts. Part of the [analysis documentation](ANALYSIS.md).

## Type storage

Member-function pointers carry optional const/ref signature metadata. Parsed
declarations record it explicitly; missing metadata in coarse or legacy types
remains conservative. `FnPtr` parameter lists use a trailing `Ellipsis` for a
declared variadic signature and `Unknown` for unresolved types or coarse open
signatures. A dependent `Unknown` parameter does not hide later known parameters.

Type tables share immutable `Arc<TypeDesc>` descriptors and typedef alias
payloads across simultaneously live headers and translation units. Interning
order, IDs, layouts, and tag-completion state remain local to each table;
aggregate union replaces a shared descriptor rather than modifying it.
Sharing requires full structural equality, including collision checks, and
weak pool entries do not retain unused descriptor payloads. The public Rust
`TypeInfo.desc` field is an `Arc`; match through `.as_ref()`. Serialized type
values and exported data are unchanged. See [memory measurements](MEMORY_PROFILE.md)
and [evaluation report](EVAL_REPORT.md#struct-compaction-expansion-sharing-and-lifecycle-reclamation--2026-10-01).

### Call-record storage and merge lifetime

`CallSite` keeps common facts directly on the record. Function-pointer
arguments, member/plain address markers, and an explicit macro occurrence
live together in `Option<Box<CallSiteDetails>>`; empty ordinary calls need no
payload allocation. Rust callers construct this field with
`CallSiteDetails::boxed(fn_args, addr_of_member_args, addr_of_args, occurrence)`
or `None`, read the argument slices through `fn_args()`,
`addr_of_member_args()` and `addr_of_args()`, and add rare facts through
`details_mut()`. `occurrence()` still returns the effective identity, including
its original fallback for calls without explicit macro metadata. Cloning
records copies their argument vectors and shares immutable callee text.

Callee spellings use `CallName`, an immutable `Arc<str>` wrapper interned by
full text within each `SymbolTable`. `intern_call_name` shares repeated names
when lowering and merging. Rust literal construction uses `.into()`, textual
reads retain `as_str()` and string methods, and `to_string()` supplies an
owned `String` when required. Name lookup, symbol identity and SQLite strings
are unchanged. The pool is local to the symbol table, and dropping its lookup
after the final call is created leaves call records owning the shared text.

Merge source lookup uses a borrowed `CallSourceKey` and a fingerprint index
referring to typed call IDs. Fingerprints only group candidates: every hit
compares the full occurrence, callee spelling and merge-time callee binding.
Source coordinates and names remain immutable while this index is retained;
the binding is separately captured because finalization can resolve an unknown
external callee. A stable source representative keys variant facts even when
an ordinary merge replaces the canonical record. Singleton fingerprint buckets
keep their first entry inline and allocate only for hash collisions.

`build_program_with_jobs` retains merge state for callers adding further units.
The CLI and C API use `build_program_for_analysis_with_jobs`, which releases
merge-only indexes after the last merge and before the shared finalization
steps. `Program::release_merge_state` also drops the call-name interning lookup;
finalization may rebuild it, so the terminal builder clears it again after the
last call is created. Call records retain ownership of their shared names.
Reference-return category metadata remains available through PAG construction
and solving, where callable signature checks restore the reference layer. It
is released with flow facts by `Program::release_flow` after their last consumer.
With explicit images, idle indexing workers return freed lexer/AST
pages before scoped copies are built on glibc. Consumed linked-unit vectors
drop immediately after image merging,
before orphan indexing and finalization. Finalized semantic facts and IDs
follow the same path. Overload-site
indexing stably sorts borrowed call references by the existing group identity;
it preserves table order within each group, including repeated sibling targets.
On glibc, the ordered indexing window returns unused pages after every 64
consumed payloads, once the merge callback has returned and before the next
window slot is admitted; buffered units and the growing Program stay live.
See [evaluation measurements](EVAL_REPORT.md) for whole-process peak results.

### TypeFields and layout storage

`TypeLayout.fields` is wrapped in `TypeFields` rather than a bare `IndexMap<FieldId, FieldLayout>`.

- **Migration**: Callers constructing a `TypeLayout` with an existing `IndexMap` pass `fields: some_map.into()`. Existing access via indexing (`layout.fields[&fid]`) or borrowing iteration (`for (id, fl) in &layout.fields`) continues to work through `Deref` / `DerefMut` and `IntoIterator`. By-value iteration (`for (id, fl) in layout.fields`) is supported via `IntoIterator for TypeFields`.
- **Lazy allocation**: Internally, `TypeFields(Option<Box<IndexMap<FieldId, FieldLayout>>>)` stores `None` for empty layouts, shrinking `TypeLayout` from 72 B to 8 B on 64-bit platforms. Aggregate layouts allocate the boxed map lazily on mutable dereference or when constructed from a non-empty `IndexMap`. Calling `TypeFields::clear()` drops the allocated box back to `None`. Inherent `is_empty(&self)` and `len(&self)` methods, as well as fast-path `PartialEq`, bypass static map access when either operand is `None`.
- **Compaction**: When `TypeTable::compact_for_header_merge()` runs on cached header units, unused layout fields are dropped via `layout.fields.clear()`, releasing heap-allocated field maps before serialization or merge caching.
- **Serialization contract**: Wire formats and SQLite export schemas are unchanged. `TypeFields` implements `Serialize` and `Deserialize` transparently as the underlying map of fields. See [memory measurements](MEMORY_PROFILE.md) and [evaluation report](EVAL_REPORT.md#struct-compaction-expansion-sharing-and-lifecycle-reclamation--2026-10-01).

Sharing also makes re-interning cheap: a table answers `intern_arc` for an
allocation it already holds by address, without hashing the descriptor tree,
and a header unit carries the descriptor each of its types merges as
(`UnitIndex::merge_descs`), computed once rather than by every consumer.

After an unconfigured header unit prepares `merge_descs`, its type table is
retained only for merging: it drops interning, pointer, canonicalization and
tag indexes, plus field layouts. Consumers use the prepared descriptors, type
IDs, aliases and class declaration sets; they must not intern into the
compacted table or read its layouts. Header flow-ownership maps are also
discarded on this path. Ordinary TU and configured-build tables remain mutable.

For mutable tables, a descriptor that tag completion may rewrite
(`Ptr(Struct { name, fields: [] })`) is answered from a small cache keyed by
that spelling and validated against the tag's current id, its descriptor
allocation and the aggregate-union count, so the answer changes exactly when
the layout it names does. Internal-linkage
function and file-static lookups are indexed by name and filtered by the
asking file's scope, rather than walking every header of the scope per name.
See the [Clang benchmark](PERFORMANCE_REVIEW.md#clang-source-benchmark-header-import-and-lowering).

Repeated metadata imports check borrowed constructor signatures, constant names,
template-return keys and member-initializer keys before allocating copies.
They retain the same conflict handling, fact order and constructor-cache
invalidation. A nested-tag completion pass inspects shared descriptors by
reference and memoizes each original field type's answer for that pass. The
pass can append pointer types but cannot declare or widen a named tag, so its
answers remain valid until the pass ends. First-use order still determines
new IDs; later declarations and aggregate unions start a fresh pass.

**Declarator shapes.** C declarators bind inside-out: in `T D`, a declarator `*D1` gives `D1` the type "pointer to T", `D1[n]` "array of T" and `D1(..)` "function returning T", and the identifier gets what is left after the last layer (`walk_declarator_shape`). So `int *t[4]` is `Array(Ptr(Int))`, `void (*h[4])(int)` is `Array(Ptr(FnPtr))`, `int *(*get)(void)` is `Ptr(FnPtr { ret: Ptr(Int) })`, and a fn-pointer variable `void (*fp)(int)` is `Ptr(FnPtr)`. Before #127 the walker wrapped outside-in: `int *t[4]` was `Ptr(Array(Int))` (exported `int[]*`), `int *(*get)(void)` was `Ptr(Ptr(FnPtr { ret: Int }))` (exported `fn_ptr**`), and `void (*h[4])(int)` was `Ptr(FnPtr { ret: Array(Void) })` — exported `fn_ptr*`, because an exported type name does not spell a function's return type.

## IR flow constraints (`trace-ir`)

Lowered from C during parse. Mapped to PAG in `Pag::build_flow_constraints`.

| Constraint | Meaning | C example |
|------------|---------|-----------|
| `Copy { dst, src }` | pointer assignment | `p = q` |
| `AddrOfVar { dst, src }` | address of variable | `p = &x` |
| `AddrOfFn { dst, callee }` | address of function | `p = handler` (fn ptr) |
| `Load { dst, src }` | load through pointer | `y = *p`; `y = *h->f` is `Load(y, Load(GEP(h, f)))` (see [Dereferenced operands](#dereferenced-operands)) |
| `Store { dst, src }` | store through pointer | `*p = y`, `field = val` |
| `GepField { dst, base, field }` | field address | `&obj.field`, `p->field` |
| `ArrayFnMember { array, callee }` | fn-ptr array init member | `{ fn0, fn1 }` |
| `CallReturn { dst, callee_name, caller, occurrence }` | `dst = callee()` written in `caller` (`None` for a file-scope initializer). Allocation calls in a body record the source occurrence for binding to the selected call site's targets. | `p = GetOps()`; `void *g = GetOps();` |
| `CallReturnResolved { dst, callee }` | return from an exact selected function | selected free arrow-star overload or post-merge file-scope getter return |
| `CallReturnIndirect { dst, callee_var }` | `dst = *callee_var()` | `sbuf->impl->readBuffer(...)` (indirect return) |
| `NewHeap { dst }` | heap allocation | `new T(...)` (C++ ctor result) |
| `StringConst { dst, value }` | `dst` points at a string literal | `p = "target"`; `dlsym(h, "target")` |
| `NullPointer { dst }` | explicit null pointer value with no points-to target | C++ `nullptr`, including a call argument |
| `UnwrapPointer { dst, src }` | cross a smart-pointer wrapper into its pointee (see [Smart-pointer unwrap](#smart-pointer-unwrap)) | C++ `sp->field`, `(*sp).field`, `sp.get()` |

`NullPointer` copies from an interned immutable PAG value node into `dst`.
That source is not an abstract location and always has an empty points-to set:
no `AddrOf`, object storage, or function target is seeded. Ordinary argument
collection materializes the value in a temporary and retains its syntactic
position; existing parameter wiring and occurrence export handle it like other
values. A null argument therefore has a visible value-flow connection without
adding pointees or indirect-call candidates. This does not kill other possible
values in a context-insensitive formal or introduce path/context sensitivity.
Integer zero/`NULL` spellings and omitted defaults are not materialized by this
explicit C++ `nullptr` rule.

### Return-value flow

When only a callee name is available, file-scope direct reference-getter
addresses use `CallReferenceReturn { dst, callee_name }`. PAG construction
resolves this fact after merge with the
same destination-file/link-target lookup as a file-scope `CallReturn`, and
expands only `LvalueAddress` return facts. It needs no synthetic call site.

Functions record abstract return values in `program.fn_returns`:

| `ReturnFlow` | Source |
|--------------|--------|
| `AddrOfVar { src }` | `return &global` / `return &file_static` |
| `AddrOfFn { callee }` | `return &Fn` / `return Fn` |
| `Copy { src }` | `return local` or `return param` |
| `Call { callee_name, result }` | `return Other()` (transitive; resolved in the returning function's contexts). `result` is the returned call's own temporary for call-site field models. |
| `LvalueAddress(inner)` | Address of the lvalue returned by a reference-returning definition; `inner` uses the address/copy/call forms above |

`CallReferenceReturnDeclared { dst, callee, args }` preserves a selected free
or member declaration for a file-scope address consumer without inventing a
call site.
After merge, the shared initializer resolver matches the declaration's
signature in the destination's file and link target before applying TU-local
definition precedence. Only the selected definitions' `LvalueAddress` facts
expand into `dst`; declaration hints do not bypass image or overload selection.
The selected-declaration fact also retains explicit actual argument variables.
After choosing definitions, PAG construction copies these values to the matching
explicit parameters (past `this` for members) before expanding address returns.
Reference parameters retain the actual lvalue's original cell address, so
stores through the getter's returned reference reach that cell too.
Free reference getters use the same signature-preserving path. No file-scope
call site or synthetic caller is introduced.
Default argument expressions retain their declaration scope and source mappings
as function metadata through header imports and merge. Deferred defaults and
member initializers capture the original preprocessed spelling at the parser's
preserved byte offsets, so reparsing retains member-pointer operators and other
syntax adapted for the grammar. A file-scope reference
getter instantiates only omitted defaults for the selected declaration, then
binds their values or original reference-cell addresses alongside explicit
actuals. Explicit arguments suppress the corresponding defaults.

`LvalueAddress` facts are separate from ordinary return values. Taking the
address of a reference-returning call requests these facts through the same
post-merge, image-aware callee selection, including virtual and indirect calls.
Named returns identify their original cells; a dereferenced return preserves
its pointer operand, and a returned reference call forwards the address facts.
Value consumers continue to use the ordinary return facts. A call can retain
both destinations when a reference result is needed in both forms.
Forwarding a reference-returning call keeps an address destination on that
actual call site and records a `Copy` of it inside `LvalueAddress`. The call's
selected overload, parameter bindings and image remain authoritative after
merge; the address fact does not repeat a name-only lookup. Value returns
from that forwarding expression read the same original cell (class objects
keep their address for member projection). Repeated address readers reuse
the recorded destination.
When viable overloads have mixed value/reference return categories, the value
destination also receives ordinary return facts from every selected candidate.
Only reference candidates contribute address facts; possible value-returning
alternatives are not discarded because another candidate returns a reference.
Reference-return classification follows the complete declarator to the
function's own return binding, including pointer-reference returns such as
`F *&get()`, and resolves trailing reference aliases (`auto get() -> R`). A
pointer to a function returning a reference remains a pointer return.
One declaration's return category and return cv qualification share a single
lookup of its return type. The lookup is skipped when no reference alias exists
in the type table or an enclosing block, since then no spelling can name one;
the same holds for callable aliases that return references.
Ordinary consumers of a returned pointer reference still receive the loaded
pointer value; its original cell address is a separate `LvalueAddress` fact.
Callable aliases also retain whether their function returns a reference,
separately from the alias's own binding and lowered pointer shape. This
metadata survives header/type merges and local alias scopes, including merges
where the destination already holds the alias name. The independent
callable-reference flag is unioned across units; a unit without the flag does
not clear an earlier positive fact. Indirect calls
through such declarations establish lvalues for `decltype(auto)` wrappers;
a pointer-returning callable with the same lowered shape remains a value.

Selected ordinary calls and constructor/member sites bind omitted arguments
through the same declaration-scoped expression lowering as file-scope getters.
The written argument count is retained during lowering even when a scalar
argument produces no pointer-flow variable. Explicit arguments suppress their
defaults. Each selected declaration supplies only its applicable omitted
positions; member positions are shifted past `this` exactly once. Default
expressions retain their declaration source mapping and the executing caller;
their nested calls and side effects are lowered before syntax state is retired.
Lvalue reference defaults keep their original cells. A value expression bound
to a reference, such as `const F &f = make()`, is instead lowered into a value
temporary and passes that temporary's address; it does not require the
value-returning getter to supply reference-address facts. This preserves value
flow without simulating temporary destruction or general implicit conversions.
Default metadata uses explicit source parameter positions for both prototypes
and definitions. Binding translates them through the actual IR receiver
convention; a static member definition can still have a synthetic `this` slot.
Class-member prototypes retain expressions without allocating parameter variables.
This does not change prototype signature interning: where no parameter type
is recorded, the temporary can remain unknown while known default values flow.
Virtual dispatch uses defaults from the statically visible declaration, rather
than the override's defaults, and does not reject a matching override for
omitting those defaults. Within a caller, repeated uses of one declaration's
omitted position share its value and nested source call. Merge keys
keep separate nested call records for different executing callers, even when
the default is spelled at one shared-header location. Evaluation ownership
follows the caller while exported request spans retain the declaration source;
project calls can therefore evaluate defaults declared under a dependency root
without enabling dependency function bodies. Transient requests
retain the visible declaration and written count, and are consumed before
publishing the per-TU index; no new SQLite column is introduced.
Block function declarations keep their own default sets even when symbol
interning reuses an outer function ID. Calls retain the set visible at that
source position; leaving a block restores the outer set, and an explicitly
qualified outer call uses that declaration. Retained expressions capture local
alias descriptors plus cv, lvalue/rvalue-reference and callable-return flags.
Nested default calls use the declaration's saved block-default environment. These temporary
bindings are consumed during lowering, without exporting local symbol IDs.
Defaults are not evaluated merely because their block declaration is indexed.
Later prototypes also preserve the definition's authoritative parameter storage.
The retained default also records its evaluated call sites. Loop evidence is
unioned from every invoking host, including a later loop use of a cached value
and nested default hosts. Calls inside a separate lambda body do not inherit
the creating host's loop solely through that capture/default expression.
Default expansion obeys the AST depth bound. Unknown callees and declarations
without a retained default expression supply no invented argument value.
This binds pointer/callback values and evaluated effects; it does not instantiate
dependent templates. Default calls expanded after CFG construction currently
lack their own argument-order events. With sequencing enabled, their host has
an explicit `unsupported_construct` gap and partial coverage, rather than
claiming complete ordering. See [Call sequencing](USAGE.md#call-sequencing-control-flow-sidecar).
It is not a complete C++ default-argument evaluator: dependent types/templates
and arbitrary local value environments are not reconstructed. Name lookup and
overload selection retain their existing conservative boundaries. If
overload ranking retains several candidates, each retained candidate can
supply a default even when the compiler would select just one; such extra
flows are precision debt. Conflicting same-qualified class declarations from
different headers can also retain extra constructor candidates through the
existing merged lookup. Name-only file-scope value-return summaries without
a call site do not acquire ordinary call-site defaults through this pass.
An `auto` callable variable also retains the initializer's known return
binding, including named casts and function addresses. Cast targets establish
their own return binding. Built-in callable array subscripts retain the
element's binding; class `operator[]` inputs do not establish that category.
Uninitialized declarations carry the same callable-return metadata as initialized
ones. Validated pointer offsets preserve that binding, and subscript probes read
pointer-table cast targets before checking the built-in element category.
Selectors, comma/assignment callees and typed callable fields preserve that
return category. Field category metadata follows its declaring class through
header/type merging, independently of the field's pointer descriptor.
Callable class objects use the selected `operator()` declarations from the
shared call-result/overload lookup for return-category deduction, including
compound object callees. A lambda's explicit reference-return declaration
also establishes its initialized variable's callable binding. Thus a
`decltype(auto)` wrapper retains the reference address supplied by either
kind of callable; this does not add a general lambda/template deduction engine.

Prototype classification reads the enclosing declaration's aliased return
type as well as the full declarator. A wrapper lowered before the getter's
definition can therefore recognize a reference-returning prototype.
`decltype(auto)` definitions also record address facts for recognized lvalue
returns: parenthesized names/members, dereferences, same-type lvalue selectors,
assignment/comma results and known reference-returning calls. An unparenthesized
name or member uses its declared reference identity; `return a` therefore stays
a value when `a` is an ordinary variable, while `return (a)` preserves its
reference. Plain `auto` does not apply this rule.

`return &local` is recorded as `AddrOfVar` but is **unsound** for stack locals (may-analysis may report escaped addresses). Prefer treating this as a known imprecision.

At PAG build time, `CallReturn` resolves through
`SymbolTable::call_return_candidates` and transitive `ReturnFlow::Call` through
`SymbolTable::return_flow_candidates`. Both scope the lookup by the function
the call is written in, never by the destination's owner, so a global or
file-static `g = f()` inside `use` sees a `static` `f` exactly as a local
would (#132). The resolver applies target scope and TU-local definition
precedence; header-body contexts follow
[Shared header functions](ANALYSIS.md#shared-header-functions). A `CallReturn` with no
caller (a file-scope initializer, `void *g = f();`) resolves in `g`'s own file
with the same precedence. Callee ids that survived lowering and merge (e.g.
`AddrOfFn`) are used directly.

This models patterns like:

```c
subDev->subDevOps = GetSensorDeviceOps();  // return &g_sensorDeviceOps
subDev.subDevOps->setConfig(subDev);
```

Nested call arguments (`consume(make())`, `Register(obj->GetHandler())`, `consume(table[i]())`, `std::move(make())`) materialize a temporary variable assigned via `CallReturn`, `CallReturnIndirect`, or member call return destinations, which is then passed as the actual argument into the outer call site, matching the semantics of `Callback f = make(); consume(f);` both within a translation unit and across units. Member call returns (`consume(obj->GetHandler())`, `consume(base->virtual_method())`, `x = obj->method()`, `return obj->method()`) are classified as `ResolvedCallReturn::Member` during lowering: candidate methods are ranked by argument count and overload resolution (`rank_per_class`), attaching `return_dst` to the resulting `CallSite`. Class Hierarchy Analysis (`expand_virtual_overrides`) replicates `return_dst` across all concrete derived overrides, and `Pag::expand_call_site_returns` expands return flows into `return_dst` across all candidates and overrides at PAG construction time, properly connecting return values from derived implementations (including pure virtual base interface declarations `= 0`). Candidate grouping, visited cycle-guards, and model fallback decisions are scoped to each syntactic call occurrence (`(dst_var, caller, occurrence)`), ensuring a real return from one call does not suppress models for a separate bodyless call assigning the same variable. Returned call expressions (`return make()`, `return (*factory)()`, `return obj->method()`) resolve through the shared `resolve_call_for_return`, ensuring indirect and member call returns materialize into temporaries with `ReturnFlow::Copy` and preventing local variable or field names from leaking into direct function resolution. Temporaries are allocated only after confirming resolution, preventing orphaned variables in symbol tables.

Value lowering peels [standard move and forward calls](CXX.md#c-support-first-step)
to their operand. Argument ranking retains the known lvalue or rvalue
category and referent qualification. Parameter metadata uses the
collapsed reference category for declarations such as `Ref&&` with
`using Ref = T&`. Pointer-reference parameters keep this category and known
pointee constness in overload metadata even when lowered as pointer values
for flow analysis. For member calls ranking checks receiver viability before
preferring an argument binding. Base qualifiers resolve in their lexical scope, with a leading
`::` starting lookup at global scope. Result type and constness inference
share the receiver and reference viability checks. Default arguments come from declarations
matching the selected member overload. Reference viability also checks direct
bindings to base subobjects. Virtual dispatch expands the overload selected
from the static receiver's declarations to its override family.
A same-class glvalue such as a named object, member access, reference-returning
call, or conditional glvalue converted to a common base selects a copy or move
constructor when it initializes an object. Constructors eligible for implicit
conversion participate in selection, including defaulted and deleted overloads.
When a winning defaulted move may be implicitly deleted because of its
subobjects, lowering also considers the overloads that could win without it.
An own move-assignment declaration on a subobject can suppress its implicit
move constructor and delete its implicit copy constructor. The viability
check reuses the result for layouts reached through multiple subobjects.
A const class subobject requires a viable copy. A reference field can make
that copy unavailable when the field is an rvalue reference.
The call graph records selected user-provided constructors. Reference
declarations bind directly to the initializer, and same-type prvalues
construct the destination directly under C++17 copy elision. Same-class assignments record
declared copy or move `operator=` sites where a visible user-provided overload
can accept a same-class argument and wins overload selection. Defaulted
assignments participate in selection and have no synthetic call site. This
supplements the existing may-flow for assignment.

**`CallReturnIndirect`** is the indirect-call analogue of `CallReturn`. The callee is resolved by the solver when indirect call targets are known (via function-pointer analysis). The `callee_var` is a synthetic load variable that holds the resolved function pointer; the solver wires return flows from each resolved target into `dst`.

**`ContainerElement`** gives `dst` the address of a typed, index-insensitive
container element cell. A named container uses its merged variable identity.
A data member uses its declaring type and member identity within the link
target, sharing the cell across instances of that member. Lowering emits
ordinary `Store` and `Load` facts through the address. See [Container
elements](CXX.md#container-elements) for the supported C++ operations.

**`NewHeap`** represents C++ `new T(...)` allocations, and the object a recognised smart-pointer factory builds ([Factory construction](CXX.md#factory-construction)). The PAG allocates a heap location typed to the allocated struct and adds an `AddrOf` edge from `dst` to the heap location. The solver then propagates into the struct's fields, enabling resolution of function pointers stored by constructors (e.g., `MParcelImplInterfaceAssign` writing into `HdfSBufImpl.readBuffer`). Lowering (`lower_new_object`, `construct_on_heap` in `lower.rs`) gives every `new T(args)` its heap object, a `$new<id>` temporary typed `T`, in value and statement position alike, and at file scope too; the constructor sites (in a body) take it as `this` and the constructor's own argument list as their arguments, never a placement list (`new (std::nothrow) T(a, b)` passes `a, b`); a braced list (`new T{a, b}`) is that list when `T` has a user-provided constructor, as for a braced local, and otherwise initializes an aggregate member by member, with no constructor site; and the expression's value is that object. A construction from one argument records no constructor site (`copies_implicitly`, checked in `emit_member_targets`) wherever the object is built (`new T(other)`, a factory, a local `T x(other)`, a class-typed member initializer `m_(other)`) when either no constructor of the class takes one argument, or the argument is an object of the class itself (directly or by reference) and no constructor takes the class: that is a copy or a move, which runs the implicit constructor, where arity filtering would otherwise fall back to every constructor, or rank `T(int)`, and bind the copied object to an unrelated parameter. A pointer argument (`T *`) is not an object copy; a reference to a pointer keeps that pointer type too. A user-declared copy or move constructor (`T(const T &)`) keeps its site, including constructors whose later parameters have defaults (`T(const T &, int = 0)`). An aggregate whose definition and constructor set are indexed, copied from its own type, records no unresolved constructor site. For `new` and factories, cached project headers import project prerequisites as types only, so an empty constructor lookup there retains a site for the later TU symbol merge to resolve. A missing class definition or an unknown constructor parameter type retains the possible constructor edge on those paths. Member-initializer lists omit the implicit default or copy constructor itself while retaining nontrivial construction of its base and member subobjects. A reference member's binding (`ref_(other)`) records none regardless of its class's constructors. See [Ctors / dtors](CXX.md#c-support-first-step) for the member-initializer rule.

**`StringConst`** intern a C string literal as an abstract location (`LocKind::StringLit`). Assignments (`const char *n = "foo"`), copies, and call arguments intern the same way, so a later `dlsym(h, n)` still sees `"foo"`. Concatenated literals (`"ta" "rget"`) are folded. String literals represent immutable constants and are excluded from being writable memory cells: stores through untyped or cast pointers do not write into string literal locations, and dereferences of pointers holding string literals do not merge cell memory (see [Propagation highlights](POINTERS.md#propagation-highlights)). No `sprintf` / buffer writes.

### Conditional values

`condition ? left : right` contributes both arms under the flow-insensitive
may-analysis. Lowering materializes one cached value per conditional syntax
node, lowers the arms into that destination in source order, and copies the
result wherever it is consumed: initializers, assignments (including stores),
returns, arguments, receiver values and indirect callees. Nested conditionals
use the same operation. Existing `Copy`, address, load, call-return, heap and
null constraints union the alternatives monotonically, without selecting a
runtime branch. Ordinary value returns use `ReturnFlow::Copy`, so the union
survives a function boundary and translation-unit merging.

Compound multidimensional array arguments decay to the selected arrays'
original storage before binding pointer-to-array parameters. Character-array
decay retains both storage addresses and interned strings for consumers such
as `dlsym`. Struct arrays assigned to pointers likewise retain object storage;
a pointer cast used for indexing projects fields from those objects.
Known pointer/function aliases used as functional casts, including C/C++
`(Alias)(value)`, preserve operand values without inventing calls to the alias.
In C, a tag with the same spelling does not hide that ordinary typedef.
Parenthesized alias casts also recognize an ungrouped comma operand list
(`(Alias)(touch(), value)`); only its last operand supplies the value, while
all evaluated operands retain their calls, effects and source provenance.
For a reference cast, the argument-list-shaped comma operand forwards its last
operand's original cell to address lowering as well as its value.
The same nearest-type/value/function shadowing rules as reference casts apply.
Recognition asks first whether any alias name ends in the callee's last `::`
segment, then walks the scopes, and only after finding an alias asks whether a
nearer value hides it: most calls name a function, so neither lookup runs for
them. Every alias-only scope lookup (alias casts, alias cv and reference
categories, callable aliases, aliased instantiations) makes the same
last-segment check first; the type table keeps a hash of each alias name's last
segment for it.

Built-in array ranges consume the same element summary when a range-for
expression selects arrays or uses comma/assignment values. Explicit, `auto`
and reference loop variables receive the possible elements. This does not
supply a container iterator model. Callable class objects in compound callee
expressions resolve their `operator()` through ordinary member lookup and
overload ranking. Comma and assignment receivers retain their known class
for method lookup, just as conditional receivers do.

Compound elements in callback-array initializer lists (`F table[] =
{c ? alpha : beta}`) copy their shared expression value into the array's
index-insensitive element summary. Nested lists use the same summary. Arm
calls retain their own return destinations; their temporary values do not
replace the declared array type. Struct/union array members continue through
the aggregate field paths rather than copying unrelated fields into this
scalar element summary. Value casts around an initializer element are peeled
by the same normalization as ordinary RHS lowering, while the element's
enclosing operation retains its source provenance. Positional and field-designated
struct arrays containing compound values use the existing typed aggregate
traversal, including nested arrays and positional brace elision; each callback
goes through its
declared field's GEP and store. Known unrelated fields never join the scalar
array summary. Mixed C lists advance from a designated subobject through its
remaining array elements and fields before advancing the containing object.
The designated cursor initially includes every available positional array
element. Exhausting that cursor does not skip later explicit designators.
After explicit clauses, untouched subobjects receive normal omitted-member
initialization, including defaults at nested designated levels. An explicitly
initialized subobject suppresses its declaration default. Constant declarator
bounds are retained in the initializer's private traversal shape.
Global default completion recovers those bounds before traversing an array, so
written omitted elements execute their defaults even if the exported type has
an unknown extent. For an omitted outer `[]` bound, designated completion
infers the extent from the highest initialized index plus one. Contiguous
coverage, including reordered designators, adds no whole-element default;
gaps still evaluate their omitted elements' defaults. Arrays with known or
initializer-inferred extents do not gain extra default evaluations when fully
supplied. An explicitly written bound that cannot be computed remains
conservative and can still cause an extra default evaluation; it is not
treated as an omitted bound.
For a designated union, only the explicitly selected member completes its
nested defaults. Inactive members never run their declaration initializers.
Field designators use the known containing type when projecting
their fields. C positional, mixed and index-designated lists use the same
typed subobject traversal when a layout is available. Anonymous aggregates
and C structs do not require C++ construction metadata. Constructor-free
unions initialize the first member or the designated member; nested array
fields retain their element type while indices share summary storage.
Constant struct arrays with distinct callback members or nested fields also
use field stores, so a subsequent pointer cast cannot observe callbacks from
unrelated members through a whole-array function summary. Existing constant
tables with one callback field retain their per-array function summary.
Opaque scalar spellings, including recovered enum tags, consume one clause;
only available aggregate layouts introduce subobject traversal. Non-aggregate class-array elements lower their selector
arguments through the ordinary constructor path.

GNU shorthand `value ?: fallback` uses the condition itself as its true
value, for both flow and type inference. A call in that position is lowered
once and retains one source site and return destination. Plain `=` assignment
arms contribute their assigned RHS value while also writing the destination;
previous values in that destination do not become assignment-result values.
Comma arms contribute only their right operand. Calls and side effects in
the left operand remain indexed. The shared expression cache prevents the
later body walk from repeating assignments materialized for a value reader.
File-scope initializers have no body walk: their evaluated operands are walked
once at the initializer boundary, before value, address or aggregate lowering.
This includes conditions, comma operands, array-decay operands, subscript
indices and lambda init-captures. Assignments
use the same expression cache, so GNU shorthand and repeated value readers
do not repeat a store. Walking operands for effects does not allocate an
unused conditional-value temporary. The same effects walk covers parenthesized constructor arguments and
instantiated default-member initializers at file scope. Nonempty positional
and designated global aggregate lists complete omitted defaults while explicit
member values suppress those defaults.
Anonymous layouts and constructor-free unions use the same traversal. A
default-initialized union selects its declared default member, including a
member after the first. With a configured C++20 or newer standard, a global
aggregate's parenthesized initializer completes omitted members as its
braced form does. Explicitly supplied members suppress those defaults.
The effects walk skips C `_Generic`
controlling operands using the shared unevaluated-operand rule. Its explicit
stack observes the ordinary AST depth limit and emits one warning instead of
recursing through generated initializer chains.
A closure's init-captures execute when
the closure is created; its body and unevaluated operands such as `sizeof`
and `noexcept` do not execute as part of that initializer.

An address of a conditional lvalue (`&(c ? a : b)` in C++) unions the
addresses of the original selected cells, rather than taking the address of
the merged value temporary. This preserves both callbacks through a pointer
argument and lets stores through the selected address reach `a` and `b`.
Named variables, fields, dereferenced pointers, function designators and
nested selectors share the ordinary address and GEP constraints. Array
element addresses retain the ordinary index-insensitive base-array summary.
Address-taken pointer/callback array summaries synchronize their tracked element
values with storage; see [Variable cells](POINTERS.md#propagation-highlights).
Reference-returning call arms retain their original referent addresses via
the separate [reference return facts](#return-value-flow), including across
translation units and through indirect and virtual calls. A bodyless getter
without reference-address return facts does not supply a known cell.
These address values also flow through initializers, assignments and returns.
Standalone `&(touch(), a)` and `&(p = a)` use the same address lowering.
Assignments to conditional lvalues (`(c ? a : b) = value`) store into both
possible cells under the may-analysis.

Field paths and dereference operands also consume the shared compound value:
`(c ? &a : &b)->callback()`, `*(c ? pa : pb)` and stores through the latter
retain the flow of their named-pointer equivalents. Smart-pointer field roots
continue through the existing wrapper boundaries and compatibility filter.
Built-in subscripts with a compound table (`(c ? a : b)[i]`) select the
original array-summary addresses or the pointer arms' values, then load or
store through that address. Initializers, arguments, returns, callees and
assignment targets share this lowering. Unknown indices remain summaries;
class `operator[]` and container return models retain their existing limits.
Reference casts used as tables, including functional casts through array
reference aliases (`R(c ? a : b)[i]`), unwrap before array decay. Reads and
stores therefore reach the original element-summary cells, as do element
addresses and indirect calls.
Ordinary pointer casts around compound tables also preserve the operand's
pointer value, with array decay applied before indexing. An aggregate element
retains its selected object address for member projection (`(c ? a : b)[i].f`);
scalar elements load the contents of their summary cell. Multidimensional
subscripts remove one type layer per index. Each additional pointer index loads
the intermediate pointer, while array layers retain summary storage. The rule
applies to selected pointer tables' reads, stores and addresses. Compound
callees preserve their original dereference chain before materialization.
Indexed member paths collect
indices in the syntax walk and validate them during one forward field/type
traversal. Pointer layers load their stored values at the index/arrow boundary;
array layers keep summary storage. Mixed arrays of pointers use both steps.
Dereference operands with built-in pointer offsets (`array + index`,
`index + pointer`, `pointer - index`) retain that index-insensitive
pointer/array summary, including reference returns such as
`return *(table + index)`. This operand path requires a known integer offset;
known unscoped enum tags and directly spelled defined unscoped enumerators
also admit integer offsets. Class bodies register their enums in the owning
scope, and enum/enumerator offset lookup uses ordinary enclosing scopes,
using-declarations and using-directives. Shared type imports retain enum tags,
their scoped status, and the tag associated with each enumerator. A table keeps
its own entries and shares each imported table's entries. A header's entries
cover its whole include closure, and every including unit imports the header. A copy
per import costs a hash probe per name per merge and a set per table, while a
lookup, which is rare, probes each layer. An imported layer is a snapshot.
Built-in integral arithmetic, shifts, bitwise/unary operations,
comparisons and logical operations retain integral offset compatibility when
their operands establish the required built-in types. Integral cast targets
and `sizeof`/`alignof` also establish it. This category probe stays separate
from overload argument typing: it does not coerce enum identities there or
interpret overloaded class arithmetic as built-in arithmetic.
Ordinary pointer-offset
values and arguments, offset arms of selectors, and dereferenced offset
callees use this same cached value path. Offset-chain type probing uses an
explicit stack bounded by the ordinary AST limit, so generated arithmetic
does not recursively overflow the parser's stack.
Pointer differences, unknown offset types and class operator overloads do
not establish a pointer value. It does not add offset arguments to byte-buffer
or class-conversion models. Nested known-integer offsets materialize
their pointer operands recursively through the same cached value path.
Class value conversions keep their own semantics; their constructor input
does not become a built-in table through cast peeling.
Address lowering also applies this element-address rule to ordinary subscript
arms: `&(c ? p[i] : q[j])` selects the cells reached through `p` and `q`,
as `&(c ? p : q)[i]` does. Taking the address of a pointer variable would
add a spurious level of indirection; taking an array element's address keeps
the array's existing summary cell.
Field-held tables still require their field layout/value to be resolvable;
the root pointer variable's storage does not substitute for a missing field.
Lvalue reference casts in an address (`static_cast<T&>(x)`, C-style reference
casts, reference aliases and nested reference casts) preserve the original
referent cell. Address lowering unwraps each reference cast separately and
keeps the enclosing cast's operation provenance. Reference-return facts use
the same recognition, including deduced lvalue-reference returns. Value and
pointer casts do not establish this cell identity.
Recognized reference casts on assignment targets use the same referent-address
path as conditional lvalues, for both ordinary assignments and assignments
whose result is consumed. This includes named, functional and C-style
reference casts, including an address passed directly as an argument.
Reference binding detection follows function, array and pointer declarators
to the modifier nearest the declared name or abstract endpoint. Explicit
function-reference targets such as `void(&)()` preserve the same function
address as a function-type alias followed by `&`. `F*&` binds a reference
to a pointer; `F& (*)()` binds a pointer to a function returning a reference
and does not establish reference identity for that pointer.
Casts to rvalue references to functions (`void(&&)()`, or a function-type
alias followed by `&&`) also produce function lvalues and retain function
addresses. Object casts to `T&&` retain their xvalue category.
A one-argument functional cast through a visible reference alias (`R(x)`)
uses the same referent-address lowering and records no call to `R`. Ordinary
calls through shadowing variables/members or nearer functions retain normal
resolution. Calls inside a cast's operand retain their own source sites and
return destinations. Alias lookup stops at the nearest visible declared
class or union, including a forward declaration, before consulting outer
reference aliases. A class constructor sharing an outer alias's name retains
its ordinary constructor and receiver behavior. Receiver and conditional
type probes use the alias's referent type, preserving any additional pointer
or array layers, before ordinary function-result lookup. Thus
`R(c ? a : b).method()` and a selector
between two `R(x)` casts have the same receiver type as their referents.
Functional reference casts also contribute values as arguments and indirect
callees, including when the cast wraps a selector. Complete function-reference
aliases (`using Ref = void(&)();` and the corresponding typedef) retain their
reference binding with the same declarator walk used for explicit casts.
Same-signature function designators in an lvalue selector can produce
`decltype(auto)` reference-address facts; signature comparison uses the shared
symbol metadata, including parameter binding and variadic status.
Compound assignments on compound lvalues read the selected value and write
it through the selected addresses without re-entering the same assignment.
Pointer offsets remain summarized; arithmetic does not select a runtime arm.

The condition and calls in both arms retain their ordinary source sites.
Arm operations and the right operand of standalone comma values retain their
own flow provenance. Calls assigning an arm retain the conditional's return
destination when the later body walk records
their sites. Reusing the conditional value avoids repeating arm lowering or
allocating a second callee temporary when return lowering and call collection
visit the same expression. Scope, linkage and target selection still use the
ordinary resolvers described in [Return-value flow](#return-value-flow).
When default member initializers are reparsed for construction, their syntax
caches are isolated and the enclosing caches restored afterward. Node IDs
from a discarded initializer tree never identify a different initializer.

A result type is inferred only when both arm types agree by the IR's strict
type-shape comparison, when one C++ class pointer converts to the other arm's
known base-class pointer, when a class converts to the other arm's known
base class, or when a null arm (`nullptr` or literal zero)
accompanies a known pointer type. Otherwise the result type is unknown,
while every known arm value still flows. This also
applies to `auto` deduction and receiver/argument type probes. Named class and
union types compare by their canonical tag name, independently of the layout
fields available at either arm; pointer layers and function signatures must
still match, and unknown types are not wildcards. Nested conditional type
probes cache each syntax node's result, including unknown types, so materializing
successive suffixes of a selector chain does not rescan those suffixes. Type
probes and value materialization obey the AST depth bound; a truncated suffix
can therefore lose values and is reported by the depth warning. Reparsing an
initializer isolates this type cache alongside its value caches. Explicit casts to known classes use
their written type when probing conditional arms. Base-pointer conversion
uses the shared inheritance graph and applies to one pointer layer only;
unrelated classes and sibling classes' common ancestors are not inferred.
Common-class type probes do not recurse through lvalue-category checks;
reference-return classification checks the category separately.
Matching array lvalues retain their array shape. Mixed/null arms apply
array-to-pointer decay, including for `auto` deduction and receiver probes;
the value flow then carries the original array summary's address.
Selecting between
container-returned values still depends on the container's return
model and visible type declarations; selecting smart pointers retains the
existing [unwrap rules and compatibility filter](#smart-pointer-unwrap).
The reduced `conditional_values` fixture uses explicit stream getters and
null-or-object construction to test these flows independently of container
models. Field values retain the existing instance-insensitive summary rule.

#### Conditional value limitations

The support above extends the existing expression and pointer models; it is
not a complete implementation of C/C++ expression semantics. The regression
fixtures establish the covered forms, not every possible composition of casts,
declarators, overloads, templates, initializers and value categories. A source
file parsing without diagnostics does not guarantee complete value flow.

- **No path or execution-order precision.** Both arms contribute even when a
  condition is constant. Correlations between selections are lost. Initializer
  and assignment effects accumulate monotonically; they do not simulate
  execution order, object lifetime or which branch actually runs.
- **Summarized storage.** Array indices and pointer offsets share element
  summaries, and field summaries are instance-insensitive. Different elements
  or objects can therefore contribute extra targets. Null arms contribute no
  pointee; retaining a non-null alternative does not prove dereference safety.
  The structural union/first-member aliases described in
  [Field sensitivity](POINTERS.md#field-sensitivity) do not provide a general byte-storage,
  ABI layout, arbitrary type-punning or bidirectional cast model. Packed layouts,
  bit-field widths and full C++ standard-layout eligibility are not established
  by these rules. A compatible layout prefix can conservatively alias even
  when the analyzer cannot prove all language eligibility requirements.
- **Partial type and conversion inference.** The result-type rules above do
  not implement the full C++ conditional-conversion or overload rules. Common
  ancestors of sibling classes and conversions through multiple pointer layers
  are not inferred. Unknown argument types can retain multiple overloads;
  unknown receiver types can prevent method resolution even when arm values
  are preserved. No complete user-defined conversion or template evaluator is
  supplied by this feature.
- **Built-in arithmetic only.** Pointer-offset flow requires a recognized
  pointer/array operand and a known integral offset. Unknown offset types,
  pointer differences and overloaded class arithmetic do not establish a
  pointer value through this rule. Offsets do not distinguish elements or
  model byte ranges and bounds.
- **Existing model dependencies remain.** Arms can contribute only values
  established by their declarations, bodies or summaries. Container element
  returns and iterator ranges need container models; smart-pointer receivers
  need the existing unwrap/type-compatibility rules; fields need resolvable
  layouts. This change does not fill in missing library bodies or summaries.
- **Parser and traversal limits remain.** Accepted syntax can still contain
  unmodeled value forms. Bounded AST walks warn and skip deeper nodes when
  their depth limit is reached, so generated extreme inputs can have incomplete
  flow despite completing analysis.
  Value, lvalue-address and array-decay expansion share the AST budget.
  Function-designator representatives and lvalue categories cache each syntax
  node's result, including misses, and bound category recursion too. Scratch
  initializer trees isolate these caches together with the type/value caches.
  The alias-call normalization here admits pointer/function/reference aliases;
  an ambiguous scalar/class alias cast parsed as a call, such as `(ul)(p)` for
  a scalar typedef, remains outside this path and can produce a phantom call.

Exact target assertions apply to the focused fixtures. Pinned-corpus comparisons
measure the configured revisions and models, not all valid C/C++ programs; the
known HDF summary imprecision is recorded in the
[evaluation report](EVAL_REPORT.md#conditional-pointer-and-callback-values--2026-11-03).
Uncovered combinations should be assessed against these limits and a reduced
control before being described as regressions. Crashes, invalid IDs and lost
targets in covered forms remain defects regardless of how rare the input is.

### Dereferenced operands

This section is the authoritative description of what `*x` reads, stores
through and passes. `*x` goes through the value `x` holds, computed as a value
(`Operand` in `lower.rs`):

- A name is its own variable: `y = *p` is one `Load`, with no temporary. A
  name bound to a reference is loaded through first. A reference is a
  declaration whose outermost declarator is `&` or `&&` (`declares_reference`),
  so `T *&r` and `T *const &r` are bound to the pointer's value, as
  initialization and argument passing bind them.
- Reference typedefs and `using` aliases retain their reference identity
  alongside the pointer-shaped alias descriptor, including through cached
  header merging. Parameter signatures and local bindings use this metadata;
  constructor copy detection unwraps only a referent's address, keeping raw
  pointer arguments and references to pointers distinct from class objects.
- Header units and reusable preambles retain reference-binding metadata with
  their declarations. Unit merging remaps its variable and function IDs before
  a consuming lowering context takes ownership of it; reference type-layer,
  address-representation and return-reference facts travel with the bindings.
  Skipping a declaration walk therefore preserves `&ref` as the referent's
  address (`Copy`), including for static reference members. An ordinary
  variable's address still uses `AddrOfVar`. This indexing metadata is released
  with merge state and adds no database columns.
- A member path (`h->pp`, `this->a->pp`, `(*this).m`) is projected and its cell
  loaded: `y = *h->f` is `Load(y, Load(GEP(h, f)))`, and `*h->f = v` stores
  through the loaded value the same way. An array member is its own cell
  (`*h->arr` reads `h->arr[0]`; a struct's array field is typed
  `Array { elem }`); an element of a member table (`h->t[i]`) is loaded from
  the table's cell.
- An inner `*` is loaded: `**pp` loads twice.
- A conditional, assignment or comma operand uses its cached compound value
  before the dereference, for reads, stores and arguments alike; see
  [Conditional values](#conditional-values).
- An ordinary array element uses the array summary's value with the element's
  type, so `*table[i]` loads through the pointer stored in the table. Callable
  elements use the ordinary table load, which preserves initializer functions
  without copying the array's storage address into the callable value. An
  element reached through a pointer instead loads through that pointer.
- A call's result, for a direct call by plain name (`*f()`) or a call through
  a function-pointer data member (`*h->cb()`). A method's result is not
  modeled, so `*obj->Get()` has no value.
- `this` is its own value.
- A function pointer is read in place (`*fp` is `fp`); a pointer to a
  function pointer is loaded. A function designator is the function itself:
  `y = *handler`, `T y = *handler` and `take(*handler)` take `handler`'s
  address, as `y = handler` does.

A bare instance field in a member body is a member path rooted at `this`
(`m_pp` is `this->m_pp`, `(*m_in).pp` is `this->m_in->pp`); see
[Implicit this member variable access](POINTERS.md#implicit-this-member-variable-access)
for when a name is one.

Resolution emits nothing until the whole operand resolves, so an operand that
fails leaves no temporaries behind.

Positions:

- A read `y = *x` and a store target `*x = v`: an operand that does not
  resolve falls back to the root variable its lvalue names (`h` in `*h->f`
  when neither `h`'s class nor a base declares `f`), loading or storing through it. This
  over-approximates rather than drops the flow.
- A store resolves its target first, then its value; a target that cannot be
  lowered emits no value temporaries.
- The value positions `return *x;` and the store source in `*out = *x` emit
  nothing when the operand does not resolve. Arguments fall back instead
  (below).
- A member read without `*` is the same value: `y = h->f`, `y = m_f` and
  `return h->f;` load the member's cell, and an array member is its cell
  there too (`p = h->arr` and `return arr;` hold the array's address, not
  what its elements hold).
- `return *p;` in a function declared to return a reference (`S &f()`, or a
  trailing `-> S &`; `returns_reference`) returns the address, `p`'s value.
  `decltype(auto)` returns by value.

Arguments `f(*x)`:

- C passes the loaded value.
- C++ passes by the callee's declared parameter kind,
  `Function.reference_params`. It is recorded from declaration syntax, in-class
  prototypes, unnamed parameters and lambdas included, with the outermost-
  reference predicate above. A `...` tail is by value; a parameter pack takes
  its own kind for every position past the others.
  - A reference parameter takes the address, `x`'s value.
  - A by-value parameter takes the loaded value. The load is emitted once per
    argument and shared by every target of the site.
  - Where the kind is unknown (no declaration read, an indirect call, an
    unresolved callee), one temporary holds both the address and the loaded
    value.
- An operand that does not resolve passes its root variable, unloaded
  (`resolve_expr_var`): `o` in `take(*o->absent)`, where no layout declares
  `absent`, and `arr` in `take(*arr[i])`.

A dereferenced owning-pointer extraction, `*sp.get()`, follows the same
argument rules; supported extraction forms are defined in
[Smart-pointer unwrap](#smart-pointer-unwrap).

Field-path roots:

- `(*X).f` is rooted at `X`'s value: `(*h->in).f` projects `f` from the object
  `h->in` points to, and `(*m_in).f` in a member body from the object the
  instance field `m_in` points to.
- `(*&x).f` is `x.f`.
- `(*pp)->x` reads `x` through the pointer `*pp` loads.

Limits:

- A call lowering cannot name a callee for (a function-pointer call, a lambda
  called through its closure variable) passes the combined temporary for `*x`.
- `*h->t[i]` on a member table of pointers reads one level short, as
  `field_table` reads a table element.
- Unsupported cast operands and elements of a local array
  (`*arr[i]`) are not values: a read or store target and an argument keep
  the root variable where the lvalue names one, and `return` or a store
  source emits nothing.
- `&*x` is not simplified: `&*h->f` takes `h`'s address.
- A `T *&` out-parameter is bound by value, so assigning to it does not reach
  the caller's pointer.

Built-in pointer offsets retain their index-insensitive summary; see
[Conditional values](#conditional-values). Aliased and deduced reference
return classification follows [Return-value flow](#return-value-flow).

### Cast receivers

A field path rooted at a pointer cast of a variable — `((T *)x)->f`,
`static_cast<T *>(x)->f` and the other named casts — resolves its fields
against `T`, whatever `x` is declared as: a `void *` context argument, a base
pointer cast down. The root is a receiver temporary of class `T` with
`Copy(recv ← x)`, so the path sees every object `x` points to and, when `x`
points to nothing known, `T`'s field summaries, as a `T *` local initialized
from the cast does. `cast_root` in `lower.rs` decides:

- the cast's type, read as a declaration's is (a typedef of the class or of
  the pointer included), is one pointer level to a class the index knows.
  The cast's own levels are its abstract declarator's `*` layers, so a `*`
  inside a template argument (`static_cast<Box<int *> *>`) is the type's and
  a reference (`T *&`) adds none;
- the operand is a variable. The root of any other operand (`n->data`,
  `a[i]`, `&obj`) is a different object, and such a path is left as it was;
- an arrow follows the cast.

A C-style cast to the operand's own class keeps the operand as the root.

### Smart-pointer unwrap

`UnwrapPointer { dst, src }` is the step through a smart pointer's overloaded `operator->` or `operator*`, or its modeled raw-pointer extraction. `src` holds the wrapper value (`sp`, or the loaded `h.item`); `dst` is the pointee-typed receiver temporary (`_recv`) that field accesses continue from, or the raw pointer returned by extraction. The destination's class type (through one pointer layer for a raw pointer) is the only type the constraint consults, so merge remaps nothing but the two variables.

The wrapper and its pointee are separate objects. A plain `Copy` would let the wrapper's own storage reach GEPs typed for the pointee, and the GEP field-name guard cannot tell two classes apart when they share a field name at the same position. The PAG kind therefore filters: `pts(dst) ⊇ { o ∈ pts(src) | o is compatible with dst's pointee type P }`, where a location is judged by its recorded type:

| Location type | Decision |
|---|---|
| Struct/union whose declaration (`tag_identity`) is `P`, or a class deriving from `P` | admitted |
| Any other struct/union, wrapper storage included | rejected, unless the receiver's pointee type is itself unresolved: then every struct/union is admitted |
| `Unknown` or `void` (untyped storage, e.g. a modeled `ReturnHeap` location) | admitted conservatively |
| Array whose element type is admitted (`&pool[i]` is the array's location, index-insensitively) | admitted |
| Pointer or scalar (the storage of a pointer variable), or an array of one | rejected: not a pointee object |
| Function or string literal | rejected: not a pointee object |

A field location is judged by its own type, which is the field's type, so `&h.item` (a wrapper-typed field) is rejected.

The unwrap adds no fallback of its own. The receiver's GEPs already fall back to the pointee type's field summary when the receiver has no pointees or none of them yields a field cell (see [Propagation highlights](POINTERS.md#propagation-highlights)), so `sp->cb` and `raw->cb` still share `summary:Payload.cb` when `pts(sp)` is empty, which is the common case for parameters and for standard wrappers whose headers are not in the tree. In those cases the unwrap's value is the traversable `unwrap` edge in the exported flow graph; it carries concrete locations only when the wrapper value has some.

A wrapper variable is a pointer to its pointee as far as the solver is concerned: after merging, `mark_wrapper_values` (`lower.rs`) sets `is_pointer` on every variable whose type `operator->` steps through. Its value and its storage are therefore kept in step ([Variable cells](POINTERS.md#propagation-highlights)), so an object stored through `&sp` (`Fill(&sp)` writing `*out = &obj`) reaches `sp->f`, and `(*w)->f` sees `sp = x` copies; and a wrapper argument is wired to its parameter by a persistent copy, as a raw pointer is.

A field path may also start at a call's result when it crosses a wrapper at its root (`GetSp()->cb()`, `GetSp()->value`): the path validates first, then the call's result is lowered into a temporary (`CallReturn`) that the unwrap reads. Apart from the modeled `get()` extraction below, only a direct call by plain name has a return flow to read; a qualified or member call (`Foo::GetInstance()->f`) is not decomposed, and a call returning a raw pointer keeps its previous handling. A dereferenced root is a value ([Dereferenced operands](#dereferenced-operands)): `(*h->sp).f` unwraps the value the member `h->sp` holds.

**Raw-pointer extraction (`get()`).** Zero-argument `get()` on
`std::shared_ptr<T>` and `std::unique_ptr<T, D>`, with a held class or union
`T` the call site can name, yields a raw `T *` alias through the same
filtered unwrap. The optional unique-pointer deleter does not affect the
pointee. Explicitly qualified member names, namespace directives and specific
class imports use the ordinary class resolver, including namespace shadowing.
One recognizer supplies both the result type and value flow. Other classes'
`get` methods, calls with arguments, and `unique_ptr::release` receive no
extraction summary.

Expression lowering emits a provisional unwrap regardless of getter
visibility. Each extraction has a `TempKind::Unwrap` result typed to the
canonical held tag, followed by a copy into its destination. The type is a
compatibility filter, so variant merges pair this temporary by position,
kind and type; differently configured held classes retain separate filters.
Named destination spellings, including bare names imported from a namespace,
cannot replace the held tag's identity. Template-held classes use the declared
template's layout, consistently with the existing arrow model.

After unit and link-target merging, getter settlement uses the actual call
site's resolved candidates and return slot. An executable getter removes the
provisional unwrap and leaves ordinary member-return expansion, preserving
overloads, default arguments, inherited owners and per-image selection.
File-scope initializers have no `CallSite`: zero-argument-compatible getters
are resolved through the same member hierarchy and image-aware resolver, then
recorded as exact `CallReturnResolved { dst, callee }` bindings. The PAG
expands these from the selected `FnId`'s return facts. No name-only return
lookup runs after this selection. Unused supporting loads, copies and field-address
computations are pruned using dependency closures independent of flow order;
call, argument and return uses remain. There is no declaration prepass or
getter-definition visibility set. Excluded dependency bodies remain
declarations and retain summaries. If a receiver value cannot be lowered,
the call falls back to ordinary member-return handling.

Receivers are evaluated as ordinary expression values, including wrapper
fields, owning-pointer array elements, `std::move`/`std::forward`, casts with
known owning-pointer targets and init-captures. Subscript typing applies only
to extraction receivers; ordinary C/C++ array reads retain their existing
summary semantics. General move/forward and init-capture receiver typing
also applies outside extraction (see [C++ support](CXX.md#c-support-first-step)).
Returned wrappers are received once. A wrapper descriptor
present only inside a pointer/reference parameter is interned on demand.
Reference bindings and reference-returned wrappers can hold either addresses
(`return *p`, dereferenced arguments) or already-lowered wrapper values
(`return global`, `return field`, named arguments and local bindings).
Receiver-value lowering therefore retains the original value alongside a
load through it. This preserves named const/reference parameters, `auto &`
and `auto &&` bindings, range-for references, captured reference parameters
and reference-returning weak-pointer promotions. Subsequent owning-pointer
unwraps exclude wrapper storage and incompatible objects. Result typing and
receiver normalization share overload selection and reference-return
metadata from declarations and definitions. Return statements use their own
definition's declarator to decide load depth, so a prototype or another
overload sharing an indexed function cannot alter the body. A `T *&` return
retains the pointer-value convention described in [Dereferenced operands](#dereferenced-operands).
Known value-returning receivers receive no extra load.

Assignments, arguments, dereferences, returns (including cast operands),
and field paths rooted at extraction, such as `p.get()->field` and
`(*p.get()).field`, reuse the result. The unwrap adds no allocation or
pointee load; `*p.get()` adds the ordinary dereference load, and reference
arguments retain the extracted address. Conversions such as
`void *raw = p.get()` still filter against the held class before copying to
the destination. The external getter call site is retained when the expression
appears in a function body.

Extraction limits: alias-template substitution and namespace aliases are not
resolved by this recognizer. An unqualified owning-pointer name found only
through `using namespace std` or `using std::shared_ptr` without an in-tree
declaration can also remain untyped; use a qualified standard name or provide
the declaration. Reference-returned wrappers through function pointers are
not normalized by the declaration-based receiver probe and can lose their
pointees. The mixed value/address reference representation can conservatively
add a same-typed pointee read from an already-returned object's cell. The
compatibility filter removes wrapper storage and incompatible objects but
cannot distinguish that extra same-typed alias. Normalizing reference
bindings, arguments and returns to one address representation remains a
separate change. Assignment conversions to `void *` retain extraction, but
existing pointer-argument wiring can still lose a class pointer passed to a
`void *` formal (`take(p.get())`). Non-owning cast receivers remain subject to
the general cast-typing limits; a move does not turn them into their operands.

Limits. Each leaves the receiver on the pointee's field summary, which is what every smart-pointer field access resolved through before `UnwrapPointer` existed:

- **Subclasses are those the inheritance graph records.** A class deriving from the pointee through a template parameter is one only when the template's definition is in the tree ([Template-parameter bases](CXX.md#template-parameter-bases)); otherwise its objects are rejected, as virtual dispatch through the same graph would not see them either.

### Weak-pointer promotion

`wp.lock()` on a `weak_ptr<T>` and `wp.promote()` on a `wptr<T>` whose body is not in the tree lower to `Copy { dst: result, src: receiver value }`: the strong result may alias the weak pointer's value. One recognizer (`weak_promotion` in `lower.rs`) decides both this and the result's type ([C++ support](CXX.md#c-support-first-step), `auto` locals), so the rules match:

- a `.` call with no arguments, named `lock`/`promote` as `WEAK_PTR_UPGRADES` pairs them with the receiver's weak pointer class;
- the receiver's class has no body in the tree. A declared wrapper's own `promote()` is an ordinary member call, and another class's `lock()` (`Mutex::lock`) is never a promotion.

The result's type additionally needs a held class the call site can name; the value flow does not, so a promotion whose type stays unknown still carries its receiver's value.

The receiver value is the variable for a plain name, and the object it names for a reference (`const wptr<T> &weak` is loaded through). Otherwise it is a temporary filled by ordinary expression lowering: `msg->weak.lock()` copies the loaded `weak` field, not `msg`; `get_weak().lock()` copies the call's return. A dereferenced receiver is read as any `*x` is ([Dereferenced operands](#dereferenced-operands)): `(*h->weak).lock()` loads through the `weak` member's value and `(*GetWeakPtr()).lock()` through the direct call's result, while a method call's result (`(*obj->GetWeak()).lock()`) has no value and nothing is copied. The copy is emitted wherever a call result lands: an initializer or assignment to a local, and, as a direct store of the receiver value, a store into a field (`s->strong = wp.lock()`) or through a pointer (`*out = wp.lock()`, parenthesized or not). The call site itself is recorded as before, so the call edge to the external `lock`/`promote` remains.

This is a may-alias relation. Expiry, a null result, and ownership counts are not modeled. `return wp.lock();` records no return flow: `ReturnFlow` has no receiver form.

### `shared_from_this`

`shared_from_this()` and `weak_from_this()` on an object whose class derives, directly or through its bases, from a class named `enable_shared_from_this` yield that object. One recognizer (`from_this_call` in `lower.rs`) decides both the result's type and its value, as [Weak-pointer promotion](#weak-pointer-promotion) does, once per call:

- a call with no arguments: bare in a member body (or a lambda capturing `this`) unless a local, a function the body or the class imports (`using factories::shared_from_this;`, `using A::shared_from_this;`) or an explicit global qualifier (`::shared_from_this()`) makes the name something else, or with an explicit receiver, the base itself included (`enable_shared_from_this<M> &r`) (`this->`, `p->`, `sp->`, `r.`, `m_ref.` for a reference member, `(*p).`, `Get().` for a call returning a reference). A class that declares its own `shared_from_this` without deriving from `enable_shared_from_this`, or that hides the inherited member with its own (`shared_ptr<Derived> shared_from_this()` casting the base's result, a callable data member of that name, or a static function-pointer member: ordinary member lookup finds it before the base's), keeps it as an ordinary member call. A declaration on the base, or on a class the base derives from (an internal base a library may declare the members on), is the standard member, not a hiding one. A qualified call names the member its qualifier sees, the qualifier looked up in the receiver's class and bases before the call's scope (`p->B::shared_from_this()` on an `n::D : n::B` is `n::B`'s, as is `p->E::` for an alias `E` that `n::B` declares; an injected class template name is the receiver's specialization, so `p->H::` on an `H<M> *p` binds `M`, and on a receiver spelled without one (`M *p` for `M : H<M>`) the concrete base fact supplies it, the resolved class keeping its namespace (`p->H<M>::` on an `n::H<M> *p` is `n::H<M>`): `Base::shared_from_this()` is `Base`'s, past such a hiding member, and `enable_shared_from_this<C>::shared_from_this()` the base's own, in the namespace the qualifier names (`std::enable_shared_from_this<B>::` past an `other::enable_shared_from_this<A>` base); a qualifier spelled as an instantiation (`Shared<M>::`) or as an alias of one (`using E = enable_shared_from_this<M>; E::`) binds its template's parameters as a receiver's does. Only a declaration the zero-argument call can take hides the standard member: an own `shared_ptr<S> shared_from_this(int)` next to `using enable_shared_from_this<D>::shared_from_this;` does not, nor does an imported overload set with no such declaration (`using A::shared_from_this;` for an `A` declaring only the one-argument form) veto another import that selects the standard member, an own member template (`template <class U = void> shared_ptr<S> shared_from_this();`) or an imported one (`using A::shared_from_this;` on such an `A`) loses to the imported standard non-template, as overload resolution prefers the non-template, and an own `const` overload does not hide the inherited non-const standard member a non-const member body matches better (`shared_ptr<S> shared_from_this() const;` beside `using std::enable_shared_from_this<M>::shared_from_this;` in `M`'s non-const body still yields the standard member). A class body's `using Base::member;` is a declaration in the derived class: member lookup unions the nearest class level's own functions with that level's imports, and a nearer declaration hides the imports of the classes above it. An import is followed to what lookup finds from `Base`, through `Base`'s own imports (`using Mid::shared_from_this;` where `Mid` writes `using A::shared_from_this;`), so an imported callable data member is a field call and an imported nonstandard function an ordinary call. Imports are indexed by member, so a call that no `using` names reads no import facts at all, and each class is looked up once per query: a hierarchy that re-exports the name on every inheritance path stays linear in its classes, and a chain of re-exports is followed to its end, only a cycle stopping it. The same import-aware set is what a call site records: `shared_from_this()` beside an own `shared_from_this(int)` and the import records the base's member, imported candidates stay when another overload of the name is virtual (only the virtual ones expand through the class hierarchy), and an overload of another arity is not recorded. The import's head is resolved from the class body that writes the `using`, never from a caller's scope: in that class and its bases first (a base's injected class name, an alias a base declares), then in the enclosing scopes. An import that positively selects the standard member is authoritative over an unselected declaration inherited elsewhere, and binds the held class from the base it names (`using B::shared_from_this;` in `D : A, B` yields `shared_ptr<B>`; the import keeps its template arguments, so `using enable_shared_from_this<B>::shared_from_this;` does too). Those arguments are named where the `using` is written (`enable_shared_from_this<M>` inside `namespace n` is `n::M`), and ones naming the importing class template's parameters (`using enable_shared_from_this<T>::shared_from_this;` in `H<T>`) are substituted from the receiver's instantiation (`H<M> *p` yields `shared_ptr<M>`), omitted ones taking their defaults (`H<>`). A fixed name keeps the `using`'s declaration scope even when another argument is substituted from a caller's scope (`using n::H<T, A>::shared_from_this;` in `n` holds `n::A`, not a global `A`). These facts are recorded in the IR and merged like the other member facts, so a header's imports reach the out-of-line bodies and every source unit the header is lowered into;
- the result is `shared_ptr<C>` / `weak_ptr<C>` in `enable_shared_from_this`'s own namespace (`std::shared_ptr<Manager>`; `std` when no declaration of the base is in the tree, however `using namespace std` reached it). `C` is the class `enable_shared_from_this<C>` names, resolved where the base is written, cv-qualifiers dropped (`enable_shared_from_this<const M>` yields `shared_ptr<M>`); through class-template holders (`template <class T> class Shared : public enable_shared_from_this<T>`) it is the argument the deriving class binds, substituted down every level (`M : Shared<M>` yields `shared_ptr<M>`), or the one a receiver spelled as an instantiation binds (`Shared<M> *p`), or the instantiation an explicit base qualifier writes (`enable_shared_from_this<B>::` inside a class deriving from both `A` and `B` selects `B`, not the first holder the ancestry reaches). Substitution is the one [Template-parameter bases](CXX.md#template-parameter-bases) uses: an omitted argument takes its default, a file-local (anonymous-namespace) holder is the unit's own, and a base named through an alias of an instantiation (`using Mix = H<M>; struct M : Mix`) is that instantiation. Such a holder's spelling keeps its arguments in the receiver's type, as a template whose members return a parameter does; an alias of the instantiation (`using InfoCallback = Callback<Info>;`) still names the template's class, so an out-of-line `InfoCallback::Format` is `Callback::Format`. The declared overloads are not consulted: the standard's const twins (`shared_ptr<T> f()`, `shared_ptr<const T> f() const`) disagree on the result, and `<memory>` need not be in the tree. `auto` locals, init-captures (`[self = shared_from_this()]`) and call receivers (`shared_from_this()->Run()`) are typed through it;
- the result is `Copy { dst: result, src: object address }`: wrapper variables hold their pointee's address, so the result may alias `this`, the pointer an arrow or `*p` reads through, a reference's or reference member's referent (qualified spellings included: `n::r`, `Owner::r`), a named object's or non-reference object member's address (`AddrOfVar`, and the field's `GepField` location, an instance field a member body names bare included: `m.shared_from_this()` as `this->m`; whether or not a `shared_ptr` owns the object, as ownership is not checked), or what a reference-returning call yields (loaded through, as any such receiver is). `return shared_from_this();` returns the object (`ReturnFlow::Copy`), and the copy is emitted wherever a call result lands, as for a promotion. A weak result carries the object into a later `.lock()` wherever the promotion is recognized.

The call site itself is recorded as before, so the call edge to the external `shared_from_this`/`weak_from_this` remains.

This is a may-alias relation. It does not check that a `shared_ptr` owns the object: on an unowned object `shared_from_this()` throws `std::bad_weak_ptr` (C++17 and later) and `weak_from_this()` returns an empty `weak_ptr`, and neither outcome is modeled; the result still aliases the object. Expiry, ownership counts, the held class's constness and the C++17 availability of `weak_from_this` are not modeled.

### `dlsym` / `GetProcAddress`

Built-in models treat `dlsym` / `dlvsym` / `GetProcAddress` as **symbol lookup**: the return destination of a call (the `CallSite.return_dst` of `f = dlsym(...)`, including `return dlsym(...)` via a temp) may point to every **in-tree** function whose exact name matches a string constant in the name argument (parameter 1). Out-of-tree names add no pointees (true external). The handle / DSO path is ignored. Lookup searches the caller’s link target and its incorporated dependencies when link metadata exists, otherwise the whole program. Dynamically loaded targets absent from that dependency closure are not modeled. Non-literal names that never receive a string constant stay unresolved — they do **not** fan out to every exported function.
