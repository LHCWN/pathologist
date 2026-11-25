use super::*;
use std::ops::RangeInclusive;
use std::sync::{mpsc, OnceLock};
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
    if !model.initializer_list || copies_container(program, ctx, source, init, class) {
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
    let viable = |values, argc, value_initialized_allocator| {
        (accepts(argc, value_initialized_allocator)
            && !copies_container(program, ctx, source, values, elements.class()))
        .then_some(())?;
        let uncertain = list_element_conversions(
            program,
            ctx,
            source,
            values,
            model.initializer_entry,
            &elements.element,
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
                    // C++ first tries the entire outer list as one argument.
                    // For example, {{alpha}, {}} holds alpha and a null callback.
                    return viable(init, 1, false).or_else(|| viable(values, 2, true));
                }
                if allocator_argument(program, ctx, source, elements, model, allocator) {
                    return viable(values, 2, false).or_else(|| viable(init, 1, false));
                }
            }
            viable(init, 1, false)
        }
        "argument_list" => {
            let args = arguments(init)?;
            let values = *args.first()?;
            (values.cached_kind() == "initializer_list").then_some(())?;
            viable(values, args.len(), false)
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
            .and_then(|ty| type_spelling_for(program, ctx, source, ty, ty, 0))
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
        trace_ir::split_instance(&elements.instance)
            .is_some_and(|(_, args)| args.get(position) == Some(&spelling.as_str()))
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

/// `None` for an incompatible clause, `Some(true)` for a conversion whose
/// viability is unknown, and `Some(false)` for a viable list.
fn list_element_conversions(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    init: Node,
    entry: Option<usize>,
    element: &TypeDesc,
) -> Option<bool> {
    // The modeled elements are pointers or callables. Arithmetic cannot
    // initialize one, except for a null pointer constant. Keep clauses of
    // uncertain type, including class conversions and callable expressions.
    let mut uncertain = false;
    let viable = init
        .named_children(&mut init.walk())
        .filter(|value| value.cached_kind() != "comment")
        .map(|value| braced_value(value, entry).unwrap_or(value))
        .all(|mut value| {
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
    class: &str,
) -> bool {
    single_argument(init).is_some_and(|value| {
        receiver_desc(program, ctx, source, value)
            .as_ref()
            .and_then(class_name_of_desc)
            .as_deref()
            == Some(class)
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
    if arg.cached_kind() != "type_descriptor"
        || spelling_is_dependent(ctx, source, arg, node_text(source, &arg), Spelling::Source)
    {
        return None;
    }
    let decl = arg.cached_field("declarator");
    if decl.is_some_and(declares_reference) {
        return None;
    }
    let base = type_desc_from_node(program, ctx, source, arg);
    let element = abstract_declarator_shape(base, decl, true, ReferenceLayer::Pointer);
    let callable = matches!(element, TypeDesc::FnPtr { .. })
        && is_callable_wrapper(&normalize_qualified(node_text(
            source,
            &arg.cached_field("type")?,
        )));
    if !callable && !matches!(element, TypeDesc::Ptr(_)) {
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
    let cv = spelled_type_cv(program, ctx, node_text(source, &arg));
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
    type_spelling_for(program, ctx, source, node, node, 0)
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
    if !second.is_pointer_like() {
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
    ctx: &LowerContext,
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

/// Different pair specializations have separate field summaries. Copy only
/// the corresponding pointer/callable slots through the usual field cells.
pub(super) fn copy_pair_fields(
    program: &mut Program,
    ctx: &LowerContext,
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
    for name in ["first", "second"] {
        let Some(((dst_field, dst_ty), (src_field, src_ty))) =
            field_in_pair(program, dst_pair, name).zip(field_in_pair(program, src_pair, name))
        else {
            continue;
        };
        let dst_desc = program.types.get(dst_ty).desc.as_ref();
        let src_desc = program.types.get(src_ty).desc.as_ref();
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
    }
    bases.is_some()
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
        return type_spelling_for(program, ctx, source, node, ty, depth)
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

fn complete_template_arguments(
    program: &Program,
    ctx: &LowerContext,
    class: &str,
    args: &mut Vec<String>,
) {
    let defaults = program
        .types
        .container_template_defaults(class)
        .or_else(|| {
            if program.types.is_struct_declared(class) {
                return None;
            }
            canonical_declarations(ctx)?.defaults.get(class)
        });
    if let Some(defaults) = defaults {
        while let Some(arg) = defaults
            .get(&args.len())
            .and_then(|default| default.bind(args))
        {
            args.push(arg);
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
            type_spelling_at(program, ctx, source, arg, depth)
                .unwrap_or_else(|| normalize_spacing(node_text(source, &arg)))
        })
        .collect()
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
    type_spelling_at(program, ctx, source, node, 0)
}

fn type_spelling_at(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    depth: u32,
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
    )
}

fn type_spelling_for(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    node: Node,
    ty: Node,
    depth: u32,
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
        if let Some((pointee, tail)) = alias.rsplit_once('*') {
            // A qualifier on a pointer alias decorates the outer pointer.
            let mut pointer = qualifiers;
            if pointer.take_prefix(tail).is_empty() {
                qualifiers = TypeQualifiers::default();
                format!("{pointee}*{}", pointer.spelling())
            } else if alias.contains('(')
                && (!after.is_empty() || !qualifiers.spelling().is_empty())
            {
                format!("({alias})")
            } else {
                alias.to_owned()
            }
        } else if alias.contains('(') && !after.is_empty() {
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
            if ctx.container_models.get(&class).is_some() {
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
                if let Some(spelling) = type_spelling_at(program, ctx, source, param, depth + 1) {
                    edits.push((param.byte_range(), spelling));
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

pub(super) struct Elements {
    owner: ContainerOwner,
    instance: String,
    element: TypeDesc,
}

enum StoredValue<'t> {
    Expression(Node<'t>),
    PairSecond(Node<'t>, VarId, FieldId, TypeId),
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
        if ctx.reference_vars.contains(&var) {
            return None;
        }
        let ty = program.symbols.variable(var).type_id;
        Self::new(
            program,
            ContainerOwner::Variable(var),
            program.types.get(ty).desc.as_ref(),
        )
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
                if let Some(var) = static_member_access(program, ctx, source, recv) {
                    return Self::for_var(program, ctx, var);
                }
                let base = recv.cached_field("argument")?;
                let name = node_text(source, &recv.cached_field("field")?);
                let arrow = is_arrow_access(recv);
                let field = (!arrow)
                    .then(|| subscript_field(program, ctx, source, base, name))
                    .flatten()
                    .or_else(|| {
                        let cls = member_receiver_class(program, ctx, source, base, arrow)?;
                        lookup_field_in_hierarchy(program, Some(ctx), &cls, name)
                    })?;
                (field, name)
            }
            _ => return None,
        };
        Self::for_field(
            program,
            field.owner_type_id,
            name,
            program.types.get(field.type_id).desc.as_ref(),
        )
    }

    fn for_field(
        program: &Program,
        class: trace_ir::TypeId,
        name: &str,
        desc: &TypeDesc,
    ) -> Option<Self> {
        Self::new(
            program,
            ContainerOwner::Field {
                class,
                name: name.into(),
            },
            desc,
        )
    }

    fn new(program: &Program, owner: ContainerOwner, desc: &TypeDesc) -> Option<Self> {
        let TypeDesc::Struct { name, .. } = desc else {
            return None;
        };
        let element = program.types.template_element(name)?.clone();
        name.split_once('<')?;
        Some(Self {
            owner,
            instance: name.clone(),
            element,
        })
    }

    fn class(&self) -> &str {
        self.instance
            .split_once('<')
            .expect("container specialization")
            .0
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

    fn address(self, program: &mut Program, dst: VarId) -> trace_ir::TypeId {
        let element = program.types.intern(self.element);
        program.flow.push(FlowConstraint::ContainerElement {
            dst,
            owner: self.owner,
            element,
        });
        element
    }

    fn store<'t>(
        self,
        program: &mut Program,
        ctx: &mut LowerContext,
        source: &str,
        at: Node,
        values: impl IntoIterator<Item = Node<'t>>,
    ) {
        self.store_values(
            program,
            ctx,
            source,
            at,
            values.into_iter().map(StoredValue::Expression),
        );
    }

    fn store_values<'t>(
        self,
        program: &mut Program,
        ctx: &mut LowerContext,
        source: &str,
        at: Node,
        values: impl IntoIterator<Item = StoredValue<'t>>,
    ) {
        let ty = program.types.ptr_to(self.element.clone());
        let address = alloc_load_temp(program, ctx, at, ty);
        let element = self.address(program, address);
        for stored in values {
            let value = match stored {
                StoredValue::Expression(value) | StoredValue::PairSecond(value, ..) => value,
            };
            if node_is_from_ignored_macro(ctx, value) {
                continue;
            }
            lower_operation(program, ctx, source, value, |program, ctx| {
                if let StoredValue::PairSecond(_, var, field, ty) = stored {
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

fn subscript_occurrence<'t>(node: Node<'t>, ctx: &LowerContext) -> Option<Node<'t>> {
    if ctx.line_map.is_none() || node.cached_kind() != "subscript_expression" {
        return None;
    }
    let indices = node.cached_field("indices")?;
    let mut cursor = indices.walk();
    let token = indices
        .children(&mut cursor)
        .find(|token| token.cached_kind() == "[" && node_has_expansion_location(ctx, *token));
    token
}

pub(super) fn effects_allowed(ctx: &LowerContext, source: &str, node: Node) -> bool {
    evaluated(ctx, source, node)
        && (ctx.ignored_macros.is_empty()
            || !subscript_occurrence(node, ctx)
                .is_some_and(|token| node_is_from_ignored_macro(ctx, token)))
}

fn inserted_values<'t>(
    program: &Program,
    ctx: &LowerContext,
    source: &str,
    arg: Node<'t>,
    entry: Option<usize>,
    list_overload: bool,
    values: &mut Vec<StoredValue<'t>>,
) {
    let arg = peel_expression(arg);
    if node_is_from_ignored_macro(ctx, arg) {
        return;
    }
    if list_overload && arg.cached_kind() == "initializer_list" {
        let mut cursor = arg.walk();
        // A map initializer list is a list of entries. Treat the argument
        // as one pair when its clauses are the key and the mapped value.
        if entry.is_none()
            || arg
                .named_children(&mut cursor)
                .filter(|node| node.cached_kind() != "comment")
                .all(|clause| braced_value(clause, entry).is_some())
        {
            values.extend(
                arg.named_children(&mut cursor)
                    .filter(|node| node.cached_kind() != "comment")
                    .filter_map(|clause| {
                        unwrap_braced_value(braced_value(clause, entry)?)
                            .map(StoredValue::Expression)
                    }),
            );
            return;
        }
    }
    if let Some(value) = braced_value(arg, entry).and_then(unwrap_braced_value) {
        values.push(StoredValue::Expression(value));
    } else if let Some(position) = entry {
        if arg.cached_kind() == "call_expression" {
            if let Some(callee) = arg.cached_field("function") {
                let name = normalize_qualified(node_text(source, &callee));
                if matches!(
                    name.trim_start_matches("::"),
                    "std::make_pair" | "std::__1::make_pair"
                ) {
                    if let Some([first, second]) = arg
                        .cached_field("arguments")
                        .and_then(arguments)
                        .and_then(|args| <[Node; 2]>::try_from(args).ok())
                    {
                        values.extend(
                            [first, second]
                                .get(position)
                                .copied()
                                .map(StoredValue::Expression),
                        );
                    }
                }
            }
        } else if position == 1 && is_name(arg) {
            if let Some(var) = lookup_var_node(program, ctx, source, arg) {
                if let Some((field, ty)) = pair_field(program, var, "second") {
                    values.push(StoredValue::PairSecond(arg, var, field, ty));
                }
            }
        }
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
        || node_is_from_ignored_macro(ctx, recv)
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
                *entry,
                matches!(method.as_ref(), "insert" | "insert_after"),
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
    let occurrence = macro_token.unwrap_or(node);
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
    if let Some(elements) = Elements::for_field(program, owner, name, &object.desc) {
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
            if program.types.is_struct_defined(&owner) {
                list_constructors(program, &owner, model)
                    .any(|ctor| ctor.accepts(arity, value_initialized_allocator))
            } else {
                canonical_declarations(ctx)
                    .and_then(|decls| decls.list_constructors.get(owner.as_ref()))
                    .is_some_and(|ctors| {
                        ctors
                            .iter()
                            .any(|ctor| ctor.accepts(arity, value_initialized_allocator))
                    })
            }
        },
    ) else {
        return false;
    };
    let node = list.values;
    if !node_is_from_ignored_macro(ctx, init) && !node_is_from_ignored_macro(ctx, node) {
        let entry = model.initializer_entry;
        let mut cursor = node.walk();
        let mut values = node
            .named_children(&mut cursor)
            .filter(|n| n.cached_kind() != "comment")
            .filter_map(|value| unwrap_braced_value(braced_value(value, entry)?))
            .peekable();
        if values.peek().is_some() {
            lower_operation(program, ctx, source, node, |program, ctx| {
                elements.store(program, ctx, source, node, values);
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
