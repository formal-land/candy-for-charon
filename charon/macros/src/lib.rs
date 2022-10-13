//! This module contains some macros for Charon. Due to technical reasons, Rust
//! forces users to define such macros in a separate, dedicated library. Note
//! that this doesn't apply to `macro_rules`.

extern crate proc_macro;
extern crate syn;
use proc_macro::{TokenStream, TokenTree};
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::vec::Vec;
use syn::punctuated::Punctuated;
use syn::token::{Add, Comma};
use syn::{
    parse, Binding, Constraint, Data, DataEnum, DeriveInput, Expr, Fields, GenericArgument,
    GenericParam, Ident, Lifetime, Lit, Path, PathArguments, PathSegment, TraitBound,
    TraitBoundModifier, Type, TypeParamBound, TypePath, WhereClause, WherePredicate,
};

const _TAB: &'static str = "    ";
const _TWO_TABS: &'static str = "        ";
const THREE_TABS: &'static str = "            ";

/// This is very annoying, but we can't use a global constant string in `format`:
/// we need to define a macro to return a string literal.
macro_rules! index_generic_code {
    () => {
        "
pub mod {} {{
    #[derive(std::fmt::Debug, std::clone::Clone, std::marker::Copy,
             std::hash::Hash, std::cmp::PartialEq, std::cmp::Eq,
             std::cmp::PartialOrd, std::cmp::Ord)]
    pub struct Id {{
        index: usize,
    }}

    #[derive(std::fmt::Debug, std::clone::Clone, std::marker::Copy)]
    pub struct Generator {{
        counter: usize,
    }}

    pub type Vector<T> = crate::id_vector::Vector<Id,T>;

    impl Id {{
        pub fn new(init: usize) -> Id {{
            Id {{ index: init }}
        }}
        
        pub fn is_zero(&self) -> bool {{
            self.index == 0
        }}

        pub fn incr(&mut self) {{
            // Overflows are extremely unlikely, but we do want to make sure
            // we panick whenever there is one.
            self.index = self.index.checked_add(1).unwrap();
        }}
    }}

    pub static ZERO: Id = Id {{ index: 0 }};
    pub static ONE: Id = Id {{ index: 1 }};

    impl crate::id_vector::ToUsize for Id {{
        fn to_usize(&self) -> usize {{
            self.index
        }}
    }}

    impl crate::id_vector::Increment for Id {{
        fn incr(&mut self) {{
            self.incr();
        }}
    }}

    impl crate::id_vector::Zero for Id {{
        fn zero() -> Self {{
            Id::new(0)
        }}
    }}

    impl std::fmt::Display for Id {{
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) ->
          std::result::Result<(), std::fmt::Error> {{
            f.write_str(self.index.to_string().as_str())
        }}
    }}
    
    impl serde::Serialize for Id {{
        fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {{
            // usize is not necessarily contained in u32
            assert!(self.index <= std::u32::MAX as usize);
            serializer.serialize_u32(self.index as u32)
        }}
    }}
 
    impl Generator {{
        pub fn new() -> Generator {{
            Generator {{ counter: 0 }}
        }}

        pub fn fresh_id(&mut self) -> Id {{
            // The release version of the code doesn't check for overflows.
            // As the max usize is very large, overflows are extremely
            // unlikely. Still, it is extremely important for our code that
            // no overflows happen on the index counters.
            let index = Id::new(self.counter);
            self.counter = self.counter.checked_add(1).unwrap();
            index
        }}
    }}
}}"
    };
}

/// Generate an index module which contains an index type and a generator
/// for fresh indices. We use it because for the semantics we need to manipulate
/// a lot of different indices (for values, variables, definitions, holes, etc.).
/// For sanity purposes, we prevent any confusing between the different kinds
/// of indices by using different types. The following macro allows us to
/// easily derive those types, and the needed utilities.
#[proc_macro]
pub fn generate_index_type(item: TokenStream) -> TokenStream {
    // Check that the token strem is made of exactly one identifier
    let mut tokens = vec![];
    for token in item {
        tokens.push(token);
    }
    if tokens.len() != 1 {
        panic!("generate_index_type: invalid parameters: should receive exactly one identifier");
    }
    let token = &tokens[0];
    match token {
        TokenTree::Ident(ident) => {
            // Generate the index code
            let code: String = format!(index_generic_code!(), ident.to_string());
            let code: TokenStream = code.parse().unwrap();
            return code;
        }
        _ => {
            panic!(
                "generate_index_type: invalid parameters: should receive exactly one identifier"
            );
        }
    }
}

macro_rules! derive_variant_name_impl_code {
    () => {
        "impl{} {}{}{} {{
    pub fn variant_name(&self) -> &'static str {{
        match self {{
{}
        }}
    }}
}}"
    };
}

macro_rules! derive_variant_index_arity_impl_code {
    () => {
        "impl{} {}{}{} {{
    pub fn variant_index_arity(&self) -> (u32, usize) {{
        match self {{
{}
        }}
    }}
}}"
    };
}

macro_rules! derive_impl_block_code {
    () => {
        "impl{} {}{}{} {{
{}
}}"
    };
}

macro_rules! derive_enum_variant_impl_code {
    () => {
        "    pub fn {}{}(&self) -> {} {{
        match self {{
{}
        }}
    }}"
    };
}

fn lifetime_to_string(lf: &Lifetime) -> String {
    format!("'{}", lf.ident.to_string()).to_string()
}

/// We initially used the convert-case crate, but it converts names like "I32"
/// to "i_32", while we want to get "i32". We thus reimplemented our own converter
/// (which removes one dependency at the same time).
fn to_snake_case(s: &str) -> String {
    let mut snake_case = String::new();

    // We need to keep track of whether the last treated character was
    // lowercase (or not) to prevent this kind of transformations:
    // "VARIANT" -> "v_a_r_i_a_n_t"
    // Note that if we remember whether the last character was uppercase instead,
    // we get things like this:
    // "I32" -> "I3_2"
    let mut last_is_lowercase = false;

    for (_, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if last_is_lowercase {
                snake_case.push('_');
            }
            last_is_lowercase = false;
            snake_case.push(c.to_lowercase().next().unwrap());
        } else {
            last_is_lowercase = true;
            snake_case.push(c);
        }
    }

    snake_case
}

/// TODO: this is also used to format field types, so we have to take all the
/// cases into account
fn type_to_string(ty: &Type) -> String {
    match ty {
        Type::Array(type_array) => format!(
            "[{}; {}]",
            type_to_string(&type_array.elem),
            expr_to_string(&type_array.len)
        )
        .to_string(),
        Type::BareFn(_) => {
            panic!("type_to_string: unexpected type: BareFn");
        }
        Type::Group(_) => {
            panic!("type_to_string: unexpected type: Group");
        }
        Type::ImplTrait(_) => {
            panic!("type_to_string: unexpected type: ImplTrait");
        }
        Type::Infer(_) => {
            panic!("type_to_string: unexpected type: Infer");
        }
        Type::Macro(_) => {
            panic!("type_to_string: unexpected type: Macro");
        }
        Type::Never(_) => {
            panic!("type_to_string: unexpected type: Never");
        }
        Type::Paren(_) => {
            panic!("type_to_string: unexpected type: Paren");
        }
        Type::Path(p) => type_path_to_string(p),
        Type::Ptr(_) => {
            panic!("type_to_string: unexpected type: Ptr");
        }
        Type::Reference(type_ref) => {
            let lifetime = match &type_ref.lifetime {
                None => "".to_string(),
                Some(lf) => lifetime_to_string(lf),
            };
            let mutability = if type_ref.mutability.is_some() {
                format!("&{} mut", lifetime)
            } else {
                format!("&{}", lifetime)
            };

            format!("{} {}", mutability, type_to_string(&type_ref.elem)).to_string()
        }
        Type::Slice(type_slice) => format!("[{}]", type_to_string(&type_slice.elem)).to_string(),
        Type::TraitObject(_) => {
            panic!("type_to_string: unexpected type: TraitObject");
        }
        Type::Tuple(type_tuple) => {
            let tys: Vec<String> = type_tuple
                .elems
                .iter()
                .map(|ty| type_to_string(ty))
                .collect();
            format!("({})", tys.join(", ")).to_string()
        }
        Type::Verbatim(_) => {
            panic!("type_to_string: unexpected type: Verbatim");
        }
        _ => {
            panic!("type_to_string: unexpected type");
        }
    }
}

fn binding_to_string(b: &Binding) -> String {
    format!("{} = {}", b.ident.to_string(), type_to_string(&b.ty)).to_string()
}

fn constraint_to_string(c: &Constraint) -> String {
    format!(
        "{} : {}",
        c.ident.to_string(),
        type_param_bounds_to_string(&c.bounds)
    )
    .to_string()
}

fn lit_to_string(l: &Lit) -> String {
    match l {
        Lit::Str(l) => l.value(),
        Lit::ByteStr(_) => unimplemented!(),
        Lit::Byte(l) => l.value().to_string(),
        Lit::Char(l) => l.value().to_string(),
        Lit::Int(l) => l.base10_digits().to_string(),
        Lit::Float(l) => l.base10_digits().to_string(),
        Lit::Bool(l) => l.value().to_string(),
        Lit::Verbatim(_) => unimplemented!(),
    }
}

/// Converts an expression to a string.
/// For now, only supports the cases useful for the type definitions (literals)
fn expr_to_string(e: &Expr) -> String {
    match e {
        Expr::Lit(lit) => lit_to_string(&lit.lit),
        _ => unimplemented!(),
    }
}

fn angle_bracketed_generic_arguments_to_string(
    args: &Punctuated<GenericArgument, Comma>,
) -> String {
    let args: Vec<String> = args.iter().map(|a| generic_argument_to_string(a)).collect();
    if args.is_empty() {
        "".to_string()
    } else {
        format!("<{}>", args.join(", ")).to_string()
    }
}

fn generic_argument_to_string(a: &GenericArgument) -> String {
    match a {
        GenericArgument::Lifetime(lf) => lifetime_to_string(lf),
        GenericArgument::Type(ty) => type_to_string(ty),
        GenericArgument::Binding(b) => binding_to_string(b),
        GenericArgument::Constraint(c) => constraint_to_string(c),
        GenericArgument::Const(e) => expr_to_string(e),
    }
}

fn path_segment_to_string(ps: &PathSegment) -> String {
    let seg = ps.ident.to_string();

    match &ps.arguments {
        PathArguments::None => seg,
        PathArguments::AngleBracketed(args) => format!(
            "{}{}",
            seg,
            angle_bracketed_generic_arguments_to_string(&args.args)
        )
        .to_string(),
        PathArguments::Parenthesized(_) => {
            // Don't know in which situation this may happen
            unimplemented!()
        }
    }
}

fn path_to_string(path: &Path) -> String {
    let path: Vec<String> = path
        .segments
        .iter()
        .map(|x| path_segment_to_string(x))
        .collect();
    path.join("::")
}

fn type_path_to_string(tp: &TypePath) -> String {
    // Don't know what to do with that
    assert!(tp.qself.is_none());

    path_to_string(&tp.path)
}

fn trait_bound_to_string(tb: &TraitBound) -> String {
    // Sanity check
    match tb.modifier {
        TraitBoundModifier::None => (),
        TraitBoundModifier::Maybe(_) => {
            unimplemented!()
        }
    }

    assert!(tb.lifetimes.is_none());

    path_to_string(&tb.path)
}

fn type_param_bounds_to_string(bounds: &Punctuated<TypeParamBound, Add>) -> String {
    let mut s: Vec<String> = vec![];

    for p in bounds {
        match p {
            TypeParamBound::Trait(tb) => {
                s.push(trait_bound_to_string(tb));
            }
            TypeParamBound::Lifetime(lf) => {
                s.push(lifetime_to_string(lf));
            }
        }
    }

    s.join(" + ")
}

fn lifetime_bounds_to_string(bounds: &Punctuated<Lifetime, Add>) -> String {
    let bounds: Vec<String> = bounds.iter().map(|lf| lifetime_to_string(lf)).collect();
    bounds.join(" + ")
}

/// Auxiliary helper
fn generic_param_with_opt_constraints_to_string(
    param: &GenericParam,
    with_constraints: bool,
) -> String {
    match param {
        GenericParam::Type(type_param) => {
            let ident = type_param.ident.to_string();

            if type_param.bounds.is_empty() || !with_constraints {
                ident
            } else {
                format!(
                    "{} : {}",
                    ident,
                    type_param_bounds_to_string(&type_param.bounds)
                )
                .to_string()
            }
        }
        GenericParam::Lifetime(lf_param) => {
            let ident = lifetime_to_string(&lf_param.lifetime);

            if lf_param.bounds.is_empty() || !with_constraints {
                ident
            } else {
                format!(
                    "{} : {}",
                    ident,
                    lifetime_bounds_to_string(&lf_param.bounds)
                )
                .to_string()
            }
        }
        GenericParam::Const(_) => {
            // Don't know what to do with const parameters
            unimplemented!()
        }
    }
}

/// Generate a string from generic parameters.
/// `with_constraints` constrols whether we should format the constraints or not.
/// For instance, should we generate: `<'a, T1 : 'a, T2 : Clone>` or ``<'a, T1, T2>`?
fn generic_params_with_opt_constraints_to_string(
    params: &Punctuated<GenericParam, Comma>,
    with_constraints: bool,
) -> String {
    let gens: Vec<String> = params
        .iter()
        .map(|g| generic_param_with_opt_constraints_to_string(g, with_constraints))
        .collect();
    if gens.is_empty() {
        "".to_string()
    } else {
        format!("<{}>", gens.join(", "))
    }
}

/// See [`generic_params_with_opt_constraints_to_string`](generic_params_with_opt_constraints_to_string)
fn generic_params_to_string(params: &Punctuated<GenericParam, Comma>) -> String {
    generic_params_with_opt_constraints_to_string(params, true)
}

/// See [`generic_params_with_opt_constraints_to_string`](generic_params_with_opt_constraints_to_string)
fn generic_params_without_constraints_to_string(
    params: &Punctuated<GenericParam, Comma>,
) -> String {
    generic_params_with_opt_constraints_to_string(params, false)
}

fn where_predicate_to_string(wp: &WherePredicate) -> String {
    match wp {
        WherePredicate::Type(pred_type) => {
            assert!(pred_type.lifetimes.is_none());

            let ty = type_to_string(&pred_type.bounded_ty);

            if pred_type.bounds.is_empty() {
                ty
            } else {
                format!(
                    "{} : {}",
                    ty,
                    type_param_bounds_to_string(&pred_type.bounds)
                )
                .to_string()
            }
        }
        WherePredicate::Lifetime(pred_lf) => format!(
            "{} : {}",
            lifetime_to_string(&pred_lf.lifetime),
            lifetime_bounds_to_string(&pred_lf.bounds)
        )
        .to_string(),
        WherePredicate::Eq(pred_eq) => format!(
            "{} = {}",
            type_to_string(&pred_eq.lhs_ty),
            type_to_string(&pred_eq.rhs_ty)
        )
        .to_string(),
    }
}

fn where_clause_to_string(wc: &WhereClause) -> String {
    let preds = wc.predicates.iter().map(|p| where_predicate_to_string(p));
    let preds: Vec<String> = preds.map(|p| format!("    {},\n", p).to_string()).collect();
    format!("\nwhere\n{}", preds.join("")).to_string()
}

fn opt_where_clause_to_string(wc: &Option<WhereClause>) -> String {
    match wc {
        None => "".to_string(),
        Some(wc) => where_clause_to_string(wc),
    }
}

struct MatchPattern {
    /// The variant id
    variant_id: Ident,
    /// The match pattern as a string.
    /// For instance: `List::Cons(hd, tl)`
    match_pattern: String,
    /// The number of arguments in the match pattern (including anonymous
    /// arguments).
    num_args: usize,
    /// The variables we introduced in the match pattern.
    /// `["hd", "tl"]` if the pattern is `List::Cons(hd, tl)`.
    /// Empty vector if the variables are anonymous (i.e.: `_`).
    named_args: Vec<String>,
    /// The types of the variables introduced in the match pattern
    arg_types: Vec<String>,
}

/// Generate matching patterns for an enumeration
/// `patvar_name` controls the name to give to the variables introduced in the
/// pattern. We introduce anonymous variables if `None`.
fn generate_variant_match_patterns(
    enum_name: &String,
    data: &DataEnum,
    patvar_name: Option<&String>,
) -> Vec<MatchPattern> {
    let mut patterns: Vec<MatchPattern> = vec![];
    for variant in &data.variants {
        let variant_name = variant.ident.to_string();

        // Indices for variables
        let mut var_index: usize = 0;
        fn generate_varname(var_index: &mut usize, patvar_name: Option<&String>) -> String {
            match patvar_name {
                None => "_".to_string(),
                Some(v) => {
                    let s = format!("{}{}", v, var_index).to_string();
                    *var_index = var_index.checked_add(1).unwrap();
                    s
                }
            }
        }

        // Compute the pattern (without the variant constructor), the list
        // of introduced arguments and the list of field types.
        let (pattern, num_vars, named_vars, vartypes) = match &variant.fields {
            Fields::Named(fields) => {
                let fields_vars: Vec<(String, String)> = fields
                    .named
                    .iter()
                    .map(|f| {
                        let var = generate_varname(&mut var_index, patvar_name);
                        let field = format!("{}:{}", f.ident.as_ref().unwrap().to_string(), var)
                            .to_string();
                        (field, var)
                    })
                    .collect();
                let (fields_pats, vars): (Vec<String>, Vec<String>) =
                    fields_vars.into_iter().unzip();

                let num_vars = fields.named.iter().count();

                let vars = if patvar_name.is_none() { vec![] } else { vars };

                let vartypes: Vec<String> =
                    fields.named.iter().map(|f| type_to_string(&f.ty)).collect();

                let pattern = format!("{{ {} }}", fields_pats.join(", ")).to_string();
                (pattern, num_vars, vars, vartypes)
            }
            Fields::Unnamed(fields) => {
                let fields_vars: Vec<(String, String)> = fields
                    .unnamed
                    .iter()
                    .map(|_| {
                        let var = generate_varname(&mut var_index, patvar_name);
                        (var.clone(), var)
                    })
                    .collect();

                let (fields_pats, vars): (Vec<String>, Vec<String>) =
                    fields_vars.into_iter().unzip();

                let num_vars = fields.unnamed.iter().count();

                let vars = if patvar_name.is_none() { vec![] } else { vars };

                let vartypes: Vec<String> = fields
                    .unnamed
                    .iter()
                    .map(|f| type_to_string(&f.ty))
                    .collect();

                let pattern = format!("({})", fields_pats.join(", ")).to_string();

                (pattern, num_vars, vars, vartypes)
            }
            Fields::Unit => ("".to_string(), 0, vec![], vec![]),
        };

        let pattern = format!("{}::{}{}", enum_name, variant_name, pattern).to_string();
        patterns.push(MatchPattern {
            variant_id: variant.ident.clone(),
            match_pattern: pattern,
            num_args: num_vars,
            named_args: named_vars,
            arg_types: vartypes,
        });
    }

    patterns
}

/// Macro to derive a function `fn variant_name(&self) -> String` printing the
/// constructor of an enumeration. Only works on enumerations, of course.
#[proc_macro_derive(VariantName)]
pub fn derive_variant_name(item: TokenStream) -> TokenStream {
    // Parse the input
    let ast: DeriveInput = parse(item).unwrap();

    // Generate the code
    let adt_name = ast.ident.to_string();

    // Retrieve and format the generic parameters
    let generic_params_with_constraints = generic_params_to_string(&ast.generics.params);
    let generic_params_without_constraints =
        generic_params_without_constraints_to_string(&ast.generics.params);

    // Generat the code for the `where` clause
    let where_clause = opt_where_clause_to_string(&ast.generics.where_clause);

    // Generate the code for the matches
    let match_branches: Vec<String> = match &ast.data {
        Data::Enum(data) => {
            let patterns = generate_variant_match_patterns(&adt_name, data, None);
            patterns
                .iter()
                .map(|mp| {
                    format!(
                        "{}{} => {{ \"{}\" }},",
                        THREE_TABS,
                        mp.match_pattern,
                        mp.variant_id.to_string()
                    )
                    .to_string()
                })
                .collect()
        }
        Data::Struct(_) => {
            panic!("VariantName macro can not be called on structs");
        }
        Data::Union(_) => {
            panic!("VariantName macro can not be called on unions");
        }
    };

    if match_branches.len() > 0 {
        let match_branches = match_branches.join("\n");
        let impl_code = format!(
            derive_variant_name_impl_code!(),
            generic_params_with_constraints,
            adt_name,
            generic_params_without_constraints,
            where_clause,
            match_branches
        )
        .to_string();
        return impl_code.parse().unwrap();
    } else {
        "".parse().unwrap()
    }
}

/// Macro to derive a function `fn variant_index_arity(&self) -> (u32, usize)`
/// the pair (variant index, variant arity).
/// Only works on enumerations, of course.
#[proc_macro_derive(VariantIndexArity)]
pub fn derive_variant_index_arity(item: TokenStream) -> TokenStream {
    // Parse the input
    let ast: DeriveInput = parse(item).unwrap();

    // Generate the code
    let adt_name = ast.ident.to_string();

    // Retrieve and format the generic parameters
    let generic_params_with_constraints = generic_params_to_string(&ast.generics.params);
    let generic_params_without_constraints =
        generic_params_without_constraints_to_string(&ast.generics.params);

    // Generat the code for the `where` clause
    let where_clause = opt_where_clause_to_string(&ast.generics.where_clause);

    // Generate the code for the matches
    let match_branches: Vec<String> = match &ast.data {
        Data::Enum(data) => {
            let patterns = generate_variant_match_patterns(&adt_name, data, None);
            patterns
                .iter()
                .enumerate()
                .map(|(i, mp)| {
                    format!(
                        "{}{} => {{ ({}, {}) }},",
                        THREE_TABS, mp.match_pattern, i, mp.num_args
                    )
                    .to_string()
                })
                .collect()
        }
        Data::Struct(_) => {
            panic!("VariantIndex macro can not be called on structs");
        }
        Data::Union(_) => {
            panic!("VariantIndex macro can not be called on unions");
        }
    };

    if match_branches.len() > 0 {
        let match_branches = match_branches.join("\n");
        let impl_code = format!(
            derive_variant_index_arity_impl_code!(),
            generic_params_with_constraints,
            adt_name,
            generic_params_without_constraints,
            where_clause,
            match_branches
        )
        .to_string();
        return impl_code.parse().unwrap();
    } else {
        "".parse().unwrap()
    }
}

#[derive(PartialEq, Eq)]
enum EnumMethodKind {
    EnumIsA,
    EnumAsGetters,
}

impl EnumMethodKind {
    /// We have to write this by hand: we can't use the macros defined above on
    /// the declarations of this file...
    fn variant_name(&self) -> String {
        match self {
            EnumMethodKind::EnumIsA => "EnumIsA".to_string(),
            EnumMethodKind::EnumAsGetters => "EnumAsGetters".to_string(),
        }
    }
}

/// Generic helper for `EnumIsA` and `EnumAsGetters`.
/// This generates one function per variant.
fn derive_enum_variant_method(item: TokenStream, method_kind: EnumMethodKind) -> TokenStream {
    // Parse the input
    let ast: DeriveInput = parse(item).unwrap();

    // Generate the code
    let adt_name = ast.ident.to_string();

    // Retrieve and format the generic parameters
    let generic_params_with_constraints = generic_params_to_string(&ast.generics.params);
    let generic_params_without_constraints =
        generic_params_without_constraints_to_string(&ast.generics.params);

    // Generat the code for the `where` clause
    let where_clause = opt_where_clause_to_string(&ast.generics.where_clause);

    // Generate the code for all the functions in the impl block
    let impls: Vec<String> = match &ast.data {
        Data::Enum(data) => {
            // We start by generating the body of the function: the matches.
            //
            // If there is only one variant, we generate:
            // ```
            //  match self {
            //      Foo::Variant(...) => ...,
            // }
            // ```
            //
            // If there is more than one variant, we generate an otherwise branch:
            // ```
            //  match self {
            //      Foo::Variant(...) => ...,
            //      _ => ...,
            // }
            // ```
            //
            // Finally, If there are no variants, we don't generate any function,
            // so we don't really have to take that case into account...
            let several_variants = data.variants.len() > 1;
            let varbasename = match method_kind {
                EnumMethodKind::EnumIsA => None,
                EnumMethodKind::EnumAsGetters => Some("x".to_string()),
            };
            let patterns = generate_variant_match_patterns(&adt_name, data, varbasename.as_ref());

            match method_kind {
                EnumMethodKind::EnumIsA => {
                    patterns
                        .iter()
                        .map(|mp| {
                            // Generate the branch for the target variant
                            let true_pat =
                                format!("{}{} => true,", THREE_TABS, mp.match_pattern,).to_string();
                            // Add the otherwise branch, if necessary
                            let complete_pat = if several_variants {
                                format!("{}\n{}_ => false,", true_pat, THREE_TABS).to_string()
                            } else {
                                true_pat
                            };

                            // Generate the impl
                            format!(
                                derive_enum_variant_impl_code!(),
                                "is_",
                                to_snake_case(&mp.variant_id.to_string()),
                                "bool",
                                complete_pat
                            )
                            .to_string()
                        })
                        .collect()
                }
                EnumMethodKind::EnumAsGetters => {
                    patterns
                        .iter()
                        .map(|mp| {
                            // Generate the branch for the target variant
                            let vars = format!("({})", mp.named_args.join(", ")); // return value
                            let variant_pat =
                                format!("{}{} => {},", THREE_TABS, mp.match_pattern, vars)
                                    .to_string();
                            // Add the otherwise branch, if necessary
                            let complete_pat = if several_variants {
                                format!(
                                    "{}\n{}_ => unreachable!(\"{}::as_{}: Not the proper variant\"),",
                                    variant_pat, THREE_TABS, adt_name, to_snake_case(&mp.variant_id.to_string()),
                                )
                                .to_string()
                            } else {
                                variant_pat
                            };

                            // The function's return type
                            let ret_tys: Vec<String> = mp
                                .arg_types
                                .iter()
                                .map(|ty| format!("&({})", ty.to_string()))
                                .collect();
                            let ret_ty = format!("({})", ret_tys.join(", "));

                            // Generate the impl
                            format!(
                                derive_enum_variant_impl_code!(),
                                "as_",
                                // TODO: write our own to_snake_case function:
                                // names like "i32" become "i_32" with this one.
                                to_snake_case(&mp.variant_id.to_string()),
                                ret_ty,
                                complete_pat
                            )
                            .to_string()
                        })
                        .collect()
                }
            }
        }
        Data::Struct(_) => {
            panic!(
                "{} macro can not be called on structs",
                method_kind.variant_name()
            );
        }
        Data::Union(_) => {
            panic!(
                "{} macro can not be called on unions",
                method_kind.variant_name()
            );
        }
    };

    if impls.len() > 0 {
        // Concatenate all the functions
        let impls = impls.join("\n\n");

        // Generate the impl block
        let impl_code = format!(
            derive_impl_block_code!(),
            generic_params_with_constraints,
            adt_name,
            generic_params_without_constraints,
            where_clause,
            impls
        )
        .to_string();
        return impl_code.parse().unwrap();
    } else {
        return "".parse().unwrap();
    }
}

/// Macro `EnumIsA`
/// Derives functions of the form `fn is_{variant_name}(&self) -> bool` returning true
/// if an enumeration instance is of some variant. For lists, it would generate
/// `is_cons` and `is_nil`.
/// Note that there already exists a crate implementing such macros,
/// [`enum_methods`](https://docs.rs/enum-methods/0.0.8/enum_methods/), but
/// it doesn't work when the enumeration has generic parameters and it seems
/// dead (a PR from 2019 has never been merged), so it seems better to maintain
/// our own code here (which is small) rather than doing PRs for this crate.
#[proc_macro_derive(EnumIsA)]
pub fn derive_enum_is_a(item: TokenStream) -> TokenStream {
    derive_enum_variant_method(item, EnumMethodKind::EnumIsA)
}

/// Macro `EnumAsGetters`
/// Derives functions of the form `fn as_{variant_name}(&self) -> ...` checking
/// that an enumeration instance is of the proper variant and returning shared
/// borrows to its fields.
/// Also see the comments for [`derive_enum_is_a`](derive_enum_is_a)
#[proc_macro_derive(EnumAsGetters)]
pub fn derive_enum_as_getters(item: TokenStream) -> TokenStream {
    derive_enum_variant_method(item, EnumMethodKind::EnumAsGetters)
}

/// This struct is used to deserialize the "rust-toolchain" file.
#[derive(Deserialize)]
struct RustToolchain {
    toolchain: Toolchain,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct Toolchain {
    channel: String,
    components: Vec<String>,
}

/// The following macro retrieves the rust compiler version from the
/// "rust-toolchain" file at compile time. We need it at exactly one place.
#[proc_macro]
pub fn rust_version(_item: TokenStream) -> TokenStream {
    let mut file = File::open("rust-toolchain").unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    let toolchain: RustToolchain = toml::from_str(&contents).unwrap();
    format!("\"+{}\"", toolchain.toolchain.channel)
        .parse()
        .unwrap()
}

#[test]
fn test_snake_case() {
    let s = to_snake_case("ConstantValue");
    println!("{}", s);
    assert!(s == "constant_value".to_string());
}
