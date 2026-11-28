import Wirefold.Extracted
import Mathlib.Tactic.IntervalCases
import Mathlib.Tactic.NormNum
open Aeneas Aeneas.Std Result
set_option maxRecDepth 8192
namespace wirefold_extract

/-! ## The power-of-ten table `G` is exact

`G[2i]` and `G[2i+1]` are the high and low limbs of `g(j) = ceil(10^j * 2^(125 - f))` for `j = i - 292`
and `f = floor(log2 10^j)` computed by the same fixed-point formula the core uses. -/

/-- The 126-bit table entry `i`. -/
def gAt (i : ℕ) : ℕ :=
  (float_tables.G.val[2 * i]!).val * 2 ^ 64 + (float_tables.G.val[2 * i + 1]!).val

/-- `g = ceil(10^j * 2^e)` with `e = 125 - f`, stated without division:
`(g - 1) * B * 2^max(-e,0) < A * 2^max(e,0) ≤ g * B * 2^max(-e,0)` where `10^j = A / B`,
and `2^125 ≤ g ≤ 2^126`. -/
abbrev TableOk (j : ℤ) (g : ℕ) : Prop :=
  2 ^ 125 ≤ g ∧ g ≤ 2 ^ 126 ∧
  (g - 1) * 10 ^ (-j).toNat * 2 ^ (-(125 - ((j * 913124641741) >>> 38))).toNat
    < 10 ^ j.toNat * 2 ^ (125 - ((j * 913124641741) >>> 38)).toNat ∧
  10 ^ j.toNat * 2 ^ (125 - ((j * 913124641741) >>> 38)).toNat
    ≤ g * 10 ^ (-j).toNat * 2 ^ (-(125 - ((j * 913124641741) >>> 38))).toNat

theorem table_exact_aux :
    (List.range 617).all (fun (i : ℕ) => decide (TableOk ((i : ℤ) - 292) (gAt i))) = true := by
  unfold gAt float_tables.G
  decide +kernel

theorem table_exact : ∀ i : ℕ, i < 617 → TableOk ((i : ℤ) - 292) (gAt i) := by
  intro i hi
  have h := table_exact_aux
  rw [List.all_eq_true] at h
  have := h i (List.mem_range.mpr hi)
  exact of_decide_eq_true this


/-! ## Exponent formulas: the table index and the shift stay in range for every `f64` and `f32`

For every binary exponent `q` the core uses (`-1074 ≤ q ≤ 971`; `f32`: `-149 ≤ q ≤ 104`), the decimal
exponent `k = flog10_pow2 q` (or its 3/4 variant for the irregular lower neighbour), the table index
`-k - J_MIN` and the shift `h = q + flog2_pow10 (-k) + 1` stay in range, so that `cb << h` is below `2^59`
and the table lookup is in bounds. -/

/-- `⌊q · 661971961083 / 2^41⌋` -/
def fl10 (q : ℤ) : ℤ := (q * 661971961083) >>> 41
/-- `⌊(q · 661971961083 - 274743187321) / 2^41⌋` -/
def fl10t (q : ℤ) : ℤ := (q * 661971961083 - 274743187321) >>> 41
/-- `⌊e · 913124641741 / 2^38⌋` -/
def fl2 (e : ℤ) : ℤ := (e * 913124641741) >>> 38

abbrev ExpOk (q k : ℤ) : Prop := -324 ≤ k ∧ k ≤ 292 ∧ 1 ≤ q + fl2 (-k) + 1 ∧ q + fl2 (-k) + 1 ≤ 4

theorem exp_regular_ok :
    (List.range 2046).all (fun (n : ℕ) => decide (ExpOk ((n : ℤ) - 1074) (fl10 ((n : ℤ) - 1074)))) = true := by
  decide +kernel

theorem exp_irregular_ok :
    (List.range 2044).all (fun (n : ℕ) => decide (ExpOk ((n : ℤ) - 1073) (fl10t ((n : ℤ) - 1073)))) = true := by
  decide +kernel


end wirefold_extract
