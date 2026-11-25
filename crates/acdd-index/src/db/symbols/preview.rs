use crate::db::source::verified_source;
use crate::models::tags::NodeKind;
use acdd_core::response::{QueryOptions, SourceOptions};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{cell::RefCell, collections::BTreeMap, path::Path};

#[derive(Deserialize, Serialize)]
struct SymbolWithSourceDto {
    #[serde(default)]
    source: String,
    #[serde(default)]
    source_preview: Value,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

pub(super) struct IndexedTextMatch {
    pub(super) line: usize,
    pub(super) snippet: String,
    pub(super) preview: Option<(String, usize, usize)>,
}

thread_local! {
    static LINE_FTS: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn with_line_fts<T>(f: impl FnOnce(&mut Connection) -> Option<T>) -> Option<T> {
    LINE_FTS.with(|slot| {
        let mut guard = slot.borrow_mut();
        if guard.is_none() {
            let conn = Connection::open_in_memory().ok()?;
            conn.execute_batch(
                "CREATE VIRTUAL TABLE _line USING fts5(body, tokenize='unicode61 separators ''._-/''');",
            )
            .ok()?;
            *guard = Some(conn);
        }
        let conn = guard.as_mut()?;
        let result = f(conn);
        let _ = conn.execute("DELETE FROM _line", []);
        result
    })
}

pub(super) fn resolve_indexed_text_match(
    text: &str,
    start_line: usize,
    end_line: usize,
    pattern: &str,
    max_bytes: usize,
) -> Option<IndexedTextMatch> {
    const SNIPPET_CHAR_LIMIT: usize = 180;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let lo = start_line.max(1).min(lines.len());
    let hi = end_line.max(lo).min(lines.len());
    let trimmed_pattern = pattern.trim();
    let is_natural_query =
        !trimmed_pattern.starts_with('@') && trimmed_pattern.chars().any(char::is_whitespace);
    let tokens: Vec<String> = trimmed_pattern
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if tokens.is_empty() {
        return None;
    }

    let quoted: Vec<String> = tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    let and_query = quoted.join(" AND ");
    let or_query = quoted.join(" OR ");

    // Delegate line matching to a reusable thread-local SQLite FTS5 handle with identical
    // `unicode61 separators '._-/'` tokenization so diacritics, Unicode case folding, and
    // separator boundaries match `node_search` identically without per-hit DDL.
    let matched_line = with_line_fts(|fts| {
        fts.execute("DELETE FROM _line", []).ok()?;
        let tx = fts.transaction().ok()?;
        {
            let mut insert = tx
                .prepare_cached("INSERT INTO _line(rowid, body) VALUES (?1, ?2)")
                .ok()?;
            for ln in lo..=hi {
                let _ = insert.execute(rusqlite::params![ln as i64, lines[ln - 1]]);
            }
        }
        tx.commit().ok()?;
        let mut query_stmt = fts
            .prepare_cached("SELECT rowid FROM _line WHERE _line MATCH ?1 ORDER BY rowid LIMIT 1")
            .ok()?;
        let matched = query_stmt
            .query_row([and_query.as_str()], |row| row.get::<_, usize>(0))
            .ok();
        matched.or_else(|| {
            (is_natural_query && tokens.len() > 1)
                .then(|| {
                    query_stmt
                        .query_row([or_query.as_str()], |row| row.get::<_, usize>(0))
                        .ok()
                })
                .flatten()
        })
    })?;

    let raw_line = lines[matched_line - 1].trim();
    let mut snippet: String = raw_line.chars().take(SNIPPET_CHAR_LIMIT).collect();
    if raw_line.chars().count() > SNIPPET_CHAR_LIMIT {
        snippet.push('…');
    }

    let preview = build_centered_preview(&lines, matched_line, max_bytes);
    Some(IndexedTextMatch {
        line: matched_line,
        snippet,
        preview,
    })
}

fn build_centered_preview(
    lines: &[&str],
    matched_line: usize,
    max_bytes: usize,
) -> Option<(String, usize, usize)> {
    let center = format!("{matched_line}: {}", lines[matched_line - 1]);
    let center_bytes = center.len() + usize::from(!center.ends_with('\n'));
    if center_bytes > max_bytes {
        return None;
    }

    let mut total_bytes = center_bytes;
    let mut win_start = matched_line;
    let mut win_end = matched_line;
    let min_start = matched_line.saturating_sub(2).max(1);
    let max_end = (matched_line + 4).min(lines.len());

    let try_expand = |edge: &mut usize, candidate_ln: usize, total_bytes: &mut usize| -> bool {
        let entry = format!("{candidate_ln}: {}", lines[candidate_ln - 1]);
        let extra = entry.len() + usize::from(!entry.ends_with('\n'));
        if *total_bytes + extra <= max_bytes {
            *edge = candidate_ln;
            *total_bytes += extra;
            true
        } else {
            false
        }
    };

    loop {
        let mut expanded = false;
        if win_start > min_start {
            let prev = win_start - 1;
            expanded |= try_expand(&mut win_start, prev, &mut total_bytes);
        }
        if win_end < max_end {
            let next = win_end + 1;
            expanded |= try_expand(&mut win_end, next, &mut total_bytes);
        }
        if !expanded {
            break;
        }
    }

    let mut preview = String::with_capacity(total_bytes);
    for ln in win_start..=win_end {
        preview.push_str(&format!("{ln}: {}", lines[ln - 1]));
        if !preview.ends_with('\n') {
            preview.push('\n');
        }
    }
    Some((preview, win_start, win_end))
}

pub(super) fn first_docstring_sentence(docstring: &str) -> Option<String> {
    const MAX_CHARS: usize = 240;
    let literal = docstring.trim();
    let prefix_len = literal
        .char_indices()
        .take_while(|(_, character)| {
            matches!(character, 'r' | 'R' | 'u' | 'U' | 'b' | 'B' | 'f' | 'F')
        })
        .take(3)
        .last()
        .map_or(0, |(index, character)| index + character.len_utf8());
    let content = (prefix_len > 0)
        .then(|| strip_string_literal_delimiters(&literal[prefix_len..]))
        .flatten()
        .or_else(|| strip_string_literal_delimiters(literal))
        .unwrap_or(literal);
    let sentence = content.trim().lines().next()?.trim();
    if sentence.is_empty() {
        return None;
    }
    let end = sentence
        .char_indices()
        .find_map(|(index, character)| {
            (matches!(character, '.' | '!' | '?')
                && sentence[index + character.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace))
            .then_some(index + character.len_utf8())
        })
        .unwrap_or(sentence.len());
    let sentence = sentence.get(..end)?;
    let mut bounded = sentence.chars().take(MAX_CHARS).collect::<String>();
    if sentence.chars().count() > MAX_CHARS {
        bounded.push('…');
    }
    Some(bounded)
}

fn strip_string_literal_delimiters(literal: &str) -> Option<&str> {
    ["\"\"\"", "'''", "\"", "'"].iter().find_map(|delimiter| {
        literal
            .strip_prefix(delimiter)
            .and_then(|body| body.strip_suffix(delimiter))
    })
}

#[derive(Clone, Copy)]
pub(super) enum SourcePreviewMode {
    Configured,
    CompleteSymbol { response_bytes: usize },
}

pub(super) fn with_source(
    conn: &Connection,
    root: &Path,
    result: Value,
    source: &SourceOptions,
    options: &QueryOptions,
    mode: SourcePreviewMode,
) -> Result<Value> {
    anyhow::ensure!(
        source.offset == 0 || options.generation.is_some(),
        "source continuation requires generation from the previous page; pass the previous page's generation or restart at source_offset=0"
    );
    if !source.enabled {
        return Ok(result);
    }
    anyhow::ensure!(
        source.leading_lines <= 20 && source.max_body_lines > 0,
        "invalid source preview bounds"
    );
    let source_bytes = match mode {
        SourcePreviewMode::Configured => {
            anyhow::ensure!(
                source.max_body_lines <= 500,
                "invalid source preview bounds"
            );
            8192
        }
        SourcePreviewMode::CompleteSymbol { response_bytes } => response_bytes,
    };
    let node = &result["node"];
    let path = node["path"].as_str().context("invalid node path")?;
    let text = verified_source(conn, root, path)?;
    let (snippet, preview) = source_preview(conn, node, &text, source, source_bytes)?;
    let mut dto: SymbolWithSourceDto = serde_json::from_value(result)?;
    dto.source = snippet;
    dto.source_preview = preview;
    Ok(serde_json::to_value(dto)?)
}

pub(super) fn source_preview(
    conn: &Connection,
    node: &Value,
    text: &str,
    options: &SourceOptions,
    source_bytes: usize,
) -> Result<(String, Value)> {
    let path = node["path"].as_str().context("invalid node path")?;
    let start = node["line"].as_u64().context("invalid start line")? as usize;
    let end = node["end_line"].as_u64().context("invalid end line")? as usize;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    anyhow::ensure!(
        start > 0 && end >= start && end <= lines.len(),
        "node has no valid source range"
    );
    let line_start: usize = lines[..start - 1].iter().map(|line| line.len()).sum();
    let line_end: usize = line_start
        + lines[start - 1..end]
            .iter()
            .map(|line| line.len())
            .sum::<usize>();
    let mut range = symbol_range(node, text)?.unwrap_or(line_start..line_end);
    anyhow::ensure!(
        range.start >= line_start && range.end <= line_end,
        "source boundary differs from indexed symbol"
    );
    let starts_on_clean_line = text[line_start..range.start].trim().is_empty();
    if text[line_start..range.start].trim().is_empty() {
        range.start = line_start;
    }
    if text[range.end..line_end].trim().is_empty() {
        range.end = line_end;
    }
    let body: Vec<_> = text[range].split_inclusive('\n').collect();
    anyhow::ensure!(
        options.offset <= body.len(),
        "source_offset exceeds symbol body"
    );
    let previous: usize = conn.query_row(
        &format!(
            "SELECT coalesce(max(n.end_line),0) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.end_line<?2 AND n.kind NOT IN('file','{}','{}')",
            NodeKind::Module.as_str(),
            NodeKind::Component.as_str(),
        ),
        rusqlite::params![path, start],
        |r| r.get(0),
    )?;
    let leading_start = (start - 1)
        .saturating_sub(options.leading_lines)
        .max(previous);
    let available_leading = if options.offset == 0 && starts_on_clean_line {
        start - 1 - leading_start
    } else {
        0
    };
    let mut leading = String::new();
    let mut leading_count = 0;
    if options.offset == 0 && starts_on_clean_line {
        for line in lines[leading_start..start - 1].iter().rev() {
            if escaped_cost(line) + escaped_cost(&leading) > source_bytes / 4 {
                break;
            }
            leading.insert_str(0, line);
            leading_count += 1;
        }
    }
    let mut output = leading;
    let mut cost = escaped_cost(&output);
    let mut emitted = 0;
    for line in body
        .iter()
        .skip(options.offset)
        .take(options.max_body_lines)
    {
        let line_cost = escaped_cost(line);
        if emitted == 0 && cost.saturating_add(line_cost) > source_bytes.saturating_sub(2) {
            output.clear();
            leading_count = 0;
            cost = 0;
        }
        if cost.saturating_add(line_cost) > source_bytes.saturating_sub(2) {
            anyhow::ensure!(
                emitted > 0,
                "snippet line exceeds source preview byte budget at {path}:{}; use a larger response budget or read the local file",
                start.checked_add(options.offset).context("source line overflow")?
            );
            break;
        }
        output.push_str(line);
        cost += line_cost;
        emitted += 1;
    }
    let next = options
        .offset
        .checked_add(emitted)
        .context("source continuation offset overflow")?;
    let has_more = next < body.len();
    let hint = if has_more {
        Some(
            "Continue with show_source=true, source_offset=next_source_offset, and generation."
                .to_owned(),
        )
    } else {
        None
    };
    let symbol_line = start
        .checked_add(options.offset)
        .context("source start line overflow")?;
    let source_start_line = symbol_line
        .checked_sub(leading_count)
        .context("source leading line overflow")?;
    let end_line = if output.is_empty() {
        None
    } else if emitted == 0 {
        Some(symbol_line)
    } else {
        Some(
            start
                .checked_add(next - 1)
                .context("source end line overflow")?,
        )
    };
    let start_line = (!output.is_empty()).then_some(source_start_line);
    let mut preview = serde_json::Map::new();
    preview.insert("start_line".into(), json!(start_line));
    preview.insert("end_line".into(), json!(end_line));
    preview.insert("total_body_lines".into(), json!(body.len()));
    preview.insert("body_offset".into(), json!(options.offset));
    preview.insert("body_lines".into(), json!(emitted));
    preview.insert("leading_lines".into(), json!(leading_count));
    if has_more {
        preview.insert("next_source_offset".into(), json!(next));
    }
    preview.insert("has_more".into(), json!(has_more));
    let omitted_leading = available_leading.saturating_sub(leading_count);
    if omitted_leading > 0 {
        preview.insert("omitted_leading_lines".into(), json!(omitted_leading));
    }
    preview.insert(
        "omitted_body_lines".into(),
        json!(body.len().saturating_sub(next)),
    );
    preview.insert("truncated_line".into(), json!(false));
    if let Some(hint) = hint {
        preview.insert("continuation_hint".into(), json!(hint));
    }
    Ok((output, Value::Object(preview)))
}

fn escaped_char_cost(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
        ch if ch <= '\u{1f}' => 6,
        ch => ch.len_utf8(),
    }
}

fn escaped_cost(text: &str) -> usize {
    text.chars().map(escaped_char_cost).sum()
}

fn symbol_range(node: &Value, text: &str) -> Result<Option<std::ops::Range<usize>>> {
    let kind = node["kind"].as_str().unwrap_or("");
    if kind == "file" || kind == NodeKind::Module.as_str() || kind == NodeKind::Component.as_str() {
        return Ok(None);
    }
    let path = node["path"].as_str().context("invalid node path")?;
    let language = node["language"].as_str().context("invalid language")?;
    let profile = crate::languages::require(language)?;
    let start = node["line"].as_u64().context("invalid start line")? as usize;
    let name = node["name"].as_str().context("invalid node name")?;
    let matches =
        crate::languages::find_symbol_byte_ranges(profile, path, text, kind, start, name)?;
    let qualname = node["qualname"].as_str().unwrap_or(name);
    let base = format!("{}:{path}:{start}:{name}", profile.node_prefix(kind));
    let field_base = format!("field:{path}:{start}:{qualname}");
    let id = node["id"].as_str().context("invalid node id")?;
    let selected = if id == base || id == field_base {
        matches.into_iter().next()
    } else {
        matches.into_iter().find(|range| {
            let col = text[..range.start].rsplit('\n').next().unwrap_or("").len();
            id == format!("{base}:{col}") || id == format!("{field_base}:{col}")
        })
    };
    if selected.is_none() && (id.starts_with("field:") || id.starts_with("template:")) {
        return Ok(None);
    }
    selected
        .map(Some)
        .context("indexed symbol boundary is unavailable; read the local file instead")
}

#[cfg(all(test, feature = "lang-python"))]
mod tests {
    use super::super::search::search_results_with_navigation;
    use super::*;

    #[test]
    fn test_symbol_boundary_recovery_and_preview_resilience() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::schema::SCHEMA_DDL).unwrap();
        let options = SourceOptions {
            enabled: true,
            leading_lines: 0,
            max_body_lines: 35,
            offset: 0,
        };
        for (id, kind, name, text, expected) in [
            (
                "type:case.py:1:Strategy",
                "type",
                "Strategy",
                "Strategy = Literal['direct']; neighbor = 1\n",
                "Strategy = Literal['direct']",
            ),
            (
                "py:case.py:1:pool_size",
                "variable",
                "pool_size",
                "pool_size = 3; neighbor = 1\n",
                "pool_size = 3",
            ),
            (
                "type:case.py:1:Strategy",
                "type",
                "Strategy",
                "type Strategy = str; neighbor = 1\n",
                "type Strategy = str",
            ),
            (
                "field:case.py:1:Pool.size",
                "field",
                "size",
                "    self.size = (\n        3\n    )\n",
                "    self.size = (\n        3\n    )\n",
            ),
        ] {
            let node = json!({"id":id,"kind":kind,"name":name,"qualname":format!("Pool.{name}"),"path":"case.py","line":1,"end_line":text.lines().count(),"language":"python"});
            let (source, _) = source_preview(&conn, &node, text, &options, 8192).unwrap();
            assert_eq!(source, expected, "{id}");
        }
        if cfg!(feature = "lang-html") {
            let node = json!({"id":"template:case.html:1:strategy","kind":"variable","name":"strategy","path":"case.html","line":1,"end_line":1,"language":"html"});
            let text = "{% set strategy = 'direct' %}\n";
            assert_eq!(
                source_preview(&conn, &node, text, &options, 8192)
                    .unwrap()
                    .0,
                text
            );
        }
        let unresolved = json!({"id":"py:case.py:1:missing","kind":"function","name":"missing","path":"case.py","line":1,"end_line":1,"language":"python"});
        assert!(symbol_range(&unresolved, "# no declaration\n").is_err());
        let results = json!({"nodes":{"total":1,"items":[unresolved]}});
        let result = search_results_with_navigation(&conn, Path::new("."), results, true).unwrap();
        let hit = &result["nodes"]["items"][0];
        assert_eq!(hit["path"], "case.py");
        assert_eq!(hit["line"], 1);
        assert_eq!(hit["inspect_selector"]["selector"], hit["id"]);
        assert!(hit.get("source_preview").is_none());
    }

    #[test]
    fn indexed_text_resolution_uses_whole_tokens_and_respects_preview_byte_cap() {
        let source = "fn prepared() {}\n// red line\n// red and blue\n";
        let resolved = resolve_indexed_text_match(source, 1, 3, "red blue", 4096)
            .expect("all-token line should match");
        assert_eq!(resolved.line, 3);
        assert_eq!(resolved.snippet, "// red and blue");
        assert!(resolved.preview.is_some());

        let split = "fn prepared() {}\nfn blue_later() {}\n";
        assert!(
            resolve_indexed_text_match(split, 1, 2, "red", 4096).is_none(),
            "whole-token match must not hit substring 'red' in 'prepared'"
        );

        let accented = "fn cafeorder() {}\n// Caf\u{00e9}\n";
        let unicode_hit = resolve_indexed_text_match(accented, 1, 2, "cafe", 4096)
            .expect("unicode61 diacritic/case folding should match line 2");
        assert_eq!(unicode_hit.line, 2);

        let oversized = format!("// needle {}\n", "a".repeat(200));
        let capped = resolve_indexed_text_match(&oversized, 1, 1, "needle", 64).unwrap();
        assert_eq!(capped.line, 1);
        assert!(capped.snippet.contains("needle"));
        assert!(
            capped.preview.is_none(),
            "oversized line must drop preview rather than exceed budget"
        );

        // Reuse of thread-local FTS5 handle must clear prior document lines cleanly and honor natural OR fallback.
        let second = "fn unrelated() {}\n// only blue here\n";
        assert!(
            resolve_indexed_text_match(second, 1, 2, "needle", 4096).is_none(),
            "thread-local FTS5 table must not retain rows from prior calls"
        );
        let or_fallback = resolve_indexed_text_match(second, 1, 2, "red blue", 4096)
            .expect("natural query should fall back to OR without repopulating");
        assert_eq!(or_fallback.line, 2);
    }
}
