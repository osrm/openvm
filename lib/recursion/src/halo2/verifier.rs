use afs_compiler::ir::{DslIr, TracedVec, Witness};
use afs_stark_backend::prover::types::Proof;
use ax_sdk::config::{baby_bear_poseidon2_outer::BabyBearPoseidon2OuterConfig, FriParameters};
use itertools::Itertools;
use snark_verifier_sdk::{
    evm::{gen_evm_proof_shplonk, gen_evm_verifier_shplonk},
    halo2::{
        aggregation::{AggregationCircuit, AggregationConfigParams, VerifierUniversality},
        gen_snark_shplonk,
    },
    snark_verifier::halo2_base::{
        gates::circuit::{
            CircuitBuilderStage,
            CircuitBuilderStage::{Keygen, Prover},
        },
        halo2_proofs::{halo2curves::bn256::Fr, plonk::keygen_pk2},
    },
    CircuitExt, Snark, SHPLONK,
};

use crate::{
    config::outer::OuterConfig,
    halo2::{
        utils::{read_params, KZG_PARAMS_FOR_SVK},
        Halo2Prover, Halo2ProvingPinning,
    },
    stark::outer::build_circuit_verify_operations,
    types::MultiStarkVerificationAdvice,
    witness::Witnessable,
};

#[derive(Debug, Clone)]
pub struct Halo2VerifierCircuit {
    pub pinning: Halo2ProvingPinning,
    pub dsl_ops: TracedVec<DslIr<OuterConfig>>,
}

/// Generate a Halo2 verifier circuit for a given stark.
pub fn generate_halo2_verifier_circuit(
    halo2_k: usize,
    advice: MultiStarkVerificationAdvice<OuterConfig>,
    fri_params: &FriParameters,
    proof: &Proof<BabyBearPoseidon2OuterConfig>,
) -> Halo2VerifierCircuit {
    let mut witness = Witness::default();
    proof.write(&mut witness);
    let operations = build_circuit_verify_operations(advice, fri_params, proof);
    Halo2VerifierCircuit {
        pinning: Halo2Prover::keygen(halo2_k, operations.clone(), witness),
        dsl_ops: operations,
    }
}

impl Halo2VerifierCircuit {
    pub fn prove(&self, input: Proof<BabyBearPoseidon2OuterConfig>) -> Snark {
        let mut witness = Witness::default();
        input.write(&mut witness);
        Halo2Prover::prove(
            self.pinning.config_params.clone(),
            self.pinning.break_points.clone(),
            &self.pinning.pk,
            self.dsl_ops.clone(),
            witness,
        )
    }

    pub fn keygen_wrapper_circuit(&self, k: usize, snark: Snark) -> Halo2ProvingPinning {
        let mut circuit = generate_wrapper_circuit(Keygen, k, snark);
        circuit.calculate_params(Some(20));
        let params = read_params(k as u32);
        let config_params = circuit.builder.config_params.clone();
        // Wrapper circuit should only have 1 column.
        tracing::info!(
            "Wrapper circuit num advice: {:?}",
            config_params.num_advice_per_phase
        );
        let pk = keygen_pk2(params.as_ref(), &circuit, false).unwrap();
        let num_pvs = circuit.instances().iter().map(|x| x.len()).collect_vec();
        Halo2ProvingPinning {
            pk,
            config_params,
            break_points: circuit.break_points(),
            num_pvs,
        }
    }
}

fn generate_wrapper_circuit(
    stage: CircuitBuilderStage,
    k: usize,
    snark: Snark,
) -> AggregationCircuit {
    let config_params = AggregationConfigParams {
        degree: k as u32,
        lookup_bits: k - 1,
        ..Default::default()
    };
    let mut circuit = AggregationCircuit::new::<SHPLONK>(
        stage,
        config_params,
        &KZG_PARAMS_FOR_SVK,
        [snark],
        VerifierUniversality::None,
    );
    circuit.expose_previous_instances(false);
    circuit
}

/// Return (EVM proof, public values)
pub fn gen_wrapper_circuit_evm_proof(
    pinning: &Halo2ProvingPinning,
    snark: Snark,
) -> (Vec<u8>, Vec<Vec<Fr>>) {
    let k = pinning.config_params.k;
    let params = read_params(k as u32);
    let prover_circuit = generate_wrapper_circuit(Prover, k, snark)
        .use_params(pinning.config_params.clone().try_into().unwrap())
        .use_break_points(pinning.break_points.clone());
    let pvs = prover_circuit.instances();
    #[cfg(feature = "bench-metrics")]
    let start = std::time::Instant::now();
    #[cfg(feature = "bench-metrics")]
    {
        let stats = prover_circuit.builder.statistics();
        let total_advices: usize = stats.gate.total_advice_per_phase.into_iter().sum();
        let total_lookups: usize = stats.total_lookup_advice_per_phase.into_iter().sum();
        let total_cell = total_advices + total_lookups + stats.gate.total_fixed;
        metrics::gauge!("halo2_total_cells").set(total_cell as f64);
    }

    let proof = gen_evm_proof_shplonk(&params, &pinning.pk, prover_circuit, pvs.clone());

    #[cfg(feature = "bench-metrics")]
    metrics::gauge!("evm_proof_time_ms").set(start.elapsed().as_millis() as f64);

    (proof, pvs)
}

pub fn gen_wrapper_circuit_snark(
    pinning: &Halo2ProvingPinning,
    snark: Snark,
) -> (Snark, Vec<Vec<Fr>>) {
    let k = pinning.config_params.k;
    let params = read_params(k as u32);
    let prover_circuit = generate_wrapper_circuit(Prover, k, snark)
        .use_params(pinning.config_params.clone().try_into().unwrap())
        .use_break_points(pinning.break_points.clone());
    let pvs = prover_circuit.instances();
    (
        gen_snark_shplonk(&params, &pinning.pk, prover_circuit, None::<String>),
        pvs,
    )
}

pub fn gen_wrapper_circuit_evm_verifier(pinning: &Halo2ProvingPinning) -> Vec<u8> {
    let params = read_params(pinning.config_params.k as u32);
    gen_evm_verifier_shplonk::<AggregationCircuit>(
        &params,
        pinning.pk.get_vk(),
        pinning.num_pvs.clone(),
        None,
    )
}