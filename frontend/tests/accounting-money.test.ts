import { describe, expect, test } from "bun:test";
import {
  formatAccountingNano,
  parseAccountingInput,
} from "../src/lib/accounting-money";

describe("CNY accounting amounts", () => {
  test("CNY balance never changes with the display exchange rate", () => {
    for (const rate of [undefined, "", "6.722655", "6.729032", "0"]) {
      expect(formatAccountingNano("4078000000000", "CNY", rate)).toBe("¥4078.00");
    }
  });

  test("formats tiny charges with exact signed half-away rounding", () => {
    expect(formatAccountingNano("5", "CNY", undefined, 8)).toBe("¥0.00000001");
    expect(formatAccountingNano("-5", "CNY", undefined, 8)).toBe("-¥0.00000001");
    expect(formatAccountingNano("170141183460469231731687303715884105727", "CNY", undefined, 9))
      .toBe("¥170141183460469231731687303715.884105727");
  });

  test("USD reference display divides once at the requested display precision", () => {
    expect(formatAccountingNano("6737033685", "USD", "6.737", 6)).toBe("$1.000005");
    expect(formatAccountingNano("-33685000", "USD", "6.737")).toBe("-$0.01");
    expect(() => formatAccountingNano("1000000000", "USD")).toThrow("exchange rate");
  });

  test("parses CNY without FX and converts USD input only once", () => {
    expect(parseAccountingInput("100.25", "CNY")).toBe("100250000000");
    expect(parseAccountingInput("1", "USD", "6.729032")).toBe("6729032000");
    expect(parseAccountingInput("0.000000001", "USD", "6.5")).toBe("7");
    expect(parseAccountingInput("-0.000000001", "USD", "6.5")).toBe("-7");
    expect(parseAccountingInput("100.25", "USD", "6.729032")).toBe("674585458000");
  });

  test("rejects ambiguous amounts, unsupported precision, and overflowing API values", () => {
    for (const amount of ["", "1e2", "01", "NaN", "--1", "-0", "0.0000000001"]) {
      expect(() => parseAccountingInput(amount, "CNY")).toThrow();
    }
    expect(() => formatAccountingNano("-0", "CNY")).toThrow();
    expect(() => formatAccountingNano("1", "CNY", undefined, 10)).toThrow();
    expect(() => parseAccountingInput("170141183460469231731687303716", "CNY")).toThrow("range");
    expect(parseAccountingInput("-170141183460469231731687303715.884105728", "CNY"))
      .toBe("-170141183460469231731687303715884105728");
  });
});
