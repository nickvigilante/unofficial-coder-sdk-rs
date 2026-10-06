"""Patch coder/coder's OpenAPI 3 spec so progenitor can generate a usable Rust client.

Every change is returned as a (rule, location, detail) tuple and written to the log,
so a regeneration PR shows exactly how the upstream spec differs from what progenitor accepts.
"""

import json
import sys

BINARY = {"type": "string", "format": "binary"}
REMAP = {"*/*": "application/octet-stream", "application/scim+json": "application/json", "text/event-stream": "application/octet-stream"}
METHODS = ("get", "put", "post", "patch", "delete")


def operations(spec):
    for path, item in spec.get("paths", {}).items():
        for method in METHODS:
            if method in item:
                yield f"{method.upper()} {path}", item[method]


def remap_content(content, where, log):
    for media in list(content):
        if media in REMAP:
            target = REMAP[media]
            body = content.pop(media)
            content[target] = {"schema": dict(BINARY)} if target == "application/octet-stream" else body
            log.append(("remap-media", where, f"{media} -> {target}"))


def fix_operations(spec, log):
    for where, op in operations(spec):
        body = op.get("requestBody")
        if body and len(body.get("content", {})) > 1:
            count = len(body["content"])
            body["content"] = {"application/octet-stream": {"schema": dict(BINARY)}}
            log.append(("multi-media-request", where, f"{count} media types -> application/octet-stream"))
        elif body:
            remap_content(body.get("content", {}), where, log)
        responses = op.get("responses", {})
        for code, response in responses.items():
            remap_content(response.get("content", {}), f"{where} {code}", log)
        success = sorted(c for c in responses if c.startswith("2") and c != "2XX")
        if len(success) < 2:
            continue
        bodies = {json.dumps(responses[c].get("content"), sort_keys=True) for c in success}
        if len(bodies) == 1:
            merged = responses[success[0]]
            for code in success:
                del responses[code]
            responses["2XX"] = merged
            log.append(("multi-success", where, f"merged identical {success} into 2XX"))
        else:
            for code in success[1:]:
                del responses[code]
            log.append(("multi-success", where, f"kept {success[0]} of {success} (LOSSY)"))


def walk(node, where, visit):
    if isinstance(node, dict):
        visit(node, where)
        for key, value in node.items():
            walk(value, f"{where}.{key}", visit)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, f"{where}[{index}]", visit)


def fix_array_format(spec, log):
    def visit(node, where):
        if node.get("type") == "array" and "format" in node and isinstance(node.get("items"), dict):
            fmt = node.pop("format")
            node["items"].setdefault("format", fmt)
            log.append(("array-format", where, f"moved format {fmt} to items"))

    walk(spec, "$", visit)


def fix_raw_fields(spec, raw_fields, log):
    schemas = spec.get("components", {}).get("schemas", {})
    for entry in raw_fields:
        props = schemas.get(entry["definition"], {}).get("properties", {})
        prop = props.get(entry["property"])
        if prop is None or prop.get("type") != "array":
            continue
        description = {"description": prop["description"]} if "description" in prop else {}
        if entry["kind"] == "json":
            props[entry["property"]] = description
        else:
            props[entry["property"]] = {"type": "string", "format": "byte", **description}
        log.append(("raw-field", f"{entry['definition']}.{entry['property']}", entry["kind"]))


def open_enums(spec, log):
    def visit(node, where):
        values = node.get("enum")
        if not isinstance(values, list):
            return
        del node["enum"]
        for key in [k for k in node if k.startswith("x-enum-")]:
            del node[key]
        if "allOf" not in node and "$ref" not in node:
            known = ", ".join(f"`{v}`" for v in values)
            existing = node.get("description", "")
            node["description"] = f"{existing} Known values: {known}.".strip()
        log.append(("open-enums", where, f"{len(values)} values"))

    walk(spec, "$", visit)


def apply(spec, raw_fields):
    log = []
    fix_operations(spec, log)
    fix_array_format(spec, log)
    fix_raw_fields(spec, raw_fields, log)
    open_enums(spec, log)
    return log


def main(argv):
    if len(argv) != 5:
        print("usage: patch_spec.py <openapi3.json> <rawfields.json> <out.json> <log>", file=sys.stderr)
        return 2
    with open(argv[1]) as f:
        spec = json.load(f)
    with open(argv[2]) as f:
        raw_fields = json.load(f)
    log = apply(spec, raw_fields)
    with open(argv[3], "w") as f:
        json.dump(spec, f, indent=1, sort_keys=True)
        f.write("\n")
    with open(argv[4], "w") as f:
        for rule, where, detail in log:
            f.write(f"{rule}\t{where}\t{detail}\n")
    counts = {}
    for rule, _, _ in log:
        counts[rule] = counts.get(rule, 0) + 1
    print(json.dumps(counts, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
