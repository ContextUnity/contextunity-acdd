---
title: "Framework manifests"
doc_type: guide
---

# Framework manifests

Framework manifests supply declarative linking rules (routes, dependency injection, ORM tables, templates, and component bindings) to ACDD's existing language parsers and linkers — without compiling custom Rust code.

- **Declarative data only**: a manifest configures existing typed extraction operations; it does not execute code, install packages, or add a new language grammar.

## Bundled manifests

Built-in packs live in [`crates/acdd-index/src/languages/manifests/`](https://github.com/ContextUnity/contextunity-acdd/tree/main/crates/acdd-index/src/languages/manifests) and are embedded into the `acdd` binary at compile time (using each file stem as the pack ID):

- **Included packs**: FastAPI, Starlette, Django, SQLAlchemy, HTMX, Alpine, Vue/Pinia (`vue-runtime`), Nuxt, Playwright, WebIDL/DOM (`web`), Rust stdlib/Cargo, Python `unittest`, and GridViewSpec.
- **FastAPI & Starlette**: resolves imported `Depends`, HTTP route decorators (`response_model` `uses_model`, `exception_handler`), `Route(methods=[...])`, `Mount`, `WebSocketRoute`, and `BaseHTTPMiddleware`.
- **GridViewSpec**: maps `GridViewSpec`, `GridViewBlockBase` dataclass fields, host adapters, and `GridViewActionBase` action subclasses.

## Project manifests (`.acdd/frameworks/`)

Override a built-in pack or add project-specific framework rules without rebuilding `acdd` by placing `.toml`, `.yaml`, or `.yml` files in `.acdd/frameworks/`:

```text
<workspace>/.acdd/frameworks/<framework-id>.toml
<workspace>/.acdd/frameworks/<framework-id>.yaml
<workspace>/.acdd/frameworks/<framework-id>.yml
```

- **Pack identity**: determined by the filename stem (e.g. `django.toml` extends the built-in `django` pack; `project-rules.toml` creates a new pack).
- **Default merge (`extend = true`)**: project rules merge with bundled rules of the same pack ID and override matching rule keys.
- **Full replacement (`replace = true`)**: discards bundled rules for that pack ID and uses only the local file.
- **Disable a pack (`enabled = false`)**: disables the entire pack (including bundled defaults) for that workspace until removed or set to `true`.
- **Workspace isolation**: each primary or linked workspace reads only its own `.acdd/frameworks/` folder; refresh with `acdd build` / `acdd delta` (or let MCP auto-refresh).
- **Reference templates**: see the annotated [TOML example](../../skills/acdd/references/frameworks/declarative-rules.toml.example) and [YAML example](../../skills/acdd/references/frameworks/declarative-rules.yaml.example).

Example — extending HTML template tags for Django:

```toml
[[syntax_rules]]
id = "django-template-symbols"
language = "html"
package_family = "python"
activation = [{ kind = "package", name = "django" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["csrf_token", "url", "load"], filters = ["safe"], libraries = ["static"], library_routes = { static = "static" } }
```

## Dependency and configuration inputs

Package activation checks declared dependencies in your project's manifest files (scoped to the nearest project; installed global/venv packages are never scanned):

| Profile | Package declarations and locks | Other collected configuration |
| --- | --- | --- |
| Python | `pyproject.toml`, `requirements*.txt`, `setup.cfg`, `Pipfile` | — |
| JavaScript/TypeScript | `package.json`, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` | `pnpm-workspace.yaml`, `package.json` workspaces/exports, `tsconfig*.json` aliases |
| Rust | `Cargo.toml` | Workspace & crate package names |
| Go | `go.mod` | `ModulePrefix` extraction (`lang-go`) |
| Java/Kotlin | `pom.xml`, `build.gradle`, `build.gradle.kts` | `lang-java` / `lang-kotlin` |
| PHP | `composer.json` | PSR-4 `ModulePrefix` extraction (`lang-php`) |
| C# | `*.csproj` | `RootNamespace` extraction (`lang-csharp`) |
| Ruby | `Gemfile`, `*.gemspec` | `lang-ruby` |
| C/C++ | `CMakeLists.txt`, `meson.build` | `lang-c` / `lang-cpp` |

- **Activation conditions (`activation`)**: rules can gate on `package`, `syntax_attribute`, `syntax_attribute_prefix`, `file_extension`, or `any` (evaluated per file via `FileActivationSignals`).
- **Cross-language delegation**: Vue delegates package discovery to TypeScript; HTML delegates to JavaScript (or `package_family = "python"` when specified on the rule).

## Typed extraction rules

Every manifest can declare up to six typed rule sections:

| Section | Required fields | Meaning |
| --- | --- | --- |
| `class_hierarchy_receivers` | `bases`, `provider`, `members` | Expose external base-class methods/properties on verified local subclasses. |
| `route_decorators` | `callee`, `method`, `path_argument` | Extract HTTP routes from decorators using a zero-based path argument. |
| `route_calls` | `callee`, `method`, `path_argument` | Extract HTTP routes from calls (optional `handler_argument` links the handler). |
| `di_calls` | `callee`, `argument` | Extract dependency-injection edges from a zero-based call argument. |
| `semantic_rules` | `id`, `namespace`, `scope`, `operation` | Link an imported call or class attribute to a template file, symbol, or SQL table. |
| `syntax_rules` | `id`, `language`, `operation` | Run a closed grammar operation (templates, components, factories, HTMX/Alpine attributes, CLI dispatch). |

Key extraction capabilities:

- **Hierarchy return policies (`returns`)**: `kind = "self"` preserves local subclass types across chained calls; `kind = "receiver"` maps to another base class in the pack (`<= 256` policies/row, `<= 128` rows; used for Django `QuerySet` chains).
- **ORM & SQL table rules**:
  - `class_attribute_to_table` (`python`): links static table-name attributes (`__tablename__`, `Meta.db_table`) via `defines_table`.
  - `class_default_table` (`python`): derives default `<app>_<model>` table names for subclasses without an explicit table attribute.
  - `macro_type_to_table` (`rust`): links typed SQL macros (such as `sqlx::query_as!`) to target SQL tables via `queries_table`.
- **JS/TS `argument_factories`**: maps literal-argument factory calls (`document.createElement('input').value`) to concrete DOM receiver types (`index` `0..=3`, `<= 32` literals).
- **Route composition**: supports `receiver_names`, router `prefixes` (e.g. class decorators or `Mount`), `include` mounts, `methods_keyword`, `name_keyword`, and `registration_guard` (`argument < 32`, `handler_argument != path_argument`).

Example — custom route decorator with a class-level mount prefix:

```toml
[[route_decorators]]
callee = "Expose"
method = "GET"
path_argument = 0
prefixes = [{ kind = "class_decorator", callee = "Mount", argument = 0 }]
```

## Validation & diagnostics

- **Strict parsing**: TOML and YAML deserialize into the same schema — unknown fields, duplicate keys, duplicate pack stems, symlinks, or files above `4 MiB` fail index builds cleanly.
- **Prepared `RuleIR` tables**: rules compile once per workspace and grammar before parallel file extraction; AST nodes filter by `kind_id` first and require full import/argument proof before emitting edges.
- **`framework_rule_diagnostics`**: build and `delta` reports list `eligible_rule_identity_removed` whenever a local override removes previously active bundled rule IDs.
- **Language boundaries**: manifests extend framework rules on compiled languages; adding a brand-new programming language requires a `LanguageProfile` and Cargo `lang-*` grammar (see [ADR 0011](../adr/0011-language-linkers-and-framework-manifests.md)).

## Linking imported calls to template files (`call_to_file`)

Link a proven imported call (such as `django.shortcuts.render`) and its static template-path literal to an indexed HTML module (`renders_template` edge):

```toml
[[semantic_rules]]
id = "render-template"
namespace = "templates"
scope = "project"
activation = [{ kind = "package", name = "django" }]
operation = { kind = "call_to_file", language = "python", callee = { kind = "imported", module = "django.shortcuts", export = "render" }, argument = 1, target = { kind = "project_path_suffix", prefix = "templates/", language = "html" }, relation = "renders_template" }
```

Equivalent YAML (`.acdd/frameworks/render.yaml`):

```yaml
semantic_rules:
  - id: render-template
    namespace: templates
    scope: project
    activation:
      - kind: package
        name: django
    operation:
      kind: call_to_file
      language: python
      callee: {kind: imported, module: django.shortcuts, export: render}
      argument: 1
      target: {kind: project_path_suffix, prefix: templates/, language: html}
      relation: renders_template
```

- **Path safety**: accepts normalized relative path literals only (rejects absolute paths, `..`, interpolations, or ambiguous multi-file matches).

## Closed syntax & projection operations

Each `[[syntax_rules]]` entry declares `id`, `language`, optional `package_family`, optional `activation`, and one typed `operation`:

| Operation kind | Purpose |
| --- | --- |
| `render_context` | Propagate literal template-context dictionary keys from a render call to the target template module. |
| `named_route_call` / `template_named_route` | Emit typed static route keys from code calls (e.g. `reverse(...)`) or template tags (`{% url %}`). |
| `receiver_registration` | Verify lexical router/app construction (`FastAPI()`, `APIRouter()`) for guarded route rules. |
| `decorated_registration` | Register decorated callables (`tool`, `cli_command`, or `event_handler`) on proven receivers or imported decorators. |
| `class_method_registration` | Register overridden methods on proven local subclasses of an imported framework base class. |
| `command_dispatch` | Link Rust CLI subcommand enums and match arms to their handler functions. |
| `template_loader` / `template_library` | Record static template roots and custom tag/filter libraries within project scope. |
| `template_symbols` / `template_dialect` | Declare built-in template tags/filters/routes and custom block/include/extends directives. |
| `object_factory` / `receiver_type` | Bind factory return members, nominal types, callback parameter types, and optional `argument_factories` / `member_returns` / `call_returns` (`ReturnTypeSpec` compound generic wrappers) for chained return-type resolution. |
| `implicit_receivers` / `implicit_builtins` | Admit framework-injected globals (`defineProps`, `$el`) unless shadowed by local variables. |
| `component_environment` / `component_options` / `component_macros` | Resolve auto-imported Vue/Nuxt components, composables (`exports`, `call_results`), options API registrations, and `props`/`emits`/`slots` macros. |
| `route_object` / `file_route` | Extract routes from JS/TS route objects or file-based Vue page directories (`method = "GET"`). |
| `http_client_call` | Emit outbound endpoint calls from literal HTTP client invocations (optional `target_context` scopes `request_owners` to `route_owners`). |
| `html_request_attributes` / `html_script_attributes` | Extract HTMX-style request/context nodes (`hx-get`, `hx-vals`) and parse Alpine-style inline JS attributes. |

Additional projection operations:

| Envelope / operation | Required fields | Effect |
| --- | --- | --- |
| Semantic `symbol_attachment` | `language="python"`, `callee`, `source`, `target`, `relation` | Links a decorated/registered callable to a uniquely resolved model/schema class (`uses_model` or `references`). |
| Syntax `factory_members` | `callee`, `argument`, optional `shape` (`options` or `setup_return`), `configured_module` | Exposes state/action/setup members returned by JS/TS store or composable factories (e.g. Pinia `defineStore`). |
| Syntax `named_binding` | `domain`, `binding` (`call_definition`, `call_reference`, `assignment_definition`, `template_reference`, `member_reference`, `attribute_definition`) | Joins literal string keys across producers and consumers within project/provider scope (`<= 16` `provider_scopes`). |
| Semantic `endpoint_projection` | `language="python"`, `callee`, `attachment`, `url_keyword`, `method_keyword`, `default_method` | Extracts local endpoint URL and HTTP method from nested constructors. |

### Safety & resolution bounds

- **Lexical precedence**: local variables, parameters, imports, and reassignments always shadow implicit framework globals and invalidate speculative projections.
- **Fail-closed ambiguity**: dynamic strings, non-literal methods, or duplicate matching targets remain `unresolved` or `ambiguous` rather than guessing an edge.
- **Per-scope caps**: rule identifiers (`<= 128 B`), literal keys (`<= 256 B`), argument indices (`< 32`), factory members (`<= 128`), and per-scope typed projection vectors (`<= 128` facts) enforce bounded memory and deterministic incremental `delta` relinking.
