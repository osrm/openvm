use halo2curves_axiom::{
    bls12_381::{Fq, Fq12, Fq2, G1Affine},
    ff::Field,
};
use rand::{rngs::StdRng, SeedableRng};

use crate::{
    common::{fp12_square, EcPoint, EvaluatedLine, FieldExtension, LineMType},
    curves::bls12_381::{mul_023_by_023, mul_by_023, mul_by_02345, Bls12_381},
};

/// Returns a line function for a tangent line at the point P
#[cfg(test)]
#[allow(non_snake_case)]
pub fn point_to_023<Fp, Fp2>(P: EcPoint<Fp>) -> EvaluatedLine<Fp, Fp2>
where
    Fp: Field,
    Fp2: FieldExtension<BaseField = Fp>,
{
    let one = Fp2::ONE;
    let two = one + one;
    let three = one + two;
    let x = Fp2::embed(&P.x);
    let y = Fp2::embed(&P.y);

    // λ = (3x^2) / (2y)
    // 1 - λ(x/y)w^-1 + (λx - y)(1/y)w^-3
    // = (λx - y)(1/y) - λ(x/y)w^2 + w^3
    //
    // b = -(λ * x / y)
    //   = -3x^3 / 2y^2
    // c = (λ * x - y) / y
    //   = 3x^3/2y^2 - 1
    let x_squared = x.square();
    let x_cubed = x_squared * x;
    let y_squared = y.square();
    let three_x_cubed = three * x_cubed;
    let over_two_y_squared = (two * y_squared).invert().unwrap();

    let b = three_x_cubed.neg() * over_two_y_squared;
    let c = three_x_cubed * over_two_y_squared - Fp2::ONE;

    EvaluatedLine { b, c }
}

// TODO[yj]: Probably should refactor these tests so that they don't repeat the ones in BN254
#[test]
fn test_fp12_square() {
    let mut rng = StdRng::seed_from_u64(8);
    let rnd = Fq12::random(&mut rng);
    let sq = fp12_square::<Fq12>(rnd);
    let sq_native = rnd.square();
    assert_eq!(sq, sq_native);
}

#[test]
fn test_mul_023_by_023() {
    // Generate random curve points
    let mut rng = StdRng::seed_from_u64(8);
    let rnd_pt_0 = G1Affine::random(&mut rng);
    let rnd_pt_1 = G1Affine::random(&mut rng);
    let ec_point_0 = EcPoint::<Fq> {
        x: rnd_pt_0.x,
        y: rnd_pt_0.y,
    };
    let ec_point_1 = EcPoint::<Fq> {
        x: rnd_pt_1.x,
        y: rnd_pt_1.y,
    };

    // Get lines evaluated at rnd_pt_0 and rnd_pt_1
    let line_0 = point_to_023::<Fq, Fq2>(ec_point_0);
    let line_1 = point_to_023::<Fq, Fq2>(ec_point_1);

    // Multiply the two line functions & convert to Fq12 to compare
    let mul_023_by_023 = mul_023_by_023::<Fq, Fq2>(line_0, line_1, Bls12_381::xi());
    let mul_023_by_023 = Fq12::from_coeffs(&mul_023_by_023);

    // Compare with the result of multiplying two Fp12 elements
    let fp12_0 = Fq12::from_evaluated_line_m_type(line_0);
    let fp12_1 = Fq12::from_evaluated_line_m_type(line_1);
    let check_mul_fp12 = fp12_0 * fp12_1;
    assert_eq!(mul_023_by_023, check_mul_fp12);
}

#[test]
fn test_mul_by_023() {
    let mut rng = StdRng::seed_from_u64(8);
    let f = Fq12::random(&mut rng);
    let rnd_pt = G1Affine::random(&mut rng);
    let ec_point = EcPoint::<Fq> {
        x: rnd_pt.x,
        y: rnd_pt.y,
    };
    let line = point_to_023::<Fq, Fq2>(ec_point);
    let mul_by_023 = mul_by_023::<Fq, Fq2, Fq12>(f, line);

    let check_mul_fp12 = Fq12::from_evaluated_line_m_type(line) * f;
    assert_eq!(mul_by_023, check_mul_fp12);
}

#[test]
fn test_mul_by_02345() {
    let mut rng = StdRng::seed_from_u64(8);
    let f = Fq12::random(&mut rng);
    let x = [
        Fq2::random(&mut rng),
        Fq2::random(&mut rng),
        Fq2::random(&mut rng),
        Fq2::random(&mut rng),
        Fq2::random(&mut rng),
        Fq2::random(&mut rng),
    ];
    let mul_by_02345 = mul_by_02345::<Fq, Fq2, Fq12>(f, x);

    let x_f12 = Fq12::from_coeffs(&x);
    assert_eq!(mul_by_02345, f * x_f12);
}