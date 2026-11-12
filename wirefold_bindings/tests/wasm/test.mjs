// Tests for the WebAssembly build. Run `scripts/build-wasm.sh` first, then `node tests/wasm/test.mjs`.
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const w = require("../../dist/wirefold_bindings.js");

// translate
const yaml = w.translate('{"name":"w","tags":["a","b"],"n":3}', "json", "yaml");
assert.match(yaml, /name: w/);
assert.deepEqual(JSON.parse(w.translate(yaml, "yaml", "json")), { name: "w", tags: ["a", "b"], n: 3 });
assert.equal(w.translate("{}", " JSON ", "Yaml"), w.translate("{}", "json", "yaml"));

// errors name what was rejected and are real Errors
assert.throws(() => w.translate("{", "json", "yaml"), (e) => e instanceof Error && /cannot read the input as json/.test(e.message));
assert.throws(() => w.translate("{}", "json", "xml"), /expected one of json, yaml/);

// msgpack
const src = { a: 1, b: [true, null, "x"], c: { d: 2.5 } };
const packed = w.jsonToMsgpack(JSON.stringify(src));
assert.ok(packed instanceof Uint8Array);
assert.deepEqual(JSON.parse(w.msgpackToJson(packed)), src);
for (const bad of [new Uint8Array([]), new Uint8Array([0xc1]), new Uint8Array([0xdd, 0xff, 0xff, 0xff, 0xff])]) {
  assert.throws(() => w.msgpackToJson(bad), Error);
}
assert.throws(() => w.jsonToMsgpack("{nope"), Error);

// meta
assert.deepEqual(w.formats().slice(0, 2), ["json", "yaml"]);
assert.match(w.version(), /^\d+\.\d+\.\d+/);
console.log("wasm: all tests passed");
