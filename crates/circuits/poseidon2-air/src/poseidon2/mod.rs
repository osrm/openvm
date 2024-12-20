pub mod air;
pub mod bridge;
pub mod columns;
pub mod trace;

#[cfg(test)]
pub mod tests;

use lazy_static::lazy_static;
use openvm_stark_backend::p3_field::{AbstractField, PrimeField32};
use openvm_stark_sdk::p3_baby_bear::{BabyBear, BabyBearInternalLayerParameters};
use p3_monty_31::InternalLayerBaseParameters;

pub use self::{air::Poseidon2Air, columns::Poseidon2Cols};

#[derive(Clone)]
pub struct Poseidon2Config<const WIDTH: usize, F: Clone> {
    pub external_constants: Vec<[F; WIDTH]>,
    pub internal_constants: Vec<F>,
    pub ext_mds_matrix: [[u32; 4]; 4],
    pub int_diag_m1_matrix: [F; WIDTH],
    pub reduction_factor: F,
}

impl<const WIDTH: usize, F: Clone> Poseidon2Config<WIDTH, F> {
    pub fn rounds_f(&self) -> usize {
        self.external_constants.len()
    }
    pub fn rounds_p(&self) -> usize {
        self.internal_constants.len()
    }
}
/// MDSMat4 from Plonky3
/// [ 2 3 1 1 ]
/// [ 1 2 3 1 ]
/// [ 1 1 2 3 ]
/// [ 3 1 1 2 ].
/// <https://github.com/plonky3/Plonky3/blob/64370174ba932beee44307c3003e1d787c101bb6/poseidon2/src/external.rs#L43>
pub const MDS_MAT_4: [[u32; 4]; 4] = [[2, 3, 1, 1], [1, 2, 3, 1], [1, 1, 2, 3], [3, 1, 1, 2]];
// Multiply a 4-element vector x by
// [ 5 7 1 3 ]
// [ 4 6 1 1 ]
// [ 1 3 5 7 ]
// [ 1 1 4 6 ].
// This uses the formula from the start of Appendix B in the Poseidon2 paper, with multiplications unrolled into additions.
// It is also the matrix used by the Horizon Labs implementation.
pub const HL_MDS_MAT_4: [[u32; 4]; 4] = [[5, 7, 1, 3], [4, 6, 1, 1], [1, 3, 5, 7], [1, 1, 4, 6]];

impl Poseidon2Config<16, BabyBear> {
    pub fn horizen_config() -> Self {
        Self {
            external_constants: HL_BABYBEAR_EXT_CONST_16.to_vec(),
            internal_constants: HL_BABYBEAR_INT_CONST_16.to_vec(),
            ext_mds_matrix: HL_MDS_MAT_4,
            int_diag_m1_matrix: *HL_BABYBEAR_INT_DIAG_16,
            reduction_factor: BabyBear::ONE,
        }
    }
}

impl<F: PrimeField32> Poseidon2Config<16, F> {
    /// Using HorizenLab's round constants: https://github.com/HorizenLabs/poseidon2
    pub fn new_hl_baby_bear_16() -> Self {
        let external_round_constants_f: Vec<[F; 16]> = HL_BABYBEAR_EXT_CONST_16
            .iter()
            .map(|round| {
                round
                    .iter()
                    .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()))
                    .collect::<Vec<F>>()
                    .try_into()
                    .unwrap()
            })
            .collect();

        let internal_round_constants_f: Vec<F> = HL_BABYBEAR_INT_CONST_16
            .iter()
            .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()))
            .collect();

        let horizen_int_diag_f: [F; 16] = HL_BABYBEAR_INT_DIAG_16
            .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()));

        Self {
            external_constants: external_round_constants_f,
            internal_constants: internal_round_constants_f,
            ext_mds_matrix: HL_MDS_MAT_4,
            int_diag_m1_matrix: horizen_int_diag_f,
            reduction_factor: F::ONE,
        }
    }

    pub fn new_p3_baby_bear_16() -> Self {
        let external_round_constants_f: Vec<[F; 16]> = HL_BABYBEAR_EXT_CONST_16
            .iter()
            .map(|round| {
                round
                    .iter()
                    .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()))
                    .collect::<Vec<F>>()
                    .try_into()
                    .unwrap()
            })
            .collect();

        let internal_round_constants_f: Vec<F> = HL_BABYBEAR_INT_CONST_16
            .iter()
            .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()))
            .collect();

        let p3_int_diag_f = BabyBearInternalLayerParameters::INTERNAL_DIAG_MONTY
            .map(|babybear| F::from_canonical_u32(babybear.as_canonical_u32()));

        Self {
            external_constants: external_round_constants_f,
            internal_constants: internal_round_constants_f,
            ext_mds_matrix: MDS_MAT_4,
            int_diag_m1_matrix: p3_int_diag_f,
            reduction_factor: F::ONE,
        }
    }
}

impl Default for Poseidon2Config<16, BabyBear> {
    fn default() -> Self {
        Self::new_p3_baby_bear_16()
    }
}

lazy_static! {
    pub static ref HL_BABYBEAR_EXT_CONST_16: Vec<[BabyBear; 16]> =
        Poseidon2Air::<16, BabyBear>::horizen_round_consts_16().0;
    pub static ref HL_BABYBEAR_INT_CONST_16: Vec<BabyBear> =
        Poseidon2Air::<16, BabyBear>::horizen_round_consts_16().1;
    pub static ref HL_BABYBEAR_INT_DIAG_16: [BabyBear; 16] =
        Poseidon2Air::<16, BabyBear>::horizen_round_consts_16().2;
}
