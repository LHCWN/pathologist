use super::*;
use std::ops::RangeInclusive;
use std::sync::{mpsc, Arc, OnceLock};
use trace_ir::{
    ContainerModel, ContainerOperation, ContainerOwner, ContainerTemplateDefault,
    ContainerTemplateDefaults, TypeId,
};

fn accepts(program: &Program, id: FnId, class: &str, method: &str, arity: usize) -> bool {
    let f = program.symbols.function(id);
    !f.is_static_member
        && f.name.rsplit_once("::") == Some((class, method))
        && method_explicit_arity(program, id).map_or(!f.pack_positions.is_empty(), |fixed| {
            arity_takes(f, fixed, arity)
        })
}

fn declared(program: &Program, class: &str, method: &str, arity: usize) -> bool {
    class_member_declarations(program, class, method)
        .into_iter()
        .any(|id| accepts(program, id, class, method, arity))
}

fn specialized_owners<'a>(
    program: &'a Program,
    models: &'a ContainerModels,
) -> impl Iterator<Item = (&'a str, &'a ContainerModel)> + 'a {
    program
        .types
        .container_specialization_owners()
        .filter_map(|owner| {
            models
                .get(&receiver_lookup_name(owner))
                .map(|model| (owner, model))
        })
}

pub(super) fn record_header_members(program: &mut Program, models: &ContainerModels) {
    let indexed = &*program;
    let ordinary = models.iter().flat_map(|model| {
        std::iter::once(&model.name)
            .chain(model.declaration_aliases())
            .map(move |class| (class.as_str(), model))
    });
    let members: Vec<_> = ordinary
        .chain(specialized_owners(indexed, models))
        .filter(|(owner, _)| indexed.types.is_struct_defined(owner))
        .flat_map(|(owner, model)| {
            model
                .operations
                .iter()
                .filter(move |op| declared(indexed, owner, op.method(), op.arity()))
                .map(move |op| (owner.to_owned(), op.method().to_owned(), op.arity()))
        })
        .collect();
    for (class, method, arity) in members {
        program.types.note_container_member(class, method, arity);
    }
}

fn imported_member(
    program: &Program,
    ctx: &LowerContext,
    class: &str,
    method: &str,
    arity: usize,
) -> bool {
    program.types.has_container_member(class, method, arity)
        || !program.types.is_struct_defined(class) && canonical_member(ctx, class, method, arity)
}

fn available(
    program: &Program,
    ctx: &LowerContext,
    class: &str,
    method: &str,
    arity: usize,
) -> bool {
    imported_member(program, ctx, class, method, arity)
        || program.types.is_struct_defined(class) && declared(program, class, method, arity)
}

fn is_initializer_list(desc: &TypeDesc) -> bool {
    class_spelling_of_desc(desc).is_some_and(|name| {
        matches!(
            receiver_lookup_name(name).as_ref(),
            "std::initializer_list" | "std::__1::initializer_list"
        )
    })
}

struct ListConstructor {
    arities: RangeInclusive<usize>,
    value_initialized_allocator: bool,
}

impl ListConstructor {
    fn accepts(&self, argc: usize, value_initialized_allocator: bool) -> bool {
        self.arities.contains(&argc)
            && (!value_initialized_allocator || self.value_initialized_allocator)
    }
}

fn value_initializes_allocator(
    signature: &trace_ir::ConstructorSignature,
    model: &ContainerModel,
) -> bool {
    let Some(parameter) = signature.parameters.get(1) else {
        return false;
    };
    if signature
        .reference_forms
        .get(1)
        .and_then(Option::as_ref)
        .is_some_and(|reference| !reference.rvalue && reference.is_const == Some(false))
    {
        return false;
    }
    let parameter = if signature.references.get(1) == Some(&true) {
        parameter.pointee().unwrap_or(parameter)
    } else {
        parameter.as_ref()
    };
    match parameter {
        TypeDesc::Void | TypeDesc::Ellipsis | TypeDesc::Array { .. } => false,
        // A declared dependent allocator can be unresolved in its primary
        // template. The model identifies the slot bound by the instance.
        TypeDesc::Unknown => model.initializer_allocator.is_some(),
        _ => true,
    }
}

fn list_constructors<'a>(
    program: &'a Program,
    class: &'a str,
    model: &'a ContainerModel,
) -> impl Iterator<Item = ListConstructor> + 'a {
    // Standalone prototypes can omit parameter types in the function table.
    // Constructor metadata also survives types-only header imports.
    let arities = |fixed: usize, defaults: u32, variadic: bool| {
        fixed.saturating_sub(defaults as usize)..=if variadic { usize::MAX } else { fixed }
    };
    let signatures = program
        .types
        .constructor_signatures(class)
        .iter()
        .filter(move |signature| {
            signature
                .parameters
                .first()
                .is_some_and(|desc| is_initializer_list(desc))
        })
        .map(move |signature| ListConstructor {
            arities: arities(
                signature.parameters.len(),
                signature.default_args,
                signature.variadic,
            ),
            value_initialized_allocator: value_initializes_allocator(signature, model),
        });
    let symbols = std::iter::once_with(move || {
        declared_members_upward(program, class, &trace_ir::MethodKind::Ctor)
    })
    .flatten()
    .filter_map(move |id| {
        let f = program.symbols.function(id);
        let (owner, _) = f.name.rsplit_once("::")?;
        let first = program.symbols.explicit_params(f)?.get(0)?;
        if owner != class || !is_initializer_list(program.types.get(first).desc.as_ref()) {
            return None;
        }
        Some(ListConstructor {
            arities: arities(
                method_explicit_arity(program, id)?,
                f.default_args,
                f.variadic,
            ),
            value_initialized_allocator: constructor_signature(program, id)
                .is_some_and(|signature| value_initializes_allocator(&signature, model)),
        })
    });
    signatures.chain(symbols)
}

fn list_constructor_accepts(
    program: &Program,
    ctx: &LowerContext,
    owner: &str,
    model: &ContainerModel,
    arity: usize,
    value_initialized_allocator: bool,
) -> bool {
    if program.types.is_struct_defined(owner) {
        list_constructors(program, owner, model)
            .any(|ctor| ctor.accepts(arity, value_initialized_allocator))
    } else {
        canonical_declarations(ctx)
            .and_then(|decls| decls.list_constructors.get(owner))
            .is_some_and(|ctors| {
                ctors
                    .iter()
                    .any(|ctor| ctor.accepts(arity, value_initialized_allocator))
            })
    }
}

/// Whether an inner clause can call an ordinary constructor (for example the
/// count/value constructor of a vector) even though its contents are not
/// modeled as container elements.
fn ordinary_constructor_accepts(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    owner: &str,
    model: &ContainerModel,
    node: Node,
) -> bool {
    let argc = if node.cached_kind() == "initializer_list" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|child| child.cached_kind() != "comment")
            .count()
    } else {
        arguments(node).map_or(0, |args| args.len())
    };
    // `{}` value-initializes any modeled container through its default
    // constructor, not just a sequence container and not necessarily through
    // `std::initializer_list`.
    if argc == 0 {
        return true;
    }
    // Count/value and size constructors are only relevant for sequence
    // containers; treating any 2-argument constructor as an ordinary match
    // would accept map compare/iterator constructors too.
    if !matches!(
        canonical_std_class(&model.name).as_str(),
        "std::vector" | "std::deque" | "std::list" | "std::forward_list"
    ) {
        return false;
    }
    let first = if node.cached_kind() == "initializer_list" {
        let mut cursor = node.walk();
        let first = node
            .named_children(&mut cursor)
            .find(|child| child.cached_kind() != "comment");
        first
    } else {
        arguments(node).and_then(|args| args.first().copied())
    };
    let Some(first) = first.map(peel_expression) else {
        return false;
    };
    if !integer_offset_type(
        program,
        ctx,
        &arithmetic_operand_desc(program, ctx, source, first, true),
    ) {
        return false;
    }
    let declared: Vec<_> = declared_members_upward(program, owner, &trace_ir::MethodKind::Ctor)
        .into_iter()
        .filter(|&id| {
            method_explicit_arity(program, id)
                .is_some_and(|arity| arity_takes(program.symbols.function(id), arity, argc))
        })
        .collect();
    let signatures = program.types.constructor_signatures(owner);
    let accepts_signature = |signature: &trace_ir::ConstructorSignature| {
        let fixed = signature.parameters.len();
        let minimum = fixed.saturating_sub(signature.default_args as usize);
        argc >= minimum
            && (signature.variadic || argc <= fixed)
            && signature.parameters.first().is_some_and(|param| {
                let desc = if signature.references.first() == Some(&true) {
                    param.pointee().unwrap_or(param)
                } else {
                    param.as_ref()
                };
                integer_offset_type(program, ctx, desc)
            })
    };
    if declared.iter().any(|&id| {
        program
            .symbols
            .explicit_params(program.symbols.function(id))
            .and_then(|params| params.get(0))
            .is_some_and(|ty| {
                integer_offset_type(program, ctx, program.types.get(ty).desc.as_ref())
            })
    }) || signatures.iter().any(accepts_signature)
    {
        return true;
    }
    // Canonical-prelude standard containers have no local body. Their indexed
    // signatures are unavailable here, but the sequence count constructor is
    // part of the modeled API, so keep the clause viable.
    !program.types.is_struct_defined(owner)
        && argc <= 3
        && signatures.is_empty()
        && declared_members_upward(program, owner, &trace_ir::MethodKind::Ctor).is_empty()
}

pub(super) struct BracedConstructors {
    pub targets: Vec<FnId>,
    pub wrap_list: bool,
    pub ordinary_alternatives: Vec<FnId>,
}

pub(super) fn braced_constructor_targets(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    var: VarId,
    init: Node,
) -> Option<BracedConstructors> {
    let elements = Elements::for_var(program, ctx, var)?;
    let class = elements.class();
    let model = ctx.container_models.get(class)?;
    if !model.initializer_list || copies_container(program, ctx, source, init, &elements.instance) {
        return None;
    }
    let owner = member_lookup_owner(program, ctx, &elements.instance);
    let (mut lists, mut ordinary): (Vec<_>, Vec<_>) =
        declared_members_upward(program, &owner, &trace_ir::MethodKind::Ctor)
            .into_iter()
            .partition(|&id| {
                constructor_signature(program, id)
                    .and_then(|sig| sig.parameters.into_iter().next())
                    .is_some_and(|param| is_initializer_list(&param))
            });
    if lists.is_empty() {
        return None;
    }
    let accepts = |id, argc, value_initialized_allocator: bool| {
        takes_initializer_arguments(program, id, argc)
            && (!value_initialized_allocator
                || constructor_signature(program, id)
                    .is_some_and(|signature| value_initializes_allocator(&signature, model)))
    };
    let Some(list) = list_initialization(
        program,
        ctx,
        source,
        &elements,
        model,
        init,
        |argc, value_initialized_allocator| {
            lists
                .iter()
                .any(|&id| accepts(id, argc, value_initialized_allocator))
        },
    ) else {
        if single_argument(init).is_some_and(|value| {
            integer_offset_type(
                program,
                ctx,
                &arithmetic_operand_desc(program, ctx, source, value, true),
            )
        }) {
            // An indexed count constructor's standard conversion outranks
            // converting the integer to an allocator or container object.
            let takes_count = |id| {
                takes_initializer_arguments(program, id, 1)
                    && constructor_signature(program, id).is_some_and(|signature| {
                        signature
                            .parameters
                            .first()
                            .is_some_and(|desc| integer_offset_type(program, ctx, desc))
                    })
            };
            if ordinary.iter().copied().any(takes_count) {
                ordinary.retain(|&id| takes_count(id));
            }
        }
        return Some(BracedConstructors {
            targets: ordinary,
            wrap_list: false,
            ordinary_alternatives: Vec::new(),
        });
    };
    lists.retain(|&id| accepts(id, list.argc, list.value_initialized_allocator));
    Some(BracedConstructors {
        targets: lists,
        wrap_list: list.values == init,
        ordinary_alternatives: if list.uncertain { ordinary } else { Vec::new() },
    })
}

fn takes_initializer_arguments(program: &Program, id: FnId, argc: usize) -> bool {
    method_explicit_arity(program, id)
        .is_some_and(|arity| arity_takes(program.symbols.function(id), arity, argc))
}

struct ListInitialization<'t> {
    values: Node<'t>,
    argc: usize,
    value_initialized_allocator: bool,
    uncertain: bool,
}

fn list_initialization<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    elements: &Elements,
    model: &ContainerModel,
    init: Node<'t>,
    accepts: impl Fn(usize, bool) -> bool,
) -> Option<ListInitialization<'t>> {
    let key = model
        .initializer_entry
        .and_then(|_| elements.map_key())
        .map(|key| canonical_alias_spelling(program, ctx, key));
    let viable = |values, argc, value_initialized_allocator, allow_empty_entry| {
        (accepts(argc, value_initialized_allocator)
            && !copies_container(program, ctx, source, values, &elements.instance))
        .then_some(())?;
        let uncertain = list_element_conversions(
            program,
            ctx,
            source,
            values,
            model.initializer_entry,
            key.as_deref(),
            &elements.element,
            allow_empty_entry,
        )?;
        Some(ListInitialization {
            values,
            argc,
            value_initialized_allocator,
            uncertain,
        })
    };
    match init.cached_kind() {
        "initializer_list" => {
            if let Some((values, allocator)) = braced_allocator_arguments(init) {
                if allocator.cached_kind() == "initializer_list"
                    && !allocator
                        .named_children(&mut allocator.walk())
                        .any(|child| child.cached_kind() != "comment")
                {
                    // A trailing empty brace is an allocator only when the
                    // first clause is itself a whole list of entries. When it
                    // is a single entry, C++ reads the whole list as two
                    // entries, the second default-constructed.
                    let entry_list = viable(values, 2, true, true);
                    if entry_list.is_some() {
                        return viable(init, 1, false, false).or(entry_list);
                    }
                    return viable(init, 1, false, true);
                }
                if allocator_argument(program, ctx, source, elements, model, allocator) {
                    return viable(values, 2, false, true).or_else(|| viable(init, 1, false, true));
                }
            }
            viable(init, 1, false, true)
        }
        "argument_list" => {
            let args = arguments(init)?;
            let values = *args.first()?;
            (values.cached_kind() == "initializer_list").then_some(())?;
            viable(values, args.len(), false, true)
        }
        _ => None,
    }
}

fn braced_allocator_arguments(init: Node) -> Option<(Node, Node)> {
    let mut cursor = init.walk();
    let mut args = init
        .named_children(&mut cursor)
        .filter(|arg| arg.cached_kind() != "comment");
    let values = args.next()?;
    (values.cached_kind() == "initializer_list").then_some(())?;
    let allocator = args.next()?;
    (args.next().is_none()).then_some((values, allocator))
}

fn allocator_argument(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    elements: &Elements,
    model: &ContainerModel,
    arg: Node,
) -> bool {
    let arg = peel_expression(arg);
    let spelling = if arg.cached_kind() == "compound_literal_expression" {
        arg.cached_field("type")
            .and_then(|ty| type_spelling_node(program, ctx, source, ty))
    } else {
        match known_arg_type(program, ctx, source, arg) {
            TypeDesc::Struct { name, .. } => Some(name),
            _ => None,
        }
    };
    let Some(spelling) = spelling else {
        return false;
    };
    let matches_instantiation = model.initializer_allocator.is_some_and(|position| {
        trace_ir::split_instance(&elements.instance).is_some_and(|(_, args)| {
            args.get(position).is_some_and(|arg| {
                canonical_specialization(program, ctx, arg)
                    == canonical_specialization(program, ctx, &spelling)
            })
        })
    });
    matches_instantiation || {
        let owner = member_lookup_owner(program, ctx, &elements.instance);
        program
            .types
            .constructor_signatures(&owner)
            .iter()
            .any(|signature| {
                signature
                    .parameters
                    .first()
                    .is_some_and(|param| is_initializer_list(param))
                    && signature.parameters.get(1).is_some_and(|param| {
                        let desc = if signature.references.get(1) == Some(&true) {
                            param.pointee().unwrap_or(param)
                        } else {
                            param.as_ref()
                        };
                        matches!(desc, TypeDesc::Struct { name, .. } if *name == spelling)
                    })
            })
    }
}

/// Canonical specialization spelling with recorded aliases expanded, so the
/// same C++ type compared on both sides of a copy reaches its descendant
/// cells even when the source and destination store different spellings.
fn canonical_specialization(program: &Program, ctx: &LowerContext, spelling: &str) -> String {
    canonical_specialization_at(program, ctx, spelling, 0, true)
}

/// Expand recorded aliases without collapsing the whitespace of an ordinary
/// scalar or pointer spelling (`const int`, `void (*)()`).
fn canonical_alias_spelling(program: &Program, ctx: &LowerContext, spelling: &str) -> String {
    canonical_alias_spelling_at(program, ctx, spelling.trim(), 0)
}

fn canonical_alias_spelling_at(
    program: &Program,
    ctx: &LowerContext,
    spelling: &str,
    depth: u32,
) -> String {
    // A cv-qualified key alias (`const CallbackKey`) still names the same
    // alias for key-shape classification.
    let spelling = without_leading_cv(spelling);
    if depth >= 64 {
        return spelling.to_owned();
    }
    if let Some(alias) = program.types.alias_type_spelling(spelling) {
        if alias.trim() != spelling {
            return canonical_alias_spelling_at(program, ctx, alias.trim(), depth + 1);
        }
    }
    if let Some(desc) = program.types.resolve_alias(spelling) {
        return canonical_desc_at(program, ctx, desc, depth + 1, false);
    }
    spelling.to_owned()
}

/// Long specialization identities are replaced by a stable digest. This
/// bounds an alias chain that would otherwise duplicate the previous spelling
/// into both the element and default-allocator slots at every level. The
/// digest is an internal identity key, not a display spelling.
const COMPACT_IDENTITY_THRESHOLD: usize = 4_096;

/// A leaf spelling has no template arguments to walk. Collapse spacing the
/// same way registered specialization keys do, and treat `void (*)()` and
/// `void (*)(void)` as the same function-pointer type.
fn normalize_leaf_spelling(spelling: &str) -> String {
    normalize_spacing(spelling).replace("(void)", "()")
}

fn compact_identity(spelling: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in spelling.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("#{hash:016x}")
}

fn maybe_compact_identity(spelling: String) -> String {
    if spelling.len() > COMPACT_IDENTITY_THRESHOLD {
        compact_identity(&spelling)
    } else {
        spelling
    }
}

fn canonical_specialization_at(
    program: &Program,
    ctx: &LowerContext,
    spelling: &str,
    depth: u32,
    strip_cv: bool,
) -> String {
    // Leading cv-qualification does not change the outer container's element
    // identity, but it does distinguish template arguments such as
    // `std::vector<const Handler *>` from `std::vector<Handler *>`.
    let spelling = if strip_cv {
        without_leading_cv(spelling.trim()).trim()
    } else {
        spelling.trim()
    };
    if depth >= 64 {
        return normalize_leaf_spelling(spelling);
    }
    if let Some(alias) = program.types.alias_type_spelling(spelling) {
        if alias.trim() != spelling {
            return canonical_specialization_at(program, ctx, alias, depth + 1, strip_cv);
        }
    }
    if let Some(desc) = program.types.resolve_alias(spelling) {
        return canonical_desc_at(program, ctx, desc, depth + 1, strip_cv);
    }
    let Some((class, args)) = trace_ir::split_instance(spelling) else {
        return normalize_leaf_spelling(spelling);
    };
    let canonical_class = canonical_std_class(class);
    let owner = ctx
        .container_models
        .get(&canonical_class)
        .and_then(|model| {
            indexed_owner(program, model).or_else(|| {
                canonical_declarations(ctx)?
                    .owners
                    .get(&model.name)
                    .map(String::as_str)
            })
        })
        .unwrap_or(canonical_class.as_str());
    let mut args: Vec<String> = args
        .iter()
        .map(|arg| {
            maybe_compact_identity(canonical_specialization_at(
                program,
                ctx,
                arg,
                depth + 1,
                false,
            ))
        })
        .collect();
    // Fill template defaults that the written spelling omitted, so an
    // explicit default allocator and its omission share one identity. The
    // canonical prelude carries defaults when the declaration is absent here.
    if let Some(defaults) = class_template_defaults(program, ctx, class) {
        while let Some(value) = defaults
            .get(&args.len())
            .and_then(|default| default.bind(&args))
        {
            args.push(maybe_compact_identity(canonical_specialization_at(
                program,
                ctx,
                &value,
                depth + 1,
                false,
            )));
        }
    }
    maybe_compact_identity(format!("{owner}<{}>", args.join(",")))
}

fn canonical_desc_at(
    program: &Program,
    ctx: &LowerContext,
    desc: &TypeDesc,
    depth: u32,
    strip_cv: bool,
) -> String {
    if depth >= 64 {
        return canonical_scalar(desc).unwrap_or_else(|| format!("{desc:?}").replace(' ', ""));
    }
    match desc {
        TypeDesc::Struct { name, .. } | TypeDesc::Union { name, .. } => {
            canonical_specialization_at(program, ctx, name, depth + 1, strip_cv)
        }
        TypeDesc::Ptr(inner) => match inner.as_ref() {
            TypeDesc::FnPtr { ret, params } => {
                canonical_fn_ptr_at(program, ctx, ret, params, depth + 1)
            }
            _ => format!(
                "{}*",
                canonical_desc_at(program, ctx, inner, depth + 1, false)
            ),
        },
        TypeDesc::Array { elem, .. } => {
            format!(
                "{}[]",
                canonical_desc_at(program, ctx, elem, depth + 1, false)
            )
        }
        TypeDesc::FnPtr { ret, params } => {
            canonical_fn_ptr_at(program, ctx, ret, params, depth + 1)
        }
        other => canonical_scalar(other).unwrap_or_else(|| format!("{other:?}").replace(' ', "")),
    }
}

fn canonical_scalar(desc: &TypeDesc) -> Option<String> {
    Some(
        match desc {
            TypeDesc::Void => "void",
            TypeDesc::Char => "char",
            TypeDesc::Bool => "bool",
            TypeDesc::Short => "short",
            TypeDesc::Int => "int",
            TypeDesc::Long => "long",
            TypeDesc::LongLong => "long long",
            TypeDesc::Float => "float",
            TypeDesc::Double => "double",
            TypeDesc::SizeT => "size_t",
            TypeDesc::Unknown => "unknown",
            TypeDesc::Ellipsis => "...",
            _ => return None,
        }
        .to_owned(),
    )
}

fn canonical_fn_ptr_at(
    program: &Program,
    ctx: &LowerContext,
    ret: &TypeDesc,
    params: &[TypeDesc],
    depth: u32,
) -> String {
    // `void (*)()` and `void (*)(void)` are the same type.
    let params: Vec<_> = if matches!(params, [TypeDesc::Void]) {
        Vec::new()
    } else {
        params
            .iter()
            .map(|param| canonical_desc_at(program, ctx, param, depth + 1, false))
            .collect()
    };
    format!(
        "{}(*)({})",
        canonical_desc_at(program, ctx, ret, depth + 1, false),
        params.join(",")
    )
}

/// A descriptor naming a modeled container, whose braced form must be
/// interpreted at the inner container level rather than as a scalar element.
fn modeled_container_desc(ctx: &LowerContext, desc: &TypeDesc) -> bool {
    let TypeDesc::Struct { name, .. } = desc else {
        return false;
    };
    ctx.container_models
        .get(
            name.split_once('<')
                .map_or(name.as_str(), |(class, _)| class),
        )
        .is_some()
}

/// A multi-clause braced key only fits an aggregate/pair key. A scalar,
/// pointer or function-pointer key takes a single braced element (or none).
fn key_braced_accepts(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    spelling: Option<&str>,
) -> bool {
    let Some(spelling) = spelling else {
        return true;
    };
    let node = peel_expression(node);
    let (bare, suffix) = trace_ir::split_pointer_suffix(spelling);
    let pointer_like = !suffix.trim().is_empty();
    let bare = bare.trim();
    // Only a specialization whose closing `>` ends the spelling has a class
    // base. A function-pointer key returning `Foo<int>` must keep its `(*`
    // declarator instead of being truncated at the return type's `<`.
    let base = trace_ir::split_instance(bare)
        .map_or(bare, |(base, _)| base)
        .trim();
    let base = without_leading_cv(base);
    let pair = canonical_std_class(base) == "std::pair";
    let enum_lookup = |name: &str| program.types.enum_is_scoped(name);
    let aggregate = !pointer_like
        && !is_fundamental_type_name(base)
        && !base.contains("(*")
        && !base.contains("(&")
        // An unscoped enum reads as an integer in the expression probe below.
        // Its enumerators still initialize an enum key without a constructor.
        && resolve_function_named_with_imports(program, ctx, base, enum_lookup, enum_lookup)
            .is_none();
    let mut cursor = node.walk();
    let clauses = node.cached_kind().eq("initializer_list").then(|| {
        node.named_children(&mut cursor)
            .filter(|clause| clause.cached_kind() != "comment")
            .count()
    });
    if pointer_like {
        // A pointer key takes one expression (including null); it cannot be
        // initialized from a multi-clause braced list.
        return clauses.is_none_or(|clauses| clauses <= 1);
    }
    let Some(clauses) = clauses else {
        if pair || aggregate {
            let argument = key_argument_desc(program, ctx, source, node);
            return key_type_may_convert(
                program,
                ctx,
                &argument,
                spelling,
                is_null_constant(source, node),
            );
        }
        return true;
    };
    if pair {
        match clauses {
            // `{}` value-initializes a default-constructible pair key.
            0 => true,
            2 => true,
            // A single clause copy-list-initializes the pair key from a
            // compatible same-type expression (`{key}`).
            1 => {
                let mut cursor = node.walk();
                let clause = node
                    .named_children(&mut cursor)
                    .find(|clause| clause.cached_kind() != "comment");
                clause.is_some_and(|clause| {
                    key_type_may_convert(
                        program,
                        ctx,
                        &key_argument_desc(program, ctx, source, clause),
                        spelling,
                        false,
                    )
                })
            }
            _ => false,
        }
    } else {
        clauses <= 1 || aggregate
    }
}

/// Whether a descriptor names the type `spelling`, including a primary-template
/// layout that kept its base name but carries the instantiated fields.
fn desc_matches_type_spelling(
    program: &Program,
    ctx: &LowerContext,
    desc: &TypeDesc,
    spelling: &str,
) -> bool {
    if class_spelling_of_desc(desc).is_some_and(|name| {
        canonical_specialization(program, ctx, name)
            == canonical_specialization(program, ctx, spelling)
    }) {
        return true;
    }
    let (TypeDesc::Struct { name, fields }, Some((class, args))) =
        (desc, trace_ir::split_instance(spelling))
    else {
        return false;
    };
    name == class
        && fields.len() == args.len()
        && fields.iter().zip(args).all(|((_, field), arg)| {
            canonical_desc_at(program, ctx, field, 0, false)
                == canonical_specialization_at(program, ctx, arg, 0, false)
        })
}

/// Preserve the pointer shape of a callable designator that the ordinary
/// expression type probe could leave unknown.
fn key_argument_desc(program: &Program, ctx: &LowerContext, source: &str, node: Node) -> TypeDesc {
    let node = peel_expression(node);
    let desc = receiver_desc(program, ctx, source, node)
        .unwrap_or_else(|| arithmetic_operand_desc(program, ctx, source, node, true));
    if matches!(desc, TypeDesc::Unknown)
        && (node.cached_kind() == "lambda_expression"
            || fn_designator(source, node)
                .is_some_and(|name| resolve_fn_ref(program, ctx, source, name).is_some()))
    {
        return TypeDesc::Ptr(Box::new(TypeDesc::Unknown));
    }
    desc
}

/// Whether an indexed key has a one-argument constructor whose parameter
/// could bind this argument. Both ordinary and types-only declarations count.
fn class_accepts_key_argument(
    program: &Program,
    ctx: &LowerContext,
    class: &str,
    arg: &TypeDesc,
    null_constant: bool,
) -> bool {
    let is_numeric = |desc: &TypeDesc| {
        integer_offset_type(program, ctx, desc)
            || matches!(desc, TypeDesc::Float | TypeDesc::Double)
    };
    let accepts = |param: &TypeDesc, reference| {
        let param = if reference {
            param.pointee().unwrap_or(param)
        } else {
            param
        };
        arg == param
            || matches!(param, TypeDesc::Unknown)
            || (is_numeric(arg) && is_numeric(param))
            || ((null_constant || arg.is_pointer_like() || matches!(arg, TypeDesc::Array { .. }))
                && param.is_pointer_like())
            || (arg.is_pointer_like() && matches!(param, TypeDesc::Bool))
            || (arg.is_class_like()
                && param.is_class_like()
                && (trace_ir::may_name_same_type(arg, param)
                    || class_conversion_possible(program, arg, param)))
    };
    let declared = declared_members_upward(program, class, &trace_ir::MethodKind::Ctor)
        .into_iter()
        .any(|id| {
            let function = program.symbols.function(id);
            method_explicit_arity(program, id).is_some_and(|arity| arity_takes(function, arity, 1))
                && program
                    .symbols
                    .explicit_params(function)
                    .and_then(|params| params.get(0))
                    .is_some_and(|ty| {
                        accepts(
                            program.types.get(ty).desc.as_ref(),
                            function.reference_params.first() == Some(&true),
                        )
                    })
        });
    declared
        || program
            .types
            .constructor_signatures(class)
            .iter()
            .any(|signature| {
                signature.parameters.first().is_some_and(|param| {
                    accepts(param, signature.references.first() == Some(&true))
                }) && signature
                    .parameters
                    .len()
                    .saturating_sub(signature.default_args as usize)
                    <= 1
            })
}

fn without_leading_cv(mut spelling: &str) -> &str {
    loop {
        let trimmed = spelling.trim_start();
        let rest = trimmed
            .strip_prefix("const")
            .or_else(|| trimmed.strip_prefix("volatile"));
        match rest {
            Some(rest) if rest.chars().next().is_some_and(char::is_whitespace) => spelling = rest,
            _ => return trimmed,
        }
    }
}

/// A conversion can make a list viable even when there is no source element
/// cell to copy. Class hierarchies can retain a template base's primary name.
fn inner_value_may_convert(
    program: &Program,
    ctx: &LowerContext,
    desc: &TypeDesc,
    expected: &str,
) -> bool {
    let TypeDesc::Struct { name, .. } = desc else {
        return matches!(desc, TypeDesc::Unknown);
    };
    let expected_identity = canonical_specialization(program, ctx, expected);
    canonical_specialization(program, ctx, name) == expected_identity
        || program
            .ancestor_closure(name)
            .into_iter()
            .skip(1)
            .any(|(base, _)| {
                canonical_specialization(program, ctx, base) == expected_identity
                    || base == receiver_lookup_name(expected).as_ref()
            })
        || class_may_convert(program, desc)
}

/// Whether a braced value can initialize a modeled inner container, including
/// its initializer-list or ordinary constructors and allocator clauses.
fn inner_container_viable(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    element: &TypeDesc,
    node: Node,
) -> Option<bool> {
    let TypeDesc::Struct { name, .. } = element else {
        return None;
    };
    let node = peel_expression(node);
    if !matches!(
        node.cached_kind(),
        "initializer_list" | "argument_list" | "compound_literal_expression"
    ) {
        // A declared return type is enough to identify a container value even
        // when lowering has no element cell for the expression; the cell is
        // required only to emit a copy.
        let has_cell = Elements::for_receiver(program, ctx, source, node).is_some_and(|from| {
            canonical_specialization(program, ctx, &from.instance)
                == canonical_specialization(program, ctx, name)
        });
        return Some(
            has_cell || {
                let desc = receiver_desc(program, ctx, source, node)
                    .unwrap_or_else(|| known_arg_type(program, ctx, source, node));
                let known_callable = matches!(desc, TypeDesc::Unknown)
                    && (node.cached_kind() == "lambda_expression"
                        || fn_designator(source, node).is_some_and(|designator| {
                            resolve_fn_ref(program, ctx, source, designator).is_some()
                        }));
                !known_callable && inner_value_may_convert(program, ctx, &desc, name)
            },
        );
    }
    // A single braced named container is a copy. `list_initialization`
    // deliberately rejects it so the list reading cannot swallow the source,
    // but the copy is still a viable inner value.
    if copies_container(program, ctx, source, node, name) {
        return Some(true);
    }
    let class = name
        .split_once('<')
        .map_or(name.as_str(), |(class, _)| class);
    let model = ctx.container_models.get(class)?;
    let synthetic = Elements::new(program, ctx, ContainerOwner::Variable(VarId(0)), element)?;
    if node.cached_kind() == "compound_literal_expression" {
        let ty = node.cached_field("type")?;
        let spelling = type_spelling_node(program, ctx, source, ty)?;
        let desc = TypeDesc::Struct {
            name: spelling,
            fields: Vec::new(),
        };
        if inner_value_may_convert(program, ctx, &desc, name) {
            return Some(true);
        }
        return Some(
            model.initializer_list
                && initializer_list_compound_brace(program, ctx, source, node, synthetic.element())
                    .is_some(),
        );
    }
    if allocator_extended_copy(program, ctx, source, &synthetic, node).is_some() {
        return Some(true);
    }
    let owner = member_lookup_owner(program, ctx, name);
    if model.initializer_list
        && list_initialization(
            program,
            ctx,
            source,
            &synthetic,
            model,
            node,
            |arity, value_initialized_allocator| {
                list_constructor_accepts(
                    program,
                    ctx,
                    &owner,
                    model,
                    arity,
                    value_initialized_allocator,
                )
            },
        )
        .is_some()
    {
        return Some(true);
    }
    Some(ordinary_constructor_accepts(
        program, ctx, source, &owner, model, node,
    ))
}

/// A viable entry may carry no modeled value, as with a default-initialized
/// entry or a call returning a pair.
enum PairEntry<'t> {
    Mapped(StoredValue<'t>),
    Unmodeled,
}

fn pair_clause_entry<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    clause: Node<'t>,
    entry: usize,
    key: Option<&str>,
    element: &TypeDesc,
) -> Option<PairEntry<'t>> {
    let clause = peel_expression(clause);
    if let Some(args) = make_pair_arguments(source, clause) {
        let node = *args.get(entry)?;
        if key.is_some() {
            let first = key_argument_desc(program, ctx, source, args[0]);
            if !pair_key_may_convert(program, ctx, &first, key) {
                return None;
            }
        }
        let viable = if modeled_container_desc(ctx, element) {
            inner_container_viable(program, ctx, source, element, node) == Some(true)
        } else {
            let ty = receiver_desc(program, ctx, source, node)
                .unwrap_or_else(|| known_arg_type(program, ctx, source, node));
            is_null_constant(source, node)
                || node.cached_kind() == "lambda_expression"
                || fn_designator(source, node)
                    .is_some_and(|name| resolve_fn_ref(program, ctx, source, name).is_some())
                || pair_member_matches_element(program, ctx, &ty, element)
        };
        return viable.then_some(PairEntry::Mapped(StoredValue::Expression(node)));
    }
    if entry == 1 && is_name(clause) {
        if let Some((var, pair)) = lookup_var_node(program, ctx, source, clause)
            .and_then(|var| Some((var, pair_type(program, var)?)))
        {
            if field_in_pair(program, pair, "first").is_some_and(|(_, ty)| {
                !pair_key_may_convert(program, ctx, program.types.get(ty).desc.as_ref(), key)
            }) {
                return None;
            }
            if let Some((field, ty)) = field_in_pair(program, pair, "second") {
                return pair_member_matches_element(
                    program,
                    ctx,
                    program.types.get(ty).desc.as_ref(),
                    element,
                )
                .then_some(PairEntry::Mapped(StoredValue::PairSecond(
                    clause, var, pair, field, ty,
                )));
            }
        }
    }
    pair_expression_matches_element(program, ctx, source, clause, key, element)
        .then_some(PairEntry::Unmodeled)
}

/// Whether a pair's mapped member descriptor fits the modeled element.
fn pair_member_matches_element(
    program: &Program,
    ctx: &LowerContext,
    desc: &TypeDesc,
    element: &TypeDesc,
) -> bool {
    if modeled_container_desc(ctx, element) {
        let TypeDesc::Struct { name: expected, .. } = element else {
            return false;
        };
        inner_value_may_convert(program, ctx, desc, expected)
    } else {
        matches!(desc, TypeDesc::Unknown)
            || trace_ir::may_name_same_type(desc, element)
            || (desc.is_pointer_like() && element.is_pointer_like())
            || (desc.is_class_like() && class_may_convert(program, desc))
    }
}

/// Whether an unmodeled expression is a `std::pair` whose mapped member fits
/// the modeled element, so it can stand as one map entry without a stored
/// value.
fn pair_expression_matches_element(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    clause: Node,
    key: Option<&str>,
    element: &TypeDesc,
) -> bool {
    let Some(desc) = receiver_desc(program, ctx, source, clause) else {
        return false;
    };
    // A class with a conversion operator (or an unresolved expression) may
    // still convert to the entry pair type. Keep it viable without emitting
    // flow for the value.
    if matches!(desc, TypeDesc::Unknown)
        || desc.is_class_like() && class_may_convert(program, &desc)
    {
        return true;
    }
    let TypeDesc::Struct { name, fields } = &desc else {
        return false;
    };
    let Some((class, args)) = trace_ir::split_instance(name) else {
        return false;
    };
    if !is_std_pair(class) || args.len() != 2 {
        return false;
    }
    // A call result can retain the written specialization alongside a
    // primary-template field layout. Use the written key in that case.
    if key.is_some()
        && !pair_key_may_convert(
            program,
            ctx,
            &key_component_desc(program, ctx, args[0]),
            key,
        )
    {
        return false;
    }
    if let Some((_, second)) = fields.iter().find(|(field, _)| field == "second") {
        return pair_member_matches_element(program, ctx, second, element);
    }
    // A pair whose mapped member is not pointer-like or modeled may not carry
    // a field layout; compare the written mapped template argument instead.
    canonical_specialization_at(program, ctx, args[1], 0, false)
        == canonical_desc_at(program, ctx, element, 0, false)
}

fn pair_key_may_convert(
    program: &Program,
    ctx: &LowerContext,
    first: &TypeDesc,
    key: Option<&str>,
) -> bool {
    key.is_none_or(|key| key_type_may_convert(program, ctx, first, key, false))
}

fn key_type_may_convert(
    program: &Program,
    ctx: &LowerContext,
    arg: &TypeDesc,
    key: &str,
    null_constant: bool,
) -> bool {
    key_type_may_convert_at(program, ctx, arg, key, null_constant, 0)
}

/// A pair-shaped key is itself a possible list entry only if its first
/// member can initialize the entire key. Check converting pairs by component
/// so a shallower pair cannot stand in for a nested pair key.
fn key_type_may_convert_at(
    program: &Program,
    ctx: &LowerContext,
    arg: &TypeDesc,
    key: &str,
    null_constant: bool,
    depth: u32,
) -> bool {
    if matches!(arg, TypeDesc::Unknown) || depth >= MAX_AST_WALK_DEPTH {
        return true;
    }
    let (base, suffix) = trace_ir::split_pointer_suffix(key);
    let base = without_leading_cv(trace_ir::strip_trailing_cv(base));
    let pointer_key = suffix.contains('*') || split_function_pointer_star(base).is_some();
    if pointer_key {
        return null_constant
            || arg.is_pointer_like()
            || matches!(arg, TypeDesc::Array { .. })
            || class_may_convert(program, arg);
    }
    if arg.is_class_like() && desc_matches_type_spelling(program, ctx, arg, base) {
        return true;
    }
    if let Some(expected) = pair_type_arguments(base) {
        let component_fits = |from: &TypeDesc, to| {
            let to = canonical_alias_spelling(program, ctx, to);
            key_type_may_convert_at(program, ctx, from, &to, false, depth + 1)
        };
        if let TypeDesc::Struct { name, fields } = arg {
            if let Some(actual) = pair_type_arguments(name) {
                return actual
                    .into_iter()
                    .zip(expected)
                    .all(|(from, to)| component_fits(&key_component_desc(program, ctx, from), to))
                    || class_may_convert(program, arg);
            }
            if is_std_pair(name) {
                // An instantiated pair with unmodeled members can keep its
                // primary name. Use any member types the index still has.
                return ["first", "second"]
                    .into_iter()
                    .zip(expected)
                    .all(|(member, to)| {
                        fields
                            .iter()
                            .find(|(name, _)| name == member)
                            .is_none_or(|(_, from)| component_fits(from, to))
                    });
            }
        }
        return class_may_convert(program, arg);
    }
    if is_fundamental_type_name(base) {
        return integer_offset_type(program, ctx, arg)
            || matches!(arg, TypeDesc::Float | TypeDesc::Double)
            || (base == "bool" && arg.is_pointer_like())
            || class_may_convert(program, arg);
    }
    if program.types.enum_is_scoped(base).is_some() {
        return class_may_convert(program, arg);
    }
    let owner = receiver_lookup_name(base);
    !program.types.is_struct_defined(&owner)
        || class_may_convert(program, arg)
        || matches!(arg, TypeDesc::Struct { name, .. }
            if program.derives_from(&receiver_lookup_name(name), &owner))
        || class_accepts_key_argument(program, ctx, &owner, arg, null_constant)
}

fn pair_type_arguments(spelling: &str) -> Option<[&str; 2]> {
    let (class, args) = trace_ir::split_instance(spelling)?;
    if !is_std_pair(class) {
        return None;
    }
    args.try_into().ok()
}

/// Pair descriptors can omit their field layouts. Recover enough of a written
/// component type to compare pointer, numeric, and structured key shapes.
fn key_component_desc(program: &Program, ctx: &LowerContext, spelling: &str) -> TypeDesc {
    let spelling = canonical_alias_spelling(program, ctx, spelling);
    let (bare, suffix) = trace_ir::split_pointer_suffix(&spelling);
    let bare = trace_ir::strip_trailing_cv(bare);
    if suffix.contains('*') || split_function_pointer_star(bare).is_some() || bare.ends_with(']') {
        TypeDesc::Ptr(Box::new(TypeDesc::Unknown))
    } else if is_fundamental_type_name(bare) {
        match bare {
            "auto" | "nullptr_t" => TypeDesc::Unknown,
            "void" => TypeDesc::Void,
            _ => primitive_scalar_desc(bare).unwrap_or(TypeDesc::Int),
        }
    } else {
        TypeDesc::Struct {
            name: bare.to_owned(),
            fields: Vec::new(),
        }
    }
}

/// A known constructor set without a default overload rules out `{}` as
/// an entry. An implicit or unindexed constructor leaves it viable.
fn map_entry_can_default(program: &Program, key: Option<&str>, mapped: &TypeDesc) -> bool {
    let can_default = |name: &str| {
        let owner = receiver_lookup_name(name);
        !program.types.has_declared_constructor(&owner)
            || program.types.default_constructor(&owner).is_some()
    };
    key.is_none_or(can_default)
        && match mapped {
            TypeDesc::Struct { name, .. } | TypeDesc::Union { name, .. } => can_default(name),
            _ => true,
        }
}

/// `None` for an incompatible clause, `Some(true)` for a conversion whose
/// viability is unknown, and `Some(false)` for a viable list.
#[allow(clippy::too_many_arguments)]
fn list_element_conversions(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    init: Node,
    entry: Option<usize>,
    key: Option<&str>,
    element: &TypeDesc,
    allow_empty_entry: bool,
) -> Option<bool> {
    // The modeled elements are pointers or callables. Arithmetic cannot
    // initialize one, except for a null pointer constant. Keep clauses of
    // uncertain type, including class conversions and callable expressions.
    let mut uncertain = false;
    let viable = init
        .named_children(&mut init.walk())
        .filter(|value| value.cached_kind() != "comment")
        .map(|value| {
            if entry.is_none() {
                return Some(Some(value));
            }
            let entry_node = compound_pair_brace(program, ctx, source, value).unwrap_or(value);
            if entry_node.cached_kind() != "initializer_list" {
                return pair_clause_entry(
                    program,
                    ctx,
                    source,
                    value,
                    entry.expect("map entry"),
                    key,
                    element,
                )
                .is_some()
                .then_some(None);
            }
            // A trailing empty allocator clause is classified separately.
            if braced_clause_count(entry_node) == Some(0) {
                return (allow_empty_entry && map_entry_can_default(program, key, element))
                    .then_some(None);
            }
            let mapped = braced_value(entry_node, entry)?;
            if let Some(key) = key {
                let key_node = braced_value(entry_node, Some(0))?;
                if !key_braced_accepts(program, ctx, source, key_node, Some(key)) {
                    return None;
                }
            }
            Some(Some(mapped))
        })
        .all(|mapped| {
            let Some(mapped) = mapped else {
                return false;
            };
            let Some(mut value) = mapped else {
                return true;
            };
            let root = value;
            // Modeled inner containers validate their own brace via their
            // initializer-list or ordinary constructors, whether it has one
            // clause or several.
            if modeled_container_desc(ctx, element) {
                if inner_container_viable(program, ctx, source, element, root) == Some(true) {
                    uncertain = true;
                    return true;
                }
                return false;
            }
            while value.cached_kind() == "initializer_list" {
                let mut cursor = value.walk();
                let mut clauses = value
                    .named_children(&mut cursor)
                    .filter(|clause| clause.cached_kind() != "comment");
                let Some(clause) = clauses.next() else {
                    return true;
                };
                if clauses.next().is_some() {
                    // The only modeled non-scalar element is std::function.
                    // Its multi-argument constructors before C++17 start
                    // with an allocator tag, which a callback cannot supply.
                    if matches!(element, TypeDesc::Ptr(_)) || ctx.cpp_standard >= 201703 {
                        return false;
                    }
                    let first = peel_expression(clause);
                    uncertain = true;
                    return first.cached_kind() != "lambda_expression"
                        && resolve_fn_ref(program, ctx, source, first).is_none()
                        && !known_arg_type(program, ctx, source, first).is_pointer_like();
                }
                value = clause;
            }
            let value = peel_expression(value);
            if is_null_constant(source, value)
                || value.cached_kind() == "lambda_expression"
                || fn_designator(source, value)
                    .is_some_and(|name| resolve_fn_ref(program, ctx, source, name).is_some())
            {
                return true;
            }
            // Reuse the bounded integral-expression probe for enumerators and
            // arithmetic, including the result type of a declared call.
            let ty = arithmetic_operand_desc(program, ctx, source, value, true);
            if integer_offset_type(program, ctx, &ty)
                || matches!(ty, TypeDesc::Float | TypeDesc::Double)
            {
                return false;
            }
            let decays_to_element = matches!((&ty, element),
                (TypeDesc::Array { elem, .. }, TypeDesc::Ptr(pointee))
                    if trace_ir::may_name_same_type(elem, pointee) || **pointee == TypeDesc::Void);
            uncertain |= !(ty.is_pointer_like() || decays_to_element);
            true
        });
    viable.then_some(uncertain)
}

fn copies_container(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    init: Node,
    instance: &str,
) -> bool {
    single_argument(init).is_some_and(|value| {
        receiver_desc(program, ctx, source, value)
            .as_ref()
            .and_then(class_spelling_of_desc)
            .is_some_and(|name| {
                canonical_specialization(program, ctx, name)
                    == canonical_specialization(program, ctx, instance)
            })
    })
}

/// The configured operations found in the indexed canonical prelude. The
/// complete prelude is merged once, after TU lowering.
#[derive(Default)]
pub(super) struct CanonicalDeclarations {
    members: BTreeMap<String, Vec<(String, usize)>>,
    list_constructors: BTreeMap<String, Vec<ListConstructor>>,
    owners: BTreeMap<String, String>,
    specialized_owners: std::collections::BTreeSet<String>,
    partial_owners: BTreeMap<String, Arc<str>>,
    defaults: BTreeMap<String, ContainerTemplateDefaults>,
}

impl CanonicalDeclarations {
    pub(super) fn collect(program: &Program, models: &ContainerModels) -> Self {
        let mut declarations = Self::default();
        for model in models.iter() {
            if let Some(class) = indexed_owner(program, model).filter(|class| *class != model.name)
            {
                declarations
                    .owners
                    .insert(model.name.clone(), class.to_owned());
            }
            for class in std::iter::once(&model.name).chain(model.declaration_aliases()) {
                declarations.collect_owner(program, class, model);
                if let Some(defaults) = program.types.container_template_defaults(class) {
                    declarations
                        .defaults
                        .insert(class.clone(), defaults.clone());
                }
            }
        }
        for (owner, model) in specialized_owners(program, models) {
            declarations.specialized_owners.insert(owner.to_owned());
            declarations.collect_owner(program, owner, model);
        }
        declarations.partial_owners.extend(
            program
                .types
                .container_partial_owners()
                .map(|(prefix, owner)| (prefix.clone(), Arc::clone(owner))),
        );
        declarations
    }

    fn collect_owner(&mut self, program: &Program, class: &str, model: &ContainerModel) {
        for op in &model.operations {
            if declared(program, class, op.method(), op.arity()) {
                self.members
                    .entry(class.to_owned())
                    .or_default()
                    .push((op.method().to_owned(), op.arity()));
            }
        }
        if model.initializer_list {
            let constructors: Vec<_> = list_constructors(program, class, model).collect();
            if !constructors.is_empty() {
                self.list_constructors
                    .insert(class.to_owned(), constructors);
            }
        }
    }

    fn contains(&self, class: &str, method: &str, arity: usize) -> bool {
        self.members.get(class).is_some_and(|members| {
            members
                .iter()
                .any(|(name, count)| name == method && *count == arity)
        })
    }
}

/// Publish the prelude's small declaration index independently from the unit.
/// Only a modeled operation that needs canonical declarations waits for it.
pub(super) struct CanonicalLookup {
    ready: OnceLock<Option<CanonicalDeclarations>>,
    pending: Mutex<Option<mpsc::Receiver<CanonicalDeclarations>>>,
}

impl CanonicalLookup {
    pub(super) fn new(receiver: mpsc::Receiver<CanonicalDeclarations>) -> Self {
        Self {
            ready: OnceLock::new(),
            pending: Mutex::new(Some(receiver)),
        }
    }

    fn get(&self) -> Option<&CanonicalDeclarations> {
        self.ready
            .get_or_init(|| self.pending.lock().unwrap().take()?.recv().ok())
            .as_ref()
    }
}

fn canonical_declarations(ctx: &LowerContext) -> Option<&CanonicalDeclarations> {
    ctx.canonical_containers.as_deref()?.get()
}

fn canonical_member(ctx: &LowerContext, class: &str, method: &str, arity: usize) -> bool {
    canonical_declarations(ctx).is_some_and(|decls| decls.contains(class, method, arity))
}

fn indexed_owner<'a>(program: &Program, model: &'a ContainerModel) -> Option<&'a str> {
    std::iter::once(&model.name)
        .chain(model.declaration_aliases())
        .find(|class| program.types.is_struct_defined(class))
        .map(String::as_str)
}

pub(super) fn models_iterators(model: &ContainerModel) -> bool {
    model
        .operations
        .iter()
        .any(|op| matches!(op, ContainerOperation::Iterator { .. }))
}

pub(super) fn aliased_iterator(
    program: &Program,
    ctx: &LowerContext,
    spelling: &str,
) -> Option<TypeDesc> {
    let (alias, member) = spelling.rsplit_once("::")?;
    if !matches!(member, "iterator" | "const_iterator") {
        return None;
    }
    let instance = aliased_instantiation(program, ctx, alias)?;
    let (class, _) = instance.split_once('<')?;
    if !models_iterators(ctx.container_models.get(class)?) {
        return None;
    }
    program.types.template_element(&instance)?;
    Some(TypeDesc::Struct {
        name: format!("{instance}::{member}"),
        fields: Vec::new(),
    })
}

/// Lower the template argument through the same type/declarator path as a
/// `using` declaration. Bind aliases in their declaration scope and keep
/// the descriptor with its specialization for later uses and header imports.
pub(super) fn specialization(
    program: &mut Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    class: &str,
    model: &ContainerModel,
) -> Option<String> {
    let arguments = template_type_node(node)?.cached_field("arguments")?;
    let arg = arguments
        .named_children(&mut arguments.walk())
        .filter(|n| n.cached_kind() != "comment")
        .nth(model.element)?;
    // With an explicit allocator the grammar may spell a simple element type
    // as a bare identifier; accept a known type/alias there as well.
    let type_node = if arg.cached_kind() == "type_descriptor"
        || is_name(arg)
        || matches!(arg.cached_kind(), "type_identifier" | "template_type")
    {
        arg
    } else {
        return None;
    };
    if spelling_is_dependent(
        ctx,
        source,
        type_node,
        node_text(source, &type_node),
        Spelling::Source,
    ) {
        return None;
    }
    let decl = type_node.cached_field("declarator");
    if decl.is_some_and(declares_reference) {
        return None;
    }
    let base = type_desc_from_node(program, ctx, source, type_node);
    let element = abstract_declarator_shape(base, decl, true, ReferenceLayer::Pointer);
    let type_text = type_node.cached_field("type").unwrap_or(type_node);
    let callable = matches!(element, TypeDesc::FnPtr { .. })
        && is_callable_wrapper(&normalize_qualified(node_text(source, &type_text)));
    // A container element may itself be a modeled container, so its stored
    // callbacks stay addressable through the nested element cell.
    let nested = modeled_container_desc(ctx, &element)
        && matches!(
            &element,
            TypeDesc::Struct { name, .. }
                if program.types.template_element(name).is_some()
                    || program
                        .types
                        .template_element(&canonical_specialization(program, ctx, name))
                        .is_some()
        );
    if !callable && !matches!(element, TypeDesc::Ptr(_)) && !nested {
        return None;
    }
    let class = if class == model.name && !model.declaration_aliases().is_empty() {
        indexed_owner(program, model)
            .or_else(|| {
                canonical_declarations(ctx)?
                    .owners
                    .get(class)
                    .map(String::as_str)
            })
            .unwrap_or(class)
    } else {
        class
    };
    let cv = spelled_type_cv(program, ctx, node_text(source, &type_node));
    let mut args = template_argument_spellings(program, ctx, source, arguments, 0);
    complete_template_arguments(program, ctx, class, &mut args);
    let name = format!("{class}<{}>", args.join(","));
    program
        .types
        .register_template_element(&name, without_fields(&element), cv);
    Some(name)
}

fn template_type_node(node: Node<'_>) -> Option<Node<'_>> {
    qualified_identifier_chain(node).find_map(|part| {
        if part.cached_kind() == "template_type" {
            Some(part)
        } else {
            ["scope", "name"].into_iter().find_map(|field| {
                part.cached_field(field)
                    .filter(|name| name.cached_kind() == "template_type")
            })
        }
    })
}

/// Preserve the declared specialization of a named standard allocator so
/// its template arguments match the container's bound allocator type.
pub(super) fn allocator_specialization(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    class: &str,
) -> Option<String> {
    if !matches!(class, "std::allocator" | "std::__1::allocator") {
        return None;
    }
    if node.has_error()
        || ctx.template_depth.get() > 0
            && spelling_is_dependent(
                ctx,
                source,
                node,
                node_text(source, &node),
                Spelling::Source,
            )
    {
        return None;
    }
    type_spelling_for(program, ctx, source, node, node, 0, true)
}

pub(super) fn is_std_pair(name: &str) -> bool {
    matches!(name, "std::pair" | "std::__1::pair")
}

/// Layout for a concrete standard pair carrying a modeled second value.
/// Its primary template's `second` type does not describe that value.
pub(super) fn pair_specialization(
    program: &mut Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    class: &str,
) -> Option<TypeDesc> {
    if !is_std_pair(class) {
        return None;
    }
    let args = template_type_node(node)?.cached_field("arguments")?;
    let [first, second]: [Node; 2] = arguments(args)?.try_into().ok()?;
    let name = format!(
        "{class}<{}>",
        template_argument_spellings(program, ctx, source, args, 0).join(",")
    );
    if program.types.is_struct_declared(&name) {
        return Some(TypeDesc::Struct {
            name,
            fields: Vec::new(),
        });
    }
    let field_type = |program: &mut Program, arg: Node| {
        let decl = arg.cached_field("declarator");
        if arg.cached_kind() != "type_descriptor"
            || decl.is_some_and(declares_reference)
            || spelling_is_dependent(ctx, source, arg, node_text(source, &arg), Spelling::Source)
        {
            return None;
        }
        let base = type_desc_from_node(program, ctx, source, arg);
        Some(abstract_declarator_shape(
            base,
            decl,
            true,
            ReferenceLayer::Pointer,
        ))
    };
    let second = field_type(program, second)?;
    if !second.is_pointer_like() && !modeled_container_desc(ctx, &second) {
        return None;
    }
    let first = field_type(program, first)?;
    Some(TypeDesc::Struct {
        name,
        fields: vec![
            ("first".into(), without_fields(&first)),
            ("second".into(), without_fields(&second)),
        ],
    })
}

fn pair_type(program: &Program, var: VarId) -> Option<TypeId> {
    let id = program.symbols.variable(var).type_id;
    let TypeDesc::Struct { name: class, .. } = program.types.get(id).desc.as_ref() else {
        return None;
    };
    is_std_pair(class.split_once('<')?.0).then_some(id)
}

pub(super) fn pair_field(program: &Program, var: VarId, name: &str) -> Option<(FieldId, TypeId)> {
    field_in_pair(program, pair_type(program, var)?, name)
}

fn field_in_pair(program: &Program, id: TypeId, name: &str) -> Option<(FieldId, TypeId)> {
    let field = program.types.field_id_by_name(id, name)?;
    let ty = program.types.get(id).layout.fields.get(&field)?.type_id;
    Some((field, ty))
}

pub(super) fn lower_pair_initializer(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    var: VarId,
    init: Node,
) -> bool {
    if !ctx.is_cpp || node_is_from_ignored_macro(ctx, init) {
        return false;
    }
    let Some(pair) = pair_type(program, var) else {
        return false;
    };
    let copy_from = |program: &mut Program, ctx: &mut LowerContext, value| {
        pair_source(program, ctx, source, value).is_some_and(|src| {
            lower_operation(program, ctx, source, init, |program, ctx| {
                copy_pair_fields(program, ctx, init, var, src)
            })
        })
    };
    if !is_list_initializer(ctx, init) {
        return copy_from(program, ctx, init);
    }
    let (Some(first), Some(second)) = (
        field_in_pair(program, pair, "first"),
        field_in_pair(program, pair, "second"),
    ) else {
        return false;
    };
    let Some(values) = arguments(init) else {
        return false;
    };
    let [first_value, second_value] = values.as_slice() else {
        return single_argument(init).is_some_and(|value| copy_from(program, ctx, value));
    };
    for ((name, (field, ty)), clause) in [("first", first), ("second", second)]
        .into_iter()
        .zip([*first_value, *second_value])
    {
        let desc = program.types.get(ty).desc.as_ref().clone();
        if modeled_container_desc(ctx, &desc) {
            if let Some(elements) = Elements::for_field(program, ctx, pair, name, &desc) {
                let handled = lower_operation(program, ctx, source, clause, |program, ctx| {
                    if node_is_from_ignored_macro(ctx, clause) {
                        return true;
                    }
                    if store_nested_braced(program, ctx, source, elements.clone(), clause) {
                        return true;
                    }
                    if let Some(braced) =
                        typed_inner_braces(program, ctx, source, clause, &elements)
                    {
                        if store_nested_braced(program, ctx, source, elements.clone(), braced) {
                            return true;
                        }
                    }
                    false
                });
                if handled {
                    continue;
                }
            }
            // The mapped slot is a modeled container; an unrecognized clause
            // must not be stored into its field cell as a scalar element.
            continue;
        }
        let Some(value) = unwrap_braced_value(clause) else {
            continue;
        };
        if node_is_from_ignored_macro(ctx, clause) || node_is_from_ignored_macro(ctx, value) {
            continue;
        }
        lower_operation(program, ctx, source, value, |program, ctx| {
            let base = addr_of_temp(program, ctx, init, var);
            let dest = alloc_gep_temp(program, ctx, init, base, field, name.into());
            if let Some(src) = operand_value(program, ctx, source, value) {
                program.flow.push(FlowConstraint::Store { dst: dest, src });
            } else {
                emit_store_to_location(program, ctx, source, init, dest, value, Some(ty));
            }
        });
    }
    true
}

fn pair_source(program: &Program, ctx: &LowerContext, source: &str, node: Node) -> Option<VarId> {
    let node = peel_expression(node);
    if !is_name(node)
        || node_is_from_ignored_macro(ctx, node)
        || implicit_this(program, ctx, source, node).is_some()
    {
        return None;
    }
    lookup_var_node(program, ctx, source, node)
}

pub(super) fn lower_pair_assignment(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    node: Node,
    dst: VarId,
    rhs: Node,
) {
    if ctx.is_cpp
        && pair_type(program, dst).is_some()
        && node
            .cached_field("operator")
            .is_some_and(|op| node_text(source, &op) == "=")
    {
        if let Some(src) = pair_source(program, ctx, source, rhs) {
            copy_pair_fields(program, ctx, node, dst, src);
        }
    }
}

/// Different pair specializations have separate field summaries. Copy the
/// corresponding pointer/callable slots and modeled container elements
/// through the usual field cells.
pub(super) fn copy_pair_fields(
    program: &mut Program,
    ctx: &mut LowerContext,
    node: Node,
    dst: VarId,
    src: VarId,
) -> bool {
    let Some((dst_pair, src_pair)) = pair_type(program, dst).zip(pair_type(program, src)) else {
        return false;
    };
    if dst_pair == src_pair {
        return false;
    }
    let mut bases = None;
    let mut copied = false;
    for name in ["first", "second"] {
        let Some(((dst_field, dst_ty), (src_field, src_ty))) =
            field_in_pair(program, dst_pair, name).zip(field_in_pair(program, src_pair, name))
        else {
            continue;
        };
        let dst_desc = program.types.get(dst_ty).desc.as_ref().clone();
        let src_desc = program.types.get(src_ty).desc.as_ref().clone();
        if modeled_container_desc(ctx, &dst_desc) && modeled_container_desc(ctx, &src_desc) {
            let from = Elements::for_field(program, ctx, src_pair, name, &src_desc);
            let to = Elements::for_field(program, ctx, dst_pair, name, &dst_desc);
            if let (Some(from), Some(to)) = (from, to) {
                copy_container_elements(program, ctx, node, from, to);
                copied = true;
            }
            continue;
        }
        if !(dst_desc.is_pointer_like() && src_desc.is_pointer_like()) {
            continue;
        }
        let (dst_base, src_base) = *bases.get_or_insert_with(|| {
            (
                addr_of_temp(program, ctx, node, dst),
                addr_of_temp(program, ctx, node, src),
            )
        });
        let src = alloc_gep_temp(program, ctx, node, src_base, src_field, name.into());
        let value = emit_load(program, ctx, node, src, src_ty);
        let dst = alloc_gep_temp(program, ctx, node, dst_base, dst_field, name.into());
        program.flow.push(FlowConstraint::Store { dst, src: value });
        copied = true;
    }
    copied
}

pub(super) fn record_template_defaults(
    program: &mut Program,
    ctx: &LowerContext,
    source: &str,
    class: &str,
    params: Node,
) {
    let names = template_parameter_names(source, params);
    for (position, param) in params
        .named_children(&mut params.walk())
        .filter(|param| param.cached_kind() != "comment")
        .enumerate()
    {
        let Some(node) = param.cached_field("default_type") else {
            continue;
        };
        if let Some(default) =
            template_type_default(program, ctx, source, node, &names[..position], 0)
        {
            program
                .types
                .note_container_template_default(class, position, default);
        }
    }
}

fn template_type_default(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    parameters: &[Option<&str>],
    depth: u32,
) -> Option<ContainerTemplateDefault> {
    if node.has_error() || depth >= MAX_AST_WALK_DEPTH {
        return None;
    }
    let spelling = node_text(source, &node).trim();
    if let Some(position) = parameters.iter().position(|p| *p == Some(spelling)) {
        return Some(ContainerTemplateDefault::Parameter(position));
    }
    let mentions_parameter = |spelling| {
        parameters
            .iter()
            .flatten()
            .any(|param| spelling_mentions(spelling, Spelling::Source, param))
    };
    if !mentions_parameter(spelling) {
        let ty = node.cached_field("type").or_else(|| {
            matches!(
                node.cached_kind(),
                "type_identifier"
                    | "qualified_identifier"
                    | "template_type"
                    | "primitive_type"
                    | "sized_type_specifier"
            )
            .then_some(node)
        })?;
        return type_spelling_for(program, ctx, source, node, ty, depth, true)
            .map(ContainerTemplateDefault::Type);
    }
    // Bind simple dependent type arguments, such as `allocator<T>`. A
    // parameter with a declarator or member lookup needs semantic substitution.
    let ty = node.cached_field("type").unwrap_or(node);
    if spelling != node_text(source, &ty).trim() || !template_tail(spelling).trim().is_empty() {
        return None;
    }
    let head = type_name_before_template(spelling);
    if mentions_parameter(head) {
        return None;
    }
    let arguments = arguments(template_type_node(ty)?.cached_field("arguments")?)?
        .into_iter()
        .map(|arg| template_type_default(program, ctx, source, arg, parameters, depth + 1))
        .collect::<Option<Vec<_>>>()?;
    Some(ContainerTemplateDefault::Template {
        head: qualify_class_name(program, ctx, &normalize_qualified(head)),
        arguments,
    })
}

/// Defaults for a modeled class template under its written, normalized, or
/// indexed (inline-namespace) owner name. A forward declaration without
/// defaults must not hide the canonical prelude's full definition.
fn class_template_defaults<'a>(
    program: &'a Program,
    ctx: &'a LowerContext,
    class: &str,
) -> Option<&'a ContainerTemplateDefaults> {
    let canonical = canonical_std_class(class);
    let owner = ctx.container_models.get(&canonical).and_then(|model| {
        indexed_owner(program, model).or_else(|| {
            canonical_declarations(ctx)?
                .owners
                .get(&model.name)
                .map(String::as_str)
        })
    });
    let mut keys: Vec<&str> = vec![class];
    if canonical != class {
        keys.push(canonical.as_str());
    }
    if let Some(owner) = owner {
        if !keys.contains(&owner) {
            keys.push(owner);
        }
    }
    for key in keys {
        if let Some(defaults) = program.types.container_template_defaults(key) {
            return Some(defaults);
        }
        if !program.types.is_struct_defined(key) {
            if let Some(defaults) = canonical_declarations(ctx).and_then(|d| d.defaults.get(key)) {
                return Some(defaults);
            }
        }
    }
    None
}

fn complete_template_arguments(
    program: &Program,
    ctx: &LowerContext,
    class: &str,
    args: &mut Vec<String>,
) {
    let defaults = class_template_defaults(program, ctx, class);
    if let Some(defaults) = defaults {
        while let Some(arg) = defaults
            .get(&args.len())
            .and_then(|default| default.bind(args))
        {
            args.push(bounded_template_argument(program, ctx, arg));
        }
    }
}

/// Keep concrete specializations of modeled class templates under their own
/// member names. A trailing list of distinct, unconstrained template
/// parameters lets that declaration match instances sharing its prefix.
pub(super) fn declaration_owner(
    program: &mut Program,
    ctx: &LowerContext,
    source: &str,
    specifier: Node,
    class: &str,
) -> Option<String> {
    if !ctx.is_cpp {
        return None;
    }
    let name = specifier.cached_field("name")?;
    if is_std_pair(class) {
        let parent = specifier.parent()?;
        let template = if parent.cached_kind() == "declaration" {
            parent.parent()?
        } else {
            parent
        };
        // Only declarations introduced by `template<>` replace the primary
        // pair's members. An ordinary instantiation still uses the primary.
        if template.cached_kind() != "template_declaration"
            || template.cached_field("parameters")?.named_child_count() != 0
        {
            return None;
        }
        let args = template_type_node(name)?.cached_field("arguments")?;
        let spellings = template_argument_spellings(program, ctx, source, args, 0);
        return Some(format!("{class}<{}>", spellings.join(",")));
    }
    if name.cached_kind() != "template_type" || ctx.container_models.get(class).is_none() {
        return None;
    }
    let args = arguments(name.cached_field("arguments")?)?;
    let mut dependent = Vec::new();
    let mut prefix_len = args.len();
    for (index, &arg) in args.iter().enumerate() {
        let spelling = node_text(source, &arg);
        if spelling_is_dependent(ctx, source, arg, spelling, Spelling::Source) {
            if index == 0
                || !spelling.chars().all(|c| c.is_alphanumeric() || c == '_')
                || dependent.contains(&spelling)
            {
                return None;
            }
            prefix_len = prefix_len.min(index);
            dependent.push(spelling);
        } else if !dependent.is_empty() {
            return None;
        }
    }
    let mut spellings: Vec<_> = args
        .iter()
        .map(|&arg| {
            type_spelling(program, ctx, source, arg)
                .unwrap_or_else(|| normalize_spacing(node_text(source, &arg)))
        })
        .collect();
    if dependent.is_empty() {
        complete_template_arguments(program, ctx, class, &mut spellings);
    }
    let owner = format!("{class}<{}>", spellings.join(","));
    let partial_prefix = (prefix_len < args.len())
        .then(|| format!("{class}<{}>", spellings[..prefix_len].join(",")));
    program
        .types
        .note_container_specialization(&owner, partial_prefix);
    Some(owner)
}

fn instantiated_owner(
    program: &Program,
    ctx: &LowerContext,
    name: &str,
    class: &str,
) -> Option<String> {
    if program.types.is_struct_declared(name) {
        return Some(name.to_owned());
    }
    let canonical = canonical_declarations(ctx);
    if canonical.is_some_and(|declarations| declarations.specialized_owners.contains(name)) {
        return Some(name.to_owned());
    }
    let mut args = template_arguments(name);
    while !args.is_empty() {
        let prefix = format!("{class}<{}>", args.join(","));
        if let Some(owner) = program
            .types
            .container_partial_owner(&prefix)
            .or_else(|| canonical?.partial_owners.get(&prefix).map(AsRef::as_ref))
        {
            return Some(owner.to_owned());
        }
        args.pop();
    }
    None
}

pub(super) fn member_lookup_owner<'a>(
    program: &Program,
    ctx: &LowerContext,
    name: &'a str,
) -> Cow<'a, str> {
    let name = name.trim_end();
    let Some((class, _)) = name.split_once('<') else {
        return Cow::Borrowed(name);
    };
    if is_std_pair(class) && program.types.is_struct_declared(name) {
        return Cow::Borrowed(name);
    }
    if name.ends_with('>') && ctx.container_models.get(class).is_some() {
        if let Some(owner) = instantiated_owner(program, ctx, name, class) {
            return Cow::Owned(owner);
        }
    }
    receiver_lookup_name(name)
}

pub(super) fn call_member_owner(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    receiver: Node,
    arrow: bool,
    class: &str,
) -> String {
    let lookup = receiver_lookup_name(class);
    if ctx.container_models.get(&lookup).is_some() {
        if let Some(desc) = receiver_desc(program, ctx, source, receiver) {
            let instance = if arrow {
                resolve_operator_arrow_receiver(program, desc)
            } else {
                class_spelling_of_desc(&desc).map(str::to_owned)
            };
            if let Some(instance) = instance {
                return member_lookup_owner(program, ctx, &instance).into_owned();
            }
        }
    }
    lookup.into_owned()
}

pub(super) fn known_specialization(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    class: &str,
) -> Option<String> {
    let args = template_type_node(node)?.cached_field("arguments")?;
    let mut args = template_argument_spellings(program, ctx, source, args, 0);
    complete_template_arguments(program, ctx, class, &mut args);
    instantiated_owner(program, ctx, &format!("{class}<{}>", args.join(",")), class)
}

fn template_argument_spellings(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    arguments: Node,
    depth: u32,
) -> Vec<String> {
    arguments
        .named_children(&mut arguments.walk())
        .filter(|n| n.cached_kind() != "comment")
        .map(|arg| {
            let spelling = type_spelling_at(program, ctx, source, arg, depth, true)
                .unwrap_or_else(|| normalize_spacing(node_text(source, &arg)));
            bounded_template_argument(program, ctx, spelling)
        })
        .collect()
}

/// Long arguments are shared by their canonical identity before an enclosing
/// specialization can duplicate them, including in written allocator slots.
/// The enclosing name stays parseable and the element descriptor still names
/// its separately registered specialization.
fn bounded_template_argument(program: &Program, ctx: &LowerContext, spelling: String) -> String {
    if spelling.len() > COMPACT_IDENTITY_THRESHOLD {
        canonical_specialization_at(program, ctx, &spelling, 0, false)
    } else {
        spelling
    }
}

/// Keep the type's source qualifiers, signedness and declarator separate
/// from the descriptor used for flow. Expand recorded aliases in their
/// declaration scope before using the spelling as a specialization key.
pub(super) fn type_spelling(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
) -> Option<String> {
    type_spelling_at(program, ctx, source, node, 0, true)
}

/// Spell the type named by a C-style `(T){...}` type descriptor or by a C++
/// braced temporary's `T` node (`T{...}`), which is not a descriptor.
pub(super) fn type_spelling_node(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
) -> Option<String> {
    if node.cached_kind() == "type_descriptor" {
        type_spelling(program, ctx, source, node)
    } else {
        type_spelling_for(program, ctx, source, node, node, 0, true)
    }
}

/// The alias's own spelling with default template arguments left implicit.
/// Expanding them into an alias that names another specialization would
/// duplicate the previous spelling in both the element and allocator slots
/// and grow exponentially down an alias chain.
pub(super) fn type_spelling_implicit_defaults(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
) -> Option<String> {
    type_spelling_at(program, ctx, source, node, 0, false)
}

fn type_spelling_at(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    depth: u32,
    fill_defaults: bool,
) -> Option<String> {
    if node.has_error() || depth >= MAX_AST_WALK_DEPTH {
        return None;
    }
    type_spelling_for(
        program,
        ctx,
        source,
        node,
        node.cached_field("type")?,
        depth,
        fill_defaults,
    )
}

/// Split at the last `target` outside `<...>` template arguments and
/// parenthesized declarators.
fn split_top_level(spelling: &str, target: char) -> Option<(&str, &str)> {
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut split = None;
    for (index, ch) in spelling.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            _ => {}
        }
        if ch == target && angle == 0 && paren == 0 {
            split = Some(index);
        }
    }
    split.map(|index| (&spelling[..index], &spelling[index + target.len_utf8()..]))
}

/// The `*` of a function-pointer declarator (`void (*)(int *)`), which is
/// inside the declarator's parentheses rather than at the top level.
fn split_function_pointer_star(spelling: &str) -> Option<(&str, &str)> {
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut in_declarator = false;
    for (index, ch) in spelling.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            '(' if angle == 0 => {
                if paren == 0 {
                    in_declarator = true;
                }
                paren += 1;
            }
            ')' if angle == 0 => {
                paren = paren.saturating_sub(1);
                if paren == 0 {
                    in_declarator = false;
                }
            }
            '*' if angle == 0 && in_declarator && paren == 1 => {
                return Some((&spelling[..index], &spelling[index + 1..]));
            }
            _ => {}
        }
    }
    None
}

/// Function parameter types discard object cv, retaining qualifications on
/// referred-to objects, pointees and array elements.
fn parameter_type_spelling(spelling: String) -> String {
    if split_top_level(&spelling, '&').is_some() || split_top_level(&spelling, '[').is_some() {
        return spelling;
    }
    if split_top_level(&spelling, '*').is_some() {
        return strip_trailing_cv(&spelling).to_owned();
    }
    if let Some((before, tail)) = split_function_pointer_star(&spelling) {
        if let Some((qualifiers, after)) = tail.split_once(')') {
            if !qualifiers.contains('&') {
                let declarator = &spelling[before.len()..before.len() + 1 + qualifiers.len()];
                return format!("{before}{}){after}", strip_trailing_cv(declarator));
            }
        }
        return spelling;
    }
    strip_trailing_cv(without_leading_cv(&spelling)).to_owned()
}

pub(super) fn has_top_level(spelling: &str, target: char) -> bool {
    let mut depth = 0usize;
    for ch in spelling.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch == target && depth == 0 {
            return true;
        }
    }
    false
}

fn type_spelling_for(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    ty: Node,
    depth: u32,
    fill_defaults: bool,
) -> Option<String> {
    let before = source[node.start_byte()..ty.start_byte()].trim();
    let before = before.strip_prefix("typedef").unwrap_or(before).trim();
    let after = declarator_spelling(program, ctx, source, node, ty.end_byte(), depth)?;
    let mut qualifiers = TypeQualifiers::default();
    let before = qualifiers.take_prefix(before);
    let after = qualifiers.take_prefix(after.trim().trim_end_matches(';').trim());
    let named = if matches!(
        ty.cached_kind(),
        "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier"
    ) {
        ty.cached_field("name")?
    } else {
        ty
    };
    let raw = node_text(source, &named).trim();
    let alias = if !raw.contains('<') {
        let name = normalize_qualified(raw);
        let resolved = if let Some(alias) = local_alias_info(ctx, &name) {
            Some((alias.type_spelling.as_deref(), alias.qualification))
        } else {
            find_alias_in_scope(program, ctx, &name, |candidate, _| {
                program.types.resolve_alias(candidate)?;
                Some((
                    program
                        .types
                        .alias_type_spelling(candidate)
                        .map(AsRef::as_ref),
                    program
                        .types
                        .alias_qualification(candidate)
                        .unwrap_or_default(),
                ))
            })
        };
        resolved.and_then(|(spelling, cv)| {
            if spelling.is_none() {
                // Class aliases can carry cv without having a stored type spelling.
                qualifiers.constant |= cv.object_const;
            }
            spelling
        })
    } else {
        None
    };
    let head = if let Some(alias) = alias {
        let star = split_top_level(alias, '*').or_else(|| split_function_pointer_star(alias));
        if let Some((pointee, tail)) = star {
            // A qualifier on a pointer alias decorates the outer pointer.
            let mut pointer = qualifiers;
            if pointer.take_prefix(tail).is_empty() {
                qualifiers = TypeQualifiers::default();
                format!("{pointee}*{}", pointer.spelling())
            } else if has_top_level(alias, '(') && !qualifiers.spelling().is_empty() {
                let cv = qualifiers.spelling();
                qualifiers = TypeQualifiers::default();
                format!("{pointee}*{cv}{tail}")
            } else if has_top_level(alias, '(') && !after.is_empty() {
                format!("({alias})")
            } else {
                alias.to_owned()
            }
        } else if has_top_level(alias, '(') && !after.is_empty() {
            format!("({alias})")
        } else {
            alias.to_owned()
        }
    } else if let Some(builtin) = builtin_type_spelling(raw, &mut qualifiers) {
        builtin
    } else {
        let head = type_name_before_template(raw);
        let class = qualify_class_name(program, ctx, &normalize_qualified(head));
        if let Some(args) =
            template_type_node(named).and_then(|template| template.cached_field("arguments"))
        {
            let mut args_spelling =
                template_argument_spellings(program, ctx, source, args, depth + 1);
            if fill_defaults && ctx.container_models.get(&class).is_some() {
                complete_template_arguments(program, ctx, &class, &mut args_spelling);
            }
            format!(
                "{class}<{}>{}",
                args_spelling.join(","),
                &source[args.end_byte()..named.end_byte()]
            )
        } else {
            format!("{class}{}", &raw[head.len()..])
        }
    };
    let mut declarator = String::with_capacity(after.len());
    for (index, part) in after.split('*').enumerate() {
        if index == 0 {
            declarator.push_str(part);
        } else {
            let mut pointer = TypeQualifiers::default();
            let remainder = pointer.take_prefix(part);
            declarator.push('*');
            declarator.push_str(pointer.spelling());
            declarator.push(' ');
            declarator.push_str(remainder);
        }
    }
    Some(normalize_spacing(&format!(
        "{before} {} {head} {declarator}",
        qualifiers.spelling()
    )))
}

/// Substitute nested function parameter types inside an abstract declarator.
/// Declarator names and parameter defaults do not identify a type.
fn declarator_spelling(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    start: usize,
    depth: u32,
) -> Option<String> {
    let mut end = node.end_byte();
    if let Some(default) = node.cached_field("default_value") {
        let prefix = source.get(start..default.start_byte())?;
        end = start + prefix.trim_end().trim_end_matches('=').len();
    }
    let mut edits = Vec::new();
    for part in std::iter::successors(node.cached_field("declarator"), |part| {
        part.cached_field("declarator").or_else(|| {
            matches!(
                part.cached_kind(),
                "parenthesized_declarator" | "abstract_parenthesized_declarator"
            )
            .then(|| part.named_child(0))
            .flatten()
        })
    }) {
        if matches!(part.cached_kind(), "identifier" | "type_identifier") {
            edits.push((part.byte_range(), String::new()));
        } else if let Some(params) = part.cached_field("parameters") {
            for param in params.named_children(&mut params.walk()) {
                if let Some(spelling) =
                    type_spelling_at(program, ctx, source, param, depth + 1, true)
                {
                    edits.push((param.byte_range(), parameter_type_spelling(spelling)));
                }
            }
        }
    }
    edits.sort_unstable_by_key(|(range, _)| range.start);
    let mut spelling = source.get(start..end)?.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        if range.start < start || range.end > end {
            return None;
        }
        spelling.replace_range(range.start - start..range.end - start, &replacement);
    }
    Some(spelling)
}

#[derive(Clone, Copy, Default)]
struct TypeQualifiers {
    constant: bool,
    volatile: bool,
}

impl TypeQualifiers {
    fn take_prefix<'a>(&mut self, mut spelling: &'a str) -> &'a str {
        loop {
            spelling = spelling.trim_start();
            let end = spelling
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(spelling.len());
            match &spelling[..end] {
                "const" => self.constant = true,
                "volatile" => self.volatile = true,
                _ => return spelling,
            }
            spelling = &spelling[end..];
        }
    }

    fn spelling(self) -> &'static str {
        match (self.constant, self.volatile) {
            (true, true) => "const volatile",
            (true, false) => "const",
            (false, true) => "volatile",
            (false, false) => "",
        }
    }
}

/// Canonicalize the ordinary integral specifier order without conflating
/// signed and unsigned types, or either spelling of signedness with char.
fn builtin_type_spelling(raw: &str, qualifiers: &mut TypeQualifiers) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let words = raw.split_whitespace();
    if !words.clone().all(|word| {
        matches!(
            word,
            "const"
                | "volatile"
                | "signed"
                | "unsigned"
                | "short"
                | "long"
                | "int"
                | "char"
                | "double"
        )
    }) {
        return (is_fundamental_type_name(raw) || primitive_scalar_desc(raw).is_some())
            .then(|| raw.to_owned());
    }
    let mut sign = "";
    let mut base = "int";
    let mut longs = 0;
    for word in words {
        match word {
            "const" => qualifiers.constant = true,
            "volatile" => qualifiers.volatile = true,
            "signed" | "unsigned" => sign = word,
            "long" => longs += 1,
            "short" | "char" | "double" => base = word,
            _ => {}
        }
    }
    let base = match (longs, base) {
        (1, "int") => "long",
        (2, "int") => "long long",
        (1, "double") => "long double",
        (_, base) => base,
    };
    if sign == "unsigned" || sign == "signed" && base == "char" {
        Some(format!("{sign} {base}"))
    } else {
        Some(base.to_owned())
    }
}

#[derive(Clone)]
pub(super) struct Elements {
    owner: ContainerOwner,
    instance: String,
    element: TypeDesc,
}

enum StoredValue<'t> {
    Expression(Node<'t>),
    /// A named pair's `second` member: node, pair variable, pair type,
    /// member field, and member type.
    PairSecond(Node<'t>, VarId, TypeId, FieldId, TypeId),
}

/// A modeled container field of an object selected from an array or pointer.
pub(super) fn subscript_field(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    base: Node,
    name: &str,
) -> Option<HierarchyField> {
    let base = peel_expression(base);
    if base.cached_kind() != "subscript_expression"
        || !is_name(peel_expression(base.cached_field("argument")?))
    {
        return None;
    }
    let class = class_name_of_desc(&receiver_desc(program, ctx, source, base)?)?;
    let field = lookup_field_in_hierarchy(program, Some(ctx), &class, name)?;
    let TypeDesc::Struct { name, .. } = program.types.get(field.type_id).desc.as_ref() else {
        return None;
    };
    program.types.template_element(name)?;
    Some(field)
}

impl Elements {
    fn for_var(program: &Program, ctx: &LowerContext, var: VarId) -> Option<Self> {
        let ty = program.symbols.variable(var).type_id;
        let mut desc = program.types.get(ty).desc.as_ref();
        // A reference alias lowers to a pointer type. Its address is still the
        // referenced container's cell, so unwrap that layer before recording
        // or resolving a nested range owner.
        if ctx.reference_vars.contains(&var) {
            if let TypeDesc::Ptr(inner) = desc {
                desc = inner.as_ref();
            }
        }
        // A recorded nested range binding names the referenced container's
        // element cell even when the loop variable is an explicit reference.
        if let Some(owner) = ctx.nested_container_owners.get(&var) {
            return Self::new(program, ctx, owner.clone(), desc);
        }
        if ctx.reference_vars.contains(&var) {
            return None;
        }
        Self::new(program, ctx, ContainerOwner::Variable(var), desc)
    }

    fn for_receiver(
        program: &Program,
        ctx: &LowerContext,
        source: &str,
        recv: Node,
    ) -> Option<Self> {
        let recv = peel_expression(recv);
        let (field, name) = match recv.cached_kind() {
            "identifier" => match implicit_this_field(program, ctx, source, recv) {
                Some((_, field)) => (field, node_text(source, &recv)),
                None => {
                    return Self::for_var(
                        program,
                        ctx,
                        lookup_var_node(program, ctx, source, recv)?,
                    )
                }
            },
            "qualified_identifier" => {
                return Self::for_var(program, ctx, lookup_var_node(program, ctx, source, recv)?);
            }
            "field_expression" => {
                let field_node = recv.cached_field("field")?;
                if node_is_from_ignored_macro(ctx, recv)
                    || node_is_from_ignored_macro(ctx, field_node)
                {
                    return None;
                }
                if let Some(var) = static_member_access(program, ctx, source, recv) {
                    return Self::for_var(program, ctx, var);
                }
                let base = recv.cached_field("argument")?;
                // The field token is checked above, but an ignored bracket or
                // accessor in the base chain must stop the read too
                // (`holders IDX->slots`, `holders.AT_METHOD(0)->slots`).
                if receiver_chain_ignored(ctx, source, base) {
                    return None;
                }
                let name = node_text(source, &field_node);
                let arrow = is_arrow_access(recv);
                // A named standard pair's specialized layout carries the
                // mapped member type; resolve its container member directly
                // instead of the primary template's dependent field.
                if !arrow {
                    if let Some(var) = lookup_var_node(program, ctx, source, base) {
                        if let Some(pair) = pair_type(program, var) {
                            if let Some((_, ty)) = field_in_pair(program, pair, name) {
                                let desc = program.types.get(ty).desc.as_ref().clone();
                                if let Some(elements) =
                                    Self::for_field(program, ctx, pair, name, &desc)
                                {
                                    return Some(elements);
                                }
                            }
                        }
                    }
                }
                let field = (!arrow)
                    .then(|| subscript_field(program, ctx, source, base, name))
                    .flatten()
                    .or_else(|| {
                        let cls = member_receiver_class(program, ctx, source, base, arrow)?;
                        lookup_field_in_hierarchy(program, Some(ctx), &cls, name)
                    })?;
                (field, name)
            }
            "call_expression" => {
                // An ignored accessor must not resolve through its return temp
                // either.
                if node_is_from_ignored_macro(ctx, recv)
                    || method_is_from_ignored_macro(ctx, source, recv)
                {
                    return None;
                }
                if is_std_move(source, recv) {
                    return Self::for_receiver(
                        program,
                        ctx,
                        source,
                        *call_arg_nodes(recv).first()?,
                    );
                }
                // A modeled accessor (`at`, `front`, `back`) returns an element
                // of its receiver, so descend into that receiver's nested cell.
                // Keep the descent in an unevaluated chain so the site emitter
                // records every bracket; the method token has its own macro
                // provenance.
                if let Some(inner) = ctx
                    .container_access_owners
                    .borrow()
                    .get(&recv.id())
                    .cloned()
                {
                    return Some(inner);
                }
                if let Some(Access::Direct(elements)) = access(program, ctx, source, recv) {
                    return elements.remember_nested_receiver(program, ctx, recv);
                }
                // Otherwise a call result materializes a return temp; when its
                // type is a modeled container, its cell is the only safe
                // storage. The callee's element summary remains a follow-up.
                let temp = ctx.call_return_dst.borrow().get(&recv.id()).copied()?;
                return Self::for_var(program, ctx, temp);
            }
            "subscript_expression" => {
                // Each bracket in a chain carries its own macro provenance;
                // an ignored bracket must stop the read even when the outer
                // expression is written outside the macro. Evaluation is
                // checked when flow is emitted, so unevaluated chains still
                // record every site.
                if node_is_from_ignored_macro(ctx, recv) || subscript_bracket_ignored(ctx, recv) {
                    return None;
                }
                if let Some(inner) = ctx
                    .container_access_owners
                    .borrow()
                    .get(&recv.id())
                    .cloned()
                {
                    return Some(inner);
                }
                let container = match access(program, ctx, source, recv)? {
                    Access::Direct(elements) => elements,
                    Access::Iterator { .. } => return None,
                };
                return container.remember_nested_receiver(program, ctx, recv);
            }
            _ => return None,
        };
        Self::for_field(
            program,
            ctx,
            field.owner_type_id,
            name,
            program.types.get(field.type_id).desc.as_ref(),
        )
    }

    fn remember_nested_receiver(
        &self,
        program: &Program,
        ctx: &LowerContext,
        node: Node,
    ) -> Option<Self> {
        let inner = self.nested(program, ctx)?;
        ctx.container_access_owners
            .borrow_mut()
            .insert(node.id(), inner.clone());
        Some(inner)
    }

    fn for_field(
        program: &Program,
        ctx: &LowerContext,
        class: trace_ir::TypeId,
        name: &str,
        desc: &TypeDesc,
    ) -> Option<Self> {
        Self::new(
            program,
            ctx,
            ContainerOwner::Field {
                class,
                name: name.into(),
            },
            desc,
        )
    }

    fn new(
        program: &Program,
        ctx: &LowerContext,
        owner: ContainerOwner,
        desc: &TypeDesc,
    ) -> Option<Self> {
        let TypeDesc::Struct { name, .. } = desc else {
            return None;
        };
        // A specialization spelled through an alias may be registered under
        // its expanded identity; use that key so the same C++ type shares its
        // element cell across spellings. Check the exact key first: rebuilding
        // the canonical spelling is expensive for deeply nested types.
        let canonical;
        let instance = if program.types.template_element(name).is_some() {
            name.as_str()
        } else {
            canonical = canonical_specialization(program, ctx, name);
            if program.types.template_element(&canonical).is_some() {
                canonical.as_str()
            } else {
                return None;
            }
        };
        let element = program.types.template_element(instance)?.clone();
        instance.split_once('<')?;
        Some(Self {
            owner,
            instance: instance.to_owned(),
            element,
        })
    }

    fn class(&self) -> &str {
        self.instance
            .split_once('<')
            .expect("container specialization")
            .0
    }

    /// Only standard map layouts identify template argument zero as the key.
    fn map_key(&self) -> Option<&str> {
        if !matches!(
            canonical_std_class(self.class()).as_str(),
            "std::map" | "std::unordered_map" | "std::multimap" | "std::unordered_multimap"
        ) {
            return None;
        }
        trace_ir::split_instance(&self.instance)?.1.first().copied()
    }

    pub(super) fn owner(&self) -> &ContainerOwner {
        &self.owner
    }

    /// The elements carried by containers stored in this container's element
    /// cell. Only a modeled element type nests; a plain callable or pointer
    /// element keeps the ordinary element cell.
    pub(super) fn nested(&self, program: &Program, ctx: &LowerContext) -> Option<Self> {
        let TypeDesc::Struct { name, .. } = &self.element else {
            return None;
        };
        // The nested cell key uses the canonical specialization identity, so
        // two TUs that spell an equivalent inner type differently (for
        // example an explicit default allocator) share one cell after merge.
        let canonical = canonical_specialization(program, ctx, name);
        let owner = ContainerOwner::Nested {
            parent: Arc::new(self.owner.clone()),
            instance: Arc::from(canonical.as_str()),
        };
        let mut inner = Self::new(program, ctx, owner.clone(), &self.element)?;
        inner.owner = owner;
        ctx.container_models.get(inner.class())?;
        Some(inner)
    }

    /// The stored element descriptor. Placeholder deduction uses it so an
    /// `auto` loop variable gets the element type the container model
    /// resolved, including a function-pointer alias the spelling walk cannot
    /// classify.
    pub(super) fn element(&self) -> &TypeDesc {
        &self.element
    }

    /// Qualification recorded beside the element descriptor (const object or
    /// pointee). A range-for variable carries it so overload resolution sees
    /// `for (auto h : std::vector<const T*>)` as a `T const*`.
    pub(super) fn cv(&self, program: &Program) -> trace_ir::CvQualification {
        program
            .types
            .template_element_cv(&self.instance)
            .unwrap_or_default()
    }

    fn address(&self, program: &mut Program, dst: VarId) -> trace_ir::TypeId {
        let element = program.types.intern(self.element.clone());
        program.flow.push(FlowConstraint::ContainerElement {
            dst,
            owner: self.owner.clone(),
            element,
        });
        element
    }

    fn store_values<'t>(
        self,
        program: &mut Program,
        ctx: &mut LowerContext,
        source: &str,
        at: Node,
        values: impl IntoIterator<Item = StoredValue<'t>>,
    ) {
        let nested = self.nested(program, ctx);
        let mut cell = None;
        for stored in values {
            let value = match stored {
                StoredValue::Expression(value) | StoredValue::PairSecond(value, ..) => value,
            };
            if node_is_from_ignored_macro(ctx, value) {
                continue;
            }
            if let Some(inner) = &nested {
                if let Some(braced) = typed_inner_braces(program, ctx, source, value, inner) {
                    store_nested_braced(program, ctx, source, inner.clone(), braced);
                    continue;
                }
                if let StoredValue::PairSecond(_, _, pair, _, member_ty) = &stored {
                    let desc = program.types.get(*member_ty).desc.as_ref().clone();
                    if let Some(from) = Elements::for_field(program, ctx, *pair, "second", &desc)
                        .filter(|from| ctx.container_models.get(from.class()).is_some())
                    {
                        lower_operation(program, ctx, source, value, |program, ctx| {
                            copy_container_elements(program, ctx, value, from, inner.clone());
                        });
                        continue;
                    }
                }
                if matches!(value.cached_kind(), "initializer_list" | "argument_list") {
                    store_nested_braced(program, ctx, source, inner.clone(), value);
                    continue;
                }

                if let Some(from) = Elements::for_receiver(program, ctx, source, value)
                    .filter(|from| ctx.container_models.get(from.class()).is_some())
                {
                    lower_operation(program, ctx, source, value, |program, ctx| {
                        copy_container_elements(program, ctx, value, from, inner.clone());
                    });
                    continue;
                }
            }
            // A nested element cell holds only inner containers or validated
            // inner elements. An unrecognized outer value must not be written
            // into it as if it were an inner element.
            if nested.is_some() {
                continue;
            }
            // The nested walker preserves braces while descending; unwrap the
            // innermost scalar braces the way the outer list path does.
            let value = if nested.is_none()
                && matches!(value.cached_kind(), "initializer_list" | "argument_list")
            {
                match unwrap_braced_value(value) {
                    Some(value) => value,
                    None => continue,
                }
            } else {
                value
            };
            let (address, element) = *cell.get_or_insert_with(|| {
                let ty = program.types.ptr_to(self.element.clone());
                let address = alloc_load_temp(program, ctx, at, ty);
                let element = self.address(program, address);
                (address, element)
            });
            lower_operation(program, ctx, source, value, |program, ctx| {
                if let StoredValue::PairSecond(_, var, _, field, ty) = stored {
                    let base = addr_of_temp(program, ctx, value, var);
                    let cell = alloc_gep_temp(program, ctx, value, base, field, "second".into());
                    let src = emit_load(program, ctx, value, cell, ty);
                    program
                        .flow
                        .push(FlowConstraint::Store { dst: address, src });
                    return;
                }
                let value = peel_value_casts(program, ctx, source, value);
                let cached = ctx.call_return_dst.borrow().get(&value.id()).copied();
                if let Some(src) = cached {
                    program
                        .flow
                        .push(FlowConstraint::Store { dst: address, src });
                } else if value.cached_kind() == "field_expression"
                    || implicit_this(program, ctx, source, value).is_some()
                {
                    if let Some(src) = operand_value(program, ctx, source, value) {
                        program
                            .flow
                            .push(FlowConstraint::Store { dst: address, src });
                    }
                } else {
                    emit_store_to_location(program, ctx, source, at, address, value, Some(element));
                }
            });
        }
    }
}

/// Store a braced value for an inner container: a constructor list when the
/// element type accepts one, otherwise a copy of a named inner container.
fn store_nested_braced(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    elements: Elements,
    node: Node,
) -> bool {
    // A single braced named container is a copy even when the inner list
    // could also accept it as one element; test the canonical type first.
    if let Some(candidate) = unwrap_braced_value(node) {
        if node_is_from_ignored_macro(ctx, candidate) {
            return true;
        }
        if let Some(from) = Elements::for_receiver(program, ctx, source, candidate).filter(|from| {
            let a = canonical_specialization(program, ctx, &from.instance);
            let b = canonical_specialization(program, ctx, &elements.instance);

            ctx.container_models.get(from.class()).is_some() && a == b
        }) {
            lower_operation(program, ctx, source, node, |program, ctx| {
                copy_container_elements(program, ctx, node, from, elements);
            });
            return true;
        }
    }
    // `{source, allocator}` invokes the allocator-extended copy constructor.
    // Recognize the matching source container before reading the brace as an
    // initializer list of inner elements.
    if let Some(copy_source) = allocator_extended_copy(program, ctx, source, &elements, node) {
        match copy_source {
            AllocatorCopySource::Cell(from) => {
                lower_operation(program, ctx, source, node, |program, ctx| {
                    copy_container_elements(program, ctx, node, from, elements);
                });
            }
            AllocatorCopySource::Temporary(candidate) => {
                if let Some(braced) = typed_inner_braces(program, ctx, source, candidate, &elements)
                {
                    store_nested_braced(program, ctx, source, elements.clone(), braced);
                }
            }
        }
        return true;
    }
    // A typed temporary of the matching inner specialization (`{T{...}}`)
    // is a copy source; store its initializer into the destination cell.
    if let Some(braced) = unwrap_braced_value(node)
        .and_then(|candidate| typed_inner_braces(program, ctx, source, candidate, &elements))
    {
        if store_nested_braced(program, ctx, source, elements.clone(), braced) {
            return true;
        }
    }
    lower_list_initializer(program, ctx, source, elements, node)
}

/// Whether a clause names an allocator that can extend the container's copy
/// constructor. An empty clause value-initializes the allocator.
fn allocator_clause_compatible(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    elements: &Elements,
    model: &ContainerModel,
    allocator: Node,
) -> bool {
    let allocator = peel_expression(allocator);
    if braced_clause_count(allocator) == Some(0) {
        return true;
    }
    if allocator.cached_kind() == "argument_list"
        && arguments(allocator).is_none_or(|args| args.is_empty())
    {
        return true;
    }
    if allocator_argument(program, ctx, source, elements, model, allocator) {
        return true;
    }
    let Some(expected) = copy_allocator_spelling(elements, model) else {
        return false;
    };
    let actual = match allocator.cached_kind() {
        "compound_literal_expression" => allocator
            .cached_field("type")
            .and_then(|ty| type_spelling_node(program, ctx, source, ty)),
        _ => {
            let ty = known_arg_type(program, ctx, source, allocator);
            class_spelling_of_desc(&ty).map(str::to_owned)
        }
    };
    actual.is_some_and(|actual| {
        canonical_specialization(program, ctx, &actual)
            == canonical_specialization(program, ctx, &expected)
    })
}

/// The copy constructor uses the allocator bound by the specialization.
/// Standard containers can have this parameter without modeling an
/// initializer-list overload with an allocator.
fn copy_allocator_spelling<'a>(
    elements: &'a Elements,
    model: &ContainerModel,
) -> Option<Cow<'a, str>> {
    let (_, args) = trace_ir::split_instance(&elements.instance)?;
    let class = canonical_std_class(elements.class());
    let standard_position = match class.as_str() {
        "std::vector" | "std::deque" | "std::list" | "std::forward_list" => Some(1),
        "std::set" | "std::multiset" => Some(2),
        "std::unordered_set" | "std::unordered_multiset" | "std::map" | "std::multimap" => Some(3),
        "std::unordered_map" | "std::unordered_multimap" => Some(4),
        _ => None,
    };
    let position = model.initializer_allocator.or(standard_position)?;
    if let Some(bound) = args.get(position) {
        return Some(Cow::Borrowed(bound));
    }
    // Template defaults with a dependent const key may be missing from the
    // declaration index. The standard allocator still has its specified type.
    standard_position?;
    let value = if let Some(key) = elements.map_key() {
        format!("std::pair<{},{}>", const_value_spelling(key), args.get(1)?)
    } else {
        args.first()?.to_string()
    };
    Some(Cow::Owned(format!("std::allocator<{value}>")))
}

fn const_value_spelling(spelling: &str) -> String {
    let mut qualifiers = TypeQualifiers {
        constant: true,
        ..TypeQualifiers::default()
    };
    if let Some((pointee, tail)) =
        split_top_level(spelling, '*').or_else(|| split_function_pointer_star(spelling))
    {
        let remainder = qualifiers.take_prefix(tail);
        return format!("{pointee}*{} {remainder}", qualifiers.spelling());
    }
    let value = qualifiers.take_prefix(spelling);
    format!("{} {value}", qualifiers.spelling())
}

enum AllocatorCopySource<'t> {
    /// A named container whose element cell can be copied.
    Cell(Elements),
    /// A typed temporary or type-only match with no element cell; a typed
    /// temporary's initializer is lowered into the destination instead.
    Temporary(Node<'t>),
}

/// The source of an allocator-extended copy (`{source, allocator}`) when the
/// clause shape and allocator fit `elements`.
fn allocator_extended_copy<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    elements: &Elements,
    node: Node<'t>,
) -> Option<AllocatorCopySource<'t>> {
    if node.cached_kind() != "initializer_list" {
        return None;
    }
    let mut cursor = node.walk();
    let clauses: Vec<_> = node
        .named_children(&mut cursor)
        .filter(|clause| clause.cached_kind() != "comment")
        .collect();
    let [source_clause, allocator] = clauses.as_slice() else {
        return None;
    };
    let candidate = unwrap_braced_value(*source_clause)?;
    if node_is_from_ignored_macro(ctx, candidate) {
        return None;
    }
    let model = ctx.container_models.get(elements.class())?;
    if !allocator_clause_compatible(program, ctx, source, elements, model, *allocator) {
        return None;
    }
    if let Some(from) = Elements::for_receiver(program, ctx, source, candidate).filter(|from| {
        let a = canonical_specialization(program, ctx, &from.instance);
        let b = canonical_specialization(program, ctx, &elements.instance);
        ctx.container_models.get(from.class()).is_some() && a == b
    }) {
        return Some(AllocatorCopySource::Cell(from));
    }
    let typed = typed_inner_braces(program, ctx, source, candidate, elements).is_some()
        || receiver_desc(program, ctx, source, candidate)
            .as_ref()
            .and_then(class_spelling_of_desc)
            .is_some_and(|name| {
                canonical_specialization(program, ctx, name)
                    == canonical_specialization(program, ctx, &elements.instance)
            });
    typed.then_some(AllocatorCopySource::Temporary(candidate))
}

/// The braced payload of a `std::initializer_list<T>{...}` temporary when
/// `T` matches the expected inner element. Such a temporary initializes the
/// inner container's element list even though its type is not the container.
fn initializer_list_compound_brace<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node<'t>,
    expected: &TypeDesc,
) -> Option<Node<'t>> {
    let node = peel_expression(node);
    if node.cached_kind() != "compound_literal_expression" {
        return None;
    }
    let ty = node.cached_field("type")?;
    let spelling = type_spelling_node(program, ctx, source, ty)?;
    let (class, args) = trace_ir::split_instance(&spelling)?;
    if canonical_std_class(without_leading_cv(class)) != "std::initializer_list" || args.len() != 1
    {
        return None;
    }
    if canonical_specialization_at(program, ctx, args[0], 0, false)
        != canonical_desc_at(program, ctx, expected, 0, false)
    {
        return None;
    }
    node.cached_field("value")
        .filter(|value| value.cached_kind() == "initializer_list")
}

/// A typed braced temporary (`std::vector<T>{...}`) that names the expected
/// inner container. Outer initializers and mapped slots reach `store_values`
/// without the insertion path's earlier unwrapping.
fn typed_inner_braces<'a>(
    program: &Program,
    ctx: &LowerContext,
    source: &'a str,
    value: Node<'a>,
    inner: &Elements,
) -> Option<Node<'a>> {
    let value = peel_expression(value);
    if value.cached_kind() != "compound_literal_expression" {
        return None;
    }
    let ty = value.cached_field("type")?;
    if node_is_from_ignored_macro(ctx, ty) {
        return None;
    }
    let spelling = type_spelling_node(program, ctx, source, ty)?;
    if canonical_specialization(program, ctx, &spelling)
        == canonical_specialization(program, ctx, &inner.instance)
    {
        return value
            .cached_field("value")
            .filter(|value| value.cached_kind() == "initializer_list");
    }
    let brace = initializer_list_compound_brace(program, ctx, source, value, inner.element())?;
    let model = ctx.container_models.get(inner.class())?;
    model.initializer_list.then_some(brace)
}

/// Seed a by-value range copy's own cells from the source container. The copy
/// must not alias the source: writing to the loop variable cannot change what
/// a later read of the outer container returns.
pub(super) fn copy_into_range_var(
    program: &mut Program,
    ctx: &mut LowerContext,
    at: Node,
    from: Elements,
    var: VarId,
) {
    let Some(to) = Elements::for_var(program, ctx, var) else {
        return;
    };
    copy_container_elements(program, ctx, at, from, to);
}

/// A container value stored in another container: copy the inner element
/// cell's contents into the nested cell, then recurse through matching
/// descendant cells so a three-level container keeps its deepest callbacks.
/// Loads and stores are monotone, so a later insertion into the inner
/// container still reaches the nested read.
fn copy_container_elements(
    program: &mut Program,
    ctx: &mut LowerContext,
    at: Node,
    from: Elements,
    to: Elements,
) {
    let from_canonical = canonical_specialization(program, ctx, &from.instance);
    let to_canonical = canonical_specialization(program, ctx, &to.instance);
    if from_canonical != to_canonical {
        return;
    }
    let from_pointer = program.types.ptr_to(from.element.clone());
    let from_address = alloc_load_temp(program, ctx, at, from_pointer);
    let from_element = from.address(program, from_address);
    let to_pointer = program.types.ptr_to(to.element.clone());
    let to_address = alloc_load_temp(program, ctx, at, to_pointer);
    to.address(program, to_address);
    let loaded = emit_load(program, ctx, at, from_address, from_element);
    program.flow.push(FlowConstraint::Store {
        dst: to_address,
        src: loaded,
    });
    if let (Some(from_deep), Some(to_deep)) = (from.nested(program, ctx), to.nested(program, ctx)) {
        copy_container_elements(program, ctx, at, from_deep, to_deep);
    }
}

/// An iterator carries the address of its originating container's cell.
/// Ordinary copies preserve that address, even when two iterator variables
/// of the same type originate from different containers.
pub(super) struct IteratorResult {
    container: Elements,
    constant: bool,
}

impl IteratorResult {
    pub(super) fn desc(&self) -> TypeDesc {
        let member = if self.constant {
            "const_iterator"
        } else {
            "iterator"
        };
        TypeDesc::Struct {
            name: format!("{}::{member}", self.container.instance),
            fields: Vec::new(),
        }
    }

    pub(super) fn address(self, program: &mut Program, dst: VarId) {
        self.container.address(program, dst);
    }
}

pub(super) enum Access<'t> {
    Direct(Elements),
    Iterator {
        receiver: Node<'t>,
        element: TypeDesc,
        cv: trace_ir::CvQualification,
        read_only: bool,
    },
}

impl Access<'_> {
    pub(super) fn element(&self) -> &TypeDesc {
        match self {
            Self::Direct(elements) => &elements.element,
            Self::Iterator { element, .. } => element,
        }
    }

    pub(super) fn cv(&self, program: &Program) -> trace_ir::CvQualification {
        match self {
            Self::Direct(elements) => program
                .types
                .template_element_cv(&elements.instance)
                .unwrap_or_default(),
            Self::Iterator { cv, .. } => *cv,
        }
    }

    pub(super) fn writable(&self) -> bool {
        !matches!(
            self,
            Self::Iterator {
                read_only: true,
                ..
            }
        )
    }

    pub(super) fn address(
        self,
        program: &mut Program,
        ctx: &mut LowerContext,
        source: &str,
        dst: VarId,
    ) {
        match self {
            Self::Direct(elements) => {
                elements.address(program, dst);
            }
            Self::Iterator { receiver, .. } => {
                if let Some(flow) = rhs_flow(program, ctx, source, receiver, &RhsDst::Var(dst)) {
                    program.flow.push(flow);
                }
            }
        }
    }
}

fn member_call<'a>(source: &'a str, node: Node<'a>) -> Option<(Node<'a>, Cow<'a, str>, Node<'a>)> {
    if let Some(op) = node
        .has_error()
        .then(|| explicit_member_operator_call(node, source))
        .flatten()
    {
        if op.is_arrow {
            return None;
        }
        return Some((
            op.receiver,
            Cow::Owned(normalize_qualified(op.op_name)),
            node.cached_field("arguments")?,
        ));
    }
    let func = node.cached_field("function")?;
    if func.cached_kind() != "field_expression" {
        return None;
    }
    let field = func.cached_field("field")?;
    let field = if field.cached_kind() == "template_method" {
        field.cached_field("name")?
    } else {
        field
    };
    if is_arrow_access(func) {
        return None;
    }
    Some((
        func.cached_field("argument")?,
        Cow::Borrowed(node_text(source, &field)),
        node.cached_field("arguments")?,
    ))
}

pub(super) fn method_is_from_ignored_macro(ctx: &LowerContext, source: &str, call: Node) -> bool {
    if ctx.ignored_macros.is_empty() {
        return false;
    }
    if let Some(op) = call
        .has_error()
        .then(|| explicit_member_operator_call(call, source))
        .flatten()
    {
        return node_is_from_ignored_macro(ctx, op.op_node)
            || node_is_from_ignored_macro(ctx, op.punct_node);
    }
    call.cached_field("function")
        .and_then(|func| func.cached_field("field"))
        .is_some_and(|field| {
            let field = if field.cached_kind() == "template_method" {
                field.cached_field("name").unwrap_or(field)
            } else {
                field
            };
            node_is_from_ignored_macro(ctx, field)
        })
}

fn arguments(args: Node) -> Option<Vec<Node>> {
    args.named_children(&mut args.walk())
        .filter(|arg| arg.cached_kind() != "comment")
        .map(|arg| (arg.cached_kind() != "parameter_pack_expansion").then_some(arg))
        .collect()
}

fn braced_value(node: Node, entry: Option<usize>) -> Option<Node> {
    let Some(entry) = entry else {
        return Some(node);
    };
    (node.cached_kind() == "initializer_list").then_some(node)?;
    node.named_children(&mut node.walk())
        .filter(|child| child.cached_kind() != "comment")
        .nth(entry)
}

fn unwrap_braced_value(mut value: Node) -> Option<Node> {
    while value.cached_kind() == "initializer_list" {
        value = single_argument(value)?;
    }
    Some(value)
}

fn single_argument(args: Node) -> Option<Node> {
    let mut cursor = args.walk();
    let mut args = args
        .named_children(&mut cursor)
        .filter(|n| n.cached_kind() != "comment");
    let arg = args.next()?;
    (args.next().is_none() && arg.cached_kind() != "parameter_pack_expansion").then_some(arg)
}

pub(super) fn iterator_result(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
) -> Option<IteratorResult> {
    if !ctx.is_cpp || node.cached_kind() != "call_expression" {
        return None;
    }
    let (recv, method, args) = member_call(source, node)?;
    let iterator = |op: &ContainerOperation| matches!(op, ContainerOperation::Iterator { method: name, .. } if name == method.as_ref());
    if !ctx
        .container_models
        .iter()
        .any(|model| model.operations.iter().any(iterator))
    {
        return None;
    }
    let container = Elements::for_receiver(program, ctx, source, recv)?;
    let class = container.class();
    let owner = member_lookup_owner(program, ctx, &container.instance);
    let arity = arguments(args)?.len();
    if !ctx
        .container_models
        .get(class)?
        .operations
        .iter()
        .any(|op| iterator(op) && op.arity() == arity)
        || !available(program, ctx, &owner, &method, arity)
    {
        return None;
    }
    let constant = receiver_is_const(program, ctx, source, recv, false) == Some(true);
    Some(IteratorResult {
        container,
        constant,
    })
}

fn iterator_member<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node<'t>,
) -> Option<Access<'t>> {
    let (receiver, member) = iterator_member_operand(ctx, source, node)?;
    let desc = receiver_desc(program, ctx, source, receiver)?;
    iterator_projection(program, ctx, receiver, Some(member), &desc)
}

pub(super) fn iterator_member_operand<'s, 't>(
    ctx: &LowerContext,
    source: &'s str,
    node: Node<'t>,
) -> Option<(Node<'t>, &'s str)> {
    if !ctx.is_cpp {
        return None;
    }
    let recv = node.cached_field("argument")?;
    let receiver = if is_arrow_access(node) {
        recv
    } else {
        deref_operand(source, peel_expression(recv))?
    };
    let member = node_text(source, &node.cached_field("field")?);
    if !ctx.container_models.iter().any(|model| {
        model.operations.iter().any(|op| {
            matches!(op, ContainerOperation::Iterator { member: Some(field), .. } if field == member)
        })
    }) {
        return None;
    }
    Some((peel_expression(receiver), member))
}

pub(super) fn iterator_projection<'t>(
    program: &Program,
    ctx: &LowerContext,
    receiver: Node<'t>,
    member: Option<&str>,
    desc: &TypeDesc,
) -> Option<Access<'t>> {
    let projects = |op: &ContainerOperation| matches!(op, ContainerOperation::Iterator { member: field, .. } if field.as_deref() == member);
    let TypeDesc::Struct { name, .. } = desc else {
        return None;
    };
    let (instance, nested) = name.rsplit_once("::")?;
    if !matches!(nested, "iterator" | "const_iterator") {
        return None;
    }
    let (class, _) = instance.split_once('<')?;
    if !ctx
        .container_models
        .get(class)?
        .operations
        .iter()
        .any(projects)
    {
        return None;
    }
    Some(Access::Iterator {
        receiver,
        element: program.types.template_element(instance)?.clone(),
        cv: program.types.template_element_cv(instance)?,
        read_only: member.is_none() || nested == "const_iterator",
    })
}

pub(super) fn access<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node<'t>,
) -> Option<Access<'t>> {
    if !ctx.is_cpp {
        return None;
    }
    let (recv, method, args) = match node.cached_kind() {
        "subscript_expression" => (
            node.cached_field("argument")?,
            Cow::Borrowed("operator[]"),
            node.cached_field("indices")?,
        ),
        "call_expression" => member_call(source, node)?,
        "field_expression" => return iterator_member(program, ctx, source, node),
        "pointer_expression" => {
            let receiver = peel_expression(deref_operand(source, node)?);
            // An iterator obtained through this model is either a named
            // binding or the result of a lookup. Ordinary chains of unary
            // pointer operators use the regular type walk.
            if !is_name(receiver) && receiver.cached_kind() != "call_expression" {
                return None;
            }
            let desc = if receiver.cached_kind() == "call_expression" {
                iterator_result(program, ctx, source, receiver)?.desc()
            } else {
                receiver_desc(program, ctx, source, receiver)?
            };
            return iterator_projection(program, ctx, receiver, None, &desc);
        }
        _ => return None,
    };
    let access = |op: &ContainerOperation| matches!(op, ContainerOperation::Access { method: name, .. } if name == method.as_ref());
    if !ctx
        .container_models
        .iter()
        .any(|model| model.operations.iter().any(access))
    {
        return None;
    }
    let elements = Elements::for_receiver(program, ctx, source, recv)?;
    let class = elements.class();
    let arity = arguments(args)?.len();
    let model = ctx.container_models.get(class)?;
    if !model
        .operations
        .iter()
        .any(|op| access(op) && op.arity() == arity)
    {
        return None;
    }
    let owner = member_lookup_owner(program, ctx, &elements.instance);
    available(program, ctx, &owner, &method, arity).then_some(Access::Direct(elements))
}

/// A standard-library class spelling with the inline namespaces libc++ and
/// the libstdc++ ABI insert (`std::__1::vector`, `std::__cxx11::list`)
/// removed. Only components directly under `std` are dropped, so a custom
/// namespace never gains standard-container semantics.
fn canonical_std_class(name: &str) -> String {
    let name = name.trim().trim_start_matches("::");
    let mut parts = name.split("::");
    let Some(first) = parts.next() else {
        return String::new();
    };
    let mut kept = vec![first];
    let under_std = first == "std";
    for part in parts {
        // Only inline namespaces directly under `std` are ABI tags; once a
        // real component follows, a later `__…` name is part of the class.
        if under_std && kept.len() == 1 && is_inline_std_namespace(part) {
            continue;
        }
        kept.push(part);
    }
    kept.join("::")
}

fn is_inline_std_namespace(part: &str) -> bool {
    matches!(part, "__1" | "__cxx11" | "__debug" | "__ndk1")
}

/// The range models whose iteration value is their stored element. A custom
/// `[[container]]` model's `element` index only names a template argument; it
/// does not promise that `begin()` yields that element, so range iteration
/// stays off unless the model names one of these standard iterable containers.
/// Vendor inline-namespace spellings and declaration aliases are normalized,
/// so a model registered as `std::__1::vector` or reached through an alias
/// still counts.
fn iterable_model(model: &ContainerModel) -> bool {
    let iterable = |class: &str| {
        matches!(
            canonical_std_class(class).as_str(),
            "std::vector"
                | "std::deque"
                | "std::list"
                | "std::forward_list"
                | "std::array"
                | "std::span"
                | "std::set"
                | "std::multiset"
                | "std::unordered_set"
                | "std::unordered_multiset"
        )
    };
    std::iter::once(model.name.as_str())
        .chain(model.declaration_aliases().iter().map(String::as_str))
        .any(iterable)
}

/// The element summary a `range-for` over `range` reads, or `None` when the
/// range is not an iterable modeled container. Resolve this before the loop
/// variable is declared so a declaration such as
/// `for (Callback callbacks : callbacks)` still names the outer container.
/// The caller (`range_source`) already filters ignored range macros.
pub(super) fn range_elements(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    range: Node,
) -> Option<Elements> {
    if !ctx.is_cpp {
        return None;
    }
    let elements = Elements::for_receiver(program, ctx, source, range)?;
    let model = ctx.container_models.get(elements.class())?;
    (model.element == 0 && iterable_model(model)).then_some(elements)
}

/// Connect a `range-for` variable to an already resolved element summary. The
/// variable receives what a read of the element cell yields, matching
/// `x = c[i]`; the load and copy are attributed to the range expression so a
/// value slice shows where the read happens.
pub(super) fn flow_range_element(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    range: Node,
    elements: Elements,
    var: VarId,
) {
    let element = elements.element.clone();
    lower_operation(program, ctx, source, range, |program, ctx| {
        let pointer = program.types.ptr_to(element.clone());
        let ty = program.types.intern(element);
        let address = alloc_load_temp(program, ctx, range, pointer);
        elements.address(program, address);
        let loaded = emit_load(program, ctx, range, address, ty);
        program.flow.push(FlowConstraint::Copy {
            dst: var,
            src: loaded,
        });
    });
}

/// Reuse the type computed by member-call lookup. Only a modeled type can
/// need the additional check that its expression names a supported owner.
pub(super) fn typed_call_result(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    call: Node,
    receiver: &TypeDesc,
) -> Option<CallResult> {
    let TypeDesc::Struct { name, .. } = receiver else {
        return None;
    };
    program.types.template_element(name)?;
    if let Some(access) = access(program, ctx, source, call) {
        return Some(CallResult::Qualified(
            access.element().clone(),
            access.cv(program),
        ));
    }
    iterator_result(program, ctx, source, call)
        .map(|iterator| CallResult::Decided(Some(iterator.desc())))
}

fn evaluated(ctx: &LowerContext, source: &str, mut node: Node) -> bool {
    while let Some(parent) = node.parent() {
        match parent.cached_kind() {
            "lambda_expression" | "function_definition" => break,
            "sizeof_expression" | "alignof_expression" | "decltype" | "requires_expression" => {
                return false
            }
            _ if unevaluated_operand(ctx.is_cpp, ctx.original_spelling(source), parent, node) => {
                return false
            }
            _ => node = parent,
        }
    }
    true
}

fn subscript_bracket<'t>(node: Node<'t>) -> Option<Node<'t>> {
    if node.cached_kind() != "subscript_expression" {
        return None;
    }
    let indices = node.cached_field("indices")?;
    let mut cursor = indices.walk();
    let token = indices
        .children(&mut cursor)
        .find(|token| token.cached_kind() == "[");
    token
}

fn subscript_occurrence<'t>(node: Node<'t>, ctx: &LowerContext) -> Option<Node<'t>> {
    ctx.line_map.as_ref()?;
    subscript_bracket(node).filter(|token| node_has_expansion_location(ctx, *token))
}

/// Whether a receiver chain reaches an earlier subscript through a field,
/// call or pointer wrapper. Such wrappers share the chain's expression start,
/// so an outer bracket needs its own occurrence anchor.
fn has_subscript_base(mut node: Node) -> bool {
    loop {
        node = peel_expression(node);
        node = match node.cached_kind() {
            "subscript_expression" => return true,
            "field_expression" | "pointer_expression" => {
                let Some(argument) = node.cached_field("argument") else {
                    return false;
                };
                argument
            }
            "call_expression" => {
                let Some(argument) = node
                    .cached_field("function")
                    .and_then(|function| function.cached_field("argument"))
                else {
                    return false;
                };
                argument
            }
            _ => return false,
        };
    }
}

pub(super) fn subscript_bracket_ignored(ctx: &LowerContext, node: Node) -> bool {
    subscript_occurrence(node, ctx).is_some_and(|token| node_is_from_ignored_macro(ctx, token))
}

/// Whether an ignored macro spells any bracket, accessor, or field token in a
/// receiver chain. Resolving a field's element owner by type would otherwise
/// bypass an ignored bracket or accessor in its base.
pub(super) fn receiver_chain_ignored(ctx: &LowerContext, source: &str, node: Node) -> bool {
    if ctx.ignored_macros.is_empty() {
        return false;
    }
    if node_is_from_ignored_macro(ctx, node) {
        return true;
    }
    let node = peel_expression(node);
    if node_is_from_ignored_macro(ctx, node) {
        return true;
    }
    match node.cached_kind() {
        // An explicit dereference forwards to the pointer expression that
        // spells the ignored bracket or accessor.
        "pointer_expression" => node
            .cached_field("argument")
            .is_some_and(|argument| receiver_chain_ignored(ctx, source, argument)),
        "subscript_expression" => {
            subscript_bracket_ignored(ctx, node)
                || node
                    .cached_field("argument")
                    .is_some_and(|base| receiver_chain_ignored(ctx, source, base))
        }
        "call_expression" => {
            method_is_from_ignored_macro(ctx, source, node)
                || node
                    .cached_field("function")
                    .and_then(|function| function.cached_field("argument"))
                    .or_else(|| {
                        node.has_error()
                            .then(|| explicit_member_operator_call(node, source))
                            .flatten()
                            .map(|op| op.receiver)
                    })
                    .is_some_and(|base| receiver_chain_ignored(ctx, source, base))
        }
        "field_expression" => {
            node.cached_field("field")
                .is_some_and(|field| node_is_from_ignored_macro(ctx, field))
                || node
                    .cached_field("argument")
                    .is_some_and(|base| receiver_chain_ignored(ctx, source, base))
        }
        _ => false,
    }
}

pub(super) fn effects_allowed(ctx: &LowerContext, source: &str, node: Node) -> bool {
    evaluated(ctx, source, node)
        && (ctx.ignored_macros.is_empty()
            || (!subscript_bracket_ignored(ctx, node)
                && !method_is_from_ignored_macro(ctx, source, node)))
}

fn braced_clause_count(node: Node) -> Option<usize> {
    (node.cached_kind() == "initializer_list").then(|| {
        node.named_children(&mut node.walk())
            .filter(|child| child.cached_kind() != "comment")
            .count()
    })
}

/// The brace of a typed `std::pair{...}` compound literal. A pair literal is
/// one map entry, never an outer initializer list of entries.
fn compound_pair_brace<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node<'t>,
) -> Option<Node<'t>> {
    let node = peel_expression(node);
    if node.cached_kind() != "compound_literal_expression" {
        return None;
    }
    let spelling = node
        .cached_field("type")
        .and_then(|ty| type_spelling_node(program, ctx, source, ty))?;
    let base = spelling
        .split_once('<')
        .map_or(spelling.as_str(), |(base, _)| base);
    let base = without_leading_cv(base);
    (canonical_std_class(base) == "std::pair").then_some(())?;
    node.cached_field("value")
        .filter(|value| value.cached_kind() == "initializer_list")
}

struct InsertionOptions<'a> {
    entry: Option<usize>,
    list_overload: bool,
    preserve_braces: bool,
    /// A map `insert` accepts one `{key, mapped}` pair argument; a map
    /// initializer list accepts only entries.
    allow_single_pair: bool,
    mapped: &'a TypeDesc,
    /// The map key's specialization spelling, for validating the key clause
    /// of a single-pair reading.
    key: Option<&'a str>,
}

fn make_pair_arguments<'t>(source: &str, node: Node<'t>) -> Option<[Node<'t>; 2]> {
    if node.cached_kind() != "call_expression" {
        return None;
    }
    let callee = node.cached_field("function")?;
    let name = normalize_qualified(node_text(source, &callee));
    if !matches!(
        name.trim_start_matches("::"),
        "std::make_pair" | "std::__1::make_pair"
    ) {
        return None;
    }
    arguments(node.cached_field("arguments")?)?.try_into().ok()
}

fn inserted_values<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    arg: Node<'t>,
    options: InsertionOptions<'_>,
    values: &mut Vec<StoredValue<'t>>,
) {
    let entry = options.entry;
    let list_overload = options.list_overload;
    let preserve_braces = options.preserve_braces;
    let key_spelling = options
        .key
        .map(|key| canonical_alias_spelling(program, ctx, key));
    let clause_braced_value = |node, position| {
        let node = compound_pair_brace(program, ctx, source, node).unwrap_or(node);
        braced_value(node, position)
    };
    let single_entry_viable = |candidate: Node| {
        preserve_braces
            && inner_container_viable(program, ctx, source, options.mapped, candidate) == Some(true)
    };
    let pair_entry = |clause, entry| {
        pair_clause_entry(
            program,
            ctx,
            source,
            clause,
            entry,
            key_spelling.as_deref(),
            options.mapped,
        )
    };
    let key_viable = |arg| {
        if entry.is_none() {
            return true;
        }
        let Some(key) = clause_braced_value(arg, Some(0)) else {
            return true;
        };
        key_braced_accepts(program, ctx, source, key, key_spelling.as_deref())
    };
    let list_entry = |clause, entry| {
        if braced_clause_count(clause) == Some(0) {
            return map_entry_can_default(program, key_spelling.as_deref(), options.mapped)
                .then_some(PairEntry::Unmodeled);
        }
        if let Some(mapped) = clause_braced_value(clause, Some(entry)) {
            return (key_viable(clause) && (!preserve_braces || single_entry_viable(mapped)))
                .then_some(PairEntry::Mapped(StoredValue::Expression(mapped)));
        }
        pair_entry(clause, entry)
    };
    let original = peel_expression(arg);
    if node_is_from_ignored_macro(ctx, original)
        || original
            .cached_field("type")
            .is_some_and(|ty| node_is_from_ignored_macro(ctx, ty))
    {
        return;
    }
    // Only an initializer_list compound literal collapses into the outer
    // list branch. A typed std::pair literal is always one entry, so its
    // brace stays separate for the single-pair reading below.
    let initializer_list_literal = preserve_braces
        && original.cached_kind() == "compound_literal_expression"
        && original
            .cached_field("type")
            .and_then(|ty| type_spelling_node(program, ctx, source, ty))
            .is_some_and(|spelling| {
                let base = spelling
                    .split_once('<')
                    .map_or(spelling.as_str(), |(base, _)| base);
                canonical_std_class(without_leading_cv(base)) == "std::initializer_list"
            });
    let arg = if initializer_list_literal {
        original
            .cached_field("value")
            .filter(|value| value.cached_kind() == "initializer_list")
            .unwrap_or(original)
    } else {
        original
    };
    let pair_brace = compound_pair_brace(program, ctx, source, original);
    if node_is_from_ignored_macro(ctx, arg) {
        return;
    }
    if list_overload && arg.cached_kind() == "initializer_list" {
        if let Some(entry) = entry {
            // Read each map clause independently: a mixed list may hold
            // braced entries and named or make_pair entries, and one bad
            // clause must not discard the others.
            let mut cursor = arg.walk();
            let entries: Vec<_> = arg
                .named_children(&mut cursor)
                .filter(|clause| {
                    clause.cached_kind() != "comment" && !node_is_from_ignored_macro(ctx, *clause)
                })
                .map(|clause| list_entry(clause, entry))
                .collect();
            // A `{key, mapped}` pair whose mapped initializer itself has two
            // clauses must be read as one pair unless every clause is a
            // viable map entry.
            let all_clauses_entries = !entries.is_empty() && entries.iter().all(Option::is_some);
            if options.allow_single_pair
                && !all_clauses_entries
                && braced_clause_count(arg) == Some(entry + 1)
                && key_viable(arg)
            {
                let mapped = braced_value(arg, Some(entry)).expect("mapped clause");
                if preserve_braces {
                    if single_entry_viable(mapped) {
                        values.push(StoredValue::Expression(mapped));
                    }
                    return;
                }
                // An empty brace value-initializes the mapped callback.
                if braced_clause_count(mapped) == Some(0) {
                    return;
                }
                if let Some(value) = unwrap_braced_value(mapped) {
                    values.push(StoredValue::Expression(value));
                    return;
                }
            }
            values.extend(
                entries
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| match entry {
                        PairEntry::Mapped(value) => Some(value),
                        PairEntry::Unmodeled => None,
                    }),
            );
            return;
        } else {
            // A position insert may initialize one inner container
            // (`insert(pos, {{1, alpha}, {2, beta}})`) or a list of inner
            // containers (`insert(pos, {inner1, inner2})`). Keep the whole
            // brace only when its clauses are not each inner containers.
            let mut cursor = arg.walk();
            let clauses: Vec<_> = arg
                .named_children(&mut cursor)
                .filter(|node| node.cached_kind() != "comment")
                .collect();
            let active: Vec<_> = clauses
                .iter()
                .filter(|clause| !node_is_from_ignored_macro(ctx, **clause))
                .collect();
            let all_clauses_inner =
                !active.is_empty() && active.iter().all(|clause| single_entry_viable(**clause));
            if preserve_braces && single_entry_viable(arg) && !all_clauses_inner {
                values.push(StoredValue::Expression(arg));
            } else {
                for clause in clauses {
                    if node_is_from_ignored_macro(ctx, clause) {
                        continue;
                    }
                    if preserve_braces {
                        values.push(StoredValue::Expression(clause));
                    } else if let Some(value) = unwrap_braced_value(clause) {
                        values.push(StoredValue::Expression(value));
                    }
                }
            }
            return;
        }
    }
    // A braced argument that initializes an inner container keeps its braces
    // so `store_values` can interpret it at the inner level. This also covers
    // the single-pair map spelling `insert({key, {alpha, beta}})`.
    if preserve_braces {
        let node = pair_brace.unwrap_or(arg);
        if let Some(value) = braced_value(node, entry) {
            if entry.is_none() || (key_viable(node) && single_entry_viable(value)) {
                values.push(StoredValue::Expression(value));
            }
            return;
        }
    }
    let node = entry.and(pair_brace).unwrap_or(arg);
    if let Some(value) = braced_value(node, entry).and_then(unwrap_braced_value) {
        // A multi-clause braced key cannot initialize a scalar/pointer key,
        // so this is not a single pair; do not read its mapped clause.
        if entry.is_none() || key_viable(node) {
            values.push(StoredValue::Expression(value));
        }
    } else if let Some(PairEntry::Mapped(value)) =
        entry.and_then(|position| pair_entry(arg, position))
    {
        values.push(value);
    }
}

pub(super) fn lower_insertion(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    call: Node,
    first_site: usize,
) {
    if !ctx.is_cpp {
        return;
    }
    let Some((recv, method, args)) = member_call(source, call) else {
        return;
    };
    let named_store = |op: &ContainerOperation| matches!(op, ContainerOperation::Store { method: name, .. } if name == method.as_ref());
    if !ctx
        .container_models
        .iter()
        .any(|model| model.operations.iter().any(named_store))
    {
        return;
    }
    let Some(elements) = Elements::for_receiver(program, ctx, source, recv) else {
        return;
    };
    let class = elements.class();
    let Some(model) = ctx.container_models.get(class) else {
        return;
    };
    let Some(args) = arguments(args) else {
        return;
    };
    let arity = args.len();
    if !evaluated(ctx, source, call)
        || node_is_from_ignored_macro(ctx, call)
        || node_is_from_ignored_macro(ctx, recv)
        || method_is_from_ignored_macro(ctx, source, call)
        || !model
            .operations
            .iter()
            .any(|op| named_store(op) && op.arity() == arity)
    {
        return;
    }
    let owner = member_lookup_owner(program, ctx, &elements.instance);
    let matched = program.symbols.call_sites[first_site..].iter().any(|site| {
        site.callee_fn_id.map_or_else(
            || {
                site.receiver_class.as_deref() == Some(owner.as_ref())
                    && site.callee_name.rsplit_once("::") == Some((owner.as_ref(), method.as_ref()))
                    && imported_member(program, ctx, &owner, &method, arity)
            },
            |id| accepts(program, id, &owner, &method, arity),
        )
    });
    if !matched {
        return;
    }
    let preserve_braces = elements.nested(program, ctx).is_some();
    let key = elements.map_key();
    let mut values = Vec::new();
    for op in model
        .operations
        .iter()
        .filter(|op| named_store(op) && op.arity() == arity)
    {
        if let ContainerOperation::Store { value, entry, .. } = op {
            inserted_values(
                program,
                ctx,
                source,
                args[*value],
                InsertionOptions {
                    entry: *entry,
                    list_overload: matches!(method.as_ref(), "insert" | "insert_after"),
                    preserve_braces,
                    allow_single_pair: true,
                    mapped: elements.element(),
                    key,
                },
                &mut values,
            );
        }
    }

    if !values.is_empty() {
        elements.store_values(program, ctx, source, call, values);
    }
}

pub(super) fn lower_subscript_call(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    node: Node,
    caller: FnId,
) {
    let Some(receiver) = node.cached_field("argument") else {
        return;
    };
    let Some(indices) = node.cached_field("indices") else {
        return;
    };
    let macro_token = subscript_occurrence(node, ctx);
    // Chained subscripts share their expression start, which would coalesce
    // their call records during merge. Anchor an intermediate receiver's site
    // at its own bracket so every level keeps a distinct source identity.
    let occurrence = macro_token
        .or_else(|| {
            has_subscript_base(receiver)
                .then(|| subscript_bracket(node))
                .flatten()
        })
        .unwrap_or(node);
    let Some(Access::Direct(elements)) = access(program, ctx, source, node) else {
        return;
    };
    if node_is_from_ignored_macro(ctx, node)
        || (occurrence != node && node_is_from_ignored_macro(ctx, occurrence))
    {
        return;
    }
    let Some(indices) = arguments(indices) else {
        return;
    };
    let kind = trace_ir::MethodKind::Named("operator[]".into());
    let span = node_call_span(program, ctx, occurrence);
    let expansion = node_expansion_span(program, ctx, occurrence);
    let receiver = Receiver::named(program, ctx, source, receiver);
    let first_site = program.symbols.call_sites.len();
    let first_flow = program.flow.len();
    let first_pending = ctx.pending.borrow().len();
    let unevaluated = !evaluated(ctx, source, node);
    let args = if unevaluated {
        CallArgs {
            argc: indices.len() as u32,
            arg_desc: indices
                .iter()
                .map(|&arg| known_arg_type(program, ctx, source, arg))
                .collect(),
            null_constants: null_constant_args(source, &indices),
            ..CallArgs::empty()
        }
    } else {
        collect_call_arg_nodes(program, ctx, source, indices)
    };
    emit_member_sites(
        program,
        ctx,
        caller,
        &elements.instance,
        &kind,
        receiver,
        args,
        span,
        expansion,
        None,
    );
    for site in &mut program.symbols.call_sites[first_site..] {
        site.unevaluated |= unevaluated;
    }
    let expression = operation_expression(ctx, source, node);
    let operation_span = node_span(program, ctx, node);
    record_call_expressions(program, first_site, &expression);
    if let Some(token) = macro_token {
        record_call_occurrence(program, ctx, first_site, token);
    }
    record_operation_origins(
        program,
        ctx,
        first_flow,
        first_pending,
        operation_span,
        &expression,
    );
}

pub(super) fn lower_initializer(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    var: VarId,
    init: Node,
) -> bool {
    if !is_list_initializer(ctx, init) {
        return false;
    }
    let Some(elements) = Elements::for_var(program, ctx, var) else {
        return false;
    };
    lower_list_initializer(program, ctx, source, elements, init)
}

pub(super) fn lower_field_initializer(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    object: &InitializedObject,
    init: Node,
) {
    if !is_list_initializer(ctx, init) {
        return;
    }
    let Some((class, name)) = &object.member else {
        return;
    };
    let Some(owner) = program.types.class_type_id(class) else {
        return;
    };
    if program.types.field_is_reference(class, name) {
        return;
    }
    if let Some(elements) = Elements::for_field(program, ctx, owner, name, &object.desc) {
        lower_list_initializer(program, ctx, source, elements, init);
    }
}

fn is_list_initializer(ctx: &LowerContext, init: Node) -> bool {
    ctx.is_cpp && matches!(init.cached_kind(), "initializer_list" | "argument_list")
}

fn lower_list_initializer(
    program: &mut Program,
    ctx: &mut LowerContext,
    source: &str,
    elements: Elements,
    init: Node,
) -> bool {
    let class = elements.class();
    let Some(model) = ctx.container_models.get(class) else {
        return false;
    };
    if !model.initializer_list {
        return false;
    }
    let owner = member_lookup_owner(program, ctx, &elements.instance);
    let Some(list) = list_initialization(
        program,
        ctx,
        source,
        &elements,
        model,
        init,
        |arity, value_initialized_allocator| {
            list_constructor_accepts(
                program,
                ctx,
                &owner,
                model,
                arity,
                value_initialized_allocator,
            )
        },
    ) else {
        return false;
    };
    let node = list.values;
    if !node_is_from_ignored_macro(ctx, init) && !node_is_from_ignored_macro(ctx, node) {
        let entry = model.initializer_entry;
        // A nested container's braced entry describes that inner container,
        // so keep the braces for `store_values` to interpret at the inner
        // level instead of flattening single-element lists.
        let preserve_braces = elements.nested(program, ctx).is_some();
        let mut values = Vec::new();
        if let Some(entry) = entry {
            inserted_values(
                program,
                ctx,
                source,
                node,
                InsertionOptions {
                    entry: Some(entry),
                    list_overload: true,
                    preserve_braces,
                    allow_single_pair: false,
                    mapped: elements.element(),
                    key: elements.map_key(),
                },
                &mut values,
            );
        } else {
            let mut cursor = node.walk();
            for clause in node
                .named_children(&mut cursor)
                .filter(|n| n.cached_kind() != "comment")
            {
                if node_is_from_ignored_macro(ctx, clause) {
                    continue;
                }
                let value = if preserve_braces {
                    clause
                } else {
                    let Some(value) = unwrap_braced_value(clause) else {
                        continue;
                    };
                    value
                };
                values.push(StoredValue::Expression(value));
            }
        }
        if !values.is_empty() {
            lower_operation(program, ctx, source, node, |program, ctx| {
                elements.store_values(program, ctx, source, node, values);
            });
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_type_spelling_is_not_a_builtin() {
        let mut qualifiers = TypeQualifiers::default();
        for raw in ["", " \t\n"] {
            assert_eq!(builtin_type_spelling(raw, &mut qualifiers), None);
        }
        assert_eq!(
            builtin_type_spelling("unsigned", &mut qualifiers).as_deref(),
            Some("unsigned int")
        );
    }

    fn container(name: &str, aliases: Option<&[&str]>) -> ContainerModel {
        ContainerModel {
            name: name.to_owned(),
            declaration_aliases: aliases
                .map(|aliases| aliases.iter().map(|alias| (*alias).to_owned()).collect()),
            element: 0,
            initializer_list: false,
            initializer_entry: None,
            initializer_allocator: None,
            operations: Vec::new(),
        }
    }

    #[test]
    fn iterable_models_normalize_vendor_inline_namespaces() {
        for name in [
            "std::vector",
            "::std::vector",
            "std::__1::vector",
            "std::__debug::vector",
            "std::__cxx11::list",
            "std::__ndk1::deque",
            "std::__1::__debug::vector",
        ] {
            assert!(iterable_model(&container(name, None)), "{name}");
        }
        assert!(!iterable_model(&container("std::queue", None)));
        // Only inline namespaces directly under `std` are dropped.
        assert!(!iterable_model(&container("app::__1::vector", None)));
        assert!(!iterable_model(&container("std::__detail::vector", None)));
        assert!(!iterable_model(&container("std::vector::__1", None)));
        assert!(!iterable_model(&container("std::foo::__1::vector", None)));
    }

    #[test]
    fn iterable_models_accept_a_vendor_alias() {
        assert!(iterable_model(&container(
            "app::Cache",
            Some(&["std::__1::set"])
        )));
        assert!(!iterable_model(&container(
            "app::Cache",
            Some(&["std::__1::map"])
        )));
    }
}
