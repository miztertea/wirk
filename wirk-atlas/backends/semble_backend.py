#!/usr/bin/env python3
"""Reference `wirk-embed/v2` and `wirk-query/v1` backend over an installed
`semble` (P3 W4 B, `W4-PUBLIC-RETRIEVAL-BUILD.md`).

Two protocols, one file, because they must agree about one thing: the
*ranking representation*. What this backend chunks at build time is what
it ranks at query time, and both halves are the installed native
implementation used the way it uses itself — `semble.chunking.chunk_source`
for boundaries, `semble.index.bm25.BM25` + `semble.index.sparse.enrich_for_bm25`
for the sparse side, `semble.index.dense.SelectableBasicBackend` for the
dense side and `semble.search.search` for the ranking itself. No rank
function is copied, retuned or patched here; nothing below re-implements a
chunker.

`wirk-embed/v2` (build). Wirk sends the *committed blob bytes* of every
admitted resource, and the ranking path string it has frozen for them.
This backend returns, per native chunk, the byte range **in those original
bytes**, plus the digest of the ranking text and the resolved language,
and writes the vectors. It does not decide what is admitted, what a
coordinate is, or what gets published.

    stdin (NDJSON)
      {"protocol":"wirk-embed/v2","mode":"chunk-embed"|"embed","model_path",
       "output","chunks","scratch","vector_format","inputs"}
      mode "chunk-embed": {"input":i,"ranking_path":str,"bytes_file":path}
      mode "embed":       {"input":i,"text":str}
    stdout (one JSON line)
      {"protocol","backend","chunker"?,"model_path","model_digest",
       "rows","dimensions","unmapped","environment"?}
    `chunks`  NDJSON, one line per produced row, in vector row order
              (mode "chunk-embed" only; in mode "embed" Wirk already owns
              the boundaries and this file is not written).
    `output`  vectors, f32le row-major, rows x dimensions, no header.

Blob bytes travel as a file path rather than inline: they are committed
bytes of arbitrary size, the product already owns a private staging
directory for the build, and encoding a repository through a JSON string
would buy nothing.

The byte range is *derived*, never searched for. `semble` reads a file as
`Path.read_text(encoding="utf-8", errors="replace")`, so the text it chunks
is a universal-newline, lossy-decoded derivative of the bytes and the
character offsets its boundary functions compute are offsets into *that*
string (`native-chunk-boundary-use/HANDOFF.md` N1). This backend therefore
decodes the original bytes while recording, per produced character, the
original byte it came from; asserts the reconstructed string is exactly
what the production reader returns for the same bytes; re-runs the very
boundary functions `chunk_source` runs and proves each boundary positionally
(`source[start:end] == chunk.content`, never a substring search — four
chunks of a repeated stanza are identical text); and only then projects.
A resource whose reconstruction does not match is reported `unmapped`
with its reason and contributes no row. Wirk re-derives the same text from
the same committed bytes on its own side and refuses the build if the two
digests disagree.

`wirk-query/v1` (query). Wirk sends exactly the admitted rows and their
already-built vectors; this backend composes a native in-memory index over
those rows only and ranks with `semble.search.search`. It embeds one thing
— the query — and writes nothing.

    stdin (NDJSON)
      {"protocol":"wirk-query/v1","model_path","vectors","rows","dimensions",
       "query","top_k"}
      {"row":i,"ranking_path":str,"slot":j,"text":str,"start_line","end_line",
       "language"}                                                   x rows
    stdout   {"protocol","backend","native","model_path","model_digest",
              "returned","environment"?} then one line per result
              {"row":i,"score":f,"rank":n}

Neither protocol hard-codes a model, a cache directory, an interpreter or
a host path: everything comes from the request on stdin.
"""
from __future__ import annotations

import base64
import codecs
import csv
import hashlib
import json
import os
import platform
import secrets
import shutil
import struct
import sys
import tempfile
from pathlib import Path

EMBED_PROTOCOL = "wirk-embed/v2"
QUERY_PROTOCOL = "wirk-query/v1"
VECTOR_FORMAT = "f32le-row-major/v1"

TEXT_IDENTITY = "identity"
TEXT_NORMALIZED = "universal-newline+utf8-replace/v1"


def fail(message: str) -> "NoReturn":  # noqa: F821
    print(message, file=sys.stderr)
    raise SystemExit(2)


def absorb(digest: "hashlib._Hash", part: bytes) -> None:
    digest.update(len(part).to_bytes(8, "big"))
    digest.update(part)


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ---- provenance ----------------------------------------------------------
#
# Identical in contract to `model2vec_embed.py`'s: the distributions this
# process actually imported, the modules that actually loaded, and an
# explicit entry for everything skipped. Kept here rather than imported so
# that the `wirk-embed/v1` reference backend stays exactly the file whose
# bytes every existing edition record already binds.


def model_directory_digest(directory: str) -> str:
    """`wirk-model-directory/v1`: every regular file below `directory`, keyed
    by its relative path, sorted, length-prefixed. Symlinks followed."""
    files = []
    for root, _directories, names in os.walk(directory, followlinks=True):
        for name in names:
            absolute = os.path.join(root, name)
            if os.path.isfile(absolute):
                files.append((os.path.relpath(absolute, directory).encode(), absolute))
    files.sort(key=lambda pair: pair[0])
    digest = hashlib.sha256()
    absorb(digest, b"wirk-model-directory/v1")
    absorb(digest, len(files).to_bytes(8, "big"))
    for relative, absolute in files:
        with open(absolute, "rb") as handle:
            content = handle.read()
        absorb(digest, relative)
        absorb(digest, content)
    return digest.hexdigest()


def record_hash(entry: str) -> "tuple[str, bytes] | None":
    algorithm, _, encoded = entry.partition("=")
    if algorithm != "sha256" or not encoded:
        return None
    padded = encoded + "=" * (-len(encoded) % 4)
    try:
        return algorithm, base64.urlsafe_b64decode(padded)
    except Exception:  # noqa: BLE001 - a malformed entry is "unchecked", not fatal
        return None


def verify_record(root: str, record_text: str) -> "tuple[int, int, int, int, int]":
    declared = 0
    declared_bytes = 0
    checked = missing = mismatched = 0
    for line in csv.reader(record_text.splitlines()):
        if not line:
            continue
        relative = line[0]
        entry = line[1] if len(line) > 1 else ""
        size = line[2] if len(line) > 2 else ""
        declared += 1
        if size.isdigit():
            declared_bytes += int(size)
        absolute = os.path.join(root, relative)
        if not os.path.isfile(absolute):
            missing += 1
            continue
        parsed = record_hash(entry)
        if parsed is None:
            continue
        with open(absolute, "rb") as handle:
            actual = hashlib.sha256(handle.read()).digest()
        checked += 1
        if actual != parsed[1]:
            mismatched += 1
    return declared, declared_bytes, checked, missing, mismatched


def loaded_modules(
    owners: "dict[str, list[str]]", roots: "list[str]"
) -> "tuple[list[dict], list[dict]]":
    measured: list[dict] = []
    unmeasured: list[dict] = []
    prefixes = tuple(os.path.join(root, "") for root in roots)
    for name in sorted(sys.modules):
        top = name.split(".", 1)[0]
        claimed = sorted(owners.get(top, ()))
        module = sys.modules.get(name)
        if module is None:
            if claimed:
                unmeasured.append({"name": name, "reason": "module entry is None"})
            continue
        origin = getattr(module, "__file__", None)
        if not origin:
            if claimed:
                spec = getattr(module, "__spec__", None)
                reason = "no __file__: {}".format(
                    getattr(spec, "origin", None) or "builtin, frozen or namespace package"
                )
                unmeasured.append({"name": name, "reason": reason})
            continue
        try:
            absolute = os.path.abspath(os.path.realpath(origin))
        except OSError as error:
            if claimed:
                unmeasured.append({"name": name, "reason": f"{origin} is unresolvable: {error}"})
            continue
        inside = absolute.startswith(prefixes) or os.path.abspath(origin).startswith(prefixes)
        if not claimed and not inside:
            continue
        try:
            with open(absolute, "rb") as handle:
                content = handle.read()
        except OSError as error:
            unmeasured.append({"name": name, "reason": f"{origin} is unreadable: {error}"})
            continue
        measured.append(
            {
                "name": name,
                "origin": origin,
                "path": absolute,
                "digest": sha256_hex(content),
                "byte_len": len(content),
                "claims": claimed,
            }
        )
    return measured, unmeasured


def environment_report() -> "dict | None":
    try:
        import importlib.metadata as metadata  # noqa: PLC0415

        owners = metadata.packages_distributions()
    except Exception:  # noqa: BLE001 - no metadata is "unreported", not a failure
        return None
    wanted: set[str] = set()
    for module in list(sys.modules):
        top = module.split(".", 1)[0]
        for name in owners.get(top, ()):
            wanted.add(name)
    distributions = []
    undescribed = []
    for name in sorted(wanted):
        try:
            distribution = metadata.distribution(name)
            dist_info = getattr(distribution, "_path", None)
            if dist_info is None or not os.path.isdir(str(dist_info)):
                undescribed.append({"name": name, "reason": "no readable .dist-info directory"})
                continue
            record_path = os.path.join(str(dist_info), "RECORD")
            metadata_path = os.path.join(str(dist_info), "METADATA")
            missing = [
                base
                for base, path in (("RECORD", record_path), ("METADATA", metadata_path))
                if not os.path.isfile(path)
            ]
            if missing:
                undescribed.append(
                    {"name": name, "reason": f"{'/'.join(missing)} absent from .dist-info"}
                )
                continue
            with open(record_path, "rb") as handle:
                record_bytes = handle.read()
            with open(metadata_path, "rb") as handle:
                metadata_bytes = handle.read()
            declared, declared_bytes, checked, missing_files, mismatched = verify_record(
                os.path.dirname(str(dist_info)), record_bytes.decode("utf-8", "replace")
            )
            distributions.append(
                {
                    "name": name,
                    "version": distribution.version,
                    "metadata_path": os.path.abspath(str(dist_info)),
                    "record_digest": sha256_hex(record_bytes),
                    "metadata_digest": sha256_hex(metadata_bytes),
                    "declared_files": declared,
                    "declared_byte_len": declared_bytes,
                    "files_checked": checked,
                    "files_missing": missing_files,
                    "files_mismatched": mismatched,
                }
            )
        except Exception as error:  # noqa: BLE001, PERF203 - one unreadable distribution is not the run
            undescribed.append({"name": name, "reason": f"{type(error).__name__}: {error}"})
            continue
    if not distributions:
        return None
    described = {entry["name"] for entry in distributions}
    roots = sorted({os.path.dirname(entry["metadata_path"]) for entry in distributions})
    modules, unmeasured = loaded_modules(
        {top: [name for name in names if name in described] for top, names in owners.items()},
        roots,
    )
    return {
        "kind": "python-distributions/v2",
        "root": sys.prefix,
        "runtime": f"{sys.implementation.name}/{platform.python_version()}",
        "executable": sys.executable,
        "distributions": distributions,
        "undescribed_distributions": undescribed,
        "modules": modules,
        "unmeasured_modules": unmeasured,
    }


# ---- the exact original-byte mapping -------------------------------------


def decode_with_offsets(data: bytes) -> "tuple[str, list[int]]":
    """Decode `data` the way `semble.index.files.read_file_text` does, and
    record for every produced character the offset of the original byte it
    began at.

    Two transformations, in the order Python's text layer applies them:
    UTF-8 with `errors="replace"`, then universal-newline translation. The
    result is checked against the production reader itself before any
    offset derived from it is used, so nothing here is trusted for being
    plausible.
    """
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    raw: list[str] = []
    raw_offsets: list[int] = []
    pending = 0  # first byte offset not yet attributed to a character
    for index in range(len(data)):
        produced = decoder.decode(data[index : index + 1], index + 1 == len(data))
        for step, character in enumerate(produced):
            raw.append(character)
            # A run of bytes [pending..index] produced these characters. One
            # character is the ordinary case and starts at `pending`; several
            # only happen on error recovery, where each is at most one byte
            # wide, so they are attributed in order. Every attribution is
            # verified below by reconstructing the chunk bytes.
            raw_offsets.append(min(pending + step, index))
        if produced:
            pending = index + 1
    tail = decoder.decode(b"", True)
    for step, character in enumerate(tail):
        raw.append(character)
        raw_offsets.append(min(pending + step, max(len(data) - 1, 0)))

    text: list[str] = []
    offsets: list[int] = []
    index = 0
    while index < len(raw):
        character = raw[index]
        if character == "\r":
            text.append("\n")
            offsets.append(raw_offsets[index])
            index += 2 if index + 1 < len(raw) and raw[index + 1] == "\n" else 1
        else:
            text.append(character)
            offsets.append(raw_offsets[index])
            index += 1
    offsets.append(len(data))  # exclusive end of the last character
    return "".join(text), offsets


def native_boundaries(source: str, language: "str | None") -> "list":
    """Re-run the very boundary functions `chunk_source` runs.

    `semble.chunking.core` is not re-exported by `semble.chunking.__all__`
    and `_DESIRED_CHUNK_LENGTH_CHARS` is a module constant with an upstream
    `# TODO: make this configurable`. Both are pinned dependencies of this
    integration rather than a stable public API, so both are capability
    checked here and their identities are reported to Wirk, which records
    them in the edition. The alternative — locating each returned chunk in
    the source by substring — is unsound: a repeated stanza produces
    several identical chunk texts.
    """
    from semble.chunking import core as chunking_core  # noqa: PLC0415
    from semble.chunking import chunking as chunking_module  # noqa: PLC0415

    for attribute in ("chunk", "chunk_lines"):
        if not callable(getattr(chunking_core, attribute, None)):
            fail(f"installed semble.chunking.core has no callable {attribute}")
    desired = getattr(chunking_module, "_DESIRED_CHUNK_LENGTH_CHARS", None)
    if not isinstance(desired, int):
        fail("installed semble.chunking.chunking has no _DESIRED_CHUNK_LENGTH_CHARS")
    if not source.strip():
        return []
    boundaries = None
    if language is not None and language_gate(chunking_core)(language):
        boundaries = chunking_core.chunk(source, language, desired)
    if boundaries is None:
        boundaries = chunking_core.chunk_lines(source, desired)
    return boundaries


def language_gate(chunking_core: "object"):
    """The condition the *installed* `chunk_source` puts in front of
    `core.chunk`, as that installation states it.

    Up to semble 0.5.5 that condition is
    `core.is_supported_language(language)`: the language has to be in the
    grammar pack's table before a parser is asked for. Upstream #238
    (0.5.6) replaced `tree-sitter-language-pack` with `semble-grammars`,
    removed `is_supported_language` entirely, and made `chunk_source` call
    `chunk` for every non-`None` language, leaving the parser's own `None`
    return to decide the fallback.

    This is a capability check, not a compatibility shim with a private
    opinion: whichever gate the installed `chunk_source` applies, the
    boundary re-run applies the same one, and `chunker_identity` reports
    which. Guessing wrong is not silent — `run_embed` compares the
    recovered boundaries with `chunk_source`'s own chunks position by
    position and refuses the resource if they disagree.
    """
    gate = getattr(chunking_core, "is_supported_language", None)
    if callable(gate):
        return gate
    return lambda language: True


def language_gate_identity(chunking_core: "object") -> str:
    """Which of the two gates above is in force, recorded on the edition
    because it is an actual input to every boundary this build produces."""
    if callable(getattr(chunking_core, "is_supported_language", None)):
        return "core.is_supported_language"
    return "any-language-parser-fallback"


def grammar_identity(chunking_core: "object") -> str:
    """The grammar implementation that actually supplies parsers, found
    rather than guessed from a fixed package list.

    The frozen version of this named `tree_sitter` and
    `tree_sitter_language_pack`. Against 0.5.6 that reports
    `tree_sitter_language_pack/absent` and says nothing at all about
    `semble-grammars`, which is where every parser now comes from — a
    grammar bump that moves boundaries would leave the recorded identity
    unchanged. So the provider is read off `core.get_parser` itself, and
    its distribution resolved from the module that defines it.
    """
    import importlib.metadata as metadata  # noqa: PLC0415

    parts = []
    provider = getattr(chunking_core, "get_parser", None)
    provider_module = getattr(provider, "__module__", None)
    if provider_module:
        parts.append(f"provider={provider_module}")
        top_level = provider_module.split(".")[0]
        distributions = set()
        try:
            for name in metadata.packages_distributions().get(top_level, []):
                distributions.add(name)
        except Exception:  # noqa: BLE001 - an undescribed provider is reported, not fatal
            pass
        if not distributions:
            distributions = {top_level}
        for name in sorted(distributions):
            try:
                parts.append(f"{name}/{metadata.version(name)}")
            except Exception:  # noqa: BLE001, PERF203
                parts.append(f"{name}/undescribed")
    else:
        parts.append("provider=absent")
    try:
        parts.append(f"tree_sitter/{metadata.version('tree_sitter')}")
    except Exception:  # noqa: BLE001
        parts.append("tree_sitter/absent")
    return " ".join(parts)


def grammar_libraries(chunking_core: "object") -> dict:
    """The parser shared libraries this run actually loaded, by their bytes.

    `grammar_identity` names the provider and resolves its distribution
    version, and a version string is all it can be. `semble_grammars`
    loads every grammar from a shared library it extracts out of its
    bundled archive into a cache directory (`SEMBLE_GRAMMARS_CACHE_DIR`,
    else `~/.cache/semble/grammars/<version>`). That file is never a
    `sys.modules` entry, so it is in neither the chunker identity nor the
    loaded-module environment record — and upstream's `extract_atomic`
    returns early *without* re-checking its sha256 once the destination
    exists. A grammar library that changes under an unchanged version
    therefore moves boundaries and moves nothing that is recorded.

    So the files that actually decided these boundaries are reported
    here, from the loader's own registry of what it loaded, with the
    archive manifest's declared digest carried beside each one rather
    than assumed to hold. Where the parsers come from something else,
    that is said in words instead: an unenumerable provider is missing
    coverage, never absent risk (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md`
    O1).
    """
    provider = getattr(chunking_core, "get_parser", None)
    provider_module = getattr(provider, "__module__", None) or "absent"
    if provider_module != "semble_grammars.loader":
        return {
            "state": "unavailable",
            "provider": provider_module,
            "reason": (
                f"parsers come from {provider_module}, whose loaded grammar files this "
                "integration cannot enumerate; no grammar byte is covered by this record"
            ),
        }
    import semble_grammars.loader as grammar_loader  # noqa: PLC0415

    try:
        cache_root = str(grammar_loader.cache_dir())
    except Exception as error:  # noqa: BLE001 - reported, not fatal
        cache_root = f"unresolved: {error}"
    declared: dict = {}
    manifest_error = None
    try:
        manifest = grammar_loader._platform_manifest()
        for language, entry in manifest["languages"].items():
            record = declared.setdefault(
                entry["file"], {"sha256": entry["sha256"], "languages": []}
            )
            record["languages"].append(language)
    except Exception as error:  # noqa: BLE001
        manifest_error = str(error)

    loaded = dict(getattr(grammar_loader, "_loaded_libraries", {}) or {})
    libraries = []
    uncovered = []
    if manifest_error is not None:
        uncovered.append(
            {
                "name": "bundled archive manifest",
                "reason": (
                    f"unreadable ({manifest_error}), so no loaded library's bytes can be "
                    "compared with what the archive declares"
                ),
            }
        )
    for path in sorted(str(entry) for entry in loaded):
        try:
            with open(path, "rb") as handle:
                data = handle.read()
        except OSError as error:
            uncovered.append({"name": path, "reason": f"loaded but unreadable: {error}"})
            continue
        entry = declared.get(os.path.basename(path))
        libraries.append(
            {
                "path": os.path.abspath(path),
                "digest": sha256_hex(data),
                "byte_len": len(data),
                "languages": sorted(entry["languages"]) if entry else [],
                "declared_digest": entry["sha256"] if entry else None,
            }
        )
    if not libraries and not uncovered:
        return {
            "state": "none_loaded",
            "provider": provider_module,
            "cache_root": cache_root,
            "reason": (
                "no parser shared library was loaded in this process, so every boundary this "
                "build produced came from the line chunker"
            ),
        }
    return {
        "state": "measured",
        "provider": provider_module,
        "cache_root": cache_root,
        "libraries": libraries,
        "uncovered": uncovered,
    }


def chunker_identity() -> dict:
    """Everything that actually determines a boundary, as the installed
    implementation states it. The resolved language is per resource and is
    reported on each row, not here (`native-chunk-boundary-use` N4)."""
    from semble.chunking import chunking as chunking_module  # noqa: PLC0415
    from semble.chunking import core as chunking_core  # noqa: PLC0415
    import semble  # noqa: PLC0415

    files = {}
    for module in (chunking_module, chunking_core):
        path = os.path.abspath(getattr(module, "__file__", ""))
        if path and os.path.isfile(path):
            with open(path, "rb") as handle:
                files[module.__name__] = {
                    "path": path,
                    "digest": sha256_hex(handle.read()),
                    "byte_len": os.path.getsize(path),
                }
    return {
        "implementation": f"semble/{getattr(semble, '__version__', 'unknown')}",
        "entry_point": "semble.chunking.chunk_source",
        "constants": (
            f"desired_chunk_length_chars={chunking_module._DESIRED_CHUNK_LENGTH_CHARS};"
            f"min_chunk_size={chunking_core._MIN_CHUNK_SIZE};"
            f"recursion_depth={chunking_core._RECURSION_DEPTH};"
            f"language_gate={language_gate_identity(chunking_core)}"
        ),
        # Boundaries come from parse trees, so a grammar bump changes the
        # output. Neither provider exposes a `__version__`, so the
        # installer's own metadata is the version of record.
        "parsers": grammar_identity(chunking_core),
        "files": files,
    }


# ---- wirk-embed/v2 -------------------------------------------------------


def run_embed(header: dict) -> None:
    from semble.chunking import chunk_source  # noqa: PLC0415
    from semble.index.files import detect_language, read_file_text  # noqa: PLC0415
    from pathlib import Path  # noqa: PLC0415

    mode = header.get("mode")
    if mode not in ("chunk-embed", "embed"):
        fail(f"unsupported mode {mode!r}")
    if header.get("vector_format") != VECTOR_FORMAT:
        fail(f"unsupported vector format {header.get('vector_format')!r}")
    model_path = header["model_path"]
    if not os.path.isabs(model_path) or not os.path.isdir(model_path):
        fail(f"model_path {model_path!r} is not an existing absolute directory")

    inputs = []
    for line in sys.stdin:
        if line.strip():
            inputs.append(json.loads(line))
    if len(inputs) != int(header["inputs"]):
        fail(f"header promised {header['inputs']} inputs, {len(inputs)} arrived")
    for index, entry in enumerate(inputs):
        if entry.get("input") != index:
            fail(f"inputs arrived out of order at {entry.get('input')}")

    rows: list[dict] = []
    texts: list[str] = []
    unmapped: list[dict] = []
    identity = None

    if mode == "embed":
        # Wirk already fixed every boundary (one generation unit per row)
        # and already owns the text; this side only embeds it.
        texts = [entry["text"] for entry in inputs]
        rows = texts
    else:
        identity = chunker_identity()
        scratch = header["scratch"]
        if not os.path.isabs(scratch) or not os.path.isdir(scratch):
            fail(f"scratch {scratch!r} is not an existing absolute directory")
        for entry in inputs:
            with open(entry["bytes_file"], "rb") as handle:
                data = handle.read()
            ranking_path = entry["ranking_path"]

            # The production reader is the ground truth for the decoded
            # text; the offset-tracking decoder is trusted only once it
            # reproduces it byte for byte.
            with tempfile.NamedTemporaryFile(dir=scratch, delete=False) as handle:
                handle.write(data)
                temporary = handle.name
            try:
                canonical = read_file_text(Path(temporary))
            finally:
                os.unlink(temporary)
            source, offsets = decode_with_offsets(data)
            if source != canonical:
                unmapped.append(
                    {
                        "path": ranking_path,
                        "reason": (
                            "the offset-tracking decode does not reproduce the reader's text "
                            f"({len(source)} vs {len(canonical)} characters); no exact "
                            "original-byte mapping is available for this resource"
                        ),
                    }
                )
                continue

            language = detect_language(Path(ranking_path))
            chunks = chunk_source(source, ranking_path, language)
            boundaries = native_boundaries(source, language)
            if len(chunks) != len(boundaries):
                unmapped.append(
                    {
                        "path": ranking_path,
                        "reason": (
                            f"{len(chunks)} chunks but {len(boundaries)} recovered boundaries; "
                            "the pinned boundary entry point no longer matches chunk_source"
                        ),
                    }
                )
                continue
            failed = None
            projected: list[dict] = []
            projected_texts: list[str] = []
            for slot, (chunk, boundary) in enumerate(zip(chunks, boundaries)):
                end_index = max(boundary.end - 1, boundary.start)
                if source[boundary.start : end_index + 1] != chunk.content:
                    failed = f"chunk {slot} is not the text at its own recovered boundary"
                    break
                byte_start = offsets[boundary.start]
                byte_end = offsets[end_index + 1] if end_index + 1 < len(offsets) else len(data)
                # The decisive local check: these original bytes must
                # normalise back to exactly the text that was chunked and
                # will be ranked. No substring search anywhere.
                recovered, _ = decode_with_offsets(data[byte_start:byte_end])
                if recovered != chunk.content:
                    failed = (
                        f"chunk {slot} bytes [{byte_start},{byte_end}) do not normalise back to "
                        "its own text"
                    )
                    break
                encoded = chunk.content.encode("utf-8")
                projected.append(
                    {
                        "input": entry["input"],
                        "slot": slot,
                        "byte_start": byte_start,
                        "byte_end": byte_end,
                        "language": chunk.language,
                        "text_digest": sha256_hex(encoded),
                        "text_normalization": (
                            TEXT_IDENTITY
                            if encoded == data[byte_start:byte_end]
                            else TEXT_NORMALIZED
                        ),
                        "text_byte_len": len(encoded),
                    }
                )
                projected_texts.append(chunk.content)
            if failed is not None:
                unmapped.append({"path": ranking_path, "reason": failed})
                continue
            rows.extend(projected)
            texts.extend(projected_texts)

    from model2vec import StaticModel  # noqa: PLC0415
    import model2vec  # noqa: PLC0415

    model = StaticModel.from_pretrained(model_path, force_download=False)
    if texts:
        vectors = model.encode(texts, use_multiprocessing=False)
        dimensions = int(vectors.shape[1])
    else:
        vectors = []
        dimensions = 0
    with open(header["output"], "wb") as handle:
        for index in range(len(texts)):
            handle.write(struct.pack(f"<{dimensions}f", *(float(v) for v in vectors[index])))
        handle.flush()
        os.fsync(handle.fileno())
    if mode == "chunk-embed":
        with open(header["chunks"], "w", encoding="utf-8") as handle:
            for row in rows:
                handle.write(json.dumps(row, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())

    reply = {
        "protocol": EMBED_PROTOCOL,
        "backend": f"model2vec/{getattr(model2vec, '__version__', 'unknown')}",
        "model_path": model_path,
        "model_digest": model_directory_digest(model_path),
        "rows": len(texts),
        "dimensions": dimensions,
        "unmapped": unmapped,
    }
    if identity is not None:
        # Measured *after* the chunking loop, because a grammar library
        # is extracted and loaded lazily by the language a resource
        # resolves to: before the loop the loader has loaded nothing.
        from semble.chunking import core as chunking_core  # noqa: PLC0415

        identity["grammars"] = grammar_libraries(chunking_core)
        reply["chunker"] = identity
    environment = environment_report()
    if environment is not None:
        reply["environment"] = environment
    json.dump(reply, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


# ---- wirk-query/v1 -------------------------------------------------------



def cached_index_identity(header: dict) -> "tuple[Path, str] | None":
    """The directory Wirk owns for this exact view's index, and the key it
    is kept under -- or None when Wirk offered neither, which is every
    caller older than this field and every call where the product declined
    to keep anything."""
    directory = header.get("index_cache")
    key = header.get("index_key")
    if not directory or not key:
        return None
    return Path(directory), str(key)


def load_cached_bm25(header: dict, chunk_ids: "list[str]") -> "BM25 | None":
    """A previously built index for exactly these rows, or None.

    Refused unless four things hold: the identity Wirk computed for this
    view is the identity the stored index was written under, the `semble`
    that would rank through it is the `semble` that built it, the stored
    `index.json` bytes hash to the digest recorded alongside them (so a
    postings file replaced, truncated, or torn from its identity file is
    caught here rather than ranked on), and the documents it actually
    holds are the documents this view sends. Any other outcome -- absent,
    unreadable, from another version, from another view, mismatched --
    is not a failure, it is a build.

    The digest is checked against the exact bytes handed to `BM25.load`:
    they are read once, verified, then loaded from a private copy so a
    second, unchecked read of `index.json` off disk never happens."""
    from semble.index.bm25 import BM25  # noqa: PLC0415
    import semble  # noqa: PLC0415

    identity = cached_index_identity(header)
    if identity is None:
        return None
    directory, key = identity
    try:
        stored = json.loads((directory / "identity.json").read_bytes())
    except (OSError, ValueError):
        return None
    if not isinstance(stored, dict):
        return None
    if (
        stored.get("protocol") != QUERY_PROTOCOL
        or stored.get("index_key") != key
        or stored.get("native") != f"semble/{getattr(semble, '__version__', 'unknown')}"
    ):
        return None
    expected_digest = stored.get("index_sha256")
    if not isinstance(expected_digest, str) or not expected_digest:
        return None
    try:
        index_bytes = (directory / "index.json").read_bytes()
    except OSError:
        return None
    if sha256_hex(index_bytes) != expected_digest:
        return None
    verified = directory / f".tmp-verified-{os.getpid()}-{secrets.token_hex(8)}"
    try:
        verified.mkdir(parents=True, exist_ok=False)
        (verified / "index.json").write_bytes(index_bytes)
        index = BM25.load(verified)
    except Exception:  # noqa: BLE001 - a damaged index is a rebuild, never an error
        return None
    finally:
        shutil.rmtree(verified, ignore_errors=True)
    if index.doc_order != chunk_ids:
        return None
    return index


def save_cached_bm25(header: dict, index: "BM25", chunk_ids: "list[str]") -> None:
    """Keep this index where Wirk said to, or keep nothing.

    Written through a private temporary directory and renamed into place,
    so a reader either sees a whole index or sees none: two queries over
    the same view race to write the same bytes, and neither may show the
    other a half-written one. Failing to keep it costs the next query a
    rebuild and nothing else, so nothing here raises."""
    import semble  # noqa: PLC0415

    identity = cached_index_identity(header)
    if identity is None:
        return
    directory, key = identity
    staging = directory / f".tmp-{os.getpid()}-{secrets.token_hex(8)}"
    try:
        index.save(staging)
        # The digest of the exact bytes `index.save` just wrote -- not a
        # digest of the in-memory index -- so `load_cached_bm25` verifies
        # what it is actually about to decode, not a description of it.
        index_digest = sha256_hex((staging / "index.json").read_bytes())
        (staging / "identity.json").write_bytes(
            json.dumps(
                {
                    "protocol": QUERY_PROTOCOL,
                    "index_key": key,
                    "native": f"semble/{getattr(semble, '__version__', 'unknown')}",
                    "documents": len(chunk_ids),
                    "index_sha256": index_digest,
                }
            ).encode()
        )
        for name in ("index.json", "identity.json"):
            os.replace(staging / name, directory / name)
    except OSError:
        pass
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def run_query(header: dict) -> None:
    import numpy as np  # noqa: PLC0415
    from vicinity.backends.basic import BasicArgs  # noqa: PLC0415

    from semble.index.bm25 import BM25  # noqa: PLC0415
    from semble.index.dense import SelectableBasicBackend  # noqa: PLC0415
    from semble.index.sparse import enrich_for_bm25  # noqa: PLC0415
    from semble.index.types import make_chunk_id  # noqa: PLC0415
    from semble.search import search as native_search  # noqa: PLC0415
    from semble.tokens import tokenize  # noqa: PLC0415
    from semble.types import Chunk  # noqa: PLC0415
    import semble  # noqa: PLC0415

    model_path = header["model_path"]
    if not os.path.isabs(model_path) or not os.path.isdir(model_path):
        fail(f"model_path {model_path!r} is not an existing absolute directory")
    expected_rows = int(header["rows"])
    dimensions = int(header["dimensions"])
    top_k = int(header["top_k"])

    rows = []
    for line in sys.stdin:
        if line.strip():
            rows.append(json.loads(line))
    if len(rows) != expected_rows:
        fail(f"header promised {expected_rows} rows, {len(rows)} arrived")
    for index, row in enumerate(rows):
        if row["row"] != index:
            fail(f"rows arrived out of order at {row['row']}")

    with open(header["vectors"], "rb") as handle:
        raw = handle.read()
    if len(raw) != expected_rows * dimensions * 4:
        fail(
            f"admitted vector view is {len(raw)} bytes; {expected_rows} rows of {dimensions} "
            f"dimensions is {expected_rows * dimensions * 4}"
        )
    vectors = np.frombuffer(raw, dtype="<f4").reshape(expected_rows, dimensions).astype(np.float32)

    chunks = [
        Chunk(
            content=row["text"],
            file_path=row["ranking_path"],
            start_line=int(row["start_line"]),
            end_line=int(row["end_line"]),
            language=row["language"],
        )
        for row in rows
    ]
    # `semble` ranks through dicts keyed by `Chunk`, which is a frozen
    # dataclass compared by value: two rows that are equal in every field
    # would silently merge and one coordinate would be lost. The admitted
    # view is refused rather than ranked in that case; it has never
    # occurred, because the ranking path is source-qualified and two chunks
    # of one file differ in their line span.
    seen: dict[tuple, int] = {}
    for index, chunk in enumerate(chunks):
        key = (chunk.content, chunk.file_path, chunk.start_line, chunk.end_line, chunk.language)
        if key in seen:
            fail(
                f"admitted rows {seen[key]} and {index} are indistinguishable to the native ranker "
                f"({chunk.file_path}:{chunk.start_line}-{chunk.end_line})"
            )
        seen[key] = index
    back: dict[int, int] = {id(chunk): index for index, chunk in enumerate(chunks)}

    chunk_ids = [make_chunk_id(row["ranking_path"], int(row["slot"])) for row in rows]
    if len(set(chunk_ids)) != len(chunk_ids):
        fail("admitted rows collide on a native document id")
    # The sparse index over these rows is a pure function of them, and
    # Wirk has already told us, in `index_key`, exactly which rows these
    # are: it digests the coordinate and the verified ranking-text digest
    # of every row, in view order, under the producer configuration these
    # very functions were read from. So an index built for that key ranks
    # the same documents with the same tokens, and `semble`'s own
    # `BM25.save`/`BM25.load` are what move it — no rank function, tokeniser
    # or posting list is re-implemented here to make that possible.
    bm25_index = load_cached_bm25(header, chunk_ids)
    if bm25_index is None:
        bm25_index = BM25()
        for chunk_id, chunk in zip(chunk_ids, chunks):
            bm25_index.add_document(chunk_id, tokenize(enrich_for_bm25(chunk)))
        bm25_index.set_doc_order(chunk_ids)
        save_cached_bm25(header, bm25_index, chunk_ids)
    else:
        bm25_index.set_doc_order(chunk_ids)
    semantic_index = SelectableBasicBackend(vectors, BasicArgs())

    from model2vec import StaticModel  # noqa: PLC0415
    import model2vec  # noqa: PLC0415

    model = StaticModel.from_pretrained(model_path, force_download=False)
    results = native_search(
        header["query"], model, semantic_index, bm25_index, chunks, top_k=top_k
    )

    reply = {
        "protocol": QUERY_PROTOCOL,
        "backend": f"model2vec/{getattr(model2vec, '__version__', 'unknown')}",
        "native": f"semble/{getattr(semble, '__version__', 'unknown')}",
        "model_path": model_path,
        "model_digest": model_directory_digest(model_path),
        "returned": len(results),
    }
    environment = environment_report()
    if environment is not None:
        reply["environment"] = environment
    json.dump(reply, sys.stdout)
    sys.stdout.write("\n")
    for rank, result in enumerate(results, 1):
        index = back.get(id(result.chunk))
        if index is None:
            fail("the native ranker returned a chunk this view did not send")
        sys.stdout.write(
            json.dumps({"row": index, "score": float(result.score), "rank": rank}) + "\n"
        )
    sys.stdout.flush()


def main() -> None:
    header_line = sys.stdin.readline()
    if not header_line.strip():
        fail("no request header on stdin")
    header = json.loads(header_line)
    protocol = header.get("protocol")
    if protocol == EMBED_PROTOCOL:
        run_embed(header)
    elif protocol == QUERY_PROTOCOL:
        run_query(header)
    else:
        fail(
            f"unsupported protocol {protocol!r}; this backend speaks "
            f"{EMBED_PROTOCOL} and {QUERY_PROTOCOL}"
        )


if __name__ == "__main__":
    main()
