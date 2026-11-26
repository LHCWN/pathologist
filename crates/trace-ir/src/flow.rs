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

/// Storage identity for a container with index-insensitive element flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContainerOwner {
    Variable(VarId),
    Field { class: TypeId, name: Arc<str> },
}

/// Overload selection retained for a direct allocation return.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallReturnBinding {
    Occurrence(crate::CallOccurrence),
    InitializerDeclarations(Vec<InitializerCall>),
}

/// A selected file-scope declaration and the arguments lowered for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InitializerCall {
    pub callee: FnId,
    pub args: Vec<(u32, VarId)>,
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
        binding: Option<Box<CallReturnBinding>>,
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
            | FlowConstraint::NewHeap { dst }
            | FlowConstraint::StringConst { dst, .. }
            | FlowConstraint::NullPointer { dst } => (dst, None),
        };
        std::iter::once(first).chain(second).chain(
            self.initializer_calls()
                .flat_map(|(_, args)| args.iter().map(|&(_, var)| var)),
        )
    }

    /// Selected initializer declarations and the actuals associated with each.
    pub fn initializer_calls(&self) -> impl Iterator<Item = (FnId, &[(u32, VarId)])> + '_ {
        let (reference, value) = match self {
            Self::CallReferenceReturnDeclared { callee, args, .. } => {
                (Some((*callee, args.as_slice())), &[][..])
            }
            Self::CallReturn { binding, .. } => match binding.as_deref() {
                Some(CallReturnBinding::InitializerDeclarations(calls)) => (None, calls.as_slice()),
                _ => (None, &[][..]),
            },
            _ => (None, &[][..]),
        };
        reference
            .into_iter()
            .chain(value.iter().map(|call| (call.callee, call.args.as_slice())))
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
