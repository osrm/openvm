use alloc::vec::Vec;

use itertools::izip;
use openvm_algebra_guest::Field;
use openvm_ecc_guest::{algebra::field::FieldExtension, AffinePoint};
use rand::{rngs::StdRng, SeedableRng};

use crate::affine_point::AffineCoords;

/// Generates a set of random G1 and G2 points from a random seed and outputs the vectors of P and Q points as well as
/// the corresponding P and Q EcPoint structs.
#[allow(non_snake_case)]
#[allow(clippy::type_complexity)]
pub fn generate_test_points<A1, A2, Fp, Fp2>(
    rand_seeds: &[u64],
) -> (
    Vec<A1>,
    Vec<A2>,
    Vec<AffinePoint<Fp>>,
    Vec<AffinePoint<Fp2>>,
)
where
    A1: AffineCoords<Fp>,
    A2: AffineCoords<Fp2>,
    Fp: Field,
    Fp2: FieldExtension<Fp>,
{
    let (P_vec, Q_vec) = rand_seeds
        .iter()
        .map(|seed| {
            let mut rng0 = StdRng::seed_from_u64(*seed);
            let p = A1::random(&mut rng0);
            let mut rng1 = StdRng::seed_from_u64(*seed * 2);
            let q = A2::random(&mut rng1);
            (p, q)
        })
        .unzip::<_, _, Vec<_>, Vec<_>>();
    let (P_ecpoints, Q_ecpoints) = izip!(P_vec.clone(), Q_vec.clone())
        .map(|(P, Q)| {
            (
                AffinePoint { x: P.x(), y: P.y() },
                AffinePoint { x: Q.x(), y: Q.y() },
            )
        })
        .unzip::<_, _, Vec<_>, Vec<_>>();
    (P_vec, Q_vec, P_ecpoints, Q_ecpoints)
}
