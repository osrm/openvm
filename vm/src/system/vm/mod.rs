use std::{collections::VecDeque, marker::PhantomData, mem};

use afs_stark_backend::{
    config::{Domain, StarkGenericConfig},
    p3_commit::PolynomialSpace,
    prover::types::ProofInput,
};
use metrics::VmMetrics;
use p3_field::PrimeField32;
pub use segment::ExecutionSegment;

use crate::{
    arch::AxVmChip,
    intrinsics::hashes::poseidon2::CHUNK,
    kernels::core::Streams,
    system::{
        memory::Equipartition,
        program::{ExecutionError, Program},
        vm::config::{PersistenceType, VmConfig},
    },
};

pub mod chip_set;
pub mod config;
pub mod connector;
pub mod cycle_tracker;
/// Instrumentation metrics for performance analysis and debugging
pub mod metrics;
#[macro_use]
pub mod segment;

/// Parent struct that holds all execution segments, program, config.
pub struct VirtualMachine<F: PrimeField32> {
    pub config: VmConfig,
    input_stream: VecDeque<Vec<F>>,
    initial_memory: Option<Equipartition<F, CHUNK>>,
}

pub struct VirtualMachineResult<SC: StarkGenericConfig> {
    pub per_segment: Vec<ProofInput<SC>>,
}

impl<F: PrimeField32> VirtualMachine<F> {
    /// Create a new VM with a given config, program, and input stream.
    ///
    /// The VM will start with a single segment, which is created from the initial state of the Core.
    pub fn new(config: VmConfig) -> Self {
        Self {
            config,
            input_stream: VecDeque::new(),
            initial_memory: None,
        }
    }

    pub fn with_input_stream(mut self, input_stream: Vec<Vec<F>>) -> Self {
        self.input_stream = VecDeque::from(input_stream);
        self
    }

    pub fn with_initial_memory(mut self, memory: Equipartition<F, CHUNK>) -> Self {
        self.initial_memory = Some(memory);
        self
    }

    fn execute_segments(
        &mut self,
        program: Program<F>,
    ) -> Result<Vec<ExecutionSegment<F>>, ExecutionError> {
        let mut segments = vec![];
        let mut segment = ExecutionSegment::new(
            self.config.clone(),
            program.clone(),
            Streams {
                input_stream: mem::take(&mut self.input_stream),
                hint_stream: VecDeque::new(),
            },
            self.initial_memory.take(),
        );
        let mut pc = program.pc_start;

        loop {
            pc = segment.execute_from_pc(pc)?;
            if segment.did_terminate() {
                break;
            }

            assert_eq!(
                pc,
                segment.chip_set.connector_chip.boundary_states[1]
                    .unwrap()
                    .pc
            );

            let config = mem::take(&mut segment.config);
            let cycle_tracker = mem::take(&mut segment.cycle_tracker);
            let streams = mem::take(&mut segment.streams);
            let final_memory = mem::take(&mut segment.final_memory)
                .expect("final memory should be set in continuations segment");

            segments.push(segment);

            segment = ExecutionSegment::new(config, program.clone(), streams, Some(final_memory));
            segment.cycle_tracker = cycle_tracker;
        }
        segments.push(segment);
        tracing::debug!("Number of continuation segments: {}", segments.len());

        Ok(segments)
    }

    pub fn execute(mut self, program: Program<F>) -> Result<(), ExecutionError> {
        self.execute_segments(program).map(|_| ())
    }

    pub fn execute_and_generate<SC: StarkGenericConfig>(
        mut self,
        program: Program<F>,
    ) -> Result<VirtualMachineResult<SC>, ExecutionError>
    where
        Domain<SC>: PolynomialSpace<Val = F>,
    {
        let segments = self.execute_segments(program)?;

        Ok(VirtualMachineResult {
            per_segment: segments
                .into_iter()
                .map(ExecutionSegment::generate_proof_input)
                .collect(),
        })
    }
}

/// A single segment VM.
pub struct SingleSegmentVM<F: PrimeField32> {
    pub config: VmConfig,
    _marker: PhantomData<F>,
}

impl<F: PrimeField32> SingleSegmentVM<F> {
    pub fn new(config: VmConfig) -> Self {
        assert_eq!(
            config.memory_config.persistence_type,
            PersistenceType::Volatile,
            "Single segment VM only supports volatile memory"
        );
        Self {
            config,
            _marker: Default::default(),
        }
    }
    /// Executes a program and returns the public values. None means the public value is not set.
    pub fn execute(
        &self,
        program: Program<F>,
        input: Vec<Vec<F>>,
    ) -> Result<Vec<Option<F>>, ExecutionError> {
        let segment = self.execute_impl(program, input.into())?;
        let pv_chip = find_chip!(segment.chip_set, AxVmChip::PublicValues);
        let borrowed_pv_chip = pv_chip.borrow();
        let pvs = borrowed_pv_chip.core.get_custom_public_values();
        Ok(pvs)
    }
    /// Executes a program and returns its proof input.
    pub fn execute_and_generate<SC: StarkGenericConfig>(
        &self,
        program: Program<F>,
        input: Vec<Vec<F>>,
    ) -> Result<ProofInput<SC>, ExecutionError>
    where
        Domain<SC>: PolynomialSpace<Val = F>,
    {
        let segment = self.execute_impl(program, input.into())?;
        Ok(segment.generate_proof_input())
    }

    fn execute_impl(
        &self,
        program: Program<F>,
        input: VecDeque<Vec<F>>,
    ) -> Result<ExecutionSegment<F>, ExecutionError> {
        let pc_start = program.pc_start;
        let mut segment = ExecutionSegment::new(
            self.config.clone(),
            program,
            Streams {
                input_stream: input,
                hint_stream: VecDeque::new(),
            },
            None,
        );
        segment.execute_from_pc(pc_start)?;
        Ok(segment)
    }
}
