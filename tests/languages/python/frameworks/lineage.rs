use super::*;
use crate::languages::harness::PersistedSnapshot;

fn lineage_snapshot() -> &'static PersistedSnapshot {
    static SNAPSHOT: std::sync::OnceLock<PersistedSnapshot> = std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| {

    let first = fixture_workspace(&[]);
    first.write("app/__init__.py", "");
    first.write(
        "app/models.py",
        "from django.db import models\nclass Product(models.Model):\n    pass\n",
    );
    first.write(
        "app/contracts.py",
        "from typing import Protocol\nclass Notifier(Protocol):\n    def notify(self) -> None: ...\n",
    );
    first.write(
        "consumer.py",
        concat!(
            "from django.http import HttpRequest\n",
            "from django.core.management.base import BaseCommand\n",
            "from pydantic import BaseModel\n",
            "from app.models import Product\n",
            "from app.contracts import Notifier\n",
            "class SecuredRequest(HttpRequest):\n",
            "    pass\n",
            "class Command(BaseCommand):\n",
            "    def handle(self) -> None:\n",
            "        self.stdout.write('ok')\n",
            "class Record(BaseModel):\n",
            "    title: str\n",
            "def run(request: SecuredRequest, record: Record, notifier: Notifier) -> None:\n",
            "    request.GET.get('q')\n",
            "    Product.objects.create()\n",
            "    Record.model_validate({'title': 'ok'})\n",
            "    record.model_dump()\n",
            "    notifier.notify()\n",
            "def choose(flag: bool) -> None:\n",
            "    if flag:\n",
            "        model = Product\n",
            "    else:\n",
            "        model = Notifier\n",
            "    model.objects.create()\n",
        ),
    );



    let second = fixture_workspace(&[]);
    second.write("app/__init__.py", "");
    second.write("app/models.py", "class Product:\n    pass\n");
    second.write(
        "app/contracts.py",
        "from typing import Protocol\nclass Notifier(Protocol):\n    def cancel(self) -> None: ...\n",
    );
    second.write(
        "consumer.py",
        concat!(
            "from app.models import Product\n",
            "from app.contracts import Notifier\n",
            "def run(notifier: Notifier, client) -> None:\n",
            "    Product.objects.create()\n",
            "    notifier.notify()\n",
            "    client.send()\n",
            "def test_injected(db_client) -> None:\n",
            "    db_client.query()\n",
        ),
    );


    let linked = fixture_workspace(&[]);
    linked.write("acdd.yaml", &format!("roots: [.]\nlinked_workspaces:\n  - name: first\n    path: '{}'\n    roots: [.]\n  - name: second\n    path: '{}'\n    roots: [.]\n", first.root().display(), second.root().display()));
    linked.build();
    PersistedSnapshot::from_connection(&linked.open())
    })
}

#[test]
fn framework_lineage_exact_and_unproved() {
    let conn = lineage_snapshot().open();
    for (case_id, project, line, expression, expected, origin) in [
        (
            "imported_framework_lineage",
            "first",
            10,
            "self.stdout.write",
            "external",
            "django",
        ),
        (
            "imported_framework_lineage",
            "first",
            14,
            "request.GET.get",
            "external",
            "django",
        ),
        (
            "imported_framework_lineage",
            "first",
            15,
            "Product.objects.create",
            "external",
            "django",
        ),
        (
            "imported_framework_lineage",
            "first",
            16,
            "Record.model_validate",
            "external",
            "pydantic",
        ),
        (
            "imported_framework_lineage",
            "first",
            17,
            "record.model_dump",
            "external",
            "pydantic",
        ),
        (
            "imported_framework_lineage",
            "first",
            18,
            "notifier.notify",
            "resolved",
            "",
        ),
        (
            "conditional_alias",
            "first",
            24,
            "model.objects.create",
            "unresolved",
            "",
        ),
        (
            "unproved_local_members",
            "second",
            4,
            "Product.objects.create",
            "unresolved",
            "",
        ),
        (
            "unproved_local_members",
            "second",
            5,
            "notifier.notify",
            "unresolved",
            "",
        ),
        (
            "injected_names",
            "second",
            6,
            "client.send",
            "unresolved",
            "",
        ),
        (
            "injected_names",
            "second",
            8,
            "db_client.query",
            "unresolved",
            "",
        ),
    ] {
        let path = format!("[{project}]/consumer.py");
        let (status, evidence) = persisted_coverage(&conn, &path, line, expression);
        assert_eq!(status, expected, "{case_id}: {expression}: {evidence}");
        assert!(
            evidence.contains(origin),
            "{case_id}: {expression}: {evidence}"
        );
    }
    let local_edge: i64 = query_scalar(&conn,
        "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash JOIN path_dictionary owner ON owner.path_id=e.owner_id JOIN path_dictionary target ON target.path_id=dst.path_id WHERE owner.path='[first]/consumer.py' AND e.kind='calls' AND target.path='[first]/app/contracts.py' AND dst.kind='method' AND dst.name='notify'", []);
    assert_eq!(
        local_edge, 1,
        "protocol call retains its exact local method"
    );
}

#[test]
fn linked_projects_preserve_framework_lineage_and_protocol_identity_isolation() {
    let linked_db = lineage_snapshot().open();
    assert_eq!(
        persisted_coverage(&linked_db, "[first]/consumer.py", 14, "request.GET.get").0,
        "external"
    );
    assert_eq!(
        persisted_coverage(&linked_db, "[first]/consumer.py", 18, "notifier.notify").0,
        "resolved"
    );
    for (line, expression) in [(4, "Product.objects.create"), (5, "notifier.notify")] {
        assert_eq!(
            persisted_coverage(&linked_db, "[second]/consumer.py", line, expression).0,
            "unresolved",
            "project isolation for {expression}"
        );
    }
}
