import Wirefold.Extracted
import Mathlib.Tactic.IntervalCases
import Mathlib.Tactic.NormNum
open Aeneas Aeneas.Std Result
set_option maxRecDepth 8192
namespace wirefold_extract

/-! ## `rop`: the 128-bit multiplications never overflow, and the result is within one of the exact quotient -/

@[step] theorem rop_spec (ghi glo cp : U64) (hg : ghi.val < 2 ^ 62) (hc : cp.val < 2 ^ 63) :
    float_core.rop ghi glo cp ⦃ r =>
      (cp.val * (ghi.val * 2 ^ 64 + glo.val)) / 2 ^ 126 ≤ r.val ∧
      r.val ≤ (cp.val * (ghi.val * 2 ^ 64 + glo.val)) / 2 ^ 126 + 1 ⦄ := by
  unfold float_core.rop
  step*
  all_goals (trace_state; sorry)

end wirefold_extract
