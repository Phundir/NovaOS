//! The NIST curves P-256 and P-384: ECDSA verification and ECDH.
//!
//! Certificates are signed with ECDSA on either curve -- Let's Encrypt's
//! ECDSA chain is P-384 above a P-256 leaf -- and a TLS 1.3 server may ask
//! for a P-256 key share instead of X25519. Both curves have `a = -3`, so
//! one doubling formula serves.
//!
//! Points are Jacobian (X, Y, Z) with coordinates in Montgomery form;
//! the point at infinity is Z = 0.

use alloc::vec::Vec;
use core::cmp::Ordering;

use super::bignum::{self, Mont};

pub struct Curve {
    pub name: &'static str,
    /// Size of a coordinate, in bytes.
    pub size: usize,
    field: Mont,
    order: Mont,
    b: Vec<u64>,
    gx: Vec<u64>,
    gy: Vec<u64>,
}

fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| byte.is_ascii_hexdigit())
        .map(|byte| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => byte - b'A' + 10,
        })
        .collect();
    digits.chunks(2).map(|pair| (pair[0] << 4) | pair[1]).collect()
}

impl Curve {
    fn build(
        name: &'static str,
        size: usize,
        p: &str,
        n: &str,
        b: &str,
        gx: &str,
        gy: &str,
    ) -> Curve {
        let limbs = size / 8;
        let field = Mont::new(bignum::from_be(&hex(p), limbs)).expect("odd prime");
        let order = Mont::new(bignum::from_be(&hex(n), limbs)).expect("odd order");
        let b = field.to(&bignum::from_be(&hex(b), limbs));
        let gx = field.to(&bignum::from_be(&hex(gx), limbs));
        let gy = field.to(&bignum::from_be(&hex(gy), limbs));
        Curve {
            name,
            size,
            field,
            order,
            b,
            gx,
            gy,
        }
    }

    pub fn p256() -> Curve {
        Curve::build(
            "P-256",
            32,
            "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
            "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
            "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
            "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
            "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
        )
    }

    pub fn p384() -> Curve {
        Curve::build(
            "P-384",
            48,
            "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe\
             ffffffff0000000000000000ffffffff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf\
             581a0db248b0a77aecec196accc52973",
            "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875a\
             c656398d8a2ed19d2a85c8edd3ec2aef",
            "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a38\
             5502f25dbf55296c3a545e3872760ab7",
            "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c0\
             0a60b1ce1d7e819d7a431d7c90ea0e5f",
        )
    }

    fn zero(&self) -> Vec<u64> {
        alloc::vec![0u64; self.field.limbs()]
    }

    /// Is (x, y), in Montgomery form, on the curve?
    fn on_curve(&self, x: &[u64], y: &[u64]) -> bool {
        let f = &self.field;
        let y2 = f.mul(y, y);
        let x3 = f.mul(&f.mul(x, x), x);
        let three_x = f.add(&f.add(x, x), x);
        let right = f.add(&f.sub(&x3, &three_x), &self.b);
        y2 == right
    }

    fn double(&self, point: &Point) -> Point {
        let f = &self.field;
        if bignum::is_zero(&point.z) {
            return point.clone();
        }
        // dbl-2001-b, a = -3.
        let delta = f.mul(&point.z, &point.z);
        let gamma = f.mul(&point.y, &point.y);
        let beta = f.mul(&point.x, &gamma);
        let t1 = f.sub(&point.x, &delta);
        let t2 = f.add(&point.x, &delta);
        let t3 = f.mul(&t1, &t2);
        let alpha = f.add(&f.add(&t3, &t3), &t3);
        let beta4 = f.add(&f.add(&beta, &beta), &f.add(&beta, &beta));
        let beta8 = f.add(&beta4, &beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&point.y, &point.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let gamma2 = f.mul(&gamma, &gamma);
        let gamma2_8 = {
            let two = f.add(&gamma2, &gamma2);
            let four = f.add(&two, &two);
            f.add(&four, &four)
        };
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &gamma2_8);
        Point { x: x3, y: y3, z: z3 }
    }

    fn add(&self, p: &Point, q: &Point) -> Point {
        let f = &self.field;
        if bignum::is_zero(&p.z) {
            return q.clone();
        }
        if bignum::is_zero(&q.z) {
            return p.clone();
        }
        // add-2007-bl.
        let z1z1 = f.mul(&p.z, &p.z);
        let z2z2 = f.mul(&q.z, &q.z);
        let u1 = f.mul(&p.x, &z2z2);
        let u2 = f.mul(&q.x, &z1z1);
        let s1 = f.mul(&f.mul(&p.y, &q.z), &z2z2);
        let s2 = f.mul(&f.mul(&q.y, &p.z), &z1z1);
        if u1 == u2 {
            if s1 == s2 {
                return self.double(p);
            }
            return Point {
                x: f.one.clone(),
                y: f.one.clone(),
                z: self.zero(),
            };
        }
        let h = f.sub(&u2, &u1);
        let h2 = f.add(&h, &h);
        let i = f.mul(&h2, &h2);
        let j = f.mul(&h, &i);
        let r = {
            let d = f.sub(&s2, &s1);
            f.add(&d, &d)
        };
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zz = f.add(&p.z, &q.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zz, &zz), &z1z1), &z2z2), &h);
        Point { x: x3, y: y3, z: z3 }
    }

    /// scalar * point, scalar as plain limbs.
    fn multiply(&self, scalar: &[u64], point: &Point) -> Point {
        let mut result = Point {
            x: self.field.one.clone(),
            y: self.field.one.clone(),
            z: self.zero(),
        };
        for index in (0..bignum::bits(scalar)).rev() {
            result = self.double(&result);
            if (scalar[index / 64] >> (index % 64)) & 1 == 1 {
                result = self.add(&result, point);
            }
        }
        result
    }

    /// Affine x and y, out of Montgomery form. `None` at infinity.
    fn affine(&self, point: &Point) -> Option<(Vec<u64>, Vec<u64>)> {
        if bignum::is_zero(&point.z) {
            return None;
        }
        let f = &self.field;
        let zinv = f.invert(&point.z);
        let zinv2 = f.mul(&zinv, &zinv);
        let zinv3 = f.mul(&zinv2, &zinv);
        Some((f.from(&f.mul(&point.x, &zinv2)), f.from(&f.mul(&point.y, &zinv3))))
    }

    fn generator(&self) -> Point {
        Point {
            x: self.gx.clone(),
            y: self.gy.clone(),
            z: self.field.one.clone(),
        }
    }

    /// An uncompressed point `04 || x || y`, checked to be on the curve.
    fn decode(&self, encoded: &[u8]) -> Option<Point> {
        if encoded.len() != 1 + 2 * self.size || encoded[0] != 4 {
            return None;
        }
        let limbs = self.field.limbs();
        let x = bignum::from_be(&encoded[1..1 + self.size], limbs);
        let y = bignum::from_be(&encoded[1 + self.size..], limbs);
        if bignum::compare(&x, &self.field.modulus) != Ordering::Less
            || bignum::compare(&y, &self.field.modulus) != Ordering::Less
        {
            return None;
        }
        let (x, y) = (self.field.to(&x), self.field.to(&y));
        if !self.on_curve(&x, &y) {
            return None;
        }
        Some(Point {
            x,
            y,
            z: self.field.one.clone(),
        })
    }

    /// The hash as an integer modulo the order, truncated to its bit
    /// length (FIPS 186-4, 6.4).
    fn hash_scalar(&self, digest: &[u8]) -> Vec<u64> {
        let order_bits = bignum::bits(&self.order.modulus);
        // Wide enough for the whole digest: `from_be` keeps the low bytes,
        // and truncation keeps the **high** ones.
        let width = digest.len().div_ceil(8) + 1;
        let mut value = bignum::from_be(digest, width);
        let digest_bits = digest.len() * 8;
        if digest_bits > order_bits {
            // Shift right by the excess.
            let shift = digest_bits - order_bits;
            let mut shifted = alloc::vec![0u64; width];
            for index in 0..width * 64 {
                let source = index + shift;
                if source / 64 < value.len() && (value[source / 64] >> (source % 64)) & 1 == 1 {
                    shifted[index / 64] |= 1 << (index % 64);
                }
            }
            value = shifted;
        }
        bignum::reduce(&value, &self.order.modulus)
    }

    /// ECDSA verification of `r`, `s` (big-endian) over a digest.
    pub fn verify(&self, public: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
        let Some(q) = self.decode(public) else {
            return false;
        };
        let limbs = self.order.limbs();
        let strip = |bytes: &[u8]| {
            let start = bytes.iter().position(|byte| *byte != 0).unwrap_or(bytes.len());
            bytes[start..].to_vec()
        };
        let (r, s) = (strip(r), strip(s));
        if r.len() > self.size || s.len() > self.size || r.is_empty() || s.is_empty() {
            return false;
        }
        let r = bignum::from_be(&r, limbs);
        let s = bignum::from_be(&s, limbs);
        let n = &self.order;
        if bignum::compare(&r, &n.modulus) != Ordering::Less
            || bignum::compare(&s, &n.modulus) != Ordering::Less
        {
            return false;
        }
        let z = self.hash_scalar(digest);
        let w = n.invert(&n.to(&s));
        let u1 = n.from(&n.mul(&n.to(&z), &w));
        let u2 = n.from(&n.mul(&n.to(&r), &w));
        let sum = self.add(&self.multiply(&u1, &self.generator()), &self.multiply(&u2, &q));
        let Some((x, _)) = self.affine(&sum) else {
            return false;
        };
        bignum::reduce(&x, &n.modulus) == r
    }

    /// Public key `04 || x || y` of a private scalar (big-endian, below
    /// the order).
    pub fn public_key(&self, private: &[u8]) -> Option<Vec<u8>> {
        let scalar = bignum::from_be(private, self.order.limbs());
        let (x, y) = self.affine(&self.multiply(&scalar, &self.generator()))?;
        let mut out = alloc::vec![4u8];
        out.extend_from_slice(&bignum::to_be(&x, self.size));
        out.extend_from_slice(&bignum::to_be(&y, self.size));
        Some(out)
    }

    /// ECDH: the x coordinate of `private * peer`.
    pub fn shared_secret(&self, private: &[u8], peer: &[u8]) -> Option<Vec<u8>> {
        let q = self.decode(peer)?;
        let scalar = bignum::from_be(private, self.order.limbs());
        let (x, _) = self.affine(&self.multiply(&scalar, &q))?;
        Some(bignum::to_be(&x, self.size))
    }

    /// A private scalar from random bytes: reduced below the order and
    /// never zero.
    pub fn private_from(&self, random: &[u8]) -> Vec<u8> {
        let limbs = self.order.limbs();
        let mut value = bignum::reduce(&bignum::from_be(random, limbs + 1), &self.order.modulus);
        if bignum::is_zero(&value) {
            value[0] = 1;
        }
        bignum::to_be(&value, self.size)
    }
}

#[derive(Clone)]
struct Point {
    x: Vec<u64>,
    y: Vec<u64>,
    z: Vec<u64>,
}
