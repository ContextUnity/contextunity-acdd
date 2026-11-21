pub const DEFAULT_FACTS_PER_SCOPE: usize = 65_536;

/// Canonical Python literal constructions identify intrinsic types even when
/// their ordinary constructor names are shadowed by local declarations.
const PYTHON_LITERAL_CONSTRUCTORS: [(&str, &str); 4] = [
    ("{}", "dict"),
    ("[]", "list"),
    ("{...}", "set"),
    ("()", "tuple"),
];

pub fn python_literal_type(callee: &str) -> Option<&'static str> {
    PYTHON_LITERAL_CONSTRUCTORS
        .iter()
        .find_map(|(literal, builtin)| (*literal == callee).then_some(*builtin))
}

pub fn python_literal_constructor(builtin: &str) -> Option<&'static str> {
    PYTHON_LITERAL_CONSTRUCTORS
        .iter()
        .find_map(|(literal, name)| (*name == builtin).then_some(*literal))
}
