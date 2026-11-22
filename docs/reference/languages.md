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

`all-languages` enables every listed profile. Files outside the enabled profiles are excluded from code extraction. The scanner still admits Markdown and MDX in configured roots. Default Bash, CSS, and JSON profiles as well as optional Tier-2 profiles (`go`, `java`, `csharp`, `kotlin`, `php`, `ruby`, `c`, `cpp`) run on the shared declarative `SyntaxGrammarSpec` + `GenericScopeAnalysis` + `GENERIC_LINKER` engine.

## What the graph means

The index records declarations, imports, references, and relationships recoverable from source syntax. A resolved edge means the linker found a candidate within its indexed scope. An unresolved or external edge remains evidence of a reference, not evidence that the dependency is absent. Confirm behavior in the build system and runtime where it matters.

Cross-language framework links use indexed source facts after language-level resolution. Route calls, HTMX requests, render targets, and template context bindings retain their source ownership and ambiguity status; they do not infer targets from shared names alone.

For Python, a complete local `if`/`elif`/`else` join can expose method targets through `possible_calls` edges. These edges list bounded possibilities and are not exact runtime calls. Unknown paths remain incomplete; more than eight distinct local declarations widens the whole result without emitting a truncated set. Inbound impact and removal assessments include possible edges as dependency evidence, while exact call counts use only exact `calls` edges.

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

Python indexes module-level PEP 695 aliases, `TypeAlias` and `TypeAliasType` annotations, and assignments calling `TypeVar`, `NewType`, or `TypeAliasType` as `type` symbols. These symbols have module-qualified names and participate in import and annotation resolution. Module-level declarations inside `TYPE_CHECKING` blocks are included. Function-local and class-local aliases are excluded.

## Dependencies and typed receivers

ACDD collects declared dependencies and `ModulePrefix` records from workspace manifests via modular `ManifestParser` plugins before linking: Python `pyproject.toml`, `requirements*.txt`, `setup.cfg`, and `Pipfile`; JavaScript/TypeScript `package.json`, lockfiles, `pnpm-workspace.yaml`, and `tsconfig*.json`; Rust `Cargo.toml`; Go `go.mod`; JVM `pom.xml`, `build.gradle`, and `build.gradle.kts`; PHP `composer.json` (PSR-4); C# `*.csproj` (`RootNamespace`); Ruby `Gemfile` and `*.gemspec`; and C/C++ `CMakeLists.txt` and `meson.build`. Linked workspaces use the admitted adapter configuration. Standard-library imports (including `google/protobuf/*` in Protocol Buffers and stdlib modules across Tier-2 languages) and declared dependencies have distinct evidence. An absolute import with no indexed provider is classified as external even when no manifest declares it; relative imports and missing symbols in indexed modules retain their local resolution evidence.

The committed manifest digest covers scanner-admitted manifests and supported
files directly in primary and linked workspace roots, including those root files
outside source roots. Nested manifests outside admitted roots are excluded.
See [manifest discovery boundaries](configuration.md#indexing-scope). A changed
admitted manifest triggers a full rebuild during delta admission or the next MCP
inventory check, after the normal freshness TTL. Discovery excludes ignored
directories and symlinks and reads manifests up to 4 MiB.

JavaScript and TypeScript recognize CommonJS `require` bindings, including destructuring, as import aliases. A locally bound `require` is treated as an ordinary callable.

Python functions retain explicit parameter annotations in `details.param_types` (including `Callable`/`Protocol`, `type[T]`, generic subscripts, and PEP 604 `A | B` union parameters via bounded `possible_calls`). Calls through an unmodified parameter can resolve to methods of an indexed receiver class with `inferred` confidence, or to external evidence when the receiver type comes from an external import or proven `logging.getLogger` factory (`logging_factories.rs`). Inherited member lookup uses a cached Python C3 order and respects local overrides. Known unittest assertions, Django `BaseCommand`/`Model.objects`/`QuerySet` operations receive external evidence only when their framework ancestry is established.

Python records receiver hints for string literals, f-strings, string slices, `Path /` expressions, `with ... as` bindings, constructor calls, chained calls, and zero-argument `super()`. Known string methods and verified class members can resolve without interpreting arbitrary expressions. Ambiguous types, reassigned parameters, unknown return values, and unsupported computed receivers retain uncertainty.

Declared Python hierarchy-member return policies also support assigned and direct chains after canonical receiver/member proof. An absent policy supplies no result; finite explicit results never become unrestricted external types. The bundled Django QuerySet policies include `all`, `filter`, `exclude`, `order_by`, `select_related` and `prefetch_related`, with explicit QuerySet results on the corresponding Manager and Model members. JavaScript and TypeScript share a bounded AST call constructor for assigned and direct literal-argument factory receivers. Only manifest-declared global/member and literal combinations select a subtype; shadowing, writes, wrong properties and ambiguous mappings preserve uncertainty. Read references can use declared properties, including direct `document.createElement('input').value` and assigned `script.src`; property lookup does not admit `value()` or `src()` as calls. Existing local members take precedence, and independently proven generic DOM evidence remains available. Vue and HTML retain their current inference. See [framework manifests](framework-manifests.md#typed-extraction-rules) for schema and bounds.

Python import resolution follows explicit exports across ordinary modules and packages. Statically described PEP 562 exports are admitted only when a literal export name identifies an existing provider; dynamic export names and missing providers remain unverified. Incremental indexing hydrates the required import chain before linking. Linked workspaces retain both `src`-qualified and package-root module names.

Python, TypeScript/JavaScript, and Rust share source-ordered local value-flow analysis. Constructor assignments, local aliases, explicit return types, and declared fields can establish receiver identity. Named type aliases follow indexed declarations with cycle and depth limits. Generic arguments remain in the facts; documented builtin containers can establish container identity, while nullable annotations outside the supported `typing.Optional[T]` and single-payload `typing.Union[T, None]` forms and custom generics remain unsupported. Python and JavaScript also admit simple complete factory-return summaries; arbitrary external factories do not establish an instance type. Reassignments, ambiguous types, incomplete returns, generators, unawaited async results, and unsupported control flow preserve uncertainty.

Rust `?`, `.unwrap()`, and `.expect()` explicitly project the first payload of standard `Option<T>`, `Result<T, E>`, and `anyhow::Result<T>` in initializer and receiver expressions. Ordinary wrapper values retain their container type, so `is_some` and `is_ok` remain builtin evidence and payload methods stay unresolved without an explicit operation. A custom method named `unwrap` uses its own declared return contract. TypeScript and JavaScript `await` bindings can expose a Promise payload annotation or the constructor returned by a supported async function or arrow. Typed Promise parameters and local aliases follow the same rule: `const service = await promise; service.work()` can resolve the payload, while `promise.work()` remains unresolved. Python parameters and local bindings explicitly project one non-None payload from `typing.Optional[T]`, `typing.Union[T, None]`, and PEP 604 binary union expressions (`T | None` or `None | T`), resolving member calls to the inner type. Multi-nonnull unions (`A | B`), unions with `NoneType`, and malformed wrapper arities retain unresolved semantics. Reassignments, conditional writes, shadowed Promise declarations, and unknown async returns retain uncertainty.

Milestone 046 admits bounded possible targets when the enabled profile can enumerate every path of a supported local join. Python may admit complete `if`/`elif`/`else` joins, including an incoming value when `else` is omitted; Rust and TypeScript/JavaScript use only complete forms admitted by their profile. A join with one proven declaration remains exact. Two through eight distinct declarations may produce bounded possible-target evidence; an unknown path makes that evidence incomplete, and overflow widens to `Unknown/Incomplete`. Possible targets remain separate from exact call links. See [ADR 0010](../adr/0010-static-value-flow-and-receiver-resolution.md) for the versioned language matrix and compatibility contract.

TypeScript/JavaScript imports use explicit ESM and supported CommonJS exports, including transitive re-exports. Private declarations and type-only exports do not become callable targets. Class and interface member lookup respects inheritance and static versus instance methods. Rust indexes cross-file inherent implementations and explicitly visible trait implementations. Trait-object calls retain an explicit unresolved dispatch gap, with inferred navigation to the trait method and separate possible edges to at most eight indexed concrete implementations. Python abstract methods (`abc.ABC`) and interface methods similarly link to the base declaration with inferred confidence and emit up to eight `possible_calls` edges to concrete subclass overrides in the workspace. More than eight implementations emit no concrete candidates. Macro bodies supply macro-owned references to statically named call and type tokens; invocation retains declaration navigation and an unresolved expansion gap without expanding the macro.

Rust extracts ordinary paths, chained receivers, string/slice receivers, struct-field propagation, `&T` parameters, and field-chain callees structurally, including turbofish and multiline syntax. Canonical names retain original call coordinates and use at most eight segments and 512 bytes. Initializer calls use the same extraction and existing nominal return contracts (`rust-std.toml`, `rust-cargo.toml`, `declarative_macros.toml`). Rust also indexes `const_item`, `static_item`, `enum_variant`, and `associated_type`, synthesizes standard `#[derive(...)]` implementations, and indexes bounded declarative struct/enum `macro_rules!` exports (`rust/macros.rs`). Unsupported computed receivers and qualified trait dispatch retain uncertainty; `impl` blocks supply method scopes without declaring lexical type names.

Rust module calls, `Self` associated calls and transitive module glob imports resolve through unique indexed declarations with visibility and alias-depth limits. Conflicting, cyclic, inaccessible or missing providers retain uncertainty. Proven standard `Deref` implementations with a unique nominal `Target` can supply inherited field and member lookup; custom traits with the same name supply no standard dereference proof. Standard `Option`/`Result` `.as_ref()` and `.as_mut()` retain wrapper identity, and `Result::ok()` changes it to `Option`; a subsequent explicit payload projection is still required. Custom methods keep their declared return contracts.

Vue templates use HTML element boundaries and JavaScript expression parsing. Component imports, event handlers, qualified members, `v-for` iteration bindings, Pinia `defineStore` auto-imports (`nuxt.toml`), slot bindings, and arrow parameters retain lexical ownership and original coordinates. TypeScript/JavaScript parses IIFEs as lexical scopes, includes bundled DOM/WebIDL (`web.toml`) and Playwright (`playwright.toml`) receiver packs, and HTML supports syntax-activated Alpine.js rules (`ActivationCondition` / `FileActivationSignals`) alongside `.j2` template indexing. HTML classic scripts share verified document declarations; modules and inline event handlers retain separate lexical owners and ordinary JavaScript AST facts. Encoded handler expressions that require HTML entity decoding remain diagnostic evidence rather than guessed calls.

## Python registrations

For an indexed module that imports `FastMCP` from `fastmcp` or `mcp.server.fastmcp`, ACDD records `@server.tool` and `@server.tool(...)` on module-level functions when `server` is assigned a `FastMCP(...)` instance. Each confirmed tool has a `tool_registration` node, a `contains` edge from its module, and a `handles` edge to its function. A literal `name=` sets the advertised name; a dynamic name is marked unknown. Import aliases and module-qualified constructors are supported. Reassignment of the constructor before instance creation, or the server before registration, prevents a confirmed link.

The graph records registrations evident in source. Runtime-created tools, factory-returned servers, and imported server instances require runtime confirmation. Other decorator-based frameworks use their own registration rules; a method named `tool` alone does not identify FastMCP.

## HTML and configuration files

HTML and HTMX share one HTML Tree-sitter grammar and one language profile. HTMX attributes are read from the parsed HTML tags.

HTML supports file inspection, source snippets, syntax diagnostics, and nested AST search. Its module name retains the extension so `app.html` and `app.js` remain distinct. Local `script src` and `link href` references to JavaScript/TypeScript assets resolve when those targets are indexed. With `lang-typescript` enabled, inline JavaScript contributes declarations, imports and call references at their original coordinates. Classic scripts share a document scope; modules and event handlers retain their own scopes. Inline content in scripts with `src` or non-JavaScript data types is skipped.

Each HTMX request in `.html` and `.htm` files is a separate `htmx_request` node. `code_map_search` returns its method, URL, file, and line with normal pagination; `code_map_inspect(..., detail="full")` adds the URL status and same-element trigger, target, swap, include, and `hx-vals` status. The profile recognizes `hx-get`, `hx-post`, `hx-put`, `hx-patch`, `hx-delete`, and standard `data-hx-` forms. Elements that declare only supporting HTMX attributes become `htmx_context` nodes. `local_unverified` identifies a local-looking runtime route whose handler can be confirmed in server code. Remote URLs, template expressions, and current-page requests have distinct statuses. Static `hx-vals` records the key count; dynamic or invalid values receive a status. Request nodes are connected to their source file by `contains` relationships. Check runtime inheritance, selector matching, and server dispatch where those behaviors matter. See the [HTMX attribute reference](https://htmx.org/reference/) for runtime behavior.

YAML exposes block and flow mapping keys, including nested mappings. TOML exposes tables and array tables as namespaces and key/value pairs as variables. Quoted key spelling is preserved; repeated keys or array-table entries have distinct symbol IDs. Use a path and line selector when a repeated name is ambiguous.

These profiles use the same index, selectors, snippets, and incremental update path as other languages. Diagnostics describe syntax accepted by the grammar, not complete HTML, YAML, or TOML specification validation.
