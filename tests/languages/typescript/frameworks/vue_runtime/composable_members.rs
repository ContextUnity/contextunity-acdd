use super::super::*;

#[cfg(feature = "lang-vue")]
#[test]
fn composable_return_provider() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    {
        w.write(
        "Good.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup>function makeActions() { function save() { return 1; } return { save }; } const actions = makeActions(); const alias = actions;</script>",
    );
        w.write(
        "Arrow.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup>function makeActions() { const save = () => 1; return { save }; } const actions = makeActions(); const alias = actions;</script>",
    );
        w.write(
        "Unknown.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup>const alias = unknownFactory();</script>",
    );
        w.write(
        "Shadow.vue",
        "<template><div v-for=\"alias in aliases\"><button @click=\"alias.save\" /></div></template>\n<script setup>function makeActions() { function save() { return 1; } return { save }; } const actions = makeActions(); const alias = actions; const aliases = [];</script>",
    );
        w.write(
        "Rebound.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup>function makeActions() { function save() { return 1; } return { save }; } const actions = makeActions(); let alias = actions; alias = {};</script>",
    );
        w.write(
        "MutatedMember.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup>function makeActions() { function save() { return 1; } save = () => 2; return { save }; } const actions = makeActions(); const alias = actions;</script>",
    );
    }
    {
        w.write(
            "app/actions.ts",
            "export function buildActions() { function save() { return 1; } return { save }; }",
        );
        w.write(
        "app/View.vue",
        "<template><button @click=\"alias.save\" /></template>\n<script setup lang=\"ts\">import { buildActions } from './actions'; const actions = buildActions(); const alias = actions;</script>",
    );
        w.write(
            "other/actions.ts",
            "export function buildActions() { function save() { return 2; } return { save }; }",
        );
    }

    w.build();
    let conn = w.open();
    for (path, expression, expected, evidence) in [
        ("Good.vue", "alias.save", "resolved", ""),
        ("Arrow.vue", "alias.save", "resolved", ""),
        ("app/View.vue", "alias.save", "resolved", ""),
        ("Unknown.vue", "alias.save", "unresolved", ""),
        ("Shadow.vue", "alias.save", "unresolved", ""),
        ("Rebound.vue", "alias.save", "unresolved", ""),
        ("MutatedMember.vue", "alias.save", "unresolved", ""),
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
        ("Good.vue", "calls", "Good.vue", "function", "save"),
        ("Arrow.vue", "calls", "Arrow.vue", "function", "save"),
        (
            "app/View.vue",
            "calls",
            "app/actions.ts",
            "function",
            "save",
        ),
    ] {
        let count: i64 = query_scalar(&conn, "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)=?1 AND e.kind=?2 AND e.line=1 AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)=?3 AND (?4='' OR dst.kind=?4) AND (?5='' OR dst.name=?5)", rusqlite::params![path, kind, target_path, target_kind, target_name]);
        assert_eq!(
            count, 1,
            "{path}: exact {kind} target {target_path}:{target_name}"
        );
    }
}
