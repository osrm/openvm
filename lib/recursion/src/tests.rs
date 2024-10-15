use std::{panic::catch_unwind, sync::Arc};

use afs_primitives::{
    sum::SumChip,
    var_range::{bus::VariableRangeCheckerBus, VariableRangeCheckerChip},
};
use afs_stark_backend::{prover::types::AirProofInput, utils::disable_debug_builder, Chip};
use ax_sdk::{
    config::{
        baby_bear_poseidon2::{BabyBearPoseidon2Config, BabyBearPoseidon2Engine},
        fri_params::standard_fri_params_with_100_bits_conjectured_security,
        setup_tracing, FriParameters,
    },
    dummy_airs::{
        fib_air::chip::FibonacciChip,
        interaction::dummy_interaction_air::{
            DummyInteractionAir, DummyInteractionChip, DummyInteractionData,
        },
    },
    engine::{ProofInputForTest, StarkFriEngine},
    utils::to_field_vec,
};
use p3_field::PrimeField32;
use p3_matrix::dense::RowMajorMatrix;
use p3_uni_stark::{StarkGenericConfig, Val};
use stark_vm::{sdk::gen_vm_program_test_proof_input, system::vm::config::VmConfig};

use crate::{
    hints::Hintable, stark::VerifierProgram, testing_utils::inner::run_recursive_test,
    types::new_from_inner_multi_vk,
};

pub fn fibonacci_test_proof_input<SC: StarkGenericConfig>(n: usize) -> ProofInputForTest<SC>
where
    Val<SC>: PrimeField32,
{
    setup_tracing();

    let fib_chip = FibonacciChip::new(0, 1, n);
    ProofInputForTest {
        per_air: vec![fib_chip.generate_air_proof_input()],
    }
}

pub fn interaction_test_proof_input<SC: StarkGenericConfig>() -> ProofInputForTest<SC>
where
    Val<SC>: PrimeField32,
{
    const INPUT_BUS: usize = 0;
    const OUTPUT_BUS: usize = 1;
    const RANGE_BUS: usize = 2;
    const RANGE_MAX_BITS: usize = 4;

    let range_bus = VariableRangeCheckerBus::new(RANGE_BUS, RANGE_MAX_BITS);
    let range_checker = Arc::new(VariableRangeCheckerChip::new(range_bus));
    let sum_chip = SumChip::new(INPUT_BUS, OUTPUT_BUS, 4, range_checker);

    let mut sum_trace_u32 = Vec::<(u32, u32, u32, u32)>::new();
    let n = 16;
    for i in 0..n {
        sum_trace_u32.push((0, 1, i + 1, (i == n - 1) as u32));
    }

    let kv: &[(u32, u32)] = &sum_trace_u32
        .iter()
        .map(|&(key, value, _, _)| (key, value))
        .collect::<Vec<_>>();
    let sum_trace = sum_chip.generate_trace(kv);
    let sender_air = DummyInteractionAir::new(2, true, INPUT_BUS);
    let sender_trace = RowMajorMatrix::new(
        to_field_vec(
            sum_trace_u32
                .iter()
                .flat_map(|&(key, val, _, _)| [1, key, val])
                .collect(),
        ),
        sender_air.field_width() + 1,
    );
    let receiver_air = DummyInteractionAir::new(2, false, OUTPUT_BUS);
    let receiver_trace = RowMajorMatrix::new(
        to_field_vec(
            sum_trace_u32
                .iter()
                .flat_map(|&(key, _, sum, is_final)| [is_final, key, sum])
                .collect(),
        ),
        receiver_air.field_width() + 1,
    );
    let range_checker_trace = sum_chip.range_checker.generate_trace();
    let sum_air = Arc::new(sum_chip.air);
    let sender_air = Arc::new(sender_air);
    let receiver_air = Arc::new(receiver_air);
    let range_checker_air = Arc::new(sum_chip.range_checker.air);

    let range_checker_air_proof_input =
        AirProofInput::simple_no_pis(range_checker_air, range_checker_trace);
    let sum_air_proof_input = AirProofInput::simple_no_pis(sum_air, sum_trace);
    let sender_air_proof_input = AirProofInput::simple_no_pis(sender_air, sender_trace);
    let receiver_air_proof_input = AirProofInput::simple_no_pis(receiver_air, receiver_trace);

    ProofInputForTest {
        per_air: vec![
            range_checker_air_proof_input,
            sum_air_proof_input,
            sender_air_proof_input,
            receiver_air_proof_input,
        ],
    }
}

pub fn unordered_test_proof_input<SC: StarkGenericConfig>() -> ProofInputForTest<SC>
where
    Val<SC>: PrimeField32,
{
    const BUS: usize = 0;
    const SENDER_HEIGHT: usize = 2;
    const RECEIVER_HEIGHT: usize = 4;
    let sender_air = DummyInteractionAir::new(1, true, BUS);
    let sender_trace = RowMajorMatrix::new(
        to_field_vec([[2, 1]; SENDER_HEIGHT].into_iter().flatten().collect()),
        sender_air.field_width() + 1,
    );
    let receiver_air = DummyInteractionAir::new(1, false, BUS);
    let receiver_trace = RowMajorMatrix::new(
        to_field_vec([[1, 1]; RECEIVER_HEIGHT].into_iter().flatten().collect()),
        receiver_air.field_width() + 1,
    );

    let sender_air_proof_input = AirProofInput::simple_no_pis(Arc::new(sender_air), sender_trace);
    let receiver_air_proof_input =
        AirProofInput::simple_no_pis(Arc::new(receiver_air), receiver_trace);

    ProofInputForTest {
        per_air: vec![sender_air_proof_input, receiver_air_proof_input],
    }
}

#[test]
fn test_fibonacci_small() {
    setup_tracing();

    run_recursive_test(
        fibonacci_test_proof_input::<BabyBearPoseidon2Config>(1 << 5),
        standard_fri_params_with_100_bits_conjectured_security(3),
    )
}

#[test]
fn test_fibonacci() {
    setup_tracing();

    // test lde = 27
    run_recursive_test(
        fibonacci_test_proof_input::<BabyBearPoseidon2Config>(1 << 24),
        FriParameters {
            log_blowup: 3,
            num_queries: 2,
            proof_of_work_bits: 0,
        },
    )
}

#[test]
fn test_interactions() {
    setup_tracing();

    run_recursive_test(
        interaction_test_proof_input::<BabyBearPoseidon2Config>(),
        standard_fri_params_with_100_bits_conjectured_security(3),
    )
}

#[test]
fn test_unordered() {
    setup_tracing();

    run_recursive_test(
        unordered_test_proof_input::<BabyBearPoseidon2Config>(),
        standard_fri_params_with_100_bits_conjectured_security(3),
    )
}

#[test]
fn test_optional_air() {
    use afs_stark_backend::{engine::StarkEngine, prover::types::ProofInput, Chip};
    setup_tracing();

    let fri_params = standard_fri_params_with_100_bits_conjectured_security(3);
    let engine = BabyBearPoseidon2Engine::new(fri_params);
    let fib_chip = FibonacciChip::new(0, 1, 8);
    let mut send_chip1 = DummyInteractionChip::new_without_partition(1, true, 0);
    let mut send_chip2 =
        DummyInteractionChip::new_with_partition(engine.config().pcs(), 1, true, 0);
    let mut recv_chip1 = DummyInteractionChip::new_without_partition(1, false, 0);
    let mut keygen_builder = engine.keygen_builder();
    let fib_chip_id = keygen_builder.add_air(fib_chip.air());
    let send_chip1_id = keygen_builder.add_air(send_chip1.air());
    let send_chip2_id = keygen_builder.add_air(send_chip2.air());
    let recv_chip1_id = keygen_builder.add_air(recv_chip1.air());
    let pk = keygen_builder.generate_pk();
    let prover = engine.prover();
    let verifier = engine.verifier();

    let m_advice = new_from_inner_multi_vk(&pk.get_vk());
    let vm_config = VmConfig::aggregation(7);
    let program = VerifierProgram::build(m_advice, &fri_params);

    // Case 1: All AIRs are present.
    {
        let mut challenger = engine.new_challenger();
        send_chip1.load_data(DummyInteractionData {
            count: vec![1, 2, 4],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        send_chip2.load_data(DummyInteractionData {
            count: vec![1, 2, 8],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        recv_chip1.load_data(DummyInteractionData {
            count: vec![2, 4, 12],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        let proof = prover.prove(
            &mut challenger,
            &pk,
            ProofInput {
                per_air: vec![
                    fib_chip.generate_air_proof_input_with_id(fib_chip_id),
                    send_chip1.generate_air_proof_input_with_id(send_chip1_id),
                    send_chip2.generate_air_proof_input_with_id(send_chip2_id),
                    recv_chip1.generate_air_proof_input_with_id(recv_chip1_id),
                ],
            },
        );
        let mut challenger = engine.new_challenger();
        verifier
            .verify(&mut challenger, &pk.get_vk(), &proof)
            .expect("Verification failed");
        // The VM program will panic when the program cannot verify the proof.
        gen_vm_program_test_proof_input::<BabyBearPoseidon2Config>(
            program.clone(),
            proof.write(),
            vm_config.clone(),
        );
    }
    // Case 2: The second AIR is not presented.
    {
        let mut challenger = engine.new_challenger();
        send_chip1.load_data(DummyInteractionData {
            count: vec![1, 2, 4],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        recv_chip1.load_data(DummyInteractionData {
            count: vec![1, 2, 4],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        let proof = prover.prove(
            &mut challenger,
            &pk,
            ProofInput {
                per_air: vec![
                    send_chip1.generate_air_proof_input_with_id(send_chip1_id),
                    recv_chip1.generate_air_proof_input_with_id(recv_chip1_id),
                ],
            },
        );
        let mut challenger = engine.new_challenger();
        verifier
            .verify(&mut challenger, &pk.get_vk(), &proof)
            .expect("Verification failed");
        // The VM program will panic when the program cannot verify the proof.
        gen_vm_program_test_proof_input::<BabyBearPoseidon2Config>(
            program.clone(),
            proof.write(),
            vm_config.clone(),
        );
    }
    // Case 3: Negative - unbalanced interactions.
    {
        disable_debug_builder();
        let mut challenger = engine.new_challenger();
        recv_chip1.load_data(DummyInteractionData {
            count: vec![1, 2, 4],
            fields: vec![vec![1], vec![2], vec![3]],
        });
        let proof = prover.prove(
            &mut challenger,
            &pk,
            ProofInput {
                per_air: vec![recv_chip1.generate_air_proof_input_with_id(recv_chip1_id)],
            },
        );
        let mut challenger = engine.new_challenger();
        assert!(verifier
            .verify(&mut challenger, &pk.get_vk(), &proof)
            .is_err());
        // The VM program should panic when the proof cannot be verified.
        let unwind_res = catch_unwind(|| {
            gen_vm_program_test_proof_input::<BabyBearPoseidon2Config>(
                program.clone(),
                proof.write(),
                vm_config.clone(),
            )
        });
        assert!(unwind_res.is_err());
    }
}