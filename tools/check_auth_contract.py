"""Validate public auth artifacts, not runtime JWT signatures or permissions."""
import json
from pathlib import Path
import unittest
from jsonschema import Draft202012Validator, FormatChecker

AUTH = Path(__file__).resolve().parents[1] / "schemas" / "auth"

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result

def load(name):
    return json.loads((AUTH / name).read_text(), object_pairs_hook=unique_object)

class ContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.claim_schema = load("jwt-claims.v1.schema.json")
        cls.catalog_schema = load("scope-catalog.v1.schema.json")
        cls.catalog = load("scopes.v1.json")
        cls.claims = Draft202012Validator(cls.claim_schema, format_checker=FormatChecker())
        cls.valid = dict(iss="https://issuer.example.test/cm", aud="campaign-trail-api",
                         sub="principal-07", jti="credential-19", iat=1795737600,
                         exp=1827273600, client_id="client-03",
                         scope="cm:cards:read cm:escalations:resolve")

    def test_schemas_and_catalog(self):
        Draft202012Validator.check_schema(self.claim_schema)
        Draft202012Validator.check_schema(self.catalog_schema)
        Draft202012Validator(self.catalog_schema).validate(self.catalog)

    def test_claim_examples(self):
        self.claims.validate(self.valid)
        self.claims.validate(dict(self.valid, scope="", campaign_id="campaign-11"))
        self.claims.validate(dict(self.valid, scope="unrecognized-capability"))

    def test_missing_claims(self):
        for field in self.claim_schema["required"]:
            with self.subTest(field=field):
                claims = dict(self.valid)
                del claims[field]
                self.assertFalse(self.claims.is_valid(claims))

    def test_bad_shapes_and_extra_claims(self):
        cases = [dict(scope=["cm:cards:read"]),
                 dict(scope="cm:cards:read  cm:cards:write"), dict(aud="another-api"),
                 dict(iat="1795737600"), dict(exp=0), dict(jti=""),
                 dict(role="all"), dict(is_admin=True), dict(display_name="principal-07")]
        for change in cases:
            with self.subTest(change=change):
                self.assertFalse(self.claims.is_valid(dict(self.valid, **change)))

    def test_operation_coverage(self):
        declared, used, public = set(self.catalog["scopes"]), set(), set()
        for operation, rule in self.catalog["operations"].items():
            with self.subTest(operation=operation):
                required = set(rule["all_of"])
                self.assertTrue(required <= declared)
                if rule["public"]:
                    self.assertFalse(required)
                    public.add(operation)
                else:
                    self.assertTrue(required)
                used.update(required)
        self.assertEqual(used, declared)
        self.assertEqual(public, {"getJwtSchema", "getScopeCatalog", "getLineage"})

    def test_private_action_cannot_have_empty_requirements(self):
        catalog = json.loads(json.dumps(self.catalog))
        catalog["operations"]["newAction"] = {"public": False, "all_of": []}
        self.assertFalse(Draft202012Validator(self.catalog_schema).is_valid(catalog))

    def test_public_action_cannot_claim_scoped_access(self):
        catalog = json.loads(json.dumps(self.catalog))
        catalog["operations"]["getScopeCatalog"]["all_of"] = ["cm:cards:read"]
        self.assertFalse(Draft202012Validator(self.catalog_schema).is_valid(catalog))

    def test_duplicate_keys(self):
        with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
            json.loads('{"scope":"first","scope":"second"}', object_pairs_hook=unique_object)

if __name__ == "__main__":
    unittest.main(verbosity=2)
