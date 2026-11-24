---
doc_type: api
title: Language support
---

# Language support

Language profiles are selected at compile time (with built-in `sql` always available). A default build includes Python, Rust, TypeScript, JavaScript, Vue, Protocol Buffers, HTML (including `.j2`), Bash/Shell, CSS, JSON, YAML, TOML, and SQL. Markdown and MDX documentation is indexed separately from code profiles.

```sh
cargo build --release --locked
cargo build --release --locked --features all-languages
cargo build --release --locked --no-default-features --features lang-rust
```

Use `acdd --help` to inspect the installed binary. Enabling a profile changes which files the scanner admits and which parser and linker rules are available. Rebuild the index after changing the binary's feature set.

## File profiles

| Cargo feature | Language | Extensions |
| --- | --- | --- |
| `lang-python` (default) | Python | `.py`, `.pyi` |
| `lang-rust` (default) | Rust | `.rs` |
| `lang-typescript` (default) | TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` |
| `lang-typescript` (default) | JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` |
| `lang-vue` (default) | Vue | `.vue` |
| `lang-proto` (default) | Protocol Buffers | `.proto` |
| `lang-html` (default) | HTML / Jinja2 | `.html`, `.htm`, `.j2` |
| `lang-bash` (default) | Bash / Shell | `.sh`, `.bash`, `.zsh` |
| `lang-css` (default) | CSS | `.css` |
| `lang-json` (default) | JSON | `.json` |
| `lang-yaml` (default) | YAML | `.yaml`, `.yml` |
| `lang-toml` (default) | TOML | `.toml` |
| *(always enabled)* | SQL | `.sql` |
| `lang-go` | Go | `.go` |
| `lang-java` | Java | `.java` |
| `lang-csharp` | C# | `.cs` |
| `lang-kotlin` | Kotlin | `.kt`, `.kts` |
| `lang-php` | PHP | `.php` |
| `lang-ruby` | Ruby | `.rb` |
| `lang-c` | C | `.c`, `.h` |
| `lang-cpp` | C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` |

- **`--features all-languages`**: compiles all 21 language profiles into the `acdd` binary.
- **Unenabled extensions**: source extensions without an active profile are skipped during code extraction (Markdown and MDX in `roots`/`docs` are always indexed).
- **Declarative Tier-2 engine**: Bash, CSS, JSON, and optional profiles (`go`, `java`, `csharp`, `kotlin`, `php`, `ruby`, `c`, `cpp`) run on the shared declarative `SyntaxGrammarSpec` + `GenericScopeAnalysis` + `GENERIC_LINKER` engine.

## What the graph means

- **Static syntax provenance**: the index captures declarations, imports, references, and edges recoverable from syntax — resolved edges link to indexed candidates, while unresolved/external edges record static reference evidence.
- **Cross-language framework links**: route calls, HTMX requests, render targets, and template bindings connect only after language-level proof (never by bare name coincidence).
- **`calls` vs. `possible_calls`**: complete `if`/`elif`/`else` joins with 2–8 distinct targets emit bounded `possible_calls` edges (`> 8` widens to `Unknown`); impact and removal checks include `possible_calls`, while exact call counts include only `calls`.

| Profile group | Important static limit |
| --- | --- |
| Python and Ruby | Dynamic imports, metaprogramming, monkey patching, and runtime dispatch can escape syntax-level analysis. |
| TypeScript, JavaScript, and Vue | Compiler aliases, generated types, framework transforms, and inferred dispatch may require build context. |
| Rust | Macros, conditional compilation, generated code, and `include!` can change the compiled program. |
| Go | Build tags, generated code, CGo, and package selection affect the final build. |
| Protocol Buffers | Generated language bindings and import resolution depend on the compiler and include paths. |
| HTML and HTMX | Files are modules; individual tags are not graph symbols. `.html`, `.htm`, and `.j2` templates use the HTML profile. Each HTMX request is an indexed source node contained by its HTML module. Local JavaScript/TypeScript script and link targets contribute references. Inline JavaScript analysis requires `lang-typescript`. Confirm runtime route ownership in server code. |
| Bash, CSS, JSON, YAML, and TOML | Bash indexes top-level/exported functions, variables, and `source`/`.` links; CSS indexes rule sets, `@keyframes`, declarations, `@import`, and `var(--*)`; JSON indexes top-level/$defs keys and `$ref` targets; YAML and TOML index mapping keys and tables. Application configuration meaning and runtime shell/schema evaluation are outside the graph. |
| Java, C#, Kotlin, and PHP | Classpath, reflection, dependency injection, autoloading, and generated code may add relationships. |
| C and C++ | Preprocessing, conditional compilation, templates, and linker configuration determine compiled relationships. |

Use [configuration](configuration.md) to control admitted paths and [indexing architecture](../architecture/indexing.md) to understand the extraction and linker boundary.

## Python type aliases

- **Indexed as `type` symbols**: module-level PEP 695 aliases, `TypeAlias` / `TypeAliasType` annotations, and `TypeVar`, `NewType`, or `TypeAliasType` assignments (including inside `TYPE_CHECKING` blocks).
- **Scope**: module-qualified names participate in import and annotation resolution; function-local and class-local aliases are excluded.

## Dependencies and typed receivers

ACDD collects declared dependencies and `ModulePrefix` records from workspace manifests via modular `ManifestParser` plugins before linking:

- **Python**: `pyproject.toml`, `requirements*.txt`, `setup.cfg`, `Pipfile`
- **JavaScript / TypeScript**: `package.json`, lockfiles, `pnpm-workspace.yaml`, `tsconfig*.json`
- **Rust / Go / JVM**: `Cargo.toml`, `go.mod`, `pom.xml`, `build.gradle`, `build.gradle.kts`
- **PHP / C# / Ruby / C++**: `composer.json` (PSR-4), `*.csproj` (`RootNamespace`), `Gemfile`, `*.gemspec`, `CMakeLists.txt`, `meson.build`
- **Classification**: standard-library imports (including `google/protobuf/*`) and declared dependencies retain distinct evidence; unindexed absolute imports become external, while relative/local missing symbols retain local resolution evidence.

- **Manifest discovery**: covers manifests inside admitted roots plus direct workspace-root manifests up to `4 MiB` (see [manifest discovery boundaries](configuration.md#indexing-scope)); edits trigger an index rebuild automatically on MCP reads or `acdd delta`.
- **CommonJS `require`**: JS/TS recognizes top-level `require(...)` (including destructuring) as import aliases unless shadowed by a local callable.

- **Python annotations & C3 hierarchy**: functions store parameter types in `details.param_types` (`Callable`/`Protocol`, `type[T]`, generics, PEP 604 `A | B` unions). Calls through unmodified parameters resolve to indexed receiver methods (`inferred`) or external provenance (`logging.getLogger`, `unittest`, Django `BaseCommand`/`Model.objects`/`QuerySet`), respecting C3 MRO overrides.
- **Python receiver hints**: tracks string literals, f-strings, slices, `Path /`, `with ... as`, constructors, chained calls, and `super()`. Unsupported computed receivers or reassigned bindings retain explicit uncertainty.
- **Hierarchy return & DOM literal factories**: bundled Django QuerySet chains (`all`, `filter`, `exclude`, `order_by`, `select_related`, `prefetch_related`) and JS/TS literal-factory rules (`document.createElement('input').value`, `script.src`) resolve only via manifest declarations without inventing dynamic properties (see [framework manifests](framework-manifests.md#typed-extraction-rules)).
- **Python explicit exports**: follows module/package exports including static PEP 562 literal names; incremental delta hydrates required import chains and preserves both `src`-qualified and package-root names across linked workspaces.
- **Shared value-flow analysis**: Python, JS/TS, and Rust track source-ordered assignments, aliases, return types, fields, and simple complete factory returns while leaving reassignments, unawaited async, and generators unresolved. Cross-module external aliases propagate uniformly across indexed profiles alongside compound generic `call_returns` rules and element-wise tuple/`Option`/`Result` branch joins.
- **Wrapper unwrapping**:
  - **Rust**: `?`, `.unwrap()`, `.expect()` project inner types from `Option<T>`, `Result<T, E>`, `anyhow::Result<T>` (custom `unwrap` keeps its declared signature).
  - **TypeScript / JavaScript**: `await` unwraps Promise annotations and async constructors (`await promise` resolves, raw `promise` does not).
  - **Python**: projects single non-None payloads from `Optional[T]`, `Union[T, None]`, and `T | None` (multi-type unions like `A | B` remain unresolved).
- **Bounded branch joins**:
  - Complete `if`/`elif`/`else` joins with 1 proven declaration remain `exact`.
  - 2–8 distinct targets emit bounded `possible_calls` edges separate from exact calls; >8 widens to `Unknown/Incomplete` (see [ADR 0010](../adr/0010-static-value-flow-and-receiver-resolution.md)).
- **Trait objects, inheritance & macros**:
  - Rust trait-object dispatch and Python `abc.ABC` methods navigate to the base declaration (`inferred`) plus up to 8 concrete `possible_calls` targets (with explicit `trait_object_dispatch` gaps: `resolved` when `1..=8` indexed implementations exist, `unresolved` when `0` or `> 8`); >8 suppresses concrete fan-out.
  - Declarative `macro_rules!` struct/enum exports (`rust/macros.rs`) and `impl Trait for $ty` expansions are indexed, while macro invocations record `macro_expansion_not_analyzed` without arbitrary expansion.
- **Rust chains & `Deref`**: tracks multi-segment calls (<=8 segments, 512B), `#[derive(...)]`, `#[derive(clap::Parser)]` `Self` constructors, `#[serde(...)]` callbacks, generic-bounded inherent methods, external Cargo re-exports (`RustForwarding`), nominal `Deref` targets, and `.as_ref()`/`.as_mut()`/`.ok()` transformations.

- **Vue & web packs**:
  - **Vue templates**: uses HTML boundaries + JS expression parsing for component imports, event handlers, qualified members, `v-for` bindings, Pinia `defineStore` auto-imports (`nuxt.toml`), slot bindings, and arrow parameters while preserving original coordinates.
  - **TypeScript / JavaScript**: parses IIFEs as lexical scopes and loads bundled DOM/WebIDL (`web.toml`) and Playwright (`playwright.toml`) packs.
  - **HTML / `.j2` templates**: loads syntax-activated Alpine.js rules (`ActivationCondition` / `FileActivationSignals`); classic scripts share file scope while modules and inline event handlers keep separate lexical scopes (entity-encoded expressions stay unguessed diagnostics).

## Python registrations

- **FastMCP tools (`@server.tool`)**: when a module imports `FastMCP` (`fastmcp` / `mcp.server.fastmcp`) and binds `server = FastMCP(...)`, `@server.tool` / `@server.tool(...)` emits a `tool_registration` node with `contains` from the module and `handles` to the handler function (honoring literal `name=`, import aliases, and qualified constructors).
- **Dynamic boundaries**: runtime-created tools, factory servers, imported instances, or reassigned constructors stay unconfirmed without guessing.

## HTML and configuration files

- **HTML**: shares one HTML Tree-sitter grammar and profile (`app.html` stays distinct from `app.js`), resolving local `script src` / `link href` targets and inline `<script>` AST facts (when `lang-typescript` is enabled; `<script src=...>` inline bodies and non-JS data blocks are skipped).
- **HTMX requests (`htmx_request` / `htmx_context`)**:
  - `hx-get`, `hx-post`, `hx-put`, `hx-patch`, `hx-delete`, and `data-hx-*` attributes emit `htmx_request` nodes (trigger, target, swap, include, and `hx-vals` metadata shown in `detail="full"`).
  - Elements with only supporting HTMX attributes emit `htmx_context` nodes; `local_unverified` marks local route paths awaiting server handler proof (see [HTMX attributes](https://htmx.org/reference/)).
- **YAML & TOML**: YAML indexes block/flow mapping keys; TOML indexes tables and array tables as namespaces and key/value pairs as variables (disambiguate repeated names by path and line).

These profiles use the same index, selectors, snippets, and incremental update path as other languages. Diagnostics describe syntax accepted by the grammar, not complete HTML, YAML, or TOML specification validation.
