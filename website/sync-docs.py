#!/usr/bin/env python3
"""Assemble the human-oriented Zola documentation tree in website/content/docs/.

Organizes curated guides (website/pages/*.md.in), embedded profiles
(crates/acdd-state/src/profile/embedded/*.yaml), and live canonical repo docs
into 4 top-level sections:
  1. ACDD State     (/docs/state/)
  2. ACDD Index     (/docs/index/)
  3. ACDD Lint      (/docs/lint/)
  4. ACDD Profiles  (/docs/profiles/)
"""

from __future__ import annotations

import re
import shutil
from pathlib import Path

SITE_DIR = Path(__file__).resolve().parent
WORKTREE_ROOT = SITE_DIR.parent
PAGES_DIR = SITE_DIR / "pages"
EMBEDDED_PROFILES_DIR = WORKTREE_ROOT / "crates" / "acdd-state" / "src" / "profile" / "embedded"
OUT_DOCS = SITE_DIR / "content" / "docs"
GITHUB_BLOB = "https://github.com/ContextUnity/contextunity-acdd/blob/main"

ALLOWED_FRONTMATTER_KEYS = {
    "title",
    "description",
    "date",
    "updated",
    "weight",
    "draft",
    "slug",
    "path",
    "aliases",
    "authors",
    "template",
    "in_search_index",
    "sort_by",
    "page_template",
    "paginate_by",
    "render",
    "transparent",
    "insert_anchor_links",
}

ALERT_TITLES = {
    "NOTE": "Note",
    "TIP": "Tip",
    "IMPORTANT": "Important",
    "WARNING": "Warning",
    "CAUTION": "Caution",
}

# Exact repo-relative doc -> website route mapping
ROUTE_MAP = {
    "README.md": "/docs/",
    "INSTALL.md": "/docs/state/install/",
    "docs/README.md": "/docs/",
    "docs/reference/acdd.md": "/docs/state/",
    "docs/reference/tasks.md": "/docs/state/state-machine/",
    "docs/reference/mcp-setup.md": "/docs/state/mcp-cli-setup/",
    "docs/reference/cli.md": "/docs/state/cli/",
    "docs/reference/mcp-tools.md": "/docs/state/mcp-tools/",
    "docs/reference/configuration.md": "/docs/index/acdd-yaml/",
    "docs/runbooks/limits-and-freshness.md": "/docs/index/acdd-yaml/#limits-and-index-freshness",
    "docs/reference/languages.md": "/docs/index/languages/",
    "docs/reference/framework-manifests.md": "/docs/index/framework-manifests/",
    "docs/runbooks/framework-packs.md": "/docs/index/framework-manifests/#workspace-framework-rules",
    "docs/reference/profiles.md": "/docs/profiles/profile-options/",
    "docs/reference/acdd_profile.yaml.example": "/docs/profiles/overriding-profiles/",
}

BUILTIN_PROFILE_DOCS = [
    (
        "base.yaml",
        "profiles/builtin-base.md",
        40,
        "Built-in Profile: base",
        "The foundation profile supplying default Git worktree isolation, `commit` save policy, core execution/review guidance, standard `paths`/`claims`/`administration` review contours, and resource `limits:`.",
    ),
    (
        "audit.yaml",
        "profiles/builtin-audit.md",
        50,
        "Built-in Profile: audit",
        "Read-only 3-phase security and correctness audit profile (`recon` -> `analyze` -> `triage`) using `git.isolation: in_place`, `save: none`, strict JSON Schema proofs, and Markdown reports under `.acdd/reports/{run_name}.md`.",
    ),
    (
        "documentation.yaml",
        "profiles/builtin-documentation.md",
        60,
        "Built-in Profile: documentation",
        "Compiler-free living documentation and invariant lifecycle (`blueprint_synthesis` -> per-task `build`/`review`/`deliver` -> `verify_docs` -> `polish` -> `integration_review` -> `handoff`) gated on `docs.links` and `code.lint` (`[arch, docs]`).",
    ),
    (
        "spec_planning.yaml",
        "profiles/builtin-spec-planning.md",
        70,
        "Built-in Profile: spec_planning",
        "In-place architectural planning workflow (`synthesize` -> `review`) validating `acdd.yaml` `specs.*` collections (`docs.specs`), `docs.links`, `code.lint` (`[arch, docs]`), and disjoint same-wave task scopes.",
    ),
]


def strip_and_parse_frontmatter(text: str) -> tuple[dict[str, str], str]:
    if not text.startswith("---\n"):
        return {}, text
    end = text.find("\n---\n", 4)
    if end == -1:
        return {}, text
    raw_fm = text[4:end]
    body = text[end + 5 :]
    meta: dict[str, str] = {}
    for line in raw_fm.splitlines():
        if ":" in line and not line.startswith(" ") and not line.startswith("-"):
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip().strip('"').strip("'")
    return meta, body


def infer_title(body: str, fallback: str) -> str:
    for line in body.splitlines():
        m = re.match(r"^#\s+(.+?)\s*$", line)
        if m:
            return m.group(1).strip()
    return fallback.replace("-", " ").replace("_", " ").title()


def yaml_quote(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def build_frontmatter(
    *,
    title: str,
    weight: int,
    is_section: bool,
    extra: dict[str, str],
) -> str:
    lines = ["---", f"title: {yaml_quote(title)}", f"weight: {weight}"]
    if is_section:
        lines.append('sort_by: "weight"')
        lines.append('template: "docs_section.html"')
        lines.append('page_template: "docs_page.html"')
        lines.append('insert_anchor_links: "right"')
    else:
        lines.append('template: "docs_page.html"')
        lines.append('insert_anchor_links: "right"')
    if extra:
        lines.append("extra:")
        for k, v in sorted(extra.items()):
            lines.append(f"  {k}: {yaml_quote(v)}")
    lines.append("---\n")
    return "\n".join(lines)


def convert_github_alerts(body: str) -> str:
    lines = body.splitlines()
    out: list[str] = []
    i = 0
    in_fence = False

    while i < len(lines):
        line = lines[i]
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            out.append(line)
            i += 1
            continue

        if not in_fence:
            m = re.match(r"^>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\](.*)$", line.strip())
            if m:
                kind = m.group(1)
                inline_rest = m.group(2).strip()
                title = inline_rest if inline_rest else ALERT_TITLES[kind]
                alert_lines: list[str] = []
                i += 1
                while i < len(lines) and lines[i].lstrip().startswith(">"):
                    stripped = re.sub(r"^>\s?", "", lines[i].lstrip())
                    alert_lines.append(stripped)
                    i += 1
                inner = "\n".join(alert_lines).strip()
                out.append(f'<div class="callout callout-{kind.lower()}">')
                out.append(f'<div class="callout-title">{title}</div>')
                if inner:
                    out.append("")
                    out.append(inner)
                    out.append("")
                out.append("</div>")
                continue

        out.append(line)
        i += 1

    return "\n".join(out) + "\n"


def rewrite_links(body: str, rel_repo_path: Path | None) -> str:
    if rel_repo_path is None:
        return body

    link_re = re.compile(r"(!?\[[^\]]*\])\(([^)\s]+)\)")

    def resolve_target(raw_target: str) -> str:
        if raw_target.startswith(("http://", "https://", "mailto:", "#", "/", "data:")):
            return raw_target

        if "#" in raw_target:
            path_part, frag = raw_target.split("#", 1)
            frag_suffix = f"#{frag}"
        else:
            path_part, frag_suffix = raw_target, ""

        if not path_part:
            return raw_target

        resolved = (WORKTREE_ROOT / rel_repo_path.parent / path_part).resolve()
        try:
            repo_rel = resolved.relative_to(WORKTREE_ROOT)
        except ValueError:
            return raw_target

        repo_rel_str = repo_rel.as_posix()
        if repo_rel_str in ROUTE_MAP:
            base = ROUTE_MAP[repo_rel_str]
            if "#" in base and frag_suffix:
                return base.split("#", 1)[0] + frag_suffix
            return f"{base}{frag_suffix}"

        return f"{GITHUB_BLOB}/{repo_rel_str}{frag_suffix}"

    out_lines: list[str] = []
    in_fence = False
    for line in body.splitlines():
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            out_lines.append(line)
            continue
        if in_fence:
            out_lines.append(line)
            continue
        out_lines.append(link_re.sub(lambda m: f"{m.group(1)}({resolve_target(m.group(2))})", line))
    return "\n".join(out_lines) + "\n"


def escape_zola_shortcodes(body: str) -> str:
    if "{{" not in body and "{%" not in body:
        return body
    return body.replace("{%", "{% raw %}{%{% endraw %}").replace("{{", "{% raw %}{{{% endraw %}")


def process_markdown_body(raw_text: str, rel_repo_path: Path | None) -> tuple[dict[str, str], str]:
    meta, body = strip_and_parse_frontmatter(raw_text)
    body = rewrite_links(body, rel_repo_path)
    body = convert_github_alerts(body)
    body = escape_zola_shortcodes(body)
    return meta, body.lstrip("\n")


def write_doc(
    dst_rel: str,
    *,
    title: str,
    weight: int,
    is_section: bool,
    body: str,
    extra: dict[str, str] | None = None,
) -> None:
    dst_path = OUT_DOCS / dst_rel
    dst_path.parent.mkdir(parents=True, exist_ok=True)
    fm = build_frontmatter(title=title, weight=weight, is_section=is_section, extra=extra or {})
    dst_path.write_text(fm + "\n" + body.lstrip("\n"), encoding="utf-8")


def emit_from_repo(
    src_rel: str,
    dst_rel: str,
    *,
    weight: int,
    title: str | None = None,
    is_section: bool = False,
) -> None:
    rel = Path(src_rel)
    raw = (WORKTREE_ROOT / rel).read_text(encoding="utf-8")
    meta, body = process_markdown_body(raw, rel)
    resolved_title = title or meta.get("title") or infer_title(body, rel.stem)
    extra = {k: v for k, v in meta.items() if k not in ALLOWED_FRONTMATTER_KEYS}
    write_doc(dst_rel, title=resolved_title, weight=weight, is_section=is_section, body=body, extra=extra)


def emit_from_curated(
    curated_name: str,
    dst_rel: str,
    *,
    weight: int,
    title: str,
    is_section: bool = False,
) -> None:
    raw = (PAGES_DIR / curated_name).read_text(encoding="utf-8")
    meta, body = process_markdown_body(raw, None)
    write_doc(dst_rel, title=title, weight=weight, is_section=is_section, body=body, extra=meta)


def strip_first_h1(body: str) -> str:
    lines = body.splitlines()
    for idx, line in enumerate(lines):
        if line.startswith("# "):
            return "\n".join(lines[:idx] + lines[idx + 1 :]).lstrip("\n")
    return body


def main() -> None:
    if OUT_DOCS.exists():
        shutil.rmtree(OUT_DOCS)
    OUT_DOCS.mkdir(parents=True, exist_ok=True)

    # Documentation Hub root
    emit_from_curated("docs_root.md.in", "_index.md", weight=1, title="Documentation", is_section=True)

    # 1. ACDD State
    emit_from_curated("state_specs_index.md.in", "state/_index.md", weight=10, title="ACDD State", is_section=True)
    emit_from_repo("docs/reference/tasks.md", "state/state-machine.md", weight=10, title="State Machine, Tasks & Receipts")
    emit_from_repo("INSTALL.md", "state/install.md", weight=20, title="Installation")
    emit_from_repo("docs/reference/mcp-setup.md", "state/mcp-cli-setup.md", weight=30, title="MCP & CLI Setup")
    emit_from_repo("docs/reference/cli.md", "state/cli.md", weight=40, title="CLI Reference")
    emit_from_repo("docs/reference/mcp-tools.md", "state/mcp-tools.md", weight=50, title="MCP Tools Reference")

    # 2. ACDD Index
    emit_from_curated("index_index.md.in", "index/_index.md", weight=20, title="ACDD Index", is_section=True)
    cfg_raw = (WORKTREE_ROOT / "docs/reference/configuration.md").read_text(encoding="utf-8")
    _, cfg_body = process_markdown_body(cfg_raw, Path("docs/reference/configuration.md"))
    lim_raw = (WORKTREE_ROOT / "docs/runbooks/limits-and-freshness.md").read_text(encoding="utf-8")
    _, lim_body = process_markdown_body(lim_raw, Path("docs/runbooks/limits-and-freshness.md"))
    lim_section = "## Limits and index freshness\n\n" + strip_first_h1(lim_body)
    write_doc(
        "index/acdd-yaml.md",
        title="acdd.yaml & Limits",
        weight=10,
        is_section=False,
        body=cfg_body.rstrip() + "\n\n---\n\n" + lim_section,
    )
    emit_from_repo("docs/reference/languages.md", "index/languages.md", weight=20, title="Languages & Coverage")
    fm_raw = (WORKTREE_ROOT / "docs/reference/framework-manifests.md").read_text(encoding="utf-8")
    _, fm_body = process_markdown_body(fm_raw, Path("docs/reference/framework-manifests.md"))
    fp_raw = (WORKTREE_ROOT / "docs/runbooks/framework-packs.md").read_text(encoding="utf-8")
    _, fp_body = process_markdown_body(fp_raw, Path("docs/runbooks/framework-packs.md"))
    fp_lines = []
    for line in strip_first_h1(fp_body).splitlines():
        if line.strip() == "## Report a real-control comparison":
            break
        fp_lines.append(line)
    fp_section = "## Workspace framework rules\n\n" + "\n".join(fp_lines).strip()
    write_doc(
        "index/framework-manifests.md",
        title="Framework Manifests & Rules",
        weight=30,
        is_section=False,
        body=fm_body.rstrip() + "\n\n---\n\n" + fp_section + "\n",
    )

    # 3. ACDD Lint
    emit_from_curated("lint_index.md.in", "lint/_index.md", weight=30, title="ACDD Lint", is_section=True)
    emit_from_curated(
        "lint_writing_docs_and_rules.md.in",
        "lint/writing-docs-and-rules.md",
        weight=10,
        title="Writing Docs & Linter Rules",
    )

    # 4. ACDD Profiles
    emit_from_curated("gates_profiles_index.md.in", "profiles/_index.md", weight=40, title="ACDD Profiles", is_section=True)
    emit_from_curated("gates_profile_options.md.in", "profiles/profile-options.md", weight=10, title="Profile Options & Variables")
    emit_from_curated("gates_overriding_profiles.md.in", "profiles/overriding-profiles.md", weight=20, title="Overriding Built-in Profiles")

    # Built-in `code` profile: walkthrough + full verbatim embedded YAML
    code_intro_raw = (PAGES_DIR / "gates_builtin_code_intro.md.in").read_text(encoding="utf-8")
    _, code_intro_body = process_markdown_body(code_intro_raw, None)
    code_yaml = escape_zola_shortcodes((EMBEDDED_PROFILES_DIR / "code.yaml").read_text(encoding="utf-8"))
    write_doc(
        "profiles/builtin-code.md",
        title="Built-in Profile: code",
        weight=30,
        is_section=False,
        body=code_intro_body.rstrip() + "\n\n---\n\n## Full embedded YAML (`code.yaml`)\n\n```yaml\n" + code_yaml.rstrip() + "\n```\n",
    )

    # Other 4 embedded profiles (full verbatim YAML + summary)
    for filename, dst_rel, weight, title, summary in BUILTIN_PROFILE_DOCS:
        raw_yaml = escape_zola_shortcodes((EMBEDDED_PROFILES_DIR / filename).read_text(encoding="utf-8"))
        body = (
            f"# {title}\n\n"
            f"{summary}\n\n"
            f"Source: [`crates/acdd-state/src/profile/embedded/{filename}`]({GITHUB_BLOB}/crates/acdd-state/src/profile/embedded/{filename})\n\n"
            f"## Full embedded YAML (`{filename}`)\n\n"
            f"```yaml\n{raw_yaml.rstrip()}\n```\n"
        )
        write_doc(dst_rel, title=title, weight=weight, is_section=False, body=body)

    count = sum(1 for _ in OUT_DOCS.rglob("*.md"))
    print(f"Synced {count} documentation files into {OUT_DOCS.relative_to(WORKTREE_ROOT)}")


if __name__ == "__main__":
    main()
