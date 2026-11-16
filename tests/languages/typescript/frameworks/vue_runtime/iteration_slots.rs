use super::super::*;
use crate::languages::harness::ExtractorHarness;

#[cfg(feature = "lang-vue")]
#[test]
fn vue_iteration_and_slot_scopes_keep_all_bindings_and_source_coordinates() {
    let source = r#"<template>
  <div v-for="(item, index) in items" :title="item.name">{{ index }}</div>
  <Panel v-slot="{ entry, count }">{{ entry.title }} {{ count }}</Panel>
</template>
<script setup>const items = [];</script>"#;
    let facts = ExtractorHarness::for_lang("vue")
        .at_path("Scoped.vue")
        .extract(source);
    let iteration = facts
        .nodes
        .iter()
        .find(|node| {
            node.kind == "template_scope"
                && crate::common::assertions::projected_node_details(&facts, node)["bindings"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == "item"))
        })
        .unwrap();
    assert!(
        crate::common::assertions::projected_node_details(&facts, iteration)["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "index")
    );
    let index = facts
        .references
        .iter()
        .find(|reference| reference.expression == "index")
        .unwrap();
    assert_eq!(index.source, iteration.id);
    assert_eq!(index.line, 2);
    let line = source.lines().nth(index.line - 1).unwrap();
    assert!(line[index.column..].starts_with("index"));
    let slot = facts
        .nodes
        .iter()
        .find(|node| {
            node.kind == "template_scope"
                && crate::common::assertions::projected_node_details(&facts, node)["bindings"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == "entry"))
        })
        .unwrap();
    assert!(
        crate::common::assertions::projected_node_details(&facts, slot)["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "count")
    );
    assert!(facts
        .references
        .iter()
        .any(|reference| reference.source == slot.id && reference.expression == "entry.title"));
}

#[cfg(feature = "lang-vue")]
#[test]
fn typed_iteration_and_slot_members() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    {
        let source = "<template><ul><li v-for=\"(item, index) in items\">{{ item.title }} {{ index }}</li></ul></template>\n<script setup lang=\"ts\">interface Item { title: string } const items: Item[] = [];</script>";
        w.write("TypedList.vue", source);
        w.write(
        "TypedCall.vue",
        "<template><button v-for=\"item in items\" @click=\"item.title()\" /></template>\n<script setup lang=\"ts\">interface Item { title: string } const items: Item[] = [];</script>",
    );
        w.write(
        "ShadowedIterable.vue",
        "<template><div v-for=\"items in groups\"><span v-for=\"item in items\">{{ item.title }}</span></div></template>\n<script setup lang=\"ts\">interface Item { title: string } const items: Item[] = []; const groups = [];</script>",
    );
    }
    {
        w.write(
        "app/Panel.vue",
        "<template><slot /></template>\n<script lang=\"ts\">const runtimeTsSlots = defineSlots();</script>\n<script setup lang=\"ts\">interface Item { title: string } defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
        w.write(
        "app/JavascriptSlotsPanel.vue",
        "<template><slot /></template>\n<script>const runtimeJsSlots = defineSlots();</script>\n<script setup lang=\"ts\">interface Item { title: string } defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
        w.write(
        "app/View.vue",
        "<template><Panel v-slot=\"{ entry }\">{{ entry.title }}</Panel></template>\n<script setup lang=\"ts\">import Panel from './Panel.vue';</script>",
    );
        w.write(
        "app/NestedSlot.vue",
        "<template><Panel><template #default=\"{ entry }\">{{ entry.title }}</template></Panel></template>\n<script setup lang=\"ts\">import Panel from './Panel.vue';</script>",
    );
        w.write(
        "app/JavascriptSlots.vue",
        "<template><JavascriptSlotsPanel v-slot=\"{ entry }\">{{ entry.title }}</JavascriptSlotsPanel></template>\n<script setup lang=\"ts\">import JavascriptSlotsPanel from './JavascriptSlotsPanel.vue';</script>",
    );
        w.write("app/Plain.vue", "<template><slot /></template>");
        w.write(
        "app/AssignedSlotsPanel.vue",
        "<template><slot /></template>\n<script setup lang=\"ts\">interface Item { title: string } const slots = defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
        w.write(
        "app/AssignedSlots.vue",
        "<template><AssignedSlotsPanel v-slot=\"{ entry }\">{{ entry.title }}</AssignedSlotsPanel></template>\n<script setup lang=\"ts\">import AssignedSlotsPanel from './AssignedSlotsPanel.vue';</script>",
    );
        w.write(
        "app/NestedComponent.vue",
        "<template><Panel><Plain><template #default=\"{ entry }\">{{ entry.title }}</template></Plain></Panel></template>\n<script setup lang=\"ts\">import Panel from './Panel.vue'; import Plain from './Plain.vue';</script>",
    );
        w.write(
        "app/Unknown.vue",
        "<template><MissingPanel v-slot=\"{ entry }\">{{ entry.title }}</MissingPanel></template>",
    );
        w.write(
        "app/Shadow.vue",
        "<template><Panel v-slot=\"{ entry }\"><span v-for=\"entry in entries\">{{ entry.title }}</span></Panel></template>\n<script setup lang=\"ts\">import Panel from './Panel.vue'; const entries = [];</script>",
    );
        w.write(
        "app/FieldCall.vue",
        "<template><Panel v-slot=\"{ entry }\"><button @click=\"entry.title()\" /></Panel></template>\n<script setup lang=\"ts\">import Panel from './Panel.vue';</script>",
    );
        w.write(
            "app/macro.ts",
            "export function other() {} export function defineSlots() {}",
        );
        w.write(
        "app/ShadowedMacroPanel.vue",
        "<template><slot /></template>\n<script setup lang=\"ts\">import { other as defineSlots } from './macro'; interface Item { title: string } defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
        w.write(
        "app/ShadowedMacro.vue",
        "<template><ShadowedMacroPanel v-slot=\"{ entry }\">{{ entry.title }}</ShadowedMacroPanel></template>\n<script setup lang=\"ts\">import ShadowedMacroPanel from './ShadowedMacroPanel.vue';</script>",
    );
        w.write(
        "app/UnrelatedImportPanel.vue",
        "<template><slot /></template>\n<script setup lang=\"ts\">import { defineSlots as other } from './macro'; interface Item { title: string } defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
        w.write(
        "app/UnrelatedImport.vue",
        "<template><UnrelatedImportPanel v-slot=\"{ entry }\">{{ entry.title }}</UnrelatedImportPanel></template>\n<script setup lang=\"ts\">import UnrelatedImportPanel from './UnrelatedImportPanel.vue';</script>",
    );
        w.write(
        "other/Panel.vue",
        "<template><slot /></template>\n<script setup lang=\"ts\">interface Item { title: string } defineSlots<{ default(props: { entry: Item }): any }>();</script>",
    );
    }

    w.build();
    let conn = w.open();
    for (path, expression, expected, evidence) in [
        ("TypedList.vue", "item.title", "resolved", ""),
        ("TypedCall.vue", "item.title", "unresolved", ""),
        ("ShadowedIterable.vue", "item.title", "unresolved", ""),
        ("app/View.vue", "entry.title", "resolved", ""),
        ("app/NestedSlot.vue", "entry.title", "resolved", ""),
        ("app/JavascriptSlots.vue", "entry.title", "resolved", ""),
        ("app/UnrelatedImport.vue", "entry.title", "resolved", ""),
        ("app/Unknown.vue", "entry.title", "unresolved", ""),
        ("app/Shadow.vue", "entry.title", "unresolved", ""),
        ("app/FieldCall.vue", "entry.title", "unresolved", ""),
        ("app/ShadowedMacro.vue", "entry.title", "unresolved", ""),
        ("app/NestedComponent.vue", "entry.title", "unresolved", ""),
        ("app/AssignedSlots.vue", "entry.title", "unresolved", ""),
    ] {
        let (status, observed) = persisted_coverage(&conn, path, Some(1), expression);
        assert_eq!(status, expected, "{path}:{expression}: {observed}");
        if !evidence.is_empty() {
            assert!(
                observed.contains(evidence),
                "{path}:{expression}: {observed}"
            );
        }
    }
    for (path, kind, target_path, target_kind, target_name) in [
        (
            "TypedList.vue",
            "references",
            "TypedList.vue",
            "field",
            "title",
        ),
        (
            "app/View.vue",
            "references",
            "app/Panel.vue",
            "field",
            "title",
        ),
        (
            "app/NestedSlot.vue",
            "references",
            "app/Panel.vue",
            "field",
            "title",
        ),
    ] {
        let count: i64 = query_scalar(&conn, "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)=?1 AND e.kind=?2 AND e.line=1 AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)=?3 AND (?4='' OR dst.kind=?4) AND (?5='' OR dst.name=?5)", rusqlite::params![path, kind, target_path, target_kind, target_name]);
        assert_eq!(
            count, 1,
            "{path}: exact {kind} target {target_path}:{target_name}"
        );
    }
}
