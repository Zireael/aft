#!/usr/bin/env python3
"""
Patch AFT semantic-search-enhancement compile failures caused by new
SemanticBackendConfig fields not being listed in explicit struct literals.

Run from the repository root:
    python scripts/apply_semantic_backend_config_fix.py

The script is intentionally conservative:
- preserves existing LF/CRLF line endings by opening with newline=''
- patches only explicit `SemanticBackendConfig { ... }` literals without `..` spreads
- skips function signatures such as `fn foo() -> SemanticBackendConfig {`
- is idempotent; a second run should make no changes
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys

SEMANTIC_INDEX = Path("crates/aft/src/semantic_index.rs")
CONFIG = Path("crates/aft/src/config.rs")

INDEX_RERANK_FIELDS = [
    "rerank_api_type: crate::config::RerankApiType::Chat,",
    "rerank_max_candidate_chars_cross_encoder: 512,",
]
INDEX_CHUNK_FIELDS = [
    "max_embed_tokens: 512,",
    "chunk_overlap_tokens: 100,",
]
CONFIG_RERANK_FIELDS = [
    "rerank_api_type: RerankApiType::Chat,",
    "rerank_max_candidate_chars_cross_encoder: 512,",
]
CONFIG_CHUNK_FIELDS = [
    "max_embed_tokens: 512,",
    "chunk_overlap_tokens: 100,",
]
MAX_FILES_FIELD = "max_files: 20_000,"


@dataclass
class Block:
    start: int
    end: int
    text: str


def line_start(text: str, pos: int) -> int:
    return text.rfind("\n", 0, pos) + 1


def line_end(text: str, pos: int) -> int:
    end = text.find("\n", pos)
    return len(text) if end == -1 else end


def is_signature_line(text: str, marker_pos: int) -> bool:
    line = text[line_start(text, marker_pos):line_end(text, marker_pos)]
    return "fn " in line and "->" in line


def find_struct_literal_blocks(text: str, marker: str = "SemanticBackendConfig {") -> list[Block]:
    blocks: list[Block] = []
    pos = 0
    while True:
        marker_pos = text.find(marker, pos)
        if marker_pos == -1:
            break
        if is_signature_line(text, marker_pos):
            pos = marker_pos + len(marker)
            continue
        brace_pos = text.find("{", marker_pos)
        if brace_pos == -1:
            break

        depth = 0
        i = brace_pos
        # Good enough for these test/config literals. These blocks do not contain
        # Rust string literals with unmatched braces.
        while i < len(text):
            ch = text[i]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    end = i + 1
                    blocks.append(Block(marker_pos, end, text[marker_pos:end]))
                    pos = end
                    break
            i += 1
        else:
            raise RuntimeError(f"Unclosed SemanticBackendConfig block at byte {marker_pos}")
    return blocks


def insert_after_field(block: str, field_name: str, new_fields: list[str]) -> tuple[str, bool]:
    missing = [field for field in new_fields if field.split(":", 1)[0] not in block]
    if not missing:
        return block, False

    lines = block.splitlines(keepends=True)
    for idx, line in enumerate(lines):
        if re.search(rf"\b{re.escape(field_name)}\s*:", line):
            indent = re.match(r"\s*", line).group(0)
            nl = "\r\n" if line.endswith("\r\n") else "\n" if line.endswith("\n") else ""
            insertion = [f"{indent}{field}{nl}" for field in missing]
            lines[idx + 1:idx + 1] = insertion
            return "".join(lines), True
    return block, False


def patch_index_block(block: str) -> tuple[str, bool]:
    # Any struct update syntax must keep the spread last. These are already safe.
    if ".." in block:
        return block, False

    changed = False
    out = block

    out2, did = insert_after_field(out, "rerank_max_candidate_chars", INDEX_RERANK_FIELDS)
    out, changed = out2, changed or did

    if "max_files" not in out:
        out2, did = insert_after_field(out, "max_results_per_file", [MAX_FILES_FIELD, *INDEX_CHUNK_FIELDS])
        out, changed = out2, changed or did
    else:
        out2, did = insert_after_field(out, "max_files", INDEX_CHUNK_FIELDS)
        out, changed = out2, changed or did

    return out, changed


def patch_config_default(text: str) -> tuple[str, int]:
    marker = "impl Default for SemanticBackendConfig"
    start = text.find(marker)
    if start == -1:
        raise RuntimeError("Could not find impl Default for SemanticBackendConfig in config.rs")

    # Patch only the first SemanticBackendConfig default `Self { ... }` body inside this impl.
    self_pos = text.find("Self {", start)
    if self_pos == -1:
        raise RuntimeError("Could not find Self { in SemanticBackendConfig default impl")

    brace_pos = text.find("{", self_pos)
    depth = 0
    i = brace_pos
    while i < len(text):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                block_start, block_end = self_pos, i + 1
                break
        i += 1
    else:
        raise RuntimeError("Unclosed Self { block in config.rs")

    block = text[block_start:block_end]
    changed = False
    out = block
    out2, did = insert_after_field(out, "rerank_max_candidate_chars", CONFIG_RERANK_FIELDS)
    out, changed = out2, changed or did
    out2, did = insert_after_field(out, "max_files", CONFIG_CHUNK_FIELDS)
    out, changed = out2, changed or did

    if not changed:
        return text, 0
    return text[:block_start] + out + text[block_end:], 1


def patch_file(path: Path, patcher) -> int:
    original = path.read_text(encoding="utf-8", newline="")
    updated, count = patcher(original)
    if updated != original:
        path.write_text(updated, encoding="utf-8", newline="")
    return count


def patch_semantic_index(text: str) -> tuple[str, int]:
    blocks = find_struct_literal_blocks(text)
    replacements: list[tuple[int, int, str]] = []
    changed_count = 0

    for block in blocks:
        new_block, changed = patch_index_block(block.text)
        if changed:
            replacements.append((block.start, block.end, new_block))
            changed_count += 1

    if not replacements:
        return text, 0

    out = text
    for start, end, new_block in reversed(replacements):
        out = out[:start] + new_block + out[end:]
    return out, changed_count


def validate(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8", newline="")
    errors: list[str] = []
    for block in find_struct_literal_blocks(text):
        if ".." in block.text:
            continue
        required = [
            "rerank_api_type",
            "rerank_max_candidate_chars_cross_encoder",
            "max_embed_tokens",
            "chunk_overlap_tokens",
            "max_files",
        ]
        missing = [field for field in required if field not in block.text]
        if missing:
            line = text.count("\n", 0, block.start) + 1
            errors.append(f"{path}:{line}: missing {', '.join(missing)}")
    return errors


def main() -> int:
    root = Path.cwd()
    missing_paths = [p for p in (SEMANTIC_INDEX, CONFIG) if not (root / p).exists()]
    if missing_paths:
        print("Not at AFT repository root, or files are missing:", file=sys.stderr)
        for p in missing_paths:
            print(f"  - {p}", file=sys.stderr)
        return 2

    index_count = patch_file(root / SEMANTIC_INDEX, patch_semantic_index)
    config_count = patch_file(root / CONFIG, patch_config_default)

    errors = validate(root / SEMANTIC_INDEX)
    if errors:
        print("Validation failed:", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1

    print(f"Patched {index_count} explicit SemanticBackendConfig literal(s) in {SEMANTIC_INDEX}")
    print(f"Patched {config_count} Default impl block(s) in {CONFIG}")
    print("Run: bash scripts/zir-aft-check.sh quick --keep-going")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
