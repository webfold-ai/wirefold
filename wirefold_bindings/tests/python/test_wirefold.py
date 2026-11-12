"""Tests for the `wirefold` Python module. Run with scripts/test-python.sh, which builds the
extension first."""

import json
import unittest

import wirefold


class Translate(unittest.TestCase):
    def test_json_to_yaml_and_back_keeps_the_data(self):
        src = {"name": "w", "tags": ["a", "b"], "n": 3, "ok": True, "none": None}
        yaml = wirefold.translate(json.dumps(src), "json", "yaml")
        self.assertIn("name: w", yaml)
        self.assertEqual(json.loads(wirefold.translate(yaml, "yaml", "json")), src)

    def test_names_are_case_and_space_insensitive(self):
        self.assertEqual(
            wirefold.translate("{}", " JSON ", "Yaml"),
            wirefold.translate("{}", "json", "yaml"),
        )

    def test_keyword_arguments(self):
        out = wirefold.translate("[1,2]", from_format="json", to_format="yaml")
        self.assertIn("1", out)

    def test_bad_input_raises_value_error_naming_the_format(self):
        with self.assertRaises(ValueError) as cm:
            wirefold.translate("{", "json", "yaml")
        self.assertIn("cannot read the input as json", str(cm.exception))

    def test_unknown_format_lists_the_valid_ones(self):
        with self.assertRaises(ValueError) as cm:
            wirefold.translate("{}", "json", "xml")
        self.assertIn("expected one of json, yaml", str(cm.exception))


class Msgpack(unittest.TestCase):
    def test_round_trip(self):
        src = {"a": 1, "b": [True, None, "x"], "c": {"d": 2.5}}
        packed = wirefold.json_to_msgpack(json.dumps(src))
        self.assertIsInstance(packed, bytes)
        self.assertEqual(json.loads(wirefold.msgpack_to_json(packed)), src)

    def test_msgpack_is_smaller_than_the_json(self):
        text = json.dumps({"key": list(range(50))})
        self.assertLess(len(wirefold.json_to_msgpack(text)), len(text))

    def test_hostile_bytes_raise_instead_of_crashing(self):
        for bad in (b"", b"\xc1", b"\xdd\xff\xff\xff\xff", b"\x91" * 10_000):
            with self.assertRaises(ValueError):
                wirefold.msgpack_to_json(bad)

    def test_not_json_raises(self):
        with self.assertRaises(ValueError):
            wirefold.json_to_msgpack("{nope")


class Meta(unittest.TestCase):
    def test_formats_and_version(self):
        self.assertEqual(wirefold.formats()[:2], ["json", "yaml"])
        self.assertRegex(wirefold.version(), r"^\d+\.\d+\.\d+")


if __name__ == "__main__":
    unittest.main()
