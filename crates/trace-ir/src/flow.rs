use crate::{FieldId, FnId, TypeId, VarId};
use std::sync::Arc;

/// Abstract value returned from a function body (may-analysis: union all return sites).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReturnFlow {
    AddrOfVar {
        src: VarId,
    },
    AddrOfFn {
        callee: FnId,
    },
    Copy {
        src: VarId,
    },
    /// Return value is whatever `callee` returns (expanded after all TUs are merged).
    Call {
        callee_name: String,
        /// The returned call's own value destination for call-site field models.
        result: VarId,
    },
    /// Address of a returned reference's original lvalue. Expanded only by
    /// address consumers; value consumers keep the ordinary return facts.
    LvalueAddress(Box<ReturnFlow>),
}

/// One argument a `std::bind` holds for the callable it wraps
/// ([`FlowConstraint::BindCallable`]; docs/IR.md, "Bound callables").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundArg {
    /// A value the binder keeps, passed to the callable as it was written.
    Stored(VarId),
    /// A named function passed as a stored argument.
    Function(FnId),
    /// `std::placeholders::_n`, which the invocation's argument `n` fills
    /// (`n` is 1-based, as it is spelled).
    Placeholder(u32),
    /// An argument the site records no value for (a literal): it holds its
    /// position in the callable's parameters but supplies no value.
    Opaque,
}

/// Storage identity for a container with index-insensitive element flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContainerOwner {
    Variable(VarId),
    Field { class: TypeId, name: Arc<str> },
}

/// Statement-level flow facts extracted during IR lowering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlowConstraint {
    /// `dst = src` (pointer assignment)
    Copy { dst: VarId, src: VarId },
    /// `dst = &var`
    AddrOfVar { dst: VarId, src: VarId },
    /// `dst = &function`
    AddrOfFn { dst: VarId, callee: FnId },
    /// `dst = *src`
    Load { dst: VarId, src: VarId },
    /// `*dst = src` (store through pointer)
    Store { dst: VarId, src: VarId },
    /// `dst = src->field` or struct field address.
    /// `field_name` carries the human-readable name resolved during lowering;
    /// the solver uses it to reject pointees whose struct type has a different
    /// field at the same positional index.
    GepField {
        dst: VarId,
        base: VarId,
        field: FieldId,
        field_name: String,
    },
    /// `dst` points to the owner's summary cell for elements of `element` type.
    ContainerElement {
        dst: VarId,
        owner: ContainerOwner,
        element: TypeId,
    },
    /// Function-pointer array initializer: any subscript may target any listed callee.
    ArrayFnMember { array: VarId, callee: FnId },
    /// `dst = callee()` — callee resolved by name at PAG build in the scope
    /// of `caller`, the function the call is written in (`None` for a
    /// file-scope initializer). See `docs/IR.md`, "Return-value flow".
    CallReturn {
        dst: VarId,
        callee_name: String,
        caller: Option<FnId>,
    },
    /// Return from an exact, already-resolved function (not name-only lookup).
    /// Used for selected free operators and after merge when a file-scope
    /// initializer has no CallSite.
    CallReturnResolved { dst: VarId, callee: FnId },
    /// Address of a reference returned by a direct file-scope getter.
    /// Resolved after merge in the destination's file and link target.
    CallReferenceReturn { dst: VarId, callee_name: String },
    /// Address returned by a selected free/member declaration at file scope.
    /// Match definitions by signature and destination scope after merge,
    /// then bind the retained explicit arguments to their parameters.
    CallReferenceReturnDeclared {
        dst: VarId,
        callee: FnId,
        args: Vec<(u32, VarId)>,
    },
    /// `dst = callee_var()` — callee resolved at analysis time from the
    /// function-pointer variable's points-to set (indirect / virtual calls).
    CallReturnIndirect { dst: VarId, callee_var: VarId },
    /// `dst = new T(...)` — allocate a fresh heap location and point `dst` to
    /// it, so the constructor's implicit `this` has concrete pointees.
    NewHeap { dst: VarId },
    /// `dst` points at the given string literal (interned; copies propagate).
    StringConst { dst: VarId, value: String },
    /// Explicit null pointer value. No abstract location or pointee is created.
    NullPointer { dst: VarId },
    /// Step through a smart pointer's overloaded `->`/`*` or modeled `get`.
    /// `src` holds the wrapper value; `dst` is a pointee-typed receiver or
    /// raw pointer, whose pointee type filters the admitted locations. See
    /// `docs/IR.md`, "Smart-pointer unwrap".
    UnwrapPointer { dst: VarId, src: VarId },
    /// `dst` holds the callable `std::bind` built: `callee`, called on
    /// `receiver` (a member's implicit `this`; `None` for a free or static
    /// function) with `args` bound to its parameters in order. The bind
    /// itself runs nothing; the callable runs where `dst` is invoked. See
    /// `docs/IR.md`, "Bound callables".
    BindCallable {
        dst: VarId,
        callee: FnId,
        receiver: Option<BoundArg>,
        args: Vec<BoundArg>,
    },
}

impl FlowConstraint {
    /// Every variable the constraint names, destination first.
    pub fn vars(&self) -> impl Iterator<Item = VarId> + '_ {
        let (first, second) = match *self {
            FlowConstraint::Copy { dst, src }
            | FlowConstraint::Load { dst, src }
            | FlowConstraint::Store { dst, src }
            | FlowConstraint::AddrOfVar { dst, src }
            | FlowConstraint::UnwrapPointer { dst, src } => (dst, Some(src)),
            FlowConstraint::GepField { dst, base, .. } => (dst, Some(base)),
            FlowConstraint::ContainerElement { dst, ref owner, .. } => match owner {
                ContainerOwner::Variable(var) => (dst, Some(*var)),
                ContainerOwner::Field { .. } => (dst, None),
            },
            FlowConstraint::CallReturnIndirect { dst, callee_var } => (dst, Some(callee_var)),
            FlowConstraint::ArrayFnMember { array, .. } => (array, None),
            FlowConstraint::AddrOfFn { dst, .. }
            | FlowConstraint::CallReturn { dst, .. }
            | FlowConstraint::CallReturnResolved { dst, .. }
            | FlowConstraint::CallReferenceReturn { dst, .. }
            | FlowConstraint::CallReferenceReturnDeclared { dst, .. }
            | FlowConstraint::BindCallable { dst, .. }
            | FlowConstraint::NewHeap { dst }
            | FlowConstraint::StringConst { dst, .. }
            | FlowConstraint::NullPointer { dst } => (dst, None),
        };
        let arguments = match self {
            FlowConstraint::CallReferenceReturnDeclared { args, .. } => args.as_slice(),
            _ => &[],
        };
        std::iter::once(first)
            .chain(second)
            .chain(arguments.iter().map(|(_, var)| *var))
            .chain(self.held_vars())
    }

    /// The variables this constraint holds besides its operands: a bound
    /// callable's receiver and stored arguments, which are values of the
    /// function the bind is written in. Empty for every other kind.
    pub fn held_vars(&self) -> impl Iterator<Item = VarId> + '_ {
        let receiver = match self {
            FlowConstraint::BindCallable { receiver, .. } => *receiver,
            _ => None,
        };
        let args: &[BoundArg] = match self {
            FlowConstraint::BindCallable { args, .. } => args,
            _ => &[],
        };
        receiver
            .into_iter()
            .chain(args.iter().copied())
            .filter_map(|arg| match arg {
                BoundArg::Stored(var) => Some(var),
                BoundArg::Function(_) | BoundArg::Placeholder(_) | BoundArg::Opaque => None,
            })
    }
}

impl ReturnFlow {
    /// The variable the return names, if any.
    pub fn var(&self) -> Option<VarId> {
        match *self {
            ReturnFlow::AddrOfVar { src } | ReturnFlow::Copy { src } => Some(src),
            ReturnFlow::LvalueAddress(ref flow) => flow.var(),
            ReturnFlow::Call { result, .. } => Some(result),
            ReturnFlow::AddrOfFn { .. } => None,
        }
    }
}
