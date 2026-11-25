# C++ support

Lowering and resolution for C++ constructs, including classes, templates, automatic objects and factory construction. Part of the [analysis documentation](ANALYSIS.md).

## C++ support (first step)

`.cpp/.cc/.cxx/C++` files are indexed as TUs and parsed with tree-sitter-cpp
(`SourceLang` per TU; headers inherit the including TU's grammar). Lowering is
C++-aware only where it must be — everything else reuses the C machinery.

- **Namespaces**: `ns_stack` qualifies declarations (`ns::f`). Anonymous
  namespaces get internal linkage. A C++17 `namespace A::B {` opens one
  scope per segment (tree-sitter spells it as a single
  `nested_namespace_specifier`; it used to read as an anonymous namespace),
  and a C++20 `namespace A::inline B {` names the inner scope `B`.
  Unqualified C++ calls resolve through
  **namespace-aware ordinary lookup**: the global namespace, enclosing
  namespaces (innermost to outermost), plus every namespace brought in by
  `using namespace X;`. **ADL** (argument-dependent / Koenig lookup):
  namespaces are derived from the qualified tag of each argument's
  `Struct`/`Union` type (pointer/array layers peeled), including its recorded
  transitive base classes, and merged into the candidate set, so `swap(a, b)` where `a` is `kit::Widget*` finds
  `kit::swap`. **`using X::f;`** imports the exact qualified function name
  into the candidate set of the bare base name. Relative `using`
  targets are expanded against the enclosing namespace stack:
  `using namespace detail;` inside `namespace a` records `a::detail`
  **and** the literal `detail` (`expand_using_target`), matching C++'s
  first-segment resolution; exact global lookups still win when the
  enclosing spelling does not exist. Header prototypes
  (`lower_function_decl`) are now namespace-qualified like definitions
  so they register under their correct `ns::f` spelling. `using`
  directives are **block-scoped**: file-scope directives apply TU-wide,
  but a directive written inside a function body, a namespace block, or a
  nested statement block (`if`/`for`/`while` body — itself a
  `compound_statement`) applies only to that block
  (length-snapshot/restore in `lower_function`, `lower_namespace`, and
  around each `compound_statement` in `walk_function_body`). Leaking them
  block-wide could let the overload ranking collapse away the correct
  in-scope edge, and is avoided.
- **Type names** are looked up the way C++ looks them up, by one shared
  walk (`find_in_scope`, #90): the class whose body or member is being
  lowered, its bases (a nested `Node` of `struct D : Base` is `Base::Node`),
  and each class around it (a class local to a member function
  also sees that function's class), then each enclosing namespace
  innermost first, then the global scope. Locals, parameters, fields,
  return types, template heads and arguments, bases, `new` and casts all
  go through it (a C-style cast by the type it names, without its `*`), a
  bare and a partially qualified spelling alike; a leading `::` asks the
  global scope only, typedefs included; the innermost declaration shadows
  an outer one. A typedef is found under its qualified name at
  each level before the flat table of bare names is read, so two
  namespaces declaring the same typedef name keep their own. Where nothing
  is declared, the spelling qualifies to the innermost namespace as
  before. The walk sees what the unit has declared so far; member function
  bodies are lowered after the whole class body, so they see member types,
  aliases and prototypes declared later in it, as C++'s complete-class
  context does. Every in-class definition's signature is registered, with
  its parameters, before any body is lowered, so a body also sees a member
  function *defined* further down the class (#96). `using namespace`
  directives are not searched (see the arrow section below).
- **Type aliases**: `using Alias = T;` (#91) registers like
  `typedef T Alias;`, pointer, array and function shapes included. An alias
  template (`template<class T> using V = ...`) is not lowered. A typedef
  or alias declared in a class body is a member of the class: it is
  registered only as `Cls::Alias`, since classes routinely reuse alias
  names (`Ptr`, `iterator`), and a class template's member alias is
  reached through an instantiation (`Holder<int>::Ptr`), a template that
  declares `operator->` included. A typedef or alias
  declared in a function body is scoped to its block and never reaches the
  unit's alias table; one declared in a class local to a function stays in
  that class.
- **Member classes** (#92): a class defined in another class's body is
  `Outer::Inner`, with a layout of its own, and its members are lowered
  under that tag rather than leaking into the outer class (a member walk
  used to read `class It { int x; int Next(); };` as a function member of
  the outer class named after its first field). Member class templates
  are included. A struct C would also accept, nested only in such
  structs (no `class` keyword, template, base, or member other than data
  fields), keeps its namespace tag instead, because C gives a nested
  struct file scope and a header shared by C and C++ units must name it
  alike in both; its `Outer::Inner` spelling is registered as an alias of
  that tag; a class nested in it still spells the whole path
  (`Outer::Inner::Deep`). A body-less specifier follows the same rules: a
  forward declaration in a class body (`struct Impl;`) declares the member
  class, a reference inside another declaration (`struct Node *next;`,
  `void f(struct Fwd *p)`) finds the class through the scope lookup and only
  declares a new one in the innermost namespace when nothing is found, and an
  out-of-line definition (`class Outer::Inner { ... }`) defines the class
  its outer class declared (for a member a C-compatible struct declared,
  the qualified spelling of its own, as before, so its methods stay with
  their definitions). `T(args)` or `ns::T(args)` where `T` names a
  class with a declared constructor, directly or through a function-local
  alias, is a constructor call, its arguments
  bound past the implicit `this`; inside a member class the outer class is
  not the implicit `this`, and the name of a class whose body is still
  being lowered constructs it even when its constructor is defined below
  the call (the site resolves by name). A function of that name declared at
  the same or a nearer scope hides the class, as in C++, and the call stays
  a function call. Every constructor path binds all its arguments past
  `this`, functions passed by name included — member initializer lists
  too (see **Methods** below). A member initializer of a member the index
  holds no class type for, with one expression (a pointer, a scalar, a
  `std::function`, which is interned as a function value; `cb_(cb)`,
  `cb_{cb}`), is instead the store `this->cb_ = cb`, as that body assignment
  is (`store_member_initializer`): the initializer names the member even
  where a parameter spells it too (`cb_(cb_)`), and a callback a constructor
  keeps this way reads back through the object. A literal (`count_(0)`,
  `ok_(false)`, `p_(nullptr)`) and a `bool` or floating member hold no
  address and store nothing.
  Constructor-site eligibility and reference bindings follow the
  [Ctors / dtors](#c-support-first-step) rule below, including declarations
  imported from cached headers for nested classes and member class templates.
- **Overloads**: same-name entries are kept apart when **both** sides are C++
  and arity (or same-arity param types) differ (`add_function`;
  `externals_by_name` bucket). Signature comparison uses real types: at TU
  merge the incoming params are remapped into global `TypeId` space by
  `merge_unit_index` and passed via `add_function_with_param_types`, so a
  cross-TU prototype + definition of the same function collapse into one
  record instead of duplicating (two `functions` rows / two callgraph edges),
  while distinct same-arity overloads still separate across TUs. The merge
  gate resolves the surviving entry's types through `Function::param_type_ids`
  (remapped into global type space at merge; unit-local VarIds are not
  queryable mid-merge). Header entities merge by their position (file,
  line, column), so overloads one macro declares on one line share its
  expansion column and merge into one entry: a known limit, since neither a
  unit's own types (a header prototype can be met twice in one unit, once
  before its parameter type resolves) nor the signature (units spell one
  prototype's types differently) tells those overloads from copies of one
  function. A C `.c`
  body still merges with a C++-parsed `.h` prototype of the same arity —
  otherwise callers bind to the undefined prototype (HDF `GpioSetIrq` /
  `gpio->func`). A C++ prototype and definition of which one has C linkage
  (`extern "C"`, not `static`; `Function::c_linkage`) meet when their
  parameter types are spelled alike as a definition meets an in-class
  prototype, an `int` standing in for a class or a function pointer: a
  defining unit that does not see the header's typedefs reads `Callbacks*` or
  a callback typedef `OnChange cb` as an `int` stand-in (camera's
  `OH_CaptureSession_*` entry points). A C++ overload of another known type
  (`void f(double)` beside `extern "C" void f(int)`) stays apart, and two
  definitions never meet this way.
  Calls resolve over the candidate set filtered by argument
  count; an empty arity-filtered set falls back to all candidates (varargs).
  The count a call must fit is a range: every declaration records its
  **explicit arity** (`Function::explicit_arity`, parameters without `this`)
  and how many parameters carry a default (`default_args`), and a call
  passing `n` arguments fits `arity - defaults <= n <= arity`, or any
  `n >= arity - defaults` when the list ends in `...` or a parameter pack
  (`Function::variadic`). As in C++, a variadic candidate ranks below any fixed
  candidate that can take the arguments, so it is dropped only when a fixed one
  confidently can: every argument's type is known, no pointer binds an
  arithmetic parameter and no floating-point value binds a pointer.
  `Log(p, p)` reaches `Log(void*, ...)` beside `Log(int, int)`, `Show(2.5)`
  reaches `Show(double, ...)` beside `Show(const char*)`, and
  `Notify("ready", 1)` keeps `Notify(const char*, int)` over
  `Notify(const char*, ...)`; an argument of unknown type keeps both. A variadic and a fixed declaration of
  one explicit arity are two overloads and never merge (`Next(T&)` beside
  `Next(T&, Args&...)`). C++ writes
  defaults on the declaration, so a candidate also takes the defaults of a
  same-arity candidate of loosely the same signature (parameter types equal up
  to class qualification, an unresolved type name lowering as `int` matching
  a class): a definition the index could not reunite with its prototype
  (`string` beside `std::string`) still accepts `TrimStr(s)`, while
  `Format(cb, cb)` does not borrow `Format(int, int = 0)`'s default.
  An in-class prototype lowers no parameter variables, and a parameterless
  entry used to merge with a definition of any arity; between two C++
  entries the explicit arities must now agree, so `Get()` and `Get(Mode&)`
  stay two entries wherever only their class header is seen, and neither
  merges into an unrelated same-named body of another arity. Explicit arity
  excludes `this`, so a body lowered without its class in view (its header
  not found) still merges with its prototype.
  A member function reached by its qualified name (`Base::m(a)`,
  `Cls::Static(a)`) is counted and ranked without its implicit `this`, so
  `Base::m(cb, 1)` picks `m(Callback, int)` rather than `m(int)`.
  Ties emit one direct site per candidate. Same-arity C++ overloads
  additionally rank by **static argument type**: `CallArgs.arg_desc`
  carries each argument's `TypeDesc` (casts unwrapped, numeric literal
  width, `char`, `true`/`false`, string, `nullptr`, plus var/field/subscript
  declared types); a unique exact param-type match wins, otherwise the
  full arity set is kept (may-approx).
- **Template member calls**: `obj.GetNumber<int>()` parses its method slot
  as `template_method`; both it and `template_type` route to the primary
  name, so `fv.GetNumber<int>()` / `b.read<short>()` resolve directly.
  In-class template methods (`template_declaration` members in a class
  body) register as prototypes and lower their inner `function_definition`.
- **Friend declarations and operator attributes**: Tree-sitter C++ grammar admits
  only `[constexpr] friend (declaration | function_definition)` for
  `friend_declaration`, rejecting attribute specifiers that precede `friend` or
  `constexpr friend` (such as `[[nodiscard]] friend bool operator==(...)`). Before
  parsing, C++ sources undergo parse-syntax normalization
  (`normalize_cpp_parse_syntax`) where attribute specifiers preceding `friend`
  (including those preceding `constexpr friend`) are blanked with ASCII spaces
  in place, strictly preserving newlines and byte coordinates (the rewrite
  rules live in [C++ parse-input normalization](#c-parse-input-normalization)).
  Friend lowering
  (`lower_friend_declarations`) lowers in-class friend function definitions and
  friend operator declarations as non-member functions belonging to the innermost
  enclosing namespace ([class.friend]) with `ctx.class_ctx = None`. Friend functions
  do not receive an implicit `this` parameter, are not qualified with class scope,
  and do not resolve identifiers via `implicit_this`, correctly matching call sites
  and merging with out-of-class definitions. During body lowering, lexical class
  lookup (`ctx.friend_classes`) is preserved for direct calls within the friend body
  so static member calls (e.g. `helper()`) resolve to members of the enclosing class
  and its bases, shadowing outer namespace definitions in accordance with standard
  C++ scope hiding rules. Friend bodies are lowered only after the class's in-class
  member definitions have entries (`DeferredFriendBody`), so a bare call to a
  static member defined inline in the class resolves to that member too.
  Function-local / block-scoped using declarations (`using X::f;`) within the body
  take precedence, shadowing both enclosing class and namespace scopes.
- **Classes**: layouts intern under the fully qualified tag
  (`gfx::Shape`). Inheritance facts (`Program.inheritance`) drive member
  resolution: a call walks upward to the nearest declaring base. **Non-virtual**
  methods resolve exactly to that declaring function; **`virtual` methods and
  destructors** additionally expand downward through the subclass closure
  (one site per target — delete-through-base is the dominant dtor pattern),
  except at an automatic object's sites, whose receiver is exactly its class
  ([Automatic objects](#automatic-objects)).
  Expansion runs again **after TU merge** so overrides declared later in the
  same file or in other TUs are included. Downward expansion is rooted at the
  **static receiver type**. Targets are filtered by **explicit arity**
  (`params` minus implicit `this`, or the arity a parameterless prototype
  declared, within its default arguments; an entry recording neither stays,
  and an empty filtered set falls back to all candidates so varargs still
  resolve). A
  `final` class, or a method declared `final`,
  cuts off further subclasses (devirtualization). C++ `struct` inheritance
  and **virtual bases** (`class D : virtual B`) are recorded the same way as
  ordinary bases for CHA (diamond override sets include the most-derived
  override). When no ancestor declares the member, the call falls back to
  the receiver's static-type subclass closure. A base reached through a
  template parameter (`class S : public IRemoteStub<IFoo>` with
  `template<class I> class IRemoteStub : public I`) is recorded as an
  ordinary base when the template's definition is in the analyzed tree; see
  [Template-parameter bases](#template-parameter-bases).
  A bare instance field in a member body is rooted at `this`
  ([Implicit this member variable access](POINTERS.md#implicit-this-member-variable-access)).
- **Implicit `this->method()`**: a bare identifier call inside a method
  (`OnEvent()` from `OnEventProxy`) is rewritten as a member call on the
  enclosing class when that class (or a base) declares the method. This
  runs before free-function name lookup so it does not synthesize an
  unqualified external stub. A body can call a member its class defines
  further down (#96), so a class body registers every in-class definition's
  entry, parameters included, before it lowers any body. A call to a member of
  the class (implicit `this`, `this->m`, another object of the class, or a
  construction of it) then sees every overload, wherever it is defined, and
  resolves like any member call (arity filter, arguments past `this`). It wins
  over a base member or a global function of the same name, as C++
  class-scope lookup does.
- **Classes in an anonymous namespace**: their members have internal linkage,
  indexed per file like a `static` function so a same-named class in another
  file stays a different class. Member lookup names a member by its class
  (`Cls::m`) without a file, so internal members declared in their class are
  also indexed by qualified name (`SymbolTable::functions_named`): calls,
  constructions and virtual dispatch reach them like any member. Overloads of
  a C++ function with internal linkage, a member or a free `static` /
  anonymous-namespace function, stay separate entries, told apart by
  signature (parameter types, not only arity) as external overloads are:
  `fn_by_scope` keeps the first per file and name, and
  `SymbolTable::scope_overloads` the rest, which `resolve_function_candidates`
  returns with it (`resolve_function_in_scope` still answers the first). A
  declaration no definition joined when it was registered is defined where
  registration could not see it: under another spelling of a parameter type
  (`static void f(ns::Obj *);` and `static void f(Obj *) {}` under `using
  namespace ns`, since a type name lowering could not resolve reads as
  `int`), or in another file of the unit. A called but undefined `static`
  function does not link, so the unit merge (`respelled_declarations`) joins
  such a declaration to the one definition of its name and arity with exactly
  its parameter types, else the one whose parameter types may name them
  (`may_name_same_type`: a qualified name beside its unqualified suffix,
  `ns::Obj` and `Obj` though not `ns1::Config` and `ns2::Config`, a type read
  as `int` beside a named one, function pointers whose parameter lists agree
  where both are known); a later exact definition therefore keeps its
  declaration, and `f(int)` beside `f(double)` stays two. A
  function's address taken by name (`void (*p)(double) = cb;`, `f(cb)`, a
  function table, `return cb;`) resolves to the first entry; after the merge
  it is widened to every overload, since a function pointer's type does not
  carry the parameter types that would pick one. A C++ internal-linkage
  function finds its declaration and overloads in its own file, else in a
  header that file includes, since both are one translation unit: a header's
  `static void Log(int)` and the `.cpp`'s `Log(double)` are one overload set,
  and a header's anonymous-namespace class member defined in the `.cpp`
  merges with its in-class prototype, `virtual` included. A header's entry is
  every including unit's, so an overload one unit's `.cpp` adds is used only
  where that file is seen (`internal_overloads_seen_from`), and a free
  function found through a header is never folded into the entry it matches
  there, whether the header's declaration or another unit's
  `static void f(double)`: it stays its own unit's function, and the unit
  merge joins that unit's calls to it. Only the same header text lowered once
  more for the unit (same file and line) is that entry. A member of a
  class in an anonymous namespace is internal wherever its definition is
  written, after the namespace closes or in the `.cpp`, and an overload of a
  member defined outside the class is still found by member lookup. C keeps
  one entry per file and name.
  The qualified index does not say which file's class a member belongs to, so
  `Program::anonymous_classes` records the files that define each class in an
  anonymous namespace, and `Program::anonymous_final_classes` those declaring
  it `final` (an external `final` class stays in `final_classes`, by name).
  After the merge, virtual-call expansion ignores a class or member that
  neither the call's file nor, for a callee bound to an internal member, that
  member's file can see (the file itself or a header it includes): it does not
  make the callee virtual, never stops dispatch as `final`, and is no target
  when its class is the receiver's. `Program::anonymous_bases` records each
  anonymous class's bases with the file deriving it, since `inheritance`
  joins every class of a name: a member the call cannot see, reached through
  a same-named class of the receiver's hierarchy, is a target only when its
  own file derives its class from a class of that hierarchy. A walk toward the
  bases of such a class seen from the call's files follows those bases too,
  not every file's class of its name. When the receiver is a class in an
  anonymous namespace that
  file sees, its whole hierarchy is there too, since a subclass has to name
  it, so a member of a class in that hierarchy is a target only where the
  call sees it: a same-named external class and its subclasses are not. A
  call through a base declared outside an anonymous namespace still reaches
  the overrides in every file.
  Dispatch filters declarations to the selected virtual member's owner and
  override signature before nearest-base lookup or final-method cutoffs. A
  sibling base's same-named member and a derived hiding overload do not stop
  that search, including for member-pointer invocations.
- **Dependent virtual receivers**: when a virtual call is already bound to a
  member but its receiver names no declared class (for example a template
  parameter `T`), dispatch starts from the bound member's owner and retains
  its possible overrides. Direct and member-pointer calls share this rule;
  exact receivers and final members retain their ordinary restrictions.
- **Constructors are never dispatched**: when no constructor of a class is in
  view, a construction (`: Base(a)`, `new Base(a)`, `Base b(a)`) keeps an
  unresolved site that resolves by name after the merge. Member lookup's
  fallback to the subclass closure is for virtual calls only; applied to a
  constructor it reached the derived constructor the initializer is written in
  and every sibling's.
- **Field receivers**: a bare identifier used as a member-call receiver
  (`plugin_->OnEvent()` inside a method) is looked up as a data member of
  the enclosing class (and bases) when it is not a local/param, so
  `shared_ptr<Plugin>` fields unwrap like parameters.
- **Smart pointers / `operator->`**: a wrapper is recognised by the
  `operator->` it declares, not by its name (#64). Its instantiation interns
  as `Struct{sptr<T>}` — the wrapper stays the variable's class, so `sp.Get()`
  is the wrapper's member — and `p->m()` looks `m` up on what the arrow
  returns: for a class template, the argument at the declared parameter's
  position (`Handle<Meta, T>` returning `T *` takes the second); for a named
  class, that class, followed on through a chain of up to eight links with
  cycles cut; for a raw pointer, its pointee, which ends the chain. `*sp` is
  the same pointee. A raw pointer receiver, `this` included, is the built-in
  arrow, and so is an array: `m_items->Run()` on an array member calls its
  element class's `Run`. What each `operator->` returns is recorded as an `ArrowReturn` fact
  merged with a header's types, so a wrapper-typed field declared in a
  different header from the wrapper resolves too. Where the chain names no
  class — overloads that disagree, a dependent return the index cannot name
  (`sptr<T>`, or a parameter of a base template), a cycle — the site is left
  unresolved: a member invented on the wrapper would be indistinguishable
  from a real call out of tree. `shared_ptr` / `unique_ptr` / `weak_ptr` keep
  a name-based fallback to their first argument for the common case that
  their header is outside the tree and there is no declaration to ask; a
  guess from a name, not a resolution. A wrapper whose class **body** is not
  in the tree — absent, or only forward-declared — is guessed the same way
  from its arguments (#86): with exactly one argument that names a declared
  class, `->` looks the member up on that class; `sptr<T>` / `wptr<T>` and
  namespace-qualified spellings resolve without their header, on locals,
  parameters and fields. Two or more arguments, a scalar, pointer or
  reference argument or an unknown class leave the site unresolved. The
  argument is looked up through the enclosing namespaces innermost first
  and then the global scope, on bare and partially qualified spellings,
  through a typedef and past a trailing `const`, and a leading `::` names
  the global class with no enclosing namespace searched; `using namespace`
  directives are not searched, since an out-of-line member defined under
  one is indexed under the bare class name and only the bare spelling
  reaches that body. `(*sp).m()` on such a wrapper is the same guess as
  `sp->m()`, so the two spellings agree. A nested type of an out-of-tree
  template (`Outer<A>::Inner`, `std::vector<T>::iterator`) keeps its tail
  on the tag: it is a type of its own, not a wrapper around `A`, and an
  arrow on it stays unresolved rather than guessing `A` or inventing a
  member on the nested type. In a template spelling a keyword scalar, a
  fixed-width integer name or a function type (`void(int, char)`, split on
  its own commas correctly) is kept as written; only class names are
  qualified. The wrapper's own name goes through the same enclosing-scope
  lookup as its arguments, so a wrapper declared in an outer namespace and
  spelled bare in a nested one keeps its declared `operator->` and its `.`
  members, and a defined class template spelled with its arguments takes
  the class that lookup found as its tag. A typedef is registered under its qualified name as well as its
  bare one and matched whole, never by its last segment alone. An
  out-of-line `operator->` seen without its class header records its
  return under the class its name spells and defines that wrapper. A field
  step follows its operator: `sp->f` on any of these wrappers steps to the
  pointee's layout, on the wrapper variable itself as well as along a field
  chain, so field reads and writes through a smart pointer carry the same
  load and store constraints as through a raw pointer, while `w.f` stays
  the wrapper's own field. Parentheses inside the chain do not end it:
  `(a->b)->c` is decomposed as `a->b->c`, not as `c` on `a`'s layout.
  One rule decides what a wrapper is whether or not
  the spelling carries arguments, so a concrete class that inherits
  `operator->` from a base steps through it as an instantiation does.
  A raw `Wrapper<T>*` uses the built-in arrow:
  `p->f` stays on the wrapper's own layout, including callback fields and
  raw-pointer fields reached along a chain; it does not invoke `operator->`.
  References to wrapper values and explicitly dereferenced wrapper pointers
  still use the overloaded arrow. `(*sp).f` uses the same pointee field
  summary under the existing assumption that a wrapper's `operator*`
  yields the pointee of its `operator->`; dereferencing a raw pointer to
  a wrapper keeps the wrapper's own fields. At an overloaded arrow, field
  lowering starts the remaining path on a synthetic receiver typed as the
  pointee.
  Its GEP resolves to the pointee's instance-insensitive `FieldSummary`, shared
  with raw-pointer reads and writes. Wrapper storage is not treated as an
  inline pointee subobject; wrapper identity is intentionally not tracked by
  this summary model. A member type keeps its `::` and its own name
  when it carries arguments (`Outer<A>::Inner<B>`); a member type of a
  defined template keeps the class the lookup found as its prefix; `::W<T>`
  is the global `W`, tagged without the prefix, wherever a template head
  is looked up; `nullptr_t`, `intmax_t`, `uintmax_t`, `auto` and a literal
  (`4`, `true`) are never qualified; `T*const` is the pointer argument
  `T*`, and a pointer level survives a qualifier between levels
  (`T * const *` is `T**`). An argument spelled through a C++11
  `using Alias = T;` resolves as a typedef does. Class declarations and definitions are tracked in the
  type table (`declare_struct` / `define_struct`), travel with a header's
  types, and are what tells a forward-declared wrapper from a defined one.
  Only the terminal member call is
  emitted — the implicit calls to each `operator->` are not represented —
  and a reference member holding a wrapper lowers as a pointer and is read
  as one.
- **Callables**: only `std::function<Sig>` / `::std::function<Sig>` intern
  as `FnPtr` so assignment and field stores of function addresses
  participate in indirect-call resolution. Other types whose last segment
  is `function` stay ordinary classes. Lambdas lower to synthetic
  `$lambda` functions with `AddrOfFn` on init/assign/arg (captures
  unmodeled). A lambda invoked where it is written (`[&] { ... }()`,
  parenthesized or not, the expansion of an `IN_PROCESS_CALL`-style wrapper)
  is a direct call site bound to that function by its generated name, with
  its arguments bound to the lambda's parameters and its returns reaching the
  call's destination. Lambdas one macro
  expansion writes all take the invocation's position, so a later lambda with
  a different body takes the next free column in its generated name, and
  lambdas with equal bodies share one function. Functors (`operator()`) are member calls, including `obj()`
  and `h->field()` when `field`'s type declares `operator()`. Callable
  data members that are not methods fall through to the C fn-ptr
  field-load path.
- **Methods**: out-of-class definitions (`Ret Cls::m()`) merge with their
  in-class prototypes. An implicit `this` parameter (`Ptr(Struct{Cls})`,
  param index 0) is prepended to every member definition, static members
  included; in-class prototypes carry no parameters, only their declared
  arity, and take their definition's list at merge. `static` on a member
  defined in its class body makes a static member with external linkage,
  not an internal function: read as internal, a static member template's
  body never merged with its prototype on the same line and was dropped. `virtual` flags survive merges. A call site's
  argument positions are the callee's parameter positions, so every call to
  a member function binds its explicit arguments from position 1, past
  `this` (#93, #94): `recv.m(a)`, `p->m(a)`, implicit `m(a)` and
  `this->m(a)`, `Base::m(a)` and `Cls::Static(a)`, functors (`obj(a)`,
  `h.field(a)`), constructions (`new T(a)`, `T x(a, b)`, `T(a)`) and
  member initializer lists (`Base(a)`, `m_(a)`, `Base{a}`, `m_{a}`). Only a
  construction binds anything to `this` — the object it constructs (a
  declared local or the `new` allocation); a method call's receiver is not
  bound. Every site records whether its positions count `this`
  (`CallSite::args_bound_past_this`). A unit that never saw a class cannot
  supply `this` during lowering, but ordinary call and return-type lookup
  still consult the definition's written qualifier through the shared
  enclosing-scope lookup. Thus `GetCallbacks()` inside `Context::Dispatch`
  reaches an indexed `Context::GetCallbacks` even without the class header,
  including its overloads, instead of disappearing as a bare direct call.
  The lookup does not invent members for names absent from that scope.
  Without the class declaration, two cases involving implicit parameters are
  finished after the merge, in order:
  - A member *defined* in such a unit (its header include unresolved) lowers
    without `this` yet merges with the in-class prototype, whose explicit
    arity agrees; a parameterless one (`void Remote::Reset() {}`) too. The
    prototype is known to be a member by where it was declared
    (`Function::declared_in_class`), which is also what makes a parameterless
    entry a member for `is_member_function`, so a namespace function defined
    out of line (`void util::Init() {}`) never takes `this`. A body qualifies only when it was written under a qualifier
    its unit did not know (`Function::owner_unresolved`: out-of-line
    `void Remote::Shared(Callback cb) {}`); a function defined inside a
    `namespace` block never does, so an unrelated program's namespace of the
    same name as a class (hiview's `TimeUtil`), even one declaring the same
    member, is left alone. The merge records the pairing
    (`SymbolTable::members_missing_this`) and `add_missing_this_params`
    prepends `this`, taking the class from the prototype's name.
  - `bind_calls_past_this` then binds every site whose callee takes `this`
    but whose arguments were not bound: a qualified call whose unit never
    saw the class, resolved by name (`SymbolTable::callees_of`, the solver's
    own rule), and a call beside a definition that has just gained its
    `this`.

  Before this, the first argument of a method call landed on `this`,
  so a callback or pointer handed to a method never reached its parameter.
<a id="explicit-member-operator-calls"></a>
- **Explicit member operator calls**: explicit operator invocations using
  member-access syntax, such as `value.operator->()` or `a.operator>(b)`, are
  admitted in expression and trailing-return-type positions. Tree-sitter C++
  parses the explicit operator segment (`.operator->` or `->operator>`)
  inside an `ERROR` node under `call_expression`, placing the receiver in the
  `function` field. The parser classifies these as benign parse errors so no
  spurious parse diagnostic is generated. Lowering recovers the receiver and
  target operator name, resolving the call to the member operator
  (`Cls::operator->`, `Cls::operator>`) with the same callee identity as
  ordinary operator syntax, without treating the receiver variable as a callee
  or synthesizing isolated functions named `->` or `>`. In dependency headers,
  expressions in trailing return types preserve the surrounding signature
  without creating call sites.
- **Conversion operators**: `operator T()` is a member named
  `Cls::operator T`, spelled from its `operator_cast` declarator: the name
  runs to the declarator's own parameter list, so the target type keeps its
  pointer and reference layers (`operator const char*`, `operator T*`,
  `operator void(*)`) and its template arguments (`operator Vec<int>`) while
  the `()` and cv-qualifiers are dropped. Of the target's own scopes, only
  those the *member itself* sits in are dropped — any contiguous run of the
  enclosing segments, so `T`, `H::T`, `b::H::T` and `a::b::H::T` are one
  member for a class `a::b::H`, and each qualified name in the target takes
  the longest run that applies to it, a template argument's differing from
  the head's: `ns::Handle::operator ns::S`
  and the in-class `operator S` written inside `namespace ns` are one member,
  since how far the author had to qualify the type is an accident of where
  the declaration sits — but a scope the member does not sit in could not
  have been elided at either spelling, so it is kept and
  `C::operator a::S` stays distinct from `C::operator b::S`. A leading `::`
  follows the same rule: dropped when what follows re-spells a scope the
  member sits in (`operator ::ns::S` for a member of `ns`), kept otherwise,
  since it is all that separates a global type from one the member's own
  namespace shadows (`operator ::S` beside `operator S` inside a
  `namespace n` that declares its own `S`). That question is settled by the
  top-level target alone — a template argument shedding its own scope says
  nothing about whether the `::` in front was redundant. A `::` in the
  target therefore never qualifies the member out of its class either.
  Declaration, in-class definition and out-of-class definition
  (`Cls::operator T()`) all merge, and a definition returns the type it
  converts to — a function-pointer target lowering to `Ptr(FnPtr{..})`, the
  same descriptor the `typedef`ed spelling of it produces — though see the
  prototype note below for when that survives the merge. One limit follows
  from naming the member after the *spelling* of its target type: two
  spellings of one type are two members (`operator const char*` from the
  declaration and `operator char const*` from an out-of-class definition do
  not merge). The recorded *type* is the coarser of the two in one place: a
  function-pointer target's `FnPtr` carries no parameters, so
  `operator int(*)(char)` and `operator int(*)(long)` — neither valid C++
  without a typedef — are two members by name, since the name runs to the
  *member's* own parameter list and so keeps the target's, but both record
  `Ptr(FnPtr{Int, params: []})`. A conversion to a reference to an
  array (`operator int (&())[3]`) is the one target the name cannot spell: it
  is the only one whose declarator wraps *around* the parameter list rather
  than ending at it, so the name is cut mid-spelling as
  `operator int(&`. Self-consistent, so declaration and definition still
  merge, but not a spelling anyone would recognise.
- **Attribute macros around a conversion operator**: an unknown macro
  (no `#define` in the include path) takes the `type` field and leaves the
  `operator` keyword stranded in an `ERROR`, so the target type stands where
  the declared name belongs. Both shapes are read back to the ordinary
  spelling: in a class body `MACRO operator ns::S() const;` is
  `Cls::operator ns::S`, not the member `Cls::S` it used to be; out of one,
  `EXPORT Cls::operator int() {}` keeps its `Cls::` instead of losing it to
  the fabricated-qualification repair below and escaping to global scope.
  The target is read from the *source text*, not by walking the declarator
  the `ERROR` parks it in: a declarator yields the one identifier it is named
  by, which is a target's last segment only, and every other part of the
  spelling — `ns::`, `<int>`, `(*)` — decides which member it is. The
  spelling runs from the keyword to the end of the declarator the member's
  own parameter list hangs off, which is contiguous with whatever scope the
  `ERROR` swallowed however it split the two. So `MACRO operator Vec<int>()
  const GUARDED_BY(m);` is `Cls::operator Vec<int>` and
  `MACRO operator int (*)() const;` is `Cls::operator int(*)`, each meeting
  the unannotated declaration and the out-of-class definition of the same
  member; every target kind spells the same under a leading macro, a trailing
  one, or both.
  A pointer or reference target recovers differently from the rest — the
  operator keeps a whole `function_declarator`, the member's `;` goes
  *missing*, and a trailing macro is parked after it as a `declaration` of its
  own. A class-body `declaration` following a member closed by a missing `;`
  is the tail of that member (the author wrote no `;` between them), so it
  declares nothing; registering it gave the class an undefined
  `Cls::GUARDED_BY` that call sites on any annotated member resolved to. A
  genuinely separate member after a missing `;` recovers as a
  `field_declaration`, not a `declaration`, so the rule does not reach it; the
  one shape it does swallow is a *ctor* declaration after a member whose `;` the
  author actually forgot (`void f()` then `C(int);`), which is not valid C++
  either way.
  What tells the two repairs apart is where the `ERROR` sits relative to the
  `::`: before it for a fabricated qualification (the leftover type is the
  scope), after it for a real one. Both are looked for at every level of the
  `qualified_identifier` chain, not just the top: a qualified name nests one
  level per scope it carries, so each scope either half spells pushes the
  recovery's mark one level deeper (`FFI_EXPORT n::q::S A::B::M()` parks its
  `A` three levels down).
- **Ctors / dtors**: emitted for `new Cls(...)`, destructor calls on
  `delete p`, explicit qualified dtor calls, constructor-declarations with
  an argument list, ctor-initializer lists (base + member targets, with
  parentheses or braces; a member of union type with a user-provided
  constructor counts as a class). A base or class-typed member initializer
  records a site for the target class's user-provided constructors. An implicit
  or in-class defaulted constructor has no site of its own.
  Cached constructor metadata describes only that class's own declarations;
  a base constructor does not make a derived constructor user-provided.
  Empty member or base initializers reuse [automatic-object lifecycle lookup](#automatic-objects):
  an implicit or in-class defaulted default constructor runs the base
  constructors. With arguments, an implicit copy or aggregate initializer
  records no constructor site for the aggregate itself. A braced aggregate
  initializer constructs its base subobjects in declaration order, then its
  members, binding each subobject's own initializer arguments past `this`.
  Parenthesized aggregate initialization follows the translation unit's C++20
  (or later) standard; parentheses do not permit brace elision. Arrays consume
  clauses per element, including nested dimensions, and class elements run their
  converting constructors. Array extents retain integer literals and bounded
  arithmetic expressions over visible namespace or class constants, including
  constants imported through cached headers. Unsupported expressions, local
  names and conflicting constant definitions leave the extent unknown. An
  unknown extent retains possible clause boundaries for following aggregate
  members instead of assigning every remaining clause to the array; this may
  add constructor candidates. Empty unknown-size arrays retain possible element
  construction in summary storage. Empty and omitted subobjects recurse through
  class members as well as bases. Omitted members first use their default member
  initializer, retained with declaration scope and source mapping through cached
  headers; explicit initializers and copies do not apply those defaults.
  A user-provided constructor runs the default of every member its list does
  not name, as the caller of the expression's calls, and reaches defaults nested
  in omitted bases and members that have no user-provided constructor of their
  own; a list entry replaces the default, and a delegating constructor leaves
  defaults to its target. A class with no user-provided constructor has no body
  to own them, so the function that declares, braces or `new`s the object is
  their caller, at that construction in the call-sequence graph. Each caller
  keeps its own site at the initializer's spelling, the same ownership a default
  argument has, however many objects it constructs and whatever its linkage. A temporary (`return T{}`, `T x = T()`) does not run its defaults
  in this model, and a file-scope `new T()` has no function to own them. Inside a body, defaults are lowered
  only when they can have an effect: one evaluating a call (a keyword cast's own parentheses and an unevaluated operand such as `sizeof(f())` or `decltype(f())` are not one) or `new`, building a
  class the tree defines, storing an address into a pointer or member-pointer field
  (including array elements), or naming a
  function for a member of unknown type. A class outside the tree records no constructor site
  from a default, because it has no body to model and stores nothing; only the
  calls in its expression run. Operators on names (`a + b`) are not treated as
  calls here.
  Declaration-scope namespace imports are restored for the expression, without
  the caller's locals or aliases. Distinct initializer variants remain possible
  alternatives when type metadata merges.
  Scalar, pointer and reference aggregate elements retain their field identity
  and emit value stores, so callback members remain callable. An implicit outer
  copy or move has no site of its own but preserves nontrivial construction of
  its base and member subobjects, with the corresponding source subobject as
  the argument. Own declarations hide same-signature inherited constructors;
  overload selection considers the remaining own and imported candidates
  together. Constructor signatures retain parameter spelling, cv qualification
  and lvalue/rvalue reference categories through cached type imports. Missing
  or alias-erased qualification cannot prove hiding. Reference-binding checks
  exclude a candidate only when the known argument type and value category
  establish incompatibility; possible converting temporaries remain candidates.
  A forward-declared union with an unavailable definition retains a
  possible constructor site under the same rule as an unknown class.
  See the [aggregate rules](https://eel.is/c++draft/dcl.init.aggr) and
  [inherited-constructor initialization](https://eel.is/c++draft/class.inhctor.init).
  Explicit subobject initializers retain separate source positions, so two
  members invoking the same constructor remain distinct through header merges.
  Nested braces and brace elision preserve that order; omitted bases
  follow default lifecycle lookup. A `using Base::Base` declaration explicitly
  brings in that base's constructors, so argument-bearing initializers can
  call them. Inherited construction also default-initializes the derived object's
  other bases and members, using their default member initializers, including
  each derived level along an inherited-constructor chain. A base named through
  a type alias (`using Alias = Base;` followed by `using Alias::Alias`) resolves
  to the same declaring class.
  Saving an initializer's source mappings uses a binary search for its start,
  then visits only mappings before its end. Equal-offset entries at the start
  remain in output order; no preceding mapping is synthesized.
  Cached type imports preserve default- and copy-constructor eligibility and
  these explicit constructor imports, retaining sites for later symbol merging.
  Types-only cached-header imports retain this constructor declaration
  metadata for every class, nested classes and member class templates included.
  A known user-provided constructor keeps its site until the
  translation unit merges its symbols.
  An empty set or constructors all defaulted in the class record no call to
  the class itself, including implicit copies. A definition discovered only at final TU merge
  cannot restore a skipped site; construction needs a visible constructor
  declaration in the initializer's unit or its imported type metadata.
  This includes configuration-dependent class bodies: a constructor enabled
  only in another unit's preprocessor variant does not enter the initializer's
  constructor set. A missing class definition remains uncertain and retains
  a possible constructor site instead, including one-argument copies. The
  negative copy decision requires a known constructor set or class definition;
  a forward declaration alone cannot establish that its copy is implicit.
  A reference member's binding constructs nothing
  and records no constructor site, whatever constructors its class declares;
  a reference to a non-class type keeps the value store described above.
  Parentheses around a reference declarator (`Value (&ref)`) preserve the binding;
  pointer layers remain distinct from reference layers.
  Every member declarator contributes its own field and reference metadata
  (`T first, second`, `T &ref, value`); parentheses around a known class member's
  name (`T (value)`) do not turn it into a method. Qualified base names and
  unions imported through nested cached headers use the same construction rule.
  A member that is an array of a class, nested arrays
  included, constructs its elements: an empty list (`m_arr{}`) records the
  element's default constructor, and a non-empty list initializes the
  elements one by one, each listed expression lowered on its own rather than
  as one constructor's arguments. `Cls o{a};` is a constructor call when the class
  declares a user-provided constructor; without one the braces initialize an
  aggregate's fields. A constructor declared `= default` or `= delete` in its
  class body is not user-provided (`Function::defaulted_in_class`), so
  `struct A { A() = default; Callback cb; }; A a{f};` still initializes `cb`
  (C++17). `Cls o(a, b);` whose parenthesized names are all variables or
  functions in scope defines an object even though tree-sitter parses it as
  a function declaration, as C++ does when the names are not types (a data
  member of the enclosing class is a value too: `std::lock_guard<std::mutex>
  lock(mutex_);` constructs, its member argument recorded without an actual,
  as a literal's is; so does a pointer, `T *p(buf);`); it
  used to register a phantom function `o`, so `o.m()` became an indirect
  call to it. Inside a function body such a declaration lowers as the local
  and its constructor call, or for a non-class type as `o` initialized
  from its one argument. `Cls o();` and `Cls f(Type);` stay declarations, and
  so does `Cls f(Name);` when `Name` is a type nearer than a variable of that
  name: a function-local alias, or a class or typedef of an enclosing class or
  namespace (`using Value = int;` hides a global `int Value`). A local
  variable is nearest. At file scope the same declaration defines a global
  when every name is a global or file `static` variable, or a function
  (#95): `Callback g(OnReady);` initializes the pointer, and `Worker w(OnReady);` defines the
  object without a constructor call, as `Worker w{OnReady};` does, since no
  function runs it. A function name in extra parentheses, `Worker
  w((OnReady));`, is an argument like the bare name.
  A reference spelled the same way, `T &r(a);`, binds `r` to `a` and
  constructs nothing; lowering records neither a constructor call nor `r`.
  Inside a body, a parenthesized name that resolves to nothing and names no
  type in scope (an enumerator such as `Json::arrayValue`, a macro constant)
  is a value too, so `Json::Value log(Json::arrayValue);` defines `log`.
  C++ reads the line as a declaration only when the name is a type, and a
  block-scope function declaration is rare. One whose name is a function in
  scope (`Widget make(Config);` with `make` declared or defined in the unit)
  redeclares it. When the unit knows neither the function nor the parameter
  type (both in an unindexed header or another unit), the line reads as a
  local object: a documented imprecision. At file scope such a name still
  makes the line a declaration.
  Automatic (stack) objects: see [Automatic objects](#automatic-objects).
- **References** lower as pointers (aliasing stores land on caller memory).
- **C++ locals** live until the end of their block, or of the `if`, `for`,
  `while`, `switch` or `catch` whose condition or init-statement declares
  them (`if (auto p = f())`); a variable of the same name they hid is
  visible again afterwards. C lowering keeps one name per function. A
  lambda's parameters, and those of a function declared in a body
  (`void take(T *p);`), are never locals of the enclosing function.
- **`auto` local types (#87, C1)**: a call initializer copies the declared
  return type of the function it resolves to. Free, qualified, explicit and
  implicit member calls use the call-site lookup and arity filter; ranking
  narrows overloads when the argument types are known. The candidates left
  must all take the arguments and agree on one return type (`T *get()` and
  `T *get() const` do); otherwise the local stays unknown. `T(args)` is a
  construction when the call site reads it as one, and gives `T`. Pointer
  wrapper constructions `std::shared_ptr<T>(p)`, `std::unique_ptr<T>(p)`,
  `std::weak_ptr<T>(p)`, `sptr<T>(p)` and `wptr<T>(p)` also retain the wrapper
  and its known class argument when their headers are absent. Declared
  construction takes precedence, including unqualified template constructions
  and wrappers with additional template arguments such as a `unique_ptr` deleter.
  Declared constructions retain their fully qualified template arguments for
  pointee and member-return substitution, including declarations from `--dep`.
  Unqualified constructions retain the declared class's canonical namespace;
  a same-named wrapper in another namespace is not a member-call candidate.
  For calls without a declared construction, ordinary and argument-dependent
  lookup use the actual argument types before this name-based inference;
  any function declaration found takes
  precedence, even when its return type cannot be inferred.
  The fallback rejects a dependent complete callee spelling (including a
  template-template parameter named `sptr`) and unsupported templated owners
  such as `Outer<Item>::sptr<Other>`; the outer template's arguments are never
  used as that nested wrapper's pointee.
  Pointer layers on definitions and member prototypes, and declaration-only
  reference-return factories, are preserved for this lookup. A non-call
  initializer takes the receiver type the member-call path already reads:
  copies from typed variables and fields (`auto p = q.a`), `*pp` and `&w`; a
  C-style cast and `new T` give the type they spell, pointer layers
  included. Only a class, union or function-pointer type, under any pointer
  layers, is copied: a scalar may be the stand-in for a type name lowering
  could not resolve. `auto&` and `const auto&` take the type of the value
  they name, as an explicit `T&` local does, so an argument ranks overloads
  the same either way. `auto *p` and `auto **p` need an
  initializer with at least as many pointer layers, and deduce nothing from
  one with fewer. Both `if (auto p = f())` and `if (auto p = f(); p)` are
  covered. A parenthesized single-expression initializer (`auto self(shared_from_this());`,
  `auto p(factory());`, `auto p(obj.get());`) uses the same deduction and
  value flow as a copy initializer. For ambiguous function-declarator parses,
  see [C++ parse-input normalization](#c-parse-input-normalization).
  `lock()` on a `weak_ptr<T>` and `promote()` on a `wptr<T>` whose body is
  not in the tree produce `shared_ptr<T>` / `sptr<T>` in the weak pointer's
  own scope (`OHOS::CameraStandard::wptr` promotes to
  `OHOS::CameraStandard::sptr`); a declared wrapper's own members are looked
  up instead. `std::make_shared<T>` / `std::make_unique<T>` /
  `std::allocate_shared<T>` produce the corresponding wrapper type, spelled bare too where `using namespace std;`
  or `using std::make_shared;` is in scope: recognition follows ordinary
  function lookup precedence (body imports, enclosing declarations,
  namespace imports, global declarations, file imports, then directives),
  including imports of standard factories whose declarations are outside
  the indexed tree. A nearer project declaration hides an outer import; under a
  directive alone the bare name must not resolve to a function of another
  scope (a project's own `make_shared` under `using namespace project;` is
  that function, not the standard factory), with cv-qualifiers dropped and
  template arguments kept (`make_shared<const Box<int>>`). These models
  require a class the call site can name, through its scopes or a `using`
  directive. What else a factory call records is in
  [Factory construction](#factory-construction). A promotion also carries its receiver's value
  into the result (see [Weak-pointer promotion](IR.md#weak-pointer-promotion)).
  Class arguments of declared smart-pointer returns follow the class lookup
  policy below; unresolved or ambiguous arguments remain untyped.
  Unresolved callees and template-dependent types remain unknown. A type is
  dependent when it names a parameter of an enclosing template, directly or
  through a member alias of an enclosing class
  (`using Ptr = std::shared_ptr<T>; Ptr Get();`): a type parameter by its
  identifier, a value parameter (`template<int N>`) by its declarator, never
  by its type. A source spelling qualified by `::` (`OHOS::Event`) is never
  the parameter; a name lowering qualified (`ns::T` for a parameter `T` it
  could not find) still can be. A call whose callee belongs to a parameter
  (`t->make()` for `T *t`, `T::create()`, a member of a base `Base` in
  `struct M : Base`) is dependent too. Existing pointer-flow and
  virtual-dispatch handling then uses the inferred receiver exactly as an
  explicitly typed receiver.
- **Qualified names**: a leading `::` (`::f()`, `::ns::f()`,
  `::std::make_shared<T>()`) is looked up from the global scope; an
  unresolved call keeps its `::` spelling as the external callee, so
  `::operator new` stays that external. The parameters and body of a
  definition spelled `N::f` look type names up in `N`, class or namespace;
  when `N` is a namespace the unit or a header it includes opens, the body
  looks function names up in it too. A scope neither opens as a namespace is
  a class the unit cannot see (`Ast::Lookup` under
  `using namespace OHOS::Hardware`).
  Relative qualified calls (`Service::Write()` inside `namespace app`)
  use the same enclosing-scope lookup as type names, and fall back to the
  `using namespace` directives in scope, as a bare name already did.
- **`using namespace` and class names**: class/type lookup first checks local
  aliases, lexical scopes and explicit using-declarations. Active directives
  may then supply one distinct canonical class. Repeated imports of the same
  class agree; different matching classes leave the type unresolved, including
  configurations where header variants expose conflicting declarations.
  Template parameters shadow imported classes, both as bare types and inside
  template arguments. A base or out-of-line owner uses this same policy:
  `void AstObject::IsNode()` under `using namespace OHOS::Hardware` binds
  `OHOS::Hardware::AstObject::IsNode` only when that class is unambiguous.
- **Call-result receivers**: `S::GetInstance().Open()` and pointer-returning
  chains reuse declared return-type inference. Ambiguous return types stay
  unknown; receiver probes do not mutate the index.
  A cast to a class value, reference, or pointer supplies its written class
  and value category for member lookup.
- **Move/forward receivers.** Lowering recognizes one-argument calls to
  `std::move` and `std::forward` through qualification, namespace aliases,
  using-declarations, and using-directives. Qualified names and imports resolve
  each namespace alias in the path. Competing declarations that can take the
  operand keep ordinary overload resolution. ADL includes the global
  namespace for global types and the associated namespaces of class and enum
  template type arguments. Built-in dereference and address-of expressions
  preserve the specialization used for lookup. The lookup can rule out
  incompatible raw class pointers and reference bindings. Explicit template arguments
  restrict competitors to function templates. Parenthesized names use
  ordinary lookup without ADL.
  The receiver uses the explicit template argument's referent when it can
  be resolved, as in `std::forward<Base>(derived).M()`. Otherwise it uses
  the operand's static type, including any cast inside the operand.
  Forwarding through an lvalue reference, including an alias or a resolved
  `decltype`, yields an lvalue. A local `auto&&` collapses according to its
  initializer's category, so `decltype(r)` names an lvalue reference when
  `r` was initialized from an lvalue. A resolved `decltype` supplies the referent
  type and qualification, including applicable explicit qualifiers.
  `std::move` and forwarding through a known
  value or rvalue reference type yield rvalues. Dependent and unresolved
  forward types retain both possible categories. Known receiver categories
  narrow ref-qualified members for call edges and return flow. Known
  arguments can rank edges when another argument is unknown, while static
  result inference still requires agreement among the viable overloads. Unknown
  forwarding receiver categories and constness keep the winners of each
  viable receiver, including calls that pass ordinary arguments. These facts
  follow a comma expression's right operand, either arm of a conditional,
  and data member access through the forwarded object.
  The same lookup supports functor expressions
  such as `std::move(f)()`, `(f)()`, and `static_cast<F&&>(f)()`.
  See [moved-from values](FAQ.md#known-imprecision) for ownership semantics.
- **Templates**: lowered once per primary name. Class-template members
  returning a bare type parameter (`T Get()`, `T *Get()`) preserve its
  parameter position and pointer depth as type metadata across header merges.
  Lowering caches the lexical byte ranges of template parameter names and
  enclosing class aliases per syntax tree. Repeated method-return queries
  retain the same source order and alias-depth limit without rescanning all
  class members. The cache stores no resolved types or symbol decisions, is
  released with the tree, and is isolated when lowering a scratch initializer;
  scratch recovery queries read their syntax afresh. Valid primitive-keyword returns
  skip template ancestry queries when their base type is known. Template-return
  arity counts fixed parameters directly, excluding `(void)` and variadic tails,
  without constructing reference-parameter metadata; signatures still retain
  their full reference/default/variadic facts. Named types,
  including a template parameter named `size_t`, and malformed declarations
  keep the existing lookup and recovery rules.
  A concrete receiver substitutes its arguments before member lookup and
  virtual dispatch, through its pointer layers and through an instantiated
  class-template base it inherits the member from (`struct D : Holder<T *>`).
  A static call that spells the class template with its arguments is such a
  receiver too: `Box<Svc>::Get()->Run()` and
  `Singleton<Db>::GetInstance().Open()` type their results as `Svc *` and
  `Db`, and an `auto` local initialised from them keeps that type.
  A static member named on a class that inherits it (`Svc::GetInstance()`
  with `class Svc : public DelayedSingleton<Svc>`) is looked up through the
  bases as member lookup on an object is, stopping at the nearest class
  declaring it: the call's callee is the base template's
  `DelayedSingleton::GetInstance`, never a name invented on `Svc`, and its
  result substitutes the argument the base spells, resolved in that base
  declaration's scope. A derived declaration of the name hides the base's.
  **Undefined singleton templates.** When the unit being lowered sees no
  definition of c_utils' `DelayedSingleton`, `Singleton` or
  `DelayedRefSingleton` (absent from the headers it includes, or only
  forward-declared there; lowering reads one unit at a time, so a definition
  only another unit includes does not count),
  `DelayedSingleton<Svc>::GetInstance()` is typed `std::shared_ptr<Svc>`, and
  `Singleton<Db>::GetInstance()` and `DelayedRefSingleton<Db>::GetInstance()`
  are typed `Db` (a reference), as c_utils declares them (#184); a template
  of that name nested in another (`Outer<A>::DelayedSingleton<Svc>`) is not
  c_utils' and is not guessed.
  This is a guess from a name, like the undeclared wrapper rule above
  (#86): only `GetInstance` on these three templates, with
  exactly one argument naming a declared class (not a pointer), is guessed.
  The accessor itself stays the external declaration it is; no member is
  synthesized. A definition in view, a dependency root's included, decides
  instead, whatever it returns. An accessor inherited from such a base
  (`AccessTokenDb::GetInstance()` with
  `class AccessTokenDb : public DelayedSingleton<AccessTokenDb>` and no
  template in the tree) is typed the same way from the nearest undefined
  c_utils base of the class or an ancestor, its argument resolved where
  that base is written; a class declaring `GetInstance` on the way decides
  instead, as member lookup does.
  A parameter returned as the sole argument of another class template
  (`std::shared_ptr<T> GetInstance()`, `sptr<T> Get()`) substitutes inside
  it: the result is typed `std::shared_ptr<Svc>`, and `->` then unwraps it
  as it unwraps any receiver of that type ([Receiver typing through a
  wrapper](USAGE.md#receiver-typing-through-a-wrapper)). The wrapper is named from
  the template's scope when the tree declares it, else as spelled. A
  non-pointer wrapper stays itself (`Pair<T> *` reaches `Pair`'s members,
  never `T`'s), and a pointer argument inside a wrapper (`sptr<Svc *>`)
  stays unknown, as do a template template parameter used as the wrapper
  (`Ptr<T>`) and a spelling with more than one argument list
  (`Traits<T>::Ptr<U>`). A cv-qualified parameter (`shared_ptr<const T>`,
  `const T *`) substitutes as the parameter, its qualifier dropped: member
  lookup does not need it, and const twins (`shared_ptr<T> Get()` beside
  `shared_ptr<const T> Get() const`) then agree on one result.
  In-class prototypes of an overload set record their explicit parameter
  types (see below), so same-arity overloads (`T *Get(int)` beside
  `Decoy *Get(long)`) are two entries, each with its own return, and
  overload ranking picks between them; when the arguments cannot decide, the
  candidates must agree on one type. Separate definitions keep their
  parameter types, so overload ranking picks one and only its return
  applies. A scope that mentions a
  template parameter (`Box<T>::Get()`, `T::GetInstance()`) names no class
  before instantiation, even when a real class shares the parameter's name.
  The argument is resolved from the scope it is spelled in, keeping its own
  template arguments so a nested `Holder<Holder<T>>` substitutes again. Inherited arguments
  resolve in the base declaration's lexical scope, not the caller's; dependent base spellings
  are recorded at lowering and excluded from concrete return substitution. Compound dependent
  returns and function templates remain unknown; no template bodies are instantiated.
  A base that names a template parameter is substituted per concrete base
  instead ([Template-parameter bases](#template-parameter-bases)).
- **Lambda captures**: explicit (`[var]`, `[&var]`, `[this]`, `[*this]`), default (`[&]`, `[=]`), and init-captures (`[x = expr]`, `[&x = expr]`) bind the enclosing scope's variables, members, and `this` into the lowered lambda body.
  Init-captures preserve the initializer's expression type, including its
  pointer layers, before falling back to class inference. This enables
  general member and callback receivers such as `[q = &ga]() { q->cb(); }`,
  as well as captures holding an owning pointer.

Known C++ imprecision (in addition to the [general list](FAQ.md#known-imprecision)):

- Lambda captures are instance- and flow-insensitive (by-value object captures do not invoke copy constructors; captured mutations share variable points-to sets across call sites).
- `auto` inference needs a declaration visible in the translation unit;
  it does not infer returns from bodies, substitute general templates, or
  infer function-pointer call results. Trailing `auto` return declarations
  remain unknown. Named casts (`auto p = static_cast<T *>(v)`) parse as calls
  to the cast and stay unknown until C2. Conditional expressions infer only
  the shapes described in [Conditional values](IR.md#conditional-values).
  Cast arguments in a call-result receiver probe are
  conservatively unknown for overload ranking. `unique_ptr::release` has no
  extraction summary; supported `get()` forms are described in
  [Smart-pointer unwrap](IR.md#smart-pointer-unwrap).
- In-class prototypes record no parameter variables. They record their
  parameter types where those decide something: a name the class body
  declares or defines more than once, or brings in again with
  `using Base::f;`, and a virtual member, whose declaration is what a
  definition of the name in a subclass is matched against (a call ranked to
  another overload of the name does not dispatch to it, and only a
  definition of its types is an [entry](MODELS.md#execution-contexts) of a model
  naming it), except a member of an interface synthesized from an `.idl`
  file ([IDL-generated interfaces](#idl-generated-interfaces)), which
  spells its types as the IDL does (`sharedptr<T>`), not as its overrides
  do, and so records none. A name declared once otherwise merges with its
  definition by name and arity.
  So same-arity overloads of a member (`Worker *find(int)`,
  `Other *find(const char *)`, `Set(sptr<A>)` beside `Set(sptr<B>)`) are
  separate entries. A definition joins the prototype whose types it spells
  exactly; failing that, the one prototype whose types it spells alike
  (`trace_ir::spelled_alike`: a class with and without its qualifiers, its
  template arguments too; a class the index does not declare by its final
  name, since lowering may have guessed its namespace; a class named bare in
  its own body beside its instances; a name one side read as `int` because
  it could not resolve it), preferring more exact then more alike types, and
  none when two tie. A call reaches the overload its argument types rank
  first. An argument of unknown type ranks every candidate alike, so the
  known ones still choose the call's edges (`Write(GetFd(), "x")` binds
  `Write(int, const char *)`); only a result's type (an `auto` local, a
  chained receiver) is left to every overload a known argument can bind.
  Member calls rank within each declaring class, and a virtual call
  dispatches only to overrides of the overloads that won: a class's single
  override of the name is kept only when its signature is one of theirs.
  There an `int` parameter matches any integral one, since a standard
  integer typedef whose header is not indexed (`size_t`, `uint32_t`) reads
  as `int`: an override spelling `unsigned long` keeps the dispatch of a base
  `On(size_t)`, at the cost of a dispatch to a hiding `On(long)` beside a
  base `On(int)`. Converting constructors
  and conversion operators explain most mismatches, so only a value that is
  no pointer (a class object declared with no conversion operator of its own
  or a base's, or a scalar such as an enum value, never a literal `0` or
  `NULL`, a null pointer constant too, however spelled: `0x0`, `0'0`,
  `-0`) passed where a
  pointer to a class-template
  instance is taken (an
  out-parameter such as `sptr<X> *`; `T **` may be a decayed array) rules an
  overload out. An `int` argument decides nothing where some overload takes
  a reference to a class (or to `int`, which is how a class lowering cannot
  see reads), since lowering reads such a class as `int`; ranking ignores
  that position. An `auto` local initialised from the call is typed only
  when the remaining overloads' returns agree. An argument `E::VALUE` whose
  type-cased `E` is neither a class nor a namespace in the index is read as
  a value of class-like type `E` (most likely an enumerator of an enum
  outside the tree; one the index knows is `int`): it rules out
  out-parameter overloads but ranks none exactly. An out-of-tree class's
  `ALL_CAPS` constant of pointer type is misread the same way. One call bound to several
  overloads keeps one site per overload through the merge. An unnamed reference or pointer parameter
  (`const T &`) is one, like a named one.
- A reference parameter is typed one pointer layer deeper than an explicit
  `T&` local, so overload ranking can prefer `take(T*)` over `take(T)` for
  it.
- An out-of-line member definition under `using namespace N;` at global
  scope registers under the bare class name, apart from `N::`'s in-class
  prototypes; a call reaching the prototype stays external.
- `std::bind` / generic functors without a visible `operator()` stay
  unresolved-indirect unless a function address flows into them.
- Default construction without parens (`Cls o;`) emits no ctor site.
- **No call site resolves to a conversion operator**: `Cls::operator T` is
  indexed, but neither the compiler-inserted call of an implicit conversion
  nor an explicit `h.operator T()` reaches it — the explicit spelling parses
  as a field access whose `operator` keyword lands in an `ERROR` node, so it
  interns a phantom `Cls::T` instead. Conversion operators therefore have no
  incoming edges.
- **A prototype's return type wins the merge**: a member declared in a class
  and defined out of line records the prototype's placeholder `void`, not the
  definition's real type. This is general to all member functions (`int
  Cls::f()` defined out of line records `void` too), so a conversion
  operator's target type survives as its return type only when the class has
  no separate declaration.
- **A data member annotated by a macro that takes arguments is indexed as a
  function**: `int a_ GUARDED_BY(mu_);` parses as a member named `a_` in an
  `ERROR` beside a `function_declarator` spelling `GUARDED_BY(mu_)`, and only
  the latter looks like a declarator, so the class gains one undefined
  `Cls::GUARDED_BY` and the fields themselves are not indexed. A class that
  annotates several fields alike contributes one such phantom, not one per
  field, and a real method of that name would merge into it. Not separable
  from a *function* behind a leading macro (`MACRO int Plain() const;`), which
  is the same shape — type field, `ERROR`, `function_declarator` — with the
  halves meaning the opposite things; suppressing one drops the other, and
  dropping real methods is the worse trade.
- **A globally-qualified conversion target behind a leading macro escapes the
  repair**: `MACRO operator ::ns::S() const;` is the one shape whose `ERROR`
  lands at *class-body* level rather than inside the member — the macro and the
  `operator` keyword go into it together and the target becomes a
  `declaration` beside it — so the member walk, which runs per member, never
  sees the keyword. The declaration is indexed as `C::S`, and with a trailing
  macro as well the `ERROR` swallows the whole operator and the macro is left
  as the only `declaration`, giving the phantom `C::GUARDED_BY`. Every other
  target kind spells the same under a leading macro, a trailing one, both, or
  none; this one needs a repair at the class-body level, where nothing today
  reads recovery marks. Pinned as the one exclusion in
  `every_conversion_target_kind_spells_one_member_under_any_macro`.
- **A target type shadowed by an inner scope collapses onto the outer one**:
  the target drops the scopes the member sits in, which is right until two
  types of that name exist at different depths of those scopes. `operator S`
  and `operator N::S` in a class in `N` that also contains an inner `N::N::S`,
  or `operator ns::S` beside `operator ns::Handle::S`, come out one member.
  Telling them apart needs real name lookup, not a spelling rule.
- **A pointer-returning member behind a leading macro is still named after
  its return type**: `MACRO int (*Get(long))(int);` leaves a declarator-shaped
  leftover type in the `ERROR`, so the rule that reads only the `ERROR`'s
  declarators picks it rather than `Get`. The plain
  `MACRO int Get(long);` shape is handled.
- **A definition wearing a macro on both sides is lost**:
  `EXPORT void C::M() GUARDED_BY(m) { }` splits at the top level into a
  `declaration` holding `C::M` and a *separate* `function_definition` whose
  declarator is the trailing macro and which owns the body. `C::M` is
  therefore indexed undefined and its body attributed to a defined global
  `GUARDED_BY`. Either macro alone is handled; only the pair defeats it,
  because the repair the two need lives in different nodes.
- Objects at namespace scope emit no ctor/dtor sites (no enclosing function).
- A file-scope initializer written in a header (`static Cb cb = OnReady;`,
  `static void *p = f();`) is lowered for the header's own unit only; the
  copies the including units see carry no initializer flow, so a call
  through that pointer from a file including the header has no target.
- **ADL namespace derivation requires recovered aggregate names** (see
  [C++ lookup](#c-support-first-step)): enum-typed arguments and class spellings
  the index cannot qualify contribute no namespace of their own. Missing base
  facts also limit inherited associations. A leading global-scope `::` in a
  recovered tag (`::kit::Widget`) is stripped before deriving its namespace,
  matching how `functions_in_namespace` treats `kit` and `::kit` as interchangeable.
- Relative `using`/`using namespace` targets expand to every enclosing
  namespace the spelling could denote (over-approximation); genuine C++
  picks the innermost *declared* namespace only.
- Namespace **hiding/ordering is not modeled** for overload ranking:
  `rank_overloads` collapses a same-arity set to the best static-type
  match without preferring a candidate from the innermost enclosing
  namespace over an exact global match. May-analysis keeps all *tied*
  candidates (one direct edge per candidate), but a strictly-better-
  ranking leaked candidate would be preferred even where C++ scoping
  would hide it.
- Overload resolution is arity + static-type ranked; a non-exact argument
  (conversion, `auto`, unknown type) still keeps the whole arity set, and
  0-arg member-call overloads resolve through the primary-name entry only.
- Scalar type identity is coarse: `unsigned`/`signed` collapse to `Int`,
  `signed long long`→`LongLong`, `long double`→`Double`; distinct
  same-arity overloads relying on those distinctions collapse.
  Unnamed parameters (`void foo(int)`) still occupy a slot so
  `foo(int)` and `foo(int, int)` stay distinct; `void f(void)` does not.
- Template specializations collapse into the primary entry; no
  dependent-type modeling.
- Virtual expansion is CHA from the static receiver type (not points-to).
  Multiple bases resolve; nearest declarer wins when walking up.
  `override` implies virtual. `final` on a class or method stops further
  subclass targets. Flags belong to the declaration, not to local classes
  nested inside its body. Virtual inheritance is recorded as a normal base edge.
  A well-formed declaration without `virtual`, `override` or `final` before
  its body skips the recursive flag walk; malformed syntax retains that walk.
- Headers shared between `.c` and `.cpp` TUs parse under whichever
  grammar reaches them first at merge time.
- **`using namespace` in headers**: the
  [namespaces rules](#c-support-first-step) say file-scope directives apply
  TU-wide, but with header IR merged symbols-only, a `using namespace std;`
  written in a header is never seen while lowering the TU that includes it, so
  unqualified calls in the TU that depend on it degrade to external stubs.
  Sound but imprecise.
- **`using namespace` across namespace reopenings**: a using-directive at
  namespace scope stays in effect in later reopenings of the same namespace
  (`namespace A { using namespace B; }` then `namespace A { … }` finds
  `B::f`). Lowering truncates directives at namespace block exit, so
  reopenings lose the candidate. OHOS code reopens `OHOS::X` blocks often;
  this is an under-approximation. Sound for may-analysis (candidates may be
  missed but never wrongly added).

Next slices (hiview-grounded): [docs/CPP_ROADMAP.md](CPP_ROADMAP.md).

### Undeclared receiver classes

A member call on a receiver whose class the unit never declares --
`MessageParcel &data; data.WriteInt32(1);` with `message_parcel.h` in
another repository, off the include path -- resolves as if the class were
forward-declared in the scope the type is written in (#193). A bare C++
type name the unit cannot resolve (no class, typedef, alias or enum it
sees; not a fundamental or standard integer spelling) in the type position
of a parameter, a local, a field or a range-for element types the object as
the class of that name in the innermost enclosing namespace
(`OHOS::MessageParcel` inside `namespace OHOS`, `FuzzedDataProvider` at the
global scope; `undeclared_class_guess`). `data.WriteInt32(1)` and
`reply->ReadInt32()` then synthesize the externals
`OHOS::MessageParcel::WriteInt32` and `OHOS::MessageParcel::ReadInt32`,
exactly what `class MessageParcel;` beside the function yields, for `.` and
`->`, so `trace-merge` relinks the edge against a database that defines the
member. The callee is named from the declared type, never from the bare
member name: `u->handler(1)` on an undeclared `Unseen` is
`OHOS::Unseen::handler`, not a free function `handler` (#147, #153); as with
a forward-declared class, the member may really be a function-pointer field
of the unseen body, which the index cannot tell apart.

After merge, an external member on an undeclared receiver retains its external
leaf and may recover indexed definitions with the same final class name.
Candidates are indexed by receiver class and member, so repeated misses do not
scan every method sharing a common name. Lowering records the source argument
count and lexical namespace evidence for these unresolved member calls.

For a qualified owner, recovery requires a lexical namespace guess belonging
to the caller, or completion of a partial namespace qualifier (`IPC` to
`OHOS::Media::IPC`). Candidates must also lie in namespace prefixes reachable
through enclosing scopes or visible using directives/declarations. An explicit
unrelated qualifier is never replaced with a different namespace.
An explicitly named standard-library owner (`std::mutex`, for example) never
acquires an unrelated project class. A bare owner can reflect an unavailable
using import and conservatively retains namesakes. An indexed using declaration
is applied while guessing the receiver, before recovery is needed. When the
imported type itself is absent, its first qualifier is resolved against declared
namespace/class roots in enclosing scopes; without that evidence the spelling
remains global (`using std::string;` keeps `std::string`). An explicit leading
`::` also remains global. The motivating
`OHOS::DataShare::MediaDataShareExtAbility` spelling can thus reach the definition
under `OHOS::AbilityRuntime` through its enclosing `OHOS` context. A declared
receiver never takes this fallback.

The shared resolver preserves image, declaring-header and test-partition rules.
TU-local definition precedence is applied **per compatible overload signature**,
so a local `Run(int)` cannot suppress a `Run(long)` defined elsewhere. Recovered
overloads are filtered by the source call's explicit argument count, including
default arguments and variadics; when none fits, all remain as a may-analysis
fallback. The missing declaration supplies no evidence for ranking equally
viable overloads by receiver type. This adds call reachability without merging
classes or inventing missing layouts.

Not guessed: a function's return type (`Unresolved GetBare(); auto p =
GetBare();` leaves `p` untyped; `T f();` declares a function anywhere, in
a body too, while `T w(a);` with arguments in a body defines an object), a
cast or template argument, a template parameter or a name mentioning one (`T t;
t.Run();` stays unresolved), and a wrapper whose argument names no declared
class (`sptr<Unknown> p; p->Run();` stays unresolved, see
[Smart-pointer unwrap](IR.md#smart-pointer-unwrap)). A typedef of a scalar or of
a callback the unit cannot see is guessed like a class. The guess is
remembered as one (`TypeTable::note_guessed_class`, carried with a header's
types) and, while no unit declares the class
(`TypeTable::is_guessed_class`), stays what the `int` such a type read as
before wherever the spellings of a signature are read leniently: an
in-class prototype meets its definition across it (`spelled_alike`, with an
override's guessed integer typedef matching the base's integral parameter
and `size_t`), a C-linkage prototype meets a definition missing its
callback typedef (`int_stands_for_fn_ptr`), a redeclared internal function
joins its definition (`may_name_same_type_in`), and ranking lets it decide
no overload (`rank_overloads`). The strict comparison of two entries that
both lower parameter variables (`same_param_type`) reads the guess as the
class it names, as it reads an in-class prototype's unresolved parameter:
`Set(Mode)` is not `Set(int)`, and a free `Count(size_t)` prototype and a
`Count(MySize)` definition whose unit never sees the typedef are two
entries, where before #193 the two `int`s collided. Its name registers no tag while no unit
declares the class, whichever spelling of the name was interned first, so a
typedef or enum of the name declared later in the unit, or an enumerator
`E::VALUE` of it, resolves as it did; a declaration of the class after the
guess, in the unit or merged in from another, registers the tag the guessed
descriptor withheld (`TypeTable::declare_struct`), so the tag table does not
depend on which unit merged first. A guessed class constructs nothing
(`initialized_class`): a member initializer or direct initialization of an
object of that type stores the value, since the type may be a callback
typedef the unit never saw; and the solver leaves a cell of that type
unguarded (`slot_guard_for`), so a function address stored into such a
field reaches the call through it, as it did when the typedef read as
`int`. The namespace is the innermost
enclosing one, as a forward declaration written there would give: a class
really living elsewhere (`Want` of `OHOS::AAFwk` written under
`using namespace AAFwk` inside `OHOS::AbilityRuntime`) is named in the wrong
namespace, as a forward declaration there would be, and the merge cannot
relink that edge. Fixture: `tests/fixtures/cpp_undeclared_receiver/`
(`main.cpp` undeclared beside `declared.cpp` forward-declared, which must
produce the same edges).

### Automatic objects

Constructor and destructor sites of automatic C++ objects (#186). This
section is the authoritative statement of the rules; lowering
(`emit_automatic_lifecycle`, `lifecycle_members`, `lower_statement_local` in
`trace-parse/src/lower.rs`), tests and reports link here.

- **Which members run.** For a class whose own members of the kind
  include a user-provided one, those members: its destructor, or its
  constructors callable without arguments (or of unknown arity), one
  declaring no parameter at all chosen over a template or an ellipsis one,
  as overload resolution picks it. A member that is implicit, or defaulted
  in its class (`Function::defaulted_in_class`), runs every base's, so the
  rule applies to each direct base in turn: `struct D : A, B {}; D d;`
  reaches `A::A`, `B::B`, `A::~A` and `B::~B`, and `D() = default;` beside
  `template <class... T> D(T...)` reaches the base's constructor. A class
  that declares constructors, none of them a default one, is never
  default-constructed; one whose hierarchy provides none records no site.
  Each class providing members gets a site of its own, its overloads ranked
  among themselves.
- **Construction.** `T x;`, and the value-initializing `T x{};` and
  `T x = {};`, record those constructors. Any other initialized local
  (`T x = e`, `T x{a}`, `T x(a)`, a condition's `if (T x{a})`) records the
  constructor its initializer names, under Ctors / dtors above; non-empty
  braces on a class whose hierarchy declares no user-provided constructor
  initialize an aggregate (see follow-up). `static` and `thread_local`
  locals are constructed on the function's first run, so they record the
  constructor too.
- **Destruction.** Every automatic local of such a class gets **one**
  destructor site in the enclosing function, attributed to its
  declaration, whatever way its scope is left: `return`, `break`, `goto`
  and exceptions are not modeled per exit, keeping the analysis
  flow-insensitive. Locals in nested blocks, `if`/`for`/`switch`
  init-statements and conditions, and lambda bodies (the lambda is the
  caller) are covered.
- **`this` and dispatch.** The constructor's and destructor's implicit
  `this` receives the local, so a virtual destructor and calls through
  `this` inside it resolve. A local is exactly its class, so its sites
  take no virtual dispatch (`CallSite::exact_receiver`): no subclass's
  destructor is reached, nor a defaulted one of its own.
- **Classes and arrays.** A local of a named struct, class or union type,
  or an array of one: `T a[4];` and `T a[4]{};` record one constructor site
  and one destructor site for the whole array, not one per element. An
  initializer constructs a union only when it declares a constructor; a
  C-style union (`U u(src);`) is copied.
- **Range-for and catch variables.** `for (T x : r)` and `catch (T e)` are
  lowered as locals of the statement's body, after the range expression
  (which still names the outer entity: `for (Item item : item.list())`),
  so they hide an outer name whatever their type. Copy-initialized, one of
  class type gets the destructor site only. A range naming an array
  variable gives the loop variable its elements, read from the array
  summary as `x = arr[i]` reads them.
  Calls through a loop or catch variable resolve by its type:
  - the declared one (`catch (const std::logic_error &e)`,
    `for (Handler *h : hs)`);
  - for a placeholder (`for (const auto &p : ps)`), the element type of a
    range typed as a member call's receiver is (a variable, a field, `*p`)
    when it is an array, or a standard sequence container (`std::vector`,
    `std::list`, `std::set`, ..., or the bare name under
    `using namespace std;`) whose first template argument names a class,
    cv-qualifiers dropped; under the `auto` rule above, the declarator's
    `*`s must fit. Anything else leaves the type unknown.

  An unnamed by-value catch parameter (`catch (T)`) is lowered as a
  nameless local when its class has a destructor to record; an unnamed
  reference (`catch (T &)`) and a structured binding (`auto [k, v]`,
  `auto &[k, v]`) are not lowered.
- **No destructor site** for `static` and `thread_local` locals (destroyed
  at exit, not by the function; a local of a lambda in a `thread_local`
  variable's initializer is not one). **No site** for references,
  pointers, `extern` declarations, or bodies of dependency headers, which
  merge declaration-only. A scope guard a dependency header's macro
  declares in the user's code is the user's local and is recorded.
- **Left for follow-up.** Temporaries (`T().Run()`, a returned
  `std::lock_guard`); the implicit construction and destruction of
  members and bases a user-provided constructor or destructor runs; the
  base constructors non-empty aggregate braces run (`struct D : B {
  D() = default; int n; }; D d{{}, 1};` records no `B::B`); the elements a
  partly listed array value-initializes; and copy or move constructors for
  copy-initialized locals.

### Container elements

For a named modeled container or one stored in a data member, lowering binds the
element type at the template-id using the normal type and declarator rules.
The specialization key keeps qualified type spellings and expands recorded
aliases. It normalizes equivalent qualifier positions such as `const int*`
and `int const*` while preserving distinctions such as `int*` and
`unsigned int*` even when their element-flow descriptors have the same shape.
Omitted type arguments take the defaults from the indexed template declaration.
This makes a default allocator and the same allocator written explicitly share
one signature, while a distinct allocator keeps its own signature. Supported
dependent defaults use earlier arguments directly or as arguments of a template
such as `allocator<T>`. Defaults that need dependent member lookup or apply cv
qualifiers or a declarator to a dependent parameter are left unresolved.
Element qualifications also carry into member-call typing through subscript
and accessor results.
The [TOML model](MODELS.md#container-element-models) selects the type argument
and operations for pointer, function-pointer and directly spelled
`std::function` values. The sequence models cover initializer lists and
selected insertions, emplacement, and element accessors. Subscript syntax
also records the `operator[]` call through the normal member-call path.
Indexed concrete container specializations have distinct member owners.
Macro-generated subscripts use the bracket's expansion identity for the call
site and its provenance for filtering element reads and writes.
Unevaluated subscripts retain a site for sequencing without an element access.
Braced construction prefers the indexed initializer-list constructor while
the elements may initialize the modeled element type. A known incompatible
arithmetic value leaves the ordinary constructor candidates available. List
conversion checks account for array decay and visible free operators on
scoped and unscoped enums. Only unscoped enums admit implicit integer promotion.
Single-value braces can wrap a modeled insertion or an initializer-list element.
Default member initializers and constructor member initializers store list
elements in the declaring field's cell each time lowering evaluates them.

The queue model connects `push` and single-argument `emplace` to `front` and
`back`. Both accessors see the queue's may-set of values. `pop` adds no
constraints.
The summary includes values from every index. Replacement, `clear` and erasure
leave those values reachable.

Separate variable declarations use separate cells. Cross-unit sharing follows
the ordinary global symbol merge. Instances of a data member share its cell
within a link target, while different member declarations and element types have
separate cells. Subscripts on named arrays and pointers can select the object
that owns the field. Library effects require indexed container member
declarations. Canonical indexing supplies those declarations through the
shared prelude. Cached project headers carry the modeled member names and
arities through types-only imports, so an inline function can use operations
declared by another project header.

Map models read the mapped slot from braced key/value entries, direct
`std::make_pair` arguments, and named `std::pair` values initialized from two
arguments or braced clauses. Named pair reads use the ordinary field summaries
for their concrete specialization. Copy initialization and assignment between
named pair specializations propagate pointer and callable values between
matching fields. Explicit pair specializations use their indexed constructor
declarations, while ordinary instantiations use the primary template's
declarations. Braced `insert` calls can store each
element of a sequence or set list, or the mapped slot from each braced map
entry. A `find` or `begin`
result holds the address of its originating
container's summary cell. Copies and assignments propagate that address, so
`iter->second` and `(*iter).second` read the mapped values from the containers
the iterator may refer to. Both spellings preserve the mapped pointer's pointee
qualification for later member calls. Explicit `iterator` and `const_iterator` types
also bind the enclosing container's template arguments in declaration scope,
including a container named through a type alias.
A branch that tests a failed lookup does not narrow
those values. Set and unordered-set models connect braced initializers and
selected insertions to iterators from `find` and `begin`. A local iterator or
an immediate lookup can read the same cell through `*it`, preserving the
set's read-only element access.

Follow-ups include container copies, iterator arithmetic and reference
parameters, range-for value flow, access via pointers or references to the
container itself, nested containers, and structured bindings. Planned stores
include fill construction, initializer-list assignment and `assign`, and
`try_emplace`. Pair projections currently cover named standard pairs and direct
`std::make_pair` calls. Other pair-producing expressions and lists of typed
pair expressions are follow-ups.

### Factory construction

Objects built by a smart-pointer factory (#192). This section is the
authoritative statement of the rule; lowering (`factory_constructed_class`,
`emit_factory_construction` and the `construct_on_heap` it shares with
`new T(args)`, `trace-parse/src/lower.rs`), tests
(`tests/fixtures/cpp_factory_ctor`) and reports link here.

- **Recognised factories.** `std::make_shared<T>(args)`,
  `std::make_unique<T>(args)` and `std::allocate_shared<T>(alloc, args)`
  (whose allocator is no constructor argument) in every spelling the `auto`
  rules under [C++ support](#c-support-first-step) type the result of (one
  recogniser, `SMART_PTR_FACTORIES`, serves both); and
  `sptr<T>::MakeSptr(args)` (`STATIC_FACTORIES`), any scope
  whose last segment is `sptr` (`OHOS::sptr<T>`, or `sptr<T>` under
  `using namespace OHOS;`), whether or not `refbase.h` is in the tree.
- **Construction.** Such a call is also a constructor call of `T`, lowered
  through the same code as `new T(args)` ([`NewHeap`](IR.md#ir-flow-constraints-trace-ir)
  states that rule, the copy-or-move case included): a heap object, a
  `$make<id>` temporary typed `T` that every factory call in one function
  building `T` shares (`factory_object`; nothing tells two such objects
  apart while fields are instance-insensitive `FieldSummary` cells, so one
  object per call site is what to restore if they become
  instance-sensitive), is the constructor's `this`, the
  factory's arguments bind to the constructor's parameters in order, and
  the constructor set `new T` looks up is filtered by arity and ranked by
  argument type, so both pick the same overload. The factory's own call
  site is kept unchanged (an external edge to `std::make_shared` when its
  body is not in the tree, a direct one to a declared `sptr::MakeSptr`),
  and both sites share the call's position.
- **No invented constructor.** A site is recorded only when the
  constructor set above has at least one user-provided constructor (one
  not defaulted in its class). Unlike `new T(args)`, a class without one gets no
  unresolved `T::T` site either. This check is class-level: once it
  passes, the class's in-class defaulted overloads still take part in
  ranking and can be chosen, exactly as for `new T(args)`
  (`struct S { S() = default; S(int); };` resolves
  `std::make_shared<S>()` to `S::S()`). That differs from
  [Automatic objects](#automatic-objects), which drop defaulted members
  one by one. The check waits for a conclusive set (`ctor_set_known`):
  an empty lookup in a cached project header, which imports project prerequisites
  as types only, or of a class whose definition is not indexed, proves
  nothing, so the factory keeps an unresolved `T::T` site exactly as
  `new` does ([`NewHeap`](IR.md#ir-flow-constraints-trace-ir)), for the TU
  symbol merge to resolve (`inline` header functions) or to stay
  unresolved (a class defined outside the tree). The copy-or-move test
  waits for the same set.
- **Nameable, non-dependent class.** `T` must be the one template argument,
  name a class the call site can name through its scopes or a `using`
  directive (`const` and `volatile` dropped, `strip_cv`), and not mention a parameter of an
  enclosing template. Anything else records nothing beyond the factory's
  own site. One exception: a class of the standard library the unit never
  sees declared (`std::packaged_task<void()>`, whose body is not in the tree)
  is named as it is spelled, as `new std::packaged_task<void()>(f)` names it
  (a global qualification, `::std::thread`, is the same class), so the factory
  records the same unresolved constructor site, which a function model of that
  constructor then matches ([Callable wrappers](MODELS.md#callable-wrappers)). The head
  names the class, so a template argument that mentions an enclosing
  template's parameter (`std::packaged_task<R()>`) does not stop the site, as
  it does not stop `new`; the rule above is for a class the tree declares. Any such
  class gets the site, not only those a model names, so a factory building an
  undeclared standard class (`std::make_shared<std::string>("x")`) adds one
  external constructor edge, as `new` does. A standard type that is no class
  (a pointer, a reference, an array as in `std::make_unique<std::uint8_t[]>(n)`,
  or a fundamental type's standard name such as `std::size_t`) records nothing
  beyond the factory's own site.
- **The object is not the call's value.** Unlike `new`'s, a factory's heap
  object is only the constructor's `this`: it is not copied into the smart
  pointer the call yields, and a file-scope factory, which has no
  constructor site, makes none. Copying it changed no call edge on the
  pinned corpora or, with an unlimited solver budget,
  `ability_ability_runtime` (virtual calls dispatch by
  class hierarchy; a field the constructor stores reads back through any
  result, `s->cb()` reaching what `T::T` stored into `cb`, because fields
  are instance-insensitive `FieldSummary` cells), yet it put a heap
  location into every smart-pointer value and cost 43% more solver pops
  there; under the eval budget the solver stopped short and lost indirect
  edges ([measurements](EVAL_REPORT.md#factory-construction-192--2026-10-06)).
- **Not modeled.** Factories of other names (project wrappers,
  `std::make_shared_for_overwrite`) are not recognised; each is a table
  row.

### Template-parameter bases

This section is the authoritative description of subclasses reached through a
class template's parameters (`template_bases.rs`). With
`template<class I> class IRemoteStub : public I`, a
`class S : public IRemoteStub<IFoo>` derives from `IFoo`. A base computed by
`decltype(...)` is not a parameter base: it is omitted from the parse input
and never resolved (see
[C++ parse-input normalization](#c-parse-input-normalization)).

The fact. Lowering records a `ClassTemplate` (`Program.class_templates`) for a
primary class template definition whose bases mention its parameters: the
parameter names by position, the position of a parameter pack, each type
parameter's default, and those base spellings (`I`, `Layer<T>`). A default is
qualified in the template's own scope and recorded with a leading `::`, so it
names the same class wherever the template is instantiated; a default that
mentions another parameter is not recorded. C++ lets a default be written on
one declaration only, often a forward declaration
(`template <class I, class D = IDef> class Def;`), so such a declaration
records its defaults too. A template defined in an anonymous namespace is
file-local: it is recorded per file (`Program.anonymous_class_templates`), as
anonymous-namespace classes are, since another file's template of the name is
another template, and it is visible only within its translation unit (see
Expansion). A partial or explicit specialization records nothing. A base whose class is a template parameter, of the class's own
template or of an enclosing one (`struct Inner : T` in `Outer<T>`, or `B` in
`: public B<T>` with a template template parameter `B`), is not a class and
gets no inheritance edge. Lowering counts the `template_declaration`s it is
inside, so only a class within one looks at its enclosing parameters.

Expansion. For each concrete templated base a class spells (a `TemplateBase`
that is not dependent), `parameter_bases` substitutes the written arguments
into the template's recorded bases. A base whose class is a parameter yields
the argument (instantiated, for a template template parameter: `B<T>` with
`Layer, IC` yields `Layer<IC>`), looked up in the **derived class's declaration scope** (the scope
IPC pairing resolves an `IRemoteStub<IFoo>` argument in), and becomes an
ordinary `derived → base` inheritance edge; an anonymous-namespace derived
class gets its file-scoped base as well. A substituted base that is itself a
template instantiation is expanded the same way, breadth first, so each
instantiation is expanded once, at the least depth it is reached, and nesting
stops at depth 8. Expansion runs twice: while lowering the derived class, so
member lookups later in the unit see the edge, and once after merge, before
virtual-override expansion, for templates another unit defines. One lookup
(`class_template`) serves both passes:

- While lowering, a file-local template is visible to its whole
  **translation unit**, as in C++. A template name resolves to the
  file-local template of the deriving class's own file (the file its class
  specifier is in); else to one defined in another file of the unit's
  `#include` closure (the preprocessor's `included_headers`: a `.cpp`
  deriving through an anonymous-namespace template of a header it
  includes); else to the template with linkage (`Program.class_templates`).
  The unit's program can also hold file-local templates of headers the
  unit never includes (header facts reach it through the include graph,
  which ignores `#if`); those are not seen. Well-formed C++ has at most one
  file-local template of a name in a unit; where there are several, the
  lowest `FileId` is taken. Where a file-local template and a linked one of
  the name are both in view, which C++ would reject as ambiguous, the
  file-local one wins. Nested bases resolve from the same file and unit.
- After merge, only templates with linkage are consulted: a file-local
  template was expanded while lowering its own file, and its name does not
  say which file's template a class derives from, so it neither hides nor
  replaces a linked template of the name.

The post-merge pass walks `Program.template_bases` in order.

Handled:

- nesting: `BarImpl : Wrapper<IBar>`, `Wrapper<T> : Layer<T>`,
  `Layer<I> : I` makes `BarImpl` derive from `IBar`;
- packs: in `template<class T, class... Is> class Multi : public T,
  public Is...`, `Is` binds each remaining argument, one base each; a pack
  expanded inside a base's arguments (`Pack<Ts...> : B<Ts...>`,
  `B<IC, Ts...>`, or through a pattern, `B<X<Ts>...>`) is one copy of the
  expansion's pattern per remaining argument, comma-joined at its position
  (nothing, with its separator, for an empty pack), so `B<X<Ts>...>` with
  `IA, IB` is `B<X<IA>, X<IB>>`, and the result expands as any templated
  base;
- omitted arguments take the recorded default;
- `Outer<A>::In<B>` binds `In`'s own (last) argument list;
- template template parameters: `SvcC : Wrap<Layer, IC>` with
  `Wrap<B, T> : B<T>` derives from `Layer`, and through it from `IC`;
- anonymous-namespace derived classes, and file-local templates of one name
  in several files.

Merging (`add_class_template_fact`, per file for a file-local template).
The first fact with bases names the
parameters and the pack, since the bases spell the definition's names; a
default fills its position from whichever declaration writes it; later facts
add only bases not yet recorded.

Templated dependent bases (`Layer<T>`) are recorded both in `ClassTemplate`
and as a dependent `TemplateBase`. The two are not interchangeable:
`TemplateBase` facts are keyed by the class name lowering gives, which a
partial specialization shares with its primary template (`W<U*> : Layer<U>`
is recorded under `W`), and include bases that mention only an enclosing
template's parameters; `ClassTemplate` holds the primary definition's bases
alone, spelled in its own parameter names.

Member lookup. The written base's edge is recorded before the substituted
one, and a hierarchy search is breadth first, so a member of the template
hides its argument's member of the name: in a `FieldSvc :
FieldStub<IField>` body, `x` is `FieldStub`'s own `x`, not `IField::x`.

Effect. `derives_from` and `subclass_closure` see the subclass, so
[smart-pointer unwrap](IR.md#smart-pointer-unwrap) admits its objects for the
interface's pointee type and CHA virtual dispatch through the interface
reaches its overrides. For the recognized binder wrappers this includes
member lookups inside the wrapper itself, so a handler body's unqualified
interface call is not left as an external free-function call (see
[Undefined templates](#template-parameter-bases)).

Undefined templates (#122). OpenHarmony's `IRemoteProxy` and `IRemoteStub`
are defined in the ipc component, outside most trees, so a
`class FooProxy : public IRemoteProxy<IFoo>` would reach no interface. After
merge, a concrete templated base whose template the tree does not **define**
(absent, or only forward-declared; a definition from a `--dep` root counts
as one) derives the class from its sole template argument, a guess from the
spelling like the [undeclared wrapper](IR.md#smart-pointer-unwrap) rule (#86):

- only with exactly one argument, naming a class the index declares (a
  scalar, a pointer, a reference, a cv-qualified type, a template instance
  such as `Holder<IFoo>` or an unknown name gives nothing);
- only when the class, or a class deriving from it, declares a member named
  like a virtual member of the argument or of the argument's bases: a proxy
  overrides its interface and a stub's service implements it, while
  `Registry : Box<Handler>` or `CaseTest : testing::TestWithParam<Case>`
  implements nothing of its argument. A marker interface without virtual
  members is not guessed, which loses no dispatch;
- never for a standard-library template (none derives from its argument),
  or for a member of an instance (`Outer<IFoo>::Inner` and
  `Outer<IFoo>::Inner<IBar>` are not `Outer`). A class in an anonymous
  namespace keeps its guessed base in its file's view, as lowering keeps a
  declared one;
- never the class itself (`Svc : DelayedSingleton<Svc>` is CRTP, not
  inheritance), and never an edge that closes a cycle. Guesses land after
  every declared base, so a guess never displaces one (`A : W<B>` and
  `B : W<A>` guessed add only the first);
- never for a template the tree defines, whatever its bases: a visible
  `template <class T> struct Plain {};` means `Plain<IFoo>` does not derive
  from `IFoo`, and the declared rules above decide.

The guess for an arbitrary undefined template runs once, after merge, so a
definition any unit (or dependency root) contributes is seen first; lowering
makes no provisional guess for it. The recognized binder wrappers
(`IRemoteStub<I>`, `IRemoteProxy<I>`, `IProxyBroker<I>`) are the exception:
their first template argument is an interface by construction (trailing
policy arguments such as `IRemoteStub<IFoo, Policy>` are ignored), so lowering
adds it as a base immediately, with the same cycle guard. Unlike the
arbitrary-template guess it does not require override evidence: a wrapper
holder gains the edge, but contributes no dispatch target unless it overrides
the interface. A handler body such as
`AppSchedulerHost::HandleScheduleAbilityStage` therefore resolves its
unqualified `ScheduleAbilityStage(...)` call to the interface member instead
of a free function, and virtual-override expansion reaches
`MainThread::ScheduleAbilityStage`. `trace-merge` also recovers that edge from
the exported base fact and implicit member call, but analysis needs the base
earlier to keep the call's dispatch and value flow.

Limits:

- The standard-library exclusion reads the spelling: `vector<Handler>`
  after `using namespace std` is guessed like any undefined template when
  the evidence rule holds. An alias template
  (`template <class T> using Box = Holder<T>;`) is not lowered, so a base
  spelled through one is guessed as if its template were undefined.
- A template the tree defines must have its definition indexed to expand
  (for example with `--dep`); only an undefined one is guessed. IPC bridge
  pairing does not depend on either: it recovers the interface from the
  `IRemoteStub<IFoo>` argument
  ([OpenHarmony IPC bridges](MODELS.md#openharmony-ipc-bridges)).
- Every argument resolves in the derived class's scope, including a
  non-parameter name in a dependent base (`A<T, Helper>`) and the inner
  arguments of a templated default.
- A non-type parameter's default, and a type default that mentions another
  parameter, are not recorded, so default filling stops at that position.
- An exact global template name wins over the scoped lookup.
- A base spelled with a partial specialization's arguments expands through
  the primary template's bases.

### Indexing syntax reuse

Large preprocessed inputs often repeat installed headers that remain inline in
each TU or project header. Each indexing worker retains the preceding syntax
tree and normalized source separately for C and C++. Before parsing a similar
input, tree-sitter edits cover the changed bytes, with UTF-8 boundaries and
byte-based row/column updates. Tree-sitter revalidates the retained syntax in
its new context. Returned trees are independent
clones; editing the cached tree does not change a tree still being lowered.

Error recovery is not invariant under tree reuse. A preceding tree with errors
is discarded before editing. If an incremental parse reports any error, the
input is parsed again without a preceding tree, and that fresh tree is the
result used by lowering and diagnostics. Error trees are never retained for
reuse. This keeps malformed inputs independent of worker assignment and
`--jobs`; error-free inputs retain the syntax-sharing path.

Retention requires 16 KiB–4 MiB of normalized text and at most 1,000,000 syntax
nodes per language per worker. If the common prefix and suffix together cover
half the shorter input, a single edit covers the intervening bytes. Otherwise,
unique matching lines of at least 32 bytes anchor exact unchanged middle blocks.
The search retains at most 32 ordered blocks of at least 8 KiB, spending at most
four times the larger input's length on byte extensions. Prefix, suffix and
middle blocks must together cover a quarter of the shorter input. At most 33
edits, applied right to left in original coordinates, cover the remaining gaps.
Matching uses exact text, and the search never relies on a hash alone.
Small, oversized, or unrelated inputs release the preceding tree before parsing
afresh. These are source-size
and node-count limits, rather than a bound on total parser memory.

The field-layout, prototype and definition passes share each member's
syntax-only function-versus-field classification within its live tree. Recovery
queries use the original walker; scratch initializer trees receive isolated
caches, and all retained classifications are released with their tree.

Only syntax is reused. Macro expansion and
[C++ parse-input normalization](#c-parse-input-normalization) run for the current
input; lowering starts with its own symbol/type environment and uses its own
LineMap, dependency classification and link ownership. No symbols, types, or
source locations are inherited from the cached unit. Standalone parse helpers
and scratch parses use fresh trees. Measurements:
[system-header indexing](EVAL_REPORT.md#system-header-indexing-on-small-projects--2026-10-15)
and [middle-block reuse](EVAL_REPORT.md#system-header-middle-block-reuse-and-import-cost--2026-10-15).

Indexing progress distinguishes shared-header parsing (`pch-done`) from its
initial merge (`pch-merge-done`). Parallel `parse-orphan-done` and `parse-done`
messages count units after their ordered merge, reporting cumulative elapsed
time for that phase at the first, every fiftieth, and final unit. Sequential
messages retain their per-unit timings. These counters include lowering and
merging; they do not measure merge CPU time alone.

### C++ parse-input normalization

**Unavailable declaration macros (#231).** After ordinary preprocessing and
normalization, the parser can retry two exact unresolved C++ spellings. Real
definitions from headers or configuration expand normally first; tokens with
macro-expansion provenance (including a self-referential definition) are never
removed by this recovery. No macro is installed or changed, and conditionals
are evaluated entirely by the ordinary preprocessor.

- `FFI_EXPORT` as a prefix to an explicitly typed single-function prototype or
  definition, at file or namespace scope, including within `extern "C" { ... }`,
  after `extern "C"`, or before a single-declaration `extern "C"`. Recovery
  requires a parse error in the original prefix/type region and an error-free
  complete function in the retry. Pointer and reference return declarators
  are accepted; variables, function-pointer variables, class-body members,
  operators, templates and block-scope declarations are outside this recovery.
  Forms the grammar accepts without a prefix/type error, such as
  `FFI_EXPORT long f();`, keep ordinary lowering and are not repaired here.
- `DECLARE_SINGLETON(ClassName)` as a complete item directly in the body of
  that named class or struct, with one unqualified argument matching the class
  name. Comments and newlines between tokens and an optional following
  semicolon are accepted. The surrounding class body must parse without
  errors in the retry. This form usually produces a missing-semicolon error;
  with an explicit semicolon the grammar may accept it as a declaration, so
  that exact form is also recovered without requiring a parse error.

Only the prefix or exact invocation is replaced with whitespace in parser
input; byte lengths and CR/LF positions remain identical. The retry validates
the syntactic context before accepting edits; rejected edits are restored and
the result is revalidated. Each macro-recovery retry applies the same virtual
declarator-name and member-pointer precedence repairs as ordinary parsing,
using the original spelling and unchanged byte offsets. Recovering a macro
therefore preserves repairs needed elsewhere in that translation unit. The
edited retry trees never seed incremental reuse.
Explicit function names, types, bodies, C linkage,
neighboring members and access specifiers follow ordinary lowering, with
original positions supplied by LineMap. Other macro-like identifiers and
unsupported contexts are left alone. Unrelated syntax errors retain their
ordinary parse diagnostics. Ambiguous or still-malformed declarations retain
their original input and diagnostics rather than being guessed.

The approximation is based on the dependency actually included by Camera:
[`camera_ffi.h` at `8ffd69d`](https://github.com/openharmony/multimedia_camera_framework/blob/8ffd69dcd47f533e70b4dba428439da9008b0cae/frameworks/cj/camera/include/camera_ffi.h)
includes `cj_common_ffi.h` and `ffi_remote_data.h` before its `FFI_EXPORT`
declarations. OpenHarmony's
[`arkui_napi` `cj_common_ffi.h` at `9be83fd`](https://github.com/openharmony/arkui_napi/blob/9be83fd128150c67f3495598af3c72dba1d44237/interfaces/inner_api/cjffi/cj_ffi/cj_common_ffi.h)
defines `FFI_EXPORT` as a default-visibility GNU attribute, or
`__declspec(dllexport)` under `WINDOWS_PLATFORM`, guarded by `#ifndef FFI_EXPORT`.
This supports treating the missing prefix as an export annotation, but does
not prove the unavailable definition in any particular build is identical.
Each accepted recovery emits a `parse` warning at the original macro's file
and line using the existing diagnostic representation (which has no column
field). For `FFI_EXPORT` the warning states that annotation semantics were
omitted: export visibility and other unavailable annotation effects are not
inferred. For `DECLARE_SINGLETON` it states that macro-generated declarations
are unknown and were not indexed. No accessor, constructor, destructor or
other presumed generated member is synthesized. Results describe only the
explicit declarations; restoring the real dependency is necessary to index
the missing semantics. Recovery reads only the unit's text, tree and LineMap,
and uses source order, independently of job count and expansion-cache replay.

**Parenthesized `auto` local initializers.** The grammar reads
`auto self(shared_from_this());` as a function declaration, whose parameter
type is `shared_from_this` and whose declarator is an abstract function.
Lowering checks the apparent parameter type in the original scope using the
type/value lookup described below, including type template parameters and
names brought in by `using` declarations or directives. An unqualified
template-id is classified by its template name, as a qualified one is. A
single bare value name, or an apparent function call whose name is a known
value, is reparsed with the initializer's outer parentheses changed to `=`
and a space. A bare name that resolves to neither a value nor a type keeps
the block-scope value assumption direct initialization already makes for
`T w(E::kind);`; a call-shaped name that resolves to nothing keeps its
function interpretation. The standard `shared_from_this()` /
`weak_from_this()` intrinsic is recognized from the class's base clause even
when `<memory>` is not in the tree, so its parenthesized initializer recovers
without system headers. Empty parameter lists, known parameter types and
named parameter declarators keep their function interpretation. Recovery
requires a complete, error-free declaration and retains the original byte
offsets, LineMap and enclosing scope, using the same scratch-tree lifetime
and relocation rules as callback recovery below. Recovery targets
function-local declarations; namespace-scope spellings are left unchanged.
Ordinary declaration lowering then deduces and copies the initializer value;
the body walk visits its evaluated expressions once. Already parsed
argument-list initializers (`auto self(this->shared_from_this());`, extra
parentheses included) expose their one expression directly to the same
deduction and flow path. The deduced class's constructor is not called with
that expression as an argument. The type-template-parameter check applies to
non-`auto` direct initialization too: `Gadget make(T);` inside
`template<class T>` remains a declaration rather than defining an object.

**Empty-parameter callback declarations (#219).** The C++ grammar can silently
read `void post(void (*cb)());` as an initialized variable, and `void (*cb)();`
as an expression. Lowering recovers only these callback-shaped declarations by
reparsing the original statement inside a scratch class body (or, for qualified
function names, with a scratch empty function body). Recovery requires an
error-free, complete declaration containing a named function pointer with an
empty parameter list. Built-in types, enclosing template type parameters, and user
types resolved in the original scope are accepted; value names hiding types
retain their expression meaning, including instance fields and callable template
value parameters. Lookup honors nearer local aliases and template type parameters
before enclosing members, and checks each class's types and values together
before its bases. Using-declarations follow the shared lookup's import precedence:
body imports precede enclosing bindings, and namespace imports precede globals.
Imported values keep their expression meaning; imported types permit recovery. A template type parameter establishes only its own name as
a type; a qualified member such as `T::make` needs independent type evidence
and otherwise stays an expression.
Unknown type names do not independently justify recovery. The scratch wrapper
is replaced with the original source prefix's coordinate extent by editing the
scratch tree, so child and parent traversal share the original positions.
Names and LineMap positions still come from the original source; template
ancestry joins that statement's original enclosing scopes with the recovered
declaration's own nodes, excluding the scratch wrapper. A single-declaration
linkage specification retains its implicit `extern` status; a braced linkage
block does not imply it. Other declarators keep their direct initialization and
constructor/destructor sites. Return operations for sibling field initializers
span only their declarator and initializer. Scratch trees live until the unit
finishes, so node-ID caches cannot alias a later recovery. Existing declaration lowering
supplies types, linkage, initialization flow, and dependency handling; no scratch class
or function body is indexed. Within a function, recovered declarators' array bounds
and initializers are visited in source order; prototype parameters and their
defaults are not evaluated in the enclosing function. Correctly parsed declarations,
typedefs and aliases use their ordinary path. This is declaration recovery, not a
grammar upgrade.

The pinned tree-sitter C++ grammar (tree-sitter-cpp 0.23.4) rejects a few
standard or GNU spellings. Rather than fork the grammar, the parse input of a
C++ translation unit is rewritten in place before parsing
(`normalize_cpp_parse_syntax` in `trace-parse/src/parse.rs`), after the
dependency-only pass (`normalize_dependency_cpp_syntax`, see
[Compilation databases](USAGE.md#compilation-databases-62)). Adaptations preserve byte
lengths and CR/LF (usually by blanking bytes; virtual insertions described
below are removed from tree coordinates before lowering), so node offsets, LineMap
lookups and exported line/column positions still name the original source. The rewrite is
a parse adaptation only: it changes what the grammar sees, never what lowering
records as semantics beyond what is stated below. Spellings the recognizers do
not fully match stay unchanged and keep their parse diagnostic.

The function first checks cheap substring triggers (`friend` with `[[`; `::`
with `*` and `(`; `decltype`; `__typeof`; `__attribute__` with `extern`;
`template`; `->*`) and scans the unit once, outside string/character literals, raw strings and comments. A
standard-library-heavy unit can satisfy these substring triggers without needing
an edit. This normalization pass is one linear source scan plus scratch
validation parses for type candidates. Ambiguous qualified data-member owners
also inspect their preceding declaration prefix to ensure that adapting the
owner cannot erase the member type. On the first such check, normalization
builds a temporary index of lexical reset offsets in one source scan. Each
check starts at the nearest preceding reset, using the same comment, literal,
attribute and type-group rules as prefix validation. Repeated declarations
therefore do not rescan earlier declarations. Ordinary indexing restores only
owners already blanked in the parser input, reusing that decision without
another prefix scan. Standalone lowering without saved parser spelling checks
only the enclosing AST declaration. Keyword-delimited global owners need no
ambiguity check.

Declaration repair (`pointer_declaration_insertions`) has a separate substring
guard (`template`, or `::` followed by a star after optional whitespace and
comments) and scans the original source
after the initial parse. Its lexical walk skips comments and literals and
requires complete keyword tokens, but even a trigger found only inside a
comment or literal can cause this extra scan. Thus triggered C++ input can pay
two linear source scans, plus scratch type-validation parses. Nonempty
insertion plans require an additional full declaration parse; member-pointer
precedence repair can require another grouping parse. Clean member-pointer
parameter declarations avoid the declaration reparse, not its preceding scan.
The ordinary placeholder tree remains eligible for incremental reuse; virtual
repair trees do not seed that cache. These repairs are optional: if a declaration
or grouping parse returns no tree or increases the number of innermost
unrecovered parse errors, indexing retains the preceding usable tree. Rejected
declaration insertions are also excluded from the later grouping repair.

Recognized spellings:

- **GNU visibility attributes before C linkage** —
  `__attribute__((visibility("default"))) extern "C" ...` has its visibility
  group and intervening trivia blanked. Complete groups containing only
  `visibility("...")` or `__visibility__("...")` are recognized, including
  comma-separated entries, consecutive groups, comments and newlines between
  tokens. The `extern "C"` tokens, declaration or definition, and function
  identifier stay in place, preserving external linkage and original spans.
  The accepted reverse order (`extern "C" __attribute__((visibility("default")))`)
  stays unchanged. Visibility does not affect the analysis's symbol identity;
  other attributes, including mixed groups containing `weak`, are left to the
  ordinary parser so this adaptation cannot erase their semantics. Incomplete
  attributes and unrelated syntax errors retain diagnostics. This accommodates
  the pinned parser's input limitation without asserting compiler acceptance
  of the source spelling.
- **Attributes before `friend`** — `[[nodiscard]] friend bool operator==(...)`
  and `[[nodiscard]] constexpr friend ...`: the attribute specifiers are
  blanked (see "Friend declarations and operator attributes" above).
- **Parenthesized member-function pointers (#170)** — the complete shape
  `( owner :: * [cv] [name] [array-bounds] ) ( params ) [suffix]` with a simple,
  identifier-qualified, template-qualified or globally qualified owner (`C::`,
  `ns::C::`, `Box<int>::`, `::ns::C::`) and comments or newlines
  between tokens, in every declaration context: `using` aliases, named
  typedefs, template specialization arguments (`traits<R (C::*)(Args...)
  const>`), function parameters, fields and variables. The owner is blanked,
  turning the type into the ordinary function pointer the grammar accepts;
  `*`, the name, the parameter list and the trailing qualifiers stay. Lowering
  restores `MemberPointer { owner, is_function, member }`, distinguishing
  member functions from data members even when a data member itself holds a
  function pointer. Spelling comparisons strip only ordinary pointer layers:
  the owner and function/data category remain part of member-pointer identity,
  including class- or union-valued members. Ordinary pointee and pointer-depth
  queries stop at the member-pointer descriptor; callable consumers use
  `TypeDesc::callable_signature` to inspect a member function signature explicitly.
  Data-member pointers retain their category even when the member is a class,
  array, or ordinary callback. The member function's inner `FnPtr` keeps parameter zero
  as a pointer to its owner, followed by its explicit parameter types. This
  preserves the callable signature for member-pointer invocation (#246),
  including aliases, parameters, variables and fields. Unresolved template
  parameter types stay unknown. Member-pointer descriptors retain declared
  const and ref qualifiers for contextual address selection and invocation.
  `noexcept` and volatile qualification remain approximated. A declared
  ellipsis uses a distinct callable tail marker: address selection requires
  its complete fixed prefix and a variadic declaration, while an unknown
  parameter is a wildcard only at its own position. Variadic
  tails and parameter packs keep the owner and declared fixed parameter
  prefix, followed by `Ellipsis` for a declared ellipsis or `Unknown` for a
  pack's uncertain tail. Signature matching still checks the fixed prefix, so
  `void (C::*)(int, ...)` excludes `void (C::*)(double, ...)`. Ordinary
  function pointers follow the same prefix rule. Empty callable parameter
  lists denote known arity zero during type matching; only an unknown or
  ellipsis tail leaves arity open. A variable or field
  declared this way is named
  by its identifier (`cb` for `void (S::*cb)(int)`), not by the recovered
  declarator text. A **named typedef declarator** is the exception
  for the suffix: the grammar accepts `using M = R (*)(Args...) const;` but
  not `typedef R (*M)(Args...) const;`, so for a member pointer that is a
  `typedef` statement's own declarator (parenthesis depth 0 after the
  `typedef` keyword, at the brace depth of the keyword — a type body the
  typedef introduces, `typedef struct { ... } (C::*M)() const;`, is stepped
  over and its members stay fields — not a parameter nested in another
  typedef, even after a brace inside that parameter list such as a lambda or
  compound literal in a default argument) the function
  qualifiers `const`,
  `volatile`, `&`, `&&`, `noexcept`, `noexcept(...)` are blanked from the end
  of the parameter list through the last qualifier token. The suffix is first
  validated as a type on a scratch parse (`cpp_type_operand_parses`), so a
  malformed suffix leaves the whole declarator unchanged and diagnosed. The
  keyword is recognized directly, including after labels (`case expression:`,
  `default:`, `public:`), `__extension__` and attributes; colons in conditional
  expressions and template arguments do not change typedef detection. An
  unterminated `noexcept(` is recognized as part of the shape precisely so no
  pass degrades it to owner-only blanking. Data-member pointer owners
  (`F S::*`, in variables, `using` aliases and typedefs) are also blanked for
  parsing and restored in their corresponding pointer layer during lowering.
  The reversed declarator chain maps each pointer, array and function to its
  own type layer, even when the data member's type contains a callable:
  `void (*S::*pm)()` retains a data-member pointer to a callback, while
  `F S::**p` retains an ordinary pointer around the data-member pointer.
  Field arrays use that complete declarator shape once, with each constant
  dimension retained at its own layer; they are not wrapped again by the
  legacy field-array fold. Member-function-pointer fields omit their implicit
  callable storage-pointer layer, retaining explicit pointers around them.
  Layout therefore counts every array element before placing following fields.
  A global owner starts at its leading `::`: adapting `int ::C::*` retains
  the `int` declaration specifier rather than treating it as an owner component.
  Without a keyword boundary, a global owner after a typedef, class or template
  type (`MyT ::C::*`) is ambiguous to this lexical pass. It leaves that spelling
  untouched and records a distinct `member-pointer owner not recovered` warning
  at each declaration's original file and line, including when the grammar
  accepts it as an expression statement. Generic parse-error warnings report
  genuine tree errors independently. Unqualified owners (`MyT C::*`) retain ordinary
  adaptation. A separate member type must precede a qualified owner; comments,
  attributes, storage qualifiers and template parameter lists do not supply one.
  Function-member stars are identified separately, so callback layers do not
  acquire an implicit owner or hide a surrounding data-member pointer.
  Named member-pointer parameters containing bare aliases get virtual parameter
  identifiers to remove expression/declaration ambiguity. These identifiers
  become zero-width tree nodes and never become IR names. Already parsed
  parameter declarations, named or unnamed, retain their initial tree; only
  ambiguous declarations need this additional full-source parse. Empty-parameter
  pointer arrays use the ordinary callback-declaration recovery. Literals and
  comments are untouched. Invocation is handled separately below. In dependency
  headers the dependency pass recognizes this shape at its opening `(`,
  skipping comments (including line comments) and whitespace forward. It
  leaves the owner to the general pass, before its legacy data-member rule
  can rewrite `::*`, so the general pass can apply the
  complete edit once, and the general pass is idempotent on text the
  dependency pass already rewrote. A class head with a base list is scanned
  like any other text, so a specialization such as
  `struct traits<R (C::*)(A...) const &> : base<...>` is rewritten whatever
  else the unit contains.

- **Explicit instantiations (#246)** — `extern template ...;` blanks only
  `extern`; class/struct instantiations also blank the class key, allowing
  the pinned grammar to retain its native `template_instantiation` node
  without a missing declarator. Validated GNU and standard attributes following
  that class key are blanked too, including attributes expanded from macros. Qualified names, template arguments, return
  types and function parameter lists stay in place. Both declarations and
  definitions are skipped by lowering: they request instantiation of the
  existing representative template, without inventing a body, a specialization
  identity or a second function. Ordinary template declarations and explicit
  specializations keep their existing handling. Malformed instantiations and
  neighboring syntax errors retain diagnostics.
- **Unnamed pointer defaults in template heads (#246)** — a validated
  pointer-shaped unnamed parameter (`** = nullptr`, `* const = nullptr`,
  nested template arguments included) gets a virtual identifier before its
  default. The fresh parser tree is edited back to original offsets, leaving
  the identifier zero-width; lowering excludes it from template names. The
  returned source keeps every original pointer token, without substituting a
  reference. Pointer defaults in following type parameters (`class U = T *`)
  use a virtual `decltype(static_cast<T *>(nullptr))` wrapper denoting the same
  type, with all inserted bytes removed from tree coordinates. Named pointers
  need no adaptation. Unvalidated types and neighboring malformed syntax keep
  diagnostics; no error nodes are suppressed.
- **Member-pointer invocation (#246)** — expression operators `->*` and `.*`
  use a binary `+` placeholder in parser input, padded to preserve byte offsets
  and CR/LF positions. This keeps postfix operations in the callable operand:
  `(p->*callbacks[i])(args)` has `callbacks[i]` as its RHS, including parentheses,
  field chains and macro substitutions. A multiplication placeholder would
  introduce a cast/declarator ambiguity for a parenthesized RHS. Lowering and
  sequencing read the original operator; the placeholder adds no arithmetic
  flow. Where surrounding binary arithmetic would change grouping, parsing
  temporarily parenthesizes member-pointer bindings at their original C++
  precedence (above multiplication, below unary operators). The parser tree
  is edited back to original offsets before lowering; the returned parse
  source retains its original length and line positions. The grouping walk
  carries its enclosing binary-expression state on an iterative stack instead
  of repeatedly asking tree-sitter to find each node's parent. Nested bindings
  retain every inserted parenthesis, including repeated openings or closings
  at the same original byte offset. Number tokens (including `3.*p`) and ellipses (`...*xs` in a
  fold expression) are consumed whole before recognizing these operators.
  Number scanning starts only at token boundaries, so a digit in an identifier
  such as `obj1e` cannot absorb the minus sign of a following `->*`.
  C++17 still sequences the receiver before the callable operand; earlier
  versions leave their evaluations unordered. Array operands use the ordinary
  table/field-summary loads, retaining every candidate for an unknown index.
  Operator-function names (`operator->*`), including declarations, addresses
  and explicit calls with intervening comments, retain their spelling and
  identity. Lowering reads
  the original preprocessed spelling, resolves `f` as a function value, and
  binds the object's address (or `p`'s value) as implicit argument zero.
  Explicit arguments, including callbacks, start at one. Assignments and
  cross-TU parameters propagate ordinary function addresses. An instance-field
  operand (`this->*cb`) uses the same cached implicit-field load as an ordinary
  callback call, retaining its callable type, inherited-field summaries and
  local/parameter shadowing. Inline member addresses (`(b->*&B::f)()`) become
  callable temporaries through ordinary `AddrOfFn` flows, so they use the same
  receiver binding and virtual-dispatch policy as a local member pointer.
  Each invocation expands its selected override's returns into its own call
  destination, even when another receiver invokes the same pointer variable.
  The callable's return type also types chained receiver calls, restoring the
  separate reference-return binding when applicable.
  Call-result operands and inline casts likewise materialize a cached value,
  preserving return flow and the outer cast's invocation type. Object-valued
  `.*` receivers can be temporaries. Data-member pointers carry the existing
  instance-insensitive field-summary address. Reads, returns, arguments and
  invocation load the selected value; assignment stores through that address.
  Calling a callback-valued data member binds no `this`. Object-valued members
  retain their receiver type, including calls to their `operator()`. Selected
  cells compose with subsequent built-in subscripts and field accesses
  (`(p->*pm)[i]`, `(p->*pm).field`, `(p->*pm)->field`), retaining each selected
  element's type and field-summary flow. `auto`, `decltype`, aliases and array operands
  retain the data-member category; a dependent template parameter keeps both
  member-function and data-member call alternatives until its value resolves.
  A known data-member alias also repairs the grammar's ambiguous
  `(p->*(M)&C::field)` spelling for built-in binding. This repair requires a
  raw pointer for `->*`; a class receiver may overload the operator, so its
  result cannot be replaced with the selected field. The ambiguous C-style
  spelling on an overloaded receiver may retain the operator edge without
  its returned callback; a named `static_cast<M>(&C::field)` retains both.
  An inherited address such as `&D::a` uses the shared upward member lookup.
  Shared field-hiding metadata takes precedence over the inherited-method
  fallback: when a nearer data member hides a base method, the address denotes
  the field and retains its value flow. Explicit base qualification still
  denotes the base method.
  Calls recovered while resolving operands retain loop and unevaluated context. Class-valued receivers with a declared
  `operator->*` resolve member operators and non-member operators in the
  operands' associated namespaces (including friends), even when the class
  declares a member operator. A raw pointer with a class-valued RHS also
  considers free operators; its pointee's member operators do not participate.
  Member operators use ordinary receiver cv/ref selection before their result
  type and call targets are determined, so lvalue and rvalue overloads can
  supply different receiver classes for subsequent member calls.
  A non-class RHS on a raw pointer retains built-in member-pointer binding.
  Ordinary enclosing scopes and `using` imports participate alongside ADL;
  enum-shaped selectors also admit free-operator lookup. A lone candidate
  still checks known class compatibility, retaining applicable recorded
  conversion operators and constructors rather than unrelated class bindings.
  Free receiver parameters use the shared class-conversion policy, admitting
  a `Derived` value or reference where a base value or reference is taken.
  Both sets participate in the shared overload ranking with
  the free operator's receiver parameter aligned to the member's implicit
  receiver. Ties and unknown argument types retain compatible alternatives
  in one operator event; each alternative keeps its own argument binding.
  Overload resolution precedes built-in data-member cell selection for reads
  and arguments. The operator's return value flows through initializers,
  assignments, returns and immediate callback invocations; a returned functor
  invokes its ordinary `operator()` using the materialized receiver and shared
  overload selection with the original expression's cv and value category.
  A materialized value does not turn a returned temporary into an lvalue. Implicit
  operator calls inside `sizeof` remain unevaluated, as explicit calls do.
  Its agreed return type also supplies the receiver type for a chained
  `->*` operator, retaining each separate invocation.
  The operator owns a separate CFG event, sequenced before the callback,
  with distinct macro occurrences for the two executions.
  Free operator overloads compare both operands through shared argument ranking;
  known derived-to-base conversions lose to an exact receiver binding. Their
  return flow keeps the selected `FnId` across merging instead of reopening the
  same-name overload set. Unions participate in the class-valued operator lookup.
  Built-in `->*` uses pointer conversions from its left operand, independently
  of any `operator->()`. Conversion declarations and definitions share target
  typing, including the existing class-template return substitution.
  Call lowering and return-flow capture share the cached value; explicit casts
  select overloads before addresses are published. Overloaded member addresses
  retain their candidates
  until the pointer's signature can select them. A typed destination also
  selects an uncast member address before it
  becomes a local value, assignment, array member or return value
  (`M f = &C::hit` and a function returning `M` keep
  the overload declared by `M`). Mixed static/instance overload sets are
  inspected before classifying the address: a member-pointer signature selects
  the instance method even when initial name lookup chose its static sibling;
  an ordinary function-pointer destination selects static siblings by its
  return and explicit parameter signature, including aliases, bare qualified
  names, typed stores, formal callback arguments and inline casts.
  Arguments to a typed member-pointer invocation use its explicit parameter
  signatures for the same callback-overload selection as a direct call; the
  pointer's owner slot is excluded from those explicit parameter types.
  Ordinary and member-pointer signatures share the same compatibility rules;
  only a member pointer contributes an implicit owner parameter. A known empty
  C++ parameter list selects zero-argument functions and still checks the return
  type. Signature comparison restores the reference layer recorded separately
  in reference-return metadata before comparing the result, for both ordinary
  and member-function pointers. Reference aliases are normalized to their
  referent when publishing a function's return type, including references to
  pointers; the callable signature retains the separate reference layer.
  Callable signatures inferred from function
  addresses restore that same reference layer. Static member addresses infer
  ordinary function pointers using explicit parameters only; an instance
  member address also carries its owner slot. Data-member addresses infer
  their declaring owner from the shared hierarchy-field metadata: `&D::field`
  has owner `B` when `D` inherits that field from `B`. A nearer declaration
  hides a base field, and an explicitly declared destination can retain its
  own converted member-pointer type. Definitions and prototypes use
  the shared explicit-parameter metadata. Coarse signatures use an `Unknown` tail instead of an empty list; a
  callable wrapper with an unknown return type preserves that uncertainty too. Nested
  declarators refine their corresponding callable layers: in
  `typedef void (*(*S)(int))(double)`, the outer callable takes `int` and
  returns a callback taking `double`. A parameter's own callable declarator
  is refined independently of that return chain. Function return declarators
  retain callback and pointer-to-array shapes. The explicit member marker
  separates ordinary callbacks with a leading class pointer from owner slots.
  This contextual selection never crosses an
  explicit cast or reinterpretation barrier. Static-member callback re-selection
  uses the caller's link target. If incomplete signature or lookup metadata finds
  no compatible replacement, it retains the original function value for the
  solver's eventual signature check, independently for each actual at a pack position.
  An original value that admits the slot signature also survives when lookup
  omits it but returns another compatible overload. Removing a receiver position
  drops index zero from argument and address-evidence lists before renumbering.
  An overload-selecting `static_cast`
  filters compatible candidates before they become callback arguments, local
  values or array members.
  Selected overloaded arrow-star results retain the operator's return cv and
  reference category for chained receivers. Built-in subscript receivers inherit
  their element constness from the array object or pointer pointee.
  A C-style cast selects a matching overload when one exists; otherwise it
  can reinterpret an already identified function and preserves its value.
  `reinterpret_cast` preserves function values across signature changes and
  round trips, including returned addresses and values stored through pointer slots.
  A loaded operand is materialized with its own type, then copied into the
  reinterpretation node so its function locations become cast-qualified before
  any destination storage guard runs. Direct function addresses keep their
  contextual overload selection before acquiring that qualification.
  Callable casts in return statements copy a temporary carrying the cast-qualified
  function location rather than publishing an ordinary function address. Member callable
  signatures filter invocation, not storage. Reinterpreted function and member-pointer
  invocations permit return-type differences (including reference/value differences),
  retaining parameter and member-owner checks. This conservatively models ABI casts
  such as a void-returning thread worker stored as an int-returning entry point.
  Full return-type checks still apply when selecting an overload before reinterpretation.
  Its destination type supplies no selection of the original
  function, while an inner `static_cast` still selects its own operand through
  surrounding reinterpretations. Inline cast types and typedef/using aliases
  share parameter comparison and unknown-type tolerance with indirect invocation, so callback models
  and execution contexts retain overload selection without erasing reinterpreted
  values. Signature comparison admits unresolved template arguments on either
  side (`Box<T>` against `Box<int>`), including nested instances, without
  rewriting their stored types. An active template parameter that shadows an
  outer alias remains unknown in callable signatures, retaining its written
  pointer/reference shape. This also applies to standard integer alias names
  the grammar classifies as primitive types. A qualified alias such as `ns::T`
  still names that alias when an unqualified template parameter is named `T`.
  A nested dependent argument does not erase
  its enclosing template head or required pointer suffix; an unresolved leaf
  can absorb additional pointer/reference layers (`T*` against `int**`).
  Alias templates are not instantiated by lowering, so an undeclared template
  head (`Same<int>` in `Box<Same<int>>`) also remains unresolved and may match
  another argument type. Declared class heads still have to match. This can
  retain extra overloads when a template declaration is unavailable.
  Equivalent fundamental template arguments (`unsigned` and `unsigned int`)
  compare equally. Supported integral non-type arguments compare by value
  (`2`, `02`, `0x2u`; `true` and `1`), while distinct known values stay distinct.
  Apostrophes are removed only from numeric literals. Character literal values
  depend on an execution character set that the index does not model, so they
  conservatively match known integral arguments rather than interpreting `'1'`
  as the integer `1`. Distinct single unescaped characters stay distinct;
  escaped and multicharacter spellings retain possible aliases. Function
  parameters admit function-to-pointer adjustment. Named function-reference
  parameters retain their callable signature instead of a pointer to the return
  type. For a callee parameter marked as a reference, signature comparison
  reconciles the extra address layer an abstract array-reference declarator
  carries with the named parameter's array shape; element types and nested
  shapes still participate in matching. A callee's parameter pack leaves its tail open:
  a concrete callback must supply and match the recorded fixed prefix, while
  additional parameters can bind the pack. Member-pointer parameters compare
  dependent owners recursively (`int T::*` against `int C::*`), including
  inside nested callable parameters and return types. Each callable layer
  compares its return and fixed parameter prefix; an Unknown tail leaves
  only its remaining arity open. An explicit
  owner-dependency bit survives type interning and linking shapes, so an active
  template parameter stays dependent even when it shadows a declared class.
  Declared unrelated
  owners, function/data categories and incompatible member types stay distinct.
  These tolerances apply to callable selection without changing type identity.
  Unresolved standard integer typedefs
  (`size_t`, `int64_t`, `std::uint64_t`, etc.) admit integral spellings, including
  template arguments such as `Box<size_t>` against `Box<unsigned long>`. This
  tolerance does not assume the target ABI and preserves pointer/reference
  layers; these names cannot stand for floating or declared class types. Built-in
  character keywords (`wchar_t`, `char8_t`, `char16_t`, `char32_t`) do not receive
  that unresolved-typedef tolerance. Known concrete arguments (`Box<int>`,
  `Box<double>`) remain distinguishable. This comparison is local to callable
  selection and does not change declaration identity. Indirect-call handling
  excludes known unrelated receiver owners and incompatible return/parameter
  types. Receiver and declaring owner may be related in either inheritance
  direction: an explicitly converted `D::*` value can have type `B::*` and run
  through `B*` pointing to a `D`. A defined points-to target retains the
  definition selected where its address was taken, even when the invocation
  TU defines another same-signature body. Declarations use the shared scope,
  signature, TU and link-target resolver to reach definitions. Virtual
  alternatives use the same `Program::virtual_call_targets` policy as spelled
  member calls. Conversion-operator scope splitting stops before the conversion
  type (`B::operator ns::S` belongs to `B`); conversion addresses adapt to
  identifier placeholders and resolve using their original spelling. The
  adaptation consumes only conversion-type tokens and stops before surrounding
  expression operators or an unmatched template closing bracket, preserving
  conditionals such as `&C::operator bool == m ? alpha : beta` and a following
  declarator such as `Probe<&C::operator bool> probe`. Operators inside a
  validated template argument list remain part of the conversion type. Receiver
  type lookup also consults active using-directives, preserving imported class
  ownership and concrete base-member edges. Final boundaries and overload
  separation remain intact. An
  unresolved template receiver (`T`) dispatches from the bound member's owner;
  a declared receiver keeps its own hierarchy and final boundaries.
  Return values use ordinary `CallReturnIndirect` flows. This is still a
  may-analysis, without C++ ABI
  member-pointer adjustments or complete template specialization. Original
  operators stay in displayed expressions and callback call-site labels, so
  `inspect sequence --callee` matches the original arrow-star spelling.
  Header and macro spans use LineMap;
  a macro-body member-pointer operator anchors the spelling position and keeps
  the outer invocation as the expansion position. Expansion occurrence IDs
  keep repeated calls from nested macro expansions distinct.

- **Computed base classes (#167)** — `template<class T> struct D :
  decltype(make<T>()) { ... }`. `base_class_clause` admits only class names,
  so a class head (`struct`/`class`, optional `[[...]]`/`alignas(...)`/
  `__attribute__((...))`, a simple, qualified, globally qualified or
  template-id name such as `has_foo<T, void_t<decltype(check<T>(0))>>` or
  `Outer::Nested`, optional `final`) followed by `:` has its base list split
  at top-level commas, tracking template-argument nesting (`>>` included) and
  skipping parenthesized groups and string/character literals (`S<'>'>`).
  Concrete entries may be globally qualified (`::Base`). A complete entry of the form `[public|private|
  protected|virtual] decltype(expression) [...]` is a computed base; its
  operand is first validated as a type by `cpp_type_operand_parses`, and if
  every computed entry of the list validates, each one is blanked together
  with the separators it owned, leaving exactly one original comma between
  retained concrete entries and blanking the colon when nothing remains. A
  pack's `...` goes with its entry, so `: ...` is never left behind. The class
  body and every sibling byte are untouched, so lowering records only the real
  concrete bases; **computed-base inheritance stays unresolved**, including
  for non-dependent expressions, and no placeholder edge or name is invented.
  Not edited: a `decltype(...)::type` base (a qualified spelling the grammar
  already parses), a `decltype` inside a concrete base's template arguments
  (`Base<int, decltype(make<T>())>`), `enum class E : decltype(x)` (an
  underlying type, stepped over as a unit, attribute groups and other
  identifiers between `enum` and `class` included, such as an unexpanded
  `enum MYATTR class`; the search stops at any other token, so an unscoped
  enum's `:` or `{` ends it), template parameters
  (`class T = decltype(0)`), ternaries, constructor initializers, and any base
  list the recognizer cannot read completely (empty entries, double commas,
  unknown modifiers, an unnamed class, a base spelled with `::template`). A
  computed entry whose operand the
  grammar rejects (`decltype(f() +)`, `decltype()`) leaves the whole list
  unchanged, so a real syntax error is still diagnosed rather than hidden by
  the omission.

- **GNU `__typeof__` / `__typeof` (#169)** — the grammar has no typeof
  alternative in `type_specifier`. The keyword (10 or 8 bytes) with a complete
  parenthesized operand is handled by operand shape, classified lexically
  (`classify_typeof_operand`) without any symbol table:
  - *Simple type* — `[cv] core [cv] (* [cv])* [& | &&]`, where `core` is one
    or more fundamental words (`int`, `unsigned long long`, and the
    fixed-width/size names `is_fundamental_type_name` lists, such as
    `uint32_t`, which the grammar also treats as primitive) or one qualified
    name with at least one pointer/reference layer or a cv qualifier (`T *`,
    `T &&`, `const ns::Type *`, `const MyType`, `MyType volatile`).
    The operand text is validated as a type on a
    scratch parse; then the keyword through `(` and the closing `)` are
    blanked, leaving the operand in place (`typedef __typeof__(int *) type;`
    → `typedef            int *  type;`). Lowering records the type exactly
    as it would for the unwrapped spelling.
  - *Expression or bare name* — operands not covered by the simple or
    unsupported type rules, including `T() +
    U()`, `x * y`, `x & y`, calls, and a bare name without a layer or cv
    qualifier (`T`, `MyType`, `ns::value`), which syntax alone cannot tell
    from a variable.
    The keyword is re-spelled `decltype` padded to its length (`decltype  `
    for `__typeof__`), operand bytes and positions unchanged. This is a parse
    adaptation only; GNU typeof's reference semantics are not modeled.
  - *Unsupported* — an operand with a fundamental core and tokens outside
    the simple shape: abstract array or function declarators (`int[2]`,
    `void (*)(int)`), malformed modifier runs (`int const int`). These stay
    as written and diagnosed. This rule does not recognize every type-id:
    template-ids (`W<int> *`), elaborated types (`struct Foo *`) and dependent
    types (`typename T::type *`) take the expression path, are re-spelled
    as `decltype`, and remain diagnosed.
  Bare `typeof` (6 bytes) cannot be re-spelled in place and is left alone.
  Keyword substrings inside identifiers, strings and comments are never
  touched, and an unclosed group leaves the spelling unchanged. Unterminated
  ordinary and raw literals consume the rest of the input during recognition;
  their contents cannot close a group or introduce a rewrite. Known
  boundaries, recorded rather than modeled: a nested
  `__typeof__(__typeof__(x) *)` becomes a nested `decltype` and stays
  diagnosed; `struct D : __typeof__(B)` is re-spelled as
  `struct D : decltype  (B)` but remains diagnosed: computed-base recognition
  reads the original source, so it does not remove a base introduced by a
  typeof rewrite. The two adaptations do not compose in this case. A simple
  operand that heads a declaration with more than one declarator is not
  unwrapped, since a later declarator would get only the base type (`q` as
  `int` in `typedef __typeof__(int *) p, q;`); it stays as written and
  diagnosed. The scan after the operand counts a `,` at bracket depth zero
  once it reaches the declaration's `;`, so file-scope, block-scope,
  `for`-init and `if`/`switch`-init declarations
  (`for (__typeof__(int *) p = 0, q = 0;;)`) are all kept. A group that closes
  first (a parameter list, `void h(__typeof__(int *) a, int b);`), a `>`
  before any `=` (an enclosing template argument list,
  `std::map<__typeof__(int *) *, int> m;`), or a `,`/`>` directly after the
  operand means the commas separate other entities, and the operand is
  unwrapped. Template arguments in an initializer (`= g<int, int>()`) are
  skipped when their `<`/`>` balance; an unbalanced `<` is read as a
  comparison and a comma after it still counts (`= b < c, d;` is kept).
  Unwrapping applies to the declaration as spelled, so a leading `const`
  qualifies the pointee rather than the pointer.

  **Unresolved `decltype` lowers to `Unknown`.** `type_desc_from_node` now
  returns `TypeDesc::Unknown` for a `decltype` node, native or re-spelled,
  and `peel_cpp_type_node` treats `decltype` as terminal so its operand is
  never read as a type name. Before, the substring fallback made
  `decltype(avoid_copy())` `Void`, `decltype(charge())` `Char`,
  `decltype(T() + U())` `Int`, and peeling turned `decltype(Outer::Inner)`
  into whatever `Outer::Inner` names. `Unknown` is the designed
  over-approximation (it matches any type in `may_name_same_type`), so an
  alias such as `sum_result::type` is registered with an explicitly
  unresolved descriptor rather than a fabricated one. Expression type
  deduction is not performed; operands are never lowered as calls.

`cpp_type_operand_parses` parses `using __trace_base_probe = <type>;` directly
on the thread-local C++ parser. Normalization runs before
`parse_source_with_lang` borrows that parser, and the helper releases its
borrow before returning; calling normalization while holding the parser would
panic. The probe is syntax-only and never enters the IR.

## IDL-generated interfaces

OpenHarmony services and HDF HDI interfaces declare their IPC interfaces in
`.idl` files; the build runs idl-tool or hdi-gen to generate the interface
header, the proxy and the stub, and
none of the three is in the source tree. For every `.idl` file under the
analysis root or a dependency root, indexing synthesizes the declarations
for the supported C++ IPC dialect and serves them from memory at
`<root>/.trace-idl-generated/<header>`. That directory is a dependency root
([Dependency roots](USAGE.md#dependency-roots)): the headers resolve `#include` like
any other, carry `is_dep = 1`, and contribute declarations only. The headers
of an IDL file in the test partition
([Declaring-header eligibility](ANALYSIS.md#declaring-header-eligibility)) are served
one directory down, named after the file's test directory
(`.trace-idl-generated/test/<header>`), so they stay in the partition:
test code includes them and production code does not. A test directory is
one below the analysis root or below the dependency root the file is under,
so `<dep>/mock/IAtm.idl` is test IDL as `<root>/mock/IAtm.idl` is. The
include guard of a header names its directory with its file
(`TRACE_IDL_IATM_H`, `TRACE_IDL_TEST_IATM_H`): a unit that includes a
production header and the header of that name in the partition gets both.

The service dialect uses the following declarations; HDI's bases and package
mapping are described below.

| IDL | Synthesized |
|-----|-------------|
| `interface OHOS.Security.IAtm` (or `package OHOS.Security;` + `interface IAtm`) | `namespace OHOS { namespace Security { … } }` |
| interface `IAtm` | `iatm.h`: `class IAtm : public OHOS::IRemoteBroker` with one pure-virtual method per IDL method |
| `interface IFoo extends IBase` | `class IFoo : public a::IBase` (a bare base name is in the file's package); `ifoo.h` includes `ibase.h` when that header is synthesized too or is on disk |
| proxy | `atm_proxy.h`: `class AtmProxy : public IRemoteProxy<IAtm>`, every method `override`: the interface's own, then those it inherits through `extends`, nearest base first and each name once, a nearer base's declaration standing for those further up, as far as the bases are declared in the tree's IDL files. A production interface inherits from production IDL only; one in the test partition from its own test directory's IDL, then from production IDL |
| stub | `atm_stub.h`: `class AtmStub : public IRemoteStub<IAtm>` declaring `OnRemoteRequest` |
| descriptor | the dotted name as written, `OHOS.Security.IAtm`, or what the file's `interface_token a.b.c;` declares |

In service IDL, a leading `I` followed by an uppercase letter is stripped
to form the class base name (`IAtm` → `Atm`); HDI strips any leading `I`
(`Input` → `nputProxy` / `nputStub`, while service IDL keeps
`InputProxy` / `InputStub`). A file is named after its class in lower case,
with an underscore before every capital past the second
character (the generators' `CodeEmitter::FileName`):
`IAccessTokenManager` → `iaccess_token_manager.h`,
`AccessTokenManagerProxy` → `access_token_manager_proxy.h`, and a run of
capitals splits at each one (`IHDIFoo` → `ih_d_i_foo.h`). A name without
the `I` prefix is its own base (`El5FilekeyManagerInterface` →
`el5_filekey_manager_interface.h`, `El5FilekeyManagerInterfaceProxy`).

Every method returns `int32_t` status. In the service dialect (idl-tool's
`ErrCode`), a non-`void` IDL return becomes a trailing `T& funcResult`
parameter. HDI's omitted-return syntax, `Ping([in] String sendMsg,
[out] String recvMsg);`, has no result parameter: the generated signature is
`int32_t Ping(const std::string& sendMsg, std::string& recvMsg)`.
An omitted-return method selects HDI naming and bases for its file;
explicit-return methods retain their result mapping, including in a file
that also contains omitted-return methods. Parameters:

| IDL | `[in]` | `[out]` / `[inout]` |
|-----|--------|---------------------|
| `boolean byte short int long float double char`, `unsigned …`, an IDL `enum`, `FileDescriptor` | `T name` | `T& name` |
| `String` → `std::string`, `List<T>` / `T[]` → `std::vector<T>`, `Map<K,V>` → `std::unordered_map<K, V>` (service) / `std::map<K, V>` (HDI) | `const T& name` | `T& name` |
| `sequenceable a.b.S` (service) → `a::b::S` | `const T& name` | `T& name` |
| `sequenceable a.b.S` (HDI) → `sptr<S>` with the qualified C++ name | `const sptr<S>& name` | `sptr<S>& name` |
| `IRemoteObject`, an IDL `interface` → `sptr<…>` | `const sptr<…>& name` | `sptr<…>& name` |
| HDI `struct FooInfo` → `FooInfo` | `const FooInfo& name` | `FooInfo& name` |

HDI C++ IPC declarations follow the pinned HDF hdi-gen sources
([interface/status](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/codegen/cpp_interface_code_emitter.cpp),
[package names](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/codegen/cpp_code_emitter.cpp),
[class names](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/codegen/code_emitter.cpp),
[sequenceables](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/ast/ast_sequenceable_type.cpp),
[proxy](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/codegen/cpp_client_proxy_code_emitter.cpp),
[stub](https://github.com/openharmony/drivers_hdf_core/blob/cdc75a20bb8f1a046cd22e189405a20d602d0521/framework/tools/hdi-gen/codegen/cpp_service_stub_code_emitter.cpp)).
The default root `ohos.hdi` becomes `OHOS::HDI`; package segments before the
version use PascalCase (`foo_bar` → `FooBar`), `v1_0` becomes `V1_0`, and
segments after the version keep their spelling. Interface and type names
keep their spelling. Thus `ohos.hdi.foo.v1_0.IFoo` names
`OHOS::HDI::Foo::V1_0::IFoo`; the descriptor keeps the dotted IDL spelling.
Header/class base names follow the table above, with these HDI bases:
`IFoo : public OHOS::HDI::HdiBase`,
`FooProxy : public IProxyBroker<IFoo>`, and
`FooStub : public OHOS::IPCObjectStub`. The stub declares the same
`OnRemoteRequest` signature as the service dialect. The stub composes an
implementation of `IFoo` rather than inheriting it: IPC pairing uses the interface named by
`Program::idl_interfaces` (its `composed_stub` fact) to find concrete service
overrides. Every proxy named by IDL, detected as an IPC sender, or identified
by an indexed `IProxyBroker<T>` / `IRemoteProxy<T>` base, and every subclass
of those proxies, is excluded from this search, including proxies for
descendant interfaces. Broker ancestry establishes that role even if a
header collision drops the proxy's IDL record and its method delegates
`SendRequest` to a free helper. This role evidence does not by itself mark
the proxy method as an IPC sender. If no service implementation remains, pairing
falls back to interface declarations. The recorded interface also enters
the compatibility check above, so a same-named client of a declared unrelated
interface does not pair with the composed stub. It adds no inheritance edge
between the stub and interface.

Primitive spellings: `boolean` → `bool`, `byte` → `int8_t`, `short` →
`int16_t`, `int` → `int32_t`, `long` → `int64_t`, `unsigned char/short/int/long`
→ `uint8_t/uint16_t/uint32_t/uint64_t`, `FileDescriptor` → `int`. The C++
spellings `bool`, `int8_t` … `int64_t` and `uint8_t` … `uint64_t`, which some
IDL files use directly, map to themselves and are passed by value too.

The synthesized headers include nothing but each other: framework bases
(`IRemoteBroker`, `IRemoteProxy`, `IRemoteStub`, `HdiBase`, `IProxyBroker`,
`IPCObjectStub`), `sptr` and the standard types stay whatever the tree
declares. Where it declares `IRemoteBroker`, a call such as
`proxy_->AsObject()` binds to the broker's member. When the IPC framework
is not in the tree the undefined-template base
rule ([Template-parameter bases](#template-parameter-bases)) makes the proxy
and, in the service dialect, the stub subclasses of the interface. A call
through `sptr<IAtm>` reaches `AtmProxy`. That edge has resolution `external`,
as every edge to a function declared without a body has: the proxy's body is
generated, not in the tree.

Precedence and failures:

- A header already on disk with the same file name wins; that one header is
  not synthesized. On disk means among the headers of the analysis root and
  the dependency roots, or in a directory named by `--include`. For a
  production interface a header in the test partition does not count: a
  production includer cannot take a mock `iatm.h`, so the generated one is
  still synthesized. Whether a header is in the partition is decided as
  include resolution decides it, relative to the analysis root. This is
  deliberately not the rule for IDL files, which have a test directory below
  a dependency root as well: include resolution finds `<dep>/mock/iatm.h`
  for a production includer, so that header counts as on disk for a
  production interface and stands in for the generated one, while
  `<dep>/mock/IAtm.idl` renders where production code does not look. The
  asymmetry ends when the partition reaches dependency roots in include
  resolution and symbol lookup.
- Production IDL files are processed before those of the test partition,
  each group in sorted path order; when two render the same header into the
  same directory the first keeps it and a warning names both. An interface that
  lost its proxy or stub header this way, or whose proxy or stub class an
  earlier interface already names, records no fact on
  `Program::idl_interfaces`: a header of that pair belongs to the earlier
  interface. Any on-disk interface/proxy headers are still indexed. One that
  lost its interface header (`IFoo` and `Ifoo` are
  both `ifoo.h`) is not declared at all: its proxy and stub are not rendered
  either, since they would include a header declaring another class, and an
  interface extending it includes nothing for it and inherits nothing from
  it.
- HDI synthesis currently uses the flat service-IDL header layout rather
  than hdi-gen's package-relative paths. Normal versioned packages such as
  `foo/v1_0/IFoo.idl` and `foo/v1_1/IFoo.idl` therefore collide within the
  same generated partition. Without on-disk replacements, only the first
  version in sorted path order is synthesized; later versions contribute
  no interface, proxy or stub declarations or IDL pairing record. An include
  such as `foo/v1_1/foo_proxy.h` can resolve by basename to the first version's
  header without a missing-include diagnostic when that is the only available
  match. Synthesizing multiple package versions together requires a future
  package-relative header layout.
- A file that does not parse is skipped with one warning
  (`stage = idl`, `<path>:<line>: <message>`). `enum`, `struct`, `union`,
  `import`, `option_*` declarations (`option_stub_hooks on;`) and
  interface-level attributes are skipped; `sequenceable`, `rawdata` and
  forward `interface` declarations only inform the parameter mapping
  (a `rawdata` type maps as a `sequenceable` does). Each of the three may
  name the header declaring the type before `..`
  (`interface CallbackHeader..a.ICallback;`); the name is what follows it,
  and a `.` right after the separator (`interface IncludeDir...ICallback;`)
  puts it in the global namespace rather than the file's package.
  An attribute's value is read only when it is a number (`ipccode 1`); any
  other (`customMsgOption flags=MessageOption::TF_IMAGE`) is skipped,
  brackets of its own included. An `ipccode` past 32 bits is not recorded.
- A directory `<root>/.trace-idl-generated` that exists on disk is reported
  with one warning when headers are synthesized: it becomes a dependency
  root with whatever it holds, and its headers are indexed as those of any
  dependency root are.
- A type nested more than 64 levels deep (generic arguments and `[]`
  suffixes together) is a syntax error.
- An interface that `extends` itself is reported with one warning and
  synthesized without the clause.
- HDI support covers omitted-return methods, `struct`-prefixed parameter
  types, and the default C++ IPC naming convention above. Synthesis does not
  interpret hdi-gen command-line options: custom package-root mappings,
  C/Java output, lite/kernel/passthrough modes, and system-level attribute
  filtering are not modeled. Empty interfaces with no omitted-return method
  use the service dialect. Aggregate layouts and enum values are skipped;
  `enum E` / `union U` parameter prefixes, fixed-size IDL array declarators,
  and nested type declarations inside interfaces are unsupported and cause a
  parse warning. HDI-specific types such as `Ashmem`, `NativeBuffer`,
  `Pointer` and `SharedMemQueue` have no dedicated C++ mapping. Generated
  convenience methods (`Get`, `GetVersion`, `IsProxy`, `GetDesc`), stub
  marshalling helpers and their bodies are not synthesized. Attributes are
  consumed without dialect validation; `[inout]` retains the service
  dialect's mutable-reference approximation (hdi-gen accepts `[in]`/`[out]`).
- Each file is rendered on its own. A type named only through `import`
  (a sequenceable or enum declared in another `.idl`, such as
  `IdlCommon.idl`) is not qualified: it is emitted as written
  (`HapInfoParcel`, not `OHOS::Security::AccessToken::HapInfoParcel`), and an
  imported enum is passed as `const E&`. Only parameter spellings change;
  method names, classes and bases, which pairing and dispatch use, do not.

Each interface's names and methods (with `ipccode`) are recorded once on
`Program::idl_interfaces`; IPC pairing reads them from there
([OpenHarmony IPC bridges](MODELS.md#openharmony-ipc-bridges)).
