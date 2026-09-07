#!/usr/bin/env python3
"""Reference `wirk-embed/v1` backend over an installed `model2vec`.

Wirk speaks to an embedding backend across an argv/stdin/stdout boundary
(`wirk-atlas/src/semantic.rs`) and compiles none of it. This file is the
reference implementation of the *product's* side of that contract, and it
is deliberately the smallest thing that can honestly answer it:

  * it hard-codes no model, no cache directory, no interpreter and no
    host path — everything comes from the request on stdin;
  * it loads only what it was handed, from the local filesystem, with no
    network fetch and no fallback to a shared mutable cache;
  * it recomputes the model directory digest itself, under the same
    stated rule Wirk uses, and reports what it *actually* loaded. Wirk
    refuses the build when that disagrees with its own reading, so a
    backend that quietly substituted a different model cannot produce a
    selectable edition (ruling 0088's defect, checked from both sides).

Run it as `<python-with-model2vec> model2vec_embed.py`; the interpreter
and the model directory are the caller's configuration, recorded by Wirk
as build provenance.

Request  (stdin, NDJSON):
    {"protocol","model_path","rows","output","vector_format"}
    {"row": <n>, "text": <str>}   x rows, in row order
Reply    (stdout, one JSON line):
    {"protocol","backend","model_path","model_digest","rows","dimensions",
     "environment"?}
Vectors  are written to `output` as little-endian binary32, row-major,
    rows x dimensions, no header — `f32le-row-major/v1`.

`environment` is the optional provenance block
(`W4-LIFECYCLE-CORRECTION.md` item 3, `W4-PRODUCER-PROVENANCE-CORRECTION.md`
items 1-3): `backend` above is a free-form string, so two environments
with different `model2vec` builds reporting `model2vec/0.8.2` were
indistinguishable in the record. This backend therefore reports:

  * the environment root it actually ran in;
  * for every installed distribution it actually *imported*, the
    installer's own metadata directory, so Wirk can re-read `RECORD` and
    `METADATA` itself;
  * **every module object actually in `sys.modules` that claims one of
    those distributions**, named by the file `__file__` the interpreter
    really loaded it from. Wirk digests those files itself and checks each
    one against the paths the owning distribution's `RECORD` declares.

The third of those is the correction. A count of mismatched files is not
the identity of the changed bytes: two different edits to two different
files of one distribution both read `files_mismatched: 1`, and a package
placed earlier on `sys.path` than the installed one executes while
`importlib.metadata` still cheerfully attributes the *name* to the
installed distribution, whose own `RECORD` then verifies clean. Both
cases are ordinary — no hostile backend, no compromised upstream — and
both were invisible. Reporting the loaded origin, and letting Wirk digest
it, makes them the different identities they are.

Nothing here inventories the box: the module set is bounded by what this
process imported, and a module the interpreter cannot name a file for is
recorded as an explicit unmeasured entry rather than skipped. Distributions
that cannot be described are counted and named too, so a partial report
reads as partial. It remains bounded provenance, not execution
attestation: a backend that lies about which environment it ran in is
outside what any local argv boundary can check, and the record says so.
"""
from __future__ import annotations

import base64
import csv
import hashlib
import json
import os
import platform
import struct
import sys

PROTOCOL = "wirk-embed/v1"
VECTOR_FORMAT = "f32le-row-major/v1"


def fail(message: str) -> "NoReturn":  # noqa: F821
    print(message, file=sys.stderr)
    raise SystemExit(2)


def absorb(digest: "hashlib._Hash", part: bytes) -> None:
    digest.update(len(part).to_bytes(8, "big"))
    digest.update(part)


def model_directory_digest(directory: str) -> str:
    """`wirk-model-directory/v1`: every regular file below `directory`,
    keyed by its relative path, sorted, length-prefixed. Symlinks are
    followed, because a Hugging Face snapshot is entirely symlinks into a
    blob store and the blobs are what a loader actually reads."""
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
    """`sha256=<urlsafe-base64-no-padding>`, the only hash form a wheel
    `RECORD` uses in practice. Anything else is reported unchecked rather
    than guessed at."""
    algorithm, _, encoded = entry.partition("=")
    if algorithm != "sha256" or not encoded:
        return None
    padded = encoded + "=" * (-len(encoded) % 4)
    try:
        return algorithm, base64.urlsafe_b64decode(padded)
    except Exception:  # noqa: BLE001 - a malformed entry is "unchecked", not fatal
        return None


def verify_record(root: str, record_text: str) -> "tuple[int, int, int, int, int]":
    """Check every file a distribution's own `RECORD` declares against the
    bytes actually on disk. Returns
    (declared_files, declared_byte_len, checked, missing, mismatched)."""
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
    """Every module object in `sys.modules` that either (a) has a top-level
    name one of the reported distributions claims, or (b) actually loaded
    from one of the directories those distributions are installed in —
    named by the file the interpreter really loaded it from.

    Both halves are needed, and each catches what the other misses.
    Without (a), a package shadowing an installed one from elsewhere on
    `sys.path` is simply out of scope. Without (b), *removing* a
    distribution's `.dist-info` removes its name from
    `packages_distributions()` too, so its modules silently leave the
    report and a 15-of-16 measurement reads exactly like a 15-of-15 one —
    which is the defect this correction exists to close.

    This is the answer to "which bytes ran", as distinct from "which bytes
    the installer says are installed". `importlib.metadata` maps a *name*
    to a distribution; it never looks at `__file__`, so a package earlier
    on `sys.path` than the installed one is attributed to the installed
    distribution and that distribution's own `RECORD` still verifies
    clean. Reporting the origin lets Wirk check the two against each
    other.

    Returns (measured, unmeasured). A module the interpreter can name no
    file for — a builtin, a frozen or namespace package, an extension
    loaded from an archive — is an explicit `unmeasured` entry with the
    reason, never a silent omission: `W4-PRODUCER-PROVENANCE-CORRECTION.md`
    item 2 is precisely that a skipped thing must stay countable.
    """
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
        # (b): loaded from a directory the described distributions are
        # installed in. A module there that no distribution claims is
        # exactly the "its metadata went away" case.
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
                # Both spellings: `origin` is what the interpreter says it
                # loaded, `path` is that resolved, which is the file Wirk
                # opens. A symlinked install makes the two differ, and
                # which one a reader wants depends on the question.
                "origin": origin,
                "path": absolute,
                "digest": hashlib.sha256(content).hexdigest(),
                "byte_len": len(content),
                # The attribution `importlib.metadata` makes from the name
                # alone. Reported as the claim it is; Wirk decides whether
                # the loaded file is one the distribution actually declares.
                "claims": claimed,
            }
        )
    return measured, unmeasured


def environment_report() -> "dict | None":
    """The distributions this process actually imported, identified through
    the installer's own metadata, together with the modules that actually
    loaded. Bounded by what is in `sys.modules`, so it describes this
    embedding run rather than inventorying the box."""
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
    # Every distribution this run imported but could not describe, named
    # with the reason. Previously each of these was a bare `continue`, and
    # a 15-of-16 report was indistinguishable from a 15-of-15 one.
    undescribed = []
    for name in sorted(wanted):
        try:
            distribution = metadata.distribution(name)
            # `_path` is the installed `.dist-info` directory. There is no
            # public accessor for it; a distribution that does not expose
            # one is named as undescribed rather than described from a guess.
            dist_info = getattr(distribution, "_path", None)
            if dist_info is None or not os.path.isdir(str(dist_info)):
                undescribed.append(
                    {"name": name, "reason": "no readable .dist-info directory"}
                )
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
                    "record_digest": hashlib.sha256(record_bytes).hexdigest(),
                    "metadata_digest": hashlib.sha256(metadata_bytes).hexdigest(),
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
    # The directories the described distributions are installed in, which
    # is where an installed module is expected to have come from.
    roots = sorted({os.path.dirname(entry["metadata_path"]) for entry in distributions})
    modules, unmeasured = loaded_modules(
        {top: [name for name in names if name in described] for top, names in owners.items()},
        roots,
    )
    return {
        # v2: `modules`, `undescribed_distributions` and `unmeasured_modules`
        # are new, and the scheme name says so rather than letting a reader
        # of an older record assume a coverage it never had.
        "kind": "python-distributions/v2",
        # The environment prefix, not the resolved base interpreter: this
        # is the thing that actually supplies the implementation, and it
        # is exactly what canonicalizing a venv's `bin/python` loses.
        "root": sys.prefix,
        "runtime": f"{sys.implementation.name}/{platform.python_version()}",
        "executable": sys.executable,
        "distributions": distributions,
        "undescribed_distributions": undescribed,
        "modules": modules,
        "unmeasured_modules": unmeasured,
    }


def main() -> None:
    header_line = sys.stdin.readline()
    if not header_line.strip():
        fail("no request header on stdin")
    header = json.loads(header_line)
    if header.get("protocol") != PROTOCOL:
        fail(f"unsupported protocol {header.get('protocol')!r}; this backend speaks {PROTOCOL}")
    if header.get("vector_format") != VECTOR_FORMAT:
        fail(f"unsupported vector format {header.get('vector_format')!r}")
    model_path = header["model_path"]
    output_path = header["output"]
    expected_rows = int(header["rows"])

    if not os.path.isabs(model_path) or not os.path.isdir(model_path):
        fail(f"model_path {model_path!r} is not an existing absolute directory")

    texts: list[str] = []
    for line in sys.stdin:
        if not line.strip():
            continue
        row = json.loads(line)
        if row["row"] != len(texts):
            fail(f"rows arrived out of order at {row['row']}")
        texts.append(row["text"])
    if len(texts) != expected_rows:
        fail(f"header promised {expected_rows} rows, {len(texts)} arrived")

    # Imported only once the request has validated, so a malformed request
    # fails fast without paying for a model runtime import.
    from model2vec import StaticModel  # noqa: PLC0415
    import model2vec  # noqa: PLC0415

    # Offline by construction, in two independent ways: `model_path` was
    # checked above to be an existing local directory, which model2vec
    # 0.8.2 loads directly without touching the hub at all, and
    # `force_download=False` refuses to re-fetch it. Wirk additionally
    # runs this process with `HF_HUB_OFFLINE=1` in a cleared environment.
    # model2vec 0.8.2's `from_pretrained` has no `local_files_only`
    # parameter (checked against the installed signature), so claiming
    # one would be a silent no-op rather than a guarantee.
    model = StaticModel.from_pretrained(model_path, force_download=False)
    vectors = model.encode(texts, use_multiprocessing=False)

    rows = len(texts)
    dimensions = int(vectors.shape[1]) if rows else 0
    with open(output_path, "wb") as handle:
        for index in range(rows):
            handle.write(struct.pack(f"<{dimensions}f", *(float(v) for v in vectors[index])))
        handle.flush()
        os.fsync(handle.fileno())

    reply = {
        "protocol": PROTOCOL,
        "backend": f"model2vec/{getattr(model2vec, '__version__', 'unknown')}",
        "model_path": model_path,
        "model_digest": model_directory_digest(model_path),
        "rows": rows,
        "dimensions": dimensions,
    }
    # Reported after the embedding, so the set describes what this run
    # actually imported. Omitted entirely when it cannot be established,
    # which Wirk records as `unreported` rather than as an empty proof.
    environment = environment_report()
    if environment is not None:
        reply["environment"] = environment
    json.dump(reply, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
