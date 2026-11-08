#!/usr/bin/env python3
"""Generate tests/fixtures/json_strict_cases.txt: the accept/reject verdict of Python's `json`
module (the RFC 8259 reference used by tests/parser_dos.rs) for hand-picked and seeded random inputs.

    python3 -I tests/fixtures/gen_json_strict_cases.py > tests/fixtures/json_strict_cases.txt

Each line is `A <hex>` (the input is valid) or `R <hex>` (invalid); <hex> is the UTF-8 input.

Python's `json` is made as strict as RFC 8259 by: rejecting NaN/Infinity/-Infinity (not JSON), and by
the three DELIBERATE deviations our parser makes on top of the RFC grammar, applied here so the
corpus states the verdict we expect from `parse_surface(Surface::Json, ..)`:
  * duplicate object keys are rejected (RFC 8259 s4 leaves the behaviour to the receiver and
    parsers disagree on which value wins; Python/serde keep the last, others the first),
  * a lone surrogate escape (`"\\ud800"`) is rejected (Rust `String` cannot hold it),
  * nesting deeper than 128 is rejected (serde_json's limit; Python's is its recursion limit).
"""
import json
import random
import sys


def no_const(name):
    raise ValueError(name)


def no_dups(pairs):
    keys = [k for k, _ in pairs]
    if len(keys) != len(set(keys)):
        raise ValueError("duplicate key")
    return dict(pairs)


def has_lone_surrogate(v):
    if isinstance(v, str):
        return any(0xD800 <= ord(c) <= 0xDFFF for c in v)
    if isinstance(v, list):
        return any(has_lone_surrogate(x) for x in v)
    if isinstance(v, dict):
        return any(has_lone_surrogate(k) or has_lone_surrogate(x) for k, x in v.items())
    return False


def depth(v):
    if isinstance(v, list):
        return 1 + max([depth(x) for x in v], default=0)
    if isinstance(v, dict):
        return 1 + max([depth(x) for x in v.values()], default=0)
    return 0


def verdict(s):
    try:
        v = json.loads(s, parse_constant=no_const, object_pairs_hook=no_dups)
    except (ValueError, RecursionError):
        return "R"
    if has_lone_surrogate(v) or depth(v) > 128:
        return "R"
    return "A"


HAND = [
    # valid
    "null", "true", "false", "0", "-0", "1", "-1", "10", "1.5", "-1.5e10", "1E+2", "1e-2", "0.0",
    "0e0", '""', '"a"', '"\\u00e9"', '"\\ud83d\\ude00"', '"\\/\\b\\f\\n\\r\\t\\"\\\\"', "[]", "{}",
    "[1,2,3]", '{"a":1}', '{"a":{"b":[1,{"c":null}]}}', " \t\r\n[ 1 , 2 ]\n", '{ "a" : 1 , "b" : 2 }',
    '"é中\U0001f600"', '"\x7f"', '{"":1}', '{"a":1,"b":1}',
    # RFC 8259 s7: control characters must be escaped
    '"\n"', '"\t"', '"\r"', '"\x00"', '"\x1f"', '"a\x01b"', '["\n"]', '{"a\tb":1}',
    # RFC 8259 s7: only the listed escapes
    '"\\q"', '"\\a"', '"\\x41"', '"\\0"', '"\\\'"', '"\\ "', '"\\u"', '"\\u12"', '"\\u12G4"',
    '"\\U0041"', '"\\', '"abc', '"\\ud800"', '"\\udc00"', '"\\ud800\\u0041"', '"\\ud800x"',
    # RFC 8259 s6: numbers
    "01", "-01", "00", "[00]", "0123", "1.", ".5", "-.5", "+1", "1e", "1e+", "1.e1", "-", "--1",
    "0x10", "1_000", "1.5.3", "1e5e5", "NaN", "Infinity", "-Infinity", "nan", "inf", "1 2",
    "1e400", "-1e400", "123456789012345678901234567890", "0.1e-9999",
    # trailing / missing commas, bad structure
    "[1,]", "[,]", "[,1]", "[1,,2]", '{"a":1,}', '{,}', '{,"a":1}', '{"a":1,,"b":2}', "[1 2]",
    '{"a" 1}', '{"a":}', '{"a"}', "{1:2}", "{'a':1}", "{a:1}", "['a']", "[1", "{", "[", "]", "}",
    '{"a":1', '{"a":1}}', "[1]]", "[1]x", "", " ", "\n", "nul", "nulll", "True", "TRUE", "tru",
    "truee", '"a" "b"', "[]]", "[}", "{]", '{"a":[}', "// c\n1", "/* c */1", "1 // c", "﻿1",
    " 1", "\x0b1", "\x0c1", "1\x00", "\x001",
    # duplicate keys (deliberate deviation: rejected)
    '{"a":1,"a":2}', '{"a":1,"a":1}', '{"a":{"x":1,"x":2}}', '[{"a":1},{"a":2}]',
    '{"a":1,"b":2,"a":3}', '{"\\u0061":1,"a":2}', '{"a":1,"A":2}',
]

# nesting limit
HAND += ["[" * 128 + "]" * 128, "[" * 129 + "]" * 129, "[" * 127 + "]" * 127]
HAND += ['{"a":' * 128 + "1" + "}" * 128, '{"a":' * 129 + "1" + "}" * 129]

# seeded random mutation of valid documents
BASES = [
    '{"a":[1,2.5,-3e2,true,false,null,"s\\n\\u00e9"],"b":{"c":{}},"d":[]}',
    '[0,-0,0.5,10,1E5,"",{"k":"v"},[[],[{}]]]',
    '{"key with space":"\\ud83d\\ude00","n":-12.75e-3,"t":true}',
    '[1,[2,[3,[4,[5]]]],{"x":{"y":{"z":null}}}]',
]
ALPHABET = ['[', ']', '{', '}', ',', ':', '"', '\\', '0', '1', '9', '-', '+', '.', 'e', 'E', ' ',
            '\n', '\t', '\x01', 'u', 'n', 't', 'f', 'a', 'é', 'x', '/', 'b']


def mutate(rng, s):
    s = list(s)
    for _ in range(rng.choice([1, 1, 1, 2, 3])):
        op = rng.choice(["del", "ins", "sub", "dup"])
        i = rng.randrange(len(s) + 1)
        if op == "del" and s:
            del s[min(i, len(s) - 1)]
        elif op == "ins":
            s.insert(i, rng.choice(ALPHABET))
        elif op == "sub" and s:
            s[min(i, len(s) - 1)] = rng.choice(ALPHABET)
        elif op == "dup" and s:
            j = min(i, len(s) - 1)
            s.insert(j, s[j])
    return "".join(s)


def main():
    rng = random.Random(8259)
    cases = list(HAND)
    for base in BASES:
        cases.append(base)
        for _ in range(1500):
            cases.append(mutate(rng, base))
    # random short strings over the alphabet
    for _ in range(1500):
        cases.append("".join(rng.choice(ALPHABET) for _ in range(rng.randrange(1, 12))))
    seen = set()
    counts = {"A": 0, "R": 0}
    for c in cases:
        if c in seen:
            continue
        seen.add(c)
        v = verdict(c)
        counts[v] += 1
        print(v, c.encode("utf-8").hex())
    print(f"accepted={counts['A']} rejected={counts['R']}", file=sys.stderr)


if __name__ == "__main__":
    main()
