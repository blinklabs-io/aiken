use super::{
    air::ExpectLevel,
    interner::AirInterner,
    tree::{AirMsg, AirTree},
};
use crate::{
    ast::{
        DataTypeKey, FunctionAccessKey, Pattern, Span, TraceLevel, TypedArg, TypedAssignmentKind,
        TypedClause, TypedDataType, TypedPattern,
    },
    expr::TypedExpr,
    line_numbers::{LineColumn, LineNumbers},
    tipo::{
        check_replaceable_opaque_type, convert_opaque_type, find_and_replace_generics,
        lookup_data_type_by_tipo, PatternConstructor, Type, ValueConstructor,
        ValueConstructorVariant,
    },
};
use indexmap::{IndexMap, IndexSet};
use itertools::{Itertools, Position};
use std::{ops::Deref, rc::Rc};
use uplc::{
    ast::{Constant as UplcConstant, Name, Term, Type as UplcType},
    builder::{CONSTR_FIELDS_EXPOSER, CONSTR_INDEX_EXPOSER},
    builtins::DefaultFunction,
    machine::{
        runtime::{convert_constr_to_tag, Compressable, ANY_TAG},
        value::to_pallas_bigint,
    },
    Constr, KeyValuePairs, PlutusData,
};

pub type Variant = String;

pub type Params = Vec<String>;

pub type CycleFunctionNames = Vec<String>;

pub const TOO_MANY_ITEMS: &str = "__TOO_MANY_ITEMS";
pub const LIST_NOT_EMPTY: &str = "__LIST_NOT_EMPTY";
pub const CONSTR_NOT_EMPTY: &str = "__CONSTR_NOT_EMPTY";
pub const INCORRECT_BOOLEAN: &str = "__INCORRECT_BOOLEAN";
pub const INCORRECT_CONSTR: &str = "__INCORRECT_CONSTR";
pub const CONSTR_INDEX_MISMATCH: &str = "__CONSTR_INDEX_MISMATCH";
pub const DISCARDED: &str = "_";

#[derive(Clone, Debug)]
pub enum CodeGenFunction {
    Function { body: AirTree, params: Params },
    Link(Variant),
}

#[derive(Clone, Debug)]
pub enum HoistableFunction {
    Function {
        body: AirTree,
        deps: Vec<(FunctionAccessKey, Variant)>,
        params: Params,
    },
    CyclicFunction {
        functions: Vec<(Params, AirTree)>,
        deps: Vec<(FunctionAccessKey, Variant)>,
    },
    Link((FunctionAccessKey, Variant)),
    CyclicLink(FunctionAccessKey),
}

#[derive(Clone, Debug)]
pub struct AssignmentProperties {
    pub value_type: Rc<Type>,
    pub kind: TypedAssignmentKind,
    pub remove_unused: bool,
    pub full_check: bool,
    pub otherwise: Option<AirTree>,
}

#[derive(Clone, Debug)]

pub struct ClauseProperties {
    pub clause_var_name: String,
    pub complex_clause: bool,
    pub needs_constr_var: bool,
    pub original_subject_name: String,
    pub final_clause: bool,
    pub specific_clause: SpecificClause,
}
#[derive(Clone, Debug)]
pub enum SpecificClause {
    ConstrClause,
    ListClause {
        defined_tails_index: i64,
        defined_tails: Vec<String>,
        checked_index: i64,
    },
    TupleClause {
        defined_tuple_indices: IndexSet<(usize, String)>,
    },
    PairClause,
}

impl ClauseProperties {
    pub fn init(t: &Rc<Type>, constr_var: String, subject_name: String) -> Self {
        if t.is_list() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name.clone(),
                final_clause: false,
                needs_constr_var: false,
                specific_clause: SpecificClause::ListClause {
                    defined_tails_index: 0,
                    defined_tails: vec![subject_name],
                    checked_index: -1,
                },
            }
        } else if t.is_tuple() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause: false,
                specific_clause: SpecificClause::TupleClause {
                    defined_tuple_indices: IndexSet::new(),
                },
            }
        } else if t.is_pair() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause: false,
                specific_clause: SpecificClause::PairClause,
            }
        } else {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause: false,
                specific_clause: SpecificClause::ConstrClause,
            }
        }
    }

    pub fn init_inner(
        t: &Rc<Type>,
        constr_var: String,
        subject_name: String,
        final_clause: bool,
    ) -> Self {
        if t.is_list() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                final_clause,
                needs_constr_var: false,
                specific_clause: SpecificClause::ListClause {
                    defined_tails_index: 0,
                    defined_tails: vec![],
                    checked_index: -1,
                },
            }
        } else if t.is_tuple() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause,
                specific_clause: SpecificClause::TupleClause {
                    defined_tuple_indices: IndexSet::new(),
                },
            }
        } else if t.is_pair() {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause,
                specific_clause: SpecificClause::PairClause,
            }
        } else {
            ClauseProperties {
                clause_var_name: constr_var,
                complex_clause: false,
                original_subject_name: subject_name,
                needs_constr_var: false,
                final_clause,
                specific_clause: SpecificClause::ConstrClause,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct CodeGenSpecialFuncs {
    pub used_funcs: Vec<String>,
    pub key_to_func: IndexMap<String, (Term<Name>, Rc<Type>)>,
}

impl CodeGenSpecialFuncs {
    pub fn new() -> Self {
        let mut key_to_func = IndexMap::new();

        key_to_func.insert(
            CONSTR_FIELDS_EXPOSER.to_string(),
            (
                Term::snd_pair()
                    .apply(Term::unconstr_data().apply(Term::var("__constr_var")))
                    .lambda("__constr_var"),
                Type::function(vec![Type::data()], Type::list(Type::data())),
            ),
        );

        key_to_func.insert(
            CONSTR_INDEX_EXPOSER.to_string(),
            (
                Term::fst_pair()
                    .apply(Term::unconstr_data().apply(Term::var("__constr_var")))
                    .lambda("__constr_var"),
                Type::function(vec![Type::data()], Type::int()),
            ),
        );

        CodeGenSpecialFuncs {
            used_funcs: vec![],
            key_to_func,
        }
    }

    pub fn use_function_tree(&mut self, func_name: String) -> AirTree {
        if !self.used_funcs.contains(&func_name) {
            self.used_funcs.push(func_name.to_string());
        }

        let tipo = self.key_to_func.get(&func_name).unwrap().1.clone();

        AirTree::local_var(func_name, tipo)
    }

    pub fn use_function_msg(&mut self, func_name: String) -> AirMsg {
        if !self.used_funcs.contains(&func_name) {
            self.used_funcs.push(func_name.to_string());
        }

        AirMsg::LocalVar(func_name)
    }

    pub fn use_function_uplc(&mut self, func_name: String) -> String {
        if !self.used_funcs.contains(&func_name) {
            self.used_funcs.push(func_name.to_string());
        }

        func_name
    }

    pub fn get_function(&self, func_name: &String) -> Term<Name> {
        self.key_to_func[func_name].0.clone()
    }

    pub fn apply_used_functions(&self, mut term: Term<Name>) -> Term<Name> {
        for func_name in self.used_funcs.iter() {
            term = term.lambda(func_name).apply(self.get_function(func_name));
        }
        term
    }

    pub fn insert_new_function(
        &mut self,
        func_name: String,
        function: Term<Name>,
        function_type: Rc<Type>,
    ) {
        if !self.key_to_func.contains_key(&func_name) {
            self.key_to_func
                .insert(func_name, (function, function_type));
        }
    }
}

impl Default for CodeGenSpecialFuncs {
    fn default() -> Self {
        Self::new()
    }
}

pub fn get_generic_variant_name(t: &Rc<Type>) -> String {
    let uplc_type = t.get_uplc_type();

    match uplc_type {
        Some(UplcType::Bool) => "_bool".to_string(),
        Some(UplcType::Integer) => "_int".to_string(),
        Some(UplcType::String) => "_string".to_string(),
        Some(UplcType::ByteString) => "_bytearray".to_string(),
        Some(UplcType::Unit) => "_void".to_string(),
        Some(UplcType::List(_)) if t.is_map() => "_map".to_string(),
        Some(UplcType::List(_)) => "_list".to_string(),
        Some(UplcType::Pair(_, _)) => "_pair".to_string(),
        Some(UplcType::Bls12_381G1Element) => "_bls381_12_g1".to_string(),
        Some(UplcType::Bls12_381G2Element) => "_bls381_12_g2".to_string(),
        Some(UplcType::Bls12_381MlResult) => "_ml_result".to_string(),
        None if t.is_unbound() => "_unbound".to_string(),
        None if t.is_generic() => {
            unreachable!("FOUND A POLYMORPHIC TYPE. EXPECTED MONOMORPHIC TYPE")
        }
        None | Some(UplcType::Data) => "_data".to_string(),
    }
}

pub fn monomorphize(air_tree: &mut AirTree, mono_types: &IndexMap<u64, Rc<Type>>) {
    let mut held_types = air_tree.mut_held_types();

    while let Some(tipo) = held_types.pop() {
        *tipo = find_and_replace_generics(tipo, mono_types);
    }
}

pub fn erase_opaque_type_operations(
    air_tree: &mut AirTree,
    data_types: &IndexMap<&DataTypeKey, &TypedDataType>,
) {
    if let AirTree::Constr { tipo, args, .. } = air_tree {
        if check_replaceable_opaque_type(tipo, data_types) {
            let arg = args.pop().unwrap();

            match arg {
                AirTree::CastToData { value, .. } => *air_tree = *value,
                _ => *air_tree = arg,
            }
        }
    }

    let mut held_types = air_tree.mut_held_types();

    while let Some(tipo) = held_types.pop() {
        *tipo = convert_opaque_type(tipo, data_types, true);
    }
}

/// Determine whether this air_tree node introduces any shadowing over `potential_matches`
pub fn find_introduced_variables(air_tree: &AirTree) -> Vec<String> {
    match air_tree {
        AirTree::Let { name, .. } => vec![name.clone()],
        AirTree::SoftCastLet { name, .. } => vec![name.clone()],
        AirTree::TupleGuard { indices, .. } | AirTree::TupleClause { indices, .. } => {
            indices.iter().map(|(_, name)| name.clone()).collect()
        }
        AirTree::PairGuard {
            fst_name, snd_name, ..
        } => fst_name
            .iter()
            .cloned()
            .chain(snd_name.iter().cloned())
            .collect_vec(),
        AirTree::PairAccessor { fst, snd, .. } => {
            fst.iter().cloned().chain(snd.iter().cloned()).collect_vec()
        }
        AirTree::PairClause {
            fst_name, snd_name, ..
        } => fst_name
            .iter()
            .cloned()
            .chain(snd_name.iter().cloned())
            .collect_vec(),
        AirTree::Fn { params, .. } => params.to_vec(),
        AirTree::ListAccessor { names, .. } => names.clone(),
        AirTree::ListExpose {
            tail,
            tail_head_names,
            ..
        } => {
            let mut ret = vec![];
            if let Some((_, head)) = tail {
                ret.push(head.clone())
            }

            for name in tail_head_names.iter().map(|(_, head)| head) {
                ret.push(name.clone());
            }
            ret
        }
        AirTree::TupleAccessor { names, .. } => names.clone(),
        AirTree::FieldsExpose { indices, .. } => {
            indices.iter().map(|(_, name, _)| name.clone()).collect()
        }
        AirTree::When { subject_name, .. } => vec![subject_name.clone()],
        _ => vec![],
    }
}

/// Determine whether a function is recursive, and if so, get the arguments
pub fn is_recursive_function_call<'a>(
    air_tree: &'a AirTree,
    func_key: &FunctionAccessKey,
    variant: &String,
) -> (bool, Option<&'a Vec<AirTree>>) {
    if let AirTree::Call { func, args, .. } = air_tree {
        if let AirTree::Var {
            constructor:
                ValueConstructor {
                    variant: ValueConstructorVariant::ModuleFn { name, module, .. },
                    ..
                },
            variant_name,
            ..
        } = func.as_ref()
        {
            if name == &func_key.function_name
                && module == &func_key.module_name
                && variant == variant_name
            {
                return (true, Some(args));
            }
        }
    }
    (false, None)
}

pub fn identify_recursive_static_params(
    air_tree: &mut AirTree,

    func_params: &[String],
    func_key: &(FunctionAccessKey, String),
    function_calls_and_usage: &mut (usize, usize),

    potential_recursive_statics: &mut Vec<String>,
) {
    let variant = &func_key.1;
    let func_key = &func_key.0;

    // Now all variables can't be shadowed at this stage due to interning
    // Otherwise, if this is a recursive call site, disqualify anything that is different (or the same, but shadowed)
    if let (true, Some(args)) = is_recursive_function_call(air_tree, func_key, variant) {
        for (param, arg) in func_params.iter().zip(args) {
            if let Some((idx, _)) = potential_recursive_statics
                .iter()
                .find_position(|&p| p == param)
            {
                // Check if we pass something different in this recursive call site
                // by different, we mean
                // - a variable that is bound to a different name
                // - any other type of expression
                let param_is_different = match arg {
                    AirTree::Var { name, .. } => name != param,
                    _ => true,
                };
                // If so, then we disqualify this parameter from being a recursive static parameter
                if param_is_different {
                    potential_recursive_statics.remove(idx);
                }
            }
        }
        // This is a function call of a recursive function so track that
        *function_calls_and_usage = (function_calls_and_usage.0 + 1, function_calls_and_usage.1);
    } else if let AirTree::Var {
        constructor:
            ValueConstructor {
                variant: ValueConstructorVariant::ModuleFn { name, module, .. },
                ..
            },
        variant_name,
        ..
    } = air_tree
    {
        if name == &func_key.function_name
            && module == &func_key.module_name
            && variant == variant_name
        {
            // This is a usage of the recursive function either by call or being passed to a function or returned
            *function_calls_and_usage =
                (function_calls_and_usage.0, function_calls_and_usage.1 + 1);
        }
    }
}

pub fn modify_self_calls(
    body: &mut AirTree,
    func_key: &FunctionAccessKey,
    variant: &String,
    func_params: &[String],
) -> Vec<String> {
    let mut potential_recursive_statics = func_params.to_vec();

    // identify which parameters are recursively nonstatic (i.e. get modified before the self-call)
    // TODO: this would be a lot simpler if each `Var`, `Let`, function argument, etc. had a unique identifier
    // rather than just a name; this would let us track if the Var passed to itself was the same value as the method argument
    let mut calls_and_var_usage = (0, 0);
    body.traverse_tree_with(&mut |air_tree: &mut AirTree, _tree_path| {
        identify_recursive_static_params(
            air_tree,
            func_params,
            &(func_key.clone(), variant.clone()),
            &mut calls_and_var_usage,
            &mut potential_recursive_statics,
        );
    });

    // Find the index of any recursively static parameters,
    // so we can remove them from the call-site of each recursive call
    let recursive_static_indexes: Vec<_> = func_params
        .iter()
        .enumerate()
        .filter(|&(_, p)| potential_recursive_statics.contains(p))
        .map(|(idx, _)| idx)
        .collect();

    // Modify any self calls to remove recursive static parameters and append `self` as a parameter for the recursion
    body.traverse_tree_with(&mut |air_tree: &mut AirTree, _| {
        if let AirTree::Call {
            func: func_recursive,
            args,
            ..
        } = air_tree
        {
            if let AirTree::Call { func, .. } = func_recursive.as_ref() {
                if let AirTree::Var {
                    constructor:
                        ValueConstructor {
                            variant: ValueConstructorVariant::ModuleFn { name, module, .. },
                            ..
                        },
                    variant_name,
                    ..
                } = func.as_ref()
                {
                    // The name must match and the recursive function must not be
                    // passed around for this optimization to work.
                    if name == &func_key.function_name
                        && module == &func_key.module_name
                        && variant == variant_name
                        && calls_and_var_usage.0 == calls_and_var_usage.1
                    {
                        // Remove any static-recursive-parameters, because they'll be bound statically
                        // above the recursive part of the function
                        // note: assumes that static_recursive_params is sorted
                        for arg in recursive_static_indexes.iter().rev() {
                            args.remove(*arg);
                        }
                    }
                }
            }
        } else if let AirTree::Var {
            constructor:
                ValueConstructor {
                    variant: ValueConstructorVariant::ModuleFn { name, module, .. },
                    ..
                },
            variant_name,
            ..
        } = &air_tree
        {
            if name.clone() == func_key.function_name
                && module.clone() == func_key.module_name
                && variant.clone() == variant_name.clone()
            {
                let self_call = AirTree::call(
                    air_tree.clone(),
                    air_tree.return_type(),
                    vec![air_tree.clone()],
                );

                *air_tree = self_call;
            }
        }
    });

    // In the case of equal calls to usage we can reduce the static params
    if calls_and_var_usage.0 == calls_and_var_usage.1 {
        let recursive_nonstatics = func_params
            .iter()
            .filter(|p| !potential_recursive_statics.contains(p))
            .cloned()
            .collect();
        recursive_nonstatics
    } else {
        func_params.to_vec()
    }
}

pub fn modify_cyclic_calls(
    body: &mut AirTree,
    func_key: &FunctionAccessKey,
    cyclic_links: &IndexMap<
        (FunctionAccessKey, Variant),
        (CycleFunctionNames, usize, FunctionAccessKey),
    >,
) {
    body.traverse_tree_with(&mut |air_tree: &mut AirTree, _| {
        if let AirTree::Var {
            constructor:
                ValueConstructor {
                    variant: ValueConstructorVariant::ModuleFn { name, module, .. },
                    tipo,
                    ..
                },
            variant_name,
            ..
        } = air_tree
        {
            let tipo = tipo.clone();
            let var_key = FunctionAccessKey {
                module_name: module.clone(),
                function_name: name.clone(),
            };

            if let Some((names, index, cyclic_name)) =
                cyclic_links.get(&(var_key.clone(), variant_name.to_string()))
            {
                if *cyclic_name == *func_key {
                    let cyclic_var_name = if cyclic_name.module_name.is_empty() {
                        cyclic_name.function_name.to_string()
                    } else {
                        format!("{}_{}", cyclic_name.module_name, cyclic_name.function_name)
                    };

                    let index_name = names[*index].clone();

                    let var = AirTree::var(
                        ValueConstructor::public(
                            tipo.clone(),
                            ValueConstructorVariant::ModuleFn {
                                name: cyclic_var_name.clone(),
                                field_map: None,
                                module: "".to_string(),
                                arity: 2,
                                location: Span::empty(),
                                builtin: None,
                            },
                        ),
                        cyclic_var_name,
                        "".to_string(),
                    );

                    *air_tree = AirTree::call(
                        var.clone(),
                        tipo.clone(),
                        vec![
                            var,
                            AirTree::anon_func(
                                names.clone(),
                                AirTree::local_var(index_name, tipo),
                                false,
                            ),
                        ],
                    );
                }
            }
        }
    });
}

pub fn pattern_has_conditions(
    pattern: &TypedPattern,
    data_types: &IndexMap<&DataTypeKey, &TypedDataType>,
) -> bool {
    match pattern {
        Pattern::List { .. } | Pattern::Int { .. } | Pattern::ByteArray { .. } => true,
        Pattern::Tuple { elems, .. } => elems
            .iter()
            .any(|elem| pattern_has_conditions(elem, data_types)),
        Pattern::Pair { fst, snd, .. } => {
            pattern_has_conditions(fst, data_types) || pattern_has_conditions(snd, data_types)
        }
        Pattern::Constructor {
            arguments, tipo, ..
        } => {
            let data_type = lookup_data_type_by_tipo(data_types, tipo)
                .unwrap_or_else(|| panic!("Data type not found: {:#?}", tipo));

            data_type.constructors.len() > 1
                || arguments
                    .iter()
                    .any(|arg| pattern_has_conditions(&arg.value, data_types))
        }
        Pattern::Assign { pattern, .. } => pattern_has_conditions(pattern, data_types),
        Pattern::Var { .. } | Pattern::Discard { .. } => false,
    }
}

// TODO: write some tests
pub fn rearrange_list_clauses(
    clauses: Vec<TypedClause>,
    data_types: &IndexMap<&DataTypeKey, &TypedDataType>,
) -> Vec<TypedClause> {
    let mut sorted_clauses = clauses;

    // if we have a list sort clauses so we can plug holes for cases not covered by clauses
    // Now we sort by elements + tail if possible and otherwise leave an index in place if var or discard
    // This is a stable sort. i.e. matching elements amounts will remain in user given order.
    sorted_clauses = sorted_clauses
        .into_iter()
        .enumerate()
        .sorted_by(|(index1, clause1), (index2, clause2)| {
            let mut clause_pattern1 = &clause1.pattern;
            let mut clause_pattern2 = &clause2.pattern;

            if let Pattern::Assign { pattern, .. } = clause_pattern1 {
                clause_pattern1 = pattern;
            }

            if let Pattern::Assign { pattern, .. } = clause_pattern2 {
                clause_pattern2 = pattern;
            }

            let clause1_len = match clause_pattern1 {
                Pattern::List { elements, tail, .. } => {
                    Some(elements.len() + usize::from(tail.is_some()))
                }
                _ => Some(100000),
            };

            let clause2_len = match clause_pattern2 {
                Pattern::List { elements, tail, .. } => {
                    Some(elements.len() + usize::from(tail.is_some()))
                }
                _ => Some(100001),
            };

            if let Some(clause1_len) = clause1_len {
                if let Some(clause2_len) = clause2_len {
                    return clause1_len.cmp(&clause2_len);
                }
            }

            index1.cmp(index2)
        })
        .map(|(_, item)| item)
        .collect_vec();

    let mut final_clauses = sorted_clauses.clone();
    let mut holes_to_fill = vec![];
    let mut last_clause_index = 0;
    let mut last_clause_set = false;
    let mut wild_card_clause_elems = 0;

    // If we have a catch all, use that. Otherwise use todo which will result in error
    // TODO: fill in todo label with description
    let plug_in_then = &|index: usize, last_clause: &TypedClause| match &last_clause.pattern {
        Pattern::Var { .. } | Pattern::Discard { .. } => last_clause.clone().then,
        _ => {
            let tipo = last_clause.then.tipo();

            TypedExpr::Trace {
                location: Span::empty(),
                tipo: tipo.clone(),
                text: Box::new(TypedExpr::String {
                    location: Span::empty(),
                    tipo: Type::string(),
                    value: format!("Clause hole found for {index} elements."),
                }),
                then: Box::new(TypedExpr::ErrorTerm {
                    location: Span::empty(),
                    tipo,
                }),
            }
        }
    };

    let last_clause = &sorted_clauses[sorted_clauses.len() - 1];
    let assign_plug_in_name = if let Pattern::Var { name, .. } = &last_clause.pattern {
        Some(name)
    } else {
        None
    };

    for (index, clause) in sorted_clauses.iter().enumerate() {
        if last_clause_set {
            continue;
        }

        let mut clause_pattern = &clause.pattern;

        if let Pattern::Assign { pattern, .. } = clause_pattern {
            clause_pattern = pattern;
        }

        assert!(matches!(
            clause_pattern,
            Pattern::List { .. } | Pattern::Var { .. } | Pattern::Discard { .. }
        ));

        if let Pattern::List { elements, tail, .. } = clause_pattern {
            // found a hole and now we plug it
            while wild_card_clause_elems < elements.len() {
                let mut discard_elems = vec![];

                for _ in 0..wild_card_clause_elems {
                    discard_elems.push(Pattern::Discard {
                        name: "__fill".to_string(),
                        location: Span::empty(),
                    });
                }

                // If we have a named catch all then in scope the name and create list of discards, otherwise list of discards
                let clause_to_fill = if let Some(name) = assign_plug_in_name {
                    TypedClause {
                        location: Span::empty(),
                        pattern: Pattern::Assign {
                            name: name.clone(),
                            location: Span::empty(),
                            pattern: Pattern::List {
                                location: Span::empty(),
                                elements: discard_elems,
                                tail: None,
                            }
                            .into(),
                        },
                        then: plug_in_then(wild_card_clause_elems, last_clause),
                    }
                } else {
                    TypedClause {
                        location: Span::empty(),
                        pattern: Pattern::List {
                            location: Span::empty(),
                            elements: discard_elems,
                            tail: None,
                        },
                        then: plug_in_then(wild_card_clause_elems, last_clause),
                    }
                };

                holes_to_fill.push((index, clause_to_fill));
                wild_card_clause_elems += 1;
            }

            let mut is_wild_card_elems_clause = true;

            for element in elements.iter() {
                is_wild_card_elems_clause =
                    is_wild_card_elems_clause && !pattern_has_conditions(element, data_types);
            }

            if is_wild_card_elems_clause {
                if wild_card_clause_elems < elements.len() + usize::from(tail.is_none()) {
                    wild_card_clause_elems += 1;
                }

                if tail.is_some() && !elements.is_empty() {
                    last_clause_index = index;
                    last_clause_set = true;
                }
            }
        } else if let Pattern::Var { .. } | Pattern::Discard { .. } = &clause.pattern {
            last_clause_set = true;
            last_clause_index = index;
        } else {
            unreachable!("Found a clause that is not a list or var or discard");
        }

        // If the last condition doesn't have a catch all or tail then add a catch all with a todo
        if index == sorted_clauses.len() - 1 {
            if let Pattern::List { tail: None, .. } = &clause.pattern {
                final_clauses.push(TypedClause {
                    location: Span::empty(),
                    pattern: Pattern::Discard {
                        name: "_".to_string(),
                        location: Span::empty(),
                    },
                    then: plug_in_then(index + 1, last_clause),
                });
            }
        }
    }

    // Encountered a tail so stop there with that as last clause
    if last_clause_set {
        for _ in 0..(sorted_clauses.len() - 1 - last_clause_index) {
            final_clauses.pop();
        }
    }

    // insert hole fillers into clauses
    for (index, clause) in holes_to_fill.into_iter().rev() {
        final_clauses.insert(index, clause);
    }
    assert!(final_clauses.len() > 1);

    final_clauses
}

pub fn find_list_clause_or_default_first(clauses: &[TypedClause]) -> &TypedClause {
    assert!(!clauses.is_empty());

    clauses
        .iter()
        .find(|clause| match &clause.pattern {
            Pattern::List { .. } => true,
            Pattern::Assign { pattern, .. } if matches!(&**pattern, Pattern::List { .. }) => true,
            _ => false,
        })
        .unwrap_or(&clauses[0])
}

pub fn known_data_to_type(term: Term<Name>, field_type: &Type) -> Term<Name> {
    let uplc_type = field_type.get_uplc_type();

    match uplc_type {
        Some(UplcType::Integer) => Term::un_i_data().apply(term),
        Some(UplcType::ByteString) => Term::un_b_data().apply(term),
        Some(UplcType::Bool) => Term::equals_integer()
            .apply(Term::integer(1.into()))
            .apply(Term::fst_pair().apply(Term::unconstr_data().apply(term))),
        Some(UplcType::String) => Term::decode_utf8().apply(Term::un_b_data().apply(term)),
        Some(UplcType::Unit) => Term::unit().lambda("_").apply(term),
        Some(UplcType::List(_)) if field_type.is_map() => Term::unmap_data().apply(term),
        Some(UplcType::List(_)) => Term::unlist_data().apply(term),
        Some(UplcType::Pair(_, _)) => Term::mk_pair_data()
            .apply(Term::head_list().apply(Term::var("__list_data")))
            .apply(Term::head_list().apply(Term::tail_list().apply(Term::var("__list_data"))))
            .lambda("__list_data")
            .apply(Term::unlist_data().apply(term)),

        Some(UplcType::Bls12_381G1Element) => {
            Term::bls12_381_g1_uncompress().apply(Term::un_b_data().apply(term))
        }
        Some(UplcType::Bls12_381G2Element) => {
            Term::bls12_381_g2_uncompress().apply(Term::un_b_data().apply(term))
        }
        Some(UplcType::Bls12_381MlResult) => panic!("ML Result not supported"),
        Some(UplcType::Data) | None => term,
    }
}

pub fn unknown_data_to_type(term: Term<Name>, field_type: &Type) -> Term<Name> {
    let uplc_type = field_type.get_uplc_type();

    match uplc_type {
        Some(UplcType::Integer) => Term::un_i_data().apply(term),
        Some(UplcType::ByteString) => Term::un_b_data().apply(term),
        Some(UplcType::String) => Term::decode_utf8().apply(Term::un_b_data().apply(term)),
        Some(UplcType::List(_)) if field_type.is_map() => Term::unmap_data().apply(term),
        Some(UplcType::List(_)) => Term::unlist_data().apply(term),

        Some(UplcType::Bls12_381G1Element) => {
            Term::bls12_381_g1_uncompress().apply(Term::un_b_data().apply(term))
        }
        Some(UplcType::Bls12_381G2Element) => {
            Term::bls12_381_g2_uncompress().apply(Term::un_b_data().apply(term))
        }
        Some(UplcType::Bls12_381MlResult) => panic!("ML Result not supported"),

        Some(UplcType::Pair(_, _)) => Term::tail_list()
            .apply(Term::tail_list().apply(Term::var("__list_data")))
            .delayed_choose_list(
                Term::mk_pair_data()
                    .apply(Term::head_list().apply(Term::var("__list_data")))
                    .apply(
                        Term::head_list().apply(Term::tail_list().apply(Term::var("__list_data"))),
                    ),
                Term::Error,
            )
            .lambda("__list_data")
            .apply(Term::unlist_data().apply(term)),
        Some(UplcType::Bool) => Term::unwrap_bool_or(term, |result| result, &Term::Error.delay()),
        Some(UplcType::Unit) => term.as_var("val", |val| {
            Term::Var(val).unwrap_void_or(|result| result, &Term::Error.delay())
        }),

        Some(UplcType::Data) | None => term,
    }
}

/// Due to the nature of the types BLS12_381_G1Element and BLS12_381_G2Element and String coming from bytearray
/// We don't have error handling if the bytearray is not properly aligned to the type. Oh well lol
/// For BLS12_381_G1Element and BLS12_381_G2Element, hash to group exists so just adopt that.
pub fn softcast_data_to_type_otherwise(
    value: Term<Name>,
    name: &String,
    field_type: &Type,
    then: Term<Name>,
    otherwise_delayed: Term<Name>,
) -> Term<Name> {
    assert!(matches!(otherwise_delayed, Term::Var(_)));

    let uplc_type = field_type.get_uplc_type();

    let callback = |v| then.lambda(name).apply(v);

    value.as_var("__val", |val| match uplc_type {
        None => Term::choose_data_constr(val, callback, &otherwise_delayed),

        Some(UplcType::Data) => callback(Term::Var(val)),

        Some(UplcType::Bls12_381MlResult) => {
            unreachable!("attempted to cast Data into Bls12_381MlResult ?!")
        }

        Some(UplcType::Integer) => Term::choose_data_integer(val, callback, &otherwise_delayed),

        Some(UplcType::ByteString) => {
            Term::choose_data_bytearray(val, callback, &otherwise_delayed)
        }

        Some(UplcType::String) => Term::choose_data_bytearray(
            val,
            |bytes| callback(Term::decode_utf8().apply(bytes)),
            &otherwise_delayed,
        ),

        Some(UplcType::List(_)) if field_type.is_map() => {
            Term::choose_data_map(val, callback, &otherwise_delayed)
        }

        Some(UplcType::List(_)) => Term::choose_data_list(val, callback, &otherwise_delayed),

        Some(UplcType::Bls12_381G1Element) => Term::choose_data_bytearray(
            val,
            |bytes| callback(Term::bls12_381_g1_uncompress().apply(bytes)),
            &otherwise_delayed,
        ),

        Some(UplcType::Bls12_381G2Element) => Term::choose_data_bytearray(
            val,
            |bytes| callback(Term::bls12_381_g2_uncompress().apply(bytes)),
            &otherwise_delayed,
        ),

        Some(UplcType::Pair(_, _)) => Term::choose_data_list(
            val,
            |list| list.unwrap_pair_or(callback, &otherwise_delayed),
            &otherwise_delayed,
        ),

        Some(UplcType::Bool) => Term::choose_data_constr(
            val,
            |constr| constr.unwrap_bool_or(callback, &otherwise_delayed),
            &otherwise_delayed,
        ),

        Some(UplcType::Unit) => Term::choose_data_constr(
            val,
            |constr| constr.unwrap_void_or(callback, &otherwise_delayed),
            &otherwise_delayed,
        ),
    })
}

pub fn convert_constants_to_data(constants: Vec<Rc<UplcConstant>>) -> Vec<UplcConstant> {
    let mut new_constants = vec![];
    for constant in constants {
        let constant = match constant.as_ref() {
            UplcConstant::Integer(i) => UplcConstant::Data(PlutusData::BigInt(to_pallas_bigint(i))),
            UplcConstant::ByteString(b) => {
                UplcConstant::Data(PlutusData::BoundedBytes(b.clone().into()))
            }
            UplcConstant::String(s) => {
                UplcConstant::Data(PlutusData::BoundedBytes(s.as_bytes().to_vec().into()))
            }

            UplcConstant::Bool(b) => UplcConstant::Data(PlutusData::Constr(Constr {
                tag: convert_constr_to_tag((*b).into()).unwrap_or(ANY_TAG),
                any_constructor: convert_constr_to_tag((*b).into())
                    .map_or(Some((*b).into()), |_| None),
                fields: vec![],
            })),
            UplcConstant::ProtoList(list_type, constants) => {
                if matches!(list_type, UplcType::Pair(_, _)) {
                    let inner_constants = constants
                        .iter()
                        .cloned()
                        .map(|pair| match pair {
                            UplcConstant::ProtoPair(_, _, left, right) => {
                                let inner_constants = vec![left, right];
                                let inner_constants = convert_constants_to_data(inner_constants)
                                    .into_iter()
                                    .map(|constant| match constant {
                                        UplcConstant::Data(d) => d,
                                        _ => todo!(),
                                    })
                                    .collect_vec();
                                (inner_constants[0].clone(), inner_constants[1].clone())
                            }
                            _ => unreachable!(),
                        })
                        .collect_vec();

                    UplcConstant::Data(PlutusData::Map(KeyValuePairs::Def(inner_constants)))
                } else {
                    let inner_constants =
                        convert_constants_to_data(constants.iter().cloned().map(Rc::new).collect())
                            .into_iter()
                            .map(|constant| match constant {
                                UplcConstant::Data(d) => d,
                                _ => todo!(),
                            })
                            .collect_vec();

                    UplcConstant::Data(PlutusData::Array(inner_constants))
                }
            }
            UplcConstant::ProtoPair(_, _, left, right) => {
                let inner_constants = vec![left.clone(), right.clone()];
                let inner_constants = convert_constants_to_data(inner_constants)
                    .into_iter()
                    .map(|constant| match constant {
                        UplcConstant::Data(d) => d,
                        _ => todo!(),
                    })
                    .collect_vec();

                UplcConstant::Data(PlutusData::Array(vec![
                    inner_constants[0].clone(),
                    inner_constants[1].clone(),
                ]))
            }
            d @ UplcConstant::Data(_) => d.clone(),
            UplcConstant::Unit => UplcConstant::Data(PlutusData::Constr(Constr {
                tag: convert_constr_to_tag(0).unwrap(),
                any_constructor: None,
                fields: vec![],
            })),
            UplcConstant::Bls12_381G1Element(b) => UplcConstant::Data(PlutusData::BoundedBytes(
                b.deref().clone().compress().into(),
            )),
            UplcConstant::Bls12_381G2Element(b) => UplcConstant::Data(PlutusData::BoundedBytes(
                b.deref().clone().compress().into(),
            )),
            UplcConstant::Bls12_381MlResult(_) => panic!("Bls12_381MlResult not supported"),
        };
        new_constants.push(constant);
    }
    new_constants
}

pub fn convert_type_to_data(term: Term<Name>, field_type: &Rc<Type>) -> Term<Name> {
    let uplc_type = field_type.get_uplc_type();

    match uplc_type {
        Some(UplcType::Integer) => Term::i_data().apply(term),
        Some(UplcType::String) => Term::b_data().apply(Term::encode_utf8().apply(term)),
        Some(UplcType::ByteString) => Term::b_data().apply(term),
        Some(UplcType::List(_)) if field_type.is_map() => Term::map_data().apply(term),
        Some(UplcType::List(_)) => Term::list_data().apply(term),

        Some(UplcType::Bls12_381G1Element) => {
            Term::b_data().apply(Term::bls12_381_g1_compress().apply(term))
        }
        Some(UplcType::Bls12_381G2Element) => {
            Term::b_data().apply(Term::bls12_381_g2_compress().apply(term))
        }
        Some(UplcType::Bls12_381MlResult) => panic!("ML Result not supported"),
        Some(UplcType::Pair(_, _)) => Term::list_data()
            .apply(
                Term::mk_cons()
                    .apply(Term::fst_pair().apply(Term::var("__pair")))
                    .apply(
                        Term::mk_cons()
                            .apply(Term::snd_pair().apply(Term::var("__pair")))
                            .apply(Term::empty_list()),
                    ),
            )
            .lambda("__pair")
            .apply(term),
        Some(UplcType::Unit) => Term::Constant(
            UplcConstant::Data(PlutusData::Constr(Constr {
                tag: convert_constr_to_tag(0).unwrap(),
                any_constructor: None,
                fields: vec![],
            }))
            .into(),
        )
        .lambda("_")
        .apply(term),
        Some(UplcType::Bool) => term.if_then_else(
            Term::Constant(
                UplcConstant::Data(PlutusData::Constr(Constr {
                    tag: convert_constr_to_tag(1).unwrap(),
                    any_constructor: None,
                    fields: vec![],
                }))
                .into(),
            ),
            Term::Constant(
                UplcConstant::Data(PlutusData::Constr(Constr {
                    tag: convert_constr_to_tag(0).unwrap(),
                    any_constructor: None,
                    fields: vec![],
                }))
                .into(),
            ),
        ),

        Some(UplcType::Data) | None => term,
    }
}

pub fn list_access_to_uplc(
    names_types_ids: &[(String, Rc<Type>, u64)],
    tail_present: bool,
    term: Term<Name>,
    is_list_accessor: bool,
    expect_level: ExpectLevel,
    otherwise_delayed: Term<Name>,
) -> Term<Name> {
    let names_len = names_types_ids.len();

    // assert!(!(matches!(expect_level, ExpectLevel::None) && is_list_accessor && !tail_present));

    let mut no_tailing_discards = names_types_ids
        .iter()
        .rev()
        .with_position()
        .skip_while(|pos| match pos {
            // Items are reversed order
            Position::Last((name, _, _)) | Position::Middle((name, _, _)) => {
                name == "_" && matches!(expect_level, ExpectLevel::None)
            }
            Position::First((name, _, _)) | Position::Only((name, _, _)) => {
                name == "_" && (tail_present || matches!(expect_level, ExpectLevel::None))
            }
        })
        .map(|position| match position {
            Position::First(a) | Position::Middle(a) | Position::Last(a) | Position::Only(a) => a,
        })
        .collect_vec();

    // If is just discards and check_last_item then we check for empty list
    if no_tailing_discards.is_empty() {
        if tail_present || matches!(expect_level, ExpectLevel::None) {
            return term.lambda("_");
        }

        return Term::var("empty_list")
            .delay_empty_choose_list(term, otherwise_delayed)
            .lambda("empty_list");
    }

    // reverse back to original order
    no_tailing_discards.reverse();

    // If we cut off at least one element then that was tail and possibly some heads
    let tail_wasnt_cutoff = tail_present && no_tailing_discards.len() == names_len;

    let tail_name = |id| format!("tail_id_{}", id);

    let head_item = |name, tipo: &Rc<Type>, tail_name: &str, then: Term<Name>| {
        if name == "_" {
            then
        } else if tipo.is_pair() && is_list_accessor {
            then.lambda(name)
                .apply(Term::head_list().apply(Term::var(tail_name.to_string())))
        } else if matches!(expect_level, ExpectLevel::Full) {
            // Expect level is full so we have an unknown piece of data to cast
            if otherwise_delayed == Term::Error.delay() {
                then.lambda(name).apply(unknown_data_to_type(
                    Term::head_list().apply(Term::var(tail_name.to_string())),
                    &tipo.to_owned(),
                ))
            } else {
                softcast_data_to_type_otherwise(
                    Term::head_list().apply(Term::var(tail_name.to_string())),
                    name,
                    &tipo.to_owned(),
                    then,
                    otherwise_delayed.clone(),
                )
            }
        } else {
            then.lambda(name).apply(known_data_to_type(
                Term::head_list().apply(Term::var(tail_name.to_string())),
                &tipo.to_owned(),
            ))
        }
    };

    // Remember we reverse here so the First or Only is the last item
    no_tailing_discards
        .into_iter()
        .rev()
        .with_position()
        .fold(term, |acc, position| {
            match position {
                Position::First((name, _, _)) | Position::Only((name, _, _))
                    if tail_wasnt_cutoff =>
                {
                    // case for tail as the last item
                    acc.lambda(name)
                }

                Position::First((name, tipo, id)) | Position::Only((name, tipo, id)) => {
                    // case for no tail, but last item
                    let tail_name = tail_name(id);

                    // let head_item = head_item(name, tipo, &tail_name);

                    match expect_level {
                        ExpectLevel::None => {
                            head_item(name, tipo, &tail_name, acc).lambda(tail_name)
                        }

                        ExpectLevel::Full | ExpectLevel::Items => {
                            if otherwise_delayed == Term::Error.delay() && tail_present {
                                // No need to check last item if tail was present
                                head_item(name, tipo, &tail_name, acc).lambda(tail_name)
                            } else if tail_present {
                                // Custom error instead of trying to do head_item on a possibly empty list.
                                Term::var(tail_name.to_string())
                                    .delay_filled_choose_list(
                                        otherwise_delayed.clone(),
                                        head_item(name, tipo, &tail_name, acc),
                                    )
                                    .lambda(tail_name)
                            } else if otherwise_delayed == Term::Error.delay() {
                                // Check head is last item in this list
                                head_item(
                                    name,
                                    tipo,
                                    &tail_name,
                                    Term::tail_list()
                                        .apply(Term::var(tail_name.to_string()))
                                        .delayed_choose_list(acc, Term::Error),
                                )
                                .lambda(tail_name)
                            } else {
                                // Custom error if list is not empty after this head
                                Term::var(tail_name.to_string())
                                    .delay_filled_choose_list(
                                        otherwise_delayed.clone(),
                                        head_item(
                                            name,
                                            tipo,
                                            &tail_name,
                                            Term::tail_list()
                                                .apply(Term::var(tail_name.to_string()))
                                                .delay_empty_choose_list(
                                                    acc,
                                                    otherwise_delayed.clone(),
                                                ),
                                        ),
                                    )
                                    .lambda(tail_name)
                            }
                        }
                    }
                }

                Position::Middle((name, tipo, id)) | Position::Last((name, tipo, id)) => {
                    // case for every item except the last item
                    let tail_name = tail_name(id);

                    // let head_item = head_item(name, tipo, &tail_name);

                    if matches!(expect_level, ExpectLevel::None)
                        || otherwise_delayed == Term::Error.delay()
                    {
                        head_item(
                            name,
                            tipo,
                            &tail_name,
                            acc.apply(Term::tail_list().apply(Term::var(tail_name.to_string()))),
                        )
                        .lambda(tail_name)
                    } else {
                        // case for a custom error if the list is empty at this point

                        Term::var(tail_name.to_string())
                            .delay_filled_choose_list(
                                otherwise_delayed.clone(),
                                head_item(
                                    name,
                                    tipo,
                                    &tail_name,
                                    acc.apply(
                                        Term::tail_list().apply(Term::var(tail_name.to_string())),
                                    ),
                                ),
                            )
                            .lambda(tail_name)
                    }
                }
            }
        })
}

pub fn apply_builtin_forces(mut term: Term<Name>, force_count: u32) -> Term<Name> {
    for _ in 0..force_count {
        term = term.force();
    }
    term
}

pub fn undata_builtin(
    func: &DefaultFunction,
    count: usize,
    tipo: &Rc<Type>,
    args: Vec<Term<Name>>,
) -> Term<Name> {
    let mut term: Term<Name> = (*func).into();

    term = apply_builtin_forces(term, func.force_count());

    for arg in args {
        term = term.apply(arg);
    }

    let temp_var = "__item_x";

    if count == 0 {
        term = term.apply(Term::var(temp_var));
    }

    term = known_data_to_type(term, tipo);

    if count == 0 {
        term = term.lambda(temp_var);
    }
    term
}

pub fn to_data_builtin(
    func: &DefaultFunction,
    count: usize,
    tipo: &Rc<Type>,
    mut args: Vec<Term<Name>>,
) -> Term<Name> {
    let mut term: Term<Name> = (*func).into();

    term = apply_builtin_forces(term, func.force_count());

    if count == 0 {
        assert!(args.is_empty());

        for arg_index in 0..func.arity() {
            let temp_var = format!("__item_index_{}", arg_index);

            args.push(Term::var(temp_var))
        }
    }

    for (index, arg) in args.into_iter().enumerate() {
        if index == 0 || matches!(func, DefaultFunction::MkPairData) {
            term = term.apply(convert_type_to_data(arg, tipo));
        } else {
            term = term.apply(arg);
        }
    }

    if count == 0 {
        for arg_index in (0..func.arity()).rev() {
            let temp_var = format!("__item_index_{}", arg_index);
            term = term.lambda(temp_var);
        }
    }

    term
}

pub fn special_case_builtin(
    func: &DefaultFunction,
    tipo: Rc<Type>,
    count: usize,
    mut args: Vec<Term<Name>>,
) -> Term<Name> {
    match func {
        DefaultFunction::ChooseUnit if count > 0 => {
            let term = args.pop().unwrap();
            let unit = args.pop().unwrap();

            term.lambda("_").apply(unit)
        }

        DefaultFunction::MkCons => {
            let arg_type = tipo
                .arg_types()
                .and_then(|generics| generics.first().cloned())
                .expect("mk_cons should have (exactly) one type parameter");

            if let [head, tail] = &args[..] {
                Term::mk_cons()
                    .apply(if arg_type.is_pair() {
                        head.clone()
                    } else {
                        convert_type_to_data(head.clone(), &arg_type)
                    })
                    .apply(tail.clone())
            } else {
                unreachable!("mk_cons has two arguments.");
            }
        }

        DefaultFunction::ChooseUnit
        | DefaultFunction::IfThenElse
        | DefaultFunction::ChooseList
        | DefaultFunction::ChooseData
        | DefaultFunction::Trace => {
            let mut term: Term<Name> = (*func).into();

            term = apply_builtin_forces(term, func.force_count());

            if count == 0 {
                assert!(args.is_empty());

                for arg_index in 0..func.arity() {
                    let temp_var = format!("__item_index_{}", arg_index);

                    args.push(Term::var(temp_var))
                }
            }

            for (index, arg) in args.into_iter().enumerate() {
                if index == 0 {
                    term = term.apply(arg);
                } else {
                    term = term.apply(arg.delay());
                }
            }

            term = term.force();

            if count == 0 {
                for arg_index in (0..func.arity()).rev() {
                    let temp_var = format!("__item_index_{}", arg_index);
                    term = term.lambda(temp_var);
                }
            }

            term
        }
        DefaultFunction::UnConstrData => {
            let mut term: Term<Name> = (*func).into();

            let temp_tuple = "__unconstr_tuple";

            for arg in args {
                term = term.apply(arg);
            }

            let temp_var = "__item_x";

            if count == 0 {
                term = term.apply(Term::var(temp_var));
            }

            term = Term::mk_pair_data()
                .apply(Term::i_data().apply(Term::fst_pair().apply(Term::var(temp_tuple))))
                .apply(Term::list_data().apply(Term::snd_pair().apply(Term::var(temp_tuple))))
                .lambda(temp_tuple)
                .apply(term);

            if count == 0 {
                term = term.lambda(temp_var);
            }

            term
        }
        _ => unreachable!(),
    }
}

pub fn wrap_as_multi_validator(
    spend: Term<Name>,
    mint: Term<Name>,
    trace: TraceLevel,
    spend_name: String,
    mint_name: String,
) -> Term<Name> {
    match trace {
        TraceLevel::Silent | TraceLevel::Compact => Term::equals_integer()
            .apply(Term::integer(0.into()))
            .apply(Term::var(CONSTR_INDEX_EXPOSER).apply(Term::var("__second_arg")))
            .delayed_if_then_else(
                mint.apply(Term::var("__first_arg"))
                    .apply(Term::var("__second_arg")),
                spend.apply(Term::var("__first_arg")).apply(
                    Term::head_list()
                        .apply(Term::var(CONSTR_FIELDS_EXPOSER).apply(Term::var("__second_arg"))),
                ),
            )
            .lambda("__second_arg")
            .lambda("__first_arg"),
        TraceLevel::Verbose => {
            let trace_string = format!(
                    "Incorrect redeemer type for validator {}.
                    Double check you have wrapped the redeemer type as specified in your plutus.json",
                    spend_name
                );

            let error_term = Term::Error.delayed_trace(Term::var("__incorrect_second_arg_type"));

            let then_term = mint
                .apply(Term::var("__first_arg"))
                .apply(Term::var("__second_arg"));

            let else_term = spend.apply(Term::var("__first_arg")).apply(
                Term::head_list()
                    .apply(Term::var(CONSTR_FIELDS_EXPOSER).apply(Term::var("__second_arg"))),
            );

            Term::var("__second_arg")
                .delayed_choose_data(
                    Term::equals_integer()
                        .apply(Term::integer(0.into()))
                        .apply(Term::var(CONSTR_INDEX_EXPOSER).apply(Term::var("__second_arg")))
                        .delayed_if_then_else(
                            then_term.delayed_trace(Term::string(format!(
                                "Running 2 arg validator {}",
                                mint_name
                            ))),
                            else_term.delayed_trace(Term::string(format!(
                                "Running 3 arg validator {}",
                                spend_name
                            ))),
                        ),
                    error_term.clone(),
                    error_term.clone(),
                    error_term.clone(),
                    error_term,
                )
                .lambda("__incorrect_second_arg_type")
                .apply(Term::string(trace_string))
                .lambda("__second_arg")
                .lambda("__first_arg")
        }
    }
}

/// If the pattern is a list the return the number of elements and if it has a tail
/// Otherwise return None
pub fn get_list_elements_len_and_tail(
    pattern: &Pattern<PatternConstructor, Rc<Type>>,
) -> Option<(usize, bool)> {
    if let Pattern::List { elements, tail, .. } = &pattern {
        Some((elements.len(), tail.is_some()))
    } else if let Pattern::Assign { pattern, .. } = &pattern {
        if let Pattern::List { elements, tail, .. } = pattern.as_ref() {
            Some((elements.len(), tail.is_some()))
        } else {
            None
        }
    } else {
        None
    }
}

pub fn cast_validator_args(
    term: Term<Name>,
    arguments: &[TypedArg],
    interner: &AirInterner,
) -> Term<Name> {
    let mut term = term;
    for arg in arguments.iter().rev() {
        let name = arg
            .arg_name
            .get_variable_name()
            .map(|arg| interner.lookup_interned(&arg.to_string()))
            .unwrap_or_else(|| "_".to_string());

        if !matches!(arg.tipo.get_uplc_type(), Some(UplcType::Data) | None) {
            term = term
                .lambda(&name)
                .apply(known_data_to_type(Term::var(&name), &arg.tipo));
        }

        term = term.lambda(name)
    }
    term
}

pub fn wrap_validator_condition(air_tree: AirTree, trace: TraceLevel) -> AirTree {
    let otherwise = match trace {
        TraceLevel::Silent | TraceLevel::Compact => AirTree::error(Type::void(), true),
        TraceLevel::Verbose => AirTree::trace(
            AirTree::string("Validator returned false"),
            Type::void(),
            AirTree::error(Type::void(), true),
        ),
    };

    AirTree::if_branch(Type::void(), air_tree, AirTree::void(), otherwise)
}

pub fn extract_constant(term: &Term<Name>) -> Option<Rc<UplcConstant>> {
    let mut constant = None;

    if let Term::Constant(c) = term {
        constant = Some(c.clone());
    } else if let Term::Apply { function, argument } = term {
        if let Term::Constant(c) = argument.as_ref() {
            if let Term::Builtin(b) = function.as_ref() {
                if matches!(
                    b,
                    DefaultFunction::BData
                        | DefaultFunction::IData
                        | DefaultFunction::MapData
                        | DefaultFunction::ListData
                ) {
                    constant = Some(c.clone());
                }
            }
        }
    }
    constant
}

pub fn get_src_code_by_span(
    module_name: &str,
    span: &Span,
    module_src: &IndexMap<&str, &(String, LineNumbers)>,
) -> String {
    assert!(
        *span != Span::empty(),
        "tried to lookup source code from empty location"
    );

    let (src, _) = module_src
        .get(module_name)
        .unwrap_or_else(|| panic!("Missing module {module_name}"));

    src.get(span.start..span.end)
        .expect("Out of bounds span")
        .to_string()
}

pub fn get_line_columns_by_span(
    module_name: &str,
    span: &Span,
    module_src: &IndexMap<&str, &(String, LineNumbers)>,
) -> LineColumn {
    assert!(
        *span != Span::empty(),
        "tried to lookup line & columns from empty location"
    );

    let (_, lines) = module_src
        .get(module_name)
        .unwrap_or_else(|| panic!("Missing module {module_name}"));

    lines
        .line_and_column_number(span.start)
        .expect("Out of bounds span")
}

pub fn introduce_pattern(interner: &mut AirInterner, pattern: &TypedPattern) {
    match pattern {
        Pattern::Int { .. } | Pattern::ByteArray { .. } | Pattern::Discard { .. } => (),

        Pattern::Var { name, .. } => {
            interner.intern(name.clone());
        }
        Pattern::Assign { name, pattern, .. } => {
            interner.intern(name.clone());
            introduce_pattern(interner, pattern);
        }

        Pattern::List { elements, tail, .. } => {
            elements.iter().for_each(|element| {
                introduce_pattern(interner, element);
            });

            tail.iter().for_each(|tail| {
                introduce_pattern(interner, tail);
            });
        }
        Pattern::Constructor {
            arguments: elems, ..
        } => {
            elems.iter().for_each(|element| {
                introduce_pattern(interner, &element.value);
            });
        }
        Pattern::Tuple { elems, .. } => {
            elems.iter().for_each(|element| {
                introduce_pattern(interner, element);
            });
        }
        Pattern::Pair { fst, snd, .. } => {
            introduce_pattern(interner, fst);
            introduce_pattern(interner, snd);
        }
    }
}

pub fn pop_pattern(interner: &mut AirInterner, pattern: &TypedPattern) {
    match pattern {
        Pattern::Int { .. } | Pattern::ByteArray { .. } | Pattern::Discard { .. } => (),

        Pattern::Var { name, .. } => {
            interner.pop_text(name.clone());
        }
        Pattern::Assign { name, pattern, .. } => {
            interner.pop_text(name.clone());
            pop_pattern(interner, pattern);
        }

        Pattern::List { elements, tail, .. } => {
            elements.iter().for_each(|element| {
                pop_pattern(interner, element);
            });

            tail.iter().for_each(|tail| {
                pop_pattern(interner, tail);
            });
        }
        Pattern::Constructor {
            arguments: elems, ..
        } => {
            elems.iter().for_each(|element| {
                pop_pattern(interner, &element.value);
            });
        }
        Pattern::Tuple { elems, .. } => {
            elems.iter().for_each(|element| {
                pop_pattern(interner, element);
            });
        }
        Pattern::Pair { fst, snd, .. } => {
            pop_pattern(interner, fst);
            pop_pattern(interner, snd);
        }
    }
}

pub fn introduce_name(interner: &mut AirInterner, name: &String) -> String {
    interner.intern(name.clone());

    interner.lookup_interned(name)
}
