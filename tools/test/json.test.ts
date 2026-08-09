import { describe, it, expect } from "vitest";
import { stableStringify, toCanonical, hashValue, hashText } from "../src/json.js";

describe("deterministic JSON", () => {
  it("sorts object keys by code unit", () => {
    expect(stableStringify({ b: 1, a: 2 }, 0)).toBe(stableStringify({ a: 2, b: 1 }, 0));
    expect(stableStringify({ b: 1, a: 2 }, 0)).toBe('{"a":2,"b":1}');
  });

  it("drops undefined, keeps null", () => {
    expect(stableStringify({ a: undefined, b: null }, 0)).toBe('{"b":null}');
  });

  it("is order-independent for hashing", () => {
    expect(hashValue({ a: 1, b: 2 })).toBe(hashValue({ b: 2, a: 1 }));
  });

  it("preserves array order", () => {
    expect(stableStringify([3, 1, 2], 0)).toBe("[3,1,2]");
  });

  it("converts Map to sorted-key object and Set to sorted array", () => {
    expect(toCanonical(new Map([["b", 2], ["a", 1]]))).toEqual({ a: 1, b: 2 });
    expect(toCanonical(new Set(["c", "a", "b"]))).toEqual(["a", "b", "c"]);
  });

  it("hashes text deterministically", () => {
    expect(hashText("hello")).toMatch(/^[0-9a-f]{64}$/);
  });
});
