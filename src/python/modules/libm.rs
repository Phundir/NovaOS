//! The libm functions `math` needs beyond `Num`'s own, ported from
//! fdlibm (as glibc's are) and from CPython's mathmodule.c (`gamma`,
//! `lgamma`), on soft-float doubles.
//!
//! The ports keep the reference's evaluation order operation for
//! operation: rounding happens at the same points, so the results are
//! glibc's and CPython's to the last bit (except where the host's libm
//! fuses a multiply-add, which soft float does not do). `F` wraps a `Num`
//! with the arithmetic operators so the formulas read like the C.

use core::ops::{Add, Div, Mul, Neg, Sub};

use crate::web::js::num::Num;

#[derive(Clone, Copy)]
pub struct F(pub Num);

impl Add for F {
    type Output = F;
    fn add(self, o: F) -> F {
        F(self.0.add(o.0))
    }
}

impl Sub for F {
    type Output = F;
    fn sub(self, o: F) -> F {
        F(self.0.sub(o.0))
    }
}

impl Mul for F {
    type Output = F;
    fn mul(self, o: F) -> F {
        F(self.0.mul(o.0))
    }
}

impl Div for F {
    type Output = F;
    fn div(self, o: F) -> F {
        F(self.0.div(o.0))
    }
}

impl Neg for F {
    type Output = F;
    fn neg(self) -> F {
        F(self.0.neg())
    }
}

/// A constant from its IEEE 754 bits.
const fn b(bits: u64) -> F {
    F(Num(bits))
}

fn fi(x: i64) -> F {
    F(Num::from_i64(x))
}

const ONE: F = b(0x3ff0_0000_0000_0000);
const HALF: F = b(0x3fe0_0000_0000_0000);
const TWO: F = b(0x4000_0000_0000_0000);
const ZERO: F = b(0);

impl F {
    fn hi(self) -> i32 {
        (self.0 .0 >> 32) as u32 as i32
    }

    fn lo(self) -> u32 {
        self.0 .0 as u32
    }

    fn with_hi(self, hi: i32) -> F {
        F(Num(((hi as u32 as u64) << 32) | (self.0 .0 & 0xffff_ffff)))
    }

    fn with_lo(self, lo: u32) -> F {
        F(Num((self.0 .0 & 0xffff_ffff_0000_0000) | lo as u64))
    }

    fn abs(self) -> F {
        F(self.0.abs())
    }

    fn lt(self, o: F) -> bool {
        self.0.lt(o.0)
    }

    fn gt(self, o: F) -> bool {
        o.0.lt(self.0)
    }

    fn exp(self) -> F {
        F(self.0.exp())
    }

    fn ln(self) -> F {
        F(self.0.ln())
    }

    fn sqrt(self) -> F {
        F(self.0.sqrt())
    }

    fn copysign(self, sign: F) -> F {
        F(Num((self.0 .0 & !(1 << 63)) | (sign.0 .0 & (1 << 63))))
    }
}

// ------------------------------------------------------------
//  log1p, expm1 (fdlibm s_log1p.c, s_expm1.c)
// ------------------------------------------------------------

const LN2_HI: F = b(0x3fe6_2e42_fee0_0000);
const LN2_LO: F = b(0x3dea_39ef_3579_3c76);

const LP: [F; 7] = [
    b(0x3fe5_5555_5555_5593),
    b(0x3fd9_9999_9997_fa04),
    b(0x3fd2_4924_9422_9359),
    b(0x3fcc_71c5_1d8e_78af),
    b(0x3fc7_4664_96cb_03de),
    b(0x3fc3_9a09_d078_c69f),
    b(0x3fc2_f112_df3e_5244),
];

pub fn log1p(x: Num) -> Num {
    let x = F(x);
    let hx = x.hi();
    let ax = hx & 0x7fff_ffff;
    let mut k = 1i32;
    let mut f = ZERO;
    let mut hu = 0i32;
    let mut c = ZERO;
    if hx < 0x3fda_827a {
        if ax >= 0x3ff0_0000 {
            if x.0.equals(Num::ONE.neg()) {
                return Num::NEG_INFINITY;
            }
            return Num::NAN;
        }
        if ax < 0x3e20_0000 {
            if ax < 0x3c90_0000 {
                return x.0;
            }
            return (x - x * x * HALF).0;
        }
        if hx > 0 || hx <= 0xbfd2_bec4_u32 as i32 {
            k = 0;
            f = x;
            hu = 1;
        }
    }
    if hx >= 0x7ff0_0000 {
        return (x + x).0;
    }
    if k != 0 {
        let mut u;
        if hx < 0x4340_0000 {
            u = ONE + x;
            hu = u.hi();
            k = (hu >> 20) - 1023;
            c = if k > 0 { ONE - (u - x) } else { x - (u - ONE) };
            c = c / u;
        } else {
            u = x;
            hu = u.hi();
            k = (hu >> 20) - 1023;
            c = ZERO;
        }
        hu &= 0x000f_ffff;
        if hu < 0x6a09e {
            u = u.with_hi(hu | 0x3ff0_0000);
        } else {
            k += 1;
            u = u.with_hi(hu | 0x3fe0_0000);
            hu = (0x0010_0000 - hu) >> 2;
        }
        f = u - ONE;
    }
    let hfsq = HALF * f * f;
    let kf = fi(k as i64);
    if hu == 0 {
        if f.0.is_zero() {
            if k == 0 {
                return Num::ZERO;
            }
            let c = c + kf * LN2_LO;
            return (kf * LN2_HI + c).0;
        }
        let r = hfsq * (ONE - b(0x3fe5_5555_5555_5555) * f);
        if k == 0 {
            return (f - r).0;
        }
        return (kf * LN2_HI - ((r - (kf * LN2_LO + c)) - f)).0;
    }
    let s = f / (TWO + f);
    let z = s * s;
    let r1 = z * LP[0];
    let z2 = z * z;
    let r2 = LP[1] + z * LP[2];
    let z4 = z2 * z2;
    let r3 = LP[3] + z * LP[4];
    let z6 = z4 * z2;
    let r4 = LP[5] + z * LP[6];
    let r = r1 + z2 * r2 + z4 * r3 + z6 * r4;
    if k == 0 {
        return (f - (hfsq - s * (hfsq + r))).0;
    }
    (kf * LN2_HI - ((hfsq - (s * (hfsq + r) + (kf * LN2_LO + c))) - f)).0
}

const O_THRESHOLD: F = b(0x4086_2e42_fefa_39ef);
const INVLN2: F = b(0x3ff7_1547_652b_82fe);
const Q: [F; 5] = [
    b(0xbfa1_1111_1111_10f4),
    b(0x3f5a_01a0_19fe_5585),
    b(0xbf14_ce19_9eaa_dbb7),
    b(0x3ed0_cfca_86e6_5239),
    b(0xbe8a_fdb7_6e09_c32d),
];

pub fn expm1(x: Num) -> Num {
    let mut x = F(x);
    let hx0 = x.hi() as u32;
    let negative = hx0 & 0x8000_0000 != 0;
    let hx = hx0 & 0x7fff_ffff;
    if hx >= 0x4043_687a {
        if hx >= 0x4086_2e42 {
            if hx >= 0x7ff0_0000 {
                if ((hx & 0xfffff) | x.lo()) != 0 {
                    return (x + x).0;
                }
                return if negative { Num::ONE.neg() } else { x.0 };
            }
            if x.gt(O_THRESHOLD) {
                return Num::INFINITY;
            }
        }
        if negative {
            return Num::ONE.neg();
        }
    }
    let k: i32;
    let mut cc = ZERO;
    if hx > 0x3fd6_2e42 {
        let (hi, lo);
        if hx < 0x3ff0_a2b2 {
            if !negative {
                hi = x - LN2_HI;
                lo = LN2_LO;
                k = 1;
            } else {
                hi = x + LN2_HI;
                lo = -LN2_LO;
                k = -1;
            }
        } else {
            let t0 = INVLN2 * x + if negative { -HALF } else { HALF };
            k = t0.0.trunc().to_i64_saturating() as i32;
            let t = fi(k as i64);
            hi = x - t * LN2_HI;
            lo = t * LN2_LO;
        }
        x = hi - lo;
        cc = (hi - x) - lo;
    } else if hx < 0x3c90_0000 {
        return x.0;
    } else {
        k = 0;
    }
    let hfx = HALF * x;
    let hxs = x * hfx;
    let r1a = ONE + hxs * Q[0];
    let h4 = hxs * hxs;
    let r2 = Q[1] + hxs * Q[2];
    let h8 = h4 * h4;
    let r3 = Q[3] + hxs * Q[4];
    let r1 = r1a + h4 * r2 + h8 * r3;
    let t = fi(3) - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (fi(6) - x * t));
    if k == 0 {
        return (x - (x * e - hxs)).0;
    }
    e = x * (e - cc) - cc;
    e = e - hxs;
    if k == -1 {
        return (HALF * (x - e) - HALF).0;
    }
    if k == 1 {
        if x.lt(b(0xbfd0_0000_0000_0000)) {
            return (-TWO * (e - (x + HALF))).0;
        }
        return (ONE + TWO * (x - e)).0;
    }
    if k <= -2 || k > 56 {
        let mut y = ONE - (e - x);
        y = y.with_hi(y.hi().wrapping_add(k << 20));
        return (y - ONE).0;
    }
    let y;
    if k < 20 {
        let t = ONE.with_hi(0x3ff0_0000 - (0x0020_0000 >> k));
        let y0 = t - (e - x);
        y = y0.with_hi(y0.hi().wrapping_add(k << 20));
    } else {
        let t = ZERO.with_hi((0x3ff - k) << 20);
        let mut y0 = x - (e + t);
        y0 = y0 + ONE;
        y = y0.with_hi(y0.hi().wrapping_add(k << 20));
    }
    y.0
}

// ------------------------------------------------------------
//  sinh, cosh, tanh, asinh, acosh, atanh (fdlibm)
// ------------------------------------------------------------

pub fn sinh(x: Num) -> Num {
    let x = F(x);
    let jx = x.hi();
    let ix = jx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return (x + x).0;
    }
    let h = if jx < 0 { -HALF } else { HALF };
    if ix < 0x4036_0000 {
        if ix < 0x3e30_0000 {
            return x.0;
        }
        let t = F(expm1(x.abs().0));
        if ix < 0x3ff0_0000 {
            return (h * (TWO * t - t * t / (t + ONE))).0;
        }
        return (h * (t + t / (t + ONE))).0;
    }
    if ix < 0x4086_2e42 {
        return (h * x.abs().exp()).0;
    }
    if ix <= 0x4086_33ce {
        let w = (HALF * x.abs()).exp();
        let t = h * w;
        return (t * w).0;
    }
    if jx < 0 { Num::NEG_INFINITY } else { Num::INFINITY }
}

pub fn cosh(x: Num) -> Num {
    let x = F(x);
    let ix = x.hi() & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return (x * x).0;
    }
    if ix < 0x3fd6_2e43 {
        if ix < 0x3c80_0000 {
            return Num::ONE;
        }
        let t = F(expm1(x.abs().0));
        let w = ONE + t;
        return (ONE + (t * t) / (w + w)).0;
    }
    if ix < 0x4036_0000 {
        let t = x.abs().exp();
        return (HALF * t + HALF / t).0;
    }
    if ix < 0x4086_2e42 {
        return (HALF * x.abs().exp()).0;
    }
    if ix <= 0x4086_33ce {
        let w = (HALF * x.abs()).exp();
        let t = HALF * w;
        return (t * w).0;
    }
    Num::INFINITY
}

pub fn tanh(x: Num) -> Num {
    let x = F(x);
    let jx = x.hi();
    let ix = jx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        if x.0.is_nan() {
            return x.0;
        }
        return if jx >= 0 { Num::ONE } else { Num::ONE.neg() };
    }
    let z;
    if ix < 0x4036_0000 {
        if ix < 0x3c80_0000 {
            return (x * (ONE + x)).0;
        }
        if ix >= 0x3ff0_0000 {
            let t = F(expm1((TWO * x.abs()).0));
            z = ONE - TWO / (t + TWO);
        } else {
            let t = F(expm1((-TWO * x.abs()).0));
            z = -t / (t + TWO);
        }
    } else {
        z = ONE;
    }
    if jx >= 0 { z.0 } else { z.0.neg() }
}

pub fn asinh(x: Num) -> Num {
    let x = F(x);
    let hx = x.hi();
    let ix = hx & 0x7fff_ffff;
    if ix < 0x3e30_0000 {
        return x.0;
    }
    let w;
    if ix > 0x41b0_0000 {
        if ix >= 0x7ff0_0000 {
            return (x + x).0;
        }
        w = x.abs().ln() + b(0x3fe6_2e42_fefa_39ef);
    } else {
        let xa = x.abs();
        if ix > 0x4000_0000 {
            w = (TWO * xa + ONE / ((xa * xa + ONE).sqrt() + xa)).ln();
        } else {
            let t = xa * xa;
            w = F(log1p((xa + t / (ONE + (ONE + t).sqrt())).0));
        }
    }
    w.copysign(x).0
}

pub fn acosh(x: Num) -> Num {
    let x = F(x);
    let hx = x.hi();
    if hx < 0x3ff0_0000 {
        return Num::NAN;
    }
    if hx >= 0x41b0_0000 {
        if hx >= 0x7ff0_0000 {
            return (x + x).0;
        }
        return (x.ln() + b(0x3fe6_2e42_fefa_39ef)).0;
    }
    if x.0.equals(Num::ONE) {
        return Num::ZERO;
    }
    if hx > 0x4000_0000 {
        let t = x * x;
        return (TWO * x - ONE / (x + (t - ONE).sqrt())).ln().0;
    }
    let t = x - ONE;
    log1p((t + (TWO * t + t * t).sqrt()).0)
}

pub fn atanh(x: Num) -> Num {
    let x = F(x);
    let xa = x.abs();
    let t;
    if xa.lt(HALF) {
        if xa.lt(b(0x3e30_0000_0000_0000)) {
            return x.0;
        }
        let t2 = xa + xa;
        t = HALF * F(log1p((t2 + t2 * xa / (ONE - xa)).0));
    } else if xa.lt(ONE) {
        t = HALF * F(log1p(((xa + xa) / (ONE - xa)).0));
    } else if xa.0.equals(Num::ONE) {
        return if x.0.is_negative() { Num::NEG_INFINITY } else { Num::INFINITY };
    } else {
        return Num::NAN;
    }
    t.copysign(x).0
}

// ------------------------------------------------------------
//  erf, erfc (fdlibm s_erf.c, glibc's evaluation order)
// ------------------------------------------------------------

const ERX: F = b(0x3feb_0ac1_6000_0000);
const EFX: F = b(0x3fc0_6eba_8214_db69);
const PP: [F; 5] = [
    b(0x3fc0_6eba_8214_db68),
    b(0xbfd4_cd7d_691c_b913),
    b(0xbf9d_2a51_dbd7_194f),
    b(0xbf77_a291_2366_68e4),
    b(0xbef8_ead6_1200_16ac),
];
const QQ: [F; 5] = [
    b(0x3fd9_7779_cdda_dc09),
    b(0x3fb0_a54c_5536_ceba),
    b(0x3f74_d022_c4d3_6b0f),
    b(0x3f21_5dc9_221c_1a10),
    b(0xbed0_9c43_42a2_6120),
];
const PA: [F; 7] = [
    b(0xbf63_59b8_bef7_7538),
    b(0x3fda_8d00_ad92_b34d),
    b(0xbfd7_d240_fbb8_c3f1),
    b(0x3fd4_5fca_8051_20e4),
    b(0xbfbc_6398_3d3e_28ec),
    b(0x3fa2_2a36_5997_95eb),
    b(0xbf61_bf38_0a96_073f),
];
const QA: [F; 6] = [
    b(0x3fbb_3e66_18ee_e323),
    b(0x3fe1_4af0_92eb_6f33),
    b(0x3fb2_635c_d99f_e9a7),
    b(0x3fc0_2660_e763_351f),
    b(0x3f8b_edc2_6b51_dd1c),
    b(0x3f88_8b54_5735_151d),
];
const RA: [F; 8] = [
    b(0xbf84_3412_600d_6435),
    b(0xbfe6_3416_e4ba_7360),
    b(0xc025_1e04_41b0_e726),
    b(0xc04f_300a_e4cb_a38d),
    b(0xc064_4cb1_8428_2266),
    b(0xc067_135c_ebcc_abb2),
    b(0xc054_5265_57e4_d2f2),
    b(0xc023_a0ef_c69a_c25c),
];
const SA: [F; 8] = [
    b(0x4033_a6b9_bd70_7687),
    b(0x4061_350c_526a_e721),
    b(0x407b_290d_d58a_1a71),
    b(0x4084_2b19_21ec_2868),
    b(0x407a_d021_5770_0314),
    b(0x405b_28a3_ee48_ae2c),
    b(0x401a_47ef_8e48_4a93),
    b(0xbfae_eff2_ee74_9a62),
];
const RB: [F; 7] = [
    b(0xbf84_3412_39e8_6f4a),
    b(0xbfe9_93ba_70c2_85de),
    b(0xc031_c209_555f_995a),
    b(0xc064_145d_43c5_ed98),
    b(0xc083_ec88_1375_f228),
    b(0xc090_0461_6a2e_5992),
    b(0xc07e_384e_9bdc_383f),
];
const SB: [F; 7] = [
    b(0x403e_568b_261d_5190),
    b(0x4074_5cae_221b_9f0a),
    b(0x4098_02eb_189d_5118),
    b(0x40a8_ffb7_688c_246a),
    b(0x40a3_f219_cedf_3be6),
    b(0x407d_a874_e79f_e763),
    b(0xc036_70e2_4271_2d62),
];

/// The tail shared by erf and erfc: exp(-x*x - 0.5625 + R/S) / x, the
/// square formed from the high half of x exactly.
fn erf_tail(x: F, ix: i32, erf: bool) -> F {
    let s = ONE / (x * x);
    let (r, ss);
    let limit = if erf { 0x4006_db6e } else { 0x4006_db6d };
    if ix < limit {
        let r1 = RA[0] + s * RA[1];
        let s2 = s * s;
        let s1 = ONE + s * SA[0];
        let s4 = s2 * s2;
        let r2 = RA[2] + s * RA[3];
        let s6 = s4 * s2;
        let ss2 = SA[1] + s * SA[2];
        let s8 = s4 * s4;
        let r3 = RA[4] + s * RA[5];
        let ss3 = SA[3] + s * SA[4];
        let r4 = RA[6] + s * RA[7];
        let ss4 = SA[5] + s * SA[6];
        r = r1 + s2 * r2 + s4 * r3 + s6 * r4;
        ss = s1 + s2 * ss2 + s4 * ss3 + s6 * ss4 + s8 * SA[7];
    } else {
        let r1 = RB[0] + s * RB[1];
        let s2 = s * s;
        let s1 = ONE + s * SB[0];
        let s4 = s2 * s2;
        let r2 = RB[2] + s * RB[3];
        let s6 = s4 * s2;
        let ss2 = SB[1] + s * SB[2];
        let r3 = RB[4] + s * RB[5];
        let ss3 = SB[3] + s * SB[4];
        let ss4 = SB[5] + s * SB[6];
        r = r1 + s2 * r2 + s4 * r3 + s6 * RB[6];
        ss = s1 + s2 * ss2 + s4 * ss3 + s6 * ss4;
    }
    let z = x.with_lo(0);
    (-z * z - b(0x3fe2_0000_0000_0000)).exp() * ((z - x) * (z + x) + r / ss).exp()
}

fn erf_small(x: F) -> F {
    let z = x * x;
    let r1 = PP[0] + z * PP[1];
    let z2 = z * z;
    let r2 = PP[2] + z * PP[3];
    let z4 = z2 * z2;
    let s1 = ONE + z * QQ[0];
    let s2 = QQ[1] + z * QQ[2];
    let s3 = QQ[3] + z * QQ[4];
    let r = r1 + z2 * r2 + z4 * PP[4];
    let s = s1 + z2 * s2 + z4 * s3;
    r / s
}

fn erf_middle(x: F) -> (F, F) {
    let s = x.abs() - ONE;
    let p1 = PA[0] + s * PA[1];
    let s2 = s * s;
    let q1 = ONE + s * QA[0];
    let s4 = s2 * s2;
    let p2 = PA[2] + s * PA[3];
    let s6 = s4 * s2;
    let q2 = QA[1] + s * QA[2];
    let p3 = PA[4] + s * PA[5];
    let q3 = QA[3] + s * QA[4];
    let p = p1 + s2 * p2 + s4 * p3 + s6 * PA[6];
    let q = q1 + s2 * q2 + s4 * q3 + s6 * QA[5];
    (p, q)
}

pub fn erf(x: Num) -> Num {
    let x = F(x);
    let hx = x.hi();
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        if x.0.is_nan() {
            return x.0;
        }
        return if hx < 0 { Num::ONE.neg() } else { Num::ONE };
    }
    if ix < 0x3feb_0000 {
        if ix < 0x3e30_0000 {
            if ix < 0x0080_0000 {
                return (b(0x3fc0_0000_0000_0000) * (fi(8) * x + b(0x3ff0_6eba_8214_db69) * x)).0;
            }
            return (x + EFX * x).0;
        }
        return (x + x * erf_small(x)).0;
    }
    if ix < 0x3ff4_0000 {
        let (p, q) = erf_middle(x);
        return if hx >= 0 { (ERX + p / q).0 } else { (-ERX - p / q).0 };
    }
    if ix >= 0x4018_0000 {
        return if hx >= 0 { Num::ONE } else { Num::ONE.neg() };
    }
    let xa = x.abs();
    let r = erf_tail(xa, ix, true);
    if hx >= 0 { (ONE - r / xa).0 } else { (r / xa - ONE).0 }
}

pub fn erfc(x: Num) -> Num {
    let x = F(x);
    let hx = x.hi();
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        if x.0.is_nan() {
            return x.0;
        }
        return if hx < 0 { Num::TWO } else { Num::ZERO };
    }
    if ix < 0x3feb_0000 {
        if ix < 0x3c70_0000 {
            return (ONE - x).0;
        }
        let y = erf_small(x);
        if hx < 0x3fd0_0000 {
            return (ONE - (x + x * y)).0;
        }
        let mut r = x * y;
        r = r + (x - HALF);
        return (HALF - r).0;
    }
    if ix < 0x3ff4_0000 {
        let (p, q) = erf_middle(x);
        if hx >= 0 {
            let z = ONE - ERX;
            return (z - p / q).0;
        }
        let z = ERX + p / q;
        return (ONE + z).0;
    }
    if ix < 0x403c_0000 {
        if hx < 0 && ix >= 0x4018_0000 {
            return Num::TWO;
        }
        let xa = x.abs();
        let r = erf_tail(xa, ix, false);
        return if hx > 0 { (r / xa).0 } else { (TWO - r / xa).0 };
    }
    if hx > 0 { Num::ZERO } else { Num::TWO }
}

// ------------------------------------------------------------
//  gamma, lgamma (CPython's mathmodule.c, Lanczos)
// ------------------------------------------------------------

const LANCZOS_G: F = b(0x4018_1945_b980_0000);
const LANCZOS_G_MINUS_HALF: F = b(0x4016_1945_b980_0000);
const LOGPI: F = b(0x3ff2_50d0_48e7_a1bd);

const LANCZOS_NUM: [u64; 13] = [
    0x4215_ea51_43c1_a49e,
    0x4223_fc70_75f5_4c57,
    0x4220_a132_818a_b61a,
    0x4210_b0b5_22e8_261a,
    0x41f6_7fc1_b3a5_a1e8,
    0x41d5_7418_f5d3_f33f,
    0x41ad_ab0c_7bb9_5f2a,
    0x417d_f876_f95d_cc98,
    0x4145_f1e9_5080_f44c,
    0x4106_b642_1f87_87eb,
    0x40bf_87ac_0858_d804,
    0x406a_5a60_7bbc_3b52,
    0x4004_0d93_1ff6_2705,
];

const LANCZOS_DEN: [i64; 13] = [0, 39916800, 120543840, 150917976, 105258076, 45995730, 13339535, 2637558, 357423, 32670, 1925, 66, 1];

fn lanczos_sum(x: F) -> F {
    let mut num = ZERO;
    let mut den = ZERO;
    if x.lt(fi(5)) {
        for i in (0..13).rev() {
            num = num * x + b(LANCZOS_NUM[i]);
            den = den * x + fi(LANCZOS_DEN[i]);
        }
    } else {
        for i in 0..13 {
            num = num / x + b(LANCZOS_NUM[i]);
            den = den / x + fi(LANCZOS_DEN[i]);
        }
    }
    num / den
}

fn sinpi(x: F) -> F {
    let y = F(x.abs().0.rem(Num::TWO));
    let n = (TWO * y).0.round().to_i64_saturating();
    let pi = F(Num::PI);
    let r = match n {
        0 => F((pi * y).0.sin()),
        1 => F((pi * (y - HALF)).0.cos()),
        2 => F((pi * (ONE - y)).0.sin()),
        3 => -F((pi * (y - b(0x3ff8_0000_0000_0000))).0.cos()),
        _ => F((pi * (y - TWO)).0.sin()),
    };
    ONE.copysign(x) * r
}

/// Gamma; `Err(true)` for a domain error, `Err(false)` for a range error.
pub fn gamma(x: Num) -> Result<Num, bool> {
    let x = F(x);
    if !x.0.is_finite() {
        if x.0.is_nan() || !x.0.is_negative() {
            return Ok(x.0);
        }
        return Err(true);
    }
    if x.0.is_zero() {
        return Err(true);
    }
    if x.0.is_integer() {
        if x.0.is_negative() {
            return Err(true);
        }
        if let Some(n) = x.0.to_i64_exact() {
            if n <= 23 {
                let mut f: u64 = 1;
                for i in 2..n as u64 {
                    f *= i;
                }
                return Ok(Num::from_u64(f));
            }
        }
    }
    let absx = x.abs();
    if absx.lt(b(0x3bc7_9ca1_0c92_4223)) {
        let r = ONE / x;
        if r.0.is_infinite() {
            return Err(false);
        }
        return Ok(r.0);
    }
    if absx.gt(fi(200)) {
        if x.0.is_negative() {
            return Ok((ZERO / sinpi(x)).0);
        }
        return Err(false);
    }
    let y = absx + LANCZOS_G_MINUS_HALF;
    let mut z = if absx.gt(LANCZOS_G_MINUS_HALF) {
        let q = y - absx;
        q - LANCZOS_G_MINUS_HALF
    } else {
        let q = y - LANCZOS_G_MINUS_HALF;
        q - absx
    };
    z = z * LANCZOS_G / y;
    let mut r;
    if x.0.is_negative() {
        r = -F(Num::PI) / sinpi(absx) / absx * y.exp() / lanczos_sum(absx);
        r = r - z * r;
        if absx.lt(fi(140)) {
            r = r / F(y.0.pow((absx - HALF).0));
        } else {
            let sqrtpow = F(y.0.pow((absx / TWO - b(0x3fd0_0000_0000_0000)).0));
            r = r / sqrtpow;
            r = r / sqrtpow;
        }
    } else {
        r = lanczos_sum(absx) / y.exp();
        r = r + z * r;
        if absx.lt(fi(140)) {
            r = r * F(y.0.pow((absx - HALF).0));
        } else {
            let sqrtpow = F(y.0.pow((absx / TWO - b(0x3fd0_0000_0000_0000)).0));
            r = r * sqrtpow;
            r = r * sqrtpow;
        }
    }
    if r.0.is_infinite() {
        return Err(false);
    }
    Ok(r.0)
}

/// log|Gamma(x)|; errors as `gamma`.
pub fn lgamma(x: Num) -> Result<Num, bool> {
    let x = F(x);
    if !x.0.is_finite() {
        if x.0.is_nan() {
            return Ok(x.0);
        }
        return Ok(Num::INFINITY);
    }
    if x.0.is_integer() && !x.gt(TWO) {
        if !x.gt(ZERO) {
            return Err(true);
        }
        return Ok(Num::ZERO);
    }
    let absx = x.abs();
    if absx.lt(b(0x3bc7_9ca1_0c92_4223)) {
        return Ok(absx.ln().0.neg());
    }
    let mut r = lanczos_sum(absx).ln() - LANCZOS_G;
    r = r + (absx - HALF) * ((absx + LANCZOS_G - HALF).ln() - ONE);
    if x.0.is_negative() {
        r = LOGPI - sinpi(absx).abs().ln() - absx.ln() - r;
    }
    if r.0.is_infinite() {
        return Err(false);
    }
    Ok(r.0)
}

// ------------------------------------------------------------
//  cbrt (glibc's dbl-64 s_cbrt.c: not correctly rounded, but glibc's)
// ------------------------------------------------------------

pub fn cbrt(x: Num) -> Num {
    if x.is_zero() || !x.is_finite() {
        return x.add(x);
    }
    // frexp: |x| = xm * 2^xe, xm in [0.5, 1).
    let (_, m, e) = x.decompose();
    let bits = 64 - m.leading_zeros() as i32;
    let xe = e + bits;
    let xm = F(x.abs().scale2(-xe));
    let u = b(0x3fd6_b69c_ba16_8ff2)
        + (b(0x3ff8_218d_de90_28b4)
            + (b(0xc000_eb82_77cd_8d5d)
                + (b(0x4003_9350_adad_51ec)
                    + (b(0xbffd_5ae6_cfa2_0f0c) + (b(0x3fe9_1e2a_6fe7_e984) - b(0x3fc2_9801_e893_366d) * xm) * xm) * xm)
                    * xm)
                * xm)
            * xm;
    let t2 = u * u * u;
    const FACTOR: [u64; 5] = [0x3fe4_28a2_f98d_728a, 0x3fe9_65fe_a53d_6e3c, 0x3ff0_0000_0000_0000, 0x3ff4_28a2_f98d_728b, 0x3ff9_65fe_a53d_6e3d];
    let ym = u * (t2 + TWO * xm) / (TWO * t2 + xm) * b(FACTOR[(2 + xe % 3) as usize]);
    let r = ym.0.scale2(xe / 3);
    if x.is_negative() { r.neg() } else { r }
}
