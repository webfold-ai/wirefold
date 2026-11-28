import Wirefold.Extracted
import Mathlib.Tactic.NormNum
open Aeneas Aeneas.Std Result
set_option maxRecDepth 8192
namespace wirefold_extract

/-! ## `rop`: no overflow in the 128-bit multiplications

For a table limb `ghi < 2^62` (every entry of `G` has `g < 2^126`) and a scaled significand
`cp < 2^63`, `rop` returns `ok`: the two `u64 x u64 -> u128` products and the sum `t1 + (t0 >> 64)` fit.
The value of the result (round-to-odd of `cp * g / 2^126`) is NOT proved here; it is evidenced by the
exhaustive `f32` sweep and the `f64` differential tests. -/

theorem rop_no_overflow (ghi glo cp : U64) (hg : ghi.val < 2 ^ 62) (hc : cp.val < 2 ^ 63) :
    float_core.rop ghi glo cp ⦃ _ => True ⦄ := by
  unfold float_core.rop
  step*
  have a1 : i.val ≤ 2 ^ 63 := by scalar_tac
  have a2 : i1.val ≤ 2 ^ 64 := by scalar_tac
  have b1 : i2.val ≤ 2 ^ 63 := by scalar_tac
  have b2 : i3.val ≤ 2 ^ 62 := by scalar_tac
  have h0 := Nat.mul_le_mul a1 a2
  have h1 := Nat.mul_le_mul b1 b2
  have e0 : t0.val ≤ 2 ^ 127 := by rw [t0_post]; norm_num at h0 ⊢; omega
  have e1 : t1.val ≤ 2 ^ 125 := by rw [t1_post]; norm_num at h1 ⊢; omega
  scalar_tac

end wirefold_extract
