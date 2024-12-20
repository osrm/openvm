use std::{
    borrow::{Borrow, BorrowMut},
    cell::RefCell,
    marker::PhantomData,
};

use openvm_circuit::{
    arch::{
        AdapterAirContext, AdapterRuntimeContext, BasicAdapterInterface, ExecutionBridge,
        ExecutionBus, ExecutionState, MinimalInstruction, Result, VmAdapterAir, VmAdapterChip,
        VmAdapterInterface,
    },
    system::{
        memory::{
            offline_checker::{MemoryBridge, MemoryReadAuxCols, MemoryWriteAuxCols},
            MemoryAddress, MemoryAuxColsFactory, MemoryController, MemoryControllerRef,
            MemoryReadRecord, MemoryWriteRecord,
        },
        program::ProgramBus,
    },
};
use openvm_circuit_primitives_derive::AlignedBorrow;
use openvm_instructions::{instruction::Instruction, program::DEFAULT_PC_STEP};
use openvm_stark_backend::{
    interaction::InteractionBuilder,
    p3_air::BaseAir,
    p3_field::{AbstractField, Field, PrimeField32},
};

#[derive(Debug)]
pub struct VectorReadRecord<F: Field, const NUM_READS: usize, const READ_SIZE: usize> {
    pub reads: [MemoryReadRecord<F, READ_SIZE>; NUM_READS],
}

#[derive(Debug)]
pub struct VectorWriteRecord<F: Field, const WRITE_SIZE: usize> {
    pub from_state: ExecutionState<u32>,
    pub writes: [MemoryWriteRecord<F, WRITE_SIZE>; 1],
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct ConvertAdapterChip<F: Field, const READ_SIZE: usize, const WRITE_SIZE: usize> {
    pub air: ConvertAdapterAir<READ_SIZE, WRITE_SIZE>,
    _marker: PhantomData<F>,
}

impl<F: PrimeField32, const READ_SIZE: usize, const WRITE_SIZE: usize>
    ConvertAdapterChip<F, READ_SIZE, WRITE_SIZE>
{
    pub fn new(
        execution_bus: ExecutionBus,
        program_bus: ProgramBus,
        memory_controller: MemoryControllerRef<F>,
    ) -> Self {
        let memory_controller = RefCell::borrow(&memory_controller);
        let memory_bridge = memory_controller.memory_bridge();
        Self {
            air: ConvertAdapterAir {
                execution_bridge: ExecutionBridge::new(execution_bus, program_bus),
                memory_bridge,
            },
            _marker: PhantomData,
        }
    }
}

#[repr(C)]
#[derive(AlignedBorrow)]
pub struct ConvertAdapterCols<T, const READ_SIZE: usize, const WRITE_SIZE: usize> {
    pub from_state: ExecutionState<T>,
    pub a_pointer: T,
    pub b_pointer: T,
    pub a_as: T,
    pub b_as: T,
    pub writes_aux: [MemoryWriteAuxCols<T, WRITE_SIZE>; 1],
    pub reads_aux: [MemoryReadAuxCols<T, READ_SIZE>; 1],
}

#[derive(Clone, Copy, Debug, derive_new::new)]
pub struct ConvertAdapterAir<const READ_SIZE: usize, const WRITE_SIZE: usize> {
    pub(super) execution_bridge: ExecutionBridge,
    pub(super) memory_bridge: MemoryBridge,
}

impl<F: Field, const READ_SIZE: usize, const WRITE_SIZE: usize> BaseAir<F>
    for ConvertAdapterAir<READ_SIZE, WRITE_SIZE>
{
    fn width(&self) -> usize {
        ConvertAdapterCols::<F, READ_SIZE, WRITE_SIZE>::width()
    }
}

impl<AB: InteractionBuilder, const READ_SIZE: usize, const WRITE_SIZE: usize> VmAdapterAir<AB>
    for ConvertAdapterAir<READ_SIZE, WRITE_SIZE>
{
    type Interface =
        BasicAdapterInterface<AB::Expr, MinimalInstruction<AB::Expr>, 1, 1, READ_SIZE, WRITE_SIZE>;

    fn eval(
        &self,
        builder: &mut AB,
        local: &[AB::Var],
        ctx: AdapterAirContext<AB::Expr, Self::Interface>,
    ) {
        let cols: &ConvertAdapterCols<_, READ_SIZE, WRITE_SIZE> = local.borrow();
        let timestamp = cols.from_state.timestamp;
        let mut timestamp_delta = 0usize;
        let mut timestamp_pp = || {
            timestamp_delta += 1;
            timestamp + AB::F::from_canonical_usize(timestamp_delta - 1)
        };

        self.memory_bridge
            .read(
                MemoryAddress::new(cols.b_as, cols.b_pointer),
                ctx.reads[0].clone(),
                timestamp_pp(),
                &cols.reads_aux[0],
            )
            .eval(builder, ctx.instruction.is_valid.clone());

        self.memory_bridge
            .write(
                MemoryAddress::new(cols.a_as, cols.a_pointer),
                ctx.writes[0].clone(),
                timestamp_pp(),
                &cols.writes_aux[0],
            )
            .eval(builder, ctx.instruction.is_valid.clone());

        self.execution_bridge
            .execute_and_increment_or_set_pc(
                ctx.instruction.opcode,
                [
                    cols.a_pointer.into(),
                    cols.b_pointer.into(),
                    AB::Expr::ZERO,
                    cols.a_as.into(),
                    cols.b_as.into(),
                ],
                cols.from_state,
                AB::F::from_canonical_usize(timestamp_delta),
                (DEFAULT_PC_STEP, ctx.to_pc),
            )
            .eval(builder, ctx.instruction.is_valid);
    }

    fn get_from_pc(&self, local: &[AB::Var]) -> AB::Var {
        let cols: &ConvertAdapterCols<_, READ_SIZE, WRITE_SIZE> = local.borrow();
        cols.from_state.pc
    }
}

impl<F: PrimeField32, const READ_SIZE: usize, const WRITE_SIZE: usize> VmAdapterChip<F>
    for ConvertAdapterChip<F, READ_SIZE, WRITE_SIZE>
{
    type ReadRecord = VectorReadRecord<F, 1, READ_SIZE>;
    type WriteRecord = VectorWriteRecord<F, WRITE_SIZE>;
    type Air = ConvertAdapterAir<READ_SIZE, WRITE_SIZE>;
    type Interface = BasicAdapterInterface<F, MinimalInstruction<F>, 1, 1, READ_SIZE, WRITE_SIZE>;

    fn preprocess(
        &mut self,
        memory: &mut MemoryController<F>,
        instruction: &Instruction<F>,
    ) -> Result<(
        <Self::Interface as VmAdapterInterface<F>>::Reads,
        Self::ReadRecord,
    )> {
        let Instruction { b, e, .. } = *instruction;

        let y_val = memory.read::<READ_SIZE>(e, b);

        Ok(([y_val.data], Self::ReadRecord { reads: [y_val] }))
    }

    fn postprocess(
        &mut self,
        memory: &mut MemoryController<F>,
        instruction: &Instruction<F>,
        from_state: ExecutionState<u32>,
        output: AdapterRuntimeContext<F, Self::Interface>,
        _read_record: &Self::ReadRecord,
    ) -> Result<(ExecutionState<u32>, Self::WriteRecord)> {
        let Instruction { a, d, .. } = *instruction;
        let a_val = memory.write::<WRITE_SIZE>(d, a, output.writes[0]);

        Ok((
            ExecutionState {
                pc: output.to_pc.unwrap_or(from_state.pc + DEFAULT_PC_STEP),
                timestamp: memory.timestamp(),
            },
            Self::WriteRecord {
                from_state,
                writes: [a_val],
            },
        ))
    }

    fn generate_trace_row(
        &self,
        row_slice: &mut [F],
        read_record: Self::ReadRecord,
        write_record: Self::WriteRecord,
        aux_cols_factory: &MemoryAuxColsFactory<F>,
    ) {
        let row_slice: &mut ConvertAdapterCols<_, READ_SIZE, WRITE_SIZE> = row_slice.borrow_mut();

        row_slice.from_state = write_record.from_state.map(F::from_canonical_u32);
        row_slice.a_pointer = write_record.writes[0].pointer;
        row_slice.a_as = write_record.writes[0].address_space;
        row_slice.b_pointer = read_record.reads[0].pointer;
        row_slice.b_as = read_record.reads[0].address_space;

        row_slice.reads_aux = [aux_cols_factory.make_read_aux_cols(read_record.reads[0])];
        row_slice.writes_aux = [aux_cols_factory.make_write_aux_cols(write_record.writes[0])];
    }

    fn air(&self) -> &Self::Air {
        &self.air
    }
}
