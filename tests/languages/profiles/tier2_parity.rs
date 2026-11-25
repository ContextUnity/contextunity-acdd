use super::*;

struct ParityCase {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    expected_edges: &'static [(&'static str, &'static str, &'static str, &'static str)],
    expected_coverage: &'static [(&'static str, &'static str, &'static str)],
    expected_concrete_methods: &'static [(&'static str, &'static str, &'static str)],
    forbid_dynamic_in_path: Option<&'static str>,
}

const EMPTY_PARITY: ParityCase = ParityCase {
    name: "",
    files: &[],
    expected_edges: &[],
    expected_coverage: &[],
    expected_concrete_methods: &[],
    forbid_dynamic_in_path: None,
};

fn run_parity_cases(cases: &[ParityCase]) {
    let mut failures = Vec::new();
    for case in cases {
        let w = PersistedWorkspaceHarness::new();
        for &(path, source) in case.files {
            w.write(path, source);
        }
        w.build();

        let calls = w.calls();
        let coverage = w.coverage();
        let mut case_errors = Vec::new();
        for &(kind, caller, target_path, target_name) in case.expected_edges {
            if !calls.iter().any(|c| {
                c.kind == kind
                    && c.caller_name == caller
                    && c.target_path == target_path
                    && c.target_name == target_name
            }) {
                case_errors.push(format!(
                    "missing {kind} edge {caller} -> {target_path}::{target_name}"
                ));
            }
        }

        for &(path, expression, expected_status) in case.expected_coverage {
            if !coverage.iter().any(|row| {
                row.path == path && row.expression == expression && row.status == expected_status
            }) {
                case_errors.push(format!(
                    "missing coverage ({path}, {expression}, {expected_status})"
                ));
            }
        }

        if let Some(check_path) = case.forbid_dynamic_in_path {
            let dynamic_rows: Vec<_> = coverage
                .iter()
                .filter(|row| row.path == check_path && row.evidence.contains("dynamic callee"))
                .collect();
            if !dynamic_rows.is_empty() {
                case_errors.push(format!(
                    "unexpected dynamic call classification in {check_path}: {dynamic_rows:#?}"
                ));
            }
        }

        if !case.expected_concrete_methods.is_empty() {
            let conn = w.build_and_open();
            for &(path, name, receiver_type) in case.expected_concrete_methods {
                let count: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id = n.path_id \
                         WHERE p.path = ?1 AND n.name = ?2 AND n.kind = 'method' \
                         AND json_extract(n.details, '$.receiver_type') = ?3 \
                         AND json_extract(n.details, '$.is_stub') IS NULL",
                        rusqlite::params![path, name, receiver_type],
                        |row| row.get(0),
                    )
                    .unwrap();
                if count != 1 {
                    case_errors.push(format!(
                        "expected 1 concrete method {path}::{receiver_type}.{name}, found {count}"
                    ));
                }
            }
        }

        if !case_errors.is_empty() {
            failures.push(format!(
                "=== {} ===\nerrors: {:#?}\ncalls: {calls:#?}\ncoverage: {coverage:#?}",
                case.name, case_errors
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn default_bash_css_json_contract_matrix() {
    let cases: &[ParityCase] = &[
        #[cfg(feature = "lang-bash")]
        ParityCase {
            name: "bash-cross-file-source-and-builtins",
            files: &[
                (
                    "lib/helpers.sh",
                    "export APP_NAME=\"acdd\"\ngreet() {\n  echo \"hi\"\n}\n",
                ),
                (
                    "bin/run.bash",
                    "source ../lib/helpers.sh\nrun() {\n  greet\n  printf \"ok\\n\"\n}\n",
                ),
                ("bin/extra.zsh", "zsh_entry() {\n  :\n}\n"),
            ],
            expected_edges: &[("calls", "run", "lib/helpers.sh", "greet")],
            expected_coverage: &[
                ("bin/run.bash", "../lib/helpers.sh", "resolved"),
                ("bin/run.bash", "greet", "resolved"),
                ("bin/run.bash", "printf", "resolved"),
                ("lib/helpers.sh", "echo", "resolved"),
            ],
            forbid_dynamic_in_path: Some("bin/run.bash"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-css")]
        ParityCase {
            name: "css-import-keyframes-and-var-resolution",
            files: &[
                (
                    "styles/tokens.css",
                    ":root {\n  --accent: #09f;\n}\n@keyframes pulse {\n  from { opacity: 0; }\n  to { opacity: 1; }\n}\n",
                ),
                (
                    "styles/app.css",
                    "@import \"./tokens.css\";\n.btn {\n  color: var(--accent);\n}\n",
                ),
            ],
            expected_edges: &[
                ("imports", "app.css", "styles/tokens.css", "tokens.css"),
                ("references", ".btn", "styles/tokens.css", "--accent"),
            ],
            expected_coverage: &[
                ("styles/app.css", "./tokens.css", "resolved"),
                ("styles/app.css", "--accent", "resolved"),
            ],
            forbid_dynamic_in_path: Some("styles/app.css"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-json")]
        ParityCase {
            name: "json-schema-cross-file-and-internal-refs",
            files: &[
                (
                    "schemas/base.json",
                    "{\n  \"$defs\": {\n    \"User\": { \"type\": \"object\" }\n  },\n  \"title\": \"Base\"\n}\n",
                ),
                (
                    "schemas/entry.json",
                    "{\n  \"$defs\": {\n    \"Local\": { \"type\": \"string\" },\n    \"Wrapper\": { \"$ref\": \"#/$defs/Local\" }\n  },\n  \"user\": { \"$ref\": \"./base.json#/$defs/User\" }\n}\n",
                ),
            ],
            expected_edges: &[
                ("references", "Wrapper", "schemas/entry.json", "Local"),
                ("references", "user", "schemas/base.json", "User"),
            ],
            expected_coverage: &[
                ("schemas/entry.json", "./base.json#/$defs/User", "resolved"),
                ("schemas/entry.json", "Local", "resolved"),
                ("schemas/entry.json", "User", "resolved"),
            ],
            forbid_dynamic_in_path: Some("schemas/entry.json"),
            ..EMPTY_PARITY
        },
    ];
    run_parity_cases(cases);
}

#[test]
fn tier2_parity_contract_matrix() {
    let cases: &[ParityCase] = &[
        #[cfg(feature = "lang-go")]
        ParityCase {
            name: "go-workspace-import-stdlib-and-manifest",
            files: &[
                (
                    "go.mod",
                    "module example.com/app\n\ngo 1.22\n\nrequire (github.com/acme/dep v1.2.3)\n",
                ),
                (
                    "pkg/helper/lib.go",
                    "package helper\n\nfunc Build() int { return 1 }\n",
                ),
                (
                    "cmd/types.go",
                    "package main\n\nfunc SiblingHelper() int { return 1 }\n",
                ),
                (
                    "cmd/main.go",
                    "package main\n\nimport (\n\t\"fmt\"\n\t\"github.com/acme/dep\"\n\t\"example.com/app/pkg/helper\"\n)\n\ntype Worker struct{}\n\nfunc (w *Worker) Work() int { return 1 }\n\nfunc Run() int {\n\tfmt.Println()\n\tdep.Use()\n\tw := Worker{}\n\treturn helper.Build() + SiblingHelper() + w.Work()\n}\n",
                ),
            ],
            expected_edges: &[
                ("calls", "Run", "pkg/helper/lib.go", "Build"),
                ("calls", "Run", "cmd/types.go", "SiblingHelper"),
                ("calls", "Run", "cmd/main.go", "Work"),
            ],
            expected_coverage: &[
                ("cmd/main.go", "fmt", "external"),
                ("cmd/main.go", "github.com/acme/dep", "external"),
                ("cmd/main.go", "example.com/app/pkg/helper", "resolved"),
                ("cmd/main.go", "helper.Build", "resolved"),
                ("cmd/main.go", "SiblingHelper", "resolved"),
                ("cmd/main.go", "w.Work", "resolved"),
            ],
            forbid_dynamic_in_path: Some("cmd/main.go"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-csharp")]
        ParityCase {
            name: "csharp-using-sibling-stdlib-and-receiver",
            files: &[
                (
                    "App/Worker.cs",
                    "namespace App.Core;\npublic class Box<T> {}\npublic class Worker {\n    public int Run() { return 1; }\n}\n",
                ),
                (
                    "App/Runner.cs",
                    "using System;\nusing App.Core;\nusing WorkerAlias = App.Core.Worker;\nnamespace App.Web;\npublic class LocalWorker {\n    public int Ping() { return 1; }\n}\npublic class Runner {\n    public int Execute() {\n        var created = new Worker();\n        var aliased = new WorkerAlias();\n        var local = new LocalWorker();\n        var box = new Box<int>();\n        return aliased.Run() + local.Ping();\n    }\n}\n",
                ),
            ],
            expected_edges: &[
                ("calls", "Execute", "App/Worker.cs", "Worker"),
                ("calls", "Execute", "App/Worker.cs", "Box"),
                ("calls", "Execute", "App/Worker.cs", "Run"),
                ("calls", "Execute", "App/Runner.cs", "Ping"),
            ],
            expected_coverage: &[
                ("App/Runner.cs", "System", "external"),
                ("App/Runner.cs", "App.Core", "resolved"),
                ("App/Runner.cs", "App.Core.Worker", "resolved"),
                ("App/Runner.cs", "Worker", "resolved"),
                ("App/Runner.cs", "Box", "resolved"),
                ("App/Runner.cs", "aliased.Run", "resolved"),
                ("App/Runner.cs", "local.Ping", "resolved"),
            ],
            forbid_dynamic_in_path: Some("App/Runner.cs"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-php")]
        ParityCase {
            name: "php-use-require-stdlib-and-typed-receiver",
            files: &[
                (
                    "composer.json",
                    "{\"autoload\":{\"psr-4\":{\"Legacy\\\\\":\"legacy/\"}}}\n",
                ),
                (
                    "legacy/Helper.php",
                    "<?php\nclass Helper {}\n",
                ),
                (
                    "src/utils.php",
                    "<?php\nfunction util_ping() { return 1; }\n",
                ),
                (
                    "src/Core/Worker.php",
                    "<?php\nnamespace App\\Core;\nclass Worker {\n    public function run() { return 1; }\n}\n",
                ),
                (
                    "src/Entry.php",
                    "<?php\nnamespace App;\nuse DateTime;\nuse Legacy\\Helper;\nuse App\\Core\\Worker;\nrequire_once(\"./utils.php\");\nfunction execute() {\n    $w = new Worker();\n    return util_ping() + $w->run();\n}\n",
                ),
            ],
            expected_edges: &[
                ("imports", "Entry.php", "legacy/Helper.php", "Helper.php"),
                ("calls", "execute", "src/utils.php", "util_ping"),
                ("calls", "execute", "src/Core/Worker.php", "Worker"),
                ("calls", "execute", "src/Core/Worker.php", "run"),
            ],
            expected_coverage: &[
                ("src/Entry.php", "DateTime", "external"),
                ("src/Entry.php", "Legacy\\Helper", "resolved"),
                ("src/Entry.php", "./utils.php", "resolved"),
                ("src/Entry.php", "App\\Core\\Worker", "resolved"),
                ("src/Entry.php", "util_ping", "resolved"),
                ("src/Entry.php", "Worker", "resolved"),
                ("src/Entry.php", "$w.run", "resolved"),
            ],
            forbid_dynamic_in_path: Some("src/Entry.php"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-ruby")]
        ParityCase {
            name: "ruby-require-relative-stdlib-and-cross-file",
            files: &[
                (
                    "Gemfile",
                    "source 'https://rubygems.org'\ngem 'rack'\n",
                ),
                (
                    "lib/worker.rb",
                    "def build_worker\n  1\nend\n",
                ),
                (
                    "app/entry.rb",
                    "require 'json'\nrequire 'rack'\nrequire_relative '../lib/worker'\nclass LocalWorker\n  def run\n    1\n  end\nend\ndef execute\n  w = LocalWorker.new\n  w.run + build_worker()\nend\n",
                ),
            ],
            expected_edges: &[
                ("calls", "execute", "lib/worker.rb", "build_worker"),
                ("calls", "execute", "app/entry.rb", "LocalWorker"),
                ("calls", "execute", "app/entry.rb", "run"),
            ],
            expected_coverage: &[
                ("app/entry.rb", "json", "external"),
                ("app/entry.rb", "rack", "external"),
                ("app/entry.rb", "../lib/worker", "resolved"),
                ("app/entry.rb", "LocalWorker", "resolved"),
                ("app/entry.rb", "w.run", "resolved"),
                ("app/entry.rb", "build_worker", "resolved"),
            ],
            forbid_dynamic_in_path: Some("app/entry.rb"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-java")]
        ParityCase {
            name: "java-same-package-and-typed-receiver",
            files: &[
                (
                    "src/main/java/pkg/Worker.java",
                    "package pkg;\npublic class Worker {\n    public int helper() { return 1; }\n}\n",
                ),
                (
                    "src/main/java/pkg/Caller.java",
                    "package pkg;\npublic class Caller {\n    public Worker spawn() {\n        return new Worker();\n    }\n}\n",
                ),
                (
                    "src/main/java/typed/Helper.java",
                    "package typed;\npublic class Box<T> {}\npublic class Helper {\n    public int work() { return 1; }\n}\n",
                ),
                (
                    "src/main/java/app/Main.java",
                    "package app;\nimport java.util.List;\nimport typed.Box;\nimport typed.Helper;\npublic class Main extends Helper {\n    public Main() {\n        super();\n    }\n    public int boot() {\n        Helper h = new Helper();\n        Box<String> b = new Box<String>();\n        return h.work();\n    }\n}\n",
                ),
            ],
            expected_edges: &[
                ("calls", "spawn", "src/main/java/pkg/Worker.java", "Worker"),
                ("calls", "boot", "src/main/java/typed/Helper.java", "Helper"),
                ("calls", "boot", "src/main/java/typed/Helper.java", "Box"),
                ("calls", "boot", "src/main/java/typed/Helper.java", "work"),
            ],
            expected_coverage: &[
                ("src/main/java/pkg/Caller.java", "Worker", "resolved"),
                ("src/main/java/app/Main.java", "java.util.List", "external"),
                ("src/main/java/app/Main.java", "typed.Helper", "resolved"),
                ("src/main/java/app/Main.java", "Box", "resolved"),
                ("src/main/java/app/Main.java", "h.work", "resolved"),
            ],
            forbid_dynamic_in_path: Some("src/main/java/app/Main.java"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-kotlin")]
        ParityCase {
            name: "kotlin-same-package-and-typed-receiver",
            files: &[
                (
                    "src/main/kotlin/pkg/Worker.kt",
                    "package pkg\nclass Worker {\n    fun helper(): Int = 1\n}\n",
                ),
                (
                    "src/main/kotlin/pkg/Caller.kt",
                    "package pkg\nfun spawn(): Worker = Worker()\n",
                ),
                (
                    "src/main/kotlin/typed/Helper.kt",
                    "package typed\nclass Box<T>\nclass Helper {\n    fun work(): Int = 1\n}\n",
                ),
                (
                    "src/main/kotlin/app/Main.kt",
                    "package app\nimport kotlin.collections.List\nimport typed.Box\nimport typed.Helper\nfun boot(): Int {\n    val h: Helper = Helper()\n    val b = Box<Int>()\n    return h.work()\n}\n",
                ),
            ],
            expected_edges: &[
                ("calls", "spawn", "src/main/kotlin/pkg/Worker.kt", "Worker"),
                ("calls", "boot", "src/main/kotlin/typed/Helper.kt", "Helper"),
                ("calls", "boot", "src/main/kotlin/typed/Helper.kt", "Box"),
                ("calls", "boot", "src/main/kotlin/typed/Helper.kt", "work"),
            ],
            expected_coverage: &[
                ("src/main/kotlin/pkg/Caller.kt", "Worker", "resolved"),
                ("src/main/kotlin/app/Main.kt", "kotlin.collections.List", "external"),
                ("src/main/kotlin/app/Main.kt", "typed.Helper", "resolved"),
                ("src/main/kotlin/app/Main.kt", "Box", "resolved"),
                ("src/main/kotlin/app/Main.kt", "h.work", "resolved"),
            ],
            forbid_dynamic_in_path: Some("src/main/kotlin/app/Main.kt"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-c")]
        ParityCase {
            name: "c-stdlib-angle-bracket-and-relative-include",
            files: &[
                ("src/helper.h", "int c_helper(void) { return 1; }\n"),
                (
                    "src/main.c",
                    "#include <stdio.h>\n#include \"helper.h\"\nint run(void) { return c_helper(); }\n",
                ),
            ],
            expected_edges: &[("calls", "run", "src/helper.h", "c_helper")],
            expected_coverage: &[
                ("src/main.c", "<stdio.h>", "external"),
                ("src/main.c", "\"helper.h\"", "resolved"),
                ("src/main.c", "c_helper", "resolved"),
            ],
            forbid_dynamic_in_path: Some("src/main.c"),
            ..EMPTY_PARITY
        },
        #[cfg(feature = "lang-cpp")]
        ParityCase {
            name: "cpp-stdlib-angle-bracket-member-and-out-of-line-method",
            files: &[
                (
                    "src/worker.hpp",
                    "int worker_fn() { return 1; }\nclass Worker {\npublic:\n    int helper();\n};\n",
                ),
                (
                    "src/worker.cpp",
                    "#include \"worker.hpp\"\nint Worker::helper() { return 1; }\n",
                ),
                (
                    "src/main.cpp",
                    "#include <vector>\n#include \"worker.hpp\"\ntemplate <typename T> class Box {};\nclass Local {\npublic:\n    int ping();\n};\nint Local::ping() { return 1; }\nint run() {\n    Local l;\n    auto* b = new Box<int>();\n    return l.ping() + worker_fn();\n}\n",
                ),
            ],
            expected_edges: &[
                ("calls", "run", "src/main.cpp", "ping"),
                ("calls", "run", "src/worker.hpp", "worker_fn"),
                ("calls", "run", "src/main.cpp", "Box"),
                ("references", "helper", "src/worker.hpp", "Worker"),
                ("references", "ping", "src/main.cpp", "Local"),
            ],
            expected_coverage: &[
                ("src/main.cpp", "<vector>", "external"),
                ("src/main.cpp", "\"worker.hpp\"", "resolved"),
                ("src/main.cpp", "l.ping", "resolved"),
                ("src/main.cpp", "worker_fn", "resolved"),
            ],
            expected_concrete_methods: &[
                ("src/worker.cpp", "helper", "Worker"),
                ("src/main.cpp", "ping", "Local"),
            ],
            forbid_dynamic_in_path: Some("src/main.cpp"),
        },
    ];
    run_parity_cases(cases);
}
