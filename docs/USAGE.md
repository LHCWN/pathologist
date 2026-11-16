# User-facing analysis features

Queries and options built on the analysis results: value slices, sequencing, dependency roots, compilation databases, conditional exploration and dataflow presentation. Part of the [analysis documentation](ANALYSIS.md).

## Value slice (`inspect slice`)

`trace inspect <DB> slice --file F --line L --col C` asks where a value
comes from, where it goes, and which execution contexts run the code that
moves it: a bounded two-stage walk over `flow_edges` and the
[memory accesses](POINTERS.md#memory-access-edges) (`trace-db/src/inspect/slice.rs`), annotated with the
[execution contexts](MODELS.md#execution-contexts). It is an adviser query (#197): it
lays out evidence for a person or a tool (an LLM) to judge, and is not a race
report. This section is the authoritative statement of its rules; the CLI
reference is in [README.md](../README.md#trace-inspect-slice).

It reads an analysis export's flow graph, the operation sites of
`flow_origins` (every position it shows: [Where a value moves](POINTERS.md#where-a-value-moves)),
`flow_memory_access` and `execution_contexts`. Under the v7 capability contract
([schema](SQLITE_SCHEMA.md#version-and-capability-contract)) it checks the
tables and columns it reads, refusing a database without them (an earlier v7
export: re-run `trace analyze`), and refuses a `trace-merge` output, whose
flow tables hold no flow graph, as source-level `inspect dataflow` does.

### Start

- **A declaration.** The candidates are `inspect dataflow`'s declaration
  lookup (`find_symbols_at`). When the identifier at the column is the name a
  variable on that line declares, the slice starts at the variable: its node
  and its own storage location, not the cells of its fields. The declaration
  must occupy that identifier: read by tokens from its recorded column (the
  declarator's start; a named parameter's identifier), the identifier comes
  before anything that ends a declarator's prefix. Only declarator
  punctuation (`*`, `&`, `(`, `...`), type, qualifier and scope names (`::`),
  a template argument list and an attribute (`__attribute__((...))`,
  `[[...]]`, `alignas(...)`, `__declspec(...)`) may lie between, whatever
  those lists hold. So `fns` in `int (*fns[2])(int)` and `at` in
  `Cb *__attribute__((unused)) at` are declarations, while in
  `int *p = obj->p;` the second `p` is not, nor is an identifier inside a
  template argument list, an attribute or an array size. A readable column on no identifier (the `*`
  or `=` of `int *p = q;`) declares nothing: the line is then searched as a
  use, below. Without the source line, the column must fall inside the
  declared name as recorded. Columns count characters, as
  LineMap's do, and an identifier may hold characters outside ASCII
  (`größe`). Parameter twins widen as in `inspect dataflow`.
- **A use.** Otherwise the operations recorded on that line (its
  `flow_origins` rows, with the cells of its loads and stores) and the
  values its calls pass (the actuals of its `flow_calls` rows, by the call
  site's line: `p` in `sink(p);`, a line with no operation of its own; the
  formal a call passes to is never spelled on the line, and does not join)
  and the variables its indirect calls go through (`call_sites.callee_var`:
  `cb` in `cb();`, which moves nothing else, so a callback is traced from
  its invocation as from its declaration) are searched for the identifier. Each operation and call has a span:
  from its recorded line and column over the expression recorded with it,
  which keeps its newlines, so an operation written across lines (`p =`
  with `q;` on the next line, a call whose arguments run on) is found from
  a position on any of its lines. When any span covers the position, only
  the operations and calls whose span does are searched, so in
  `a->p = x; b->p = y;` each `p` starts at its own class's cells, and `y`
  in `sink(y)` is told from the `y` an operation beside it moves; when none
  does (an identifier a macro invocation spells outside the expansion's
  text), every operation and call on the line is. A field access
  starts at the cells the line reads or writes for that field: the `field`
  and `field_summary` locations of that name. A variable starts at the
  variable; but a variable of that name declared on the line, which the
  identifier does not name there (`t` in `std::thread t(Run); t.join();`),
  goes before other variables: a use of the name on its declaration's line
  is most likely that variable, and not, say, a same-named parameter of a
  callee the line passes a value to. When a field and a variable go by the
  name (`s->p = p;`), the spelling decides: an identifier that `.` or `->`
  precedes, whitespace aside (`s->p`, `s -> p`, `(*s).p`), is the field
  access, any other (`p`, `S::p`) the variable, each falling back to the
  other when its own has no candidate (`callback_ = cb;` in a member
  function: no variable, so the member's cells). Without the spelling (the
  file not readable here, or the column on no identifier), the field's
  cells go first.
- **The identifier** is read from the source file at the path the database
  records, from each file `--file` matches that has a declaration or an
  operation on that line (each read once). When that file is not readable
  from here, `--name` gives it. Without either (or with the column on no
  identifier), the line's operations must move exactly one name,
  temporaries (`variables.is_synthetic`) aside: that variable, or that
  field's cells, is the start; a line moving
  several is an error that lists them and asks for `--name`. A position
  whose line moves no pointer or function value through that name is an
  error.
- **One file.** `--file` is a literal substring of the recorded path (`_`
  and `%` are no wildcards; `/` and `\` are interchangeable), as in every
  `inspect` query. On Windows an absolute existing spelling is canonicalized,
  so an 8.3 short name expands to the stored long path. When it matches
  several files and more than one of them has a declaration or a use at the
  position, the query is refused with the files listed: give more of the
  path.
- **Globals, statics and static data members.** A start like any other, and
  expanded both ways although they are boundaries anywhere else in a slice:
  every write the graph records is a predecessor and every read a successor,
  whichever function makes it. A static data member is the variable
  `Class::member`, so `current_` and `Registry::current_` both name it. A
  slice from a widely used global can be large; the depth limits bound it.

### Stage 1 (up): sources

Breadth first from the start, following edges backwards, at most
`--up-depth` edges deep ([Stop rules](#stop-rules)):

| Edge | Followed |
|------|----------|
| `copy`, `call_arg`, `unwrap`, `dlsym`, `mem_read`, `mem_write` | against its direction: the value at the source becomes the value at the destination |
| `points_to` | both ways: a variable and its own storage hold one value |
| `addr_of` into the node | not past it: the address is the source, an `allocation` for a heap location, else `function`, `string` or `address`; the location is not expanded through this edge |
| `gep` into the node | not past it: a `field_address` computed from the base pointer, which is the source node and is not expanded through this edge |

Recording an `addr_of` or `gep` source does not mark it expanded: a value
edge reaching the same node on another path expands it as usual, so in
`p = &base->field; p = (int *)base;` both `base` (`field_address`) and the
`&object` that sets it are sources, whichever path the walk finds first.
| `load`, `store` | no: what a load reads arrives over `mem_read` |

`nullptr` reached is a source of kind `null` and is not expanded: it is one
`constant` node for the whole program
([schema](SQLITE_SCHEMA.md#flow_nodes)), whose value goes to every null
assignment, so stage 2 does not walk on from it.

A node a `load` leads into whose access is recorded without a cell is an
`unrecorded_load` source, whatever else it receives: the load's pointer
reaches more cells than the fan-out cap of
[Memory access edges](POINTERS.md#memory-access-edges), or no memory at all. The `load` edge is in the slice to show where; its pointer is not
expanded. The node's other incoming edges are followed as usual.

A node no followed edge leads into is a source too: `parameter` (an entry
parameter: nothing passes it a value, as for an IPC handler's arguments), a
`global`, `unrecorded_load` (above), or
`unknown` (a value from a callee nothing models, or that no pointer flow
writes). A node's edge into itself (a recursive call passing a parameter
on) brings no value from elsewhere and does not keep it from being a
source. Nor does the `points_to` pair of a variable and its own storage
(present when its address is taken): the two hold one value, so they are a
source when nothing outside them feeds either (a value edge from elsewhere,
an `addr_of`, `gep` or `load` into the storage). The source is the
variable, of its kind (an address-taken entry parameter is a `parameter`),
and stage 2 starts from it. A boundary reached is a source of kind `global` or
`field_summary`.

### Stage 2 (down): sinks

Breadth first from the start and from every source that is not a
boundary, following edges forwards, at most `--down-depth` edges
from the nearest root: an `addr_of` source through its `addr_of` edges (the
address goes everywhere it is taken), a field address from the `gep`
destination, any other source from itself. The value edges are followed in
their direction and `points_to` both ways. A node is expanded again
whenever a walk reaches it at a smaller depth than before, so its successors
get the extra depth budget: a return released only once its call is entered
(below) can arrive after a longer path reached the node. The start's walk,
whose returns are not restricted (below), covers a source's walk at the same
depth or deeper; a node a source's walk expanded is expanded again when the
start's walk reaches it. A node is queued at most once per walk at a time:
an arrival of a walk while its entry waits lowers that entry's depth, and
the start's walk arriving at a waiting source's entry's depth or less takes
that entry over, at its own depth, as it covers it. Each walk keeps its own
depth: the start's walk arriving deeper than a waiting source's entry gets
no budget from it. Depths only decrease, so the walk ends; nodes past the
limit are judged against the finished walk, so `truncated_down` holds only
when one of them stays unreached.

From a source, a value leaves a function through its return value (an edge
from one function's variable into a non-parameter variable of another) only
along the calls stage 1 came up through, and back into a function whose
call the walk entered (a value passed from one function's variable to
another's parameter): a pass-through function (`T *Id(T *x) { return x; }`)
hands the value back to the call it came in through, and not to its other
callers. Return and entry edges are recognized by their ends, not by
`flow_return_calls` and `flow_calls`: those record
only returns and arguments of a call a call edge resolves, and miss, for
example, the copy out of a template factory's `Create()` into each caller.
A lambda reading or writing a variable it captured (`copy = local;` or
`local = other;` in a lambda written in the function that declares `local`,
which the lambda's body names as that variable itself) is neither: nothing
is called, so the edge is followed like one within a function. Such an
access is a move between a lambda and the function it is written in, at a
site in the lambda's body; a return out of the lambda (`other = make();`)
is written in the enclosing function's body and is a return like any, so
the value of an allocating lambda returns only along the calls stage 1
came up through. A lambda is known by its name,
`{owner}::$lambda{line}:{col}` after the function whose body it is written
in; a move at no site, or at a site with no single innermost function (a
one-line lambda), is taken for a return.
Which call entered is per pair of functions, not per call site,
and a call counts as entered only once the walk follows its entry edge: an
entry edge at a node the depth limit stops does not release a return
waiting for it, and the stage is then `truncated_down`.
Each call of an allocating wrapper
returns its own object, which the context-insensitive heap merges into one
(`OsalMemCalloc`'s `malloc() storage` returns to all its callers); without
this, a slice whose value came from such a wrapper spreads to every other
caller's object. From the start, returns reach every caller: a getter of a
member hands the one object to all of them. The uses of a value are leaves: a
`load` or `gep` out of the node (dereferenced to read, or to address a
field), a `store` into it (dereferenced to write) and `terminates` (cleared
by a [`clears`](MODELS.md#terminators-clears) model). A node is a sink when it is used
that way, when no value edge leaves it, when it is a call target or a
terminator, or when it is a boundary reached here.

### Stop rules

- **Depth.** `--up-depth` bounds stage 1 and `--down-depth` stage 2, 6
  edges each by default (`SliceOptions::default()`); `truncated_up` /
  `truncated_down` say a node at the limit had more to follow (in stage 2,
  including a call entry that would have released a waiting return).
- **Returns from a source** are restricted as [Stage 2](#stage-2-down-sinks)
  states.
- **Boundaries.** A field summary (`field_summary`) and a global or static
  (`global`, `file_static` and `fn_static` variables and locations) are in
  the slice, marked `boundary`, but never expanded unless they are the start:
  a summary merges every instance of a field and a global every function that
  touches it, so expanding them would pull in unrelated code. An instance
  cell (`field`) is expanded.

### Contexts

Which contexts reach a function is computed at query time from `call_edges`
and `execution_contexts` (the callers of each function a slice reaches are
read as it is reached, transitively, not the whole call graph): every
context whose entry reaches the function over
ordinary call edges, and `root` when code nothing calls reaches it (`main`, a
framework callback the index does not see called, an exported API, a test).
That code is a strongly connected component of the ordinary-call graph that
no edge from outside enters and that holds no context's entry: a function no
ordinary edge reaches, or a recursion nothing outside it calls. A context's start edge
is not ordinary (the started code runs in the new context, not the
starter's), and neither is an IPC bridge (the request comes from another
process). A function called from `main` and from an IPC handler is in both.

Nor is a call into the entry of a context no call site starts (an `entry`
model's member or override, an IPC stub handler): that entry is reached by
its own contexts only. The context stands for the framework's dispatch,
and in-tree code that calls the entry is taken to be that dispatch:
c_utils' `Thread::ThreadStart`, started by `pthread_create` in
`Thread::Start`, calls the `Worker::Run` override, which would otherwise be
in the `pthread_create` context as well as its own, two contexts for one
thread; a stub's `OnRemoteRequest` switch calls its handlers, which would
otherwise also be `root`'s code. A memory cell such an entry alone touches
is then not `contexts_differ`. The cost is a direct call from elsewhere (a
service calling its own handler, a test calling `Run`): its contexts do not
reach the entry or what the entry calls through it. The entry's callers
keep their own contexts, and a callback started at a call site keeps its
ordinary callers' contexts: those calls run it on the caller's thread.

Each node is `private` (a local, a parameter or a temporary, a local's or
parameter's own storage, or a field cell of that storage, nested or not:
one invocation's), `shared` (heap objects and their field cells, field
summaries, globals and statics, and anything a pointer parameter points to) or `constant` (function and string
addresses, `nullptr`). A private node's contexts are its function's. A shared node's
are those of the slice's statements that touch it: the function of each
edge's site by the edge-scope rule of
[Where a value moves](POINTERS.md#where-a-value-moves) (a statement in a lambda task's
body is the lambda's, so it runs in the task's context), or for an edge
without a site (or at a site with no single innermost function, such as a
one-line lambda) the function of its private end. So they depend on the slice: they are the contexts of the
accesses it shows.

### Cross-context edges

An edge is `cross_context` when the value may change hands between code that
different contexts, or two instances of one, run at once. The reasons:

- `contexts_differ`: the edge touches a shared node that the slice shows
  touched from code of two or more contexts (`root` counts as one), and code
  of some context makes this access.
- `self_concurrent`: the edge touches a shared node from code a
  self-concurrent context reaches (an IPC handler, or a context with
  multi-instance evidence; see [Execution contexts](MODELS.md#execution-contexts)).
- `start`: the edge moves a value between private nodes of two functions,
  and the second is the entry of a context the first starts: the value is
  handed to the new thread or task (an argument `pthread_create` forwards,
  `this` in `std::thread(&C::Run, this)`).

A value in private storage otherwise stays on its thread, so an edge between
private nodes is flagged only at a start, or when a local's storage (or a
field cell of it), or a variable a lambda captured, is touched from a
function with other contexts than its owner's: the owner's code from the
lambda's, or the lambda's from the owner's
(`contexts_differ`). An edge with a constant end is never flagged. A flag is a
hint, not a proven race: locks, ordering, joins and object lifetimes are not
modelled, and context sets over-approximate.

### Output

`--format text` (the default) is for people; `--format json` is one document
for tools (the graph formats are rejected):

| Field | Content |
|-------|---------|
| `start` | `name`, `kind` (`variable` or `field`), `at` (`path`, `line`, `col`), `nodes` |
| `up_depth`, `down_depth`, `truncated_up`, `truncated_down` | the bounds and whether each stage hit them |
| `contexts` | each context a node is reached by: `id` (`C<execution_contexts.id>` or `root`), `kind`, `entry`, `start_site`, `api`, `multi_instance`, `self_concurrent` |
| `nodes` | `id` (`flow_nodes.id`), `kind`, `label`, `var_kind`, `loc_kind`, `function`, `decl`, `sharing`, `roles` (`start`, `source`, `sink`, `boundary`), `source` (its kind), `stages` (`up`, `down`), `contexts` |
| `edges` | one per `flow_edges` row and site (its distinct `flow_origins` positions, or none): `from`, `to`, `kind` (`flow_edges.kind`), `stages`, `site` (`path`, `line`, `col`), `function` (the site's), `cross_context`, `reasons` |
| `limits` | what the slice does not show, below |

Nodes, edges and contexts are sorted, so the output is the same for the same
database. The text form lists the contexts, sources, sinks, each stage's
edges with their positions and flags, each node's contexts, and the limits.

### Limits

Both formats state them:

- Scalar values are not tracked: the slice follows pointer and function
  values, so integers and plain data are not in it, and a pointer's scalar
  uses (`if (cb_ != nullptr)`) record nothing.
- A flag is a hint, not a proven race (above).
- The analysis is flow- and context-insensitive and a field summary merges
  every instance of a field, so a slice can join flows that never happen
  together.
- A boundary's other writers and readers are not followed.
- An access through a pointer past the fan-out cap is not
  followed ([Memory access edges](POINTERS.md#memory-access-edges)): a load's value is
  an `unrecorded_load` source, and a store's destination memory is not in
  the slice.

Also not shown: a member call through a variable (`p->f()`) is not a use of
`p` (receivers are identity, [Memory access edges](POINTERS.md#memory-access-edges)),
and a container's elements accessed through member calls (`v.push_back(x)`,
`v.size()`, a range-for over a field) are not value flow; a whole-container
copy is. Measurements and the camera race-fix retro-test:
[Value slice: #205](EVAL_REPORT.md#value-slice-205--2026-10-09).

## Noise macro filtering (`--ignore-macro`, `--ignore-logging`, `[noise]`)

Repetitive diagnostic and logging macros (such as OpenHarmony `HILOG_*`, `TAG_LOG*`, or vendor logging frameworks) expand into boilerplate call sites, intermediate temporary strings, and helper invocations (`__builtin_strrchr`, `std::string::c_str`, format string conversions) that can dominate call-graph edges and points-to sets without contributing meaningful domain logic.

To suppress this noise during analysis, trace provides opt-in macro filtering during AST lowering:

- **CLI `--ignore-macro <NAME>`**: Ignore expansions of the named macro (repeatable). Glob wildcards (`*`) are supported (e.g. `--ignore-macro 'LOG*'`).
- **CLI `--ignore-logging`**: Pre-configured preset targeting common OpenHarmony and standard logging macros (`HILOG_*`, `TAG_LOG*`, `HIVIEW_LOG*`, `MEDIA_*_LOG`, `LOGD`, `LOGI`, `LOGW`, `LOGE`, `LOGF`).
- **Models TOML `[noise]` section**: In any file passed to `--models <FILE>`, a `[noise]` table can specify macros to ignore:
  ```toml
  [noise]
  macros = ["LOG", "HILOG_*", "TAG_LOG*"]
  ```
- **C API**: Configured via `trace_index_options.ignore_macros` and `trace_index_options.n_ignore_macros`.

### Mechanism

1. **Preprocessing & LineMap attribution**: During preprocessing, emitted tokens carry the name of their outermost macro invocation. When token slices are committed to the `LineMap`, macro identifiers are interned and preserved across AST lowering and disk-spill caching (`IndexCache`). See [Macro expansion provenance and attribution](PREPROCESSOR.md#macro-expansion-provenance-and-attribution) in the preprocessor specification for the exact provenance contract (outermost attribution, argument forwarding, builtin/CLI fallback definitions, and cache replay).
2. **Lowering filter**: During AST lowering (`walk_function_body`), statements, declarations, call expressions, return statements, and local variables whose source tokens expand from an ignored macro are discarded:
   - No call sites or call-graph edges are emitted.
   - No local variables or storage locations are allocated.
   - No flow constraints or return flows are generated.
   - Call sites and variable line numbers in surrounding code are preserved exactly.
3. **Auditability**: Configured ignored macro patterns are stored in `Program.ignored_macros` and exported into the SQLite database under `analysis_run.options_json` in the `"ignored_macros"` array.

## Dependency roots

A repeatable `--dep <root>` names a tree the target builds against but that is
not the code under analysis: a vendored SDK, a framework checkout, a sibling
repository. The distinction it draws is between what a tree **declares** and
what it **defines**. A dependency's declarations are needed — without the
class definition behind `sptr<T>`, a receiver spelled `sptr<CaptureSession>`
has no members to look up — while its definitions are noise: indexing them
inflates the call graph with edges inside code nobody asked about, and costs
the time to parse it.

### What is discovered

- **Headers** under a dependency root are discovered and preprocessed like
  project headers. Each header's own directory joins the include path, as does
  the root itself, so a rooted spelling (`<subdir/foo.h>`) resolves.
- **Sources** (`.c`, `.cpp`, `.cc`, `.cxx`) under a dependency root are never
  translation units, even when the root sits inside the analyzed tree.
- **Unreached headers** under a dependency root are not indexed as standalone
  orphan units. A dependency contributes what the target actually includes.

A dependency root nested *inside* the analysis root is supported, as the second
bullet says. One that contains or equals the analysis root is rejected at
startup: it would classify every discovered source as a dependency and leave no
translation units, so `--dep` must name a subtree that excludes the code under
analysis.

### What a dependency contributes

Types, typedefs, struct/class definitions, inheritance edges, `final` markers,
prototypes, parameter and return types, and declared `operator->` returns —
everything a call site needs to type its receiver and name its callee.

A body written in a dependency header contributes nothing beyond its signature.
Whatever the mode a unit merges under, a function whose span lies in a
dependency file is stored as a declaration: `is_defined = false`, no body line
range, no locals (including function-local statics), no call sites, no value-flow
constraints and no return flows. Lowering stops after the signature, before
walking the body or constructor-initializer list. Dependency variable declarations
retain their types but their initializers are not lowered. This prevents assignments
to parameters or globals from leaking through the merge and avoids allocating body
IR that would only be discarded. A header's own unit merges as a preamble for the
same reason. Classification uses the original LineMap file, including cached
expansions; target headers reached through dependencies retain their bodies.

### Receiver typing through a wrapper

This is what the declarations buy. Given `sptr<T>` declared in a dependency
header with `T *operator->() const`, the recorded arrow return says the class
returns a pointer to the template parameter in position 0, so a call site can
substitute from the instantiation's arguments. `infer_static_class` unwraps
`sptr<CaptureSession>` to `CaptureSession`, and
`session->AddOutput(...)` resolves to `CaptureSession::AddOutput` instead of
falling back to an external edge named after the wrapper (`sptr::AddOutput`),
which corresponds to no function at all.

### Attribution

`FileInfo::is_dep` is decided once, when a path is interned, and every later
question is an O(1) file lookup. Export carries it through to `files.is_dep`
and `functions.is_dep` (schema v4), `analysis_run.options_json` records
`dep_roots`, and `trace inspect calls --exclude-deps` drops the edges whose
caller or callee is a dependency function.

## Compilation databases (#62)

Inferred target ownership and authoritative metadata precedence are defined
in [Link targets and weak symbols](ANALYSIS.md#link-targets-and-weak-symbols).

Indexing opportunistically reads `compile_commands.json` at the analysis root,
then `build/compile_commands.json`. `--compile-commands PATH` selects another file;
the library equivalent is `PreprocessOptions::compilation_database`. The first
discovered database wins. A `--compile-commands PATH` naming no file is a
mistyped flag and fails the run; everything else degrades. Invalid JSON, invalid
entries and unreadable databases produce `compile_commands` diagnostics and fall
back to inferred configuration for sources with no usable entry. A database is
never required.

`--system-includes` opts into compiler search-path discovery; the library
equivalent is `PreprocessOptions::with_system_includes(true)`. With a database,
trace probes each command's driver from that command's working directory and
caches equivalent results. Sources lacking a usable command and orphan project
headers use one fallback compiler for the project. GCC is preferred when it
supplies both C and C++ search lists; otherwise Clang is tried. Reached
external system headers have dependency ownership. Probe flag and executable
path rules are specified in [Compilation database options](PREPROCESSOR.md#compilation-database-options-62).
Bare-tree indexing can instead lower the compiler-default standard headers once,
as one shared prelude, with `--canonical-system-headers`: project headers then
follow the default sharing rules and translation units never hold standard
text. See [Canonical system-header prelude](CANONICAL_SYSTEM_PRELUDE.md) for
its precision tradeoffs and measurements.
For C++ dependency headers, the parse input approximates simple
pointer-to-member types (`T C::*`) as ordinary pointers (`T *`). This spelling
confuses the tree-sitter C++ grammar, while dependency bodies are
declaration-only. The normalization preserves byte offsets and newlines, so
LineMap locations still refer to the original source. Project-origin text is
left intact by this pass; the parenthesized member-function-pointer shape is
handled for project and dependency text alike by the general pass described
in [C++ parse-input normalization](CXX.md#c-parse-input-normalization), which the
dependency pass leaves to it. Other C++ grammar gaps may still produce parse
diagnostics.

The probe also supplies selected target and hosted-mode macros to the custom
preprocessor; explicit database macro operations and CLI definitions retain
their override order.
See [Compilation database options](PREPROCESSOR.md#compilation-database-options-62)
for search order and probe limits.

When neither an explicit JSON database nor either automatically discovered JSON
path is selected, indexing reads `compile_flags.txt` only in the analysis root
(or the source's parent directory for a single-file analysis). It does not search
ancestors, subdirectories, or `build/` for flags. A selected JSON database always
wins, even if empty, unreadable, malformed, or missing usable source entries;
flags never supplement it or change its error handling.

With shared flags, `--system-includes` uses the project fallback compiler because
the flags file does not name a compiler driver. The probe receives the parsed
shared `-isystem` directories, including CLI overrides, so the compiler's search
list retains them in order. Shared `-iquote` directories remain in the quoted
include search class; relative paths resolve from the flags file's directory.

The flags file supplies one literal argument per line, with LF or CRLF endings.
Compiler names, source filenames, and other positional arguments are rejected;
separate operands to recognized flags remain valid. Progress logs identify this
configuration as `compile_flags`.
Empty and whitespace-only lines are ignored; all spaces in nonempty arguments
are preserved, with no shell splitting, unquoting, expansion, response-file
support, or command execution.
For example, `-Iinclude space` is one argument; `-include` and `forced header.h`
occupy two lines. Relative paths resolve from the flags file's directory.
An MSVC `/link` (or `-link`) line ends the compilation flags; it and all
following linker arguments are ignored during analysis.
The existing command argument parser applies include paths, ordered `-D`/`-U`,
forced includes, and language/standard switches to every discovered translation
unit and to project headers not consumed by any unit. The orphan-header pass
reuses the shared language-specific configuration and validation cache, with
the usual header language inference and CLI override precedence. This also
applies to projects containing only headers. Shared configuration errors are
reported once across source and orphan-header passes.
Source discovery and dependency exclusions remain unchanged; flags cannot
introduce additional sources. Language defaults to each source's extension unless
overridden by language/standard options. CLI include and macro precedence is the
same as for JSON commands. Unreadable flags, invalid text, and rejected arguments
produce `compile_commands` configuration diagnostics naming `compile_flags.txt`
and fall back to inferred configuration without aborting analysis. Macro operations
from the flags file are validated with the preprocessor, including function-like
definitions and undefines; a malformed operation discards the entire shared
configuration for the source. Caller-supplied `command_macros` retain their usual
preprocessing diagnostics and do not invalidate the flags file. Explicit language
selections are checked against the standard for the effective source language.
Unknown non-preprocessing switches retain the existing argument parser's behavior.

The JSON reader accepts `arguments` or a shell-quoted `command`, preferring `arguments`
when both exist. Leading compiler launchers (`ccache`, `sccache`, `distcc`,
`distcc-pump`, `gomacc`, `icecc`, `icerun`, `buildcache`), including chains of
them, are skipped so the real driver decides the default language. Launcher and
driver names are matched on the executable stem, case-insensitively and with any
`.exe` suffix removed, so a database produced on Windows resolves the same way. A
launcher also matches with a version suffix (`ccache-4.10`); a wrapper merely
named after one (`ccache-clang++`) is the driver itself and is not skipped.
Command strings use POSIX quoting with one departure: outside quotes a backslash
escapes only a quote or a space, so the separators in a Windows path survive
while `\"` in a define still works. Inside double quotes it also pairs with a
following backslash, so a value ending in `\\` keeps its backslash instead of
swallowing the closing quote. No shell, compiler or build command is executed.
Entry paths are resolved against `directory`; a relative `directory` is resolved
against the database's directory. Existing sources inside
the analysis root are eligible, including nonstandard extensions selected by `-x`.
Sources outside that root or below `--dep` roots are excluded.

Each command contributes ordered `-D`/`-U` operations and `-include` headers.
For `cl` and `clang-cl` drivers (also `--driver-mode=cl`), the corresponding
case-sensitive MSVC switches `/I`, `/external:I`, `/D`, `/U`, `/FI`, `/TC`, `/TP`,
`/Tc`, `/Tp`, and `/std:` are recognized, including attached and separate operands
where applicable and their `-` spellings. `/TC` and `/TP` apply globally; `/Tc`
and `/Tp` select the named source. `/link` ends compiler-option parsing. Known
output-file operands are skipped. C++ standards set `_MSVC_LANG`; `cl` keeps
`__cplusplus=199711L` unless `/Zc:__cplusplus` enables the standard value.
MSVC standard options do not introduce `__STRICT_ANSI__`. Compiler-version and
platform predefines and implicit SDK include paths are not inferred; supply
needed defines and directories explicitly. This is preprocessing-option support,
not full compiler emulation.

Include directories are canonicalized and deduplicated within each search class
without changing their order. Source-cache-only headers participate in the same
search as filesystem headers.
Quoted includes search the including file's directory, then ordered `-iquote`
directories, ordered `-I` directories, ordered `-isystem` directories and finally
the `-idirafter` chain. Angle includes skip the first two classes. A directory
also marked `-isystem` keeps its system position. Explicit CLI include paths precede database `-I` paths; CLI
defines override command macros. Configured units do not use inferred include
directories or basename guessing. A bare-tree unit searches its inferred tree
directories after every explicit class, so a CLI `-I` or `-isystem` directory
wins over a same-named header elsewhere in the tree (before #120 the inferred
directories were ordinary `-I` entries and preceded system directories). Forced includes first search the command's
working directory and preserve their own source locations; a header they cannot
find is reported against the source, never against the synthetic search path.

For GCC-style commands, `-std` applies to the whole command, including when it follows the source. MSVC `/std:` does not change the source language; a standard for the other language is ignored.
`-x` remains positional: only following input files use that language. Target
flags such as `-xhost` and `-x86-asm-syntax=intel` are ignored as non-language flags.
Both single- and double-dash `imacros`/`include-pch` spellings are diagnosed as
unsupported. Attached output flags (`-oFILE`, `-MFFILE`, `-MTTARGET`, `-MQTARGET`)
are ignored without consuming the next argument.

`-x` at the source argument selects both lexer and tree-sitter grammar; otherwise
the file extension applies (a `++` driver selects C++). `-std` supports C90 through
C23 and C++98 through C++26, with GNU dialects and common historical aliases.
It sets `__STDC_VERSION__` or `__cplusplus`, and `__STRICT_ANSI__` for strict
dialects. A standard outside that set costs only those macros: the entry keeps
its include paths and macro operations, since the list ages out as compilers
gain standards and discarding the command would leave the source with no
configuration at all. A standard that contradicts the source language still
skips the entry. Syntax recovery remains the existing parser/preprocessor subset; this
is not full compiler dialect emulation. Vendor/target builtins, sysroot rewriting,
response files, PCH files and `-imacros` are not modeled. Response files, PCH,
`-imacros`, unsupported languages and obsolete `-I-` cause an entry to
be skipped with a diagnostic; unrelated compilation/output flags are ignored.
`-idirafter` and `-iwithprefix` append to that last chain, which is searched after
every `-isystem` directory whatever their argument position; `-iwithprefixbefore`
joins the `-I` chain instead. `-iprefix` supplies the prefix the two `-iwithprefix`
forms concatenate, textually and without inserting a separator.
`arguments` is recommended for databases produced on Windows.

All commands for each source are indexed in database order, independently of
`--explore` and its budget. Sources without entries use inferred options. When a
database contributes commands, each configuration expands headers inline with a
separate source cache and macro environment; raw file contents may be shared.
The shared header warm-up path remains in use when no usable database commands
exist. This trades some header parsing time for isolation between explicit
include configurations. Unreached project headers retain standalone indexing;
dependency headers contribute declarations only.

Function sharing follows [Shared header functions](ANALYSIS.md#shared-header-functions).
Configured sources and unconsumed configured headers use the shared ordered
indexing scheduler with the previous source-family count window. Ordinary
sources and orphan headers with settled source text use a byte budget;
admission limits are defined
in [Parsing memory limits](PERFORMANCE_REVIEW.md#6-make-parsing-memory-limits-independent-of-cpu-count). A
configuration family's variant-preserving merge consumes one completed unit at
a time, keeping deduplication state across sources; it never resets that state
between commands or files. Each source's own command/exploration results occupy
one count slot regardless of command or variant count. Explicit isolated
link targets retain their completed units for
weak-selection passes and reuse across images. See
[preprocessor cache storage](PREPROCESSOR.md#role-in-the-pipeline) for the separate
retained source-payload budget.

The complete set of configured units uses the variant-preserving merge, so a
shared header's differing bodies survive across both commands for one source and
commands for different sources. Source IDs, locals, call facts, flow constraints
and returns are remapped and unioned. Variable identity is scoped to the source
a unit came from: a file-scope `static` in a shared header is a distinct object
in every translation unit that includes it, and the configurations of one source
dedup against each other only. `variants_merged` counts configurations
beyond the first for each source, including exploratory variants when enabled.
No SQLite schema change is needed. Global `defines` metadata continues to describe
user overrides; include paths are the union of observed configuration paths,
not a replacement for per-command search order.

Regression coverage: `crates/trace-cli/tests/it/compile_commands_tests.rs`,
`crates/trace-cli/tests/it/compile_flags_tests.rs`, and the corresponding
`tests/fixtures/compile_commands/` and `tests/fixtures/compile_flags/` fixtures. Format and search semantics follow the
[Clang database specification](https://clang.llvm.org/docs/JSONCompilationDatabase.html)
and [GCC directory options](https://gcc.gnu.org/onlinedocs/gcc/Directory-Options.html).

## Bounded conditional-variant exploration (`--explore`)

A single build-free configuration necessarily omits implementations hidden behind
conditional compilation arms (`#if`, `#ifdef`, `#elif`) that are excluded by default
options (#57, #59). Concatenating alternate arms into a single stream would produce
syntactically and semantically invalid code (redefined variables, mutually exclusive
headers, broken scopes).

`--explore` (bounded by `--explore-budget <N>`, default 4) recovers excluded platform
and feature implementations by exploring feasible configuration variants independently:

1. **Candidate define discovery**: In-tree GN files (`BUILD.gn`, `*.gni`, `*.gn`) are
   scanned for candidate macro definitions (`defines = [...]`, `defines += [...]`)
   along with their values and confidence ranking (#58).
   The scan walks the tree in sorted order and breaks equal-confidence ties on the
   candidate's source path, so the winning value never depends on `read_dir` order.
   Hidden entries and `target` are excluded using filename bytes; a non-UTF-8
   directory name does not suppress candidate discovery.
2. **Semantic feasibility evaluation**: Rather than naive textual inclusion, each
   conditional chain arm that was skipped during base preprocessing is checked for
   activation against candidate defines. Expressions like `#if MODE == 1` vs
   `#elif MODE == 2` or `#ifdef BINDER_CATCHER_ENABLE` are evaluated semantically using
   `trace_preproc::preprocess_string`, seeded with the run's base defines so a condition
   such as `#if FOO && !BAR` is judged against the configuration the variant builds on.
   Synthetic predicate runs disable LineMap tracking; actual indexing retains it.
   Repeated macro reads in the chain prefix and already-discovered goals are skipped
   before activation lookup. Base bindings are cloned directly into the options
   without constructing a temporary map.
   `#elif` activation also requires every preceding arm in its chain to be false.
   A macro the base configuration already fixes is never a candidate: a different value
   conflicts, and the same value would re-lower the base as a "variant".
3. **Greedy compatibility grouping**: Compatible activation goals are clustered into
   variants (subsets of candidate defines that can hold simultaneously) up to the
   configured explore budget. Chains are identified by `(file, line)` — a translation
   unit spans every header it expands, so the line alone collides. Different arms of one
   chain are mutually exclusive and force separate variants; two defines that open the
   *same* arm (`#if defined(A) || defined(B)`) are not in conflict and share one variant,
   provided their combined definitions keep every targeted condition true. The combined
   predicates are checked in one preprocessor run per proposed grouping, including macro
   aliases in base definitions. This separates `A && !B` from `B` even across unrelated
   chains. An arm inside a
   region an enclosing chain excluded (`Unevaluated`, C11 6.10.1p6) cannot be opened by
   its own macro alone, so it is ranked behind every arm a define really does open and
   claims only budget nothing else wanted. Goals that cannot fit within the budget
   are counted in a diagnostic with `stage: "explore"` and `severity: Warning`.
   The count describes omitted candidate activation goals, not distinct feasible
   configurations or proven reachable arms.
4. **Independent preprocessing and lowering**: Each feasible variant is preprocessed and
   lowered into an isolated `UnitIndex`. Translation unit variants use a frozen header
   expansion cache, and header variants that depend on newly activated defines miss the
   cache cleanly and expand locally without polluting the shared cache.
5. **Variant-aware merge (`merge_unit_variants`)**: a unit and its variants are merged
   together, so duplicate flow constraints are dropped against a set scoped to that one
   unit's contribution rather than to the whole program. When merging variant units into
   the global `Program`:
   - Function bodies sharing `(file, name, line)` with previously merged variants are
     **preserved rather than discarded**: parameter and local variable IDs are remapped
     and extended, call sites are deduplicated and merged, and IR flow constraints and
     return flows are unioned. Calls at the same source location retain separate records
     when their remapped arguments, callee, receiver, or return destination differ; only
     identical call facts are deduplicated. This preserves macro-selected callbacks.
   - A variant body the base spells on a **different line** — the `#ifdef X / #else`
     alternative-implementation shape — extends the base entry, found by file and name.
     Registering it as a second definition would make the symbol table treat it as a
     redeclaration and overwrite the surviving definition's span and parameters with the
     variant's, dropping the call sites bound to them. Every defined overload remains
     a candidate; a unique matching signature selects the corresponding base body.
     Ambiguous matches retain the ordinary merge path. The base entry must agree on **signature** --
     parameter count and types: a configuration that *adds* an overload (`pick(int)`
     always, `pick(double)` under a define) likewise lands on a line the base never had,
     and folding it into the base would conflate two functions' parameters and call
     targets, with the result depending on which source enabled the overload. Parameter
     types are compared by *shape* rather than by id — two configurations intern their
     own copy of a type, so an id comparison would split a function from itself — with
     tags compared by name (the configurations may legitimately carry different field
     sets, which is what the layout union reconciles) and an unresolved type never
     splitting anything. The check applies to C++ only: C has no overloading, so arms that
     differ in arity because the directive wraps the whole declaration are still one
     function. Alternative arms of one function agree on their signature; a
     variant that widens a signature in place keeps the function's own line and matches
     before this fallback is reached.
   - Locals are recorded on their function by the merge, since lowering tracks scope in
     its own map and leaves the field empty. Synthesized temporaries are paired by
     position — the k-th temporary of a kind at a source position, the kind being the
     variable's `Variable::temp` (`TempKind`), never read off its name, so a declared
     local spelled like one (`_ret1`) is never taken for one; a temporary that exists only when
     something resolves (a `_recv` receiver, a `$new` or `$make` heap object) is a kind of
     its own, so a configuration lacking it shifts no other kind's pairing. A factory's
     object (`$make`) is shared per function and class, so it pairs by that
     identity without the first call's file/line/column. A configuration that
     omits an earlier same-class factory changes the object's recorded position
     but must not allocate a second object or duplicate a later constructor
     edge. It exists only where its class has a user-provided constructor:
     where one configuration gives `A` a
     constructor and another does not, `make_shared<A>()`'s object never pairs with the
     `make_shared<B>()` object beside it in one macro expansion. `new`'s (`$new`) exists
     whenever the expression does and pairs by position alone, whether or not the
     configuration indexes its class — because lowering
     names them after the unit-local id it just allocated, so two configurations of the
     same expression never agree on the name. The count is kept per incoming body: one
     unit can carry the same shared header body twice (a cached header expansion and its
     own copy), and the second copy's k-th temporary is the first copy's k-th, or each call
     in the body is recorded twice (#127 review).
     This ordinal rule assumes a stable order of same-kind temporaries at each source
     position. If a redefined macro inserts or removes an allocation ahead of another
     at the same expansion site, different allocations can pair across configurations.
     This precision limitation also applies to file-scope temporaries.
   - Parameters are paired with the base signature **by name, never by position**. A
     variant routinely inserts a parameter ahead of the ones the base has (`#ifdef
     DEBUG_LOG` file/line pairs); positional pairing would map the variant's first
     parameter onto an unrelated one and hand it every value that parameter holds.
   - A parameter that exists only in a variant is recorded as a **local**. The canonical
     arity belongs to the base configuration, and the solver reads `params.len()` as the
     arity of an indirect-call target: growing it would stop base call edges from
     resolving, making variant data cost baseline precision.
   - File-scope variables (`FileStatic` and `Global`) sharing origin location and name are
     paired with the base configuration's variables, unifying initializers, stores, and
     reads across variants rather than duplicating them into disconnected entities.
     File-scope synthesized temporaries instead pair by source scope, file/line/column,
     and `Variable::temp`, never by name, with a separate occurrence count per key in
     each incoming configuration.
     Multiple temporaries of one kind at the same position keep separate ordinals,
     so an unchanged initializer's heap object is shared across configurations while
     distinct allocations at that site retain their own ordinals, subject to the
     ordering limitation above. Declared file-scope variables continue to pair by name.
   - Aggregate layouts (`struct` and `union`) are unioned across configurations: fields
     are matched by name; variant-specific fields not present in the base layout are
     appended with unique `FieldId`s (`union_struct_layout`, `union_union_layout`).
     Anonymous members, which share the empty name, are matched on their type as well,
     so a variant's second unnamed member is not swallowed by its first.
6. **GEP field resolution by name**: Because unioning aggregate layouts across variants
   may shift positional field indices across configurations, the pointer-analysis solver
   guards GEP field resolution: when a positional `field` does not match the GEP's
   `expected_name`, it resolves the field via
   `program.types.field_id_by_name(parent_type, expected)` instead of rejecting the
   pointee. The gate is `Program::layouts_unioned` — set whenever a unit merges as a
   variant, and so whenever a layout could have been unioned — and not the `--explore`
   flag: a run that asks for exploration and generates no variant unioned no layout, and
   relaxing the cross-struct `FieldId` guard there would only let unrelated structs
   through as indirect-call targets. So baseline solver behavior is preserved exactly
   whenever no unit merges as a variant, `--explore` or not. It is deliberately *not*
   `variants_merged > 0`: that field counts exploration variants per source and is 0 for
   an ordinary compilation database, where every unit past the first still merges as a
   variant and unions its layouts — so keying on it left the recovery off exactly where
   commands disagree about a struct.

### Limits of `--explore`

- **Conditional function signatures are not modeled separately.** The base signature
  remains canonical. A variant-only parameter is retained as a local, but calls still
  wire arguments by the canonical positions and arity. Added or reordered parameters
  can therefore lose variant argument flows or route them to a different parameter.
  Analyze explicit configurations separately when these signatures matter.
- **Search is bounded and heuristic.** Goals start from individually activating GN
  candidates. Conditions needing several new defines together and nested parent/child
  activation are not exhaustively searched; a lack of budget warnings does not imply
  complete configuration coverage.
- **Overloaded definitions in one file keep line-keyed variant merging.** When several
  defined functions in a file share a name, the file/name pair does not identify one of
  them, so a variant body on a different line is not recognized as that function's other
  implementation and is merged as its own definition.

- **A unioned layout may match no single configuration.** Fields a variant adds are
  appended, so when a configuration inserts a member in the *middle* of a struct, the
  merged layout holds every field but reproduces neither configuration's offsets. Field
  identity (and therefore the analysis) is unaffected; the offsets and sizes exported to
  SQLite are the approximation.
- **Aggregates that embed a unioned struct by value keep stale offsets.** `TypeDesc`
  nests member descriptions by value, so growing `struct Inner` does not reach a
  `struct Outer { struct Inner i; int x; }` interned earlier: `x`'s exported offset and
  `Outer`'s size still describe the smaller `Inner`. Again `FieldId`s, and the analysis,
  are unaffected.
- **Feasibility is an over-approximation.** An arm's condition is evaluated against the
  command-line defines plus the candidate, not the full macro environment its headers
  build up. A variant that turns out infeasible costs a budget slot but cannot invent
  facts: the real preprocessor run still decides what that variant's text is.
- **Declaration-only rows move.** A function the index only ever sees called is
  anchored at the call site that first references it. Exploring variants opens earlier
  code, so that first reference moves and the row is re-anchored: on `drivers_hdf_core`
  29 such rows sit at a different line, or in a different file, than in the baseline.
  All of them carry `is_defined = 0` and all are still present. No *defined* function
  the baseline records is lost on hdf or hiview; on camera three shift between two
  files that define the same symbol, which is the pre-existing twin-definition dedup
  rather than anything exploration does.

## Call sequencing (control-flow sidecar)

The call graph says which functions a call site may reach; it does not say
whether one call happens before another. `trace analyze --sequencing` (off by
default) records, beside the unchanged call-graph facts, a compact control-flow
graph per function body whose nodes carry the body's call occurrences. A query
(`trace inspect sequence`) later rebuilds bounded partial sequences from the
database alone, loading only the functions it reaches. The sidecar is never
read by the PAG or the solver: `call_edges`, points-to and `arg_flow` are the
same with the flag on or off, and the analysis stays flow-insensitive
([Properties](ANALYSIS.md#properties)). With the flag off nothing is recorded and the
database carries no sequencing rows.

### Events and what "before" means

- **Event.** One syntactic invocation in a function body: one
  `CallOccurrence` (spelling span, expansion span, expansion-chain id; see
  [Call source locations](ANALYSIS.md#call-source-locations)). An event lists the
  `CallSite` records of that invocation as its `sites`: alternative targets of
  the *same* invocation (one record per member candidate, post-merge virtual
  overrides, a configuration variant's own record), never a sequence of
  calls. An automatic object whose implicit constructor or destructor runs
  its bases' (`struct C : A, B {}; C c;`) records one site per base class at
  the declaration; each base's is an event of its own (`A::A` and `B::B`),
  its overloads alternatives of that one. The constructor a smart-pointer
  factory runs ([Factory construction](CXX.md#factory-construction)) shares the
  factory call's position but is no alternative target of it: its sites are
  marked `exact_receiver` (the object is exactly `T`), so
  `std::make_shared<S>(arg())` is two events, `std::make_shared` and
  `S::S`, both after `arg` and `unordered` with each other (the constructor
  runs inside the factory, whose body is not in the tree). Kinds: `call`, `construct` (including the constructor a
  `new`-expression runs), `destroy` and `delete`. Two expansions of one macro
  are two events, even when their displayed coordinates match; so is an
  argument a macro substitutes twice (`TWICE(step())`), whose two sites share
  one source position.
- **"A before B"** compares the *invocations* of A and B, not their
  completions. Every guarantee is conditional on B executing and on every call
  between A and B completing normally (no `throw`, `longjmp`, `exit` or
  noreturn call); the query states these conditions with its answer. Relation
  strengths:
  - `sequenced_before`: same block (the blocks of one chain of `sequenced`
    edges count as one; see [Representation](#representation)), not on a
    cycle, A precedes B in the block's local order; `guaranteed_before`: A's block dominates B's block
    over every edge, exception edges included, B's block does not reach A's,
    B's block is reached from the entry without an exception edge, and no gap
    other than an `unordered_branch` gap covers either event (that kind only
    unorders its own events; see `unordered` below). When A's block has an `exception` edge, A must also
    precede every other event of its block: in
    `try { a(); b(); } catch (...) { h(); } c();` `a` may throw before `b`
    runs and the handler fall through to `c`, so `b` only `may_precede` `c`,
    while `a` (whose own throw still invoked it) is `guaranteed_before` `c`.
    A block reached only through a handler is never claimed (`a` to `h` is
    `may_precede`). Both are proofs *under the conditions above*.
  - `may_precede`, `may_follow`, `may_both` (a loop): a control-flow path
    exists. This is not feasibility: correlated branch predicates are not
    reasoned about. Two calls of one block on a cycle are `may_both`
    whatever the block's local order (`while (more()) { a(); b(); }`: one
    iteration's `b` precedes the next one's `a`).
  - `exclusive`: no path either way (`if (ok) send(); else recover();`).
    `unordered`: same block without a local order, or both held by one
    `unordered_branch` gap with a relation that names a direction
    (`sequenced_before`, `guaranteed_before`, `may_precede` or `may_follow`):
    in `sink(cond() ? left() : right(), other())` `left` and `other` are
    `unordered` both ways whichever argument the builder walked first, while
    `left` and `right` stay `exclusive`. Events of two different gaps keep
    their order: after `sink(c1() ? a() : b(), x()); sink(c2() ? d() : e(),
    y());` `c1` is `guaranteed_before` `c2`. `unknown`: a gap or boundary
    lies between them.
  - **Unmodeled transfers.** An `unsupported` edge (computed or unresolved
    `goto`) or an event under a gap that may hide control flow
    (`parse_error`, `depth_exceeded`, `unsupported_construct`,
    `unresolved_goto`: a `longjmp`, a statement expression) may lead
    anywhere; a `setjmp` may be returned to from any later call. A missing
    edge is never evidence of impossibility: when A's block, or a block it
    reaches, may leave that way, or A follows such an event, B may follow A
    (and the same the other way), so `a(); goto *p; resume: b();` relates
    `a` to `b` as `may_precede`, and the arms of `if (setjmp(env))` as
    `may_both`; the answer notes the unmodeled transfer. A proof
    (`guaranteed_before`) also needs every block that may leave that way to
    be dominated by A's (in A's own block, every hiding event to follow A),
    so no such jump enters B's block without A: in
    `if (x) goto *p; a(); skip: b();` `a` only `may_precede` `b`. An end
    under such a gap is `unknown`. Such a gap with no call under it (a
    `goto` inside a statement expression, a call-free `ERROR` subtree)
    leaves nothing to mark where: the builder ends the block at it with an
    `unsupported` edge, beside a fallthrough to the block the walk goes on
    in, so `if (x) { a(); ({ goto t; 0; }); } else { t: b(); }` relates `a`
    to `b` as `may_precede`, not `exclusive`.

### Expressions

Events of an expression are ordered post-order: a node's own events follow its
children's, so an argument's calls precede the call they are passed to.

- Call arguments form an **unordered group**. In C++17 and later the callee
  and receiver expression is sequenced before the arguments; in C and earlier
  C++ it joins the group.
- `&&`, `||` and `?:` branch (short-circuit); the comma operator is sequenced.
- C `_Generic` evaluates exactly one association and never its controlling
  expression: each association is an arm of its own (a `switch_case` edge
  from the current block, all arms joining after), and the controlling
  expression's calls are one unordered group under an `unevaluated` gap that
  orders nothing after it. So in
  `_Generic(selector(), int: chosen(), default: other()); after();`
  `chosen` and `other` are `exclusive`, each `may_precede` `after`, and
  `selector` is `unknown` to every other call.
- <a id="unevaluated-operands"></a>**Unevaluated operands.** Lowering
  (`unevaluated_operand` in `trace-parse/src/lower.rs`) tags the calls of an
  operand the language never evaluates `CallSite::unevaluated`: the operand
  of `noexcept(e)`, the requirements of a `requires` expression, the
  controlling expression of `_Generic`, and the operand of `sizeof`,
  `alignof`, `decltype` and GNU `typeof`/`__typeof__` when it is a call.
  Those four do not evaluate their operand unless its type is variably
  modified (C11 6.5.3.4p2; GNU `typeof` likewise), and lowering does not
  know types: a call's result is never variably modified (parentheses
  aside: C++ `sizeof(f())`, C `sizeof f()`; C parses `sizeof(f())` and
  `typeof(f())` as a function *type*, which calls nothing). A tagged call
  never runs, so the solver gives it no call edge, no argument or return
  flow and no model effect; it is part of the merge identity, so an
  evaluated copy of the same site in another unit keeps its edges. The
  operator keyword itself, which the grammars parse as a call
  (`noexcept(e)` and `typeid(e)` in C++, `__typeof__(e)` with an expression
  operand in C), is no call site at all. In the call-order graph a tagged
  call is kept the way `_Generic`'s controlling expression is, under an
  `unevaluated` gap ordering nothing after it, wherever it is written
  (member and default member initializers too); the query relates it
  `unknown`, never counts it as a call reaching the other end, and does not
  inline it (an `unevaluated` boundary). A type operand
  (`sizeof(int[n()])` calls `n` for its array bound) and any other
  expression (`sizeof(rows[f()])` over a two-dimensional variable-length
  array) may be evaluated: they stay ordinary calls, and like `typeid(e)`,
  whose operand is evaluated when it is a polymorphic glvalue, the operand
  is a `branch_true` arm beside a `branch_false` bypass, so its calls
  `may_precede` what follows.
- Assignment: right before left in C++17 and later, unordered otherwise.
  `<<`, `>>` and subscript are left before right in C++17 and later. Operands
  of other binary and unary operators are unordered.
- A lambda body is its own function's graph; init-captures are events of the
  enclosing function at the lambda.

The language standard is the unit's (`-std=` from the compilation database,
otherwise the default).

### Statements

Compound statements, `if`/`else`, `while`, `do`, `for` (init once; condition;
body; update; back edge to the condition; `continue` goes to the update),
range-for (range expression once; header; body; back edge), `switch` (case
edges, fallthrough edges, `default`, and an edge past the switch when there is
no `default`), `break`, `continue`, `return` (edge to the exit), `goto` and
labels (labels are resolved before the body is walked), `asm goto` (a
`switch_case` edge to each label it names, through a block destroying what
the jump leaves, and a fallthrough past it; a label the body lacks adds an
`unresolved_goto` gap and an `unsupported` edge, so in
`asm goto("jmp %l0" : : : : out); a(); out: b();` `a` only `may_precede`
`b`; any other `asm` transfers nothing), `try`/`catch` and `throw`.

- **Automatic objects** ([Automatic objects](CXX.md#automatic-objects)): the
  constructor event sits at its declarator, so `T a{f()}, b{g()};` orders
  `f`, `T::T`, `g`, `T::T`. The one destructor site the IR records per
  object is referenced from every exit of its scope: the normal end and each
  `return`, `break`, `continue`, `goto` or `throw` leaving it, in reverse
  declaration order. The sites of one object's bases (one per base class
  providing the member) are one unordered group, for construction and
  destruction alike: the IR does not record which bases are virtual (built
  first), so their order is not claimed. A `goto` back to a label the walk has passed also
  destroys the objects the label's scope declared after the label
  (`again: before(); Guard g; if (retry()) goto again;` runs `Guard::~Guard`
  on the jump), after those of the scopes it leaves; a forward `goto` cannot
  skip a declaration in valid C++ and destroys only what it leaves. An `if`
  or `switch` condition's object is destroyed where the statement ends, a
  `for` init-statement's where the loop ends. A `while` condition's object
  lives for one iteration: the end of the body and `continue` reach a block
  destroying it before the back edge to the next evaluation, and a false
  condition or `break` destroy it at the loop's exit, so in
  `while (T t = next()) work();` one iteration's `T::~T` precedes the next
  one's `next` (the pinned grammar parses no declaration as a `for`
  condition). No call site is added for this.
- **Once-only locals.** The initializer of a block-scope `static` (the
  declarations lowering classifies `FnStatic`, `storage_for`) or thread-storage
  (`thread_local`, `_Thread_local`, `__thread`) variable runs on the first
  pass only. Each such declarator whose initializer or construction holds a
  call is a `branch_true` arm beside a `branch_false` bypass, both joining
  after it, so in `void f() { static S s; after(); }` and
  `static int x = init(); after();` the initializer `may_precede` `after`
  instead of being `sequenced_before` it. The guard is not a call; no site
  is added for it. Such objects have no destructor event (they are destroyed
  at exit).
- **Exceptions.** Each `try` has one `landing` block. Every block of the
  `try` body that holds an event, nested handlers included, has an
  `exception` edge to it, except a block of a nested `try` body: that block
  throws to the nested landing, which passes on what its handlers do not
  catch. An exception leaving automatic objects destroys them on the way:
  when objects of the scopes inside the `try` are live at a call, the
  block's `exception` edge goes to the cleanup block of the latest of them,
  which holds that object's destructor events and has an `exception` edge
  on to the cleanup block of the object declared before it, and so on
  (innermost first, in reverse declaration order, as a `return` destroys
  them) to the landing. A call that leaves fewer objects for the same
  landing enters the same chain further down, so there is one cleanup
  block per object and landing, never one per set of live objects: a body
  of n objects stores O(n) cleanup events. A block whose live objects
  change between its calls (`Guard g;` after a call) has an edge for each
  set. An object counts once its constructor (its whole initializer) has
  run, in every declaration form: in `if (G b = G())` and
  `while (G d = G())` as in `G b = G();`, `G::G` throws straight to the
  landing, never through a copy of `~G` for the object it failed to build.
  It no longer counts once its destructor starts, so a destructor's own
  event never throws through a copy of itself. The cleanup events are
  additional copies of the objects' destructor sites (one position, see
  [Query](#query)). So in
  `try { Guard g; may_throw(); later(); } catch (...) { h(); }`
  `later` only `may_precede` `Guard::~Guard`, which `may_precede` `h`. A
  destructor run on the way out of a scope normally (its end, `return`,
  `break`, `continue`, `goto`) may throw like any call; one run while an
  exception propagates (a `throw`'s own destructors, a cleanup block's)
  takes no `exception` edge, since a destructor throwing then calls
  `std::terminate`. Nor does a call that is never evaluated
  ([unevaluated operands](#unevaluated-operands), `CallSite::unevaluated`). The
  landing has an edge to each handler. Unless a handler is
  `catch (...)` (its parameter list is the ellipsis alone: a pack expansion
  inside a typed handler, `catch (Fault<Ts...> &)`, does not count), the landing also has an `exception` edge to the
  enclosing landing or the function's exceptional exit, through the cleanup
  chain of the objects live around the `try` statement, once a path from
  the entry reaches the landing (a function-try-block's always, its
  initializers may throw): a `try` whose body cannot throw, or throws only
  from code after a `return` or `break`, passes nothing on and builds no
  cleanup, so in `G g; w(); try { n = 1; } catch (E &) {} k();` `w`
  stays `guaranteed_before` and `k` `sequenced_before` `~G`. A cleanup block no path reaches holds
  no events and is removed with the empty blocks. So in
  `try { try { a(); } catch (...) {} } catch (...) { c(); }` `a` and `c`
  are exclusive, while a typed inner `catch`, or a call in the inner
  handler, may reach `c`. A constructor's function-try-block
  (`A::A() try : m_(f()) { ... } catch (...) { ... }`) holds the prologue:
  its member initializers (and default member initializers) run in a block
  of the `try`, before the body, with an `exception` edge to its landing. A `throw` destroys the objects it leaves in
  its own block, then has an `exception` edge to the innermost landing or
  to the exceptional exit. A
  call *outside* a `try` has no exceptional edge: the conditions above
  already exclude abnormal completion.
- Loops stay cycles (`back_edge`), never unrolled.
- Member functions of a local class and GNU nested functions run when called,
  not where they are written: their control flow is not followed, and any
  events lowering attributes to the enclosing function there form one
  unordered group under an `unsupported_construct` gap.

### Representation

Per function body and configuration variant, `FunctionCfg`
(`trace-ir/src/cfg.rs`) in `Program::cfgs` (ordered by function id):

- blocks (`entry`, `exit`, `exceptional_exit`, `ordinary`, `landing`) with
  their events in evaluation order, and typed successor edges
  (`fallthrough`, `branch_true`, `branch_false`, `switch_case`, `back_edge`,
  `return`, `exception`, `unsupported`, `sequenced`);
- the **immediate** sequenced-before pairs between events of one block (a
  DAG); events of a block with no path between them are unordered. A
  `sequenced` edge splits one block's local order in two: it is the only way
  into its target, and every event of its source is sequenced before every
  event of its target. The builder adds one before a group that starts each
  of its members after the latest events (an unordered group, the base
  destructors of one object) when more than one event is latest, so in
  `a() + b(); c() + d();` the second statement follows the first through
  one edge rather than a pair per two calls. A query treats the blocks of
  such a chain as one block;
- gaps: regions whose order is not modeled.

Storage stays linear in blocks + edges + events + sites: no transitive
before-pairs, expanded callee bodies or enumerated paths are stored, and a
group never restores more than one latest event before each member (a wider
frontier gets a `sequenced` edge first), so a body stores at most about two
order pairs per event. The exception is where one block must hold a whole
group: inside an operand of an unordered group (its siblings start in the
block it starts in) and in a constructor's member-initializer list (one
`initializer_order` gap). There a sequenced step after an unordered one
(the second operand of a comma, an initializer-list element, the next
member initializer) stores a pair per two calls of the two steps, as in
`f((a() + b(), c() + d()), e())`. After a body is built, event-free blocks
that only fall through to their successor are removed
(`FunctionCfg::compact`; a `sequenced` edge into one becomes a
`fallthrough`); branch, loop, exit and landing structure is kept.

The export writes `cfg_functions`, `cfg_blocks`, `cfg_edges`, `cfg_events`,
`cfg_event_sites`, `cfg_event_order` and `cfg_gaps` in both export modes
([schema](SQLITE_SCHEMA.md#call-sequencing-tables)), and retains every call
site an event names even when the ordinary
[call-site filter](SQLITE_SCHEMA.md#call-site-export-filter) would drop it (a
direct site without call edges or argument flow).
`analysis_run.options_json.call_sequencing` records whether the capability
was enabled ([contract](SQLITE_SCHEMA.md#version-and-capability-contract)); a database without it reports so and names the flag to
regenerate it.

### Merge

CFGs are built per translation unit and remapped with the unit
(`merge_unit`): the function through the unit's function map, every event's
sites through its call-site map, spans through the file map; `tu` records the
unit. A body the merge does not keep has no CFG: a dropped duplicate body, a
dependency-root body, a declaration and a weak body suppressed by a strong one
in its link target.

- A shared-header body merged from a second unit is dropped when its remapped
  CFG is structurally equal to the one kept ("first body is kept", see
  [Shared header functions](ANALYSIS.md#shared-header-functions)). A body another unit
  compiles differently, or a `--explore` variant that differs, is kept as a
  separate **variant** of the function. Variants are never unioned: a query
  shows them side by side, so no sequence mixes two configurations.
- An event whose sites the merge does not keep (a call expanded from a
  dependency's macro body) is spliced out of its block and local order, and an
  `omitted_sites` gap records that it was there.
- Post-merge virtual expansion adds override records as further sites of the
  same event (`Program::attach_alternative_sites`).

`trace-merge` carries the graphs of its sequenced inputs the same way
(`trace_ir::add_variant` and `FunctionCfg::remap` are shared): function,
call-site and file ids are remapped, and row ids renumbered. A body several
inputs share (a header function the merger unifies) keeps one graph per
structurally distinct body, in input order: an input compiling it as an
earlier one did adds nothing, one compiling it differently adds a variant.
Repeated original call sites coalesce according to the
[stable identity contract](ANALYSIS.md#stable-merged-call-edge-ids). For separately
analysed copies of a header call, the comparison takes a site as its caller,
position, callee spelling and directness (and how many such sites precede it
in its input). Sites spelled alike that are alternatives of one event of an
input (a merged input's copies of one header call) count as one site, so
merging a merged database again keys them as merging its inputs at once
does: no extra variant appears. A kept graph names its own input's sites first; each later
input compiling the body alike lists its sites, in input order, as further
alternatives of the matching events (`FunctionCfg::add_alternatives_from`,
through `CallEvent::add_alternative_sites`, as
`Program::attach_alternative_sites` adds override records), so the targets
its call edges reach are kept: two repositories passing
`inline void invoke(void (*cb)()) { cb(); }` different callbacks keep one
graph whose `cb()` event reaches both. An event none of
whose sites was carried is spliced out with an `omitted_sites` gap. The output's marker is
`true` when every input was sequenced, `false` when none was, and `"partial"`
otherwise, with one `MissingSequencing` diagnostic per input without graphs:
an unsequenced input's functions have no CFG, so a merged database never
claims more order than its inputs recorded. Schema side:
[Merger inputs and output](SQLITE_SCHEMA.md#merger-inputs-and-output).

### Boundaries and gaps

A gap or boundary is uncertainty, never evidence for an order.

- **Gaps** inside a body: `parse_error` (an `ERROR` subtree; its events are
  kept as an unordered group), `depth_exceeded`, `unsupported_construct`
  (computed `goto`, `setjmp`/`longjmp`, coroutines, SEH, statement
  expressions; a nonlocal jump is recognized by its callee spelled plain or
  qualified from the global namespace or `std`, so `::_setjmp` and
  `std::longjmp` count), `unresolved_goto`, `unordered_branch` (a short-circuit or
  conditional inside an unordered operand: the affected events are
  unordered), `initializer_order` (member initializers and default member
  initializers are one unordered group in a prologue block; declaration order
  is not modeled. A site of the function that no walked node owns joins that
  group too), `omitted_sites` and `unevaluated` (calls in an operand the
  language never evaluates, the controlling expression of `_Generic` and
  the [unevaluated operands](#unevaluated-operands) of `sizeof`,
  `alignof`, `decltype`, `typeof`, `noexcept` and `requires`: kept as events so a position names them; an
  end under one relates `unknown`, as the call never runs). A
  body with a gap is `partial`.
- **Boundaries** between functions, found by the query: `omitted_body` (the
  callee has no CFG: dependency, declaration only, suppressed or not
  sequenced), `unknown_callee` (a site without call edges), `unevaluated`
  (a call that [never runs](#unevaluated-operands)), `recursion`,
  `async_start` (the site starts an [execution context](MODELS.md#execution-contexts):
  the callback runs elsewhere and is not inlined) and `ipc_bridge` (an `ipc`
  edge). None establishes happens-before across threads or processes.

### Query

`trace inspect sequence` (`trace-db/src/inspect/sequence.rs`; options in the
[README](../README.md#trace-inspect-sequence)) checks the capability marker
first, then loads graphs only as it reaches them:

- **Starting point.** `--function` names a function as the other inspect
  commands do; with `--file`, only the variants compiled from (`tu_file_id`)
  or written in (a block's file) a matching file are shown. Units that each
  define one external function share its row, which records the first
  definition's file, so `--file` may name another unit's body. A position
  (`--file`/`--line`) names one call in every variant of its function that
  compiles it (one event per graph at one position, with one callee and one
  macro invocation position); the
  query starts from each such variant, one frame per variant as for
  `--function`. The destructor events of one automatic object on each way
  out of its scope (the end, `return`, `break`, `continue`, `goto`, `throw`,
  an exception's cleanup block, each iteration of a `while` with a
  condition object) name the same sites:
  they are copies of that one call, so the position names all of them. A
  position matching several calls (two events of one graph that are not
  such copies, as the two calls `TWICE(step())` makes, or calls of
  different functions or callees) is an error listing at most
  ten of them with the count of the rest; when they were matched at a macro
  body's spelling, each candidate names its invocation and the error names
  an invocation position to use instead: a macro invoked on different lines
  in different `--explore` variants is one call per invocation, not one
  call.
- **Frames.** Each graph shown is a frame: every variant of the starting
  function, then one frame per inlined (event, target variant) pair, breadth
  first in event order up to `--depth` call levels. A `call` edge joins the
  calling event's block to the frame's entry and a `return` edge joins its
  exit back to that block, so a return always goes to the call that made it.
  Blocks are listed in reverse postorder (branch arms before their join).
- **Boundaries.** A target already on the frame stack is `recursion`; a
  target without a graph is `omitted_body` (`dependency`, `declaration_only`
  or `not_sequenced`); an event under an `unevaluated` gap is
  `unevaluated` and inlines nothing; an event without call edges is
  `unknown_callee`; the
  callback an event starts as an execution context is `async_start` (inlined
  only with `--inline-contexts`); an `ipc` edge of the frame's function is
  `ipc_bridge` and is never inlined. `--max-targets`, `--max-frames` and
  `--max-events` cut inlining with a `truncated` marker naming the budget
  (Mermaid and Graphviz draw each as a dashed edge from the cut call's block
  to a note `t<index>` labelled `truncated: <budget>`);
  once `--max-frames` is reached nothing more is loaded or inlined, and its
  one marker names the first call it cut. The starting frames are always
  shown.
- **Relations.** Two events of one graph relate as in
  [Events and what "before" means](#events-and-what-before-means). When the second event lies in another function,
  the query looks for the events of the first event's function whose calls
  reach that graph within `--depth` levels, never through an `async_start` or
  `ipc_bridge`, and relates the first event to each of them (the call itself
  counts as `guaranteed_before`); several calls combine to the strongest
  claim every one of them allows: `unknown` if any is; a proof only when all
  are (`sequenced_before` when all are, otherwise `guaranteed_before`);
  `exclusive` or `unordered` only when all are; otherwise `may_precede`,
  `may_follow` or `may_both` by the orders the inputs admit, where an
  `unordered` input admits both orders, a proof A first, and `exclusive`
  neither. Every such call is cited: a note names their
  positions and the JSON `relation.via` lists their `cfg_events` ids (empty
  when both events share a graph). A first event several variants compile is related
  in each of them, and the relations combine the same way, as do the
  relations of every pair of destructor copies at either end (so in
  `while (T t = next()) work();` `T::~T` and `work` are `may_both`); a
  variant that reaches no copy of the second event is skipped with a note. Found
  nowhere, the relation is `unknown`. One `unordered_branch` gap holding
  both ends turns a directed relation (`sequenced_before`,
  `guaranteed_before`, `may_precede`, `may_follow`) into `unordered`; any
  other gap over either end turns `sequenced_before`/`guaranteed_before`
  into `unknown`, and a gap that may hide control flow over either end turns
  every relation into `unknown` (see "Unmodeled transfers" above). Between two
  events of one graph, only the blocks on paths from the first to the second
  (or to the calls reaching it) are shown.

### Sequencing limits

- Calls outside a `try` have no exceptional edge (see Exceptions above).
- Temporaries' constructors and destructors are not lowered, so they are not
  events.
- Member-initializer declaration order is not modeled (`initializer_order`).
- Paths are not checked for feasibility; `may_*` relations over-approximate.
  Constant conditions are not folded either: the `do { ... } while (0)` of a
  statement macro keeps its back edge, so two calls in it relate as
  `may_both`.
- A call spelled in a macro body shares its spelling position with every
  expansion of that macro (`DealFormat` in HDF's `HDF_LOG_WRAPPER`: 14,852
  events), so a query position there is ambiguous; the error suggests an
  invocation position to name instead.
- C API exposure of the query is a follow-up.

Measured indexing, storage and query cost:
[Call-order sidecar](EVAL_REPORT.md#call-order-sidecar-248--2026-10-27).

## Source-level dataflow presentation

`trace inspect dataflow` describes the exported **may-flow constraint graph**.
`trace_db::dataflow_graph` remains the raw PAG query API; the CLI uses
`dataflow_view` and `render_dataflow`. The C API's `trace_db_dataflow` calls
`dataflow_graph`: it returns raw PAG nodes/edges and counts raw edges for depth.
The CLI projects source entities and counts visible transitions after technical
nodes collapse. Selecting the same declaration and depth does not imply identical
graph representation, reachability at that depth, or truncation. The existing
C ABI is unchanged. Analysis and resolution are unchanged.

Source-level inspection requires an analysis export with the required metadata.
`trace-merge` output records its merge origin but omits PAG/provenance data, even
though shared DDL creates empty flow tables. Inspection reports that limitation
before symbol lookup; use an original analysis database or analyze a combined
source tree. An ordinary analysis with no origins is supported, and a selected
symbol without flow nodes retains its separate diagnostic. See the
[SQLite capability and merger contract](SQLITE_SCHEMA.md#version-and-capability-contract).

Dataflow JSON's `direction` and the output title use `down` (where values flow)
or `up` (where values come from), matching the `--direction` argument.
The title includes the selected variable's declaration path, line, and column;
these are its recorded source coordinates, even when selection falls back to a
nearby declaration.

Compatibility: `dataflow-source-v1` uses only `down` and `up` as its emitted
direction labels. The earlier, unversioned dataflow JSON and titles used
`flows-to` and `flows-from`; those labels are not part of this source-level
contract. Consumers must select the JSON contract by `schema` and, if they
also support the earlier output, map its `flows-to` to `down` and `flows-from`
to `up` in that legacy reader. Future incompatible direction or structure
changes require a new schema discriminator. Titles are presentation text;
machine consumers should read `direction` rather than parse the title.
The C API's `TRACE_DIRECTION_DOWN`/`TRACE_DIRECTION_UP` numeric enum and raw
graph API are unchanged. Rust callers of `render_dataflow` supply their own
`GraphMeta` and must use `down`/`up` for conforming v1 JSON. The shared renderer
also recognizes legacy labels for traversal scope ordering, preserving that
behavior for existing Rust callers without rewriting their supplied metadata.

Functions group by canonical `FnId`, source variables retain `VarId`, and
argument and return edges retain `CallSiteId` and argument edges retain the recorded zero-based argument index
(JSON; text and diagrams number positions from one). Call-site IDs remain
internal and are omitted from all output formats. Names are display text, never identity.
This preserves shadowing, overloads, namespaces, file-static ownership, link
targets, and existing shared-header/static-inline canonicalization. Only
entities and edges reached in the requested direction appear. Parameter-twin
fallback reuses the raw query's canonical-function rule.

The JSON document contains exactly `schema`, `title`, `direction`, `depth`,
`truncated`, `scopes`, `nodes`, and `edges`. `schema` is the stable discriminator
`"dataflow-source-v1"`, identifying the contract documented in this section;
incompatible changes require a new discriminator. It has no `root` field. `scopes` is an
object containing four arrays, always present, with unique entries sorted by
numeric ID. Empty arrays are `[]`:

| Array | Description fields | ID space |
|-------|--------------------|----------|
| `functions` | `id`, `name`, `signature`, `location` | Canonical `FnId` |
| `globals` | `id`, `name`, `location` | Reached global variable's `VarId` |
| `statics` | `id`, `name`, `location` | Reached file-level static variable's `VarId` |
| `values` | `id`, `name`, `location` | Reached value's `PagNodeId` |

Each node contains exactly `id`, `name`, `kind`, `scope`, `location`, `depth`, and
`callees`. Node IDs are numeric `PagNodeId`s. A scope reference contains `kind`
and a numeric `id`: `function`, `global`, `static`, or `value` refers to the
corresponding description array. Global and file-level static variables reference
their own descriptions. Locals, parameters, function-local statics, and other
nodes with a known function reference that function. A function-value node
references the function it represents. Nodes without a function or internal
`var_id` reference their own value description; unknown ownership is `null`.
`callees` retains the complete list of possible `FnId`s. Function descriptions
also include every function referenced by `callees`, an operation's `callee_id`,
or operation ownership, even when that function has no reached graph node.
Variable and value descriptions come from the completed graph; missing function
headers are loaded in batches without adding nodes or changing traversal depth.

Each edge contains exactly `from`, `to`, `scope`, `expression`, `location`,
and `operations`. `from` and `to` reference node IDs in source-to-destination
order in both traversal directions. Callee IDs appear only on operations.
Edge `scope` identifies the function performing the primary operation,
including a function writing a global or file-level static variable. Recorded
calls use `caller_fn_id`; other operations use the exact source path and the
innermost function definition whose inclusive line range holds the primary
location's line ([Where a value moves](POINTERS.md#where-a-value-moves)).
Operations outside functions, missing positions, or definitions with no single
innermost one use `null`. Destination-variable ownership never supplies operation ownership.

Each operation contains `kind`, `expression`, and `location`, plus `callee_id`
when known. Null callee IDs are omitted.
Argument operations also contain `arg_index`, using the original zero-based
argument index recorded before intermediate chains collapse. An unknown or
inapplicable argument index is omitted instead of serialized as `null`.
The operation's `callee_id`, when present, is the numeric `FnId` of its original
called function; it is retained separately for each argument or return operation
before collapse.
Function-pointer array initializer entries retain each element's original source
position and expression on the function-location-to-array `addr_of` edge, even
when the function definition is in another file. Widening an internal overload
reference retains those same occurrences on every candidate, for both array
entries and scalar function-pointer initializers. Widening unions each distinct
(candidate constraint, original constraint) pair in first-seen order, using
provenance snapshotted before any candidate receives new origins. Repeated
constraint occurrences retain their flow order without repeating the provenance
union; destination origins keep their original order and append only new sites.
Synthetic copies that wire an
indirect call target are classified as `resolve indirect call` in both operation
labels and provenance; they do not introduce a source assignment.
Collapsed edges retain multiple operations without introducing intermediate
nodes. Recorded provenance supplies expressions and locations, preserving
entries with distinct expressions, locations, argument indices, or callees and removing
only exact duplicates. Operations retain deterministic provenance order.
Kinds absent from recorded provenance are appended in the edge's existing
internal operation order with `expression: ""`, `location: {"path": "",
"line": 0, "col": 0}`, omitting both `callee_id` and `arg_index`. Primary edge
expressions and
locations retain their recorded attribution and output ordering; they are not
copied into missing operation provenance. All locations contain `path`, `line`,
and `col`; unknown coordinates remain zero.

For example, an argument transition has this shape:

```json
{
  "from": 701,
  "to": 702,
  "scope": { "kind": "function", "id": 31 },
  "expression": "dispatch(message)",
  "location": { "path": "/project/main.cpp", "line": 38, "col": 5 },
  "operations": [
    {
      "kind": "pass argument",
      "expression": "dispatch(message)",
      "location": { "path": "/project/main.cpp", "line": 38, "col": 5 },
      "callee_id": 32,
      "arg_index": 0
    }
  ]
}
```

JSON omits internal `target_id`, `spelling`, and `provenance`, node `fn_id`,
`var_id`, and `targets`, and edge `fn_id`, `callee_fn_id`, `callee_id`, and
edge-level `arg_index`. Internal graph metadata retains those identities, macro spelling,
provenance, complete targets, and the edge argument index for other formats.

Declaration lookup accepts positive, 1-based line and column positions only.
Named parameter positions point to their declarator identifiers, including
pointer, array, reference, and function-pointer declarators, rather than their
type spelling; unnamed parameters retain their declaration positions.
Within the substring-matched files and the ±2-line search window, identifier
coverage **on the requested line** ranks first (from the declaration column
through the column immediately after the identifier). Other declarations on
that same line rank next by absolute distance from their declaration columns.
Declarations on nearby lines rank last by line distance, then column distance;
column coverage on a different line is not an exact source-position match.
Equal distances tie by declaration column, full file path, line, and
`VarId`. Candidate notes count declarations on the selected file's requested
line separately from nearby/cross-file candidates and show file:line:column to
explain ambiguity. One selection note combines fallback and ambiguity information on stderr in all formats.

Generated intermediate variables are omitted from symbol selection and hints;
source variables remain selectable regardless of their names. Symbol lookup,
including the C API and raw query callers, requires `variables.is_synthetic`;
databases missing it are rejected with an instruction to re-run `trace analyze`.
The source view reuses that same compatibility check. Technical storage
nodes are projected onto the source variable. Intermediate chains collapse
while retaining branches and recorded operations. Hidden expansion is memoized
per visible origin, hidden endpoint, and source/call metadata state. At
reconvergence, operation and provenance facts are unioned in deterministic sorted
order; they describe may-flow reachability rather than enumerated paths.
Cycles converge through finite fact growth, independent of the visible-depth
limit. Distinct call/argument occurrences retain separate metadata states.
Source field-write destinations remain expression nodes rather than
vanishing when a synthetic destination has no successor. Field-write expression
visibility and naming use indexed incoming write provenance and field-access
metadata for encountered destinations, even when the incoming value is outside
the traversal. A query from the base of `s->field = p` can
therefore reach `s->field` through its GEP even when the incoming store from `p`
is outside the traversal. This does not add that store edge to the base's graph;
only reached nodes and transitions are shown, with the usual visible-depth limit.
Pointer-write destinations likewise use their recorded LHS and incoming
metadata for visibility, without adding an unreached store to the graph. Assignment destinations
retain the complete LHS for plain and compound assignments (including shift
assignments), without trailing operator characters: nested comparisons,
assignments, and quoted literals inside subscripts or receiver expressions do not delimit the outer assignment
(for example, `s->table[i == 0] = v` targets `s->table[i == 0]`). Comments
in recovered source slices retain their spelling but their punctuation does not
delimit the assignment or change bracket/quote nesting, including line comments
continued with escaped newlines. Assignment delimiter scanning uses the shared
C++ lexer so digit separators, prefixed character/string literals, raw-string
bodies and user-defined literal suffixes remain atomic tokens; destination text
is still sliced from the original expression. Constructor member initializers
record their own expression and original position on immediate and deferred
store constraints before projection. Like aggregate initializers, their field
destinations follow recorded field-access metadata (`s.f`, including
nested field paths). Both field-access endpoints use the same variable/storage
alias mapping as graph endpoints. Converging records select a deterministic
parent; projected self references do not contribute field paths. Aggregate origins record each element's own span and singleton
initializer text (`{ value }` or `{ .field = value }`), rather than copying the
whole aggregate for every constraint. Nested container GEPs use their field
designator or opening brace. This keeps provenance text storage proportional
to the emitted element facts and their spellings in both IR and SQLite;
deferred references retain the same element origin. Initializer text remains
operation provenance rather than being interpreted as an assignment LHS. Real intermediate
variables remain separate nodes. Function values, allocations, string constants,
indirect-call targets, and modeled clearing events remain visible.

Concrete field locations retain their analysis identity, including the parent
object location. Their labels show the full **abstract storage path** and node
ID, so differently nested locations sharing a root `VarId` and member name are
distinguishable. These paths describe PAG storage, not necessarily legal source
expressions. Field expansion retains root-variable ownership and uses the root
object layout for variable-rooted locations (`struct_type_for_loc`); a reached
field can therefore acquire further fields of that same root layout. The
existing instance-sensitive nesting cap is four; deeper accesses use summaries.
Exported `flow_field_locations` records each concrete parent/child relation,
including in minimal exports. Presentation never merges equal labels or alters
these analysis entities. Scope ownership follows the recorded parent chain,
so file-scope initializer storage stays under its global or file-static root,
while function-local storage retains its function. Without parent metadata,
functionless local temporaries use the file-scope global group. Function value
entities use their function's file and start line, with column zero because the
export has no function column; variable entities retain variable coordinates.

Operation expressions prefer original source slices when every mapped token is
outside macro expansion, belongs to the same source file, and the recovered
slice re-lexes to the same tokens. Mapped lexer columns count Unicode characters
and are converted to byte offsets within the original source line before slicing.
Original source caches retain line starts and lazily cache character-to-byte
offsets for each recovered non-ASCII line. ASCII files and cached ASCII lines
use columns directly as byte offsets. Repeated operations on a minified line
therefore do not repeatedly scan its prefix.
Original contents and line breaks are retained, including comments and spacing
after non-ASCII text.
Unavailable, changed, macro-expanded, or normalized input uses token-aware
formatting instead. Literals stay indivisible; re-lexing checks that formatting
preserves operator tokens rather than joining distinct operators or numbers.

Operation provenance is recorded at lowering, remapped with the existing TU
merge maps, and exported separately from the raw constraint tables. Repeated
source assignments can retain separate operation sites even when the solver
has one constraint for their endpoint pair. Lowering deduplicates each
constraint's origins using hash membership while appending new origins to the
ordered vector; repeated assignments do not rescan prior sites. Membership
tables store indexes into that append-only vector, comparing the complete span
and expression even on hash collisions. Zero/singleton lists need no table.
The first attribution of each constraint occurrence still wins, recorded by
a dense occurrence flag vector; the membership indexes are discarded with the
translation unit's lowering context. Original-source recovery buffers are
released after the AST walk, when deferred references already own their spelling. Deferred
function references retain the same closest operation span and expression by
the existing pending reference identity. End-of-unit resolution records those origins on
new address and store constraints, including macro expansion locations, so
field writes remain visible when their function value is defined later.
Merging repeated constraints from conditional variants or shared header bodies
unions their operation origins,
including when the constraint itself is deduplicated. Merge visits each
unit's origin list once per distinct constraint, using membership
indexes while retaining first-seen origin order across units and variants. The
constraint occurrences themselves retain their existing merge policy. Unknown or
synthetic provenance file IDs use the merge helper's primary-file fallback,
then deduplicate the remapped origins.
Complete expression storage is interned by text during lowering and across TU merge,
using `Arc<str>` for origin, call, and return-operation text. A new merged spelling
reuses the unit's allocation. Sharing never changes span or occurrence identity,
origin membership compares full span and text, and ordering still comes from
the existing vectors. The pool is a lookup index only; its hash iteration order
is never used for output. See [flow-release lifecycle](POINTERS.md#post-solve-flow-release-lifecycle)
for its lifetime and the facts retained through solving and export.
Origin vectors start with one entry of capacity. The analysis indexing path
compacts flow and origin buffers after the last merge; all facts remain alive
through their final consumer, including CLI and C API provenance export.
Optional call return-operation payloads are boxed so calls without a return
assignment do not reserve inline space for a span and expression.
Export visits each constraint's merged origins once, even when repeated lowering facts remain in
`program.flow`; expanded return wiring is likewise deduplicated before exporting
its provenance. Incoming return-wiring indexes cover only call-return destinations
and expire before edge-row export allocates its buffers. Call-argument metadata replaces only generic wiring without source provenance; a recorded assignment
between the same actual/formal endpoints remains a separate transition.
Argument presentation collects canonical endpoint pairs and removes generic
wiring in one pass before appending call occurrences, so repeated calls do not
rescan previously constructed transitions. Full call expressions are captured with
`operation_expression` at the call AST node and retained on every emitted call
site, including cloned virtual candidates. Explicit automatic-object construction
captures its declarator expression (such as `local(input)`); heap construction
captures the full `new` expression at its own emission path. Both preserve nested
calls' own text and the existing macro attribution. Additive optional v7
`flow_call_expressions(call_site_id, expression)` metadata exports that text once
per recorded call ID in both export modes. Argument transitions load it in the
batched occurrence query, placing the full call expression on the edge and its
argument operation before hidden-node collapse. Original non-macro whitespace
is retained; macro calls use the existing token-aware expression formatter and
keep the recorded invocation location. Nested calls retain their own expression,
and return-assignment expressions remain separate in `flow_call_origins`.
No source files are read or expressions reconstructed from callee names during
inspection. Older databases lacking the table, or sites without recorded source
expressions, keep an empty expression; re-analysis supplies available call text.
`release_flow` clears this per-call presentation metadata after export.
Return-value annotations identify existing PAG wiring from recorded return
facts and the shared, target-aware resolvers (indirect returns use resolved call
edges). Each root callee retains its own transitive return-source set, so an
indirect call with multiple candidates associates a source only with candidates
that can return it. Shared sources can still name multiple valid callees.
Export groups recorded calls by destination and callee and visits each
source/destination/callee wiring once before emitting its distinct call IDs.
Different callers' return-flow constraints do not replay the same occurrence
bucket; each `(src_node, dst_node, call_site_id, callee_fn_id)` tuple is exported
once, while each constraint retains its own source origins.
Return presentation indexes distinct canonical source/destination pairs and
source operation positions once and constructs one transition per exported call
occurrence, callee, and pair. New exports bind each return-assigning call's
closest AST operation in the additive `flow_call_origins` table, keyed by call
ID. This preserves separate expressions even when several assignments in one
macro invocation share endpoints and expansion coordinates. Cloned virtual
sites retain that operation; merge remaps its file with the safe helper and
`release_flow` clears it after export. Inspection looks it up by the indexed
call ID, without multiplying origins by calls. For older v7 exports without
this optional table, the closest preceding assignment in that source file
supplies expression/provenance via a position index; ambiguous same-invocation
assignments require re-analysis for exact attribution.
Repeated return origins do not multiply occurrence transitions; recorded
call-site IDs, locations, and macro invocation/spelling coordinates come from
the existing call-site records; assignment expressions and their source
coordinates remain in the transition's provenance. A call occurrence replaces
only origins matching its recorded or fallback source operation, including
location and expression, or its call-expression origin at the recorded call
position. Other origins sharing the endpoints remain visible,
including global initializers without an annotated call occurrence; no call ID
or callee identity is invented for them. Argument and return
occurrences whose caller is NULL use the call's file-scope global group, with
no invented function identity.
Missing provenance stays missing: declaration sites are never substituted for assignment
or call sites. Collapsed transitions retain their constituent operation sites in JSON.

Macro-generated functions use their recorded expansion-site function span.
Macro invocations are not scopes/functions. Calls use the recorded invocation
as the prominent location and retain spelling coordinates and call ID internally;
text and diagrams show macro spelling, while JSON omits it. Argument-spelled calls
keep their recorded location. The authoritative
[Call source locations](ANALYSIS.md#call-source-locations) rules also apply here.

Inspection first walks indexed neighborhoods with zero cost for hidden nodes
and one for visible entities, including the extra visible frontier needed to
detect truncation. It loads metadata only for that neighborhood and required
aliases, field parents, and function headers; incoming write metadata never adds unreachable store
edges. Argument, return, and target metadata are selected in batches. The raw
`dataflow_graph` API retains its separate raw-depth traversal behavior. Fresh v7
exports include endpoint and parameter-twin lookup indexes; older exports can
lack these indexes and should be re-analyzed for the same lookup performance.
Work is proportional to encountered data and finite hidden metadata states,
so a long hidden component can still require a large walk even at depth one.

BFS counts visible transitions after projection, in either direction. Every
exported presentation edge stays oriented source to destination; `up` changes
reachability, not edge meaning. Ordering is deterministic. Hidden-node cycles
terminate, cross edges and merges survive, and boundary edges to already reached
nodes remain visible. `truncated` means an additional visible entity was cut off
by depth, rather than an unshown technical hop. Text uses fixed indentation
within each scope. Text lists selected scopes first, then scopes in first-visit
BFS order in the requested direction. Roots and sibling entities tie by PAG
node ID; parallel edges tie by their complete recorded metadata. Each transition
appears once, under its traversal origin's scope (`from` for `down`, `to` for
`up`), with text endpoints in other scopes qualified by function name, such as
`dispatch::payload [var#99]`. Function-value endpoints already name their function
and are not prefixed again. Each text function section header includes its
canonical parameter list: 1-based argument position, name, type, and `VarId`,
including parameters outside the visible graph. Positions follow the solver's
parameter list, including an implicit receiver when present, so they match the
edge's argument number. Parameterless functions show `()`. Parameter metadata
does not add graph nodes or affect depth. Older exports lacking this metadata,
or declarations without recorded parameter variables, show `parameters unavailable`
when their signature has parameters; re-analysis supplies available metadata.
Call arguments and returns use the same arrow rows as local operations,
retaining argument positions, operation sites, and macro spelling. Argument
annotations use `pass argument [fn#29, argument 2] at main.c:39:5`; the callee
function ID appears in the operation brackets rather than the destination
label. Call-site IDs remain internal, including for return and indirect-call
transitions, and are omitted from output. A return to
an earlier scope does not repeat that scope's heading; grouping is a may-flow
view, not an execution path. Scopes present only as operation/callee metadata
follow reached scopes in scope-key order. After scope discovery, transitions
within each scope sort by their recorded operation source path, then line, then
column. Transitions without a positive source line come last. Ties sort by the
complete edge metadata (endpoint IDs, operations, provenance, call identity,
and other recorded fields). Sorting uses the prominent operation location,
including a macro call's invocation, rather than declaration sites, macro
spelling, or earlier constituent provenance sites. Text rows, the JSON `edges`
array, and Graphviz/Mermaid edge declarations share this scope-and-source order
in either direction. This does not change traversal, indentation, graph content,
depth limits, or truncation. Graphviz/Mermaid scope and node declarations keep
their existing order; diagram layout does not promise source-ordered nodes.
Graphviz/Mermaid use scope groups.
All formats consume the same complete `DataflowView` and traversal truncation.
Text shows every transition in the graph selected by `--depth`.

Call-target labels show the recorded callee spelling. Numeric PAG node IDs
distinguish occurrences in structured output and diagrams; call-site IDs remain
internal. Text
shows its enclosing function, recorded location and macro spelling, then a
separate possible-target count and list, once per call occurrence. Targets sort
by `FnId`, and text shows every possible target. Diagrams use compact call labels
and counts, with no embedded target lists. JSON retains `callees` (all `FnId`s)
and includes every target function in `scopes.functions`; node `targets` is omitted.

Text has no separate display limit. The `truncated at visible depth limit`
marker reports traversal-depth truncation; increasing `--depth` includes more
of the graph. Target counts and complete target lists remain attached to each
call occurrence, with the list printed once even when multiple transitions
reach the same call.

Compatibility: raw SQLite `flow_nodes`/`flow_edges`, the raw query API, and call
graph formats retain their existing contracts. The CLI dataflow JSON contract
intentionally changes: scope-key objects become the four description arrays
and typed references defined above, edge `callee_fn_id` moves to operation `callee_id`,
operation strings become objects, and the argument index moves from the edge to
each argument operation. Null argument indices and callee IDs are omitted;
operations carry their original `callee_id` when known, and edges omit it. Internal-only fields are omitted. Consumers must
update to the new contract; SQLite, analysis, text, and diagram behavior are
preserved. Clients requiring PAG facts
should use `dataflow_graph` or the raw tables. Source-level presentation metadata
is part of the v7 layout; the source view requires a fresh analysis of older
exports and reports a re-analysis instruction rather than guessing synthetic
status or field destinations from names. Field-access and field-location parent metadata are required by the source
view; re-analyze databases exported before they were introduced.

Limitations are those of the recorded constraint graph: it is not an execution
trace or a complete source evaluator. In the `dataflow_presentation` fixture,
retained omitted defaults have the argument bindings described in
[Return-value flow](IR.md#return-value-flow); `default_output` has no flow node. Explicit pointer
arguments and C++ `nullptr` retain their recorded argument connections; `nullptr` is displayed as a null pointer value, never an object's
address or an unknown value. Each occurrence keeps its own parameter positions
and source location in every format, with call-site IDs retained internally.
The graph remains restricted to values reachable from the selected variable:
a query from one argument need not include independent arguments of that call.
Query a receiver downstream or its formal upstream to see receiver passing,
and query a nullable formal upstream to see its explicit null arguments. A
presentation cannot supply absent facts. Pointer/field operations explain the
recorded constraint connectivity; they do not introduce store-to-load alias
edges or correlations absent from that exported graph.
