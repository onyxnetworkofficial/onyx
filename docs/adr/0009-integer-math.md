# ADR-0009: Integer and fixed-point math only

## Status
Accepted, 2026-10-06 (M0).

## Context
Floats differ in rounding behavior across compilers and code paths and break byte-identical recompute.

## Decision
- Amounts in the smallest unit (sats, wei, lamports, cents) as `u128`/`i128`.
- Ratios and prices as `Value::Fixed { mantissa: i128, scale: u8 }`, normalized, scale ≤ 38.
- Probabilities as parts per million (`Confidence(u32)`, 0..=1,000,000).
- Calibration scores (M2) in integer fixed point; log score uses a deterministic integer log table
  or fixed-point series, decided in M2.
- Enforced by: the `clippy::float_arithmetic = deny` lint workspace-wide, and the WASM host rejecting float opcodes.

## Consequences
- Rounding rules must be written in the spec for every division (round toward zero unless stated).
