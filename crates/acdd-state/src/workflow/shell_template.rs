//! Canonical POSIX shell template validation, quoting, and placeholder expansion.

use super::schema::TestFilterConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::path_policy::glob_matches;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Glob specificity ordering: `(literal_segments, single_wildcard_segments, !globstar_count, literal_len)`.
pub fn glob_specificity(pattern: &str) -> (usize, usize, usize, usize) {
    let norm = pattern.trim().replace('\\', "/");
    let mut literal_segs = 0usize;
    let mut single_wild_segs = 0usize;
    let mut globstar_segs = 0usize;
    let mut literal_chars = 0usize;
    for seg in norm.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if seg == "**" {
            globstar_segs += 1;
        } else if seg.contains('*') || seg.contains('?') {
            single_wild_segs += 1;
        } else {
            literal_segs += 1;
            literal_chars += seg.len();
        }
    }
    (
        literal_segs,
        single_wild_segs,
        usize::MAX - globstar_segs,
        literal_chars,
    )
}

/// POSIX single-quote escape (`'` -> `'\''`) wrapped in single quotes.
fn posix_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellQuoteState {
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

#[derive(Debug, Default)]
struct ShellScan<'a> {
    unbalanced_quotes: bool,
    trailing_escape: bool,
    has_chain_op: bool,
    ends_in_comment: bool,
    bad_placeholder: Option<(&'a str, &'static str)>,
    active_placeholders: Vec<(&'a str, usize)>,
}

fn is_shell_comment_start(bytes: &[u8], i: usize) -> bool {
    bytes[i] == b'#'
        && (i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b';' | b'&' | b'|' | b'('
            ))
}

fn scan_shell_template<'a>(template: &str, placeholders: &[&'a str]) -> ShellScan<'a> {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    let mut state = ShellQuoteState::Unquoted;
    let mut escaped = false;
    let mut out = ShellScan::default();

    while i < bytes.len() {
        if state == ShellQuoteState::Unquoted && !escaped && is_shell_comment_start(bytes, i) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i == bytes.len() {
                out.ends_in_comment = true;
                break;
            }
            i += 1;
            continue;
        }

        if bytes[i] == b'{' {
            if let Some(ph) = placeholders
                .iter()
                .copied()
                .find(|ph| template[i..].starts_with(ph))
            {
                if escaped {
                    out.bad_placeholder
                        .get_or_insert((ph, "must not be backslash-escaped"));
                } else {
                    match state {
                        ShellQuoteState::Unquoted => {
                            out.active_placeholders.push((ph, i));
                            i += ph.len();
                            continue;
                        }
                        ShellQuoteState::SingleQuoted => {
                            out.bad_placeholder
                                .get_or_insert((ph, "must not appear inside single quotes"));
                        }
                        ShellQuoteState::DoubleQuoted => {
                            out.bad_placeholder
                                .get_or_insert((ph, "must not appear inside double quotes"));
                        }
                    }
                }
            }
        }

        let b = bytes[i];
        match state {
            ShellQuoteState::SingleQuoted => {
                if b == b'\'' {
                    state = ShellQuoteState::Unquoted;
                }
            }
            ShellQuoteState::DoubleQuoted => {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    state = ShellQuoteState::Unquoted;
                }
            }
            ShellQuoteState::Unquoted => {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else {
                    match b {
                        b'\'' => state = ShellQuoteState::SingleQuoted,
                        b'"' => state = ShellQuoteState::DoubleQuoted,
                        b';' | b'|' => out.has_chain_op = true,
                        b'&' if bytes.get(i + 1) == Some(&b'&') => out.has_chain_op = true,
                        _ => {}
                    }
                }
            }
        }
        i += 1;
    }

    out.unbalanced_quotes = state != ShellQuoteState::Unquoted;
    out.trailing_escape = escaped;
    out
}

fn check_scan_safety(scan: &ShellScan<'_>, ctx: &str) -> Result<(), AcddError> {
    if let Some((ph, reason)) = scan.bad_placeholder {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: shell placeholder '{ph}' {reason}"),
        ));
    }
    if scan.unbalanced_quotes {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: shell template has unbalanced quotes"),
        ));
    }
    if scan.trailing_escape {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: shell template ends with a trailing escape '\\'"),
        ));
    }
    Ok(())
}

/// Boundary-safe placeholder substitution used across all shell placeholders.
///
/// When `replacement` is empty, removes each placeholder occurrence plus at most one adjacent
/// space so intentional multi-space literals elsewhere in the template are preserved untouched.
fn substitute_active_placeholder(
    template: &str,
    placeholder: &str,
    positions: &[usize],
    replacement: &str,
) -> String {
    if positions.is_empty() {
        return template.to_owned();
    }
    let ph_len = placeholder.len();
    if replacement.is_empty() {
        let bytes = template.as_bytes();
        let mut out = String::with_capacity(template.len());
        let mut cursor = 0usize;
        for &pos in positions {
            let end = pos + ph_len;
            let left_is_sep = pos == cursor || bytes[pos - 1].is_ascii_whitespace();
            let right_is_sep = end == bytes.len() || bytes[end].is_ascii_whitespace();
            let mut slice_start = pos;
            let mut slice_end = end;
            if right_is_sep && slice_start > cursor && bytes[slice_start - 1] == b' ' {
                slice_start -= 1;
            } else if left_is_sep && slice_end < bytes.len() && bytes[slice_end] == b' ' {
                slice_end += 1;
            }
            out.push_str(&template[cursor..slice_start]);
            cursor = slice_end;
        }
        out.push_str(&template[cursor..]);
        out
    } else {
        let mut out = String::with_capacity(template.len() + replacement.len() * positions.len());
        let mut cursor = 0usize;
        for &pos in positions {
            out.push_str(&template[cursor..pos]);
            out.push_str(replacement);
            cursor = pos + ph_len;
        }
        out.push_str(&template[cursor..]);
        out
    }
}

/// Return `true` if `cmd` terminates inside an unquoted POSIX `#` comment.
fn ends_inside_shell_comment(cmd: &str) -> bool {
    scan_shell_template(cmd.trim_end(), &[]).ends_in_comment
}

/// Validate balanced POSIX shell quotes and ensure every substitution `placeholder`
/// occurs strictly in `Unquoted`, non-escaped shell position before rendering.
pub fn validate_shell_template_placeholders(
    template: &str,
    placeholders: &[&str],
    ctx: &str,
) -> Result<(), AcddError> {
    let scan = scan_shell_template(template, placeholders);
    check_scan_safety(&scan, ctx)
}

/// Render a command template with optional `--test <test_target>` substitution.
pub fn render_command_with_test_filter(
    command_template: &str,
    test_filter: Option<&TestFilterConfig>,
    test_target: Option<&str>,
) -> Result<String, AcddError> {
    let scan = scan_shell_template(command_template, &["{scope_target}", "{test_filter}"]);
    check_scan_safety(&scan, "command template")?;
    let tf_positions: Vec<usize> = scan
        .active_placeholders
        .iter()
        .filter(|&&(ph, _)| ph == "{test_filter}")
        .map(|&(_, pos)| pos)
        .collect();
    if tf_positions.len() > 1 {
        return Err(AcddError::new(
            ErrorCode::CommandTestFilterUnsupported,
            format!(
                "command template contains {} '{{test_filter}}' placeholders; at most 1 is allowed",
                tf_positions.len()
            ),
        ));
    }

    let tpl_pos = match test_filter {
        Some(TestFilterConfig::Template(tpl)) => {
            let tpl_scan = scan_shell_template(tpl, &["{test}"]);
            check_scan_safety(&tpl_scan, "test_filter template")?;
            if tpl.trim().is_empty() || tpl_scan.active_placeholders.len() != 1 {
                return Err(AcddError::new(
                    ErrorCode::CommandTestFilterUnsupported,
                    "test_filter template must contain '{test}' exactly once",
                ));
            }
            Some(tpl_scan.active_placeholders[0].1)
        }
        _ => None,
    };

    match test_target {
        None => Ok(substitute_active_placeholder(
            command_template,
            "{test_filter}",
            &tf_positions,
            "",
        )),
        Some(target) => {
            if target.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::CommandTestFilterUnsupported,
                    "--test target cannot be empty",
                ));
            }
            if matches!(test_filter, Some(TestFilterConfig::Enabled(false))) {
                return Err(AcddError::new(
                    ErrorCode::CommandTestFilterUnsupported,
                    "this command disables '--test' filtering (test_filter: false)",
                ));
            }
            let quoted = posix_single_quote(target);
            let rendered_filter = match (test_filter, tpl_pos) {
                (None | Some(TestFilterConfig::Enabled(true)), _) => quoted,
                (Some(TestFilterConfig::Template(tpl)), Some(pos)) => {
                    substitute_active_placeholder(tpl, "{test}", &[pos], &quoted)
                }
                (Some(TestFilterConfig::Template(_)), None)
                | (Some(TestFilterConfig::Enabled(false)), _) => unreachable!(),
            };
            if !tf_positions.is_empty() {
                Ok(substitute_active_placeholder(
                    command_template,
                    "{test_filter}",
                    &tf_positions,
                    &rendered_filter,
                ))
            } else if scan.has_chain_op {
                Err(AcddError::new(
                    ErrorCode::CommandTestFilterPlaceholderRequired,
                    format!(
                        "chained command '{command_template}' requires '{{test_filter}}' when '--test' is passed"
                    ),
                ))
            } else if ends_inside_shell_comment(command_template) {
                Err(AcddError::new(
                    ErrorCode::CommandTestFilterPlaceholderRequired,
                    format!(
                        "commented command '{command_template}' requires '{{test_filter}}' before '#' when '--test' is passed"
                    ),
                ))
            } else {
                Ok(format!("{} {rendered_filter}", command_template.trim_end()))
            }
        }
    }
}

struct ValidatedScopeArg<'a> {
    pattern: &'a str,
    arg: &'a str,
    placeholders: Vec<(&'static str, usize)>,
}

fn expand_matched_scope_arg(path: &str, entry: &ValidatedScopeArg<'_>) -> Option<String> {
    let trimmed = entry.arg.trim();
    if trimmed.is_empty() {
        return None;
    }
    if entry.placeholders.is_empty() {
        return Some(trimmed.to_owned());
    }
    if path.contains('*') || path.contains('?') || path.contains('[') || path.contains('{') {
        return None;
    }
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if entry
        .placeholders
        .iter()
        .any(|&(ph, _)| ph == "{file_stem}")
        && stem.is_empty()
    {
        return None;
    }
    let lead_ws = entry.arg.len() - entry.arg.trim_start().len();
    let quoted_stem = posix_single_quote(stem);
    let quoted_path = posix_single_quote(path);
    let mut expanded = String::with_capacity(trimmed.len() + quoted_path.len());
    let mut cursor = 0usize;
    for &(ph, abs_pos) in &entry.placeholders {
        let pos = abs_pos - lead_ws;
        expanded.push_str(&trimmed[cursor..pos]);
        match ph {
            "{file_stem}" => expanded.push_str(&quoted_stem),
            "{path}" => expanded.push_str(&quoted_path),
            _ => {}
        }
        cursor = pos + ph.len();
    }
    expanded.push_str(&trimmed[cursor..]);
    (!expanded.is_empty()).then_some(expanded)
}

fn scope_entry_matches_path(entry: &ValidatedScopeArg<'_>, path: &str) -> bool {
    if entry.pattern == path || glob_matches(entry.pattern, path) {
        return true;
    }
    if !entry.placeholders.is_empty() {
        return false;
    }
    if path.ends_with('/') {
        return glob_matches(entry.pattern, &format!("{path}__scope__.rs"));
    }
    if let Some(prefix) = path.strip_suffix("/**") {
        return glob_matches(entry.pattern, &format!("{prefix}/__scope__.rs"));
    }
    !path.contains(['*', '?', '[', '{'])
        && Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.contains('.'))
        && glob_matches(entry.pattern, &format!("{path}/__scope__.rs"))
}

fn select_scope_args(validated_args: &[ValidatedScopeArg<'_>], paths: &[String]) -> Vec<String> {
    if validated_args.is_empty() {
        return Vec::new();
    }
    type Specificity = (usize, usize, usize, usize);
    let mut selected_args: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        let mut best: Option<(Specificity, &ValidatedScopeArg<'_>)> = None;
        for entry in validated_args {
            if scope_entry_matches_path(entry, path) {
                let spec = glob_specificity(entry.pattern);
                if best.is_none_or(|(best_spec, _)| spec > best_spec) {
                    best = Some((spec, entry));
                }
            }
        }
        if let Some((_, entry)) = best {
            if let Some(arg) = expand_matched_scope_arg(path, entry) {
                if seen.insert(arg.clone()) {
                    selected_args.push(arg);
                }
            }
        }
    }
    selected_args
}

/// Render `command` by resolving `scope_args_by_pattern` against `paths` and substituting every
/// active `{scope_target}` placeholder using boundary-safe replacement.
///
/// Multiple unique matched targets produce independent rendered commands in deterministic order.
pub fn render_command_with_scope_target(
    command: &str,
    scope_args_by_pattern: &BTreeMap<String, String>,
    paths: &[String],
) -> Result<Vec<String>, AcddError> {
    let cmd_scan = scan_shell_template(command, &["{scope_target}", "{test_filter}"]);
    check_scan_safety(&cmd_scan, "command template")?;

    let mut validated_args: Vec<ValidatedScopeArg<'_>> = Vec::new();
    for (pattern, arg) in scope_args_by_pattern {
        let arg_scan = scan_shell_template(arg, &["{file_stem}", "{path}"]);
        check_scan_safety(&arg_scan, &format!("scope_args_by_pattern[{pattern}]"))?;
        validated_args.push(ValidatedScopeArg {
            pattern: pattern.as_str(),
            arg: arg.as_str(),
            placeholders: arg_scan.active_placeholders,
        });
    }

    let selected_args = select_scope_args(&validated_args, paths);

    let scope_positions: Vec<usize> = cmd_scan
        .active_placeholders
        .iter()
        .filter(|&&(ph, _)| ph == "{scope_target}")
        .map(|&(_, pos)| pos)
        .collect();
    if selected_args.is_empty() {
        return Ok(vec![substitute_active_placeholder(
            command,
            "{scope_target}",
            &scope_positions,
            "",
        )]);
    }

    if !scope_positions.is_empty() {
        return Ok(selected_args
            .into_iter()
            .map(|arg| {
                substitute_active_placeholder(command, "{scope_target}", &scope_positions, &arg)
            })
            .collect());
    }

    if cmd_scan.has_chain_op {
        return Ok(vec![command.to_owned()]);
    }

    let tf_positions: Vec<usize> = cmd_scan
        .active_placeholders
        .iter()
        .filter(|&&(ph, _)| ph == "{test_filter}")
        .map(|&(_, pos)| pos)
        .collect();
    if tf_positions.len() == 1 {
        let (before, after) = command.split_at(tf_positions[0]);
        Ok(selected_args
            .into_iter()
            .map(|arg| format!("{before}{arg} {after}"))
            .collect())
    } else if ends_inside_shell_comment(command) {
        Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "commented command '{command}' requires '{{scope_target}}' before '#' when scope_args_by_pattern matches"
            ),
        ))
    } else {
        let base = command.trim_end();
        Ok(selected_args
            .into_iter()
            .map(|arg| format!("{base} {arg}"))
            .collect())
    }
}

impl<'de> serde::Deserialize<'de> for super::schema::CommandDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use super::schema::{DetailedCommandDef, PatternCommandEntry};
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct UnboundRepr {
            unbound: bool,
            #[serde(default)]
            description: Option<String>,
        }

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PatternMapBlock {
            by_pattern: ByPatternPairs,
            #[serde(default)]
            post_command: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            timeout_seconds: Option<u64>,
            #[serde(default)]
            test_filter: Option<TestFilterConfig>,
            #[serde(default)]
            scope_args_by_pattern: BTreeMap<String, String>,
        }

        struct ByPatternPairs(Vec<(String, String)>);

        impl<'de> serde::Deserialize<'de> for ByPatternPairs {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct PairsVisitor;
                impl<'de> Visitor<'de> for PairsVisitor {
                    type Value = ByPatternPairs;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("an ordered mapping of glob pattern to command string")
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        let mut out = Vec::new();
                        while let Some((k, v)) = map.next_entry::<String, String>()? {
                            out.push((k, v));
                        }
                        Ok(ByPatternPairs(out))
                    }
                }
                deserializer.deserialize_map(PairsVisitor)
            }
        }

        let val = serde_yaml::Value::deserialize(deserializer)?;
        match &val {
            serde_yaml::Value::String(s) => Ok(Self::Script(s.clone())),
            serde_yaml::Value::Sequence(_) => {
                let entries: Vec<PatternCommandEntry> =
                    serde_yaml::from_value(val).map_err(de::Error::custom)?;
                Ok(Self::Pattern(entries))
            }
            serde_yaml::Value::Mapping(map) => {
                let has_unbound = map.contains_key(serde_yaml::Value::String("unbound".into()));
                let has_by_pattern =
                    map.contains_key(serde_yaml::Value::String("by_pattern".into()));
                if has_unbound {
                    let u: UnboundRepr = serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    if !u.unbound {
                        return Err(de::Error::custom(
                            "unbound command slot must set 'unbound: true' or provide a command",
                        ));
                    }
                    Ok(Self::Unbound {
                        unbound: u.unbound,
                        description: u.description,
                    })
                } else if has_by_pattern {
                    let block: PatternMapBlock =
                        serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    let mut entries: Vec<PatternCommandEntry> = block
                        .by_pattern
                        .0
                        .into_iter()
                        .map(|(pattern, command)| PatternCommandEntry {
                            pattern,
                            command,
                            description: block.description.clone(),
                            timeout_seconds: block.timeout_seconds,
                            test_filter: block.test_filter.clone(),
                            scope_args_by_pattern: block.scope_args_by_pattern.clone(),
                            post_always: false,
                        })
                        .collect();
                    if let Some(post_cmd) = block.post_command {
                        if !post_cmd.trim().is_empty() {
                            entries.push(PatternCommandEntry {
                                pattern: "**".to_owned(),
                                command: post_cmd,
                                description: block.description.clone(),
                                timeout_seconds: block.timeout_seconds,
                                test_filter: None,
                                scope_args_by_pattern: BTreeMap::new(),
                                post_always: true,
                            });
                        }
                    }
                    Ok(Self::Pattern(entries))
                } else {
                    let detailed: DetailedCommandDef =
                        serde_yaml::from_value(val).map_err(de::Error::custom)?;
                    Ok(Self::Detailed(Box::new(detailed)))
                }
            }
            other => Err(de::Error::custom(format!(
                "invalid command definition: {other:?}"
            ))),
        }
    }
}

/// Resolve all `guidance` items on a `GateDef` against `profile.guidance`, enforcing the mandatory budget cap.
pub fn resolve_and_budget_guidance(
    profile: &super::schema::ResolvedWorkflow,
    gate: &super::schema::GateDef,
    contract_bytes: usize,
    response_max_bytes: usize,
) -> Result<Vec<String>, AcddError> {
    use super::schema::GateGuidanceRef;
    let mut rendered = Vec::with_capacity(gate.guidance.len());
    let mut guidance_bytes = 0usize;
    for item in &gate.guidance {
        let text = match item {
            GateGuidanceRef::Slot(slot) => profile
                .guidance
                .get(slot)
                .ok_or_else(|| {
                    AcddError::new(
                        ErrorCode::UnknownGuidanceSlot,
                        format!("gate '{}': unknown guidance slot '{slot}'", gate.id),
                    )
                })?
                .render(),
            GateGuidanceRef::Inline(inline) => inline.inline.clone(),
        };
        guidance_bytes += text.len();
        rendered.push(text);
    }
    check_guidance_budget(
        contract_bytes,
        guidance_bytes,
        &profile.limits,
        response_max_bytes,
    )?;
    Ok(rendered)
}

/// Enforce the fail-closed mandatory `contract + guidance` byte reservation cap.
pub fn check_guidance_budget(
    contract_bytes: usize,
    guidance_bytes: usize,
    limits: &super::schema::ResourceLimits,
    response_max_bytes: usize,
) -> Result<usize, AcddError> {
    let total = contract_bytes.saturating_add(guidance_bytes);
    let cap = limits.max_guidance_bytes.min(response_max_bytes);
    if total > cap {
        return Err(AcddError::new(
            ErrorCode::GuidanceBudgetExceeded,
            format!(
                "mandatory contract + guidance ({total} bytes) exceeds reserved budget ({cap} bytes)"
            ),
        )
        .with_hint("Reduce inline guidance or task spec/invariants size so mandatory instructions fit without truncation"));
    }
    Ok(cap - total)
}
