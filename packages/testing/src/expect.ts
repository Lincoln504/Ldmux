/** Join an assertion failure with the caller's context, when one was given. */
function labelled(message: string, label?: string): string {
  return label ? `${message} — ${label}` : message;
}

/**
 * A Jest-style `expect` on top of `node:test`'s runner.
 *
 * Why this exists: the suite has ~680 assertions written as
 * `expect(x).toBe(y)`, and `node:assert` spells the same intent differently
 * (`assert.strictEqual(x, y)`). Rewriting them all by hand would be a large,
 * error-prone diff with no behavioural gain, and a mis-rewritten assertion is
 * exactly the kind of thing that silently weakens a test. So the *runner* is
 * Node's and the *matcher vocabulary* is this file: one small, tested
 * translation layer.
 *
 * It is deliberately not a general-purpose assertion library — it implements the
 * matchers this suite uses, and a missing one is a type error rather than a
 * silently-passing no-op.
 *
 * Semantics follow the conventions the suite already relies on: `toBe` is
 * `Object.is` (so `NaN` is itself and `-0` is not `0`), `toEqual` is structural,
 * and `not` is a true negation that still *calls* the subject for `toThrow`.
 */

import { test, describe, it, before, after, beforeEach, afterEach } from "node:test";
import { deepStrictEqual } from "node:assert/strict";

export { test, describe, it, before, after, beforeEach, afterEach };

/**
 * `before`/`after` are node:test's suite-level hooks (run once); the suite this
 * replaces called them `beforeAll`/`afterAll`. Aliases here keep the test bodies
 * unchanged and make the intent explicit.
 */
export const beforeAll = before;
export const afterAll = after;

/** Thrown for a failed assertion; carries the fields node:assert sets. */
export class ExpectationError extends Error {
  readonly actual: unknown;
  readonly expected: unknown;

  constructor(message: string, actual: unknown, expected: unknown) {
    super(message);
    this.name = "ExpectationError";
    this.actual = actual;
    this.expected = expected;
  }
}

function show(value: unknown): string {
  if (typeof value === "string") return JSON.stringify(value);
  if (value instanceof Error) return `${value.name}: ${value.message}`;
  if (typeof value === "function") return value.name ? `[function ${value.name}]` : "[function]";
  try {
    const json = JSON.stringify(value);
    if (json !== undefined) return json;
  } catch {
    /* circular */
  }
  return String(value);
}

/** Structural equality via node:assert, so the rules are the platform's. */
function structurallyEqual(a: unknown, b: unknown): boolean {
  try {
    deepStrictEqual(a, b);
    return true;
  } catch {
    return false;
  }
}

function matchThrown(thrown: unknown, expected?: string | RegExp | Error): boolean {
  if (expected === undefined) return true;
  const message = thrown instanceof Error ? thrown.message : String(thrown);
  if (typeof expected === "string") return message.includes(expected);
  if (expected instanceof RegExp) return expected.test(message);
  return message.includes(expected.message);
}

export interface Matchers {
  toBe(expected: unknown): void;
  toEqual(expected: unknown): void;
  toContain(expected: unknown): void;
  toThrow(expected?: string | RegExp | Error): void;
  toMatch(expected: string | RegExp): void;
  toHaveLength(expected: number): void;
  toBeDefined(): void;
  toBeUndefined(): void;
  toBeNull(): void;
  toBeTruthy(): void;
  toBeFalsy(): void;
  toBeGreaterThan(expected: number): void;
  toBeGreaterThanOrEqual(expected: number): void;
  toBeLessThan(expected: number): void;
  toBeLessThanOrEqual(expected: number): void;
  toBeInstanceOf(expected: unknown): void;
}

function build(actual: unknown, negated: boolean): Matchers {
  const check = (pass: boolean, describe: () => string, expected: unknown): void => {
    if (pass === negated) {
      throw new ExpectationError(
        `expected ${show(actual)} ${negated ? "not " : ""}${describe()}`,
        actual,
        expected,
      );
    }
  };

  return {
    toBe(expected) {
      check(Object.is(actual, expected), () => `to be ${show(expected)}`, expected);
    },
    toEqual(expected) {
      check(structurallyEqual(actual, expected), () => `to equal ${show(expected)}`, expected);
    },
    toContain(expected) {
      const pass =
        typeof actual === "string"
          ? actual.includes(String(expected))
          : Array.isArray(actual)
            ? actual.some((item) => structurallyEqual(item, expected))
            : false;
      check(pass, () => `to contain ${show(expected)}`, expected);
    },
    toThrow(expected) {
      if (typeof actual !== "function") {
        throw new ExpectationError(`toThrow needs a function, got ${show(actual)}`, actual, expected);
      }
      let thrown: unknown;
      let didThrow = false;
      try {
        (actual as () => unknown)();
      } catch (err) {
        didThrow = true;
        thrown = err;
      }
      const reason = expected === undefined
        ? "to throw"
        : `to throw matching ${show(expected)} (${didThrow ? `threw ${show(thrown)}` : "did not throw"})`;
      // `didThrow` alone decides a negated expectation: `.not.toThrow()` must
      // pass when the call returned normally, not when it threw something else.
      check(expected === undefined ? didThrow : didThrow && matchThrown(thrown, expected), () => reason, expected);
    },
    toMatch(expected) {
      const text = String(actual);
      check(
        typeof expected === "string" ? text.includes(expected) : expected.test(text),
        () => `to match ${show(expected)}`,
        expected,
      );
    },
    toHaveLength(expected) {
      const len = (actual as { length?: number } | null)?.length;
      check(len === expected, () => `to have length ${expected} (got ${String(len)})`, expected);
    },
    toBeDefined() {
      check(actual !== undefined, () => "to be defined", undefined);
    },
    toBeUndefined() {
      check(actual === undefined, () => "to be undefined", undefined);
    },
    toBeNull() {
      check(actual === null, () => "to be null", null);
    },
    toBeTruthy() {
      check(Boolean(actual), () => "to be truthy", true);
    },
    toBeFalsy() {
      check(!actual, () => "to be falsy", false);
    },
    toBeGreaterThan(expected) {
      check(
        typeof actual === "number" && actual > expected,
        () => `to be greater than ${expected} (got ${show(actual)})`,
        expected,
      );
    },
    toBeGreaterThanOrEqual(expected) {
      check(
        typeof actual === "number" && actual >= expected,
        () => `to be greater than or equal to ${expected} (got ${show(actual)})`,
        expected,
      );
    },
    toBeLessThan(expected) {
      check(
        typeof actual === "number" && actual < expected,
        () => `to be less than ${expected} (got ${show(actual)})`,
        expected,
      );
    },
    toBeLessThanOrEqual(expected) {
      check(
        typeof actual === "number" && actual <= expected,
        () => `to be less than or equal to ${expected} (got ${show(actual)})`,
        expected,
      );
    },
    toBeInstanceOf(expected) {
      check(
        actual instanceof (expected as new () => unknown),
        () => `to be an instance of ${(expected as { name?: string } | null)?.name ?? "?"}`,
        expected,
      );
    },
  };
}

/** `resolves`/`rejects`: getters that return promise-returning matchers. */
export interface AsyncMatchers {
  resolves: {
    toBe(expected: unknown): Promise<void>;
    toEqual(expected: unknown): Promise<void>;
    toBeDefined(): Promise<void>;
    toBeUndefined(): Promise<void>;
  };
  rejects: {
    toThrow(expected?: string | RegExp | Error): Promise<void>;
    toBeDefined(): Promise<void>;
    toBeUndefined(): Promise<void>;
  };
}

export type Expectation = Matchers & AsyncMatchers & { not: Matchers };

function makeExpectation(actual: unknown, message?: string): Expectation {
  const settled = Promise.resolve(actual);

  /** Run `check` against the resolution, converting the other outcome to a failure. */
  const onResolve = (check: (value: unknown) => void) => (): Promise<void> =>
    settled.then(
      (value) => check(value),
      (err: unknown) => {
        throw new ExpectationError(
          labelled(`expected promise to resolve, but it rejected with ${show(err)}`, message),
          err,
          undefined,
        );
      },
    );

  const onReject = (check: (err: unknown) => void) => (): Promise<void> =>
    settled.then(
      (value) => {
        throw new ExpectationError(
          labelled(`expected promise to reject, but it resolved with ${show(value)}`, message),
          value,
          undefined,
        );
      },
      (err: unknown) => check(err),
    );

  /** Attach the caller's context, which is what makes a failure findable. */
  const wrap = (matchers: Matchers): Matchers => {
    if (!message) return matchers;
    const out = {} as Record<string, unknown>;
    for (const key of Object.keys(matchers) as (keyof Matchers)[]) {
      out[key] = (...args: unknown[]) => {
        try {
          (matchers[key] as (...a: unknown[]) => void)(...args);
        } catch (err) {
          if (err instanceof ExpectationError) {
            throw new ExpectationError(labelled(err.message, message), err.actual, err.expected);
          }
          throw err;
        }
      };
    }
    return out as unknown as Matchers;
  };

  return {
    ...wrap(build(actual, false)),
    not: wrap(build(actual, true)),
    get resolves() {
      return {
        toBe: (expected: unknown) => onResolve((v) => build(v, false).toBe(expected))(),
        toEqual: (expected: unknown) => onResolve((v) => build(v, false).toEqual(expected))(),
        toBeDefined: onResolve((v) => build(v, false).toBeDefined()),
        toBeUndefined: onResolve((v) => build(v, false).toBeUndefined()),
      };
    },
    get rejects() {
      return {
        // Reuse `toThrow` by re-throwing: the matcher's message, negation and
        // matcher handling then behave exactly as they do for a direct call.
        toThrow: (expected?: string | RegExp | Error) =>
          onReject((err) => {
            build(() => {
              throw err;
            }, false).toThrow(expected);
          })(),
        toBeDefined: onReject((err) => build(err, false).toBeDefined()),
        toBeUndefined: onReject((err) => build(err, false).toBeUndefined()),
      };
    },
  };
}

export function expect(actual: unknown, message?: string): Expectation {
  return makeExpectation(actual, message);
}
