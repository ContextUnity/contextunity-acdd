use super::harness::{ExtractorHarness, PersistedSnapshot};
const TABLE_RELATION_CASES: &[(&str, &str, &str)] = &[
    (
        "SELECT * FROM public.orders",
        "public.orders",
        "queries_table",
    ),
    (
        "SELECT * FROM orders, customers",
        "customers",
        "queries_table",
    ),
    (
        "BEGIN; PRAGMA x=1; SELECT * FROM orders",
        "orders",
        "queries_table",
    ),
    (
        "SET search_path TO shop; CREATE TABLE batch_table (id INT)",
        "batch_table",
        "defines_table",
    ),
    (
        "INSERT OR REPLACE INTO orders VALUES (1)",
        "orders",
        "mutates_table",
    ),
    ("UPDATE orders SET id = 1", "orders", "mutates_table"),
    ("COMMIT; SELECT * FROM orders", "orders", "queries_table"),
    (
        "ROLLBACK; UPDATE orders AS o SET id = 1",
        "orders",
        "mutates_table",
    ),
    ("UPDATE ONLY orders o SET id = 1", "orders", "mutates_table"),
    (
        "WITH source AS (SELECT * FROM customers) UPDATE ONLY (orders) AS o SET id=1",
        "orders",
        "mutates_table",
    ),
    (
        "SELECT * FROM orders o JOIN customers c ON c.id=o.id, inventory i",
        "inventory",
        "queries_table",
    ),
    (
        "SELECT * FROM orders o, inventory i JOIN customers c ON c.id=i.id",
        "customers",
        "queries_table",
    ),
    (
        "WITH temporary AS (SELECT * FROM orders) SELECT * FROM temporary JOIN inventory ON 1=1",
        "orders",
        "queries_table",
    ),
    (
        "COMMIT; SELECT * FROM orders WHERE note = $$semi;colon$$",
        "orders",
        "queries_table",
    ),
    (
        "SET x='semi;SELECT fake'; /* ; UPDATE ignored */ SELECT * FROM orders",
        "orders",
        "queries_table",
    ),
    (
        "BEGIN; -- ; UPDATE ignored\nWITH rows AS (SELECT * FROM orders) SELECT * FROM rows",
        "orders",
        "queries_table",
    ),
    (
        "SET x=$tag$; UPDATE ignored$tag$; SELECT * FROM orders",
        "orders",
        "queries_table",
    ),
    ("DELETE FROM orders", "orders", "mutates_table"),
    (
        "CREATE TABLE IF NOT EXISTS orders (id INT)",
        "orders",
        "defines_table",
    ),
];

use acdd_index::ast;

#[test]
fn sql_profile_parses_without_tree_sitter_parser() {
    assert!(ast::parser("sql", "schema.sql").is_err());
    let mut public_facts = acdd_index::models::Facts::default();
    acdd_index::languages::parse_file(
        &acdd_index::languages::sql::SQL,
        "public.sql",
        "CREATE TABLE public_test (id INT)",
        "public",
        &mut public_facts,
    )
    .unwrap();
    assert!(public_facts
        .nodes
        .iter()
        .any(|node| node.name == "public_test"));
}

#[test]
fn sql_statement_relation_matrix() {
    let mut cases: Vec<_> = TABLE_RELATION_CASES
        .iter()
        .map(|&(sql, name, relation)| (sql, vec![(name.to_owned(), relation)], false))
        .collect();
    cases.extend([
        ("WITH temporary(id) AS (SELECT * FROM temporary) SELECT * FROM temporary", vec!["temporary"]),
        ("WITH RECURSIVE temporary(id) AS (SELECT * FROM temporary) SELECT * FROM temporary", vec![]),
        ("WITH temporary(id) AS (SELECT * FROM orders) SELECT * FROM public.temporary", vec!["orders", "public.temporary"]),
        ("WITH first_cte AS (SELECT * FROM orders), second_cte(id) AS (SELECT * FROM first_cte) SELECT * FROM second_cte", vec!["orders"]),
    ].into_iter().map(|(sql, names)| (sql, names.into_iter().map(|name| (name.to_owned(), "queries_table")).collect(), true)));
    for (sql, expected, exact) in cases {
        let actual = acdd_index::languages::sql::extract_embedded_sql_tables(sql);
        if exact {
            assert_eq!(actual, expected, "{sql}");
        } else {
            for relation in expected {
                assert!(actual.contains(&relation), "{sql}: {actual:?}");
            }
        }
    }
}

#[test]
fn language_literal_producers_ignore_prose_and_embedded_delimiters() {
    let mut embedded = vec![(
        "embedded.rs",
        "rust",
        r##"fn query() { let _ = r#"SET x=$tag$; UPDATE ignored$tag$; /* ; */ WITH rows AS (SELECT * FROM orders) SELECT * FROM rows"#; }
fn prose() { let _ = "information was updated and transformed"; }"##,
    )];
    #[cfg(feature = "lang-python")]
    embedded.push((
        "embedded.py",
        "python",
        "def query():\n    return \"SET x=$tag$; UPDATE ignored$tag$; /* ; */ WITH rows AS (SELECT * FROM orders) SELECT * FROM rows\"\ndef prose():\n    '''information was updated and transformed'''\n",
    ));
    #[cfg(feature = "lang-typescript")]
    embedded.push((
        "embedded.ts",
        "typescript",
        "function query() { return `SET x=$tag$; UPDATE ignored$tag$; /* ; */ WITH rows AS (SELECT * FROM orders) SELECT * FROM rows`; }\nfunction prose() { return 'information was updated and transformed'; }\n",
    ));
    for (path, language, source) in embedded {
        let facts = ExtractorHarness::for_lang(language)
            .at_path(path)
            .extract(source);
        let queries: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == "queries_table")
            .collect();
        assert_eq!(queries.len(), 1, "{path}: {queries:?}");
        assert_eq!(queries[0].expression, "orders", "{path}");
        assert!(
            !facts.references.iter().any(|r| r.kind == "mutates_table"),
            "{path}"
        );
    }
}

fn indexed_sql_workspace() -> crate::languages::PersistedWorkspaceHarness {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write("more_tables.sql", "CREATE TABLE inventory (id INTEGER); CREATE TABLE factory_orders (id INTEGER); CREATE TABLE temporary (id INTEGER);");
    workspace.write(
        "quoted_schema.sql",
        "CREATE TABLE \"quoted_table\" (id INTEGER);",
    );
    workspace.write(
        "case_schema.sql",
        "CREATE TABLE \"Foo\" (id INTEGER); CREATE TABLE \"foo\" (id INTEGER);",
    );
    workspace.write(
        "Cargo.toml",
        "[package]\nname = 'fixture'\nversion = '0.1.0'\n[dependencies]\nsqlx = '0.8'\n",
    );
    workspace.write("schema.sql", "CREATE TABLE orders (id INTEGER PRIMARY KEY);\nCREATE TABLE customers (id INTEGER);\nCREATE TABLE shop_cart (id INTEGER);\nCREATE TABLE shop_fake (id INTEGER);\nCREATE TABLE shop_override (id INTEGER);\nCREATE TABLE item (id INTEGER, order_id INTEGER REFERENCES orders(id) ON UPDATE CASCADE, PRIMARY KEY (id));\nCREATE TABLE a.same (id INTEGER); CREATE TABLE b.same (id INTEGER); CREATE TABLE \"x.y\" (id INT); CREATE TABLE x.y (id INT); CREATE TABLE SET (id INT); CREATE TABLE CASCADE (id INT);\nCREATE VIEW order_view AS SELECT * FROM orders;\n");
    workspace.write(
        "extra_schema.sql",
        "CREATE TABLE shop_default (id INTEGER);",
    );
    workspace.write(
        "migration.sql",
        "ALTER TABLE orders ADD COLUMN note TEXT;\n",
    );
    workspace.write("queries.rs", "#[derive(sqlx::FromRow)] struct OrderRow { id: i64 }\nfn load() { let _ = \"SELECT * FROM orders\"; }\nfn save() { let _ = \"INSERT INTO orders (id) VALUES (1)\"; }\nfn remove() { let _ = \"DELETE FROM orders WHERE id = 1\"; }\nfn customers() { let _ = \"SELECT * FROM customers\"; }\nfn macro_load() { let _ = sqlx::query!(\"SELECT * FROM orders\"); }\nfn typed_load() { let _ = sqlx::query_as!(OrderRow, \"SELECT * FROM orders\"); }\nfn inline_schema() { let _ = \"CREATE TABLE local_data (id INTEGER)\"; }\n");
    workspace.write("foreign.rs", "struct Imported;\nuse external::Imported;\nfn foreign() { let _ = sqlx::query_as!(Imported, \"SELECT * FROM orders\"); }\n");
    workspace.write("qualified.rs", "fn exact() { let _ = \"SELECT * FROM a.same\"; }\nfn ambiguous_short() { let _ = \"SELECT * FROM same\"; }\nfn batch() { let _ = \"BEGIN; PRAGMA x=1; SELECT * FROM orders, customers\"; }\nfn inline_create() { let _ = \"CREATE TABLE c.inline_qual (id INT)\"; }\nfn inline_query() { let _ = \"SELECT * FROM inline_qual\"; }\n");
    workspace.write("upsert.rs", "fn upsert() { let _ = \"INSERT INTO orders VALUES (1) ON CONFLICT DO UPDATE SET id=1\"; }\n");
    workspace.write("patterns.rs", "fn rollback_update() { let _ = \"ROLLBACK; UPDATE ONLY orders AS o SET id=1\"; }\nfn mixed_from() { let _ = \"SELECT * FROM orders o JOIN customers c ON c.id=o.id, inventory i\"; }\nfn cte_query() { let _ = \"WITH temporary AS (SELECT * FROM orders) SELECT * FROM temporary JOIN inventory ON 1=1\"; }\n");
    #[cfg(feature = "lang-python")]
    workspace.write("identifiers.py", "def quoted_orders():\n    return 'SELECT * FROM \"orders\"'\ndef unquoted_table():\n    return 'SELECT * FROM quoted_table'\ndef cte_columns():\n    return 'WITH temporary(id) AS (SELECT id FROM orders) SELECT * FROM temporary'\n");
    workspace.write("ambiguous.rs", "mod a { struct Shared; }\nmod b { struct Shared; }\nfn ambiguous() { let _ = sqlx::query_as!(Shared, \"SELECT * FROM orders\"); }\n");
    workspace.write("shadow.rs", "mod sqlx {}\nstruct Shadow;\nfn shadow() { let _ = sqlx::query_as!(Shadow, \"SELECT * FROM orders\"); }\n");
    workspace.write("other.rs", "struct Other;\nfn other() { let _ = custom::query_as!(Other, \"SELECT * FROM orders\"); }\n");
    workspace.write(
        "derive_only.rs",
        "#[derive(sqlx::FromRow)] struct DeriveOnly { id: i64 }\n",
    );
    #[cfg(feature = "lang-python")]
    workspace.write("queries.py", "from django.db import models\nfrom sqlalchemy.orm import DeclarativeBase\ndef py_load():\n    return \"SELECT * FROM orders\"\n\nclass Base(DeclarativeBase):\n    pass\nclass Order(Base):\n    __tablename__ = 'orders'\n\nclass FalseSqlAlchemy:\n    __tablename__ = 'orders'\n\nclass Item(models.Model):\n    class Meta:\n        db_table = 'orders'\n");
    #[cfg(feature = "lang-python")]
    workspace.write("quoted.py", "def quoted():\n    return 'SELECT * FROM \"x.y\"'\ndef qualified():\n    return 'SELECT * FROM x.y'\n");
    #[cfg(feature = "lang-python")]
    workspace.write("case_query.py", "def upper_case():\n    return 'SELECT * FROM \"Foo\"'\ndef lower_case():\n    return 'SELECT * FROM \"foo\"'\n");
    #[cfg(feature = "lang-python")]
    workspace.write("factory.py", "from sqlalchemy.orm import declarative_base as make_base\nBase = make_base()\nclass FactoryOrder(Base):\n    __tablename__ = 'factory_orders'\n");
    #[cfg(feature = "lang-python")]
    workspace.write("factory_qualified.py", "import sqlalchemy.orm as orm\nBase = orm.declarative_base()\nclass QualifiedFactory(Base):\n    __tablename__ = 'factory_orders'\n");
    #[cfg(feature = "lang-python")]
    workspace.write("factory_invalid.py", "from sqlalchemy.orm import declarative_base as make_base\nmake_base = object()\nBase = make_base()\nclass ReboundFactory(Base):\n    __tablename__ = 'factory_orders'\n");
    #[cfg(feature = "lang-python")]
    workspace.write(".acdd/frameworks/other-tables.toml", "[[semantic_rules]]\nid = 'other-table'\nnamespace = 'other.tables'\nscope = 'project'\noperation = { kind = 'class_attribute_to_table', language = 'python', attribute = 'table_name', container = 'Config' }\n");
    #[cfg(feature = "lang-python")]
    workspace.write(".acdd/frameworks/standalone-default.toml", "[[semantic_rules]]\nid = 'default'\nnamespace = 'other.default'\nscope = 'project'\noperation = { kind = 'class_default_table', language = 'python', bases = ['fixture.CustomBase'] }\n");
    #[cfg(feature = "lang-python")]
    workspace.write(
        "shop/default.py",
        "# class Commented(CustomBase): pass\n\"\"\"class Quoted(CustomBase): pass\"\"\"\nfrom fixture import CustomBase\nclass Empty:\n    pass\nclass Default(CustomBase):\n    pass\n",
    );
    #[cfg(feature = "lang-python")]
    workspace.write(
        "other_model.py",
        "class Generic:\n    class Config:\n        table_name = 'orders'\n",
    );
    #[cfg(feature = "lang-python")]
    workspace.write(
        "pyproject.toml",
        "[project]\nname = 'shop'\nversion = '0.1'\ndependencies = ['django', 'sqlalchemy']\n",
    );
    #[cfg(feature = "lang-python")]
    workspace.write(
        "shop/models.py",
        "from django.db import models as dbmodels\nclass Cart(dbmodels.Model):\n    pass\nclass Override(dbmodels.Model):\n    class Meta:\n        db_table = 'orders'\nclass NonModel:\n    class Meta:\n        db_table = 'orders'\n",
    );
    #[cfg(feature = "lang-python")]
    workspace.write("shop/reassigned.py", "from django.db import models as dbmodels\ndbmodels = object()\nclass Reassigned(dbmodels.Model):\n    class Meta:\n        db_table = 'orders'\n");
    #[cfg(feature = "lang-python")]
    workspace.write(
        "shop/fake.py",
        "from local import models\nclass Fake(models.Model):\n    pass\n",
    );
    #[cfg(feature = "lang-typescript")]
    workspace.write(
        "queries.ts",
        "function ts_load() { return \"SELECT * FROM orders\"; }\n",
    );
    workspace.build();
    workspace
}

#[test]
fn persisted_sql_relation_matrix() {
    let db = sql_baseline().open();
    let mut cases: Vec<(&str, Vec<&str>, i64)> = Vec::new();
    #[cfg(feature = "lang-python")]
    for (source, target, expected) in [
        ("quoted_orders", "orders", 1),
        ("unquoted_table", "quoted_table", 1),
        ("cte_columns", "temporary", 0),
        ("cte_columns", "orders", 1),
    ] {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.name=?2 AND e.kind='queries_table'", vec![source,target], expected));
    }
    #[cfg(feature = "lang-python")]
    for (source, target, expected) in [
        ("upper_case", "\"Foo\"", 1),
        ("upper_case", "\"foo\"", 0),
        ("lower_case", "\"foo\"", 1),
        ("lower_case", "\"Foo\"", 0),
    ] {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.qualname=?2 AND e.kind='queries_table'", vec![source,target], expected));
    }
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='cte_query' AND d.name='temporary' AND e.kind='queries_table'", vec![], 0));
    cases.push((
        "SELECT count(*) FROM nodes WHERE kind='table' AND qualname IN ('a.same','b.same')",
        vec![],
        2,
    ));
    cases.push((
        "SELECT count(*) FROM nodes WHERE kind='table' AND qualname IN ('\"x.y\"','x.y')",
        vec![],
        2,
    ));
    #[cfg(feature = "lang-python")]
    for (source, target, expected) in [
        ("quoted", "\"x.y\"", 1),
        ("quoted", "x.y", 0),
        ("qualified", "x.y", 1),
        ("qualified", "\"x.y\"", 0),
    ] {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.qualname=?2 AND e.kind='queries_table'", vec![source,target], expected));
    }
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name IN ('upsert','item') AND d.name IN ('SET','CASCADE') AND e.kind='mutates_table'", vec![], 0));
    for (source, target, expected) in [
        ("exact", "a.same", 1),
        ("exact", "b.same", 0),
        ("ambiguous_short", "a.same", 0),
        ("ambiguous_short", "b.same", 0),
        ("batch", "customers", 1),
        ("batch", "orders", 1),
        ("inline_query", "c.inline_qual", 1),
        ("mixed_from", "inventory", 1),
    ] {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.qualname=?2 AND e.kind='queries_table'", vec![source,target], expected));
    }
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='rollback_update' AND d.name='orders' AND e.kind='mutates_table'", vec![], 1));
    cases.push((
        "SELECT count(*) FROM nodes WHERE kind = 'table' AND name = 'orders'",
        vec![],
        1,
    ));
    cases.push((
        "SELECT count(*) FROM nodes WHERE kind='table' AND name='local_data'",
        vec![],
        1,
    ));
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='inline_schema' AND d.name='local_data' AND e.kind='defines_table'", vec![], 1));
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='OrderRow' AND d.name='orders' AND e.kind='queries_table'", vec![], 1));
    cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name IN ('Shared','Imported','Shadow','Other','DeriveOnly') AND e.kind='queries_table'", vec![], 0));
    let mut expected = vec![
        ("order_view", "queries_table"),
        ("load", "queries_table"),
        ("save", "mutates_table"),
        ("remove", "mutates_table"),
        ("macro_load", "queries_table"),
        ("typed_load", "queries_table"),
        ("item", "references_table"),
    ];
    #[cfg(feature = "lang-python")]
    expected.push(("py_load", "queries_table"));
    #[cfg(feature = "lang-python")]
    expected.extend([
        ("Order", "defines_table"),
        ("Item", "defines_table"),
        ("Generic", "defines_table"),
    ]);
    #[cfg(feature = "lang-typescript")]
    expected.push(("ts_load", "queries_table"));
    for (source, kind) in expected {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.name='orders' AND e.kind=?2", vec![source, kind], 1));
    }
    cases.push(("SELECT count(*) FROM nodes WHERE kind='column' AND qualname IN ('orders.id', 'item.order_id')", vec![], 2));
    cases.push(("SELECT count(*) FROM nodes WHERE kind='column' AND qualname='orders.note' AND path_id=(SELECT path_id FROM path_dictionary WHERE path='migration.sql')", vec![], 1));
    for (query, parameters, expected) in cases {
        let actual: i64 = db
            .query_row(query, rusqlite::params_from_iter(&parameters), |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(actual, expected, "{query}: {parameters:?}");
    }
}

#[test]
fn orm_model_provenance_matrix() {
    let db = sql_baseline().open();
    let mut cases: Vec<(&str, Vec<&str>, i64)> = Vec::new();
    #[cfg(feature = "lang-python")]
    {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='FactoryOrder' AND d.name='factory_orders' AND e.kind='defines_table'", vec![], 1));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name='QualifiedFactory' AND e.kind='defines_table'", vec![], 1));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name='ReboundFactory' AND e.kind='defines_table'", vec![], 0));
    }
    #[cfg(feature = "lang-python")]
    {
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='Cart' AND d.name='shop_cart' AND e.kind='defines_table'", vec![], 1));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='Default' AND d.name='shop_default' AND e.kind='defines_table'", vec![], 1));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name='Empty' AND e.kind='defines_table'", vec![], 0));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='Fake' AND d.name='shop_fake' AND e.kind='defines_table'", vec![], 0));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='Override' AND d.name='shop_override' AND e.kind='defines_table'", vec![], 0));
        cases.push(("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name IN ('NonModel','Reassigned','FalseSqlAlchemy') AND e.kind='defines_table'", vec![], 0));
    }
    for (query, parameters, expected) in cases {
        let actual: i64 = db
            .query_row(query, rusqlite::params_from_iter(&parameters), |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(actual, expected, "{query}: {parameters:?}");
    }
}

#[test]
fn sql_impact_traversal_reaches_views_and_language_consumers() {
    let db = sql_baseline().open();
    let page = acdd_core::response::QueryOptions::resolve(
        &acdd_core::response::ResponsePolicy::default(),
        Some(100),
        0,
        None,
        None,
    )
    .unwrap();
    let impact = acdd_index::db::traversal::traverse_with_options(
        &db,
        "schema.sql:table:orders",
        &acdd_index::db::traversal::TraversalOptions {
            depth: 1,
            inbound: true,
            mode: None,
            edge_types: None,
            page: &page,
        },
    )
    .unwrap();
    let reached = impact["nodes"]["items"].as_array().unwrap();
    assert!(
        reached.iter().any(|node| node["name"] == "order_view"),
        "SQL impact must include views: {impact}"
    );
    for name in ["load", "save", "remove", "item"] {
        assert!(
            reached.iter().any(|node| node["name"] == name),
            "SQL impact must include {name}: {impact}"
        );
    }
    #[cfg(feature = "lang-python")]
    for name in ["Order", "Item", "py_load"] {
        assert!(
            reached.iter().any(|node| node["name"] == name),
            "SQL impact must include {name}: {impact}"
        );
    }
}

#[test]
fn schema_delta_matches_independent_fresh_cold_build() {
    let workspace = indexed_sql_workspace();
    workspace.write("schema.sql", "CREATE TABLE orders (id INTEGER PRIMARY KEY);\nCREATE TABLE shop_cart (id INTEGER);\nCREATE TABLE shop_fake (id INTEGER);\nCREATE TABLE item (id INTEGER, order_id INTEGER REFERENCES orders(id), PRIMARY KEY (id));\nCREATE VIEW order_view AS SELECT * FROM orders;\nCREATE TABLE customers (id INTEGER);\n");
    let parity = super::support::delta_cold(&workspace, "schema.sql");
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    let cold = parity.cold_connection;
    let delta = parity.incremental_connection;
    let root = |conn: &rusqlite::Connection| -> String {
        conn.query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        root(&delta),
        root(&cold),
        "SQL cold and delta commitments must agree"
    );
}

#[test]
fn undeclared_sqlx_and_sqlalchemy_packs_do_not_activate_model_rules() {
    let without_sqlx = crate::languages::PersistedWorkspaceHarness::new();
    without_sqlx.write("schema.sql", "CREATE TABLE orders (id INTEGER);\n");
    #[cfg(feature = "lang-python")]
    without_sqlx.write("inactive.py", "from sqlalchemy.orm import DeclarativeBase\nclass Inactive(DeclarativeBase):\n    __tablename__ = 'orders'\n");
    #[cfg(feature = "lang-python")]
    without_sqlx.write("inactive_factory.py", "from sqlalchemy.orm import declarative_base\nBase = declarative_base()\nclass InactiveFactory(Base):\n    __tablename__ = 'orders'\n");
    without_sqlx.write("query.rs", "struct NotActive;\nfn load() { let _ = sqlx::query_as!(NotActive, \"SELECT * FROM orders\"); }\n");
    without_sqlx.build();
    let db = without_sqlx.open();
    let edges: i64 = db.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name='NotActive' AND e.kind='queries_table'", [], |row| row.get(0)).unwrap();
    assert_eq!(edges, 0, "undeclared SQLx must not activate its model rule");
    #[cfg(feature = "lang-python")]
    {
        let inactive: i64 = db.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash WHERE s.name IN ('Inactive','InactiveFactory') AND e.kind='defines_table'", [], |row| row.get(0)).unwrap();
        assert_eq!(
            inactive, 0,
            "undeclared SQLAlchemy must not activate its model rule"
        );
    }
}

fn sql_baseline() -> &'static PersistedSnapshot {
    static BASELINE: std::sync::OnceLock<PersistedSnapshot> = std::sync::OnceLock::new();
    BASELINE.get_or_init(|| {
        let workspace = indexed_sql_workspace();
        PersistedSnapshot::from_connection(&workspace.open())
    })
}

#[test]
fn persisted_sql_query_snapshots_isolate_mutations_from_other_readers_and_seed() {
    let changed = sql_baseline().open();
    changed
        .execute(
            "UPDATE metadata SET value=?1 WHERE key=?2",
            rusqlite::params!["changed-copy", "output_root"],
        )
        .unwrap();
    let untouched = sql_baseline().open();
    let output_root = |conn: &rusqlite::Connection| -> String {
        conn.query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let expected = output_root(&untouched);
    assert_eq!(output_root(&changed), "changed-copy");
    assert_eq!(output_root(&untouched), expected);
    assert_eq!(output_root(&sql_baseline().open()), expected);
}
