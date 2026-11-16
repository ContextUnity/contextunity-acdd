use crate::languages::typescript::{persisted_coverage, query_scalar};

#[cfg(feature = "lang-vue")]
#[test]
fn nuxt_component_provider_authority() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    {
        for (path, source) in [
        ("case1/apps/nuxt/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case1/apps/nuxt/nuxt.config.ts", "export default defineNuxtConfig({});"),
        ("case1/apps/nuxt/components/Base/WidgetCard.vue", "<template><article /></template>"),
        ("case1/apps/nuxt/pages/index.vue", "<template><BaseWidgetCard /></template>"),
        ("case1/apps/other/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case1/apps/other/nuxt.config.ts", "export default defineNuxtConfig({});"),
        ("case1/apps/other/components/Base/WidgetCard.vue", "<template><aside /></template>"),
        ("case1/apps/other/pages/index.vue", "<template><base-widget-card /></template>"),
        ("case1/apps/plain/package.json", r#"{"dependencies":{"vue":"^3"}}"#),
        ("case1/apps/plain/nuxt.config.ts", "export default defineNuxtConfig({});"),
        ("case1/apps/plain/components/Base/WidgetCard.vue", "<template><footer /></template>"),
        ("case1/apps/plain/pages/index.vue", "<template><BaseWidgetCard /></template>"),
        ("case1/apps/disabled/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case1/apps/disabled/nuxt.config.ts", "export default defineNuxtConfig({ components: false });"),
        ("case1/apps/disabled/components/Base/WidgetCard.vue", "<template><header /></template>"),
        ("case1/apps/disabled/pages/index.vue", "<template><BaseWidgetCard /></template>"),
        ("case1/apps/collision/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case1/apps/collision/nuxt.config.ts", "export default defineNuxtConfig({});"),
        ("case1/apps/collision/components/Base/WidgetCard.vue", "<template><article /></template>"),
        ("case1/apps/collision/components/BaseWidgetCard.vue", "<template><aside /></template>"),
        ("case1/apps/collision/pages/index.vue", "<template><BaseWidgetCard /></template>"),
        ("case1/apps/dynamic/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case1/apps/dynamic/nuxt.config.ts", "const options = { components: false }; export default defineNuxtConfig({ ...options });"),
        ("case1/apps/dynamic/components/Base/WidgetCard.vue", "<template><nav /></template>"),
        ("case1/apps/dynamic/pages/index.vue", "<template><BaseWidgetCard /></template>"),
    ] {
        w.write(path, source);
    }
    }
    {
        for (path, source) in [
        ("case2/apps/nuxt/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case2/apps/nuxt/nuxt.config.ts", "export default defineNuxtConfig({ components: false });"),
        ("case2/apps/nuxt/app.vue", "<template><NuxtLayout><NuxtPage /></NuxtLayout></template>"),
        ("case2/apps/nuxt/pages/index.vue", "<template><ClientOnly><span /></ClientOnly><UnknownWidget /></template>"),
        ("case2/apps/nuxt/pages/local.vue", "<template><ClientOnly /></template><script setup>import ClientOnly from '../widgets/ClientOnly.vue';</script>"),
        ("case2/apps/nuxt/widgets/ClientOnly.vue", "<template><div /></template>"),
        ("case2/apps/plain/package.json", r#"{"dependencies":{"vue":"^3"}}"#),
        ("case2/apps/plain/pages/index.vue", "<template><ClientOnly /></template>"),
    ] {
        w.write(path, source);
    }
    }
    {
        for (path, source) in [
        ("case3/apps/radix/package.json", r#"{"dependencies":{"nuxt":"^3","radix-vue":"^1"}}"#),
        ("case3/apps/radix/nuxt.config.ts", "export default defineNuxtConfig({ modules: ['radix-vue/nuxt'] });"),
        ("case3/apps/radix/pages/index.vue", "<template><DialogRoot><DialogPortal><DialogOverlay /><DialogContent><DialogTitle /><DialogDescription /><DialogClose /></DialogContent></DialogPortal></DialogRoot><UnknownRadixTag /></template>"),
        ("case3/apps/radix/pages/local.vue", "<template><DialogTitle /></template><script setup>import DialogTitle from '../widgets/DialogTitle.vue';</script>"),
        ("case3/apps/radix/widgets/DialogTitle.vue", "<template><h1 /></template>"),
        ("case3/apps/disabled/package.json", r#"{"dependencies":{"nuxt":"^3","radix-vue":"^1"}}"#),
        ("case3/apps/disabled/nuxt.config.ts", "export default defineNuxtConfig({ modules: [] });"),
        ("case3/apps/disabled/pages/index.vue", "<template><DialogTitle /></template>"),
        ("case3/apps/undeclared/package.json", r#"{"dependencies":{"nuxt":"^3"}}"#),
        ("case3/apps/undeclared/nuxt.config.ts", "export default defineNuxtConfig({ modules: ['radix-vue/nuxt'] });"),
        ("case3/apps/undeclared/pages/index.vue", "<template><DialogTitle /></template>"),
    ] {
        w.write(path, source);
    }
    }

    w.build();
    let conn = w.open();
    for (path, expression, expected, evidence) in [
        (
            "case1/apps/nuxt/pages/index.vue",
            "BaseWidgetCard",
            "resolved",
            "",
        ),
        (
            "case1/apps/other/pages/index.vue",
            "base-widget-card",
            "resolved",
            "",
        ),
        (
            "case1/apps/plain/pages/index.vue",
            "BaseWidgetCard",
            "unresolved",
            "",
        ),
        (
            "case1/apps/disabled/pages/index.vue",
            "BaseWidgetCard",
            "unresolved",
            "",
        ),
        (
            "case1/apps/dynamic/pages/index.vue",
            "BaseWidgetCard",
            "unresolved",
            "",
        ),
        (
            "case1/apps/collision/pages/index.vue",
            "BaseWidgetCard",
            "ambiguous",
            "",
        ),
        (
            "case2/apps/nuxt/pages/index.vue",
            "ClientOnly",
            "external",
            "builtin:nuxt",
        ),
        (
            "case2/apps/nuxt/pages/index.vue",
            "UnknownWidget",
            "unresolved",
            "",
        ),
        (
            "case2/apps/plain/pages/index.vue",
            "ClientOnly",
            "unresolved",
            "",
        ),
        (
            "case2/apps/nuxt/pages/local.vue",
            "ClientOnly",
            "resolved",
            "",
        ),
        (
            "case2/apps/nuxt/app.vue",
            "NuxtLayout",
            "external",
            "builtin:nuxt",
        ),
        (
            "case2/apps/nuxt/app.vue",
            "NuxtPage",
            "external",
            "builtin:nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogRoot",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogPortal",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogOverlay",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogContent",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogTitle",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogDescription",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "DialogClose",
            "external",
            "radix-vue/nuxt",
        ),
        (
            "case3/apps/radix/pages/index.vue",
            "UnknownRadixTag",
            "unresolved",
            "",
        ),
        (
            "case3/apps/disabled/pages/index.vue",
            "DialogTitle",
            "unresolved",
            "",
        ),
        (
            "case3/apps/undeclared/pages/index.vue",
            "DialogTitle",
            "unresolved",
            "",
        ),
        (
            "case3/apps/radix/pages/local.vue",
            "DialogTitle",
            "resolved",
            "",
        ),
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
            "case1/apps/nuxt/pages/index.vue",
            "references",
            "case1/apps/nuxt/components/Base/WidgetCard.vue",
            "",
            "",
        ),
        (
            "case1/apps/other/pages/index.vue",
            "references",
            "case1/apps/other/components/Base/WidgetCard.vue",
            "",
            "",
        ),
        (
            "case2/apps/nuxt/pages/local.vue",
            "references",
            "case2/apps/nuxt/widgets/ClientOnly.vue",
            "",
            "",
        ),
        (
            "case3/apps/radix/pages/local.vue",
            "references",
            "case3/apps/radix/widgets/DialogTitle.vue",
            "",
            "",
        ),
    ] {
        let count: i64 = query_scalar(&conn, "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)=?1 AND e.kind=?2 AND e.line=1 AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)=?3 AND (?4='' OR dst.kind=?4) AND (?5='' OR dst.name=?5)", rusqlite::params![path, kind, target_path, target_kind, target_name]);
        assert_eq!(
            count, 1,
            "{path}: exact {kind} target {target_path}:{target_name}"
        );
    }
}
