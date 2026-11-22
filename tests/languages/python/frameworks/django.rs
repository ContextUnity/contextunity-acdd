use super::*;
use crate::languages::harness::PersistedSnapshot;
use rusqlite::Connection;
use std::sync::OnceLock;

fn baseline() -> &'static PersistedSnapshot {
    static BASELINE: OnceLock<PersistedSnapshot> = OnceLock::new();
    BASELINE.get_or_init(|| {
    let workspace = fixture_workspace(&[]);
    workspace.write(
        "pyproject.toml",
        "[project]\nname='django-drf-receivers'\ndependencies=['django', 'djangorestframework']\n",
    );
    let models = concat!(
        "from django.db.models import QuerySet, Manager, ForeignKey, Model\n\nclass ProductQuerySet(QuerySet):\n    def enabled(self):\n        return self.filter(active=True)\n\nclass ProductManager(Manager):\n    def visible(self):\n        return self.get_queryset()\n\nclass ProductRelation(ForeignKey):\n    def related_type(self):\n        return self.get_internal_type()\n\nclass Unrelated:\n    def query(self):\n        return self.filter(active=True)\n",
        r#"
class Product(Model):
    pass

class LocalSelection(QuerySet):
    def own(self):
        pass

class OverriddenSelection(QuerySet):
    def filter(self):
        return unknown()

class LocalObjects(Model):
    objects = unknown()

def finite_returns(selection: LocalSelection, manager: ProductManager, overridden: OverriddenSelection):
    filtered = Product.objects.filter(active=True)
    filtered.exclude(active=False)
    Product.objects.all().filter(active=True)
    retained = selection.filter(active=True)
    retained.own()
    selection.filter(active=True).own()
    selected = manager.filter(active=True)
    selected.exclude(active=False)
    selected.visible()
    filtered.invented()
    counted = Product.objects.count()
    counted.exclude(active=False)
    replaced = overridden.filter()
    replaced.exclude(active=False)
    LocalObjects.objects.filter(active=True)
    unproved = unknown()
    unproved.exclude(active=False)
    changed = Product.objects.all()
    changed = unknown()
    changed.exclude(active=False)
    manager.visible()
    ordered = Product.objects.order_by('name')
    ordered.exclude(active=False)
    Product.objects.order_by('name').filter(active=True)
    related = Product.objects.select_related('parent')
    related.exclude(active=False)
    Product.objects.select_related('parent').filter(active=True)
    prefetched = Product.objects.prefetch_related('children')
    prefetched.exclude(active=False)
    Product.objects.prefetch_related('children').filter(active=True)
    local_ordered = selection.order_by('name')
    local_ordered.own()
    selection.order_by('name').own()
    local_related = selection.select_related('parent')
    local_related.own()
    selection.select_related('parent').own()
    local_prefetched = selection.prefetch_related('children')
    local_prefetched.own()
    selection.prefetch_related('children').own()
    manager_ordered = manager.order_by('name')
    manager_ordered.exclude(active=False)
    manager_ordered.visible()
    manager_related = manager.select_related('parent')
    manager_related.exclude(active=False)
    manager_related.visible()
    manager_prefetched = manager.prefetch_related('children')
    manager_prefetched.exclude(active=False)
    manager_prefetched.visible()
    LocalObjects.objects.order_by('name')
    LocalObjects.objects.select_related('parent')
    LocalObjects.objects.prefetch_related('children')
    got = Product.objects.get()
    got.exclude(active=False)
    created = Product.objects.create()
    created.exclude(active=False)
    first = Product.objects.first()
    first.exclude(active=False)
"#
    );
    workspace.write("app/models.py", models);
    workspace.write(
        "app/api.py",
        "from rest_framework.serializers import Serializer, ModelSerializer\nfrom rest_framework.views import APIView\nfrom rest_framework.generics import GenericAPIView\nfrom rest_framework.viewsets import ViewSet, ModelViewSet\n\nclass RequestSerializer(Serializer):\n    def validate(self):\n        return self.is_valid()\n\nclass ProductSerializer(ModelSerializer):\n    def valid(self):\n        return self.is_valid()\n\nclass ProductAPIView(APIView):\n    def get(self, request):\n        return self.get_permissions()\n\nclass ProductGenericAPIView(GenericAPIView):\n    def list(self, request):\n        return self.get_serializer()\n\nclass ProductViewSet(ModelViewSet):\n    def list(self, request):\n        return self.get_queryset()\n\nclass BasicViewSet(ViewSet):\n    def detail(self):\n        return self.reverse_action('detail')\n",
    );
    workspace.write(
        "app/routes.py",
        "from rest_framework.routers import DefaultRouter\nfrom app.api import ProductViewSet\nrouter = DefaultRouter()\nrouter.register('products', ProductViewSet, basename='product')\nrouter.register(prefix, ProductViewSet, basename='dynamic')\nrouter.register('duplicate', ProductViewSet, basename='duplicate-a')\nrouter.register('duplicate', ProductViewSet, basename='duplicate-b')\nother.register('unrelated', ProductViewSet, basename='unrelated')\n",
    );

    workspace.build();
        PersistedSnapshot::from_connection(&workspace.open())
    })
}

const MEMBERS: &[(&str, i64, &str, &str)] = &[
    (
        "app/models.py",
        5,
        "self.filter",
        "django.db.models.QuerySet",
    ),
    (
        "app/models.py",
        9,
        "self.get_queryset",
        "django.db.models.Manager",
    ),
    (
        "app/models.py",
        13,
        "self.get_internal_type",
        "django.db.models.ForeignKey",
    ),
    (
        "app/api.py",
        8,
        "self.is_valid",
        "rest_framework.serializers.Serializer",
    ),
    (
        "app/api.py",
        12,
        "self.is_valid",
        "rest_framework.serializers.ModelSerializer",
    ),
    (
        "app/api.py",
        16,
        "self.get_permissions",
        "rest_framework.views.APIView",
    ),
    (
        "app/api.py",
        20,
        "self.get_serializer",
        "rest_framework.generics.GenericAPIView",
    ),
    (
        "app/api.py",
        24,
        "self.get_queryset",
        "rest_framework.viewsets.ModelViewSet",
    ),
    (
        "app/api.py",
        28,
        "self.reverse_action",
        "rest_framework.viewsets.ViewSet",
    ),
];

const RETURNS: &[(&str, &str)] = &[
    ("Product.objects.filter", "external"),
    ("filtered.exclude", "external"),
    ("Product.objects.all().filter", "external"),
    ("retained.own", "resolved"),
    ("selection.filter(active=True).own", "resolved"),
    ("selected.exclude", "external"),
    ("selected.visible", "unresolved"),
    ("filtered.invented", "unresolved"),
    ("counted.exclude", "unresolved"),
    ("replaced.exclude", "unresolved"),
    ("LocalObjects.objects.filter", "unresolved"),
    ("unproved.exclude", "unresolved"),
    ("changed.exclude", "unresolved"),
    ("manager.visible", "resolved"),
    ("ordered.exclude", "external"),
    ("Product.objects.order_by('name').filter", "external"),
    ("related.exclude", "external"),
    (
        "Product.objects.select_related('parent').filter",
        "external",
    ),
    ("prefetched.exclude", "external"),
    (
        "Product.objects.prefetch_related('children').filter",
        "external",
    ),
    ("local_ordered.own", "resolved"),
    ("selection.order_by('name').own", "resolved"),
    ("local_related.own", "resolved"),
    ("selection.select_related('parent').own", "resolved"),
    ("local_prefetched.own", "resolved"),
    ("selection.prefetch_related('children').own", "resolved"),
    ("manager_ordered.exclude", "external"),
    ("manager_ordered.visible", "unresolved"),
    ("manager_related.exclude", "external"),
    ("manager_related.visible", "unresolved"),
    ("manager_prefetched.exclude", "external"),
    ("manager_prefetched.visible", "unresolved"),
    ("LocalObjects.objects.order_by", "unresolved"),
    ("LocalObjects.objects.select_related", "unresolved"),
    ("LocalObjects.objects.prefetch_related", "unresolved"),
    ("got.exclude", "unresolved"),
    ("created.exclude", "unresolved"),
    ("first.exclude", "unresolved"),
];

fn statuses_in(conn: &Connection, path: &str, expression: &str) -> Vec<(String, String)> {
    conn.prepare("SELECT c.status,v.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence v ON v.evidence_id=c.evidence_id WHERE p.path=?1 AND x.expression=?2 ORDER BY c.line")
        .unwrap().query_map([path, expression], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap().map(Result::unwrap).collect()
}

fn statuses(conn: &Connection, expression: &str) -> Vec<(String, String)> {
    statuses_in(conn, "app/models.py", expression)
}

#[test]
fn framework_member_authority() {
    let conn = baseline().open();

    for &(path, line, expression, provider) in MEMBERS {
        let (status, evidence) = persisted_coverage(&conn, path, line, expression);
        assert_eq!(status, "external", "{path}:{line} {expression}: {evidence}");
        assert!(evidence.contains(provider), "{path}:{line} {evidence}");
    }
    assert_eq!(
        persisted_coverage(&conn, "app/models.py", 17, "self.filter").0,
        "unresolved"
    );

    for &(expression, expected) in RETURNS {
        if expected != "external" {
            continue;
        }
        for (_, evidence) in statuses(&conn, expression) {
            assert!(evidence.contains("django"), "{expression}: {evidence}");
            assert!(
                !evidence.contains("configured_receiver")
                    && !evidence.contains("external import  at line 0"),
                "finite receiver authority and origin: {expression}: {evidence}"
            );
        }
        let calls: i64 = conn
            .query_row("SELECT count(*) FROM edge_occurrences e JOIN coverage_evidence v ON v.evidence_id=e.evidence_id WHERE e.kind='calls' AND v.evidence=?1", [expression], |row| row.get(0))
            .unwrap();
        assert_eq!(
            calls, 0,
            "external return never invents a local call: {expression}"
        );
        let origin_class = match expression {
            "selected.exclude"
            | "manager_ordered.exclude"
            | "manager_related.exclude"
            | "manager_prefetched.exclude" => Some("ProductManager"),
            "filtered.exclude"
            | "Product.objects.all().filter"
            | "ordered.exclude"
            | "Product.objects.order_by('name').filter"
            | "related.exclude"
            | "Product.objects.select_related('parent').filter"
            | "prefetched.exclude"
            | "Product.objects.prefetch_related('children').filter" => Some("Product"),
            _ => None,
        };
        let Some(origin_class) = origin_class else {
            continue;
        };
        let class_id: String = conn
            .query_row("SELECT n.id FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path='app/models.py' AND n.kind='class' AND n.name=?1", [origin_class], |row| row.get(0))
            .unwrap();
        let prefix = format!(
            "finite python configured member {expression}; pack django; hierarchy provider django.db.models.QuerySet; ordered bases django.db.models.QuerySet,django.db.models.query.QuerySet; receiver origin external import django.db.models at line 1; configuration "
        );
        let source_identity = format!(":app/models.py:django:{class_id}");
        for (_, evidence) in statuses(&conn, expression) {
            let configuration = evidence
                .strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("{expression}: {evidence}"));
            assert!(
                configuration.len() > source_identity.len()
                    && configuration.ends_with(&source_identity),
                "originating class: {expression}: {evidence}"
            );
        }
    }
}

#[test]
fn finite_method_returns_preserve_exact_resolution_statuses() {
    let conn = baseline().open();
    for &(expression, expected) in RETURNS {
        let actual = statuses(&conn, expression);
        assert!(
            !actual.is_empty() && actual.iter().all(|row| row.0 == expected),
            "finite method return {expression}: expected {expected}, observed {actual:?}"
        );
    }
}

#[test]
fn only_unique_literal_viewset_registrations_create_route_handlers() {
    let conn = baseline().open();
    let router_edges: Vec<String> = conn
        .prepare(
            "SELECT json_array(json_extract(src.details,'$.route_path'),dst.path_id,dst.name) FROM edge_occurrences e JOIN path_dictionary p ON p.path_id=e.owner_id JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE p.path='app/routes.py' AND e.kind='handles' ORDER BY src.line",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        router_edges.len(),
        1,
        "only one proven ViewSet registration has a handler edge: {router_edges:?}"
    );
    assert!(router_edges[0].contains("products"));
    assert!(router_edges[0].contains("ProductViewSet"));
    let route_count: i64 = conn.query_row("SELECT count(*) FROM nodes WHERE kind='route' AND path_id=(SELECT path_id FROM path_dictionary WHERE path='app/routes.py')", [], |row| row.get(0)).unwrap();
    assert_eq!(
        route_count, 1,
        "dynamic and duplicate registrations do not create route handler facts"
    );
}

#[test]
fn django_commands_models_auth_and_unittest_framework_members() {
    let workspace = fixture_workspace(&[]);
    workspace.write(
        "pyproject.toml",
        "[project]\nname='django-expanded'\ndependencies=['django']\n",
    );
    workspace.write(
        "app/commands.py",
        concat!(
            "from django.core.management.base import BaseCommand\n",
            "class SyncCommand(BaseCommand):\n",
            "    def handle(self):\n",
            "        self.stdout.write(self.style.SUCCESS('ok'))\n",
            "        self.style.INVENTED('bad')\n",
        ),
    );
    workspace.write(
        "app/users.py",
        concat!(
            "from django.db.models import Model\n",
            "from django.contrib.auth import get_user_model\n",
            "class Item(Model):\n",
            "    pass\n",
            "def sync(item: Item):\n",
            "    item.save()\n",
            "    item.refresh_from_db()\n",
            "    item.missing()\n",
            "    Item.objects.only('id').distinct().filter(active=True)\n",
            "    User = get_user_model()\n",
            "    User.objects.filter(is_active=True)\n",
            "    get_user_model().objects.filter(is_active=True)\n",
            "    User.objects.invented()\n",
        ),
    );
    workspace.write(
        "tests/test_app.py",
        concat!(
            "import unittest\n",
            "from django.test import TestCase\n",
            "class DjangoSuite(TestCase):\n",
            "    def test_view(self):\n",
            "        res = self.client.get('/')\n",
            "        self.assertContains(res, 'ok')\n",
            "        self.assertInvented(res)\n",
            "class UnitSuite(unittest.TestCase):\n",
            "    def test_unit(self):\n",
            "        self.assertMultiLineEqual('a', 'a')\n",
            "        self.assertBogus()\n",
        ),
    );
    workspace.build();
    let conn = workspace.open();
    for (path, expression, expected) in [
        ("app/commands.py", "self.stdout.write", "external"),
        ("app/commands.py", "self.style.SUCCESS", "external"),
        ("app/commands.py", "self.style.INVENTED", "unresolved"),
        ("app/users.py", "item.save", "external"),
        ("app/users.py", "item.refresh_from_db", "external"),
        ("app/users.py", "item.missing", "unresolved"),
        ("app/users.py", "Item.objects.only('id').distinct().filter", "external"),
        ("app/users.py", "User.objects.filter", "external"),
        ("app/users.py", "get_user_model().objects.filter", "external"),
        ("app/users.py", "User.objects.invented", "unresolved"),
        ("tests/test_app.py", "self.client.get", "external"),
        ("tests/test_app.py", "self.assertContains", "external"),
        ("tests/test_app.py", "self.assertInvented", "unresolved"),
        ("tests/test_app.py", "self.assertMultiLineEqual", "external"),
        ("tests/test_app.py", "self.assertBogus", "unresolved"),
    ] {
        let actual = statuses_in(&conn, path, expression);
        assert!(
            !actual.is_empty() && actual.iter().all(|row| row.0 == expected),
            "{path} {expression}: expected {expected}, observed {actual:?}"
        );
    }
}

#[test]
fn django_queryset_compound_return_and_item_projection() {
    let workspace = fixture_workspace(&[]);
    workspace.write(
        "pyproject.toml",
        "[project]\nname='django-compound'\ndependencies=['django']\n",
    );
    workspace.write(
        "app/posts.py",
        concat!(
            "from django.db.models import Model\n",
            "class Post(Model):\n",
            "    pass\n",
            "def publish_latest():\n",
            "    latest = Post.objects.filter(published=True).order_by('title').first()\n",
            "    latest.save()\n",
            "    latest.invented()\n",
        ),
    );
    workspace.build();
    let conn = workspace.open();
    for (expression, expected) in [
        ("Post.objects.filter", "external"),
        ("Post.objects.filter(published=True).order_by", "external"),
        ("Post.objects.filter(published=True).order_by('title').first", "external"),
        ("latest.save", "external"),
        ("latest.invented", "unresolved"),
    ] {
        let actual = statuses_in(&conn, "app/posts.py", expression);
        assert!(
            !actual.is_empty() && actual.iter().all(|row| row.0 == expected),
            "{expression}: expected {expected}, observed {actual:?}"
        );
    }
}
