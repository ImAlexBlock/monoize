import { parseRate, type StoreCurrency } from "./store-money";

const NANO_PER_UNIT = 1_000_000_000n;
const I128_MIN = -(1n << 127n);
const I128_MAX = (1n << 127n) - 1n;
const INTEGER = /^-?(?:0|[1-9][0-9]*)$/;
const DECIMAL = /^(-?)(0|[1-9][0-9]*)(?:\.([0-9]{1,9}))?$/;

function checkedAmount(value: bigint): bigint {
  if (value < I128_MIN || value > I128_MAX) {
    throw new Error("accounting amount is outside the supported range");
  }
  return value;
}

function parseNano(value: string): bigint {
  if (!INTEGER.test(value) || value === "-0") {
    throw new Error("amount must be a canonical signed integer");
  }
  return checkedAmount(BigInt(value));
}

function roundRatio(numerator: bigint, denominator: bigint): bigint {
  const negative = numerator < 0n;
  const absolute = negative ? -numerator : numerator;
  const quotient = absolute / denominator;
  const remainder = absolute % denominator;
  const rounded = quotient + (remainder >= denominator - remainder ? 1n : 0n);
  return negative ? -rounded : rounded;
}

function requiredRate(value?: string) {
  if (value === undefined || value === "") {
    throw new Error("a valid exchange rate is required for USD conversion");
  }
  return parseRate(value);
}

/** Format native nano-CNY; the CNY branch never reads the optional exchange rate. */
export function formatAccountingNano(
  nanoCny: string,
  displayCurrency: StoreCurrency = "CNY",
  cnyPerUsd?: string,
  fractionalDigits = 2,
): string {
  if (!Number.isInteger(fractionalDigits) || fractionalDigits < 2 || fractionalDigits > 9) {
    throw new Error("fractionalDigits must be an integer between 2 and 9");
  }
  const amount = parseNano(nanoCny);
  const scale = 10n ** BigInt(fractionalDigits);
  let numerator = amount * scale;
  let denominator = NANO_PER_UNIT;
  if (displayCurrency === "USD") {
    const rate = requiredRate(cnyPerUsd);
    numerator *= rate.denominator;
    denominator *= rate.numerator;
  }
  const scaled = roundRatio(numerator, denominator);
  const absolute = scaled < 0n ? -scaled : scaled;
  const whole = absolute / scale;
  const fraction = (absolute % scale).toString().padStart(fractionalDigits, "0");
  return `${scaled < 0n ? "-" : ""}${displayCurrency === "CNY" ? "¥" : "$"}${whole}.${fraction}`;
}

/** Convert a decimal editor value to native nano-CNY without binary floating point. */
export function parseAccountingInput(
  input: string,
  inputCurrency: StoreCurrency = "CNY",
  cnyPerUsd?: string,
): string {
  const match = DECIMAL.exec(input.trim());
  if (!match) throw new Error("amount must be a decimal with at most nine fractional digits");
  const magnitude = BigInt(match[2]) * NANO_PER_UNIT + BigInt((match[3] ?? "").padEnd(9, "0"));
  if (match[1] === "-" && magnitude === 0n) {
    throw new Error("negative zero is not a canonical amount");
  }
  let amount = match[1] === "-" ? -magnitude : magnitude;
  if (inputCurrency === "USD") {
    const rate = requiredRate(cnyPerUsd);
    amount = roundRatio(amount * rate.numerator, rate.denominator);
  }
  return checkedAmount(amount).toString();
}
