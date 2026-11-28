import Wirefold.Extracted
import Mathlib.Tactic.IntervalCases
import Mathlib.Tactic.NormNum
import Mathlib.Data.Nat.Digits.Lemmas
open Aeneas Aeneas.Std Result
set_option maxRecDepth 8192
namespace wirefold_extract

/-- The digit table as plain naturals. -/
def pairsNat : List Nat := (List.range 100).flatMap (fun p => [48 + p / 10, 48 + p % 10])

theorem pairs_val : itoa_core.PAIRS.val.map (fun b => b.val) = pairsNat := by
  unfold itoa_core.PAIRS
  decide +kernel

theorem pairs_digits : ∀ p, p < 100 →
    (itoa_core.PAIRS.val[2 * p]!).val = 48 + p / 10 ∧ (itoa_core.PAIRS.val[2 * p + 1]!).val = 48 + p % 10 := by
  unfold itoa_core.PAIRS
  decide +kernel

theorem set_get (buf : Array U8 40#usize) (i : Usize) (x : U8) (hi : i.val < 40) (j : Nat) :
    (buf.set i x).val[j]! = if j = i.val then x else buf.val[j]! := by
  have hl : buf.val.length = 40 := by simp
  rw [Array.set_val_eq]
  by_cases h : j = i.val
  · subst h
    simp [List.getElem!_eq_getElem?_getD, hl, hi]
  · simp [h, List.getElem!_eq_getElem?_getD, List.getElem?_set, Ne.symm h]

@[step] theorem put4_spec (buf : Array U8 40#usize) (at_ : Usize) (v : U32)
    (hat : at_.val + 4 ≤ 40) (hv : v.val < 10000) :
    itoa_core.put4 buf at_ v ⦃ buf' =>
      (∀ j, (j < at_.val ∨ at_.val + 4 ≤ j) → buf'.val[j]! = buf.val[j]!) ∧
      (buf'.val[at_.val]!).val = 48 + v.val / 1000 % 10 ∧
      (buf'.val[at_.val + 1]!).val = 48 + v.val / 100 % 10 ∧
      (buf'.val[at_.val + 2]!).val = 48 + v.val / 10 % 10 ∧
      (buf'.val[at_.val + 3]!).val = 48 + v.val % 10 ⦄ := by
  unfold itoa_core.put4
  step*
  have e1 : i1.val = i.val := by scalar_tac
  have e3 : i3.val = i2.val := by scalar_tac
  have hd := pairs_digits (v.val / 100) (by omega)
  have ld := pairs_digits (v.val % 100) (by omega)
  have hi_v : hi.val = 2 * (v.val / 100) := by omega
  have lo_v : lo.val = 2 * (v.val % 100) := by omega
  have plen : itoa_core.PAIRS.val.length = 200 := by simp
  have g1 : i4.val = 48 + v.val / 100 / 10 := by
    have h := hd.1
    rw [← hi_v, getElem!_pos _ _ (by omega)] at h
    rw [i4_post]; exact h
  have g2 : i6.val = 48 + v.val / 100 % 10 := by
    have h := hd.2
    rw [show 2 * (v.val / 100) + 1 = i5.val by omega, getElem!_pos _ _ (by omega)] at h
    rw [i6_post]; exact h
  have g3 : i8.val = 48 + v.val % 100 / 10 := by
    have h := ld.1
    rw [← lo_v, getElem!_pos _ _ (by omega)] at h
    rw [i8_post]; exact h
  have g4 : i11.val = 48 + v.val % 100 % 10 := by
    have h := ld.2
    rw [show 2 * (v.val % 100) + 1 = i10.val by omega, getElem!_pos _ _ (by omega)] at h
    rw [i11_post]; exact h
  have hv' : ∀ j, buf'.val[j]! = (if j = i12.val then i11 else if j = i9.val then i8 else if j = i7.val then i6 else if j = at_.val then i4 else buf.val[j]!) := by
    intro j
    rw [buf'_post, set_get _ _ _ (by omega), buf3_post, set_get _ _ _ (by omega), buf2_post,
      set_get _ _ _ (by omega), buf1_post, set_get _ _ _ (by omega)]
  have k1 : at_.val ≠ i12.val := by omega
  have k2 : at_.val ≠ i9.val := by omega
  have k3 : at_.val ≠ i7.val := by omega
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · intro j hj
    rw [hv' j]
    have c1 : j ≠ i12.val := by omega
    have c2 : j ≠ i9.val := by omega
    have c3 : j ≠ i7.val := by omega
    have c4 : j ≠ at_.val := by omega
    simp [c1, c2, c3, c4]
  · rw [hv', if_neg k1, if_neg k2, if_neg k3, if_pos rfl]; omega
  · rw [hv', if_neg (by omega : at_.val + 1 ≠ i12.val), if_neg (by omega : at_.val + 1 ≠ i9.val),
      if_pos (by omega : at_.val + 1 = i7.val)]; omega
  · rw [hv', if_neg (by omega : at_.val + 2 ≠ i12.val), if_pos (by omega : at_.val + 2 = i9.val)]; omega
  · rw [hv', if_pos (by omega : at_.val + 3 = i12.val)]; omega

theorem pow10_get : ∀ i, i < 20 → (itoa_core.POW10.val[i]!).val = 10 ^ i := by
  unfold itoa_core.POW10
  decide +kernel

theorem lz_pos (i : U64) (h : i.val ≠ 0) :
    (core.num.U64.leading_zeros i).val = 63 - Nat.log 2 i.val := by
  have h1 : ¬ (i.bv = 0) := by
    intro hh
    apply h
    show i.bv.toNat = 0
    simp [hh]
  have hlt : i.val < 2 ^ 64 := by scalar_tac
  have hlog : Nat.log 2 i.val < 64 := (Nat.log_lt_iff_lt_pow (by omega) h).mpr hlt
  change (BitVec.ofNat 32 (BitVec.leadingZeros i.bv)).toNat = _
  rw [BitVec.toNat_ofNat]
  unfold BitVec.leadingZeros
  rw [if_neg h1]
  have : i.bv.toNat = i.val := rfl
  rw [this, Nat.mod_eq_of_lt (by omega)]
  omega

/-- The arithmetic heart of `digit_count`: for `1 ≤ x < 2^64`, `b = ⌊log₂ x⌋`, `t = ⌊(b+1)·1233 / 4096⌋`
is `⌊log₁₀ x⌋` or one more. Checked for all 64 values of `b`. -/
theorem dc_nat (x : ℕ) (h1 : 1 ≤ x) (h2 : x < 2 ^ 64) :
    ((Nat.log 2 x + 1) * 1233 / 4096 ≤ 19) ∧ x < 10 ^ ((Nat.log 2 x + 1) * 1233 / 4096 + 1) ∧
      (1 ≤ (Nat.log 2 x + 1) * 1233 / 4096 → 10 ^ ((Nat.log 2 x + 1) * 1233 / 4096 - 1) ≤ x) := by
  have hb : 2 ^ Nat.log 2 x ≤ x := Nat.pow_log_le_self 2 (by omega)
  have hb2 : x < 2 ^ (Nat.log 2 x + 1) := Nat.lt_pow_succ_log_self (by omega) x
  have hb64 : Nat.log 2 x < 64 := (Nat.log_lt_iff_lt_pow (by omega) (by omega)).mpr h2
  generalize Nat.log 2 x = b at *
  interval_cases b <;> norm_num at hb hb2 ⊢ <;> omega

/-- `leading_zeros` of a nonzero 64-bit value, through the `Usize` cast the extraction inserts. -/
theorem lz_cast (x : U64) (hx : 1 ≤ x.val) (i : U32) (hi : i = core.num.U64.leading_zeros x)
    (i1 : Usize) (h1 : i1 = UScalar.cast UScalarTy.Usize i) :
    i1.val = 63 - Nat.log 2 x.val ∧ Nat.log 2 x.val < 64 := by
  have hl := lz_pos x (by omega)
  have hlt : x.val < 2 ^ 64 := by scalar_tac
  have hlog : Nat.log 2 x.val < 64 := (Nat.log_lt_iff_lt_pow (by omega) (by omega)).mpr hlt
  refine ⟨?_, hlog⟩
  have hiv : i.val = 63 - Nat.log 2 x.val := by rw [hi]; exact hl
  have hnb : 2 ^ 32 ≤ 2 ^ System.Platform.numBits := by
    rcases System.Platform.numBits_eq with h | h <;> simp [h]
  have hi32 : i.val < 2 ^ 32 := by omega
  rw [h1]
  simp
  exact hiv

theorem dc_main (X : U64) (hX : 1 ≤ X.val) (t i3 i5 n : Usize) (i4 : U64) (b : ℕ) (hb : b = Nat.log 2 X.val)
    (ht : t.val = (b + 1) * 1233 / 4096) (i3p : i3.val = t.val + 1) (i4v : i4.val = 10 ^ t.val)
    (i5p : i5.val = if X.val < i4.val then 1 else 0) (np : n.val = i3.val - i5.val) :
    1 ≤ n.val ∧ n.val ≤ 20 ∧ X.val < 10 ^ n.val ∧ (n.val = 1 ∨ 10 ^ (n.val - 1) ≤ X.val) := by
  have hlt : X.val < 2 ^ 64 := by scalar_tac
  have key := dc_nat X.val hX hlt
  rw [← hb] at key
  obtain ⟨k1, k2, k3⟩ := key
  rw [← ht] at k1 k2 k3
  by_cases hc : X.val < 10 ^ t.val
  · have : i5.val = 1 := by rw [i5p, i4v]; simp [hc]
    have hn : n.val = t.val := by omega
    rcases Nat.eq_zero_or_pos t.val with h0 | h0
    · rw [h0] at hc; simp at hc; omega
    · refine ⟨by omega, by omega, by rw [hn]; exact hc, ?_⟩
      rcases Nat.lt_or_ge t.val 2 with h2 | h2
      · left; omega
      · right; rw [hn]; exact k3 (by omega)
  · have : i5.val = 0 := by rw [i5p, i4v]; simp [hc]
    have hn : n.val = t.val + 1 := by omega
    refine ⟨by omega, by omega, by rw [hn]; exact k2, ?_⟩
    right; rw [hn]; simp; omega

@[step] theorem digit_count_spec (v : U64) :
    itoa_core.digit_count v ⦃ n =>
      1 ≤ n.val ∧ n.val ≤ 20 ∧ v.val < 10 ^ n.val ∧ (n.val = 1 ∨ 10 ^ (n.val - 1) ≤ v.val) ⦄ := by
  unfold itoa_core.digit_count
  by_cases h0 : v = 0#u64
  · rw [if_pos h0]
    simp only [bind_tc_ok]
    have hX : 1 ≤ (1#u64).val := by simp
    have hv0 : v.val = 0 := by scalar_tac
    step*
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; scalar_tac
    · rcases System.Platform.numBits_eq with h | h <;> simp [h]
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; scalar_tac
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; simp; scalar_tac
    · simp [i5_post]; scalar_tac
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post
      have hb : bits.val = Nat.log 2 (1#u64).val + 1 := by scalar_tac
      have p4 : i4.val = 10 ^ t.val := by
        have h := pow10_get t.val (by scalar_tac)
        rw [getElem!_pos _ _ (by scalar_tac)] at h
        rw [i4_post]; exact h
      have key := dc_main (1#u64) hX t i3 i5 n i4 (Nat.log 2 (1#u64).val) rfl (by scalar_tac)
        (by scalar_tac) p4 (by simp [i5_post, UScalar.lt_equiv]; by_cases hh : 1 < i4.val <;> simp [hh])
        (by scalar_tac)
      obtain ⟨a, b, c, d⟩ := key
      have hn1 : n.val = 1 := by
        rcases d with d | d
        · exact d
        · by_contra hne
          have : 10 ^ 1 ≤ 10 ^ (n.val - 1) := Nat.pow_le_pow_right (by omega) (by omega)
          simp at d
          omega
      refine ⟨a, b, ?_, Or.inl hn1⟩
      rw [hv0]; exact Nat.pow_pos (by omega)
  · rw [if_neg h0]
    simp only [bind_tc_ok]
    have hX : 1 ≤ v.val := by scalar_tac
    step*
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; scalar_tac
    · rcases System.Platform.numBits_eq with h | h <;> simp [h]
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; scalar_tac
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post; simp; scalar_tac
    · simp [i5_post]; scalar_tac
    · obtain ⟨e, hl⟩ := lz_cast _ hX i i_post i1 i1_post
      have hb : bits.val = Nat.log 2 v.val + 1 := by scalar_tac
      have p4 : i4.val = 10 ^ t.val := by
        have h := pow10_get t.val (by scalar_tac)
        rw [getElem!_pos _ _ (by scalar_tac)] at h
        rw [i4_post]; exact h
      exact dc_main v hX t i3 i5 n i4 (Nat.log 2 v.val) rfl (by scalar_tac)
        (by scalar_tac) p4 (by simp [i5_post, UScalar.lt_equiv]; by_cases hh : v.val < i4.val <;> simp [hh])
        (by scalar_tac)

@[step] theorem put8_spec (buf : Array U8 40#usize) (at_ : Usize) (v : U32)
    (hat : at_.val + 8 ≤ 40) (hv : v.val < 100000000) :
    itoa_core.put8 buf at_ v ⦃ buf' =>
      (∀ j, (j < at_.val ∨ at_.val + 8 ≤ j) → buf'.val[j]! = buf.val[j]!) ∧
      ∀ k, k < 8 → (buf'.val[at_.val + k]!).val = 48 + v.val / 10 ^ (7 - k) % 10 ⦄ := by
  unfold itoa_core.put8
  step*
  refine ⟨?_, ?_⟩
  · intro j hj
    rw [buf'_post j (by omega), buf1_post j (by omega)]
  · intro k hk
    have hk8 : k = 0 ∨ k = 1 ∨ k = 2 ∨ k = 3 ∨ k = 4 ∨ k = 5 ∨ k = 6 ∨ k = 7 := by omega
    rcases hk8 with rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl
    · rw [buf'_post (↑at_ + 0) (by omega)]; simp only [Nat.add_zero, Nat.reduceSub, Nat.reducePow] at buf1_post1 ⊢; omega
    · rw [buf'_post (↑at_ + 1) (by omega)]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [buf'_post (↑at_ + 2) (by omega)]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [buf'_post (↑at_ + 3) (by omega)]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [show at_.val + 4 = i1.val by omega]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [show at_.val + 5 = i1.val + 1 by omega]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [show at_.val + 6 = i1.val + 2 by omega]; simp only [Nat.reduceSub, Nat.reducePow]; omega
    · rw [show at_.val + 7 = i1.val + 3 by omega]; simp only [Nat.reduceSub, Nat.reducePow]; omega

/-- The bytes `lo ≤ i < hi` of `b` are the ASCII digits of `V`, the digit at index `i` being the one
of weight `10^(39 - i)`. -/
def WinR (b : Array U8 40#usize) (lo hi V : ℕ) : Prop :=
  ∀ i, lo ≤ i → i < hi → (b.val[i]!).val = 48 + V / 10 ^ (39 - i) % 10

theorem WinR.append {b : Array U8 40#usize} {lo mid hi V : ℕ} (h1 : WinR b lo mid V) (h2 : WinR b mid hi V) :
    WinR b lo hi V := by
  intro i h1' h2'
  by_cases hi' : i < mid
  · exact h1 i h1' hi'
  · exact h2 i (by omega) h2'

theorem WinR.frame {b b' : Array U8 40#usize} {lo hi V : ℕ} (h : WinR b lo hi V)
    (fr : ∀ j, lo ≤ j → j < hi → b'.val[j]! = b.val[j]!) : WinR b' lo hi V := by
  intro i h1 h2
  rw [fr i h1 h2]
  exact h i h1 h2

theorem put4_win (buf : Array U8 40#usize) (at_ : Usize) (u : U32) (V : ℕ)
    (hat : at_.val + 4 ≤ 40) (hu : u.val < 10000)
    (hdig : ∀ k, k < 4 → u.val / 10 ^ (3 - k) % 10 = V / 10 ^ (39 - (at_.val + k)) % 10) :
    itoa_core.put4 buf at_ u ⦃ buf' =>
      (∀ j, (j < at_.val ∨ at_.val + 4 ≤ j) → buf'.val[j]! = buf.val[j]!) ∧
      WinR buf' at_.val (at_.val + 4) V ⦄ := by
  have h := put4_spec buf at_ u hat hu
  refine WP.spec_mono h ?_
  intro buf' ⟨hf, d0, d1, d2, d3⟩
  refine ⟨hf, ?_⟩
  intro i hi1 hi2
  have hk : i = at_.val ∨ i = at_.val + 1 ∨ i = at_.val + 2 ∨ i = at_.val + 3 := by omega
  rcases hk with rfl | rfl | rfl | rfl
  · have := hdig 0 (by omega)
    simp only [Nat.add_zero, Nat.reduceSub, Nat.reducePow] at this
    rw [d0]; omega
  · have := hdig 1 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d1]; omega
  · have := hdig 2 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d2]; omega
  · have := hdig 3 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d3]; omega

theorem win8_of_post (b : Array U8 40#usize) (a u V : ℕ) (hat : a + 8 ≤ 40)
    (hd : ∀ k, k < 8 → (b.val[a + k]!).val = 48 + u / 10 ^ (7 - k) % 10)
    (hdig : ∀ k, k < 8 → u / 10 ^ (7 - k) % 10 = V / 10 ^ (39 - (a + k)) % 10) :
    WinR b a (a + 8) V := by
  intro i h1 h2
  have e := hd (i - a) (by omega)
  have g := hdig (i - a) (by omega)
  rw [show a + (i - a) = i by omega] at e g
  omega

theorem win4_of_post (b : Array U8 40#usize) (a u V : ℕ) (hat : a + 4 ≤ 40)
    (d0 : (b.val[a]!).val = 48 + u / 1000 % 10) (d1 : (b.val[a + 1]!).val = 48 + u / 100 % 10)
    (d2 : (b.val[a + 2]!).val = 48 + u / 10 % 10) (d3 : (b.val[a + 3]!).val = 48 + u % 10)
    (hdig : ∀ k, k < 4 → u / 10 ^ (3 - k) % 10 = V / 10 ^ (39 - (a + k)) % 10) :
    WinR b a (a + 4) V := by
  intro i h1 h2
  have hk : i = a ∨ i = a + 1 ∨ i = a + 2 ∨ i = a + 3 := by omega
  rcases hk with rfl | rfl | rfl | rfl
  · have := hdig 0 (by omega)
    simp only [Nat.add_zero, Nat.reduceSub, Nat.reducePow] at this
    rw [d0]; omega
  · have := hdig 1 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d1]; omega
  · have := hdig 2 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d2]; omega
  · have := hdig 3 (by omega)
    simp only [Nat.reduceSub, Nat.reducePow] at this
    rw [d3]; omega

theorem n_le (V n m : ℕ) (hm : 1 ≤ m) (hV : V < 10 ^ m) (hlow : n = 1 ∨ 10 ^ (n - 1) ≤ V) : n ≤ m := by
  rcases hlow with h | h
  · omega
  · by_contra hc
    have := Nat.pow_le_pow_right (by omega : 0 < 10) (show m ≤ n - 1 by omega)
    omega

theorem fmt_finish (V n w : ℕ) (b : Array U8 40#usize) (hn1 : 1 ≤ n) (hn20 : n ≤ 20) (hlt : V < 10 ^ n)
    (hlow : n = 1 ∨ 10 ^ (n - 1) ≤ V) (hw : n ≤ 40 - w) (hwin : WinR b w 40 V) :
    (40 - n ≤ 39) ∧ (∀ i, 40 - n ≤ i → i < 40 → (b.val[i]!).val = 48 + V / 10 ^ (39 - i) % 10) ∧
      V < 10 ^ (40 - (40 - n)) ∧ (40 - n = 39 ∨ 10 ^ (39 - (40 - n)) ≤ V) := by
  refine ⟨by omega, ?_, ?_, ?_⟩
  · intro i h1 h2
    exact hwin i (by omega) h2
  · rw [show 40 - (40 - n) = n by omega]; exact hlt
  · rw [show 39 - (40 - n) = n - 1 by omega]
    omega

@[step] theorem put16_spec (buf : Array U8 40#usize) (at_ : Usize) (v : U64)
    (hat : at_.val + 16 ≤ 40) (hv : v.val < 10000000000000000) :
    itoa_core.put16 buf at_ v ⦃ buf' =>
      (∀ j, (j < at_.val ∨ at_.val + 16 ≤ j) → buf'.val[j]! = buf.val[j]!) ∧
      ∀ k, k < 16 → (buf'.val[at_.val + k]!).val = 48 + v.val / 10 ^ (15 - k) % 10 ⦄ := by
  unfold itoa_core.put16
  step*
  have hi_v : hi.val = v.val / 100000000 := by scalar_tac
  have lo_v : lo.val = v.val % 100000000 := by scalar_tac
  have h_hi : ∀ k, k < 8 → (buf'.val[at_.val + k]!).val = 48 + hi.val / 10 ^ (7 - k) % 10 := by
    intro k hk
    rw [buf'_post _ (by omega)]
    exact buf1_post1 k hk
  have h_lo : ∀ k, k < 8 → (buf'.val[at_.val + 8 + k]!).val = 48 + lo.val / 10 ^ (7 - k) % 10 := by
    intro k hk
    rw [show at_.val + 8 + k = i2.val + k by omega]
    exact buf'_post1 k hk
  refine ⟨?_, ?_⟩
  · intro j hj
    rw [buf'_post j (by omega), buf1_post j (by omega)]
  · intro k hk
    by_cases hk8 : k < 8
    · rw [h_hi k hk8, hi_v]
      interval_cases k <;> simp only [Nat.reducePow, Nat.reduceSub] <;> omega
    · have e := h_lo (k - 8) (by omega)
      rw [show at_.val + 8 + (k - 8) = at_.val + k by omega] at e
      rw [e, lo_v]
      have hk16 : 8 ≤ k := by omega
      interval_cases k <;> simp only [Nat.reducePow, Nat.reduceSub] <;> omega

theorem buf_len_val : itoa_core.BUF_LEN.val = 40 := by
  unfold itoa_core.BUF_LEN
  rfl

theorem fmt_u64_spec (buf : Array U8 40#usize) (v : U64) :
    itoa_core.fmt_u64 buf v ⦃ start buf' =>
      start.val ≤ 39 ∧
      (∀ i, start.val ≤ i → i < 40 → (buf'.val[i]!).val = 48 + v.val / 10 ^ (39 - i) % 10) ∧
      v.val < 10 ^ (40 - start.val) ∧ (start.val = 39 ∨ 10 ^ (39 - start.val) ≤ v.val) ⦄ := by
  unfold itoa_core.fmt_u64
  step as ⟨n, hn1, hn20, hnlt, hnlow⟩
  have hv64 : v.val < 2 ^ 64 := by scalar_tac
  by_cases c1 : v < 10000#u64
  · rw [if_pos c1]
    step*
    · rw [buf_len_val]; omega
    · have hxv : x.val = v.val := by scalar_tac
      have hv4 : v.val < 10000 := by scalar_tac
      have hn : n.val ≤ 4 := n_le v.val n.val 4 (by norm_num) (by omega) hnlow
      have win : WinR buf1 36 (36 + 4) v.val := win4_of_post buf1 36 x.val v.val (by norm_num)
        buf1_post1 buf1_post2 buf1_post3 buf1_post4
        (by intro k hk; have e : 39 - (36 + k) = 3 - k := by omega
            rw [e, hxv])
      have hi : i.val = 40 - n.val := by rw [i_post, buf_len_val]
      rw [hi]
      exact fmt_finish v.val n.val 36 buf1 hn1 hn20 hnlt hnlow (by omega) win
  · rw [if_neg c1]
    by_cases c2 : v < 100000000#u64
    · rw [if_pos c2]
      step*
      · rw [buf_len_val]; omega
      · have hxv : x.val = v.val := by scalar_tac
        have hv8 : v.val < 100000000 := by scalar_tac
        have hn : n.val ≤ 8 := n_le v.val n.val 8 (by norm_num) (by omega) hnlow
        have win : WinR buf1 32 (32 + 8) v.val := win8_of_post buf1 32 x.val v.val (by norm_num) buf1_post1
          (by intro k hk; have e : 39 - (32 + k) = 7 - k := by omega
              rw [e, hxv])
        have hi : i.val = 40 - n.val := by rw [i_post, buf_len_val]
        rw [hi]
        exact fmt_finish v.val n.val 32 buf1 hn1 hn20 hnlt hnlow (by omega) win
    · rw [if_neg c2]
      by_cases c3 : v < 10000000000000000#u64
      · rw [if_pos c3]
        step*
        · rw [buf_len_val]; omega
        · have hv16 : v.val < 10000000000000000 := by scalar_tac
          have hn : n.val ≤ 16 := n_le v.val n.val 16 (by norm_num) (by omega) hnlow
          have win : WinR buf1 24 (24 + 16) v.val := by
            intro i h1 h2
            have e := buf1_post1 (i - 24) (by omega)
            rw [show 24 + (i - 24) = i by omega] at e
            rw [e]
            have hk : i = 24 ∨ i = 25 ∨ i = 26 ∨ i = 27 ∨ i = 28 ∨ i = 29 ∨ i = 30 ∨ i = 31 ∨ i = 32 ∨ i = 33 ∨ i = 34 ∨ i = 35 ∨ i = 36 ∨ i = 37 ∨ i = 38 ∨ i = 39 := by omega
            rcases hk with rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl <;>
              simp only [Nat.reducePow, Nat.reduceSub]
          have hi : i.val = 40 - n.val := by rw [i_post, buf_len_val]
          rw [hi]
          exact fmt_finish v.val n.val 24 buf1 hn1 hn20 hnlt hnlow (by omega) win
      · rw [if_neg c3]
        step*
        · rw [buf_len_val]; omega
        · rename_i rest m1 rest_post topw m2 topw_post top m3 top_post m4 m5
          have hv_lt : v.val < 2 ^ 64 := hv64
          have hrest : rest.val < 10000000000000000 := by omega
          have htop : top.val = v.val / 10000000000000000 := by scalar_tac
          have htopw : topw.val = v.val / 10000000000000000 := by omega
          have w1 : WinR x 20 (20 + 4) v.val := win4_of_post x 20 top.val v.val (by norm_num)
            x_post1 x_post2 x_post3 x_post4
            (by intro k hk
                rw [htop]
                interval_cases k <;> simp only [Nat.reducePow, Nat.reduceSub] <;> omega)
          have w2 : WinR buf1 24 (24 + 16) v.val := by
            intro i h1 h2
            have e := buf1_post1 (i - 24) (by omega)
            rw [show 24 + (i - 24) = i by omega] at e
            rw [e]
            have hk : i = 24 ∨ i = 25 ∨ i = 26 ∨ i = 27 ∨ i = 28 ∨ i = 29 ∨ i = 30 ∨ i = 31 ∨ i = 32 ∨ i = 33 ∨ i = 34 ∨ i = 35 ∨ i = 36 ∨ i = 37 ∨ i = 38 ∨ i = 39 := by omega
            rcases hk with rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl <;>
              simp only [Nat.reducePow, Nat.reduceSub] <;> omega
          have w1' : WinR buf1 20 (20 + 4) v.val := w1.frame (fun j h1 h2 => buf1_post j (by omega))
          have win : WinR buf1 20 40 v.val := w1'.append w2
          have hi : i.val = 40 - n.val := by rw [i_post, buf_len_val]
          rw [hi]
          exact fmt_finish v.val n.val 20 buf1 hn1 hn20 hnlt hnlow (by omega) win


/-! ### The round trip: reading the digits back gives the number -/

/-- Value of the decimal digits in `b[start..40]`: the byte at index `39 - j` has weight `10^j`. -/
def valueOf (b : Array U8 40#usize) (start : ℕ) : ℕ :=
  ((List.range (40 - start)).map (fun j => ((b.val[39 - j]!).val - 48) * 10 ^ j)).sum

theorem sum_digits (n : ℕ) : ∀ v : ℕ, v < 10 ^ n →
    ((List.range n).map (fun j => (v / 10 ^ j % 10) * 10 ^ j)).sum = v := by
  induction n with
  | zero => intro v hv; simp at hv; simp [hv]
  | succ n ih =>
    intro v hv
    rw [List.range_succ_eq_map, List.map_cons, List.map_map, List.sum_cons]
    have h10 : v / 10 < 10 ^ n := by
      rw [pow_succ] at hv; omega
    have := ih (v / 10) h10
    have key : ((fun j => (v / 10 ^ j % 10) * 10 ^ j) ∘ Nat.succ) = (fun j => 10 * ((v / 10 / 10 ^ j % 10) * 10 ^ j)) := by
      funext j
      simp only [Function.comp, pow_succ, Nat.div_div_eq_div_mul]
      rw [Nat.mul_comm (10 ^ j) 10, ← Nat.div_div_eq_div_mul]
      ring
    rw [key, List.sum_map_mul_left, this]
    simp
    omega

/-- The digits written by `fmt_u64` read back as the input. -/
theorem valueOf_of_digits (b : Array U8 40#usize) (start v : ℕ) (hs : start ≤ 40) (hv : v < 10 ^ (40 - start))
    (hd : ∀ i, start ≤ i → i < 40 → (b.val[i]!).val = 48 + v / 10 ^ (39 - i) % 10) :
    valueOf b start = v := by
  unfold valueOf
  have : (List.range (40 - start)).map (fun j => ((b.val[39 - j]!).val - 48) * 10 ^ j) =
      (List.range (40 - start)).map (fun j => (v / 10 ^ j % 10) * 10 ^ j) := by
    apply List.map_congr_left
    intro j hj
    have hj' : j < 40 - start := List.mem_range.mp hj
    rw [hd (39 - j) (by omega) (by omega), show 39 - (39 - j) = j by omega]
    simp
  rw [this]
  exact sum_digits (40 - start) v hv


/-- The arithmetic the digit writers rely on: splitting a four-digit number at 100. -/
theorem divmod100 (v : ℕ) (h : v < 10000) : v / 100 < 100 ∧ v % 100 < 100 ∧ 100 * (v / 100) + v % 100 = v := by
  omega

/-- What every integer formatter guarantees about its output `b[start..40]` for the magnitude `m`:
all bytes are ASCII digits, no leading zero unless the number is `0`, the length is the number of
decimal digits (`m < 10^len`, and `10^(len-1) ≤ m` unless `len = 1`), and the digits read back as `m`. -/
def Rt (m : ℕ) (start : ℕ) (b : Array U8 40#usize) : Prop :=
  start ≤ 39 ∧
  (∀ i, start ≤ i → i < 40 → 48 ≤ (b.val[i]!).val ∧ (b.val[i]!).val ≤ 57) ∧
  (start < 39 → (b.val[start]!).val ≠ 48) ∧
  (m < 10 ^ (40 - start) ∧ (start = 39 ∨ 10 ^ (39 - start) ≤ m)) ∧
  (40 - start = (Nat.digits 10 m).length ∨ (m = 0 ∧ start = 39)) ∧
  valueOf b start = m

theorem Rt_frame {m s : ℕ} {b b' : Array U8 40#usize} (h : Rt m s b)
    (fr : ∀ j, s ≤ j → j < 40 → b'.val[j]! = b.val[j]!) : Rt m s b' := by
  obtain ⟨h1, h2, h3, h4, h5, h6⟩ := h
  refine ⟨h1, ?_, ?_, h4, h5, ?_⟩
  · intro i hi1 hi2
    rw [fr i hi1 hi2]; exact h2 i hi1 hi2
  · intro hs
    rw [fr s (le_refl _) (by omega)]; exact h3 hs
  · rw [← h6]
    unfold valueOf
    congr 1
    apply List.map_congr_left
    intro j hj
    have hj' : j < 40 - s := List.mem_range.mp hj
    rw [fr (39 - j) (by omega) (by omega)]

/-- `fmt_u64`, headline theorem (a): for every `u64`, no panic or overflow (the program returns `ok`),
and the output satisfies `Rt`: ASCII digits, no leading zero, length = number of digits, round trip. -/
@[step] theorem fmt_u64_roundtrip (buf : Array U8 40#usize) (v : U64) :
    itoa_core.fmt_u64 buf v ⦃ start buf' => Rt v.val start.val buf' ⦄ := by
  refine WP.spec_mono (fmt_u64_spec buf v) ?_
  rintro ⟨start, buf'⟩ ⟨h39, hd, hlt, hlow⟩
  refine ⟨h39, ?_, ?_, ⟨hlt, hlow⟩, ?_, ?_⟩
  · intro i h1 h2
    rw [hd i h1 h2]
    have : v.val / 10 ^ (39 - i) % 10 < 10 := Nat.mod_lt _ (by norm_num)
    omega
  · intro hs
    rw [hd start.val (le_refl _) (by omega)]
    rcases hlow with h | h
    · omega
    · have : 10 ^ (39 - start.val) ≤ v.val := h
      have hpos : 1 ≤ v.val / 10 ^ (39 - start.val) := (Nat.one_le_div_iff (by positivity)).mpr this
      have hlt' : v.val / 10 ^ (39 - start.val) < 10 := by
        have : v.val < 10 ^ (40 - start.val) := hlt
        rw [show 40 - start.val = (39 - start.val) + 1 by omega, pow_succ] at this
        exact (Nat.div_lt_iff_lt_mul (by positivity)).mpr (by linarith)
      rw [Nat.mod_eq_of_lt hlt']
      omega
  · by_cases hv0 : v.val = 0
    · right; refine ⟨hv0, ?_⟩
      have : v.val < 10 ^ (40 - start.val) := hlt
      rcases hlow with h | h
      · exact h
      · have : 0 < 10 ^ (39 - start.val) := Nat.pow_pos (by norm_num)
        omega
    · left
      have hne : v.val ≠ 0 := hv0
      rw [Nat.length_digits 10 v.val (by norm_num) hne]
      have hlog : Nat.log 10 v.val = 39 - start.val := by
        apply Nat.log_eq_of_pow_le_of_lt_pow
        · rcases hlow with h | h
          · rw [h]; simp; omega
          · exact h
        · rw [show 39 - start.val + 1 = 40 - start.val by omega]; exact hlt
      omega
  · exact valueOf_of_digits buf' start.val v.val (by omega) hlt hd

theorem fmt_u32_roundtrip (buf : Array U8 40#usize) (v : U32) :
    itoa_core.fmt_u32 buf v ⦃ start buf' => Rt v.val start.val buf' ⦄ := by
  unfold itoa_core.fmt_u32
  step*

theorem start_ge {m s : ℕ} {b : Array U8 40#usize} (h : Rt m s b) (hm : m ≤ 2 ^ 63) : 20 ≤ s := by
  obtain ⟨s1, s2, s3, ⟨s4, s5⟩, s6, s7⟩ := h
  by_contra hc
  rcases s5 with h | h
  · omega
  · have h1 : 10 ^ 19 ≤ 10 ^ (39 - s) := Nat.pow_le_pow_right (by norm_num) (by omega)
    have h2 : (10:ℕ) ^ 19 > 2 ^ 63 := by norm_num
    omega

/-- `i64`: the magnitude is formatted as an unsigned number and a `-` is put just before it. -/
theorem fmt_i64_roundtrip (buf : Array U8 40#usize) (v : I64) :
    itoa_core.fmt_i64 buf v ⦃ start buf' =>
      start.val ≤ 39 ∧
      (v.val < 0 → 1 ≤ start.val ∧ (buf'.val[start.val]!).val = 45 ∧ Rt v.val.natAbs (start.val + 1) buf') ∧
      (0 ≤ v.val → Rt v.val.toNat start.val buf') ⦄ := by
  unfold itoa_core.fmt_i64
  by_cases hneg : v < 0#i64
  · rw [if_pos hneg]
    step*
    · scalar_tac
    · have hv : v.val < 0 := by scalar_tac
      have hmag : mag.val = v.val.natAbs := by scalar_tac
      rw [hmag] at start_post
      have := start_ge start_post (by scalar_tac)
      scalar_tac
    · have hv : v.val < 0 := by scalar_tac
      have hmag : mag.val = v.val.natAbs := by scalar_tac
      rw [hmag] at start_post
      have hs := start_ge start_post (by scalar_tac)
      have h39 := start_post.1
      have hb : ∀ j, j ≠ i.val → buf2.val[j]! = buf1.val[j]! := by
        intro j hj
        rw [buf2_post, set_get _ _ _ (by omega)]
        simp [hj]
      refine ⟨by omega, fun _ => ⟨by omega, ?_, ?_⟩, fun h => by omega⟩
      · rw [buf2_post, set_get _ _ _ (by omega)]
        simp
      · rw [show i.val + 1 = start.val by omega]
        exact Rt_frame start_post (fun j hj1 hj2 => hb j (by omega))
  · rw [if_neg hneg]
    step*
    have hv : 0 ≤ v.val := by scalar_tac
    have hmag : (IScalar.hcast UScalarTy.U64 v).val = v.val.toNat := by scalar_tac
    rw [hmag] at start_post
    have h39 := start_post.1
    exact ⟨by omega, fun h => by omega, fun _ => start_post⟩

end wirefold_extract
