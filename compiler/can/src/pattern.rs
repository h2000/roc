use crate::abilities::AbilitiesStore;
use crate::annotation::freshen_opaque_def;
use crate::env::Env;
use crate::expr::{canonicalize_expr, unescape_char, Expr, IntValue, Output};
use crate::num::{
    finish_parsing_base, finish_parsing_float, finish_parsing_num, FloatBound, IntBound,
    NumericBound, ParsedNumResult,
};
use crate::scope::Scope;
use roc_module::ident::{Ident, Lowercase, TagName};
use roc_module::symbol::Symbol;
use roc_parse::ast::{self, StrLiteral, StrSegment};
use roc_parse::pattern::PatternType;
use roc_problem::can::{MalformedPatternProblem, Problem, RuntimeError};
use roc_region::all::{Loc, Region};
use roc_types::subs::{VarStore, Variable};
use roc_types::types::{LambdaSet, Type};

/// A pattern, including possible problems (e.g. shadowing) so that
/// codegen can generate a runtime error if this pattern is reached.
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    Identifier(Symbol),
    AppliedTag {
        whole_var: Variable,
        ext_var: Variable,
        tag_name: TagName,
        arguments: Vec<(Variable, Loc<Pattern>)>,
    },
    UnwrappedOpaque {
        whole_var: Variable,
        opaque: Symbol,
        argument: Box<(Variable, Loc<Pattern>)>,

        // The following help us link this opaque reference to the type specified by its
        // definition, which we then use during constraint generation. For example
        // suppose we have
        //
        //   Id n := [ Id U64 n ]
        //   strToBool : Str -> Bool
        //
        //   f = \@Id who -> strToBool who
        //
        // Then `opaque` is "Id", `argument` is "who", but this is not enough for us to
        // infer the type of the expression as "Id Str" - we need to link the specialized type of
        // the variable "n".
        // That's what `specialized_def_type` and `type_arguments` are for; they are specialized
        // for the expression from the opaque definition. `type_arguments` is something like
        // [(n, fresh1)], and `specialized_def_type` becomes "[ Id U64 fresh1 ]".
        specialized_def_type: Box<Type>,
        type_arguments: Vec<(Lowercase, Type)>,
        lambda_set_variables: Vec<LambdaSet>,
    },
    RecordDestructure {
        whole_var: Variable,
        ext_var: Variable,
        destructs: Vec<Loc<RecordDestruct>>,
    },
    NumLiteral(Variable, Box<str>, IntValue, NumericBound),
    IntLiteral(Variable, Variable, Box<str>, IntValue, IntBound),
    FloatLiteral(Variable, Variable, Box<str>, f64, FloatBound),
    StrLiteral(Box<str>),
    SingleQuote(char),
    Underscore,

    /// An identifier that marks a specialization of an ability member.
    /// For example, given an ability member definition `hash : a -> U64 | a has Hash`,
    /// there may be the specialization `hash : Bool -> U64`. In this case we generate a
    /// new symbol for the specailized "hash" identifier.
    AbilityMemberSpecialization {
        /// The symbol for this specialization.
        ident: Symbol,
        /// The ability name being specialized.
        specializes: Symbol,
    },

    // Runtime Exceptions
    Shadowed(Region, Loc<Ident>, Symbol),
    OpaqueNotInScope(Loc<Ident>),
    // Example: (5 = 1 + 2) is an unsupported pattern in an assignment; Int patterns aren't allowed in assignments!
    UnsupportedPattern(Region),
    // parse error patterns
    MalformedPattern(MalformedPatternProblem, Region),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordDestruct {
    pub var: Variable,
    pub label: Lowercase,
    pub symbol: Symbol,
    pub typ: DestructType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DestructType {
    Required,
    Optional(Variable, Loc<Expr>),
    Guard(Variable, Loc<Pattern>),
}

pub fn symbols_from_pattern(pattern: &Pattern) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    symbols_from_pattern_help(pattern, &mut symbols);

    symbols
}

pub fn symbols_from_pattern_help(pattern: &Pattern, symbols: &mut Vec<Symbol>) {
    use Pattern::*;

    match pattern {
        Identifier(symbol) | Shadowed(_, _, symbol) => {
            symbols.push(*symbol);
        }

        AbilityMemberSpecialization { ident, specializes } => {
            symbols.push(*ident);
            symbols.push(*specializes);
        }

        AppliedTag { arguments, .. } => {
            for (_, nested) in arguments {
                symbols_from_pattern_help(&nested.value, symbols);
            }
        }
        UnwrappedOpaque {
            opaque, argument, ..
        } => {
            symbols.push(*opaque);
            let (_, nested) = &**argument;
            symbols_from_pattern_help(&nested.value, symbols);
        }
        RecordDestructure { destructs, .. } => {
            for destruct in destructs {
                // when a record field has a pattern guard, only symbols in the guard are introduced
                if let DestructType::Guard(_, subpattern) = &destruct.value.typ {
                    symbols_from_pattern_help(&subpattern.value, symbols);
                } else {
                    symbols.push(destruct.value.symbol);
                }
            }
        }

        NumLiteral(..)
        | IntLiteral(..)
        | FloatLiteral(..)
        | StrLiteral(_)
        | SingleQuote(_)
        | Underscore
        | MalformedPattern(_, _)
        | UnsupportedPattern(_)
        | OpaqueNotInScope(..) => {}
    }
}

pub fn canonicalize_def_header_pattern<'a>(
    env: &mut Env<'a>,
    var_store: &mut VarStore,
    scope: &mut Scope,
    abilities_store: &AbilitiesStore,
    pattern_type: PatternType,
    pattern: &ast::Pattern<'a>,
    region: Region,
) -> (Output, Loc<Pattern>) {
    use roc_parse::ast::Pattern::*;

    let mut output = Output::default();
    match pattern {
        // Identifiers that shadow ability members may appear (and may only appear) at the header of a def.
        Identifier(name) => match scope.introduce_or_shadow_ability_member(
            (*name).into(),
            &env.exposed_ident_ids,
            &mut env.ident_ids,
            region,
            abilities_store,
        ) {
            Ok((symbol, shadowing_ability_member)) => {
                output.references.bound_symbols.insert(symbol);
                let can_pattern = match shadowing_ability_member {
                    // A fresh identifier.
                    None => Pattern::Identifier(symbol),
                    // Likely a specialization of an ability.
                    Some(ability_member_name) => Pattern::AbilityMemberSpecialization {
                        ident: symbol,
                        specializes: ability_member_name,
                    },
                };
                (output, Loc::at(region, can_pattern))
            }
            Err((original_region, shadow, new_symbol)) => {
                env.problem(Problem::RuntimeError(RuntimeError::Shadowing {
                    original_region,
                    shadow: shadow.clone(),
                }));
                output.references.bound_symbols.insert(new_symbol);

                let can_pattern = Pattern::Shadowed(original_region, shadow, new_symbol);
                (output, Loc::at(region, can_pattern))
            }
        },
        _ => canonicalize_pattern(env, var_store, scope, pattern_type, pattern, region),
    }
}

pub fn canonicalize_pattern<'a>(
    env: &mut Env<'a>,
    var_store: &mut VarStore,
    scope: &mut Scope,
    pattern_type: PatternType,
    pattern: &ast::Pattern<'a>,
    region: Region,
) -> (Output, Loc<Pattern>) {
    use roc_parse::ast::Pattern::*;
    use PatternType::*;

    let mut output = Output::default();
    let can_pattern = match pattern {
        Identifier(name) => match scope.introduce(
            (*name).into(),
            &env.exposed_ident_ids,
            &mut env.ident_ids,
            region,
        ) {
            Ok(symbol) => {
                output.references.bound_symbols.insert(symbol);

                Pattern::Identifier(symbol)
            }
            Err((original_region, shadow, new_symbol)) => {
                env.problem(Problem::RuntimeError(RuntimeError::Shadowing {
                    original_region,
                    shadow: shadow.clone(),
                }));
                output.references.bound_symbols.insert(new_symbol);

                Pattern::Shadowed(original_region, shadow, new_symbol)
            }
        },
        GlobalTag(name) => {
            // Canonicalize the tag's name.
            Pattern::AppliedTag {
                whole_var: var_store.fresh(),
                ext_var: var_store.fresh(),
                tag_name: TagName::Global((*name).into()),
                arguments: vec![],
            }
        }
        PrivateTag(name) => {
            let ident_id = env.ident_ids.get_or_insert(&(*name).into());

            // Canonicalize the tag's name.
            Pattern::AppliedTag {
                whole_var: var_store.fresh(),
                ext_var: var_store.fresh(),
                tag_name: TagName::Private(Symbol::new(env.home, ident_id)),
                arguments: vec![],
            }
        }
        OpaqueRef(name) => {
            // If this opaque ref had an argument, we would be in the "Apply" branch.
            let loc_name = Loc::at(region, (*name).into());
            env.problem(Problem::RuntimeError(RuntimeError::OpaqueNotApplied(
                loc_name,
            )));
            Pattern::UnsupportedPattern(region)
        }
        Apply(tag, patterns) => {
            let mut can_patterns = Vec::with_capacity(patterns.len());
            for loc_pattern in *patterns {
                let (new_output, can_pattern) = canonicalize_pattern(
                    env,
                    var_store,
                    scope,
                    pattern_type,
                    &loc_pattern.value,
                    loc_pattern.region,
                );

                output.union(new_output);

                can_patterns.push((var_store.fresh(), can_pattern));
            }

            match tag.value {
                GlobalTag(name) => {
                    let tag_name = TagName::Global(name.into());
                    Pattern::AppliedTag {
                        whole_var: var_store.fresh(),
                        ext_var: var_store.fresh(),
                        tag_name,
                        arguments: can_patterns,
                    }
                }
                PrivateTag(name) => {
                    let ident_id = env.ident_ids.get_or_insert(&name.into());
                    let tag_name = TagName::Private(Symbol::new(env.home, ident_id));

                    Pattern::AppliedTag {
                        whole_var: var_store.fresh(),
                        ext_var: var_store.fresh(),
                        tag_name,
                        arguments: can_patterns,
                    }
                }

                OpaqueRef(name) => match scope.lookup_opaque_ref(name, tag.region) {
                    Ok((opaque, opaque_def)) => {
                        debug_assert!(!can_patterns.is_empty());

                        if can_patterns.len() > 1 {
                            env.problem(Problem::RuntimeError(
                                RuntimeError::OpaqueAppliedToMultipleArgs(region),
                            ));

                            Pattern::UnsupportedPattern(region)
                        } else {
                            let argument = Box::new(can_patterns.pop().unwrap());

                            let (type_arguments, lambda_set_variables, specialized_def_type) =
                                freshen_opaque_def(var_store, opaque_def);

                            output.references.referenced_type_defs.insert(opaque);
                            output.references.type_lookups.insert(opaque);

                            Pattern::UnwrappedOpaque {
                                whole_var: var_store.fresh(),
                                opaque,
                                argument,
                                specialized_def_type: Box::new(specialized_def_type),
                                type_arguments,
                                lambda_set_variables,
                            }
                        }
                    }
                    Err(runtime_error) => {
                        env.problem(Problem::RuntimeError(runtime_error));

                        Pattern::OpaqueNotInScope(Loc::at(tag.region, name.into()))
                    }
                },
                _ => unreachable!("Other patterns cannot be applied"),
            }
        }

        &FloatLiteral(str) => match pattern_type {
            WhenBranch => match finish_parsing_float(str) {
                Err(_error) => {
                    let problem = MalformedPatternProblem::MalformedFloat;
                    malformed_pattern(env, problem, region)
                }
                Ok((str_without_suffix, float, bound)) => Pattern::FloatLiteral(
                    var_store.fresh(),
                    var_store.fresh(),
                    str_without_suffix.into(),
                    float,
                    bound,
                ),
            },
            ptype => unsupported_pattern(env, ptype, region),
        },

        Underscore(_) => match pattern_type {
            WhenBranch | FunctionArg => Pattern::Underscore,
            TopLevelDef | DefExpr => bad_underscore(env, region),
        },

        &NumLiteral(str) => match pattern_type {
            WhenBranch => match finish_parsing_num(str) {
                Err(_error) => {
                    let problem = MalformedPatternProblem::MalformedInt;
                    malformed_pattern(env, problem, region)
                }
                Ok(ParsedNumResult::UnknownNum(int, bound)) => {
                    Pattern::NumLiteral(var_store.fresh(), (str).into(), int, bound)
                }
                Ok(ParsedNumResult::Int(int, bound)) => Pattern::IntLiteral(
                    var_store.fresh(),
                    var_store.fresh(),
                    (str).into(),
                    int,
                    bound,
                ),
                Ok(ParsedNumResult::Float(float, bound)) => Pattern::FloatLiteral(
                    var_store.fresh(),
                    var_store.fresh(),
                    (str).into(),
                    float,
                    bound,
                ),
            },
            ptype => unsupported_pattern(env, ptype, region),
        },

        &NonBase10Literal {
            string,
            base,
            is_negative,
        } => match pattern_type {
            WhenBranch => match finish_parsing_base(string, base, is_negative) {
                Err(_error) => {
                    let problem = MalformedPatternProblem::MalformedBase(base);
                    malformed_pattern(env, problem, region)
                }
                Ok((IntValue::U128(_), _)) if is_negative => {
                    // Can't negate a u128; that doesn't fit in any integer literal type we support.
                    let problem = MalformedPatternProblem::MalformedInt;
                    malformed_pattern(env, problem, region)
                }
                Ok((int, bound)) => {
                    let sign_str = if is_negative { "-" } else { "" };
                    let int_str = format!("{}{}", sign_str, int).into_boxed_str();
                    let i = match int {
                        // Safety: this is fine because I128::MAX = |I128::MIN| - 1
                        IntValue::I128(n) if is_negative => IntValue::I128(-n),
                        IntValue::I128(n) => IntValue::I128(n),
                        IntValue::U128(_) => unreachable!(),
                    };
                    Pattern::IntLiteral(var_store.fresh(), var_store.fresh(), int_str, i, bound)
                }
            },
            ptype => unsupported_pattern(env, ptype, region),
        },

        StrLiteral(literal) => match pattern_type {
            WhenBranch => flatten_str_literal(literal),
            ptype => unsupported_pattern(env, ptype, region),
        },

        SingleQuote(string) => {
            let mut it = string.chars().peekable();
            if let Some(char) = it.next() {
                if it.peek().is_none() {
                    Pattern::SingleQuote(char)
                } else {
                    // multiple chars is found
                    let problem = MalformedPatternProblem::MultipleCharsInSingleQuote;
                    malformed_pattern(env, problem, region)
                }
            } else {
                // no characters found
                let problem = MalformedPatternProblem::EmptySingleQuote;
                malformed_pattern(env, problem, region)
            }
        }

        SpaceBefore(sub_pattern, _) | SpaceAfter(sub_pattern, _) => {
            return canonicalize_pattern(env, var_store, scope, pattern_type, sub_pattern, region)
        }
        RecordDestructure(patterns) => {
            let ext_var = var_store.fresh();
            let whole_var = var_store.fresh();
            let mut destructs = Vec::with_capacity(patterns.len());
            let mut opt_erroneous = None;

            for loc_pattern in patterns.iter() {
                match loc_pattern.value {
                    Identifier(label) => {
                        match scope.introduce(
                            label.into(),
                            &env.exposed_ident_ids,
                            &mut env.ident_ids,
                            region,
                        ) {
                            Ok(symbol) => {
                                output.references.bound_symbols.insert(symbol);

                                destructs.push(Loc {
                                    region: loc_pattern.region,
                                    value: RecordDestruct {
                                        var: var_store.fresh(),
                                        label: Lowercase::from(label),
                                        symbol,
                                        typ: DestructType::Required,
                                    },
                                });
                            }
                            Err((original_region, shadow, new_symbol)) => {
                                env.problem(Problem::RuntimeError(RuntimeError::Shadowing {
                                    original_region,
                                    shadow: shadow.clone(),
                                }));

                                // No matter what the other patterns
                                // are, we're definitely shadowed and will
                                // get a runtime exception as soon as we
                                // encounter the first bad pattern.
                                opt_erroneous =
                                    Some(Pattern::Shadowed(original_region, shadow, new_symbol));
                            }
                        };
                    }

                    RequiredField(label, loc_guard) => {
                        // a guard does not introduce the label into scope!
                        let symbol = scope.ignore(label.into(), &mut env.ident_ids);
                        let (new_output, can_guard) = canonicalize_pattern(
                            env,
                            var_store,
                            scope,
                            pattern_type,
                            &loc_guard.value,
                            loc_guard.region,
                        );

                        output.union(new_output);

                        destructs.push(Loc {
                            region: loc_pattern.region,
                            value: RecordDestruct {
                                var: var_store.fresh(),
                                label: Lowercase::from(label),
                                symbol,
                                typ: DestructType::Guard(var_store.fresh(), can_guard),
                            },
                        });
                    }
                    OptionalField(label, loc_default) => {
                        // an optional DOES introduce the label into scope!
                        match scope.introduce(
                            label.into(),
                            &env.exposed_ident_ids,
                            &mut env.ident_ids,
                            region,
                        ) {
                            Ok(symbol) => {
                                let (can_default, expr_output) = canonicalize_expr(
                                    env,
                                    var_store,
                                    scope,
                                    loc_default.region,
                                    &loc_default.value,
                                );

                                // an optional field binds the symbol!
                                output.references.bound_symbols.insert(symbol);

                                output.union(expr_output);

                                destructs.push(Loc {
                                    region: loc_pattern.region,
                                    value: RecordDestruct {
                                        var: var_store.fresh(),
                                        label: Lowercase::from(label),
                                        symbol,
                                        typ: DestructType::Optional(var_store.fresh(), can_default),
                                    },
                                });
                            }
                            Err((original_region, shadow, new_symbol)) => {
                                env.problem(Problem::RuntimeError(RuntimeError::Shadowing {
                                    original_region,
                                    shadow: shadow.clone(),
                                }));

                                // No matter what the other patterns
                                // are, we're definitely shadowed and will
                                // get a runtime exception as soon as we
                                // encounter the first bad pattern.
                                opt_erroneous =
                                    Some(Pattern::Shadowed(original_region, shadow, new_symbol));
                            }
                        };
                    }
                    _ => unreachable!("Any other pattern should have given a parse error"),
                }
            }

            // If we encountered an erroneous pattern (e.g. one with shadowing),
            // use the resulting RuntimeError. Otherwise, return a successful record destructure.
            opt_erroneous.unwrap_or(Pattern::RecordDestructure {
                whole_var,
                ext_var,
                destructs,
            })
        }

        RequiredField(_name, _loc_pattern) => {
            unreachable!("should have been handled in RecordDestructure");
        }
        OptionalField(_name, _loc_pattern) => {
            unreachable!("should have been handled in RecordDestructure");
        }

        Malformed(_str) => {
            let problem = MalformedPatternProblem::Unknown;
            malformed_pattern(env, problem, region)
        }

        MalformedIdent(_str, problem) => {
            let problem = MalformedPatternProblem::BadIdent(*problem);
            malformed_pattern(env, problem, region)
        }

        QualifiedIdentifier { .. } => {
            let problem = MalformedPatternProblem::QualifiedIdentifier;
            malformed_pattern(env, problem, region)
        }
    };

    (
        output,
        Loc {
            region,
            value: can_pattern,
        },
    )
}

/// When we detect an unsupported pattern type (e.g. 5 = 1 + 2 is unsupported because you can't
/// assign to Int patterns), report it to Env and return an UnsupportedPattern runtime error pattern.
fn unsupported_pattern(env: &mut Env, pattern_type: PatternType, region: Region) -> Pattern {
    use roc_problem::can::BadPattern;
    env.problem(Problem::UnsupportedPattern(
        BadPattern::Unsupported(pattern_type),
        region,
    ));

    Pattern::UnsupportedPattern(region)
}

fn bad_underscore(env: &mut Env, region: Region) -> Pattern {
    use roc_problem::can::BadPattern;
    env.problem(Problem::UnsupportedPattern(
        BadPattern::UnderscoreInDef,
        region,
    ));

    Pattern::UnsupportedPattern(region)
}

/// When we detect a malformed pattern like `3.X` or `0b5`,
/// report it to Env and return an UnsupportedPattern runtime error pattern.
fn malformed_pattern(env: &mut Env, problem: MalformedPatternProblem, region: Region) -> Pattern {
    env.problem(Problem::RuntimeError(RuntimeError::MalformedPattern(
        problem, region,
    )));

    Pattern::MalformedPattern(problem, region)
}

pub fn bindings_from_patterns<'a, I>(loc_patterns: I) -> Vec<(Symbol, Region)>
where
    I: Iterator<Item = &'a Loc<Pattern>>,
{
    let mut answer = Vec::new();

    for loc_pattern in loc_patterns {
        add_bindings_from_patterns(&loc_pattern.region, &loc_pattern.value, &mut answer);
    }

    answer
}

/// helper function for idents_from_patterns
fn add_bindings_from_patterns(
    region: &Region,
    pattern: &Pattern,
    answer: &mut Vec<(Symbol, Region)>,
) {
    use Pattern::*;

    match pattern {
        Identifier(symbol)
        | Shadowed(_, _, symbol)
        | AbilityMemberSpecialization {
            ident: symbol,
            specializes: _,
        } => {
            answer.push((*symbol, *region));
        }
        AppliedTag {
            arguments: loc_args,
            ..
        } => {
            for (_, loc_arg) in loc_args {
                add_bindings_from_patterns(&loc_arg.region, &loc_arg.value, answer);
            }
        }
        UnwrappedOpaque {
            argument, opaque, ..
        } => {
            let (_, loc_arg) = &**argument;
            add_bindings_from_patterns(&loc_arg.region, &loc_arg.value, answer);
            answer.push((*opaque, *region));
        }
        RecordDestructure { destructs, .. } => {
            for Loc {
                region,
                value: RecordDestruct { symbol, .. },
            } in destructs
            {
                answer.push((*symbol, *region));
            }
        }
        NumLiteral(..)
        | IntLiteral(..)
        | FloatLiteral(..)
        | StrLiteral(_)
        | SingleQuote(_)
        | Underscore
        | MalformedPattern(_, _)
        | UnsupportedPattern(_)
        | OpaqueNotInScope(..) => (),
    }
}

fn flatten_str_literal(literal: &StrLiteral<'_>) -> Pattern {
    use ast::StrLiteral::*;

    match literal {
        PlainLine(str_slice) => Pattern::StrLiteral((*str_slice).into()),
        Line(segments) => flatten_str_lines(&[segments]),
        Block(lines) => flatten_str_lines(lines),
    }
}

fn flatten_str_lines(lines: &[&[StrSegment<'_>]]) -> Pattern {
    use StrSegment::*;

    let mut buf = String::new();

    for line in lines {
        for segment in line.iter() {
            match segment {
                Plaintext(string) => {
                    buf.push_str(string);
                }
                Unicode(loc_digits) => {
                    todo!("parse unicode digits {:?}", loc_digits);
                }
                Interpolated(loc_expr) => {
                    return Pattern::UnsupportedPattern(loc_expr.region);
                }
                EscapedChar(escaped) => buf.push(unescape_char(escaped)),
            }
        }
    }

    Pattern::StrLiteral(buf.into())
}
