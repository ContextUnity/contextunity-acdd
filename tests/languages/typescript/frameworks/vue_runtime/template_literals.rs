use super::super::*;

#[cfg(feature = "lang-vue")]
#[test]
fn vue_template_literal_coordinates() {
    let cases: &[(&str, &str, &[&str], &[&str])] = &[
        ("Timer.vue", "<template>\n  <div>⏱ {{ timestamp }}</div>\n  <button @click=\"onSubmit\">Go</button>\n</template>\n", &["timestamp", "onSubmit"], &[]),
        ("Card.vue", r#"
<template>
  <div :class="['bg-white/5', active && 'text-blue-500', { 'hover:opacity-100': isHovered }]">
    <p>{{ user.name + ' - anonymous' }}</p>
  </div>
</template>
<script setup lang="ts">
const active = true;
const isHovered = false;
const user = { name: "User" };
</script>
"#, &["active", "isHovered", "user", "user.name"], &["bg", "white", "text", "blue", "hover", "opacity", "anonymous"]),
        ("TemplateLiteral.vue", r#"
<template>
  <div :title="`User: ${user.name} (${role})`" :class="`prefix ${active ? 'is-active' : ''}`">
    <p>{{ `Hello ${target.title}!` }}</p>
  </div>
</template>
<script setup lang="ts">
const user = { name: "Antigravity" };
const role = "Admin";
const active = true;
const target = { title: "World" };
</script>
"#, &["user", "user.name", "role", "active", "target", "target.title"], &["User", "prefix", "Hello", "World"]),
    ];
    for &(path, source, expected, absent) in cases {
        let facts = ExtractorHarness::for_lang("vue")
            .at_path(path)
            .extract(source);
        let expressions: Vec<_> = facts
            .references
            .iter()
            .map(|r| r.expression.as_str())
            .collect();
        for expression in expected {
            assert!(
                expressions.contains(expression),
                "{path}: missing {expression}: {expressions:?}"
            );
        }
        for literal in absent {
            assert!(
                !expressions.contains(literal),
                "{path}: literal {literal}: {expressions:?}"
            );
        }
    }
}
