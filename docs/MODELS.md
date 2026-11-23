# Function models and execution contexts

Configurable summaries for bodyless functions, IPC bridges, callbacks and execution contexts. Part of the [analysis documentation](ANALYSIS.md).

## Function models (configurable summaries)

Bodyless functions (libc, `_s`-family secure variants, vendor externs) contribute no
IR: calls to them produce call edges but no data flow. **Function models** close this
gap with declarative per-function summaries that relate parameters to each other
or connect registrations to receipts across modeled API calls.

Models are matched by function name at every resolved call site — direct, recovered
cross-TU, indirect, and external (see [Model matching](#model-matching)). A model
applies regardless of whether the callee is defined in-tree, so project-specific
wrappers can be described too.

### Model matching

This section is the authoritative statement of which function a model applies to
(`ModelLookup`, `summaries.rs`). Every effect is looked up through it — at call
sites, into call results, for `dlsym`, callbacks and framework entries — so one
model is matched under the same rules whatever its effects, except that argument
and return effects skip rules 2 and 3 (below). The rules, in precedence order:

1. **The exact name** (`memcpy_s`, `ffrt::submit`, `OHOS::Utils::Timer::Register`).
2. **A constructor's class name**: a model named for a constructor (`C::C`) also
   covers a call recorded under the class name `C`, which is how a temporary is
   spelled when the unit never saw the class (`std::thread(f, x)`).
3. **An unqualified model by the callee's last segment**: `dlsym` covers `::dlsym`
   and `ns::dlsym`. A model named plain `PostTask` would therefore cover every
   class's `PostTask`; the built-in member models are qualified.
4. **A qualified member model `B::m` on another class's `m`**, when the function's
   class, or else its nearest base that does, *may be* `B`: a class the tree
   declares is the class its name says, so only equal names match; a class it
   never declares is named as written, or, written bare, in the innermost
   namespace around it ([Undeclared receiver classes](CXX.md#undeclared-receiver-classes)),
   and C++ lookup from there finds a class of an enclosing namespace too. So past
   the trailing segments the two names share (the class name at least), the
   recorded name may keep no namespace or one nested in `B`'s
   (`class_may_be`). `AppExecFwk::EventHandler::PostTask` (written under
   `using namespace OHOS`), `OHOS::Camera::ThreadPool::AddTask` (a bare
   `ThreadPool` inside `OHOS::Camera`) and `MyHandler::PostTask` (a subclass of
   the library's handler that declares no `PostTask`) take the models of
   `OHOS::AppExecFwk::EventHandler::PostTask` and `OHOS::ThreadPool::AddTask`;
   hiview's own, declared `OHOS::HiviewDFX::EventHandler` takes none. Of several
   models that may match, the one matched on the nearest class wins: the
   function's own class is at distance 0, its direct bases at 1, and a base
   reached along several paths at its shortest. Models at equal distance are
   ordered by model name (byte order), never by the order a class lists its
   bases, so `Leaf : Z, A` and `Leaf : A, Z` both take `A::Post` over `Z::Post`.
   The choice is per effect group (see below).

Rules 2 and 3 name models for `dlsym`, `invoke`, `entry` and `ipc` effects only.
**Argument and return effects** (`alias`, `mem_copy`, `content_store`, `epoll_register`, `epoll_wait`, `clears`;
`return_alias`, `return_heap`, `return_field`) skip rules 2 and 3 and match
by rule 1, where the name may also be written with a leading `::` or `std::`,
which name the same C library function: `memset`, `::memset` and `std::memset`
take the `memset` model, while a class's or namespace's own
`SecureBuffer::memset`, `Pool::memcpy`, `secure::memset` or bodyless
`Arena::malloc` take none of the libc models (no terminator, no copy, no fresh
heap), and rule 4 still applies to them.

Not matched: a member a subclass inherits from a base the unit never declares,
called bare in the subclass's body (`PostTask(f)` inside a subclass of
`AppExecFwk::EventHandler`): lowering records it as a free function `PostTask`,
not as a member of any class.

**A named model is authoritative.** A model rules 1-3 give a function, for the
effect group looked up, states all of its effects, so an explicitly empty one
disables the function: `disabled::dispatch` with no effects keeps a bare
`dispatch` model off `disabled::dispatch`. Only when no model is named for it
does a function take rule 4's.

**Rule 4 is chosen per effect group.** A function's effects of one group come from
the first rule-4 model, in the order above, that has an effect of that group; a
model with none is passed over. The groups are the parts of the analysis that
apply them: argument effects (`alias`, `mem_copy`, `content_store`, `epoll_register`, `epoll_wait`, `clears`),
return effects (`return_alias`, `return_heap`, `return_field`), `dlsym`, `invoke`, `entry` and `ipc`. So
a nearer base's `entry` model of `Mid::m` does not hide a farther base's `invoke`
model of `Base::m` for `Leaf::m`, and a requalified or subclass call takes every
group of the library member's model. The rule-4 candidates for a name are
resolved once per analysis and shared by every lookup.

### Effect kinds

| Effect | Semantics (may) | PAG realization |
|--------|-----------------|-----------------|
| `alias { dst, src }` | `pts(param[dst]) ⊇ pts(param[src])` after the call | persistent `Copy(actual[dst] ← actual[src])` |
| `mem_copy { dst, src }` | contents of `*src` copied into `*dst` (memcpy family) | modeled as `alias` (see imprecision note) |
| `content_store { ptr, value }` | `*param[ptr] = param[value]` | persistent `Store(actual[ptr] ← actual[value])` |
| `epoll_register { param }` | register the event object at this argument with the epoll summary | persistent copies from registered objects to waiting buffers |
| `epoll_wait { param }` | receive registered event objects into this argument | pairs with `epoll_register` (see below) |
| `return_alias { param }` | returned pointer may be `param[param]` | addr/copy edges into the `CallReturn` destination |
| `return_heap` | returns a fresh storage location | fresh `Heap` loc per call site into the destination |
| `return_field { param, field }` | returns the address of `field` (`return &p->field`), or the value stored there when the getter returns a pointer of the member's type (`return p->field`) | field projection at each call site, followed by a copy for an address result or a persistent load for a value result |
| `clears { param }` | **terminator**: memory reachable via `param[param]` is zeroed by this call | no value introduction; terminator event exported |
| `dlsym { param }` | return value may be the address of the in-tree function named by string constants in `param[param]` | `Dlsym` PAG constraint; unknown names add nothing |
| `entry { context }` | the named member `Class::method` is one a framework calls: it and every override of it the tree defines is the entry of an [execution context](#execution-contexts) of kind `context`, which no call site starts | none: no edge, no flow |
| `ipc { target }` | a bodyless callee dispatches to the qualified member `Base::method` or its in-tree implementations; see [OpenHarmony IPC bridges](#openharmony-ipc-bridges) for matching and imprecision | synthetic `ipc` call edges, no value flow |
| `invoke { param, args / rest, context, supplies, if_arg / unless_arg }` | may invoke the callback passed in `param[param]`, with the arguments `args = [..]` lists (none when omitted: a zero-argument callback) or every argument from position `rest` on, preceded by a value of class `supplies` when the callback takes one, in an execution context of kind `context` (`unknown` when omitted; `none` when the model only wraps the callback and starts nothing, see [Callable wrappers](#callable-wrappers)); with `if_arg` / `unless_arg`, only at a call whose argument it names may be / may not be of the class it names | adds an indirect edge from the submitting caller at the submission site, read off the converged points-to sets; callback return values and scheduling are ignored. Forwarded arguments are wired into the callback's formals inside the fixpoint (see [Callback invocation](#callback-invocation-invoke)). Unlike the value-introducing effects this is not restricted to a bodyless callee — a callback API is routinely a template whose body the index holds once, uninstantiated — and an edge the callee's own body already yields is not added twice. Each resolved callback is also an [execution context](#execution-contexts), unless the model states `context = "none"` |

Effects attach to parameter positions (0-based) of the *actual arguments* recorded at
the call site. Arguments that are not IR variables or functions (literals like
`sizeof(...)` or `0`) simply do not participate. Positions count the callee's
recorded parameters: for a C++ member the implicit `this` is position 0 and the
first explicit argument is 1. `invoke` always counts explicit arguments, as
does `return_field` for a static member.

### Epoll event data

`epoll_ctl` registers argument 3; `epoll_wait`, `epoll_pwait` and
`epoll_pwait2` receive into argument 1. The solver connects each registered
event pointer to each receiving buffer with the same whole-object aliasing
approximation as `mem_copy`. Registration retains the actual pointer, including
`&local_event`; a decayed local event array denotes its own storage. Receiving
`&buffer` arguments name the buffer's value node. Wiring immediately propagates
the source's existing pointees and installs a persistent copy for later growth,
so endpoints discovered in different fixpoint iterations still exchange data.
They are exported as ordinary flow edges, so upward value slices can cross
from the returned event back to its registration. Epoll does not itself call
`data.ptr`: an indirect call in the event loop resolves through the returned
value and retains that call's source position.

**Imprecision:** registrations are pooled across the analysis, without epoll-fd,
watched-fd, link-image, event-index or readiness discrimination. Integer handles
are not pointer identities; shared libraries may register events consumed by a
caller in another image. Whole event objects alias rather than copying only
`data`; `events` flags, timeout, return counts and failures are not evaluated.
ADD and MOD both contribute values. DEL/close do not kill earlier registrations
(flow-insensitive may-analysis); an event argument supplied to DEL is also
conservatively admitted, though the kernel ignores it. Null event arguments
contribute no values. Synthetic temporaries produced only by the null value
are omitted as registration endpoints, so a literal-null DEL argument does not
appear as a source of wait buffers. A variable initialized to null is still an
endpoint: it may receive values later. An empty points-to set alone is never a
reason to skip registration.
No epoll creation model is needed. This can mix payloads
from independent instances, but does not invent a callback or an execution
context when the program only retrieves data.

The kernel's data round trip is specified in
[epoll_wait(2)](https://man7.org/linux/man-pages/man2/epoll_wait.2.html).

### Callback invocation (`invoke`)

Lambda bodies remain separate functions during analysis and linking. Export
publishes the [lambda-free SQL interface](SQLITE_SCHEMA.md#lambda-free-call-graph-views)
by default and retains the complete analysis under `raw_*`; that section
defines projected ownership, call provenance and execution contexts.

`invoke` says the callee may call one of its arguments: a task queue running
what was submitted, a thread running its entry point. The callback is every
function the argument names or may point to, restricted to the caller's link
image as an indirect call's targets are.

- **Which arguments it gets.** `args = [3]` passes the callee's argument 3 as
  the callback's first parameter (`pthread_create(&tid, attr, fn, arg)` is
  `param = 2, args = [3]`); `rest = 1` passes argument 1 as the first
  parameter, argument 2 as the second, and so on
  (`std::thread(fn, args...)` is `param = 0, rest = 1`). `args` and `rest` are
  alternatives; with neither the callback takes nothing. Positions count
  explicit arguments, so a constructor's or member's implicit `this` is not
  one of them. A model that passes the callback to itself, or gives `args` /
  `rest` to another effect kind, is rejected when the file is loaded.
- **A value the callee supplies.** `supplies = "std::stop_token"` says the
  callee passes a value of that class ahead of the forwarded arguments to a
  callback that takes one (`std::jthread`). Per callback definition, the
  formal the first forwarded argument would reach decides: declared of that
  class (however qualified), the supplied value takes it and the forwarded
  arguments start at the next formal, which also needs room in the count; of
  a type that cannot be the class (a pointer, a function pointer, another
  class), the arguments start there as without `supplies`; of a type lowering
  could not resolve (read as `int`, or a guessed class), both readings are
  wired. A reference (`const std::stop_token &`) is declared of its
  referent's class: lowering types it as the referent's address, and the
  function's `reference_params` tell it from a pointer parameter, which stays
  a pointer. A callback that takes the class but has no room for the
  supplied value ahead of the arguments (`work(std::stop_token)` started as
  `std::jthread t(work, token)`) is not invocable with it, so the arguments
  are forwarded as given, the caller's token to the formal
  ([thread.jthread.cons]). The supplied value itself carries no flow.
- **A call it applies at.** `if_arg = { param = 0, type = "std::launch" }`
  applies the effect only at a call whose argument 0 may be a value of that
  class or enumeration, `unless_arg` only at one where it may not be; the two
  are alternatives (`std::async(policy, f, args...)` beside
  `std::async(f, args...)`). The argument decides by what the call site
  records there: a function named is never such a value; a variable is one
  by its declared type, read as for `supplies` (a reference parameter,
  `const std::launch &`, by its referent's). A local bound as a reference is
  not recorded as one past lowering, so a pointer-typed local whose referent
  may be the class may be either. An argument the site records
  nothing for (an enumerator such as `std::launch::async`, a literal, a call
  result) and a variable of an unresolved type may be either, so both a
  model's `if_arg` and its `unless_arg` effect apply there.
- **Which callees.** A constructor model also covers a temporary spelled
  with the class name (`task = std::thread(f, x)`), and a member model the
  member on a class the unit never declares and on a subclass
  ([Model matching](#model-matching)). One registration, so overriding or
  disabling it covers every form.
- **Which callbacks fit.** A listed form reaches a callback that can be
  called with exactly that many arguments: its declared parameters, less
  those with defaults, more if it is variadic. So a zero-argument model does
  not run a function that needs an argument. A `rest` form has only a lower
  bound to hold a callback to — a call site records the arguments that are
  variables or functions, not how many there are — so it reaches a callback
  with room for the last argument recorded. A function no declaration was
  read for takes anything.
- **Members.** Lowering gives every member function a `this`, a static one
  included. A listed form never passes a receiver, so its arguments start
  past `this`. A `rest` form passes the receiver first when the callback is
  a non-static member (`std::thread(&C::Run, this, x)`) and starts past
  `this` when it is a static one (`std::thread(C::Out, x)`), as its in-class
  declaration's `static` tells (`Function::is_static_member`). Only a member
  whose class body no unit showed could be either: where the argument count
  allows both readings, both are wired.
- **Wiring.** Each forwarded actual is wired into the callback's formal
  exactly as an indirect call wires its arguments (`wire_params`): a
  persistent `Copy` for pointer-like pairs, the function location for a
  function named as the argument. This happens inside the fixpoint — for a
  callback named at the site when the site is resolved, and for one held in a
  variable whenever the variable gains a function — so calls made through the
  forwarded values resolve. Wiring only adds copies and points-to members, so
  convergence stays monotonic; functions already held when a site is first
  seen are wired in `FnId` order.
- **Arg-flow rows.** Each forwarded argument is an arg-flow row at the
  starting call site: its position there, and the callback's formal.
- **Forwarded by the API itself.** A modelled API whose body hands one of
  its own callback parameters to another modelled API — `TaskHandlerWrap::SubmitTask`
  overloads calling each other, `SubmitTaskJust` forwarding to `SubmitTask`
  — adds no edge at the inner call. The outer submission site resolves the
  concrete callback; the inner one would only rebroadcast the parameter's
  points-to set (every callback handed to the API anywhere) from the API
  body. The two APIs may put the callback at different positions. A user
  wrapper that is itself an `invoke` model and forwards its own parameter is
  treated the same way, but only when its model is unconditional: a model
  with `if_arg` / `unless_arg` may not add the outer edge at every call, so
  its forwarded parameter always keeps the inner one. Only a context-less
  inner effect is skipped; one that states a context keeps its edge and
  context, so no execution context is dropped.
- **Context.** `context` names the [kind](#execution-contexts) of execution
  context the callback runs in (`unknown` when omitted). `context` on an
  effect kind other than `invoke` and `entry` is rejected when the file is
  loaded. Each resolved callback is exported as one execution context (next
  section); the call graph is the same whatever the kind. `context = "none"`
  is the one spelling that is no kind: the model only wraps the callback, so
  it keeps the call edge, the forwarded arguments and the arg-flow rows
  above, and the callback is no context, with no `execution_contexts` row
  and no `async_start` boundary. Only an `invoke` may say it (an `entry` is
  where a context starts), and the context graph then treats the wrapping
  site's edge as an ordinary call.
- **Not modeled.** No ordering or scheduling (the callback is simply
  reachable from the starting site), no return value, and no bound state:
  a `std::bind` result, a `std::packaged_task` and a `std::function` object
  are not looked into. What the first two do keep reachable is
  [Callable wrappers](#callable-wrappers).

#### Callable wrappers

`std::bind(f, args...)` and `std::packaged_task<R(A...)>(f)` only wrap a
callable that something runs later, usually a thread pool's worker. No value in the analysis carries a
callable on, so the later call, `task()` or `(*task)()`, cannot be tied back
to `f`. Both are therefore `invoke` models that start no context, `context =
"none"` ([Built-in models](#built-in-models)): `std::bind` with `param = 0,
rest = 1` and `std::packaged_task::packaged_task` with `param = 0, rest = 1`: the
constructor forwards no argument, and `rest` (not an empty `args`) lets the
stored callable take any number of parameters, as the task's signature, not the
constructor, says what it is called with. This is what keeps a
`Submit`-style pool's `Submit -> f` edge, from `std::bind(std::forward<F>(f),
...)` inside `Submit`, where `f` is the callable the pool's caller passed.

- **The edge is the wrapping site's.** It runs from the function that builds
  the bind or the task, at that site, not from the caller of `operator()`.
  It is an over-approximation: it exists whether or not the task ever runs,
  and a bind that is built and then dropped has it too.
- **Bound arguments.** `rest = 1` wires the arguments after the callable into
  the callable's formals as for `std::thread`, so a named bound argument
  reaches the callable's formal. The model does not read `std::placeholders`:
  nothing supplies the later caller's arguments.
- **A parameter pack is followed.** An argument passed through a
  variadic template's `Args&&... args` reaches the callable's formal:
  `Submit(f, a)` carries `a` through the pack and out through
  `std::forward<Args>(args)...` into `f`'s formals, as a named parameter
  would ([Variadic parameter packs](POINTERS.md#variadic-parameter-packs), including
  what it over-approximates).
- **No execution context.** Both models state `context = "none"`: nothing
  starts at a bind or at a task's construction, so a match exports no
  `execution_contexts` row and, in call sequencing, no `async_start`
  boundary; the wrapping site is an ordinary call. Where the callable runs
  is for whatever later calls the task, which the analysis does not follow.
  A `--models` entry for another wrapper (a task-making helper, an
  `emplace`-style forwarder) states `context = "none"` the same way.
- **Built through a factory.** `std::make_shared<std::packaged_task<void()>>(f)`
  and `std::make_unique<...>(f)` take the constructor model too, from a named
  function or a lambda: the factory's constructor site
  ([Factory construction](CXX.md#factory-construction)) is the one the model
  matches, with `f` as its first argument, so the edge runs from the function
  holding the factory call. Pinned by `tests/fixtures/cpp_bind_packaged_task`.
  A task whose signature names an enclosing template's parameter
  (`std::packaged_task<R()>` in `template <typename R>`) is the same class,
  so it reaches its callable too, as `new std::packaged_task<R()>(f)` does.
  `Submit`'s `std::packaged_task<ReturnType()>` records the site as well, but
  its argument is a bind result, not a function, so the site adds an external
  edge and the edge to the callable still comes from the `std::bind`.
- **`std::function`.** No model: a callable stored in one flows through the
  ordinary copy and points-to handling, and calling it is an indirect call.

### Execution contexts

An execution context is a place where code starts running on a thread, a
task queue or the IPC worker pool. The analysis lists them once the call
graph is complete (`AnalysisResult::execution_contexts`, `contexts.rs`), and
the export writes them to `execution_contexts`
([schema](SQLITE_SCHEMA.md#execution_contexts)). Nothing here feeds the
solver, and `call_edges` is unchanged: a start is still an ordinary
`indirect` edge from the submitting caller there, and an IPC handler is
still the callee of an `ipc` edge.

- **Kinds** (`kind`, a model's `context`): `thread` (a new thread),
  `pool_task` (a task on a pool of worker threads), `serial_task` (a task on
  a queue that runs one task at a time), `ipc_handler` (a request handler on
  the IPC worker pool) or `unknown`. A model's `context = "none"` is no kind
  and no context: see [Callable wrappers](#callable-wrappers).
- **Which contexts.** One per resolved callback of an `invoke` model that
  states a kind:
  submitting call site, modelled callee (`pthread_create`,
  `std::thread::thread`, `ffrt::submit`, `AppExecFwk::EventHandler::PostTask`),
  invoked parameter, the receiver the callee was called on, callback
  function, the kind the model states and the model's name (which may differ
  from the callee's, [Model matching](#model-matching)). The callbacks are the
  targets of the edges [Callback invocation](#callback-invocation-invoke)
  adds, so a callback that reaches a declaration and its definition is two
  contexts, as it is two edges. Then the entries no call site starts, by
  function: every definition of an `entry` model's member and of an override
  of it (`Worker::Run` for `OHOS::Thread::Run`; matched as in
  [Model matching](#model-matching), rule 4), with the model's kind and name,
  and every IPC stub handler a bridge targets
  ([OpenHarmony IPC bridges](#openharmony-ipc-bridges)), of kind
  `ipc_handler` and with no model: the request comes from another process.
  Rows are ordered by call site, modelled callee, parameter, callback, kind
  and model, then by entry, kind and model, so the export is the same at any
  `--jobs`, and `trace-merge` orders merged rows the same way
  ([Invariants and linking rules](ANALYSIS.md#invariants-and-linking-rules)).
- **Which receiver.** A member call site records the variable it is made on
  when the receiver is one by name (`CallSite::receiver`): `q` in
  `q.submit(f)`, `handler` in `handler->PostTask(f)`, `this` in
  `this->PostTask(f)` or a bare `Run()` in a member body, the object a
  constructor builds (`std::thread t(f)`), through parentheses, casts and
  `std::move`. It is identity only, not bound to the callee's `this`. A field
  (`handler_->PostTask(f)`, `this->handler_`), a call result or any other
  expression records none, so tasks queued on a member handler are not told
  apart here; nor is a pointer variable's pointee: two variables naming one
  queue are two receivers.
- **A start site is a context, not a thread.** A wrapper every thread is
  started through (HDF's `OsalThreadCreate` around one `pthread_create`)
  makes one start site for every entry function the wrapper is given, and
  the same callback started at two sites is two contexts. The executor a
  task is queued on is at most its receiver variable (above).
- **Self-concurrency.** `self_concurrent` says two instances may run at the
  same time: every IPC handler, a stub handler or a callback a model states
  as `ipc_handler` (the IPC worker pool runs requests on the same stub
  concurrently unless the stub serializes them, which is not read), and
  every context with multi-instance evidence. For a `serial_task` this is
  only "possibly": tasks on one queue run one at a time, and the queue is at
  most a receiver variable. An entry no call site starts has no
  multi-instance evidence of its own: it is self-concurrent only as an
  `ipc_handler` (`DeathRecipient::OnRemoteDied` runs on an IPC thread). `0`
  means no evidence, not single execution.
- **Multi-instance evidence** (`multi_instance`), the first that applies:
  - `loop`: the start site is lexically inside a loop of its function
    (`CallSite::in_loop`): a loop's body, condition or update, but not a
    `for` init-statement or a range-for's range, which run once, and not the
    body of a lambda written in the loop, which is a function of its own.
    Lowering marks the call sites a loop's children produced; a record that
    copies of one call merge into (an `--explore` variant's, or a call two
    units spell at one source occurrence) is in a loop when any copy is, and
    the sites post-merge virtual expansion adds keep the mark of the call. A
    header function another unit compiles differently is not merged: its
    first body is kept with that body's loop evidence, as with its other
    calls.
  - `cycle`: the submitting function is in a cycle of ordinary calls
    (recursion). The edge from a start site to the context it starts and an
    IPC bridge close no cycle: through a wrapper, a thread that starts
    another thread would otherwise look recursive. The
    call graph cannot tell a context that starts itself from a wrapper
    starting a different entry, so a context that starts itself, directly or
    through a wrapper, is not recorded either, though the instances of a
    thread or pool task started that way may overlap.
  - `parent`: the submitting function is reachable over the whole call
    graph, start edges included, from the entry of a self-concurrent context,
    so a context nested any depth below an IPC handler or a thread started in
    a loop counts. Through a wrapper this marks every context the wrapper's
    site starts.
  - `unknown`: none of these. A start site several ordinary calls reach (a
    wrapper called twice, a function called in a caller's loop) can still run
    more than once.
- **Call order.** A start site is an `async_start` boundary in
  [Call sequencing](USAGE.md#call-sequencing-control-flow-sidecar): the callback is
  not ordered after the starter's later calls, and no happens-before is
  claimed across contexts.

### Terminators (`clears`)

A terminator states that the call **writes zeros** through a pointer parameter
(`memset(p, 0, n)` family). Semantics under may-analysis:

- The call introduces **no pointer values** through any modeled argument; data never
  flows *out of* the terminator's parameters into memory or return values.
- Kills are **not** modeled: the solver is flow-insensitive and inclusion-based
  (monotone); values stored before a memset still reach later loads. This mirrors the
  documented no-path-sensitivity stance — adding kills would be flow-sensitive
  refinement requiring explicit design approval.
- Every applied `clears` event is exported as a `terminator` flow node with a
  `terminates` edge from the cleared argument, so `trace inspect dataflow` shows where
  value chains are zeroed instead of silently stopping.

### Built-in models

Shipped in `trace-analysis/src/summaries.rs`; user configuration overrides same-name
entries:

| Function(s) | Effects |
|-------------|---------|
| `memcpy`, `memmove`, `strcpy`, `strncpy` | `mem_copy dst=0 src=1` |
| `memcpy_s`, `memmove_s`, `strcpy_s`, `strncpy_s` | `mem_copy dst=0 src=2` |
| `epoll_ctl` | `epoll_register param=3` |
| `epoll_wait`, `epoll_pwait`, `epoll_pwait2` | `epoll_wait param=1` |
| `memset`, `memset_s` | `clears param=0` |
| `malloc`, `calloc`, `zalloc`, `kmalloc` | `return_heap` |
| `realloc` | `return_alias param=0`, `return_heap` |
| `dlsym`, `dlvsym`, `GetProcAddress` | `dlsym param=1` (symbol-name argument) |
| `ffrt::submit`, `ffrt::submit_h` | `invoke param=0 context=pool_task` (the task; dependences and attributes after it) |
| `ffrt::queue::submit`, `ffrt::queue::submit_h` | `invoke param=0 context=serial_task` (explicit callback argument, excluding implicit `this`) |
| `ffrt_submit_base`, `ffrt_submit_h_base` | `invoke param=0 context=pool_task` (the function header) |
| `ffrt_queue_submit`, `ffrt_queue_submit_h` | `invoke param=1 context=serial_task` (the function header, after the queue) |
| `OHOS::AppExecFwk::EventHandler::PostTask`, `PostImmediateTask`, `PostHighPriorityTask`, `PostIdleTask`, `PostSyncTask`, `PostTimingTask`, `PostTaskAtFront` | `invoke param=0 context=serial_task` (the callback; name, delay, priority after it) |
| `OHOS::AAFwk::TaskHandlerWrap::SubmitTask`, `OHOS::AAFwk::TaskHandlerWrap::SubmitTaskJust` | `invoke param=0 context=none` (the callback; name, delay, QoS or attribute after it; the handler's in-tree `SubmitTaskInner` submits it to an ffrt queue whose model states the context — [Callable wrappers](#callable-wrappers)) |
| `OHOS::ThreadPool::AddTask` | `invoke param=0 context=pool_task` |
| `OHOS::Utils::Timer::Register` | `invoke param=0 context=serial_task` (the callback; interval and once-flag after it) |
| `HdfWorkInit`, `HdfDelayedWorkInit` | `invoke param=1 args=[2] context=serial_task` (work function and its argument) |
| `OsalTimerCreate` | `invoke param=2 args=[3] context=thread` (timer function and its argument) |
| `pthread_create` | `invoke param=2 args=[3] context=thread` (start routine and its argument) |
| `std::thread::thread` | `invoke param=0 rest=1 context=thread` (callable and everything after it); covers a temporary recorded as `std::thread` |
| `std::jthread::jthread` | `invoke param=0 rest=1 supplies=std::stop_token context=thread` (as `std::thread`, with the thread's stop token first to a callable that takes one); covers a temporary recorded as `std::jthread` |
| `std::async` | `invoke param=0 rest=1 unless_arg={param=0, type=std::launch} context=thread` and `invoke param=1 rest=2 if_arg={param=0, type=std::launch} context=thread` (the callable first, or after a launch policy) |
| `std::bind` | `invoke param=0 rest=1 context=none` (the callable and the arguments bound after it; [Callable wrappers](#callable-wrappers)) |
| `std::packaged_task::packaged_task` | `invoke param=0 rest=1 context=none` (the callable the task stores, of any arity; the constructor forwards no argument; a bind result is not a function, so the bind site makes the edge) |
| `OHOS::Thread::Run` | `entry context=thread` (c_utils runs it on the thread `Start` creates) |
| `OHOS::AppExecFwk::EventHandler::ProcessEvent` | `entry context=serial_task` (the handler's event runner calls it) |
| `OHOS::IRemoteObject::DeathRecipient::OnRemoteDied` | `entry context=ipc_handler` (an IPC thread calls it when the remote dies) |

### Configuration format

TOML, one `[[model]]` table per function; loaded via `--models <FILE>` (repeatable;
later files override earlier entries and built-ins):

```toml
version = 1

[[model]]
name = "SbufImplAssign"
effects = [
    { kind = "mem_copy", dst = 0, src = 1 },
]

[[model]]
name = "MyInit"
effects = [
    { kind = "content_store", ptr = 0, value = 1 },
]

[[model]]
name = "MyPoolAlloc"
effects = [ { kind = "return_heap" } ]

# A bodyless accessor: `get_ops(&obj)` returns `&obj->ops` (a load through the
# result reads `obj->ops`), so a callback stored in that member is reachable
# through the getter's result.
[[model]]
name = "get_ops"
effects = [ { kind = "return_field", param = 0, field = "ops" } ]

[[model]]
name = "MyDlsym"
effects = [ { kind = "dlsym", param = 1 } ]

# A callback API: argument 1 is run with argument 2, as a task on a pool.
[[model]]
name = "pool_post"
effects = [ { kind = "invoke", param = 1, args = [2], context = "pool_task" } ]

# A wrapper that keeps its callback reachable but starts nothing: the edge
# and the forwarded arguments, no execution context, no `async_start`.
[[model]]
name = "make_task"
effects = [ { kind = "invoke", param = 0, rest = 1, context = "none" } ]

# Argument 0 is a launch policy or the callable; a callable that takes a
# `Token` first gets the runner's own ahead of the forwarded arguments.
[[model]]
name = "run_async"
effects = [
    { kind = "invoke", param = 0, rest = 1, unless_arg = { param = 0, type = "Policy" } },
    { kind = "invoke", param = 1, rest = 2, if_arg = { param = 0, type = "Policy" }, supplies = "Token" },
]

# A virtual member a framework runs on its own thread: every override is an
# entry. Named `Class::method`; `entry` takes only `context`.
[[model]]
name = "OHOS::Worker::Loop"
effects = [ { kind = "entry", context = "thread" } ]

# A bodyless framework member dispatches to indexed provider implementations.
# Matching and imprecision: see "OpenHarmony IPC bridges" below.
[[model]]
name = "MyFramework::OpenFile"
effects = [ { kind = "ipc", target = "MyProviderStub::OpenFile" } ]

# An explicitly empty effect list overrides (disables) a same-name built-in.
[[model]]
name = "memcpy"
effects = []

# Opt-in DataShare fallback for unavailable framework implementations.
# Prefer separate indexing and trace-merge when framework sources are available.
[[model]]
name = "OHOS::DataShare::DataShareHelper::OpenFile"
effects = [ { kind = "ipc", target = "OHOS::DataShare::DataShareStub::OpenFile" } ]
```

`supplies`, `if_arg` and `unless_arg` belong to `invoke` and name a class or
enumeration (`ns::Name`, no pointer, reference or template arguments); a test
table takes exactly `param` and `type`. `field` belongs to `return_field` and
names one member of the parameter's pointee (no whitespace, one name, not a
path). Anything else, or both tests on one effect, is rejected when the file is
loaded.

### Container element models

A `--models` file can also define `[[container]]` tables alongside function
models. Container models apply
during C++ lowering because an accessor must supply both an element type and
an address. Each table names a class template and the zero-based position of
its element type argument. Operations name declared members and count explicit
arguments. Declaration matching accounts for default arguments and variadic
tails. An `access` operation reads or writes the element cell through its
result. A `store` operation writes the value of the selected argument.

```toml
[[container]]
name = "std::deque"
element = 0
initializer_list = true
operations = [
  { kind = "store", method = "push_front", arity = 1, value = 0 },
  { kind = "store", method = "push_back", arity = 1, value = 0 },
  { kind = "access", method = "operator[]", arity = 1 },
  { kind = "access", method = "front", arity = 0 },
]
```

`initializer_list` enables braced elements for named variables, default member
initializers, and constructor member initializers when an initializer-list
constructor is declared. Parenthesized construction accepts an explicit
allocator after the list. Braced construction can also use a nested list
followed by a typed allocator when an indexed list constructor accepts both
arguments. `initializer_allocator` gives the zero-based template argument for
that allocator when the constructor's parameter type depends on the template.
Named `std::allocator<T>` objects keep their template arguments for this match.
An empty second argument `{}` uses the indexed constructor parameter when the
outer braces cannot initialize the elements directly. For example,
`std::vector<std::function<void()>> v{{alpha, beta}, {}}` stores both callbacks
from the inner list.
Omit `initializer_list` to leave brace handling to ordinary lowering.
A later table for the same class replaces its
element, initializer, and operation settings. Omit `declaration_aliases`
to inherit the previous aliases, supply a list to replace them, or use
`declaration_aliases = []` to clear them. An empty operation list with
`initializer_list` omitted disables the model for that class.
`declaration_aliases` can list alternate qualified owners in standard-library
implementations. The built-ins recognize libc++'s `std::__1` owners and
libstdc++'s `std::__cxx11::list`.

`initializer_entry` selects a zero-based value in each braced entry of an
initializer list. A `store` operation can use `entry` to select a value from a
braced argument, as in `map.insert({key, callback})`. A missing template
argument or entry slot contributes no model effect. Diagnostics for invalid
indices that depend on the indexed C++ declaration are a follow-up. An
`iterator` operation connects a lookup result to the container cell and names the field that
projects it, as in `find(...)->second`. With `member` omitted, dereferencing
the iterator reads the cell, as in `*set.find(key)`. A local `auto` iterator
keeps that connection through ordinary copies and assignments.

The model supports pointer, function-pointer, and directly spelled
`std::function` elements and uses a single
cell across all indices or keys of one modeled container. Erasure and
replacement keep previously stored values reachable. A map model can choose
the mapped type argument and select the value argument in `insert_or_assign`.
The built-ins cover `std::vector`, `std::deque`, `std::list`,
`std::forward_list`, `std::queue`, `std::set`, `std::unordered_set`,
`std::map`, and `std::unordered_map`.
See [Container elements](CXX.md#container-elements) for storage
identity and canonical indexing.

### Modeling recipes (examples)

The tables below are examples, not additional rules: each is expressed with the
[effect kinds](#effect-kinds) above and matched by [Model matching](#model-matching).
Parameter positions are 0-based and address the callee's recorded parameters.
`invoke` counts explicit arguments (a member's implicit `this` is not one); the
argument and return effects (`alias`, `mem_copy`, `content_store`, `return_alias`,
`return_heap`, `return_field`, `clears`, `epoll_register`, `epoll_wait`) count
the recorded parameter list, where a C++ member's implicit `this` is position 0
and its first explicit argument is 1. A call passes a position only when it
records an IR variable or function there, so literal arguments (`0`,
`sizeof(...)`) simply do not take part.

**Moving a value through an out-parameter**

`void load_config(Config *out)` fills the object the caller points at. The
`content_store` writes the value parameter through the pointer parameter, and
the PAG's out-parameter idiom carries it to later reads:

```toml
[[model]]
name = "load_config"
effects = [ { kind = "content_store", ptr = 0, value = 1 } ]
```

`void get_handler(Handler **out)` stores a *pointer* (a callback, another
object) into the caller's slot; the store still targets `*out`, so
`get_handler(&h); h();` resolves the callback:

```toml
[[model]]
name = "get_handler"
effects = [ { kind = "content_store", ptr = 0, value = 1 } ]
```

To fill a *member* of the caller's object, have the caller pass the member's
address (`init(&obj->impl, fn)`): the analysis records that argument as the
member's field address, so the same effect writes the member and not the whole
object.

**Returning one of the arguments (identity / covariant getters)**

`T *acquire(T *existing)` may return its argument; the returned pointer then
carries everything the argument pointed to:

```toml
[[model]]
name = "acquire"
effects = [ { kind = "return_alias", param = 0 } ]
```

Several sources are several effects; the result points to any of them:

```toml
[[model]]
name = "pick"
effects = [
    { kind = "return_alias", param = 0 },
    { kind = "return_alias", param = 1 },
]
```

This is how `realloc` keeps its old storage visible while also allocating.

**Returning fresh storage**

`void *arena_alloc(size_t n)` hands back a new object. `return_heap` gives each
call site a fresh heap location, so stores through the result reach later loads:

```toml
[[model]]
name = "arena_alloc"
effects = [ { kind = "return_heap" } ]
```

Combine with `return_alias` when the allocator may either reuse a parameter or
allocate, as the built-in `realloc` does.

**Accessor getters (`return_field`)**

Many bodyless/framework APIs are accessors for a member of an object the caller
passes. For example, `ops_t *get_ops(Driver *d)` can return `&d->ops` when
`ops` is an `ops_t`, or read `d->ops` when `ops` is an `ops_t *`:

```toml
[[model]]
name = "get_ops"
effects = [ { kind = "return_field", param = 0, field = "ops" } ]
```

The model lets a callback stored in `d->ops` flow into an indirect call through
the getter's result. Each call uses its own actual argument, including calls
whose results are passed directly to another function or returned by a wrapper.
See [Documented imprecision](#documented-imprecision) for how the model selects
the object and member.

**Bulk copy and pointer alias**

A wrapper around `memcpy` copies the contents of `*src` into `*dst`; `mem_copy`
is the over-approximating alias of the two objects (`alias` is the same for
pure pointer copies, without the bulk-content reading):

```toml
[[model]]
name = "copy_config"
effects = [ { kind = "mem_copy", dst = 0, src = 1 } ]

[[model]]
name = "assign_handle"
effects = [ { kind = "alias", dst = 0, src = 1 } ]
```

`memcpy_s(dst, dstmax, src, srcmax)` and friends are the built-ins; the secure
forms shift `src` to position 2. Use `mem_copy` when the function is a
copy-through (contents matter); use `alias` for "this pointer now also denotes
what that one denotes".

**Zeroing terminators**

`void secure_zero(Buffer *b)` writes zeros through its argument. `clears` marks
that chain as terminated and introduces no values; kills are not modeled
(flow-insensitive):

```toml
[[model]]
name = "secure_zero"
effects = [ { kind = "clears", param = 0 } ]
```

The exported graph shows a `terminator` node with a `terminates` edge from the
cleared object, so `trace inspect dataflow` explains why a chain stops rather
than leaving it dangling.

**Dynamic symbol lookup**

A `dlsym`-shaped wrapper turns a string name into the address of an indexed
function of that exact name:

```toml
[[model]]
name = "lookup_symbol"
effects = [ { kind = "dlsym", param = 1 } ]
```

Out-of-tree names add no pointees; a non-literal name that never receives a
string constant does not fan out to every export.

**Callbacks a callee invokes**

A task API runs the callback passed at one position, with zero or more of the
callee's other arguments, in a stated execution context. A queue that runs a
zero-argument task on a pool:

```toml
[[model]]
name = "pool_enqueue"
effects = [ { kind = "invoke", param = 0, context = "pool_task" } ]
```

A thread entry `create_thread(attr, fn, arg)` calls `fn(arg)` on a new thread:

```toml
[[model]]
name = "create_thread"
effects = [ { kind = "invoke", param = 1, args = [2], context = "thread" } ]
```

A variadic forwarder `run_async(fn, args...)` passes everything after the
callable:

```toml
[[model]]
name = "run_async"
effects = [ { kind = "invoke", param = 0, rest = 1, context = "thread" } ]
```

With neither `args` nor `rest` the callback takes no arguments; a `rest` form
also passes the receiver first when the callback is a non-static member.

**Conditional callbacks and supplied values**

`start(policy, fn, args...)` calls `fn` after a policy argument in one overload
and calls the first argument in the other. `if_arg` / `unless_arg` test the
recorded argument's class:

```toml
[[model]]
name = "start"
effects = [
    { kind = "invoke", param = 0, rest = 1, unless_arg = { param = 0, type = "Policy" } },
    { kind = "invoke", param = 1, rest = 2, if_arg = { param = 0, type = "Policy" } },
]
```

`supplies` states a value the callee passes ahead of the forwarded arguments to
a callback that takes it (`std::jthread`'s stop token):

```toml
[[model]]
name = "start_with_token"
effects = [ { kind = "invoke", param = 0, rest = 1, supplies = "Token", context = "thread" } ]
```

**Wrappers that start nothing**

`std::bind` and `std::packaged_task` keep a callable reachable without running
it. `context = "none"` keeps the call edge and forwarded arguments but starts no
execution context and no `async_start` boundary:

```toml
[[model]]
name = "make_task"
effects = [ { kind = "invoke", param = 0, rest = 1, context = "none" } ]
```

**Framework entry points**

A virtual member that a framework calls on its own thread, queue or pool is an
`entry`: every definition and override is a context entry that no call site
starts. It must be named `Class::method`:

```toml
[[model]]
name = "OHOS::Worker::Loop"
effects = [ { kind = "entry", context = "thread" } ]
```

**IPC dispatch across an opaque boundary**

A bodyless framework call that dispatches to a provider in another process is an
`ipc` effect. The target names the provider member; matching and imprecision are
in [OpenHarmony IPC bridges](#openharmony-ipc-bridges):

```toml
[[model]]
name = "MyFramework::OpenFile"
effects = [ { kind = "ipc", target = "MyProviderStub::OpenFile" } ]
```

**Registration that feeds a later receipt**

`epoll_register` remembers the event object at an argument; `epoll_wait` names
the buffer that receives registered objects. The solver connects every
registered event to every waiting buffer:

```toml
[[model]]
name = "my_epoll_add"
effects = [ { kind = "epoll_register", param = 2 } ]

[[model]]
name = "my_epoll_collect"
effects = [ { kind = "epoll_wait", param = 0 } ]
```

**Several effects on one function**

A function may do more than one thing; list every effect. For example, an
allocator that may grow in place or move:

```toml
[[model]]
name = "my_realloc"
effects = [
    { kind = "return_alias", param = 0 },
    { kind = "return_heap" },
]
```

**Disabling or declaring a no-op**

A named model is authoritative for its effect group, so an explicitly empty
effect list disables a same-name built-in or marks an opaque function as doing
nothing the analysis tracks:

```toml
[[model]]
name = "memcpy"
effects = []

[[model]]
name = "framework_barrier"
effects = []
```

A plain-identifier model also covers a namespace-qualified call's last segment
for `dlsym`/`invoke`/`entry`/`ipc`; argument and return effects take the exact
name only (plus a leading `::` or `std::`). See [Model matching](#model-matching)
for the qualified-member and constructor rules.

### Documented imprecision

- **`mem_copy` as aliasing**: true memcpy makes contents equal; modeling it as
  `pts(dst) ⊇ pts(src)` reproduces every field/value read through the destination
  (field cells and summaries of the source objects become visible), but stores through
  the destination may also land in source-side field cells (over-approximation, sound
  for may-analysis).
- **Member-address arguments**: resolved members carry their field address
  (see [Argument flow](POINTERS.md#argument-flow)); unresolved paths and array elements
  may still carry the base variable. Only these fallbacks are marked in
  `addr_of_member_args` and refused by `alias`; precise member addresses admit
  the model. `mem_copy` admits fallbacks conservatively, which can pollute other
  fields when the actual is still the enclosing container.
- `return_alias` and `return_heap` are fallbacks when return-flow expansion
  supplies no fact.
- **`return_field` resolves a recorded member.** The getter must declare a
  pointer-like result and have no indexed body. An indexed body that returns
  `0` still suppresses the model. The model projects `field` from the call
  site's actual argument at `param`, which can be the receiver of a non-static
  member.
  With a known call site, an argument with no participating value, such as
  literal `0`, contributes nothing. An initializer without a call record
  can use a recorded formal. The type comes from the formal when it supplies
  an aggregate, otherwise from the actual. It is stripped of one pointer
  layer, with an object receiver supplying its aggregate directly. A
  `void` or unresolved pointee, a deeper pointer (`T **`), or an aggregate
  without the named member contributes nothing. When the return type matches
  a pointer-valued member, a persistent load reads the member. Otherwise the
  result points at the member. Returning a scalar member's value is outside
  the pointer flow the analysis tracks.
- Terminators kill nothing (see above).
- **`invoke` is an immediate, unordered call**: the callback is reachable from
  the site that hands it over, with no notion of the new thread, of when it
  runs, or of its return value; a `rest` form checks arity only by a lower bound (see
  [Callback invocation](#callback-invocation-invoke)). The context kind is the
  model's word for it: `ffrt::queue::submit` is a `serial_task` although an
  `ffrt::queue` can be created concurrent, and an execution context is keyed
  by start site, not by thread or executor (see
  [Execution contexts](#execution-contexts)).
- **The task and thread primitives' kinds** are those of the usual
  configuration: an `EventHandler` posts to the one thread of its event
  runner, so the `PostTask` family is a `serial_task` (`PostSyncTask` too,
  although its caller waits); the callbacks of one `Utils::Timer` run one at
  a time on the timer's thread (`serial_task`); an HDF work queue is a single
  thread (`serial_task`), and the work function is started where the work is
  initialised, not where `HdfAddWork` queues it; an OSAL timer's function runs
  apart from its creator, on a thread of its own in the user-space adapter and
  as a kernel timer callback in the Linux one (`thread`). `std::async` is a
  `thread` whether or not a policy lets the library defer the callable to the
  caller's `get`. Whether its first argument is a policy is read off what the
  call site records there ([Callback invocation](#callback-invocation-invoke)):
  where that cannot tell (a variable of an unresolved type, an expression the
  site records nothing for that is not a policy), the call is read both ways,
  and a function passed as the callable's argument is a callback too. A
  `std::jthread` callable takes the stop token by its declared first
  parameter, not by whether it could be invoked with one, and the token
  carries no flow.
- **ffrt's C forms take a function header** (`ffrt_function_header_t *`)
  whose `exec` member the runtime calls with the header. The models invoke
  what the header argument itself may point to: a header object, the usual
  case, holds no function, and neither the header's members nor the return
  of `ffrt::create_function_wrapper` (a template outside the tree, with no
  model) are looked into, so a C-form submission is a context only where the
  argument may hold the function itself.
- **An `entry` is every definition of the member or an override of it** the
  tree holds, whether or not anything constructs its class or starts it
  (`Thread::Start`, a handler's runner, `AddDeathRecipient`); one declared
  but defined elsewhere is none. Where the tree declares the model's member
  (`virtual bool Run()` on the framework's class), a definition of the name
  overrides it only with the same number of parameters, of the types the
  declaration spells (`trace_ir::spelled_alike` under `Entry`: a class with
  and without its qualifiers, a template argument of a class-template base
  against the override's, a type the index does not know against any; an
  `int` lowering read an unresolved name as stands in only for a class the
  tree does not declare), each bound as the declaration binds it, by
  reference or by value (`reference_params`; a reference and a pointer
  lower to one pointer type): `Run(int *)` beside it is an overload, and
  so are `ProcessEvent(int *)` and `ProcessEvent(InnerEvent *)` beside
  `ProcessEvent(const InnerEvent &)`, and none is an entry. Without such a
  declaration, the name alone matches.

## OpenHarmony IPC bridges

For an in-tree OpenHarmony proxy/stub pair, analysis adds a synthetic call
edge from a proxy method that calls `SendRequest` or `SendTransactCmd` to the
corresponding stub handler. The final callable name segment must match exactly;
qualified, unqualified, `remote->` and `remote.` spellings are accepted, including
explicit template arguments and the member `template` keyword, while
names such as `SendTransactCmdAsync` and `LogSendTransactCmdResult` are not.
Unevaluated calls do not count. `SendTransactCmd` is the OpenHarmony proxy
helper that forwards a transaction to `SendRequest`; recognizing it marks the
calling proxy method as a sender even when the helper body is unavailable.
Other helper names are intentionally not recognized: sender status does not
propagate through calls to arbitrary transport wrappers.
The same predicate records IPC senders in exported linking facts, so
`trace-merge` can reconstruct pairs indexed in separate repositories.
Proxy-side databases indexed before `SendTransactCmd` support must be
re-indexed to export those senders; the linking-facts format remains compatible
and merging an older database does not warn about missing senders.

For `FooProxy::Run() -> FooProxy::SendTransactCmd() -> remote->SendRequest()`,
the synthetic bridge connects `FooProxy::Run` to its matching stub handler.
The operation's name supplies the handler correspondence; a shared transport
helper does not identify which handler processes a transaction. Ordinary
calls to the helper remain, and resolving the final remote-object call still
requires sufficient receiver/declaration information. This does not add
opcode dispatch or parcel-flow analysis. Production verification of this
pattern is recorded in the [evaluation report](EVAL_REPORT.md#ipc-sends-through-sendtransactcmd--2026-11-06).

Classes are paired by their qualified `*Proxy`/`*Client` and `*Stub` names, or
by a `*Host` name when that class defines `OnRemoteRequest`, defines a
`HandleX` handler on a declared class, or reaches
`IRemoteStub`/`IPCObjectStub` through ordinary or template bases
(`AppSchedulerHost`, `RenderSchedulerHost`). When both the `*Stub` and `*Host`
spellings are indexed, both are evaluated and their bridges unioned; a handler
both candidates resolve is emitted once. A `*Host` that no `*Stub` record names
uses the proxy's `.idl` record for its descriptor and unknown interface,
unless the Host already has an explicit `IRemoteStub<I>` interface.
Methods match exactly, with `HandleX` and `XStub` handler fallbacks.
For a stub with interface-only declarations, defined overrides in classes
deriving from that stub are preferred; otherwise matching methods on the
stub's transitive interface bases are retained, including default method
bodies and bodyless declarations. Interface bases are recovered from both
ordinary inheritance and the interface argument of an exact
`IRemoteStub<IFoo>` base. An unqualified interface argument is resolved in the
stub declaration namespace, independently of a qualified wrapper namespace;
a relative qualified argument such as `api::IFoo` searches the declaration
namespace and its enclosing namespaces, while only `::api::IFoo` forces global
lookup. Bodyless methods are intentionally leaf targets when no implementation
is indexed. When the `IRemoteStub` definition itself is in the tree, the stub
also has a real inheritance edge to `IFoo`
([Template-parameter bases](CXX.md#template-parameter-bases)); the argument
recovery above covers the common case where it is not.
A proxy and a stub whose names pair are bridged only when their interfaces
can be one. Each side's interfaces are the arguments of its `IRemoteProxy<I>`
or `IProxyBroker<I>` proxy bases or `IRemoteStub<I>` stub bases, written on
the class or on one of its bases, read as lowering reads a template argument:
each candidate in the lookup order
above that is a declared class, or a `typedef`/`using` alias of one. Every
such reading counts, not only the nearest, because the merged index cannot
tell which of two declared namesakes the class's unit sees. A bare spelling
(`IRemoteProxy<IFoo>`) also reads as every declared class of that final name
(`api::IFoo`), and as the class every alias of that final name stands for
(`using IFoo = IReal` in an imported namespace), since a using-directive,
which the index does not record, may import it. An explicitly global
`IRemoteProxy<::IFoo>` names the global class only. A dependent
base's argument (`template <class I> class ProxyBase : public IRemoteProxy<I>`)
is a template parameter, not an interface. When both sides name an interface
and no proxy reading is a stub reading or derives from or to one, the pair
is two services that merely share a name stem, and no bridge is added. When
either side's interface is unknown (an argument naming no declared class,
such as one known only from an out-of-class method), pairing stays
name-based as before: dropping a bridge takes more evidence than following
one. The interface fallback above searches the stub's nearest candidate the
IR represents, read as a declared class or alias when it is one, and walks a
recovered interface like a base: its own `IRemoteStub<I>` spellings are
followed too. The check is
not link-target scoped, like pairing itself. A mock or test double
implementing the same interface is not excluded: it stays a CHA target of
calls through the interface.
The edge has resolution `ipc` and no source call site, and can be disabled
with `--no-ipc`. Each handler a bridge targets is also an `ipc_handler`
[execution context](#execution-contexts).

When the framework's proxy and dispatcher are outside the analyzed tree, an
`ipc { target = "Base::method" }` function model can summarize that boundary.
For a bodyless modeled function, it adds a synthetic `ipc` edge to every
in-tree implementation that member lookup on `Base` or its subclasses reaches.
The shared inherited-method lookup retains the actual defining function,
including an implementation on an ancestor or a separate branch of a virtual
interface hierarchy. Repeated inherited targets produce one edge per sender.
Class matching uses the [model matching](#model-matching) rule, including
undeclared base spellings. All matching implementations and overloads are retained across
link targets; no URI/provider selection, parcel argument flow, or return flow
is inferred. In a bare tree, handlers obey
[declaring-header eligibility](ANALYSIS.md#declaring-header-eligibility) in **every known
calling context** of the modeled sender, including shared-header contributors.
A single sender node is shared by its callers: if any production context cannot
admit a test provider, test callers sharing that sender also cannot reach it.
Test-exclusive senders retain their mocks. A real declaration with no known
callers uses its declaring context; an unreferenced synthesized sender supplies
no declaration evidence and adds no summary. An arbitrary representative file
of a synthesized external never decides eligibility. `--no-test-partition`
disables this inferred partition. Authoritative link metadata disables it as
in ordinary resolution; IPC bridges still cross link images.

A defined sender uses its indexed body. A virtual declaration also uses its
reachable indexed overrides in the sender's image instead of a summary;
abstract interfaces without an indexed implementation retain the fallback.
The convenience Rust API `detect_ipc_pairs` uses built-in models; the pipeline
passes configured models to PAG construction.

Provider inheritance is resolved separately for each image and concrete receiver,
then unioned by defining `FnId`; final metadata in an independent service image
cannot suppress another provider. During `trace-merge`, retained IPC edges follow
the same bindings as incoming calls to their sender, including unresolved
declarations. If callers reach both a declaration and definitions, each retains
the bridge. Single-input merges preserve bridges without requiring a sender
definition, and all retained bridges keep synthetic NULL call sites. Merge
preserves these conservative summaries rather than revalidating framework absence.
Source-backed proxy/stub reconstruction follows the [linking rules](ANALYSIS.md#invariants-and-linking-rules).

Because v1 has no opcode or parcel-type information, overloaded handlers at a
matching handler-name tier are all retained as a may-analysis result. With `m`
proxy overloads and `n` handler overloads this deliberately emits the full
`m × n` cross-product, so IPC edge counts can grow for overloaded interfaces.
The interface fallback narrows instead: a proxy method and the service method
it implements share the interface's declared explicit parameters, so when at
least one candidate overload matches them, known candidates with different
parameters are dropped while the matches and any candidate whose parameters
are unknown remain. When no candidate matches, every candidate stays in the
may-set. A candidate whose parameters were not exported (cross-repository
linking) counts as unknown, not as zero arguments. When derived concrete
classes define only a different overload, a matching ancestor is preferred;
if neither the concrete method nor any ancestor matches, the concrete
implementation remains as the last may-target.

A proxy and stub synthesized from an `.idl` file
([IDL-generated interfaces](CXX.md#idl-generated-interfaces)) are registered from
`Program::idl_interfaces` instead: the proxy's IDL methods count as IPC sends
without a `SendRequest` call, the stub has no handlers so the interface
fallback above selects the service's overrides, and the bridge carries the
IDL descriptor. So does the bridge of any other sender that pairs with that
stub, a hand-written `AtmClient` beside the generated `AtmProxy`: the
descriptor is the stub's interface's. `ipccode` is recorded but not used for
matching yet. HDI's composed stubs use the implementation relationship
recorded by IDL synthesis; their interface also supplies compatibility
evidence when it is declared. Their pairing rules are described in
[IDL-generated interfaces](CXX.md#idl-generated-interfaces).

This is deliberately name-based and does not interpret transaction opcodes or
control flow in `OnRemoteRequest`. Synthetic IPC edges are appended after the
points-to fixpoint and do not feed the solver worklist. The analysis also does
not model parcel argument or return-value flow. See
[IPC_ROADMAP.md](IPC_ROADMAP.md) for the pattern study, validation targets, and
limitations.

### DataShare sources and fallbacks

**The DataShare `OpenFile` summary is opt-in.** It is absent from the built-in
model set; normal analysis still loads the built-in libc and callback models.
Choose the workflow according to the implementation evidence available:

| Available input | Workflow |
|---|---|
| Application and framework/service implementation sources | Analyze each repository independently and use `trace-merge` to recover the source-backed member and proxy/stub route. Generate the inputs without the DataShare summary. |
| Dependency/SDK headers only | Use `--include` and `--dep` for declarations, inheritance and receiver types. Dependency bodies do not contribute calls; analyze a framework's implementation separately when its sources become available. |
| Known framework behavior, but unavailable implementation | Load an explicit `--models` summary during `trace analyze`, with its target implementations present in the indexed sources. This supplies a conservative boundary shortcut. |

The DataShare fallback maps
`OHOS::DataShare::DataShareHelper::OpenFile` to
`OHOS::DataShare::DataShareStub::OpenFile` and its eligible indexed provider
implementations. It retains possible providers and overloads; URI routing and
parcel/value flow remain unmodeled. It applies to a bodyless sender, with
indexed bodies and virtual implementations taking precedence as described
above. The [configuration format](#configuration-format) includes the explicit
TOML rule; the [README fallback example](../README.md#missing-framework-fallbacks)
shows how to load it. An empty same-name model disables the configured fallback;
`--no-ipc` disables it together with detected bridges.

For source-backed cross-project navigation, follow the
[README analyze/merge/query example](../README.md#cross-project-analysis-with-framework-sources).
Merged outputs retain summary edges from their inputs: the merger does not
remove a shortcut after another input supplies framework sources. Re-analyze
any inputs previously produced with the DataShare fallback, omitting that rule,
then merge the new databases to follow the indexed framework route.
