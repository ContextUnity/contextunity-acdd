use crate::languages::harness::ExtractorHarness;

#[test]
fn html_template_masking_boundaries() {
    struct Case {
        case_id: &'static str,
        path: &'static str,
        source: &'static str,
        errors: Option<usize>,
        script_error: bool,
        includes: &'static [&'static str],
        extends: &'static [&'static str],
        function: Option<(&'static str, &'static str, usize)>,
    }
    let mut cases = vec![
        Case {
            case_id: "html_django_template_preprocessor_avoids_syntax_errors_and_extracts_links",
            path: "templates/item.html",
            source: r#"
{% extends "base.html" %}
{% load static %}
{% block content %}
<div id="container-{{ item.id }}" class="card">
    <h1>{{ item.title }}</h1>
    {% if item.is_active %}
        <p>Active</p>
    {% endif %}
    {% include "partials/sidebar.html" with extra="val" %}
</div>
<script>
    let currentId = {{ item.id }};
    {% if item.is_active %}
    console.log("active: " + currentId);
    {% endif %}
</script>
{% endblock %}
"#,
            errors: Some(0),
            script_error: false,
            includes: &["partials/sidebar.html"],
            extends: &["base.html"],
            function: None,
        },
        Case {
            case_id: "html_template_file_still_reports_syntax_error_in_plain_script",
            path: "template_with_broken_js.html",
            source: "\n<div>{{ item.title }}</div>\n<script>\n    function broken( {\n</script>\n",
            errors: None,
            script_error: true,
            includes: &[],
            extends: &[],
            function: None,
        },
        Case {
            case_id: "unicode_template_tags_keep_embedded_javascript_byte_coordinates",
            path: "template.html",
            source: "<script>{% if λ %}function afterMask() { return 1; }{% endif %}</script>",
            errors: None,
            script_error: false,
            includes: &[],
            extends: &[],
            function: Some(("afterMask", "function afterMask", 1)),
        },
    ];
    for source in [
        "{%",
        "{% ",
        "{% include",
        "{#",
        "{# unclosed comment",
        "{{",
        "{{ unclosed var",
    ] {
        cases.push(Case {
            case_id: "html_truncated_template_tags_do_not_panic",
            path: "truncated.html",
            source,
            errors: None,
            script_error: false,
            includes: &[],
            extends: &[],
            function: None,
        });
    }
    for case in cases {
        let facts = ExtractorHarness::for_lang("html")
            .at_path(case.path)
            .try_extract(case.source)
            .unwrap_or_else(|error| panic!("{}: {:?}: {error}", case.case_id, case.source));
        if let Some(expected) = case.errors {
            assert_eq!(
                facts.errors.len(),
                expected,
                "{}: {:?}",
                case.case_id,
                facts.errors
            );
        }
        if case.script_error {
            assert!(
                facts
                    .errors
                    .iter()
                    .any(|e| e.message.contains("embedded JavaScript")),
                "{}: {:?}",
                case.case_id,
                facts.errors
            );
        }
        for (kind, expected) in [("includes", case.includes), ("extends", case.extends)] {
            let actual: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.kind == kind)
                .map(|r| r.expression.as_str())
                .collect();
            assert_eq!(actual, expected, "{}: {kind}", case.case_id);
        }
        if let Some((name, marker, line)) = case.function {
            let function = facts
                .nodes
                .iter()
                .find(|node| node.name == name)
                .expect("masked function indexed");
            assert_eq!(function.line, line, "{}", case.case_id);
            assert_eq!(
                function.details["column"].as_u64(),
                Some(case.source.find(marker).unwrap() as u64),
                "{}",
                case.case_id
            );
        }
    }
}

#[cfg(feature = "lang-vue")]
#[test]
fn vue_mask_keeps_interpolation_references_and_script_coordinates() {
    let source = "<template><p>{{ user.name }}</p></template>\n<script setup>function afterVue() {}</script>";
    let facts = ExtractorHarness::for_lang("vue")
        .at_path("component.vue")
        .extract(source);
    assert!(facts
        .references
        .iter()
        .any(|reference| reference.expression == "user.name"));
    let function = facts
        .nodes
        .iter()
        .find(|node| node.name == "afterVue")
        .expect("script function remains indexed after template masking");
    assert_eq!(function.line, 2);
    assert_eq!(
        function.details["column"].as_u64(),
        Some(
            source
                .lines()
                .nth(1)
                .unwrap()
                .find("function afterVue")
                .unwrap() as u64
        )
    );
}
