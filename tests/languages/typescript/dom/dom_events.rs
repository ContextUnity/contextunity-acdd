use super::*;
#[test]
fn dom_callback_receiver_authority() {
    let cases: &[(&str, &str, &[&str])] = &[
        (            "function run(document: unknown) { document.querySelectorAll('x').forEach(e => e.querySelector('.x')); }",
 "unresolved", &["e.querySelector"]),
        (            "function run() { document.querySelectorAll('x').forEach(e => e.querySelector('.x')); }",
 "external", &["e.querySelector"]),
        (            "function run(root: Element) { root.querySelectorAll('x').forEach(e => e.querySelector('.x')); }",
 "external", &["e.querySelector"]),
        (            "function run(root: Element) { root.\u{00a0}querySelectorAll('x').forEach(e => e.querySelector('.x')); }",
 "external", &["e.querySelector"]),
        (            "document.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); });",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "const button = document.getElementById('submit'); const values = [1, 2].map(value => value + 1); button.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); });",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "function run(document) { document.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "function run(target) { target.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "function run(target: Element) { target.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); }",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "function run(target: Element) { target.\u{00a0}addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); }",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class Element { addEventListener() {} } function run(target: Element) { target.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class Event { preventDefault() {} stopPropagation() {} } document.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); });",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "document = {}; document.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); });",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class View { target: HTMLInputElement; constructor(target: HTMLInputElement) { this.target = target; } bind() { this.target.addEventListener('input', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } }",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class View { target: HTMLDivElement; bind() { this.target.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } }",
 "external", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class HTMLInputElement { addEventListener() {} } class View { target: HTMLInputElement; bind() { this.target.addEventListener('input', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class View { target: unknown; bind() { this.target.addEventListener('input', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class View { target: HTMLInputElement; static bind() { this.target.addEventListener('input', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "class View { target: HTMLInputElement; bind() { function nested() { this.target.addEventListener('input', event => { event.preventDefault(); event.stopPropagation(); event.target?.addEventListener('focus', () => {}); }); } } }",
 "unresolved", &["event.preventDefault", "event.stopPropagation", "event.target?.addEventListener"]),
        (            "function bind() { const gridDiv = document.getElementById('grid'); if (!gridDiv) return; const wrapper = gridDiv.querySelector<HTMLElement>('.header'); if (!wrapper) return; wrapper.addEventListener('click', (e) => { e.stopPropagation(); }); }",
 "external", &["e.stopPropagation"]),
        (            "function bind() { const gridDiv = unknown(); const wrapper = gridDiv.querySelector<HTMLElement>('.header'); wrapper.addEventListener('click', (e) => { e.stopPropagation(); }); }",
 "unresolved", &["e.stopPropagation"]),
        (            "function bind() { let gridDiv = document.getElementById('grid'); gridDiv = unknown(); const wrapper = gridDiv.querySelector<HTMLElement>('.header'); wrapper.addEventListener('click', (e) => { e.stopPropagation(); }); }",
 "unresolved", &["e.stopPropagation"]),
("function run(document) { document.on\\u0063lick = event => event.preventDefault(); } // require", "unresolved", &["event.preventDefault"]),
    ];
    for &(source, expected, expressions) in cases {
        let graph = linked("callback.ts", "typescript", source);
        for &expression in expressions {
            let rows: Vec<_> = graph
                .coverage
                .iter()
                .filter(|row| row.expression == expression)
                .collect();
            assert!(
                !rows.is_empty() && rows.iter().all(|row| row.status == expected),
                "{source}: {expression}: {graph:#?}"
            );
            if expected == "external" {
                assert!(
                    rows.iter()
                        .all(|row| row.evidence.contains("builtin:web_api")),
                    "{rows:#?}"
                );
            }
        }
    }
}

#[test]
fn dom_platform_event_and_storage_types() {
    let source = r#"
function runUrl() {
    const url = new URL("https://example.com");
    url.searchParams.set("q", "test");
    url.searchParams.delete("q");
}

function runEvent(btn: Element) {
    btn.onclick = (e) => {
        e.stopPropagation();
        e.preventDefault();
    };
}

function runNullable(toolbar: Element | null, el: HTMLElement) {
    toolbar.querySelectorAll("input");
    el.classList.add("active");
    el.classList.remove("active");
}

class Panel {
    modeCheckboxes: HTMLInputElement[];
    constructor() {
        this.modeCheckboxes = [];
    }
    setup() {
        this.modeCheckboxes.forEach((checkbox) => {
            checkbox.addEventListener("click", (e) => {
                e.stopPropagation();
            });
        });
    }
}
"#;
    let graph = linked("test.ts", "typescript", source);

    for expression in [
        "url.searchParams.set",
        "url.searchParams.delete",
        "e.stopPropagation",
        "e.preventDefault",
        "toolbar.querySelectorAll",
        "el.classList.add",
        "el.classList.remove",
        "checkbox.addEventListener",
    ] {
        let matches: Vec<_> = graph
            .coverage
            .iter()
            .filter(|row| row.expression == expression)
            .collect();
        assert!(
            !matches.is_empty(),
            "expected coverage entry for {expression}"
        );
        for row in matches {
            assert_eq!(
                row.status, "external",
                "expected external status for {expression}, got {}: {row:#?}",
                row.status
            );
            assert!(
                row.evidence.contains("builtin:web_api"),
                "expected builtin:web_api evidence for {expression}, got {}",
                row.evidence
            );
        }
    }

    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    let source = r#"
function handleDrag(e: DragEvent, target: ParentNode, store: Storage) {
  e.preventDefault();
  target.querySelector(".item");
  store.getItem("token");
}

function identity<T>(value: T): T {
  return value;
}
"#;
    workspace.write("drag.ts", source);
    let conn = workspace.build_and_open();

    for expression in ["DragEvent", "ParentNode", "Storage"] {
        let (status, evidence): (String, String) = conn.query_row(
            "SELECT status,(SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='drag.ts' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?1",
            rusqlite::params![expression],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(status, "external", "{expression} should be external");
        assert!(
            evidence.contains("builtin:web_api"),
            "{expression} should have builtin:web_api"
        );
    }

    for expression in ["e.preventDefault", "target.querySelector", "store.getItem"] {
        let (status, evidence): (String, String) = conn.query_row(
            "SELECT status,(SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='drag.ts' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?1",
            rusqlite::params![expression],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(status, "external", "{expression} should be external");
        assert!(
            evidence.contains("builtin:web_api"),
            "{expression} should have builtin:web_api"
        );
    }

    let count: i64 = conn.query_row(
        "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='drag.ts' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='T'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(
        count, 0,
        "naked generic parameter T should have 0 coverage rows"
    );

    drop(conn);
}

#[test]
fn dom_factory_hierarchy_members() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write(
        "dom.ts",
        r#"
function use(target: ParentNode, child: ChildNode, fragment: DocumentFragment) {
  target.querySelector(".item");
  target.setAttribute("id", "x");
  target.classList.add("on");
  child.querySelector(".item");
  child.removeChild(child);
  fragment.querySelector(".item");
  fragment.closest(".item");
}
"#,
    );
    for (path, source) in [
        (
            "dom.js",
            "function run() { const element = document.querySelector('#root'); element.addEventListener('click', () => {}); const child = element.querySelector('.child'); child.getAttribute('id'); }",
        ),
        (
            "fetch.js",
            "async function run() { const response = await fetch('/api'); response.json(); response.text(); }",
        ),
        (
            "fetch_bad_property_call.js",
            "async function run() { const response = await fetch('/api'); response.status(); }",
        ),
        (
            "dom_shadow.js",
            "function run(document) { const element = document.querySelector('#root'); element.addEventListener('click', () => {}); }",
        ),
        (
            "dom_reassigned.js",
            "function run() { document = {}; const element = document.querySelector('#root'); element.addEventListener('click', () => {}); }",
        ),
        (
            "fetch_shadow.js",
            "async function run(fetch) { const response = await fetch('/api'); response.json(); }",
        ),
        (
            "fetch_local_function.js",
            "async function fetch() { return {}; } async function run() { const response = await fetch('/api'); response.json(); }",
        ),
        (
            "fetch_import.js",
            "import { fetch } from './transport.js'; async function run() { const response = await fetch('/api'); response.json(); }",
        ),
        ("transport.js", "export async function fetch() { return {}; }"),
        (
            "fetch_rebound.js",
            "async function run() { fetch = async () => ({}); const response = await fetch('/api'); response.json(); }",
        ),
        (
            "fetch_plain.js",
            "function run() { const pending = fetch('/api'); pending.json(); }",
        ),
        (
            "fetch_reassigned.js",
            "async function run() { let response = await fetch('/api'); response = {}; response.json(); }",
        ),
        (
            "fetch_unknown.js",
            "async function run() { const response = await fetch('/api'); response.unsupported(); }",
        ),
        (
            "fetch_local.js",
            "class Response { json() {} } async function run() { const response = new Response(); response.json(); }",
        ),
        (
            "nested_page.js",
            "function outer() { const page = {}; function inner() { page.locator('button'); } inner(); }",
        ),
        (
            "nested_page_late.js",
            "function outer() { function inner() { page.locator('button'); } const page = {}; inner(); }",
        ),
    ] {
        w.write(path, source);
    }
    w.build();
    let conn = w.open();
    {
        let coverage = |expression: &str| -> String {
            conn.query_row(
            "SELECT status FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='dom.ts' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?1",
            rusqlite::params![expression],
            |row| row.get(0),
        )
        .unwrap()
        };
        assert_eq!(coverage("target.querySelector"), "external");
        assert_eq!(coverage("target.setAttribute"), "unresolved");
        assert_eq!(coverage("target.classList.add"), "unresolved");
        assert_eq!(coverage("child.querySelector"), "unresolved");
        assert_eq!(coverage("child.removeChild"), "external");
        assert_eq!(coverage("fragment.querySelector"), "external");
        assert_eq!(coverage("fragment.closest"), "unresolved");
    }
    {
        let coverage = |path: &str, expression: &str| -> (String, String) {
            conn.query_row(
            "SELECT status, (SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)=?1 AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?2",
            rusqlite::params![path, expression],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap_or_else(|error| panic!("missing {path}:{expression}: {error}"))
        };
        for (path, expression) in [
            ("dom.js", "element.addEventListener"),
            ("dom.js", "child.getAttribute"),
            ("fetch.js", "response.json"),
            ("fetch.js", "response.text"),
        ] {
            let row = coverage(path, expression);
            assert_eq!(row.0, "external", "{path}:{expression}: {row:?}");
            assert!(
                row.1.contains("builtin:web_api"),
                "{path}:{expression}: {row:?}"
            );
        }
        for (path, expression) in [
            ("dom_shadow.js", "element.addEventListener"),
            ("dom_reassigned.js", "element.addEventListener"),
            ("fetch_shadow.js", "response.json"),
            ("fetch_local_function.js", "response.json"),
            ("fetch_import.js", "response.json"),
            ("fetch_rebound.js", "response.json"),
            ("fetch_plain.js", "pending.json"),
            ("fetch_reassigned.js", "response.json"),
            ("fetch_unknown.js", "response.unsupported"),
            ("fetch_bad_property_call.js", "response.status"),
            ("nested_page.js", "page.locator"),
            ("nested_page_late.js", "page.locator"),
        ] {
            let row = coverage(path, expression);
            assert_eq!(row.0, "unresolved", "{path}:{expression}: {row:?}");
        }
        assert_eq!(coverage("fetch_local.js", "response.json").0, "resolved");
    }
}

#[test]
fn playwright_fixtures_and_webidl_interfaces_contract_matrix() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write(
        "e2e/package.json",
        r#"{"devDependencies":{"@playwright/test":"^1.49.0"}}"#,
    );
    w.write(
        "e2e/checkout.spec.ts",
        r#"
import { test, expect } from '@playwright/test';

test.beforeEach(async ({ page: currentPage }) => {
  await currentPage.goto('/checkout');
});

test('completes order', async ({ page, request, context, browser }) => {
  await page.locator('button');
  await page.keyboard.press('Enter');
  await page.mouse.click(10, 20);
  await request.get('/api/orders');
  await context.newPage();
  await browser.close();
  await expect(page.locator('h1')).toBeVisible();
});
"#,
    );
    w.write(
        "plain/checkout.spec.ts",
        r#"
function test(_name: string, _fn: (args: { page: any }) => void) {}
test('plain', ({ page }) => {
  page.locator('button');
});
"#,
    );
    w.write(
        "webidl/dom_extended.ts",
        r#"
async function probe(el: Element, resp: Response) {
  const rect = el.getBoundingClientRect();
  rect.top;
  await navigator.clipboard.writeText('ok');
  navigator.onLine;
  location.hash;
  location.reload();
  performance.getEntries();
  await crypto.subtle.generateKey('AES-GCM', true, ['encrypt']);
  const ws = new WebSocket('wss://example.com');
  ws.send('hi');
  const form = new FormData();
  form.append('k', 'v');
  const controller = new AbortController();
  controller.abort();
  const cloned = resp.clone();
  await cloned.text();
}
"#,
    );
    w.build();
    let conn = w.open();
    let coverage = |path: &str, expression: &str| -> String {
        conn.query_row(
            "SELECT status FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)=?1 AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?2",
            rusqlite::params![path, expression],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("missing {path}:{expression}: {error}"))
    };

    for (path, expression, expected) in [
        ("e2e/checkout.spec.ts", "currentPage.goto", "external"),
        ("e2e/checkout.spec.ts", "page.locator", "external"),
        ("e2e/checkout.spec.ts", "page.keyboard.press", "external"),
        ("e2e/checkout.spec.ts", "page.mouse.click", "external"),
        ("e2e/checkout.spec.ts", "request.get", "external"),
        ("e2e/checkout.spec.ts", "context.newPage", "external"),
        ("e2e/checkout.spec.ts", "browser.close", "external"),
        ("plain/checkout.spec.ts", "page.locator", "unresolved"),
        ("webidl/dom_extended.ts", "el.getBoundingClientRect", "external"),
        ("webidl/dom_extended.ts", "rect.top", "resolved"),
        ("webidl/dom_extended.ts", "navigator.clipboard.writeText", "external"),
        ("webidl/dom_extended.ts", "navigator.onLine", "external"),
        ("webidl/dom_extended.ts", "location.hash", "external"),
        ("webidl/dom_extended.ts", "location.reload", "external"),
        ("webidl/dom_extended.ts", "performance.getEntries", "external"),
        ("webidl/dom_extended.ts", "crypto.subtle.generateKey", "external"),
        ("webidl/dom_extended.ts", "ws.send", "external"),
        ("webidl/dom_extended.ts", "form.append", "external"),
        ("webidl/dom_extended.ts", "controller.abort", "external"),
        ("webidl/dom_extended.ts", "resp.clone", "external"),
        ("webidl/dom_extended.ts", "cloned.text", "external"),
    ] {
        assert_eq!(coverage(path, expression), expected, "{path}:{expression}");
    }
    acdd_index::db::commitments::verify(&conn).unwrap();
}

#[test]
fn playwright_and_webidl_compound_return_chains() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write(
        "e2e/package.json",
        r#"{"devDependencies":{"@playwright/test":"^1.49.0"}}"#,
    );
    w.write(
        "e2e/chain.spec.ts",
        r#"
import { test, expect } from '@playwright/test';

test('compound playwright and webidl chains', async ({ page, request }) => {
  await page.locator('button').filter({ hasText: 'Go' }).first().click();
  const resp = await request.get('/api/users');
  await resp.json();
  await expect(page.locator('h1')).not.toBeVisible();
  const fetched = await fetch('/api/blob');
  const data = await fetched.blob();
  data.slice(0, 4).stream().getReader();
  const box = document.querySelector('.box');
  box.getBoundingClientRect().top;
});
"#,
    );
    w.build();
    let conn = w.open();
    for (expression, expected) in [
        ("page.locator('button').filter", "external"),
        ("page.locator('button').filter({ hasText: 'Go' }).first", "external"),
        ("page.locator('button').filter({ hasText: 'Go' }).first().click", "external"),
        ("resp.json", "external"),
        ("expect(page.locator('h1')).not.toBeVisible", "external"),
        ("fetched.blob", "external"),
        ("data.slice(0, 4).stream", "external"),
        ("data.slice(0, 4).stream().getReader", "external"),
        ("box.getBoundingClientRect", "external"),
        ("box.getBoundingClientRect().top", "external"),
    ] {
        let (status, evidence): (String, String) = conn
            .query_row(
                "SELECT c.status, v.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence v ON v.evidence_id=c.evidence_id WHERE p.path='e2e/chain.spec.ts' AND x.expression=?1",
                [expression],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("missing {expression}: {e}"));
        assert_eq!(status, expected, "chain.spec.ts: {expression} ({evidence})");
    }
}
