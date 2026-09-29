import unittest

import patch_spec


def op(**kw):
    return {"paths": {"/x": {"post": {"operationId": "x", **kw}}}, "components": {"schemas": {}}}


class PatchSpecTest(unittest.TestCase):
    def test_multi_media_request_collapses_to_binary(self):
        spec = op(requestBody={"content": {"image/png": {}, "text/plain": {}}}, responses={})
        log = patch_spec.apply(spec, [])
        body = spec["paths"]["/x"]["post"]["requestBody"]["content"]
        self.assertEqual(body, {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}})
        self.assertEqual(log[0][0], "multi-media-request")

    def test_remap_star_media_type(self):
        spec = op(responses={"404": {"content": {"*/*": {"schema": {"type": "object"}}}}})
        patch_spec.apply(spec, [])
        content = spec["paths"]["/x"]["post"]["responses"]["404"]["content"]
        self.assertEqual(content, {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}})

    def test_array_format_moves_to_items(self):
        spec = op(parameters=[{"name": "ids", "in": "query", "schema": {"type": "array", "format": "uuid", "items": {"type": "string"}}}], responses={})
        patch_spec.apply(spec, [])
        schema = spec["paths"]["/x"]["post"]["parameters"][0]["schema"]
        self.assertNotIn("format", schema)
        self.assertEqual(schema["items"]["format"], "uuid")

    def test_identical_success_responses_merge_to_2xx(self):
        body = {"content": {"application/json": {"schema": {"type": "object"}}}}
        spec = op(responses={"200": dict(body), "201": dict(body)})
        patch_spec.apply(spec, [])
        self.assertEqual(list(spec["paths"]["/x"]["post"]["responses"]), ["2XX"])

    def test_different_success_responses_keep_first_and_log_lossy(self):
        spec = op(responses={"200": {"content": {"application/json": {"schema": {"type": "object"}}}}, "204": {}})
        log = patch_spec.apply(spec, [])
        self.assertEqual(list(spec["paths"]["/x"]["post"]["responses"]), ["200"])
        self.assertIn("LOSSY", log[0][2])

    def test_raw_fields_become_free_form_or_base64(self):
        spec = {"paths": {}, "components": {"schemas": {"codersdk.Part": {"type": "object", "properties": {
            "args": {"type": "array", "items": {"type": "integer"}, "description": "Args."},
            "data": {"type": "array", "items": {"type": "integer"}},
            "ids": {"type": "array", "items": {"type": "integer"}},
        }}}}}
        raw = [
            {"definition": "codersdk.Part", "property": "args", "kind": "json"},
            {"definition": "codersdk.Part", "property": "data", "kind": "bytes"},
            {"definition": "codersdk.Missing", "property": "x", "kind": "json"},
        ]
        patch_spec.apply(spec, raw)
        props = spec["components"]["schemas"]["codersdk.Part"]["properties"]
        self.assertEqual(props["args"], {"description": "Args."})
        self.assertEqual(props["data"], {"type": "string", "format": "byte"})
        self.assertEqual(props["ids"], {"type": "array", "items": {"type": "integer"}})

    def test_open_enums_removes_enum_and_documents_values(self):
        spec = {"paths": {}, "components": {"schemas": {
            "codersdk.ChatStatus": {"type": "string", "enum": ["waiting", "running"], "x-enum-varnames": ["A", "B"]},
            "codersdk.User": {"type": "object", "properties": {"status": {"enum": ["active"], "allOf": [{"$ref": "#/components/schemas/codersdk.UserStatus"}]}}},
        }}}
        patch_spec.apply(spec, [])
        status = spec["components"]["schemas"]["codersdk.ChatStatus"]
        self.assertEqual(status, {"type": "string", "description": "Known values: `waiting`, `running`."})
        user_status = spec["components"]["schemas"]["codersdk.User"]["properties"]["status"]
        self.assertNotIn("enum", user_status)

    def test_apply_is_idempotent(self):
        spec = op(requestBody={"content": {"image/png": {}, "text/plain": {}}}, responses={"200": {}, "201": {}})
        patch_spec.apply(spec, [])
        self.assertEqual(patch_spec.apply(spec, []), [])


if __name__ == "__main__":
    unittest.main()
