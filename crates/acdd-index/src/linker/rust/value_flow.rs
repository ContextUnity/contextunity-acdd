//! Rust-only wrapper and reference projection rules used by the shared reducer.

use super::super::value_flow::resolve_type;
use super::super::value_flow::types::{
    import_path, node_position, ConfiguredReceiverAuthority, ReadTrace, RustWrapperKind,
    SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowLookup,
};
use crate::{
    models::Node,
    semantic::{SourcePosition, TypeExpr, ValueExpr},
};
use hashbrown::HashMap;

pub(in crate::linker) fn deserializer_result<'a>(
    owner: &'a Node,
    callee: &str,
    payload: &TypeExpr,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
    limits: SemanticLimits,
) -> Option<TypeTarget<'a>> {
    let callee = callee.replace("::", ".");
    let origin = if let Some((base, member)) = callee.rsplit_once('.') {
        let Symbol::Type(TypeTarget::External { module, .. }) =
            resolver.resolve_symbol(owner, base, at, SymbolRole::Type)
        else {
            return None;
        };
        format!("{}.{member}", module.replace("::", "."))
    } else {
        let Symbol::Type(TypeTarget::External { module, .. }) =
            resolver.resolve_symbol(owner, &callee, at, SymbolRole::Callable)
        else {
            return None;
        };
        module.replace("::", ".")
    };
    let (package, member) = origin.rsplit_once('.')?;
    if !matches!(
        (package, member),
        (
            "serde_json" | "serde_yaml",
            "from_str" | "from_slice" | "from_reader"
        ) | ("serde_json", "from_value")
            | ("toml", "from_str")
    ) {
        return None;
    }
    let authority = resolver.resolve_member_result(
        &TypeTarget::External {
            module: package.to_owned(),
            import_line: 0,
        },
        member,
        owner,
        at,
    )?;
    if !matches!(
        authority,
        TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ..
        }
    ) && !receiver_matches(
        &authority,
        &["Result", "std::result::Result", "core::result::Result"],
    ) {
        return None;
    }
    let payload = match payload {
        TypeExpr::RustCallArguments { args } => match (member, args.as_slice()) {
            ("from_reader", [_, payload]) => payload,
            ("from_str" | "from_slice" | "from_value", [payload]) => payload,
            _ => &TypeExpr::Unknown,
        },
        _ => &TypeExpr::Unknown,
    };
    Some(TypeTarget::RustWrapper {
        wrapper: RustWrapperKind::Result,
        ok: Box::new(resolve_type(owner, payload, at, resolver, limits)),
        err: Some(Box::new(TypeTarget::External {
            module: format!("{package}::Error"),
            import_line: 0,
        })),
    })
}

fn synth_return_target<'a>(ret: &str, self_target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
    match ret {
        "Self" => Some(self_target.clone()),
        "Result<Self>" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(self_target.clone()),
            err: None,
        }),
        "Option<Self>" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(self_target.clone()),
            err: None,
        }),
        "str" | "String" | "bool" | "usize" => Some(TypeTarget::Builtin(ret.to_owned())),
        _ => None,
    }
}

fn string_utf8_result(member: &str, owned: bool) -> Option<TypeTarget<'static>> {
    match member {
        "from_utf8" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(if owned {
                TypeTarget::Builtin("String".to_owned())
            } else {
                reference(&TypeTarget::Builtin("str".to_owned()), false)
            }),
            err: Some(Box::new(TypeTarget::External {
                module: if owned {
                    "std::string::FromUtf8Error".to_owned()
                } else {
                    "std::str::Utf8Error".to_owned()
                },
                import_line: 0,
            })),
        }),
        "from_utf8_lossy" => Some(TypeTarget::Builtin("String".to_owned())),
        "from_utf8_unchecked" => Some(if owned {
            TypeTarget::Builtin("String".to_owned())
        } else {
            reference(&TypeTarget::Builtin("str".to_owned()), false)
        }),
        _ => None,
    }
}

fn constructor_local_result<'a>(
    index: &(impl ValueFlowLookup<'a> + ?Sized),
    target: &TypeTarget<'a>,
    (member, callee_clean): (&str, &str),
    owner: &'a Node,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
    trace: Option<&ReadTrace<'a, '_>>,
) -> Option<TypeTarget<'a>> {
    let TypeTarget::Local(node) = target else {
        return None;
    };
    if let Some(result) = resolver.resolve_member_result(target, member, owner, at) {
        if !matches!(result, TypeTarget::Unknown) {
            return Some(result);
        }
    }
    let callable = match resolver.resolve_associated_member(target, member, owner, at) {
        Symbol::Callable(callable) => callable,
        _ => match resolver.resolve_symbol(owner, callee_clean, at, SymbolRole::Callable) {
            Symbol::Callable(callable) => callable,
            _ => return None,
        },
    };
    if let Some(result) = index.return_type_tracked(callable, trace) {
        if !matches!(result, TypeTarget::Unknown) {
            return Some(result.clone());
        }
    }
    if node.kind == crate::models::tags::NodeKind::Enum
        && callable.kind == crate::models::tags::NodeKind::Constant
        && callable.detail_entry(crate::models::details::NodeDetailKey::RUST_ENUM_VARIANT) == true
    {
        return Some(target.clone());
    }
    if let Some(synth) = callable
        .detail_entry(crate::models::details::NodeDetailKey::RUST_SYNTH_RETURN)
        .as_str()
    {
        if let Some(resolved) = synth_return_target(synth, target) {
            return Some(resolved);
        }
    }
    resolver
        .value_flow(callable)
        .and_then(|flow| flow.return_type.as_ref())
        .filter(|returned| matches!(returned, TypeExpr::Named { name } if name == "Self"))
        .map(|_| target.clone())
}

fn constructor_builtin_result<'a>(
    target: &TypeTarget<'a>,
    name: &str,
    member: &str,
    owner: &'a Node,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    if matches!(member, "new" | "from" | "with_capacity" | "default") {
        return Some(target.clone());
    }
    if name == "String" {
        if let Some(res) = string_utf8_result(member, true) {
            return Some(res);
        }
    } else if name == "str" {
        if let Some(res) = string_utf8_result(member, false) {
            return Some(res);
        }
    }
    if matches!(member, "from_utf8" | "from_utf8_lossy" | "try_from" | "now") {
        let external = TypeTarget::External {
            module: name.to_owned(),
            import_line: 0,
        };
        if let Some(result) = resolver.resolve_member_result(&external, member, owner, at) {
            return Some(result);
        }
    }
    None
}

pub(in crate::linker) fn constructor_result<'a>(
    index: &(impl ValueFlowLookup<'a> + ?Sized),
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
    trace: Option<&ReadTrace<'a, '_>>,
) -> Option<TypeTarget<'a>> {
    if owner.language != crate::models::tags::LanguageId::Rust {
        return None;
    }
    let callee_clean = callee.replace("::", ".");
    let returned = index
        .type_of_expression_tracked(owner, &callee_clean, at, resolver, trace)
        .cloned()
        .unwrap_or(TypeTarget::Unknown);
    if !matches!(returned, TypeTarget::Unknown) {
        return Some(returned);
    }
    let (receiver, member) = callee_clean.rsplit_once('.')?;
    let target = if let Some(t) = index.lookup_tracked(owner, receiver, at, trace) {
        referent(t).0.clone()
    } else {
        let Symbol::Type(target) = resolver.resolve_symbol(owner, receiver, at, SymbolRole::Type)
        else {
            return None;
        };
        target
    };
    match &target {
        TypeTarget::Local(_) => {
            return constructor_local_result(
                index,
                &target,
                (member, &callee_clean),
                owner,
                at,
                resolver,
                trace,
            );
        }
        TypeTarget::Builtin(name) => {
            if let Some(res) =
                constructor_builtin_result(&target, name, member, owner, at, resolver)
            {
                return Some(res);
            }
        }
        TypeTarget::External { module, .. } => {
            if let Some(res) =
                external_constructor_result(module, &target, member, owner, at, resolver)
            {
                return res;
            }
        }
        TypeTarget::ConfiguredReceiver {
            authority, origin, ..
        } => {
            let module = match origin.as_ref() {
                TypeTarget::External { module, .. } => Some(module.as_str()),
                _ => match authority.as_ref() {
                    ConfiguredReceiverAuthority::Hierarchy { rule, .. } => {
                        rule.bases.first().map(String::as_str)
                    }
                    _ => None,
                },
            };
            if let Some(module) = module {
                if let Some(res) =
                    external_constructor_result(module, &target, member, owner, at, resolver)
                {
                    return res;
                }
            }
        }
        _ => return None,
    }
    match member {
        "open" | "open_in_memory" | "open_with_flags" | "from_str" | "parse" | "connect"
        | "build" | "try_new" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(target),
            err: None,
        }),
        "new" | "default" | "create" | "current" | "none" => Some(target),
        _ => None,
    }
}

/// Returns `Some(res)` to short-circuit `constructor_result`, or `None` to fall back
/// to generic Rust constructor suffixes for workspace/registered external types.
fn external_constructor_result<'a>(
    module: &str,
    target: &TypeTarget<'a>,
    member: &str,
    owner: &'a Node,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<Option<TypeTarget<'a>>> {
    let module_ref = import_path(module.trim_start_matches("::"));
    let Some(crate_name) = module_ref.split('.').next().filter(|s| !s.is_empty()) else {
        return Some(None);
    };
    if matches!(crate_name, "std" | "alloc" | "core") {
        if matches!(member, "new" | "from" | "with_capacity" | "default") {
            return Some(Some(target.clone()));
        }
    } else if !super::forwarding::is_registered_cargo_crate(crate_name) {
        // Reject unregistered Cargo crate homonyms before TOML or suffix fallbacks.
        return Some(None);
    }
    if let Some(resolved) = resolver.resolve_member_result(target, member, owner, at) {
        return Some(Some(resolved));
    }
    (!module_ref.contains('.')).then_some(None)
}

fn is_str_slice_method(member: &str) -> bool {
    matches!(
        member,
        "trim"
            | "trim_start"
            | "trim_end"
            | "trim_matches"
            | "trim_start_matches"
            | "trim_end_matches"
    )
}

fn opt_wrap<'a>(ok: TypeTarget<'a>) -> TypeTarget<'a> {
    TypeTarget::RustWrapper {
        wrapper: RustWrapperKind::Option,
        ok: Box::new(ok),
        err: None,
    }
}

fn iter_of<'a>(item: TypeTarget<'a>) -> TypeTarget<'a> {
    TypeTarget::RustIterator {
        item: Box::new(item),
    }
}

pub(in crate::linker) fn is_adapter(member: &str) -> bool {
    is_str_slice_method(member)
        || matches!(
            member,
            "?" | "unwrap"
                | "expect"
                | "ok"
                | "as_ref"
                | "as_mut"
                | "as_deref"
                | "cloned"
                | "copied"
                | "clone"
                | "iter"
                | "iter_mut"
                | "into_iter"
                | "par_iter"
                | "par_iter_mut"
                | "into_par_iter"
                | "next"
                | "map"
                | "map_or"
                | "map_or_else"
                | "filter"
                | "filter_entry"
                | "enumerate"
                | "take"
                | "skip"
                | "step_by"
                | "zip"
                | "chain"
                | "rev"
                | "peekable"
                | "flatten"
                | "collect"
                | "strip_prefix"
                | "strip_suffix"
                | "replace"
                | "replacen"
                | "to_lowercase"
                | "to_uppercase"
                | "repeat"
                | "rsplit"
                | "rsplitn"
                | "splitn"
                | "find"
                | "rfind"
                | "args"
                | "to_owned"
                | "to_string"
                | "into_owned"
                | "into_bytes"
                | "into_inner"
                | "file_type"
                | "elapsed"
                | "prefix"
                | "suffix"
                | "tempdir"
                | "tempdir_in"
                | "tempfile"
                | "tempfile_in"
                | "head"
                | "peel_to_commit"
                | "peel_to_tree"
                | "tree"
                | "index"
                | "path"
                | "depth"
                | "timestamp"
                | "into"
                | "as_bytes"
                | "lines"
                | "split"
                | "split_inclusive"
                | "split_terminator"
                | "rsplit_terminator"
                | "chars"
                | "bytes"
                | "join"
                | "to_str"
                | "to_string_lossy"
                | "to_path_buf"
                | "as_path"
                | "as_os_str"
                | "with_extension"
                | "with_file_name"
                | "components"
                | "ancestors"
                | "canonicalize"
                | "file_name"
                | "extension"
                | "current_dir"
                | "output"
                | "status"
                | "get"
                | "get_ref"
                | "get_or_insert"
                | "get_or_insert_with"
                | "get_or_init"
                | "as_str"
                | "as_array"
                | "as_array_mut"
                | "as_object"
                | "as_object_mut"
                | "as_u64"
                | "as_i64"
                | "as_bool"
                | "arg"
                | "and_then"
                | "or"
                | "or_else"
                | "and"
                | "transpose"
                | "ok_or"
                | "ok_or_else"
                | "filter_map"
                | "flat_map"
                | "inspect"
                | "fuse"
                | "as_slice"
                | "query_row"
                | "prepare"
                | "prepare_cached"
                | "transaction"
                | "unchecked_transaction"
                | "transaction_with_behavior"
                | "query"
                | "parse"
                | "parse_with"
                | "child"
                | "named_child"
                | "child_by_field_name"
                | "parent"
                | "next_sibling"
                | "prev_sibling"
                | "next_named_sibling"
                | "prev_named_sibling"
                | "children"
                | "named_children"
                | "split_whitespace"
                | "split_once"
                | "rsplit_once"
                | "first"
                | "first_mut"
                | "last"
                | "last_mut"
                | "get_mut"
                | "pop"
                | "pop_front"
                | "pop_back"
                | "front"
                | "back"
                | "remove"
                | "swap_remove"
                | "shift_remove"
                | "insert"
                | "get_key_value"
                | "entry"
                | "keys"
                | "values"
                | "values_mut"
                | "into_keys"
                | "into_values"
                | "or_insert"
                | "or_insert_with"
                | "or_insert_with_key"
                | "or_default"
                | "and_modify"
                | "lock"
                | "try_lock"
                | "read"
                | "try_read"
                | "write"
                | "try_write"
                | "borrow"
                | "try_borrow"
                | "borrow_mut"
                | "try_borrow_mut"
                | "get_or_try_init"
                | "context"
                | "with_context"
                | "then_some"
                | "unwrap_or"
                | "unwrap_or_else"
                | "unwrap_or_default"
                | "map_err"
                | "inspect_err"
                | "as_deref_mut"
                | "err"
                | "position"
                | "rposition"
                | "is_none"
                | "is_some"
                | "is_ok"
                | "is_err"
                | "is_empty"
                | "is_some_and"
                | "is_none_or"
                | "is_ok_and"
                | "is_err_and"
                | "then"
                | "cmp"
                | "partial_cmp"
                | "then_with"
                | "reverse"
                | "is_eq"
                | "is_ne"
                | "is_lt"
                | "is_le"
                | "is_gt"
                | "is_ge"
                | "checked_add"
                | "checked_sub"
                | "checked_mul"
                | "checked_div"
                | "checked_rem"
                | "saturating_add"
                | "saturating_sub"
                | "saturating_mul"
                | "append"
                | "truncate"
                | "create"
                | "create_new"
                | "mode"
                | "custom_flags"
                | "open"
                | "metadata"
                | "symlink_metadata"
                | "permissions"
                | "file_stem"
                | "is_file"
                | "is_dir"
                | "is_symlink"
                | "exists"
                | "contains"
                | "contains_key"
                | "starts_with"
                | "ends_with"
                | "is_ascii"
                | "eq_ignore_ascii_case"
                | "find_map"
                | "partition"
                | "try_fold"
                | "try_for_each"
                | "peek"
                | "peek_mut"
                | "next_if"
                | "next_if_eq"
                | "as_mut_slice"
                | "windows"
                | "chunks"
                | "chunks_exact"
                | "rchunks"
                | "split_first"
                | "split_last"
                | "take_while"
                | "skip_while"
                | "max_by"
                | "min_by"
                | "max_by_key"
                | "min_by_key"
                | "reduce"
                | "scan"
                | "to_vec"
                | "as_file"
                | "key"
                | "insert_entry"
                | "char_indices"
                | "into_mut"
                | "remove_entry"
        )
}

fn receiver_matches(receiver: &TypeTarget<'_>, expected: &[&str]) -> bool {
    let (receiver, _, _) = referent(receiver);
    let matches = |name: &str| {
        expected
            .iter()
            .any(|expected| import_path(name) == import_path(expected))
    };
    match receiver {
        TypeTarget::External { module, .. } | TypeTarget::Builtin(module) => matches(module),
        TypeTarget::ConfiguredReceiver { authority, .. } => match authority.as_ref() {
            ConfiguredReceiverAuthority::Hierarchy { rule, .. } => {
                rule.bases.iter().any(|base| matches(base))
            }
            _ => false,
        },
        _ => false,
    }
}

pub(in crate::linker) fn declared_adapter_result<'a>(
    receiver: &TypeTarget<'a>,
    member: &str,
    owner: &'a Node,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let (receiver, _, _) = referent(receiver);
    let builtin_receiver = match receiver {
        TypeTarget::Builtin(name)
            if matches!(
                name.as_str(),
                "str"
                    | "String"
                    | "Vec"
                    | "HashMap"
                    | "BTreeMap"
                    | "HashSet"
                    | "BTreeSet"
                    | "Option"
                    | "Result"
                    | "Box"
                    | "Rc"
                    | "Arc"
                    | "Cow"
                    | "VecDeque"
                    | "OsStr"
                    | "OsString"
                    | "Path"
                    | "PathBuf"
                    | "Iterator"
                    | "Slice"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
            ) || name.starts_with("std::")
                || name.starts_with("std.") =>
        {
            Some(TypeTarget::External {
                module: name.clone(),
                import_line: 0,
            })
        }
        _ => None,
    };
    if matches!(receiver, TypeTarget::Local(_)) {
        if let Symbol::Callable(callable) = resolver.resolve_member(receiver, member, owner, at) {
            if let Some(synth) = callable
                .detail_entry(crate::models::details::NodeDetailKey::RUST_SYNTH_RETURN)
                .as_str()
            {
                if let Some(resolved) = synth_return_target(synth, receiver) {
                    return Some(resolved);
                }
            }
        }
    }
    let is_stdlib_receiver = matches!(
        receiver,
        TypeTarget::Builtin(_)
            | TypeTarget::RustCollection { .. }
            | TypeTarget::RustTypedContainer { .. }
            | TypeTarget::RustSlice { .. }
            | TypeTarget::RustIterator { .. }
            | TypeTarget::RustWrapper { .. }
            | TypeTarget::RustDeref { .. }
            | TypeTarget::RustReference { .. }
    ) || matches!(
        receiver,
        TypeTarget::External { module, .. }
            if matches!(
                import_path(module).split('.').next(),
                Some("std" | "core" | "alloc")
            )
    );
    let target = resolver
        .resolve_member_result(receiver, member, owner, at)
        .or_else(|| {
            builtin_receiver
                .as_ref()
                .and_then(|receiver| resolver.resolve_member_result(receiver, member, owner, at))
        })
        .or_else(|| {
            (is_stdlib_receiver && !matches!(receiver, TypeTarget::ConfiguredReceiver { .. }))
                .then(|| stdlib_cross_pack_return(receiver, member))
                .flatten()
        })?;
    if !matches!(receiver, TypeTarget::ConfiguredReceiver { .. }) {
        if let Some(refined) = stdlib_cross_pack_return(receiver, member) {
            return Some(refined);
        }
    }
    if matches!(
        target,
        TypeTarget::RustWrapper { .. } | TypeTarget::RustIterator { .. }
    ) {
        return Some(target);
    }
    // Wrap unwrapped custom/test overrides (e.g., SuppliedResolver or custom TOML base types).
    Some(wrap_unwrapped_adapter_override(receiver, member, target))
}

fn wrap_unwrapped_adapter_override<'a>(
    receiver: &TypeTarget<'a>,
    member: &str,
    target: TypeTarget<'a>,
) -> TypeTarget<'a> {
    if member == "into_iter" && receiver_matches(receiver, &["walkdir::WalkDir"]) {
        return TypeTarget::RustIterator {
            item: Box::new(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(target),
                err: None,
            }),
        };
    }
    if member == "next" && receiver_matches(receiver, &["rusqlite::Rows"]) {
        return TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Option,
                ok: Box::new(target),
                err: None,
            }),
            err: None,
        };
    }
    if matches!(member, "get" | "get_ref") && receiver_matches(receiver, &["rusqlite::Row"]) {
        let ok = if receiver_matches(
            &target,
            &["Result", "std::result::Result", "core::result::Result"],
        ) {
            TypeTarget::Unknown
        } else {
            target
        };
        return TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(ok),
            err: None,
        };
    }
    let result_override = (member == "output"
        && receiver_matches(receiver, &["std::process::Command"]))
        || (matches!(
            member,
            "prepare"
                | "prepare_cached"
                | "transaction"
                | "unchecked_transaction"
                | "transaction_with_behavior"
        ) && receiver_matches(receiver, &["rusqlite::Connection", "rusqlite::Transaction"]))
        || (member == "query" && receiver_matches(receiver, &["rusqlite::Statement"]));
    if result_override {
        return TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(target),
            err: None,
        };
    }
    target
}

fn resolve_applied_result_alias<'a>(
    owner: &'a Node,
    base: &str,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let result_alias = matches!(
        base,
        "anyhow::Result"
            | "anyhow.Result"
            | "std::io::Result"
            | "std.io.Result"
            | "std::fmt::Result"
            | "std.fmt.Result"
    ) || matches!(
        nominal,
        Symbol::Type(TypeTarget::External { module, .. })
            if matches!(
                import_path(module).as_ref(),
                "anyhow.Result" | "std.io.Result" | "std.fmt.Result"
            )
    );
    if !result_alias || args.len() != 1 {
        return None;
    }
    let mut res = wrapper_type(owner, RustWrapperKind::Result, args, at, resolver, true);
    let ext_mod = match nominal {
        Symbol::Type(TypeTarget::External { module, .. }) => Some(import_path(module)),
        _ => None,
    };
    let err_module = if matches!(base, "std::io::Result" | "std.io.Result")
        || ext_mod.as_deref() == Some("std.io.Result")
    {
        Some("std::io::Error")
    } else if matches!(base, "anyhow::Result" | "anyhow.Result")
        || ext_mod.as_deref() == Some("anyhow.Result")
    {
        Some("anyhow::Error")
    } else if matches!(base, "std::fmt::Result" | "std.fmt.Result")
        || ext_mod.as_deref() == Some("std.fmt.Result")
    {
        Some("std::fmt::Error")
    } else {
        None
    };
    if let (Some(err_module), TypeTarget::RustWrapper { err, .. }) = (err_module, &mut res) {
        *err = Some(Box::new(TypeTarget::External {
            module: err_module.to_owned(),
            import_line: 0,
        }));
    }
    Some(res)
}

fn resolve_applied_external<'a>(
    owner: &'a Node,
    base: &str,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    if matches!(nominal, Symbol::Type(TypeTarget::Local(_))) {
        return None;
    }
    let module = generic_external_container(base, nominal)?;
    let resolve_arg = |idx: usize| {
        args.get(idx)
            .map(|arg| resolve_type(owner, arg, at, resolver, SemanticLimits::default()))
            .unwrap_or(TypeTarget::Unknown)
    };
    match module {
        "std::collections::HashMap"
        | "hashbrown::HashMap"
        | "std::collections::BTreeMap"
        | "indexmap::IndexMap"
        | "serde_json::Map" => {
            let (k, v) = if module == "serde_json::Map" && args.is_empty() {
                (
                    TypeTarget::Builtin("String".to_owned()),
                    TypeTarget::External {
                        module: "serde_json::Value".to_owned(),
                        import_line: 0,
                    },
                )
            } else if args.len() >= 2 {
                (resolve_arg(0), resolve_arg(1))
            } else {
                (TypeTarget::Unknown, TypeTarget::Unknown)
            };
            let wrapper = match module {
                "std::collections::BTreeMap" => "BTreeMap",
                "indexmap::IndexMap" => "IndexMap",
                _ => "HashMap",
            };
            return Some(TypeTarget::RustDeref {
                wrapper,
                target: Box::new(TypeTarget::RustTuple(vec![k, v])),
            });
        }
        "std::sync::OnceLock"
        | "std::sync::Mutex"
        | "parking_lot::Mutex"
        | "tokio::sync::Mutex"
        | "std::sync::RwLock"
        | "parking_lot::RwLock"
        | "tokio::sync::RwLock"
        | "std::cell::RefCell"
        | "std::thread::LocalKey"
        | "tokio::task::JoinHandle"
            if args.len() == 1 =>
        {
            let wrapper = match module {
                "std::sync::OnceLock" => "OnceLock",
                "std::sync::Mutex" | "parking_lot::Mutex" | "tokio::sync::Mutex" => "Mutex",
                "std::sync::RwLock" | "parking_lot::RwLock" | "tokio::sync::RwLock" => "RwLock",
                "std::cell::RefCell" => "RefCell",
                "std::thread::LocalKey" => "LocalKey",
                _ => "JoinHandle",
            };
            return Some(TypeTarget::RustDeref {
                wrapper,
                target: Box::new(TypeTarget::RustTuple(vec![resolve_arg(0)])),
            });
        }
        _ => {}
    }
    Some(TypeTarget::External {
        module: module.to_owned(),
        import_line: 0,
    })
}

fn resolve_applied_smart_ptr<'a>(
    owner: &'a Node,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let Symbol::Type(target) = nominal else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    let wrapper = deref_wrapper(target)?;
    let mut pointee = resolve_type(owner, &args[0], at, resolver, SemanticLimits::default());
    if matches!(pointee, TypeTarget::Unknown) {
        if let TypeExpr::Applied { base, .. } = &args[0] {
            if let Symbol::Type(TypeTarget::External {
                module,
                import_line,
            }) = resolver.resolve_symbol(owner, base, at, SymbolRole::Type)
            {
                if matches!(
                    import_path(&module).split('.').next(),
                    Some("std" | "alloc" | "core")
                ) {
                    pointee = TypeTarget::External {
                        module,
                        import_line,
                    };
                }
            }
        }
    }
    Some(TypeTarget::RustDeref {
        wrapper,
        target: Box::new(pointee),
    })
}

pub(in crate::linker) fn resolve_applied<'a>(
    owner: &'a Node,
    base: &str,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    if owner.language != crate::models::tags::LanguageId::Rust {
        return None;
    }
    let resolve_item = |item: &TypeExpr| {
        Box::new(resolve_type(
            owner,
            item,
            at,
            resolver,
            SemanticLimits::default(),
        ))
    };
    if matches!(
        base,
        "Iterator"
            | "std::iter::Iterator"
            | "core::iter::Iterator"
            | "std.iter.Iterator"
            | "core.iter.Iterator"
            | "DoubleEndedIterator"
            | "std::iter::DoubleEndedIterator"
            | "ExactSizeIterator"
            | "std::iter::ExactSizeIterator"
    ) && args.len() == 1
    {
        return Some(iter_of(*resolve_item(&args[0])));
    }
    let single_arg = |build: &dyn Fn(Box<TypeTarget<'a>>) -> TypeTarget<'a>| {
        Some(match args {
            [item] => build(resolve_item(item)),
            _ => TypeTarget::Unknown,
        })
    };
    if matches!(base, "[]" | "Slice") {
        return single_arg(&|item| TypeTarget::RustSlice { item });
    }
    let typed_container = match base {
        "VecDeque"
        | "std::collections::VecDeque"
        | "std.collections.VecDeque"
        | "alloc::collections::VecDeque"
        | "alloc.collections.VecDeque" => Some("VecDeque"),
        "HashSet" | "std::collections::HashSet" | "std.collections.HashSet" => Some("HashSet"),
        "BTreeSet" | "std::collections::BTreeSet" | "std.collections.BTreeSet" => Some("BTreeSet"),
        _ => None,
    };
    if let Some(container_base) = typed_container {
        return single_arg(&|item| TypeTarget::RustTypedContainer {
            base: container_base,
            item,
        });
    }
    if let Some(res) = resolve_applied_result_alias(owner, base, args, nominal, at, resolver) {
        return Some(res);
    }
    let is_collection = matches!(
        base,
        "Vec" | "std::vec::Vec" | "alloc::vec::Vec" | "std.vec.Vec" | "alloc.vec.Vec"
    ) || matches!(nominal, Symbol::Type(target) if standard_collection(target));
    if is_collection {
        return single_arg(&|item| TypeTarget::RustCollection { item });
    }
    let standard_opt_or_res = if matches!(
        base,
        "Option"
            | "std::option::Option"
            | "core::option::Option"
            | "std.option.Option"
            | "core.option.Option"
    ) {
        Some(RustWrapperKind::Option)
    } else if matches!(
        base,
        "Result"
            | "std::result::Result"
            | "core::result::Result"
            | "std.result.Result"
            | "core.result.Result"
    ) {
        Some(RustWrapperKind::Result)
    } else if let Symbol::Type(target) = nominal {
        standard_wrapper(target)
    } else {
        None
    };
    if let Some(wrapper) = standard_opt_or_res {
        let valid_arity = match wrapper {
            RustWrapperKind::Option => args.len() == 1,
            RustWrapperKind::Result => args.len() == 2 || (args.len() == 1 && base == "Result"),
        };
        if !valid_arity {
            return Some(TypeTarget::Unknown);
        }
        return Some(wrapper_type(
            owner,
            wrapper,
            args,
            at,
            resolver,
            args.len() == 1,
        ));
    }
    if let Some(smart_ptr) = resolve_applied_smart_ptr(owner, args, nominal, at, resolver) {
        return Some(smart_ptr);
    }
    if matches!(
        base,
        "Cow" | "std::borrow::Cow" | "alloc::borrow::Cow" | "std.borrow.Cow" | "alloc.borrow.Cow"
    ) && !args.is_empty()
    {
        return Some(TypeTarget::RustDeref {
            wrapper: "Cow",
            target: resolve_item(args.last().expect("non-empty Cow args")),
        });
    }
    if let Some(ext) = resolve_applied_external(owner, base, args, nominal, at, resolver) {
        return Some(ext);
    }
    match nominal {
        Symbol::Type(TypeTarget::Local(node))
            if node.kind == crate::models::tags::NodeKind::Type =>
        {
            if let Some(alias) = resolver
                .value_flow(node)
                .and_then(|facts| facts.alias_type.as_ref())
            {
                return Some(resolve_type(
                    node,
                    alias,
                    node_position(node),
                    resolver,
                    SemanticLimits::default(),
                ));
            }
            None
        }
        Symbol::Type(TypeTarget::Local(node))
            if owner.language == crate::models::tags::LanguageId::Rust
                && matches!(node.kind.as_str(), "struct" | "enum") =>
        {
            Some(TypeTarget::Local(node))
        }
        _ => None,
    }
}

fn generic_external_container(base: &str, nominal: &Symbol<'_>) -> Option<&'static str> {
    let from_name = |name: &str| -> Option<&'static str> {
        match import_path(name).as_ref() {
            "HashMap" | "std.collections.HashMap" => Some("std::collections::HashMap"),
            "BTreeMap" | "std.collections.BTreeMap" => Some("std::collections::BTreeMap"),
            "hashbrown.HashMap" => Some("hashbrown::HashMap"),
            "hashbrown.HashSet" => Some("hashbrown::HashSet"),
            "indexmap.IndexMap" | "IndexMap" => Some("indexmap::IndexMap"),
            "indexmap.IndexSet" | "IndexSet" => Some("indexmap::IndexSet"),
            "serde_json.Map" | "serde_json.map.Map" => Some("serde_json::Map"),
            "OnceLock" | "std.sync.OnceLock" => Some("std::sync::OnceLock"),
            "std.sync.Mutex" => Some("std::sync::Mutex"),
            "parking_lot.Mutex" => Some("parking_lot::Mutex"),
            "tokio.sync.Mutex" => Some("tokio::sync::Mutex"),
            "std.sync.RwLock" => Some("std::sync::RwLock"),
            "parking_lot.RwLock" => Some("parking_lot::RwLock"),
            "tokio.sync.RwLock" => Some("tokio::sync::RwLock"),
            "RefCell" | "std.cell.RefCell" => Some("std::cell::RefCell"),
            "LocalKey" | "std.thread.LocalKey" => Some("std::thread::LocalKey"),
            "petgraph.Graph" | "petgraph.graph.Graph" => Some("petgraph::Graph"),
            "petgraph.DiGraph" | "petgraph.graph.DiGraph" => Some("petgraph::DiGraph"),
            "tokio.sync.mpsc.Sender" => Some("tokio::sync::mpsc::Sender"),
            "tokio.sync.mpsc.Receiver" => Some("tokio::sync::mpsc::Receiver"),
            "crossbeam.channel.Sender" => Some("crossbeam::channel::Sender"),
            "crossbeam.channel.Receiver" => Some("crossbeam::channel::Receiver"),
            "JoinHandle" | "tokio.task.JoinHandle" => Some("tokio::task::JoinHandle"),
            _ => None,
        }
    };
    from_name(base).or_else(|| match nominal {
        Symbol::Type(TypeTarget::External { module, .. }) => from_name(module),
        _ => None,
    })
}

fn stdlib_cross_pack_return<'a>(receiver: &TypeTarget<'a>, member: &str) -> Option<TypeTarget<'a>> {
    let ext = |module: &str| TypeTarget::External {
        module: module.to_owned(),
        import_line: 0,
    };
    let opt = opt_wrap;
    let io_res = |ok: TypeTarget<'a>| TypeTarget::RustWrapper {
        wrapper: RustWrapperKind::Result,
        ok: Box::new(ok),
        err: Some(Box::new(ext("std::io::Error"))),
    };
    if receiver_matches(receiver, &["tempfile::TempDir", "tempfile::NamedTempFile"]) {
        return match member {
            "path" => Some(ext("std::path::Path")),
            "as_file" => Some(ext("std::fs::File")),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["std::process::Output"]) {
        return match member {
            "status" => Some(ext("std::process::ExitStatus")),
            "stdout" | "stderr" => Some(TypeTarget::RustCollection {
                item: Box::new(TypeTarget::Builtin("u8".to_owned())),
            }),
            _ => None,
        };
    }
    let dir_entry_member = |opt_file_type: bool| match member {
        "path" => Some(ext("std::path::Path")),
        "into_path" => Some(TypeTarget::Builtin("PathBuf".to_owned())),
        "file_name" => Some(ext("std::ffi::OsStr")),
        "file_type" => Some(if opt_file_type {
            opt(ext("std::fs::FileType"))
        } else {
            ext("std::fs::FileType")
        }),
        "metadata" => Some(io_res(ext("std::fs::Metadata"))),
        _ => None,
    };
    if receiver_matches(receiver, &["walkdir::DirEntry"]) {
        return dir_entry_member(false);
    }
    if receiver_matches(receiver, &["ignore::DirEntry"]) {
        return dir_entry_member(true);
    }
    if receiver_matches(receiver, &["std::fs::Metadata", "Metadata"]) {
        return match member {
            "file_type" => Some(ext("std::fs::FileType")),
            "permissions" => Some(ext("std::fs::Permissions")),
            "len" => Some(TypeTarget::Builtin("u64".to_owned())),
            "is_dir" | "is_file" | "is_symlink" => Some(TypeTarget::Builtin("bool".to_owned())),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["std::fs::DirEntry"]) {
        return match member {
            "path" => Some(TypeTarget::Builtin("PathBuf".to_owned())),
            "file_name" => Some(TypeTarget::Builtin("OsString".to_owned())),
            "file_type" => Some(io_res(ext("std::fs::FileType"))),
            "metadata" => Some(io_res(ext("std::fs::Metadata"))),
            _ => None,
        };
    }
    if receiver_matches(
        receiver,
        &[
            "std::path::Path",
            "Path",
            "std::path::PathBuf",
            "PathBuf",
            "std::ffi::OsStr",
            "OsStr",
            "std::ffi::OsString",
            "OsString",
        ],
    ) {
        let is_os = receiver_matches(
            receiver,
            &["std::ffi::OsStr", "OsStr", "std::ffi::OsString", "OsString"],
        );
        return match member {
            "to_str" => Some(opt(reference(
                &TypeTarget::Builtin("str".to_owned()),
                false,
            ))),
            "to_string_lossy" => Some(TypeTarget::RustDeref {
                wrapper: "Cow",
                target: Box::new(TypeTarget::Builtin("str".to_owned())),
            }),
            "as_os_str" => Some(ext("std::ffi::OsStr")),
            "as_path" if !is_os => Some(ext("std::path::Path")),
            "parent" if !is_os => Some(opt(ext("std::path::Path"))),
            "file_name" | "file_stem" | "extension" if !is_os => Some(opt(ext("std::ffi::OsStr"))),
            "cmp" => Some(ext("std::cmp::Ordering")),
            "join" | "to_path_buf" | "with_extension" | "with_file_name" if !is_os => {
                Some(TypeTarget::Builtin("PathBuf".to_owned()))
            }
            "canonicalize" if !is_os => Some(io_res(TypeTarget::Builtin("PathBuf".to_owned()))),
            "ancestors" if !is_os => Some(TypeTarget::RustIterator {
                item: Box::new(ext("std::path::Path")),
            }),
            "components" if !is_os => Some(TypeTarget::RustIterator {
                item: Box::new(TypeTarget::Unknown),
            }),
            "strip_prefix" if !is_os => Some(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(ext("std::path::Path")),
                err: None,
            }),
            "metadata" | "symlink_metadata" if !is_os => Some(io_res(ext("std::fs::Metadata"))),
            "read_dir" if !is_os => Some(io_res(ext("std::fs::ReadDir"))),
            "read_link" if !is_os => Some(io_res(TypeTarget::Builtin("PathBuf".to_owned()))),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["toml::Value"]) {
        return match member {
            "as_str" => Some(opt(TypeTarget::Builtin("str".to_owned()))),
            "as_integer" => Some(opt(TypeTarget::Builtin("i64".to_owned()))),
            "as_float" => Some(opt(TypeTarget::Builtin("f64".to_owned()))),
            "as_bool" => Some(opt(TypeTarget::Builtin("bool".to_owned()))),
            "as_array" => Some(opt(TypeTarget::RustCollection {
                item: Box::new(ext("toml::Value")),
            })),
            "as_table" => Some(opt(ext("toml::Table"))),
            "get" | "get_mut" => Some(opt(ext("toml::Value"))),
            _ => None,
        };
    }
    let map_member = |value_ty: &str| -> Option<TypeTarget<'a>> {
        match member {
            "get" | "get_mut" => Some(opt(ext(value_ty))),
            "iter" | "iter_mut" => Some(TypeTarget::RustIterator {
                item: Box::new(TypeTarget::RustTuple(vec![
                    TypeTarget::Builtin("String".to_owned()),
                    ext(value_ty),
                ])),
            }),
            "keys" => Some(TypeTarget::RustIterator {
                item: Box::new(TypeTarget::Builtin("String".to_owned())),
            }),
            "values" | "values_mut" => Some(TypeTarget::RustIterator {
                item: Box::new(ext(value_ty)),
            }),
            _ => None,
        }
    };
    if receiver_matches(receiver, &["toml::Table", "toml::map::Map"]) {
        return map_member("toml::Value");
    }
    if receiver_matches(receiver, &["std::time::Instant"]) && member == "elapsed" {
        return Some(ext("std::time::Duration"));
    }
    if receiver_matches(receiver, &["serde_json::Value"]) {
        return match member {
            "as_str" => Some(opt(TypeTarget::Builtin("str".to_owned()))),
            "as_u64" => Some(opt(TypeTarget::Builtin("u64".to_owned()))),
            "as_i64" => Some(opt(TypeTarget::Builtin("i64".to_owned()))),
            "as_bool" => Some(opt(TypeTarget::Builtin("bool".to_owned()))),
            "as_array" | "as_array_mut" => Some(opt(TypeTarget::RustCollection {
                item: Box::new(ext("serde_json::Value")),
            })),
            "as_object" | "as_object_mut" => Some(opt(ext("serde_json::Map"))),
            "get" | "get_mut" => Some(opt(ext("serde_json::Value"))),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["serde_json::Map", "serde_json::map::Map"]) {
        return map_member("serde_json::Value");
    }
    if receiver_matches(receiver, &["tree_sitter::Node"]) {
        return match member {
            "kind" => Some(TypeTarget::Builtin("str".to_owned())),
            "utf8_text" => Some(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(TypeTarget::Builtin("str".to_owned())),
                err: Some(Box::new(ext("std::str::Utf8Error"))),
            }),
            _ => None,
        };
    }
    if receiver_matches(
        receiver,
        &["std::ffi::OsStr", "OsStr", "std::ffi::OsString", "OsString"],
    ) {
        return match member {
            "to_str" => Some(opt(TypeTarget::Builtin("str".to_owned()))),
            "to_string_lossy" => Some(TypeTarget::Builtin("String".to_owned())),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["std::process::Output", "Output"]) {
        return match member {
            "stdout" | "stderr" => Some(TypeTarget::RustCollection {
                item: Box::new(TypeTarget::Builtin("u8".to_owned())),
            }),
            "status" => Some(ext("std::process::ExitStatus")),
            _ => None,
        };
    }
    if receiver_matches(receiver, &["std::process::Command", "Command"])
        && matches!(member, "output" | "status")
    {
        return Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(ext(if member == "output" {
                "std::process::Output"
            } else {
                "std::process::ExitStatus"
            })),
            err: Some(Box::new(ext("std::io::Error"))),
        });
    }
    None
}

fn wrapper_type<'a>(
    owner: &'a Node,
    wrapper: RustWrapperKind,
    args: &[TypeExpr],
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
    aliased_error: bool,
) -> TypeTarget<'a> {
    let ok = resolve_type(owner, &args[0], at, resolver, SemanticLimits::default());
    let err = match wrapper {
        RustWrapperKind::Option => None,
        RustWrapperKind::Result if aliased_error => Some(Box::new(TypeTarget::Unknown)),
        RustWrapperKind::Result => Some(Box::new(resolve_type(
            owner,
            &args[1],
            at,
            resolver,
            SemanticLimits::default(),
        ))),
    };
    TypeTarget::RustWrapper {
        wrapper,
        ok: Box::new(ok),
        err,
    }
}

fn standard_wrapper(target: &TypeTarget<'_>) -> Option<RustWrapperKind> {
    match target {
        TypeTarget::Builtin(name) => match name.as_str() {
            "Option" => Some(RustWrapperKind::Option),
            "Result" => Some(RustWrapperKind::Result),
            _ => None,
        },
        TypeTarget::External { module, .. } => match import_path(module).as_ref() {
            "std.option.Option" | "core.option.Option" => Some(RustWrapperKind::Option),
            "std.result.Result" | "core.result.Result" => Some(RustWrapperKind::Result),
            _ => None,
        },
        _ => None,
    }
}

fn standard_collection(target: &TypeTarget<'_>) -> bool {
    match target {
        TypeTarget::Builtin(name) => {
            matches!(name.as_str(), "Vec" | "VecDeque" | "HashSet" | "BTreeSet")
        }
        TypeTarget::External { module, .. } => {
            matches!(
                import_path(module).as_ref(),
                "std.vec.Vec"
                    | "alloc.vec.Vec"
                    | "std.collections.VecDeque"
                    | "alloc.collections.VecDeque"
                    | "std.collections.HashSet"
                    | "std.collections.BTreeSet"
            )
        }
        _ => false,
    }
}

fn deref_wrapper(target: &TypeTarget<'_>) -> Option<&'static str> {
    match target {
        TypeTarget::Builtin(name) if name == "Box" => Some("Box"),
        TypeTarget::External { module, .. } => match import_path(module).as_ref() {
            "std.boxed.Box" | "alloc.boxed.Box" => Some("Box"),
            "std.sync.Arc" | "alloc.sync.Arc" => Some("Arc"),
            "std.rc.Rc" | "alloc.rc.Rc" => Some("Rc"),
            _ => None,
        },
        _ => None,
    }
}

fn project_sequence_member<'a>(
    item: &TypeTarget<'a>,
    receiver: &TypeTarget<'a>,
    member: &str,
    borrowed: bool,
    mutable: bool,
) -> Option<TypeTarget<'a>> {
    match member {
        "as_slice" | "as_mut_slice" => Some(TypeTarget::RustSlice {
            item: Box::new(item.clone()),
        }),
        "windows" | "chunks" | "chunks_exact" | "rchunks" | "split" | "rsplit" | "splitn"
        | "rsplitn" => Some(TypeTarget::RustIterator {
            item: Box::new(TypeTarget::RustSlice {
                item: Box::new(item.clone()),
            }),
        }),
        "split_first" | "split_last" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(TypeTarget::RustTuple(vec![
                reference(item, false),
                TypeTarget::RustSlice {
                    item: Box::new(item.clone()),
                },
            ])),
            err: None,
        }),
        "pop_if" if !borrowed || mutable => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(item.clone()),
            err: None,
        }),
        "as_ref" => Some(if matches!(receiver, TypeTarget::RustSlice { .. }) {
            reference(receiver, false)
        } else {
            reference(
                &TypeTarget::RustSlice {
                    item: Box::new(item.clone()),
                },
                false,
            )
        }),
        "first" | "last" | "get" => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(reference(item, false)),
            err: None,
        }),
        "first_mut" | "last_mut" | "get_mut" if !borrowed || mutable => {
            Some(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Option,
                ok: Box::new(reference(item, true)),
                err: None,
            })
        }
        "pop" if !borrowed || mutable => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(item.clone()),
            err: None,
        }),
        "remove" | "swap_remove" if !borrowed || mutable => Some(item.clone()),
        "iter" => Some(TypeTarget::RustIterator {
            item: Box::new(reference(item, false)),
        }),
        "iter_mut" if !borrowed || mutable => Some(TypeTarget::RustIterator {
            item: Box::new(reference(item, true)),
        }),
        "into_iter" if borrowed => Some(TypeTarget::RustIterator {
            item: Box::new(reference(item, mutable)),
        }),
        "into_iter" => Some(TypeTarget::RustIterator {
            item: Box::new(item.clone()),
        }),
        "join" => match referent(item).0 {
            TypeTarget::Builtin(name) if matches!(name.as_str(), "str" | "String") => {
                Some(TypeTarget::Builtin("String".to_owned()))
            }
            TypeTarget::External { module, .. }
                if matches!(
                    import_path(module).as_ref(),
                    "std.string.String" | "alloc.string.String"
                ) =>
            {
                Some(TypeTarget::Builtin("String".to_owned()))
            }
            TypeTarget::RustSlice { item: inner } | TypeTarget::RustCollection { item: inner } => {
                Some(TypeTarget::RustCollection {
                    item: inner.clone(),
                })
            }
            _ => None,
        },
        "cmp" => Some(TypeTarget::External {
            module: "std::cmp::Ordering".to_owned(),
            import_line: 0,
        }),
        _ => None,
    }
}

pub(in crate::linker) fn project_structural<'a>(
    receiver: &TypeTarget<'a>,
    operation: crate::semantic::types::RustProjection,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    use crate::semantic::types::RustProjection;
    match operation {
        RustProjection::Vector => Some(TypeTarget::RustCollection {
            item: Box::new(receiver.clone()),
        }),
        RustProjection::Some => Some(opt_wrap(receiver.clone())),
        RustProjection::Ok => Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(receiver.clone()),
            err: None,
        }),
        RustProjection::ClosureMap | RustProjection::ClosureMapOr => {
            if let TypeTarget::RustTuple(items) = referent(receiver).0 {
                project_closure_tuple(items, operation)
            } else {
                None
            }
        }
        RustProjection::ClosureAndThen => Some(match referent(receiver).0 {
            TypeTarget::RustWrapper { .. } => referent(receiver).0.clone(),
            _ => TypeTarget::Unknown,
        }),
        RustProjection::ClosureFilterMap => Some(iter_of(unwrap_filter_map_item(receiver))),
        RustProjection::ClosureFlatMap => Some(iter_of(unwrap_flat_map_item(receiver))),
        RustProjection::FlatMapItem => Some(unwrap_flat_map_item(receiver)),
        RustProjection::ClosureRow => {
            if let TypeTarget::External { module, .. } = referent(receiver).0 {
                let clean = module.replace("::", ".");
                if matches!(
                    clean.as_str(),
                    "rusqlite.Connection" | "rusqlite.Statement" | "rusqlite.Transaction"
                ) {
                    return Some(TypeTarget::External {
                        module: "rusqlite::Row".to_owned(),
                        import_line: 0,
                    });
                }
            }
            Some(TypeTarget::Unknown)
        }
        RustProjection::FilterEntryItem => {
            let receiver = referent(receiver).0;
            match receiver {
                TypeTarget::RustIterator { item } => Some(match item.as_ref() {
                    TypeTarget::RustWrapper { ok, .. } => (**ok).clone(),
                    other => other.clone(),
                }),
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ if receiver_matches(receiver, &["ignore::WalkBuilder"]) => {
                    Some(TypeTarget::External {
                        module: "ignore::DirEntry".to_owned(),
                        import_line: 0,
                    })
                }
                _ => None,
            }
        }
        RustProjection::IteratorItem => {
            let target = referent(receiver).0;
            match target {
                TypeTarget::RustIterator { item } => Some((**item).clone()),
                TypeTarget::RustWrapper { ok, .. } => Some((**ok).clone()),
                TypeTarget::RustCollection { item }
                | TypeTarget::RustSlice { item }
                | TypeTarget::RustTypedContainer { item, .. } => Some(reference(item, false)),
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => match project(receiver, "into_iter", resolver) {
                    Some(TypeTarget::RustIterator { item }) => Some(*item),
                    _ => None,
                },
            }
        }
        RustProjection::LocalKey => match referent(receiver).0 {
            TypeTarget::RustDeref {
                wrapper: "LocalKey",
                target,
            } => match target.as_ref() {
                TypeTarget::RustTuple(items) => items.first().map(|inner| reference(inner, false)),
                _ => None,
            },
            TypeTarget::RustDeref { wrapper, target }
                if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
            {
                project_structural(target, operation, resolver)
            }
            _ => None,
        },
        RustProjection::ErrorPayload => match referent(receiver).0 {
            TypeTarget::RustWrapper { err, .. } => {
                Some(err.as_deref().cloned().unwrap_or(TypeTarget::Unknown))
            }
            TypeTarget::RustDeref { wrapper, target }
                if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
            {
                project_structural(target, operation, resolver)
            }
            _ => None,
        },
        RustProjection::MapTrim | RustProjection::MapString => {
            let result = TypeTarget::Builtin(
                if operation == RustProjection::MapTrim {
                    "str"
                } else {
                    "String"
                }
                .to_owned(),
            );
            match referent(receiver).0 {
                TypeTarget::RustWrapper { wrapper, err, .. } => Some(TypeTarget::RustWrapper {
                    wrapper: *wrapper,
                    ok: Box::new(result),
                    err: err.clone(),
                }),
                TypeTarget::RustIterator { .. } => Some(iter_of(result)),
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => None,
            }
        }
        RustProjection::UnwrapOrStr | RustProjection::MapOrStr => {
            let (target, borrowed, _) = referent(receiver);
            match target {
                TypeTarget::RustWrapper { .. } if operation == RustProjection::MapOrStr => {
                    Some(TypeTarget::Builtin("str".to_owned()))
                }
                TypeTarget::RustWrapper { ok, .. } if !borrowed => {
                    Some(if matches!(ok.as_ref(), TypeTarget::Unknown) {
                        TypeTarget::Builtin("str".to_owned())
                    } else {
                        (**ok).clone()
                    })
                }
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => None,
            }
        }
        RustProjection::MapAsStr
        | RustProjection::MapU64
        | RustProjection::MapI64
        | RustProjection::MapBool
        | RustProjection::MapPathStr => {
            let name = match operation {
                RustProjection::MapU64 => "u64",
                RustProjection::MapI64 => "i64",
                RustProjection::MapBool => "bool",
                _ => "str",
            };
            let result = TypeTarget::Builtin(name.to_owned());
            let result = if operation == RustProjection::MapPathStr {
                reference(&result, false)
            } else {
                result
            };
            match referent(receiver).0 {
                TypeTarget::RustWrapper { .. } => Some(opt_wrap(result)),
                TypeTarget::RustIterator { .. } => Some(iter_of(result)),
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => None,
            }
        }
        RustProjection::MapPathBuf | RustProjection::MapCloned => {
            let (target, borrowed, mutable) = referent(receiver);
            match target {
                TypeTarget::RustWrapper { wrapper, ok, err } => Some(TypeTarget::RustWrapper {
                    wrapper: *wrapper,
                    ok: Box::new(if operation == RustProjection::MapCloned {
                        referent(ok).0.clone()
                    } else {
                        TypeTarget::Builtin("PathBuf".to_owned())
                    }),
                    err: err.clone(),
                }),
                TypeTarget::RustIterator { item } => {
                    Some(if operation == RustProjection::MapPathBuf {
                        iter_of(TypeTarget::Builtin("PathBuf".to_owned()))
                    } else if !borrowed || mutable {
                        iterator_item_by_value(item).map_or(TypeTarget::Unknown, iter_of)
                    } else {
                        TypeTarget::Unknown
                    })
                }
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => None,
            }
        }
        RustProjection::FilterMapOk => match referent(receiver).0 {
            TypeTarget::RustIterator { item } => Some(match item.as_ref() {
                TypeTarget::RustWrapper {
                    wrapper: RustWrapperKind::Result,
                    ok,
                    ..
                } => iter_of((**ok).clone()),
                _ => TypeTarget::Unknown,
            }),
            TypeTarget::RustDeref { wrapper, target }
                if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
            {
                project_structural(target, operation, resolver)
            }
            _ => None,
        },
        RustProjection::MapArray | RustProjection::MapObject | RustProjection::MapTomlTable => {
            match referent(receiver).0 {
                TypeTarget::RustWrapper { .. } => {
                    let json = TypeTarget::External {
                        module: "serde_json::Value".to_owned(),
                        import_line: 0,
                    };
                    let inner = match operation {
                        RustProjection::MapArray => TypeTarget::RustCollection {
                            item: Box::new(json),
                        },
                        RustProjection::MapTomlTable => TypeTarget::External {
                            module: "toml::Table".to_owned(),
                            import_line: 0,
                        },
                        _ => TypeTarget::RustDeref {
                            wrapper: "HashMap",
                            target: Box::new(TypeTarget::RustTuple(vec![
                                TypeTarget::Builtin("String".to_owned()),
                                json,
                            ])),
                        },
                    };
                    Some(opt_wrap(reference(&inner, false)))
                }
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                _ => None,
            }
        }
        RustProjection::IdentitySome => {
            let (target, borrowed, _) = referent(receiver);
            Some(
                if !borrowed
                    && matches!(
                        target,
                        TypeTarget::RustWrapper {
                            wrapper: RustWrapperKind::Option,
                            ..
                        }
                    )
                {
                    target.clone()
                } else {
                    TypeTarget::Unknown
                },
            )
        }
        RustProjection::Index
        | RustProjection::ElementIndex
        | RustProjection::RangeSlice
        | RustProjection::GetRange => {
            let target = referent(receiver).0;
            match target {
                TypeTarget::RustDeref { wrapper, target }
                    if is_deref_smart_pointer(wrapper) || *wrapper == "Range" =>
                {
                    project_structural(target, operation, resolver)
                }
                TypeTarget::RustDeref {
                    wrapper: "HashMap" | "BTreeMap" | "IndexMap",
                    target,
                } if matches!(
                    operation,
                    RustProjection::Index | RustProjection::ElementIndex
                ) =>
                {
                    match target.as_ref() {
                        TypeTarget::RustTuple(pair) => pair.get(1).cloned(),
                        _ => None,
                    }
                }
                TypeTarget::RustCollection { item }
                | TypeTarget::RustSlice { item }
                | TypeTarget::RustTypedContainer { item, .. } => match operation {
                    RustProjection::ElementIndex => Some((**item).clone()),
                    RustProjection::RangeSlice => {
                        Some(TypeTarget::RustSlice { item: item.clone() })
                    }
                    RustProjection::GetRange => {
                        Some(opt_wrap(TypeTarget::RustSlice { item: item.clone() }))
                    }
                    _ => None,
                },
                TypeTarget::Builtin(name)
                    if matches!(
                        operation,
                        RustProjection::RangeSlice | RustProjection::GetRange
                    ) && matches!(name.as_str(), "str" | "String") =>
                {
                    let target = TypeTarget::Builtin("str".to_owned());
                    Some(if operation == RustProjection::GetRange {
                        opt_wrap(target)
                    } else {
                        target
                    })
                }
                TypeTarget::External { module, .. }
                    if matches!(
                        operation,
                        RustProjection::Index | RustProjection::ElementIndex
                    ) =>
                {
                    match import_path(module).as_ref() {
                        "std.collections.HashMap"
                        | "hashbrown.HashMap"
                        | "std.collections.BTreeMap"
                        | "indexmap.IndexMap" => Some(TypeTarget::Unknown),
                        "serde_json.Map" | "serde_json.map.Map" => Some(TypeTarget::External {
                            module: "serde_json::Value".to_owned(),
                            import_line: 0,
                        }),
                        "toml.Table" | "toml.map.Map" => Some(TypeTarget::External {
                            module: "toml::Value".to_owned(),
                            import_line: 0,
                        }),
                        "serde_json.Value" | "serde_yaml.Value" | "toml.Value" => {
                            Some(target.clone())
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        RustProjection::Dereference => {
            if let TypeTarget::RustReference { target, .. } = receiver {
                Some((**target).clone())
            } else {
                deref_target(referent(receiver).0, resolver)
            }
        }
        _ => None,
    }
}

pub(in crate::linker) fn project<'a>(
    receiver: &TypeTarget<'a>,
    member: &str,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let (receiver, borrowed, mutable) = referent(receiver);
    if member == "clone"
        && matches!(
            receiver,
            TypeTarget::Builtin(_)
                | TypeTarget::RustCollection { .. }
                | TypeTarget::RustTypedContainer { .. }
                | TypeTarget::RustSlice { .. }
                | TypeTarget::RustIterator { .. }
                | TypeTarget::RustWrapper { .. }
                | TypeTarget::RustDeref { .. }
                | TypeTarget::RustTuple(_)
        )
    {
        return Some(receiver.clone());
    }
    match receiver {
        TypeTarget::Builtin(name) if matches!(name.as_str(), "str" | "String") => {
            project_string_member(member)
        }
        TypeTarget::RustDeref { wrapper, target } => project_deref_wrapper(
            wrapper, target, receiver, member, borrowed, mutable, resolver,
        ),
        TypeTarget::Builtin(name) if name == "char" => project_char_member(member),
        TypeTarget::Builtin(name) if name == "bool" => match member {
            "then" | "then_some" => Some(opt_wrap(TypeTarget::Unknown)),
            _ => None,
        },
        TypeTarget::Builtin(name)
            if matches!(
                name.as_str(),
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
            ) =>
        {
            match member {
                "checked_add" | "checked_sub" | "checked_mul" | "checked_div" | "checked_rem" => {
                    Some(opt_wrap(TypeTarget::Builtin(name.clone())))
                }
                "saturating_add" | "saturating_sub" | "saturating_mul" => {
                    Some(TypeTarget::Builtin(name.clone()))
                }
                "cmp" | "partial_cmp" => Some(TypeTarget::External {
                    module: "std::cmp::Ordering".to_owned(),
                    import_line: 0,
                }),
                "to_string" => Some(TypeTarget::Builtin("String".to_owned())),
                _ => None,
            }
        }
        TypeTarget::Builtin(_) if matches!(member, "cmp" | "partial_cmp") => {
            Some(TypeTarget::External {
                module: "std::cmp::Ordering".to_owned(),
                import_line: 0,
            })
        }
        TypeTarget::Builtin(_) if member == "to_string" => {
            Some(TypeTarget::Builtin("String".to_owned()))
        }
        TypeTarget::External { module, .. } => {
            project_external_member(receiver, module, member, borrowed, mutable, resolver)
        }
        TypeTarget::RustWrapper { .. } => Some(project_wrapper_member(
            receiver, member, borrowed, mutable, resolver,
        )),
        TypeTarget::RustIterator { item } => Some(project_iterator_member(
            receiver, item, member, borrowed, mutable,
        )),
        TypeTarget::RustTypedContainer { base, item } => match (*base, member) {
            ("VecDeque", "pop_front" | "pop_back") | ("HashSet" | "BTreeSet", "take") => {
                Some(opt_wrap((**item).clone()))
            }
            ("VecDeque", "front" | "back") => Some(opt_wrap(reference(item, false))),
            _ => project_sequence_member(item, receiver, member, borrowed, mutable),
        },
        TypeTarget::RustCollection { item } | TypeTarget::RustSlice { item } => {
            project_sequence_member(item, receiver, member, borrowed, mutable)
        }
        _ if standard_collection(receiver) => {
            let item = match member {
                "iter" => reference(&TypeTarget::Unknown, false),
                "iter_mut" if !borrowed || mutable => reference(&TypeTarget::Unknown, true),
                "into_iter" if borrowed => reference(&TypeTarget::Unknown, mutable),
                "into_iter" => TypeTarget::Unknown,
                _ => return None,
            };
            Some(iter_of(item))
        }
        _ => None,
    }
}

fn project_string_member<'a>(member: &str) -> Option<TypeTarget<'a>> {
    match member {
        _ if is_str_slice_method(member) => Some(TypeTarget::Builtin("str".to_owned())),
        "as_str" | "as_ref" => Some(TypeTarget::Builtin("str".to_owned())),
        "is_empty"
        | "contains"
        | "starts_with"
        | "ends_with"
        | "is_ascii"
        | "eq_ignore_ascii_case" => Some(TypeTarget::Builtin("bool".to_owned())),
        "to_owned" | "to_string" | "replace" | "replacen" | "to_lowercase" | "to_uppercase"
        | "repeat" | "into" => Some(TypeTarget::Builtin("String".to_owned())),
        "as_bytes" | "into_bytes" => Some(TypeTarget::RustSlice {
            item: Box::new(TypeTarget::Builtin("u8".to_owned())),
        }),
        "split" | "rsplit" | "splitn" | "rsplitn" | "split_inclusive" | "split_terminator"
        | "rsplit_terminator" | "lines" | "split_whitespace" => {
            Some(iter_of(TypeTarget::Builtin("str".to_owned())))
        }
        "chars" => Some(iter_of(TypeTarget::Builtin("char".to_owned()))),
        "char_indices" => Some(iter_of(TypeTarget::RustTuple(vec![
            TypeTarget::Builtin("usize".to_owned()),
            TypeTarget::Builtin("char".to_owned()),
        ]))),
        "bytes" => Some(iter_of(TypeTarget::Builtin("u8".to_owned()))),
        "find" | "rfind" => Some(opt_wrap(TypeTarget::Builtin("usize".to_owned()))),
        "strip_prefix" | "strip_suffix" => Some(opt_wrap(TypeTarget::Builtin("str".to_owned()))),
        "split_once" | "rsplit_once" => Some(opt_wrap(TypeTarget::RustTuple(vec![
            TypeTarget::Builtin("str".to_owned()),
            TypeTarget::Builtin("str".to_owned()),
        ]))),
        "cmp" | "partial_cmp" => Some(TypeTarget::External {
            module: "std::cmp::Ordering".to_owned(),
            import_line: 0,
        }),
        _ => None,
    }
}

fn project_char_member<'a>(member: &str) -> Option<TypeTarget<'a>> {
    match member {
        "len_utf8" | "len_utf16" => Some(TypeTarget::Builtin("usize".to_owned())),
        "to_lowercase" | "to_uppercase" | "escape_debug" | "escape_default" | "escape_unicode" => {
            Some(iter_of(TypeTarget::Builtin("char".to_owned())))
        }
        "to_digit" => Some(opt_wrap(TypeTarget::Builtin("u32".to_owned()))),
        "is_control"
        | "is_graphic"
        | "is_digit"
        | "is_whitespace"
        | "is_alphanumeric"
        | "is_alphabetic"
        | "is_numeric"
        | "is_ascii"
        | "is_ascii_digit"
        | "is_ascii_alphabetic"
        | "is_ascii_alphanumeric"
        | "is_ascii_whitespace"
        | "is_ascii_hexdigit"
        | "is_ascii_punctuation"
        | "is_ascii_graphic"
        | "is_ascii_control" => Some(TypeTarget::Builtin("bool".to_owned())),
        "to_string" => Some(TypeTarget::Builtin("String".to_owned())),
        _ => None,
    }
}

fn project_external_member<'a>(
    receiver: &TypeTarget<'a>,
    module: &str,
    member: &str,
    borrowed: bool,
    mutable: bool,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let ext_path = import_path(module);
    if matches!(
        ext_path.as_ref(),
        "std.collections.HashMap"
            | "hashbrown.HashMap"
            | "std.collections.BTreeMap"
            | "indexmap.IndexMap"
            | "serde_json.Map"
            | "serde_json.map.Map"
    ) {
        let wrapper = match ext_path.as_ref() {
            "std.collections.BTreeMap" => "BTreeMap",
            "indexmap.IndexMap" => "IndexMap",
            _ => "HashMap",
        };
        let pair = if matches!(ext_path.as_ref(), "serde_json.Map" | "serde_json.map.Map") {
            TypeTarget::RustTuple(vec![
                TypeTarget::Builtin("String".to_owned()),
                TypeTarget::External {
                    module: "serde_json::Value".to_owned(),
                    import_line: 0,
                },
            ])
        } else {
            TypeTarget::RustTuple(vec![TypeTarget::Unknown, TypeTarget::Unknown])
        };
        return project_deref_wrapper(
            wrapper, &pair, receiver, member, borrowed, mutable, resolver,
        );
    }
    if matches!(
        ext_path.as_ref(),
        "std.sync.OnceLock"
            | "std.sync.Mutex"
            | "parking_lot.Mutex"
            | "tokio.sync.Mutex"
            | "std.sync.RwLock"
            | "parking_lot.RwLock"
            | "tokio.sync.RwLock"
            | "std.cell.RefCell"
            | "tokio.task.JoinHandle"
    ) {
        let wrapper = match ext_path.as_ref() {
            "std.sync.OnceLock" => "OnceLock",
            "std.sync.Mutex" | "parking_lot.Mutex" | "tokio.sync.Mutex" => "Mutex",
            "std.sync.RwLock" | "parking_lot.RwLock" | "tokio.sync.RwLock" => "RwLock",
            "std.cell.RefCell" => "RefCell",
            _ => "JoinHandle",
        };
        return project_deref_wrapper(
            wrapper,
            &TypeTarget::RustTuple(vec![TypeTarget::Unknown]),
            receiver,
            member,
            borrowed,
            mutable,
            resolver,
        );
    }
    if ext_path.as_ref() == "std.cmp.Ordering" {
        return match member {
            "then" | "then_with" | "reverse" => Some(TypeTarget::External {
                module: "std::cmp::Ordering".to_owned(),
                import_line: 0,
            }),
            "is_eq" | "is_ne" | "is_lt" | "is_le" | "is_gt" | "is_ge" => {
                Some(TypeTarget::Builtin("bool".to_owned()))
            }
            _ => None,
        };
    }
    if ext_path.as_ref() == "std.fs.OpenOptions" {
        return match member {
            "read" | "write" | "append" | "truncate" | "create" | "create_new" | "mode"
            | "custom_flags" => Some(receiver.clone()),
            "open" => Some(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(TypeTarget::External {
                    module: "std::fs::File".to_owned(),
                    import_line: 0,
                }),
                err: Some(Box::new(TypeTarget::External {
                    module: "std::io::Error".to_owned(),
                    import_line: 0,
                })),
            }),
            _ => None,
        };
    }
    if ext_path.as_ref() == "std.fs.ReadDir" {
        let entry = TypeTarget::External {
            module: "std::fs::DirEntry".to_owned(),
            import_line: 0,
        };
        let result_entry = TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(entry.clone()),
            err: Some(Box::new(TypeTarget::External {
                module: "std::io::Error".to_owned(),
                import_line: 0,
            })),
        };
        return match member {
            "flatten" => Some(iter_of(entry)),
            "into_iter" => Some(iter_of(result_entry)),
            "next" => Some(opt_wrap(result_entry)),
            _ => None,
        };
    }
    if member == "to_string"
        && matches!(
            ext_path.as_ref(),
            "std.io.Error"
                | "anyhow.Error"
                | "serde_json.Value"
                | "serde_json.Error"
                | "serde_yaml.Error"
                | "toml.de.Error"
                | "toml.ser.Error"
                | "ignore.Error"
                | "std.fmt.Arguments"
        )
    {
        return Some(TypeTarget::Builtin("String".to_owned()));
    }
    None
}

fn project_wrapper_member<'a>(
    receiver: &TypeTarget<'a>,
    member: &str,
    borrowed: bool,
    mutable: bool,
    resolver: &dyn SemanticResolver<'a>,
) -> TypeTarget<'a> {
    let TypeTarget::RustWrapper { wrapper, ok, err } = receiver else {
        return TypeTarget::Unknown;
    };
    let (wrapper, ok) = (*wrapper, ok.as_ref());
    match member {
        "?" | "unwrap" | "expect" | "unwrap_or" | "unwrap_or_else" | "unwrap_or_default"
            if !borrowed =>
        {
            ok.clone()
        }
        "ok" if wrapper == RustWrapperKind::Result && !borrowed => opt_wrap(ok.clone()),
        "err" if wrapper == RustWrapperKind::Result && !borrowed => {
            opt_wrap(err.as_deref().cloned().unwrap_or(TypeTarget::Unknown))
        }
        "ok_or" | "ok_or_else" if wrapper == RustWrapperKind::Option && !borrowed => {
            TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(ok.clone()),
                err: Some(Box::new(TypeTarget::Unknown)),
            }
        }
        "transpose" if !borrowed => match (wrapper, ok) {
            (
                RustWrapperKind::Option,
                TypeTarget::RustWrapper {
                    wrapper: RustWrapperKind::Result,
                    ok: inner_ok,
                    err: inner_err,
                },
            ) => TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(opt_wrap((**inner_ok).clone())),
                err: inner_err.clone(),
            },
            (
                RustWrapperKind::Result,
                TypeTarget::RustWrapper {
                    wrapper: RustWrapperKind::Option,
                    ok: inner_ok,
                    ..
                },
            ) => opt_wrap(TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: inner_ok.clone(),
                err: err.clone(),
            }),
            _ => TypeTarget::Unknown,
        },
        "flatten" if !borrowed => match (wrapper, ok) {
            (
                RustWrapperKind::Option,
                TypeTarget::RustWrapper {
                    wrapper: RustWrapperKind::Option,
                    ok: inner_ok,
                    ..
                },
            ) => opt_wrap((**inner_ok).clone()),
            _ => TypeTarget::Unknown,
        },
        "or" | "or_else" if wrapper == RustWrapperKind::Option && !borrowed => receiver.clone(),
        "context" | "with_context" if !borrowed => TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(ok.clone()),
            err: Some(Box::new(TypeTarget::External {
                module: "anyhow::Error".to_owned(),
                import_line: 0,
            })),
        },
        "get_or_insert" | "get_or_insert_with"
            if wrapper == RustWrapperKind::Option && (!borrowed || mutable) =>
        {
            reference(ok, true)
        }
        "as_ref" => TypeTarget::RustWrapper {
            wrapper,
            ok: Box::new(reference(ok, false)),
            err: err
                .as_deref()
                .map(|error| Box::new(reference(error, false))),
        },
        "as_mut" if !borrowed || mutable => TypeTarget::RustWrapper {
            wrapper,
            ok: Box::new(reference(ok, true)),
            err: err.as_deref().map(|error| Box::new(reference(error, true))),
        },
        "as_deref" => deref_target(ok, resolver).map_or(TypeTarget::Unknown, |target| {
            TypeTarget::RustWrapper {
                wrapper,
                ok: Box::new(reference(&target, false)),
                err: err
                    .as_deref()
                    .map(|error| Box::new(reference(error, false))),
            }
        }),
        "as_deref_mut" if !borrowed || mutable => {
            deref_target(ok, resolver).map_or(TypeTarget::Unknown, |target| {
                TypeTarget::RustWrapper {
                    wrapper,
                    ok: Box::new(reference(&target, true)),
                    err: err.as_deref().map(|error| Box::new(reference(error, true))),
                }
            })
        }
        "cloned" | "copied" if !borrowed || wrapper == RustWrapperKind::Option => {
            clone_shared_reference(ok).map_or(TypeTarget::Unknown, |ok| TypeTarget::RustWrapper {
                wrapper,
                ok: Box::new(ok),
                err: err.clone(),
            })
        }
        "filter" | "map_err" | "inspect" | "inspect_err" => TypeTarget::RustWrapper {
            wrapper,
            ok: Box::new(ok.clone()),
            err: err.clone(),
        },
        "is_some" | "is_none" | "is_ok" | "is_err" | "is_some_and" | "is_none_or" | "is_ok_and"
        | "is_err_and" => TypeTarget::Builtin("bool".to_owned()),
        "iter" => iter_of(reference(ok, false)),
        "into_iter" => iter_of(if borrowed {
            reference(ok, mutable)
        } else {
            ok.clone()
        }),
        _ => TypeTarget::Unknown,
    }
}

fn project_iterator_member<'a>(
    receiver: &TypeTarget<'a>,
    item: &TypeTarget<'a>,
    member: &str,
    borrowed: bool,
    mutable: bool,
) -> TypeTarget<'a> {
    match member {
        "next" | "find" | "peek" | "peek_mut" | "max_by" | "min_by" | "max_by_key"
        | "min_by_key" | "reduce"
            if !borrowed || mutable =>
        {
            opt_wrap(item.clone())
        }
        "position" | "rposition" if !borrowed || mutable => {
            opt_wrap(TypeTarget::Builtin("usize".to_owned()))
        }
        "cloned" | "copied" if !borrowed || mutable => {
            iterator_item_by_value(item).map_or(TypeTarget::Unknown, iter_of)
        }
        "into_iter" | "filter" | "take" | "skip" | "take_while" | "skip_while" | "step_by"
        | "inspect" | "fuse" | "rev" | "peekable" => receiver.clone(),
        "find_map" if !borrowed || mutable => opt_wrap(TypeTarget::Unknown),
        "partition" if !borrowed || mutable => TypeTarget::RustTuple(vec![
            TypeTarget::RustCollection {
                item: Box::new(item.clone()),
            },
            TypeTarget::RustCollection {
                item: Box::new(item.clone()),
            },
        ]),
        "chain" => iter_of(item.clone()),
        "zip" => iter_of(TypeTarget::RustTuple(vec![
            item.clone(),
            TypeTarget::Unknown,
        ])),
        "flatten" => match referent(item).0 {
            TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Option | RustWrapperKind::Result,
                ok,
                ..
            }
            | TypeTarget::RustCollection { item: ok }
            | TypeTarget::RustSlice { item: ok }
            | TypeTarget::RustIterator { item: ok } => iter_of((**ok).clone()),
            _ => TypeTarget::Unknown,
        },
        "enumerate" => iter_of(TypeTarget::RustTuple(vec![
            TypeTarget::Builtin("usize".to_owned()),
            item.clone(),
        ])),
        "filter_entry" => iter_of(item.clone()),
        "map" | "filter_map" | "flat_map" => iter_of(reference(&TypeTarget::Unknown, false)),
        "collect" => TypeTarget::RustCollection {
            item: Box::new(item.clone()),
        },
        _ => TypeTarget::Unknown,
    }
}

fn project_closure_tuple<'a>(
    items: &[TypeTarget<'a>],
    operation: crate::semantic::types::RustProjection,
) -> Option<TypeTarget<'a>> {
    use crate::semantic::types::RustProjection;
    let mut recv = referent(items.first()?).0;
    while let TypeTarget::RustDeref {
        wrapper,
        target: inner,
    } = recv
    {
        if !is_deref_smart_pointer(wrapper) {
            break;
        }
        recv = referent(inner).0;
    }
    let mapped = items.get(1)?;
    let default = items.get(2);
    let is_seq = matches!(
        recv,
        TypeTarget::RustIterator { .. }
            | TypeTarget::RustCollection { .. }
            | TypeTarget::RustSlice { .. }
            | TypeTarget::RustDeref {
                wrapper: "Range",
                ..
            }
    ) || matches!(recv, TypeTarget::External { module, .. } if import_path(module).as_ref() == "std.fs.ReadDir");
    match operation {
        RustProjection::ClosureMap => match recv {
            TypeTarget::RustWrapper { wrapper, err, .. } => Some(TypeTarget::RustWrapper {
                wrapper: *wrapper,
                ok: Box::new(mapped.clone()),
                err: err.clone(),
            }),
            _ if is_seq => Some(TypeTarget::RustIterator {
                item: Box::new(mapped.clone()),
            }),
            _ => None,
        },
        RustProjection::ClosureMapOr if matches!(recv, TypeTarget::RustWrapper { .. }) => {
            if !matches!(mapped, TypeTarget::Unknown) {
                return Some(mapped.clone());
            }
            Some(
                default
                    .filter(|d| !matches!(d, TypeTarget::Unknown))
                    .cloned()
                    .unwrap_or(TypeTarget::Unknown),
            )
        }
        _ => None,
    }
}

fn unwrap_filter_map_item<'a>(mapped: &TypeTarget<'a>) -> TypeTarget<'a> {
    match mapped {
        TypeTarget::RustWrapper { ok, .. } => (**ok).clone(),
        _ => reference(&TypeTarget::Unknown, false),
    }
}

fn unwrap_flat_map_item<'a>(mapped: &TypeTarget<'a>) -> TypeTarget<'a> {
    match referent(mapped).0 {
        TypeTarget::RustIterator { item }
        | TypeTarget::RustCollection { item }
        | TypeTarget::RustSlice { item }
        | TypeTarget::RustWrapper { ok: item, .. } => (**item).clone(),
        TypeTarget::External { module, .. } if import_path(module).as_ref() == "std.fs.ReadDir" => {
            TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ok: Box::new(TypeTarget::External {
                    module: "std::fs::DirEntry".to_owned(),
                    import_line: 0,
                }),
                err: Some(Box::new(TypeTarget::External {
                    module: "std::io::Error".to_owned(),
                    import_line: 0,
                })),
            }
        }
        _ => reference(&TypeTarget::Unknown, false),
    }
}

pub(in crate::linker) fn unwrap_payload<'a>(target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
    match target {
        TypeTarget::RustWrapper { ok, .. } => Some((**ok).clone()),
        _ => None,
    }
}

fn is_deref_smart_pointer(wrapper: &str) -> bool {
    matches!(
        wrapper,
        "Box"
            | "Arc"
            | "Rc"
            | "Cow"
            | "MutexGuard"
            | "RwLockReadGuard"
            | "RwLockWriteGuard"
            | "Ref"
            | "RefMut"
    )
}

fn project_deref_wrapper<'a>(
    wrapper: &str,
    target: &TypeTarget<'a>,
    receiver: &TypeTarget<'a>,
    member: &str,
    borrowed: bool,
    mutable: bool,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    let opt = opt_wrap;
    let res = |ok: TypeTarget<'a>| TypeTarget::RustWrapper {
        wrapper: RustWrapperKind::Result,
        ok: Box::new(ok),
        err: None,
    };
    if is_deref_smart_pointer(wrapper) {
        return match member {
            "into_owned" if wrapper == "Cow" => Some(match target {
                TypeTarget::Builtin(name) if name == "str" => {
                    TypeTarget::Builtin("String".to_owned())
                }
                TypeTarget::RustSlice { item } => TypeTarget::RustCollection { item: item.clone() },
                other => other.clone(),
            }),
            "to_mut" if wrapper == "Cow" && (!borrowed || mutable) => Some(reference(target, true)),
            "as_ref" => Some(reference(target, false)),
            "as_mut" if wrapper == "Box" && (!borrowed || mutable) => Some(reference(target, true)),
            _ => project(target, member, resolver),
        };
    }
    match wrapper {
        "Range" => match member {
            "contains" | "is_empty" => Some(TypeTarget::Builtin("bool".to_owned())),
            "start" | "end" => match target {
                TypeTarget::RustIterator { item } => Some((**item).clone()),
                _ => None,
            },
            _ => project(target, member, resolver),
        },
        "HashMap" | "BTreeMap" | "IndexMap" => {
            let TypeTarget::RustTuple(pair) = target else {
                return None;
            };
            let (k, v) = (pair.first()?, pair.get(1)?);
            match member {
                "get" => Some(opt(reference(v, false))),
                "get_mut" if !borrowed || mutable => Some(opt(reference(v, true))),
                "get_key_value" => Some(opt(TypeTarget::RustTuple(vec![
                    reference(k, false),
                    reference(v, false),
                ]))),
                "insert" | "remove" | "swap_remove" | "shift_remove" if !borrowed || mutable => {
                    Some(opt(v.clone()))
                }
                "iter" | "par_iter" => Some(TypeTarget::RustIterator {
                    item: Box::new(TypeTarget::RustTuple(vec![
                        reference(k, false),
                        reference(v, false),
                    ])),
                }),
                "iter_mut" | "par_iter_mut" if !borrowed || mutable => {
                    Some(TypeTarget::RustIterator {
                        item: Box::new(TypeTarget::RustTuple(vec![
                            reference(k, false),
                            reference(v, true),
                        ])),
                    })
                }
                "into_iter" => {
                    let item = if borrowed {
                        TypeTarget::RustTuple(vec![reference(k, false), reference(v, mutable)])
                    } else {
                        TypeTarget::RustTuple(vec![k.clone(), v.clone()])
                    };
                    Some(TypeTarget::RustIterator {
                        item: Box::new(item),
                    })
                }
                "keys" => Some(TypeTarget::RustIterator {
                    item: Box::new(reference(k, false)),
                }),
                "into_keys" if !borrowed => Some(TypeTarget::RustIterator {
                    item: Box::new(k.clone()),
                }),
                "values" => Some(TypeTarget::RustIterator {
                    item: Box::new(reference(v, false)),
                }),
                "values_mut" if !borrowed || mutable => Some(TypeTarget::RustIterator {
                    item: Box::new(reference(v, true)),
                }),
                "into_values" if !borrowed => Some(TypeTarget::RustIterator {
                    item: Box::new(v.clone()),
                }),
                "entry" if !borrowed || mutable => Some(TypeTarget::RustDeref {
                    wrapper: "MapEntry",
                    target: Box::new(TypeTarget::RustTuple(vec![v.clone(), k.clone()])),
                }),
                _ => None,
            }
        }
        "MapEntry" => {
            let TypeTarget::RustTuple(items) = target else {
                return None;
            };
            let v = items.first()?;
            match member {
                "or_insert" | "or_insert_with" | "or_insert_with_key" | "or_default" | "insert"
                | "insert_entry" => Some(reference(v, true)),
                "key" => items.get(1).map(|k| reference(k, false)),
                "get" => Some(reference(v, false)),
                "get_mut" | "into_mut" => Some(reference(v, true)),
                "remove" => Some(v.clone()),
                "remove_entry" => items
                    .get(1)
                    .map(|k| TypeTarget::RustTuple(vec![k.clone(), v.clone()])),
                "and_modify" => Some(receiver.clone()),
                _ => None,
            }
        }
        "OnceLock" => {
            let TypeTarget::RustTuple(items) = target else {
                return None;
            };
            let inner = items.first()?;
            match member {
                "get" => Some(opt(reference(inner, false))),
                "get_mut" if !borrowed || mutable => Some(opt(reference(inner, true))),
                "get_or_init" => Some(reference(inner, false)),
                "get_or_try_init" => Some(res(reference(inner, false))),
                "take" | "into_inner" => Some(opt(inner.clone())),
                _ => None,
            }
        }
        "Mutex" | "RwLock" | "RefCell" | "JoinHandle" => {
            let TypeTarget::RustTuple(items) = target else {
                return None;
            };
            let inner = items.first()?;
            match (wrapper, member) {
                ("Mutex", "lock" | "try_lock") => Some(res(TypeTarget::RustDeref {
                    wrapper: "MutexGuard",
                    target: Box::new(reference(inner, true)),
                })),
                ("RwLock", "read" | "try_read") => Some(res(TypeTarget::RustDeref {
                    wrapper: "RwLockReadGuard",
                    target: Box::new(reference(inner, false)),
                })),
                ("RwLock", "write" | "try_write") => Some(res(TypeTarget::RustDeref {
                    wrapper: "RwLockWriteGuard",
                    target: Box::new(reference(inner, true)),
                })),
                ("Mutex" | "RwLock", "into_inner") | ("JoinHandle", "join") => {
                    Some(res(inner.clone()))
                }
                ("Mutex" | "RwLock", "get_mut") if !borrowed || mutable => {
                    Some(res(reference(inner, true)))
                }
                ("RefCell", "borrow") => Some(TypeTarget::RustDeref {
                    wrapper: "Ref",
                    target: Box::new(reference(inner, false)),
                }),
                ("RefCell", "try_borrow") => Some(res(TypeTarget::RustDeref {
                    wrapper: "Ref",
                    target: Box::new(reference(inner, false)),
                })),
                ("RefCell", "borrow_mut") => Some(TypeTarget::RustDeref {
                    wrapper: "RefMut",
                    target: Box::new(reference(inner, true)),
                }),
                ("RefCell", "try_borrow_mut") => Some(res(TypeTarget::RustDeref {
                    wrapper: "RefMut",
                    target: Box::new(reference(inner, true)),
                })),
                ("RefCell", "into_inner" | "take" | "replace" | "replace_with") => {
                    Some(inner.clone())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

pub(in crate::linker) fn local_class<'a>(mut target: &TypeTarget<'a>) -> Option<&'a Node> {
    loop {
        match target {
            TypeTarget::RustReference { target: inner, .. } => target = inner,
            TypeTarget::RustDeref {
                wrapper,
                target: inner,
            } if is_deref_smart_pointer(wrapper) => target = inner,
            _ => break,
        }
    }
    match target {
        TypeTarget::Local(node) => Some(*node),
        _ => None,
    }
}

pub(in crate::linker) fn callback_receiver<'a>(
    mut receiver: &TypeTarget<'a>,
    method: &str,
) -> Option<TypeTarget<'a>> {
    while matches!(receiver, TypeTarget::RustReference { .. })
        || matches!(receiver, TypeTarget::RustDeref { wrapper, .. } if *wrapper != "LocalKey")
    {
        match receiver {
            TypeTarget::RustReference { target, .. } | TypeTarget::RustDeref { target, .. } => {
                receiver = target
            }
            _ => break,
        }
    }
    match receiver {
        TypeTarget::RustDeref {
            wrapper: "LocalKey",
            target,
        } if matches!(method, "with" | "try_with") => Some(match target.as_ref() {
            TypeTarget::RustTuple(items) => {
                items
                    .first()
                    .map_or(TypeTarget::Unknown, |inner| TypeTarget::RustReference {
                        mutable: false,
                        target: Box::new(inner.clone()),
                    })
            }
            _ => TypeTarget::Unknown,
        }),
        TypeTarget::Promise(inner) if matches!(method, "then" | "finally") => {
            Some(if method == "then" {
                (**inner).clone()
            } else {
                TypeTarget::Unknown
            })
        }
        TypeTarget::Collection { item, .. }
            if crate::linker::value_flow::collection_callback(method) =>
        {
            Some((**item).clone())
        }
        TypeTarget::RustIterator { item }
        | TypeTarget::RustCollection { item }
        | TypeTarget::RustSlice { item }
        | TypeTarget::RustTypedContainer { item, .. }
            if crate::linker::value_flow::collection_callback(method) =>
        {
            Some((**item).clone())
        }
        TypeTarget::RustWrapper { ok, .. }
            if matches!(
                method,
                "map"
                    | "and_then"
                    | "inspect"
                    | "filter"
                    | "is_some_and"
                    | "is_none_or"
                    | "ok_or_else"
                    | "map_or"
                    | "map_or_else"
            ) =>
        {
            Some((**ok).clone())
        }
        TypeTarget::RustWrapper { err: Some(err), .. }
            if matches!(method, "map_err" | "or_else" | "inspect_err") =>
        {
            Some((**err).clone())
        }
        _ => None,
    }
}

pub(in crate::linker) fn evaluate_language_value<'a>(
    linker: &dyn crate::languages::profile::LanguageLinker,
    eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
    expression: &ValueExpr,
    limits: crate::linker::value_flow::SemanticLimits,
    evaluate: &dyn Fn(&ValueExpr) -> TypeTarget<'a>,
    payload: &dyn Fn(&ValueExpr) -> TypeTarget<'a>,
    join: &dyn Fn(Vec<TypeTarget<'a>>) -> TypeTarget<'a>,
) -> Option<TypeTarget<'a>> {
    use crate::languages::profile::LanguageValueProjection as Projection;
    use crate::semantic::types::RustProjection;

    let (value, member, operation) = match expression {
        ValueExpr::RustClosure => return Some(TypeTarget::RustClosure),
        ValueExpr::RustField { value, member } => (
            value.as_ref(),
            member.as_str(),
            Projection::Field(member.clone()),
        ),
        ValueExpr::RustProject { value, operation } => {
            let projection = match operation {
                RustProjection::GenericCall => Projection::GenericCall,
                RustProjection::Array => Projection::Array,
                RustProjection::Tuple => Projection::Tuple,
                RustProjection::BranchJoin => Projection::BranchJoin,
                RustProjection::Reference { mutable } => {
                    Projection::Reference { mutable: *mutable }
                }
                special => {
                    if !linker.structural_operation_admitted(
                        eval.owner,
                        *special,
                        eval.at,
                        eval.resolver,
                    ) {
                        return Some(TypeTarget::Unknown);
                    }
                    return Some(
                        linker
                            .structural_value(&evaluate(value), *special, eval.resolver)
                            .unwrap_or(TypeTarget::Unknown),
                    );
                }
            };
            (value.as_ref(), "", projection)
        }
        ValueExpr::Project { value, member } => (
            value.as_ref(),
            member.as_str(),
            linker.value_projection(member)?,
        ),
        _ => return None,
    };
    if matches!(operation, Projection::GenericCall) {
        let ValueExpr::BranchJoin { alternatives } = value else {
            return Some(TypeTarget::Unknown);
        };
        let [call @ ValueExpr::Call { callee, .. }, ValueExpr::Annotated { type_expr }] =
            alternatives.as_slice()
        else {
            return Some(TypeTarget::Unknown);
        };
        return Some(
            linker
                .deserialize_value(
                    eval.owner,
                    callee,
                    type_expr,
                    eval.at,
                    eval.resolver,
                    limits,
                )
                .unwrap_or_else(|| evaluate(call)),
        );
    }
    if matches!(
        operation,
        Projection::Array | Projection::Tuple | Projection::BranchJoin
    ) {
        let ValueExpr::BranchJoin { alternatives } = value else {
            return Some(TypeTarget::Unknown);
        };
        if matches!(operation, Projection::Tuple) {
            return Some(TypeTarget::RustTuple(
                alternatives.iter().map(evaluate).collect(),
            ));
        }
        if matches!(operation, Projection::BranchJoin) {
            return Some(join(alternatives.iter().map(evaluate).collect()));
        }
        let mut items = alternatives.iter().map(evaluate);
        let item = items.next().unwrap_or(TypeTarget::Unknown);
        let item =
            if items.all(|other| crate::linker::value_flow::types::same_target(&item, &other)) {
                item
            } else {
                TypeTarget::Unknown
            };
        return Some(TypeTarget::RustCollection {
            item: Box::new(item),
        });
    }
    if let Projection::Reference { mutable } = operation {
        return Some(TypeTarget::RustReference {
            target: Box::new(evaluate(value)),
            mutable,
        });
    }
    if let Projection::Field(field) = operation {
        let source = if matches!(value, ValueExpr::Alias { name } if name == "self")
            && linker.lexical_receiver_field(eval.owner, "self")
        {
            crate::linker::value_flow::types::symbol_type(eval.resolver.resolve_symbol(
                eval.owner,
                "Self",
                eval.at,
                crate::linker::value_flow::SymbolRole::Type,
            ))
        } else {
            evaluate(value)
        };
        if let TypeTarget::RustTuple(items) = linker.unwrap_reference(&source) {
            return Some(
                field
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| items.get(index))
                    .cloned()
                    .unwrap_or(TypeTarget::Unknown),
            );
        }
        return Some(
            linker
                .local_receiver_class(&source)
                .and_then(|class| eval.index.field_type_tracked(class, &field, eval.trace))
                .cloned()
                .or_else(|| {
                    if matches!(
                        &source,
                        TypeTarget::External { .. } | TypeTarget::Builtin(_)
                    ) {
                        eval.resolver
                            .resolve_member_result(&source, &field, eval.owner, eval.at)
                    } else {
                        None
                    }
                })
                .unwrap_or(TypeTarget::Unknown),
        );
    }
    if !matches!(operation, Projection::Adapter) {
        return None;
    }
    let receiver = evaluate(value);
    let should_payload = || {
        matches!(member, "?" | "unwrap" | "expect")
            && (matches!(&receiver, TypeTarget::Unknown)
                || matches!(&receiver, TypeTarget::Builtin(name) if name == "Option" || name == "Result"))
    };
    if let Some(projected) = linker.adapter_value(&receiver, member, eval.resolver) {
        return Some(if !matches!(projected, TypeTarget::Unknown) {
            projected
        } else if should_payload() {
            payload(value)
        } else {
            TypeTarget::Unknown
        });
    }
    if member != "?" {
        if let Symbol::Callable(callable) = eval
            .resolver
            .resolve_member(&receiver, member, eval.owner, eval.at)
        {
            if let Some(trace) = eval.trace {
                trace.returned(callable.id.as_str());
            }
            if let Some(result) = eval
                .index
                .return_type_tracked(callable, None)
                .filter(|target| !matches!(target, TypeTarget::Unknown))
            {
                return Some(result.clone());
            }
            return Some(
                linker
                    .member_call_result(&receiver, member, eval.owner, eval.at, eval.resolver)
                    .unwrap_or(TypeTarget::Unknown),
            );
        }
    }
    Some(
        linker
            .member_call_result(&receiver, member, eval.owner, eval.at, eval.resolver)
            .unwrap_or_else(|| {
                if should_payload() {
                    payload(value)
                } else {
                    TypeTarget::Unknown
                }
            }),
    )
}

pub(in crate::linker) fn join_targets<'a>(
    targets: &[TypeTarget<'a>],
    join: &dyn Fn(Vec<TypeTarget<'a>>) -> TypeTarget<'a>,
) -> Option<TypeTarget<'a>> {
    use crate::linker::value_flow::RustWrapperKind;
    if let TypeTarget::RustTuple(first) = targets.first()? {
        if targets.iter().all(
            |target| matches!(target, TypeTarget::RustTuple(items) if items.len() == first.len()),
        ) {
            let joined = (0..first.len())
                .map(|index| {
                    join(
                        targets
                            .iter()
                            .filter_map(|target| match target {
                                TypeTarget::RustTuple(items) => Some(items[index].clone()),
                                _ => None,
                            })
                            .collect(),
                    )
                })
                .collect();
            return Some(TypeTarget::RustTuple(joined));
        }
    }
    let is_option = |target: &TypeTarget<'a>| {
        matches!(
            target,
            TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Option,
                ..
            }
        )
    };
    if targets.iter().any(is_option)
        && targets.iter().all(|target| {
            is_option(target)
                || matches!(target, TypeTarget::Unknown)
                || matches!(target, TypeTarget::Builtin(name) if name == "Option")
        })
    {
        let payloads = targets
            .iter()
            .filter_map(|target| match target {
                TypeTarget::RustWrapper { ok, .. } => Some((**ok).clone()),
                _ => None,
            })
            .collect();
        return Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(join(payloads)),
            err: None,
        });
    }
    if targets.iter().all(|target| {
        matches!(
            target,
            TypeTarget::RustWrapper {
                wrapper: RustWrapperKind::Result,
                ..
            }
        )
    }) {
        let mut oks = Vec::with_capacity(targets.len());
        let mut errs = Vec::with_capacity(targets.len());
        for target in targets {
            if let TypeTarget::RustWrapper { ok, err, .. } = target {
                oks.push((**ok).clone());
                errs.extend(err.as_deref().cloned());
            }
        }
        let err = (errs.len() == oks.len()).then(|| Box::new(join(errs)));
        return Some(TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(join(oks)),
            err,
        });
    }
    None
}

pub(in crate::linker) fn project_deref_fields<'a>(
    fields: &mut HashMap<&'a str, HashMap<String, TypeTarget<'a>>>,
    owners: &[&'a Node],
    resolver: &dyn SemanticResolver<'a>,
) {
    let mut projected = Vec::new();
    for owner in owners.iter().filter(|owner| {
        owner.language == crate::models::tags::LanguageId::Rust
            && owner.kind == crate::models::tags::NodeKind::Struct
    }) {
        let mut target = *owner;
        for _ in 0..64 {
            let Some(next) = resolver.deref_target(target) else {
                break;
            };
            if let Some(inherited) = fields.get(next.id.as_str()) {
                for (name, value) in inherited {
                    projected.push((owner.id.as_str(), name.clone(), value.clone()));
                }
            }
            target = next;
        }
    }
    for (owner, name, value) in projected {
        fields
            .entry(owner)
            .or_default()
            .entry(name)
            .or_insert(value);
    }
}

pub(in crate::linker) fn referent<'target, 'a>(
    target: &'target TypeTarget<'a>,
) -> (&'target TypeTarget<'a>, bool, bool) {
    let mut referent = target;
    let mut borrowed = false;
    let mut mutable = true;
    for _ in 0..8 {
        let TypeTarget::RustReference {
            mutable: reference_mutable,
            target,
        } = referent
        else {
            break;
        };
        borrowed = true;
        mutable &= *reference_mutable;
        referent = target;
    }
    (referent, borrowed, borrowed && mutable)
}

fn reference<'a>(target: &TypeTarget<'a>, mutable: bool) -> TypeTarget<'a> {
    TypeTarget::RustReference {
        mutable,
        target: Box::new(target.clone()),
    }
}

fn clone_shared_reference<'a>(target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
    match target {
        TypeTarget::RustReference {
            mutable: false,
            target,
        } => Some((**target).clone()),
        _ => None,
    }
}

fn iterator_item_by_value<'a>(target: &TypeTarget<'a>) -> Option<TypeTarget<'a>> {
    clone_shared_reference(target)
}

fn deref_target<'a>(
    target: &TypeTarget<'a>,
    resolver: &dyn SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    match target {
        TypeTarget::RustReference { target, .. } => Some((**target).clone()),
        TypeTarget::RustDeref { wrapper, target } if is_deref_smart_pointer(wrapper) => {
            Some((**target).clone())
        }
        TypeTarget::Local(node) => resolver.deref_target(node).map(TypeTarget::Local),
        TypeTarget::Builtin(name) if name == "String" => {
            Some(TypeTarget::Builtin("str".to_owned()))
        }
        TypeTarget::Builtin(name) if name == "PathBuf" => {
            Some(TypeTarget::Builtin("Path".to_owned()))
        }
        TypeTarget::External { module, .. }
            if matches!(
                import_path(module).as_ref(),
                "std.string.String" | "alloc.string.String"
            ) =>
        {
            Some(TypeTarget::Builtin("str".to_owned()))
        }
        TypeTarget::RustCollection { item } => Some(TypeTarget::RustSlice { item: item.clone() }),
        _ => None,
    }
}

pub(in crate::linker) fn visible(owner: &Node, candidate: &Node) -> Option<bool> {
    if candidate.language == crate::models::tags::LanguageId::Rust
        && candidate.kind == crate::models::tags::NodeKind::Impl
    {
        Some(false)
    } else if owner.language == crate::models::tags::LanguageId::Rust {
        Some(true)
    } else {
        None
    }
}

pub(in crate::linker) fn reference_target<'target, 'node>(
    target: &'target TypeTarget<'node>,
) -> Option<&'target TypeTarget<'node>> {
    match target {
        TypeTarget::RustReference { target, .. } => Some(target),
        _ => None,
    }
}
