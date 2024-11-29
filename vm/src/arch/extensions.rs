use std::{any::Any, cell::RefCell, iter::once, rc::Rc, sync::Arc};

use ax_circuit_derive::{Chip, ChipUsageGetter};
use ax_circuit_primitives::var_range::{VariableRangeCheckerBus, VariableRangeCheckerChip};
use ax_poseidon2_air::poseidon2::air::SBOX_DEGREE;
use ax_stark_backend::{
    config::{Domain, StarkGenericConfig},
    p3_commit::PolynomialSpace,
    prover::types::{AirProofInput, CommittedTraceData, ProofInput},
    rap::AnyRap,
    Chip, ChipUsageGetter,
};
use axvm_circuit_derive::{AnyEnum, InstructionExecutor};
use axvm_instructions::{
    program::Program, PhantomDiscriminant, Poseidon2Opcode, PublishOpcode, SystemOpcode,
    UsizeOpcode,
};
use derive_more::derive::From;
use getset::Getters;
use p3_field::{AbstractField, PrimeField32};
use p3_matrix::Matrix;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use super::{
    vm_poseidon2_config, ExecutionBus, InstructionExecutor, PhantomSubExecutor, Streams,
    SystemConfig,
};
use crate::{
    intrinsics::hashes::poseidon2::Poseidon2Chip,
    kernels::{
        adapters::native_adapter::NativeAdapterChip,
        public_values::{core::PublicValuesCoreChip, PublicValuesChip},
    },
    system::{
        connector::VmConnectorChip,
        memory::{
            merkle::{DirectCompressionBus, MemoryMerkleBus},
            offline_checker::MemoryBus,
            Equipartition, MemoryController, MemoryControllerRef, BOUNDARY_AIR_OFFSET, CHUNK,
            MERKLE_AIR_OFFSET,
        },
        phantom::PhantomChip,
        program::{ProgramBus, ProgramChip},
    },
};

/// Global AIR ID in the VM circuit verifying key.
pub const PROGRAM_AIR_ID: usize = 0;
/// ProgramAir is the first AIR so its cached trace should be the first main trace.
pub const PROGRAM_CACHED_TRACE_INDEX: usize = 0;
pub const CONNECTOR_AIR_ID: usize = 1;
/// If PublicValuesAir is **enabled**, its AIR ID is 2. PublicValuesAir is always disabled when
/// continuations is enabled.
pub const PUBLIC_VALUES_AIR_ID: usize = 2;
/// AIR ID of the Memory Boundary AIR.
pub const BOUNDARY_AIR_ID: usize = PUBLIC_VALUES_AIR_ID + 1 + BOUNDARY_AIR_OFFSET;
/// If VM has continuations enabled, all AIRs of MemoryController are added after ConnectorChip.
/// Merkle AIR commits start/final memory states.
pub const MERKLE_AIR_ID: usize = CONNECTOR_AIR_ID + 1 + MERKLE_AIR_OFFSET;

const EXECUTION_BUS: ExecutionBus = ExecutionBus(0);
const MEMORY_BUS: MemoryBus = MemoryBus(1);
const PROGRAM_BUS: ProgramBus = ProgramBus(2);
const RANGE_CHECKER_BUS: usize = 3;

/// Configuration for a processor extension.
///
/// There are two associated types:
/// - `Executor`: enum for chips that are [`InstructionExecutor`]s.
/// -
pub trait VmExtension<F: PrimeField32> {
    /// Enum of chips that implement [`InstructionExecutor`] for instruction execution.
    /// `Executor` **must** implement `Chip<SC>` but the trait bound is omitted to omit the
    /// `StarkGenericConfig` generic parameter.
    type Executor: InstructionExecutor<F> + AnyEnum;
    /// Enum of periphery chips that do not implement [`InstructionExecutor`].
    /// `Periphery` **must** implement `Chip<SC>` but the trait bound is omitted to omit the
    /// `StarkGenericConfig` generic parameter.
    type Periphery: AnyEnum;

    fn build(
        &self,
        builder: &mut VmInventoryBuilder<F>,
    ) -> Result<VmInventory<Self::Executor, Self::Periphery>, VmInventoryError>;
}

/// Builder for processing unit. Processing units extend an existing system unit.
pub struct VmInventoryBuilder<'a, F: PrimeField32> {
    system: &'a SystemBase<F>,
    streams: &'a Arc<Mutex<Streams<F>>>,
    /// Bus indices are in range [0, bus_idx_max)
    bus_idx_max: usize,
    /// Chips that are already included in the chipset and may be used
    /// as dependencies. The order should be that depended-on chips are ordered
    /// **before** their dependents.
    chips: Vec<&'a dyn AnyEnum>,
}

impl<'a, F: PrimeField32> VmInventoryBuilder<'a, F> {
    pub fn new(
        system: &'a SystemBase<F>,
        streams: &'a Arc<Mutex<Streams<F>>>,
        bus_idx_max: usize,
    ) -> Self {
        Self {
            system,
            streams,
            bus_idx_max,
            chips: Vec::new(),
        }
    }

    pub fn memory_controller(&self) -> &MemoryControllerRef<F> {
        &self.system.memory_controller
    }

    pub fn system_base(&self) -> &SystemBase<F> {
        self.system
    }

    pub fn new_bus_idx(&mut self) -> usize {
        let idx = self.bus_idx_max;
        self.bus_idx_max += 1;
        idx
    }

    /// Looks through built chips to see if there exists any of type `C` by downcasting.
    /// Returns all chips of type `C` in the chipset.
    ///
    /// Note: the type `C` will usually be a smart pointer to a chip.
    pub fn find_chip<C: 'static>(&self) -> Vec<&C> {
        self.chips
            .iter()
            .filter_map(|c| c.as_any_kind().downcast_ref())
            .collect()
    }

    /// The generic `F` must match that of the `PhantomChip<F>`.
    pub fn add_phantom_sub_executor<PE: PhantomSubExecutor<F> + 'static>(
        &self,
        phantom_sub: PE,
        discriminant: PhantomDiscriminant,
    ) -> Result<(), VmInventoryError> {
        let chip_ref: &RefCell<PhantomChip<F>> =
            self.find_chip().first().expect("PhantomChip always exists");
        let mut chip = chip_ref.borrow_mut();
        let existing = chip.add_sub_executor(phantom_sub, discriminant);
        if existing.is_some() {
            return Err(VmInventoryError::PhantomSubExecutorExists { discriminant });
        }
        Ok(())
    }

    /// Shareable streams. Clone to get a shared mutable reference.
    pub fn streams(&self) -> &Arc<Mutex<Streams<F>>> {
        self.streams
    }

    fn add_chip<E: AnyEnum>(&mut self, chip: &'a E) {
        self.chips.push(chip);
    }
}

#[derive(Clone, Debug)]
pub struct VmInventory<E, P> {
    /// Lookup table to executor ID. We store executors separately due to mutable borrow issues.
    instruction_lookup: FxHashMap<AxVmOpcode, ExecutorId>,
    executors: Vec<E>,
    pub(super) periphery: Vec<P>,
    /// Order of insertion. The reverse of this will be the order the chips are destroyed
    /// to generate trace.
    insertion_order: Vec<ChipId>,
}

type ExecutorId = usize;
/// TODO: create newtype
type AxVmOpcode = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipId {
    Executor(usize),
    Periphery(usize),
}

#[derive(thiserror::Error, Debug)]
pub enum VmInventoryError {
    #[error("Opcode {opcode} already owned by executor id {id}")]
    ExecutorExists { opcode: AxVmOpcode, id: ExecutorId },
    #[error("Phantom discriminant {} already has sub-executor", .discriminant.0)]
    PhantomSubExecutorExists { discriminant: PhantomDiscriminant },
    #[error("Chip {name} not found")]
    ChipNotFound { name: String },
}

impl<E, P> Default for VmInventory<E, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E, P> VmInventory<E, P> {
    pub fn new() -> Self {
        Self {
            instruction_lookup: FxHashMap::default(),
            executors: Vec::new(),
            periphery: Vec::new(),
            insertion_order: Vec::new(),
        }
    }

    pub fn transmute<E2, P2>(self) -> VmInventory<E2, P2>
    where
        E: Into<E2>,
        P: Into<P2>,
    {
        VmInventory {
            instruction_lookup: self.instruction_lookup,
            executors: self.executors.into_iter().map(|e| e.into()).collect(),
            periphery: self.periphery.into_iter().map(|p| p.into()).collect(),
            insertion_order: self.insertion_order,
        }
    }

    /// Append `other` to current inventory. This means `self` comes earlier in the dependency chain.
    pub fn append(&mut self, mut other: VmInventory<E, P>) -> Result<(), VmInventoryError> {
        let num_executors = self.executors.len();
        let num_periphery = self.periphery.len();
        for (opcode, mut id) in other.instruction_lookup.into_iter() {
            id += num_executors;
            if let Some(old_id) = self.instruction_lookup.insert(opcode, id) {
                return Err(VmInventoryError::ExecutorExists { opcode, id: old_id });
            }
        }
        for chip_id in other.insertion_order.iter_mut() {
            match chip_id {
                ChipId::Executor(id) => *id += num_executors,
                ChipId::Periphery(id) => *id += num_periphery,
            }
        }
        self.executors.append(&mut other.executors);
        self.periphery.append(&mut other.periphery);
        self.insertion_order.append(&mut other.insertion_order);
        Ok(())
    }

    /// Inserts an executor with the collection of opcodes that it handles.
    /// If some executor already owns one of the opcodes, it will be replaced and the old
    /// executor ID is returned.
    pub fn add_executor(
        &mut self,
        executor: impl Into<E>,
        opcodes: impl IntoIterator<Item = AxVmOpcode>,
    ) -> Result<(), VmInventoryError> {
        let opcodes: Vec<_> = opcodes.into_iter().collect();
        for opcode in &opcodes {
            if let Some(id) = self.instruction_lookup.get(opcode) {
                return Err(VmInventoryError::ExecutorExists {
                    opcode: *opcode,
                    id: *id,
                });
            }
        }
        let id = self.executors.len();
        self.executors.push(executor.into());
        self.insertion_order.push(ChipId::Executor(id));
        for opcode in opcodes {
            self.instruction_lookup.insert(opcode, id);
        }
        Ok(())
    }

    pub fn add_periphery_chip(&mut self, periphery_chip: impl Into<P>) {
        let id = self.periphery.len();
        self.periphery.push(periphery_chip.into());
        self.insertion_order.push(ChipId::Periphery(id));
    }

    pub fn get_executor(&self, opcode: AxVmOpcode) -> Option<&E> {
        let id = self.instruction_lookup.get(&opcode)?;
        self.executors.get(*id)
    }

    pub fn get_mut_executor(&mut self, opcode: &AxVmOpcode) -> Option<&mut E> {
        let id = self.instruction_lookup.get(opcode)?;
        self.executors.get_mut(*id)
    }

    pub fn executors(&self) -> &[E] {
        &self.executors
    }

    pub fn periphery(&self) -> &[P] {
        &self.periphery
    }

    pub fn num_airs(&self) -> usize {
        self.executors.len() + self.periphery.len()
    }
}

// PublicValuesChip needs F: PrimeField32 due to Adapter
/// The minimum collection of chips that any VM must have.
#[derive(Getters)]
pub struct VmChipComplex<F: PrimeField32, E, P> {
    #[getset(get = "pub")]
    config: SystemConfig,
    // ATTENTION: chip destruction should follow the **reverse** of the following field order:
    pub base: SystemBase<F>,
    /// Extendable collection of chips for executing instructions.
    /// System ensures it contains:
    /// - PhantomChip
    /// - PublicValuesChip if continuations disabled
    /// - Poseidon2Chip if continuations enabled
    pub inventory: VmInventory<E, P>,

    streams: Arc<Mutex<Streams<F>>>,
    /// System buses use indices [0, bus_idx_max)
    bus_idx_max: usize,
}

/// The base [VmChipComplex] with only system chips.
pub type SystemComplex<F> = VmChipComplex<F, SystemExecutor<F>, SystemPeriphery<F>>;

/// Base system chips.
/// The following don't execute instructions, but are essential
/// for the VM architecture.
pub struct SystemBase<F> {
    // RangeCheckerChip **must** be the last chip to have trace generation called on
    pub range_checker_chip: Arc<VariableRangeCheckerChip>,
    pub memory_controller: MemoryControllerRef<F>,
    pub connector_chip: VmConnectorChip<F>,
    pub program_chip: ProgramChip<F>,

    range_checker_bus: VariableRangeCheckerBus,
}

impl<F> SystemBase<F> {
    pub fn range_checker_bus(&self) -> VariableRangeCheckerBus {
        self.range_checker_bus
    }

    pub fn memory_bus(&self) -> MemoryBus {
        MEMORY_BUS
    }

    pub fn program_bus(&self) -> ProgramBus {
        PROGRAM_BUS
    }

    pub fn execution_bus(&self) -> ExecutionBus {
        EXECUTION_BUS
    }
}

#[derive(ChipUsageGetter, Chip, AnyEnum, From, InstructionExecutor)]
pub enum SystemExecutor<F: PrimeField32> {
    PublicValues(PublicValuesChip<F>),
    Phantom(RefCell<PhantomChip<F>>),
}

#[derive(ChipUsageGetter, Chip, AnyEnum, From)]
pub enum SystemPeriphery<F: PrimeField32> {
    /// Poseidon2 chip with direct compression interactions
    Poseidon2(Poseidon2Chip<F>),
}

impl<F: PrimeField32> SystemComplex<F> {
    pub fn new(config: SystemConfig) -> Self {
        let range_bus =
            VariableRangeCheckerBus::new(RANGE_CHECKER_BUS, config.memory_config.decomp);
        let mut bus_idx_max = RANGE_CHECKER_BUS;

        let range_checker = Arc::new(VariableRangeCheckerChip::new(range_bus));
        let memory_controller = if config.continuation_enabled {
            bus_idx_max += 2;
            MemoryController::with_persistent_memory(
                MEMORY_BUS,
                config.memory_config,
                range_checker.clone(),
                MemoryMerkleBus(bus_idx_max - 2),
                DirectCompressionBus(bus_idx_max - 1),
                Equipartition::<F, CHUNK>::new(),
            )
        } else {
            MemoryController::with_volatile_memory(
                MEMORY_BUS,
                config.memory_config,
                range_checker.clone(),
            )
        };
        let memory_controller = Rc::new(RefCell::new(memory_controller));
        let program_chip = ProgramChip::new(PROGRAM_BUS);
        let connector_chip = VmConnectorChip::new(EXECUTION_BUS, PROGRAM_BUS);

        let mut inventory = VmInventory::new();
        // PublicValuesChip is required when num_public_values > 0 in single segment mode.
        if config.has_public_values_chip() {
            assert_eq!(inventory.executors().len(), Self::PV_EXECUTOR_IDX);
            let chip = PublicValuesChip::new(
                NativeAdapterChip::new(EXECUTION_BUS, PROGRAM_BUS, memory_controller.clone()),
                PublicValuesCoreChip::new(
                    config.num_public_values,
                    PublishOpcode::default_offset(),
                    config.max_constraint_degree as u32,
                ),
                memory_controller.clone(),
            );
            inventory
                .add_executor(chip, [PublishOpcode::default_offset()])
                .unwrap();
        }
        if config.continuation_enabled {
            assert_eq!(inventory.periphery().len(), Self::POSEIDON2_PERIPHERY_IDX);
            // Add direct poseidon2 chip for persistent memory.
            // This is **not** an instruction executor.
            // Currently we never use poseidon2 opcodes when continuations is enabled: we will need
            // special handling when that happens
            let direct_bus_idx = memory_controller
                .borrow()
                .interface_chip
                .compression_bus()
                .unwrap()
                .0;
            let chip = Poseidon2Chip::from_poseidon2_config(
                vm_poseidon2_config(),
                config.max_constraint_degree.min(SBOX_DEGREE),
                EXECUTION_BUS,
                PROGRAM_BUS,
                memory_controller.clone(),
                direct_bus_idx,
                Poseidon2Opcode::default_offset(),
            );
            inventory.add_periphery_chip(chip);
        }
        let streams = Arc::new(Mutex::new(Streams::default()));
        let phantom_opcode = SystemOpcode::PHANTOM.with_default_offset();
        let mut phantom_chip = PhantomChip::new(
            EXECUTION_BUS,
            PROGRAM_BUS,
            memory_controller.clone(),
            SystemOpcode::default_offset(),
        );
        phantom_chip.set_streams(streams.clone());
        inventory
            .add_executor(RefCell::new(phantom_chip), [phantom_opcode])
            .unwrap();

        let base = SystemBase {
            program_chip,
            connector_chip,
            memory_controller,
            range_checker_chip: range_checker,
            range_checker_bus: range_bus,
        };

        Self {
            config,
            base,
            inventory,
            bus_idx_max,
            streams,
        }
    }
}

impl<F: PrimeField32, E, P> VmChipComplex<F, E, P> {
    /// **If** public values chip exists, then its executor index is 0.
    pub(super) const PV_EXECUTOR_IDX: ExecutorId = 0;
    /// **If** internal poseidon2 chip exists, then its periphery index is 0.
    pub(super) const POSEIDON2_PERIPHERY_IDX: usize = 0;

    // @dev: Remember to update self.bus_idx_max after dropping this!
    pub fn inventory_builder(&self) -> VmInventoryBuilder<F>
    where
        E: AnyEnum,
        P: AnyEnum,
    {
        let mut builder = VmInventoryBuilder::new(&self.base, &self.streams, self.bus_idx_max);
        // Add range checker for convenience, the other system base chips aren't included - they can be accessed directly from builder
        builder.add_chip(&self.base.range_checker_chip);
        for chip in self.inventory.executors() {
            builder.add_chip(chip);
        }
        for chip in self.inventory.periphery() {
            builder.add_chip(chip);
        }

        builder
    }

    /// Extend the chip complex with a new extension.
    /// A new chip complex with different type generics is returned with the combined inventory.
    pub fn extend<E3, P3, Ext>(
        mut self,
        config: &Ext,
    ) -> Result<VmChipComplex<F, E3, P3>, VmInventoryError>
    where
        Ext: VmExtension<F>,
        E: Into<E3> + AnyEnum,
        P: Into<P3> + AnyEnum,
        Ext::Executor: Into<E3>,
        Ext::Periphery: Into<P3>,
    {
        let mut builder = self.inventory_builder();
        let inventory_ext = config.build(&mut builder)?;
        self.bus_idx_max = builder.bus_idx_max;
        let mut ext_complex = self.transmute();
        ext_complex.append(inventory_ext.transmute())?;
        Ok(ext_complex)
    }

    pub fn transmute<E2, P2>(self) -> VmChipComplex<F, E2, P2>
    where
        E: Into<E2>,
        P: Into<P2>,
    {
        VmChipComplex {
            config: self.config,
            base: self.base,
            inventory: self.inventory.transmute(),
            bus_idx_max: self.bus_idx_max,
            streams: self.streams,
        }
    }

    /// Appends `other` to the current inventory.
    /// This means `self` comes earlier in the dependency chain.
    pub fn append(&mut self, other: VmInventory<E, P>) -> Result<(), VmInventoryError> {
        self.inventory.append(other)
    }

    pub fn program_chip(&self) -> &ProgramChip<F> {
        &self.base.program_chip
    }

    pub fn program_chip_mut(&mut self) -> &mut ProgramChip<F> {
        &mut self.base.program_chip
    }

    pub fn connector_chip(&self) -> &VmConnectorChip<F> {
        &self.base.connector_chip
    }

    pub fn connector_chip_mut(&mut self) -> &mut VmConnectorChip<F> {
        &mut self.base.connector_chip
    }

    pub fn memory_controller(&self) -> &MemoryControllerRef<F> {
        &self.base.memory_controller
    }

    pub fn range_checker_chip(&self) -> &Arc<VariableRangeCheckerChip> {
        &self.base.range_checker_chip
    }

    pub fn public_values_chip(&self) -> Option<&PublicValuesChip<F>>
    where
        E: AnyEnum,
    {
        let chip = self.inventory.executors().get(Self::PV_EXECUTOR_IDX)?;
        chip.as_any_kind().downcast_ref()
    }

    pub fn poseidon2_chip(&self) -> Option<&Poseidon2Chip<F>>
    where
        P: AnyEnum,
    {
        let chip = self
            .inventory
            .periphery
            .get(Self::POSEIDON2_PERIPHERY_IDX)?;
        chip.as_any_kind().downcast_ref()
    }

    pub fn poseidon2_chip_mut(&mut self) -> Option<&mut Poseidon2Chip<F>>
    where
        P: AnyEnum,
    {
        let chip = self
            .inventory
            .periphery
            .get_mut(Self::POSEIDON2_PERIPHERY_IDX)?;
        chip.as_any_kind_mut().downcast_mut()
    }

    pub(crate) fn set_program(&mut self, program: Program<F>) {
        self.base.program_chip.set_program(program);
    }

    /// Warning: this sets the stream in all chips which have a shared mutable reference to the streams.
    pub(crate) fn set_streams(&mut self, streams: Streams<F>) {
        *self.streams.lock() = streams;
    }

    /// This should **only** be called after segment execution has finished.
    pub(super) fn take_streams(&mut self) -> Streams<F> {
        std::mem::take(&mut self.streams.lock())
    }

    // This is O(1).
    pub fn num_airs(&self) -> usize {
        3 + self.memory_controller().borrow().num_airs() + self.inventory.num_airs()
    }

    // TODO[jpw]: find better way to handle public values chip. It is an executor but
    // we always need to special case it because we need to fix the air id.
    fn public_values_chip_idx(&self) -> Option<ExecutorId> {
        self.config
            .has_public_values_chip()
            .then_some(Self::PV_EXECUTOR_IDX)
    }

    // Avoids a downcast when you don't need the concrete type.
    fn _public_values_chip(&self) -> Option<&E> {
        self.config
            .has_public_values_chip()
            .then(|| &self.inventory.executors[Self::PV_EXECUTOR_IDX])
    }

    // All inventory chips except public values chip, in reverse order they were added.
    pub(crate) fn chips_excluding_pv_chip(&self) -> impl Iterator<Item = Either<&'_ E, &'_ P>> {
        let public_values_chip_idx = self.public_values_chip_idx();
        self.inventory
            .insertion_order
            .iter()
            .rev()
            .flat_map(move |chip_idx| match *chip_idx {
                // Skip public values chip if it exists.
                ChipId::Executor(id) => (Some(id) != public_values_chip_idx)
                    .then(|| Either::Executor(&self.inventory.executors[id])),
                ChipId::Periphery(id) => Some(Either::Periphery(&self.inventory.periphery[id])),
            })
    }

    /// Return air names of all chips in order.
    pub(crate) fn air_names(&self) -> Vec<String>
    where
        E: ChipUsageGetter,
        P: ChipUsageGetter,
    {
        once(self.program_chip().air_name())
            .chain([self.connector_chip().air_name()])
            .chain(self._public_values_chip().map(|c| c.air_name()))
            .chain(self.memory_controller().borrow().air_names())
            .chain(self.chips_excluding_pv_chip().map(|c| c.air_name()))
            .chain([self.range_checker_chip().air_name()])
            .collect()
    }
    /// Return trace heights of all chips in order.
    pub(crate) fn current_trace_heights(&self) -> Vec<usize>
    where
        E: ChipUsageGetter,
        P: ChipUsageGetter,
    {
        once(self.program_chip().current_trace_height())
            .chain([self.connector_chip().current_trace_height()])
            .chain(self._public_values_chip().map(|c| c.current_trace_height()))
            .chain(self.memory_controller().borrow().current_trace_heights())
            .chain(
                self.chips_excluding_pv_chip()
                    .map(|c| c.current_trace_height()),
            )
            .chain([self.range_checker_chip().current_trace_height()])
            .collect()
    }
    /// Return dynamic trace heights of all chips in order, or 0 if
    /// chip has constant height.
    // Used for continuation segmentation logic, so this is performance-sensitive.
    // Return iterator so we can break early.
    pub(crate) fn dynamic_trace_heights(&self) -> impl Iterator<Item = usize> + '_
    where
        E: ChipUsageGetter,
        P: ChipUsageGetter,
    {
        // program_chip, connector_chip
        [0, 0]
            .into_iter()
            .chain(self._public_values_chip().map(|c| c.current_trace_height()))
            .chain(self.memory_controller().borrow().current_trace_heights())
            .chain(self.chips_excluding_pv_chip().map(|c| match c {
                // executor should never be constant height
                Either::Executor(c) => c.current_trace_height(),
                Either::Periphery(c) => {
                    if c.constant_trace_height().is_some() {
                        0
                    } else {
                        c.current_trace_height()
                    }
                }
            }))
            .chain([0]) // range_checker_chip
    }

    /// Return trace cells of all chips in order.
    pub(crate) fn current_trace_cells(&self) -> Vec<usize>
    where
        E: ChipUsageGetter,
        P: ChipUsageGetter,
    {
        once(self.program_chip().current_trace_cells())
            .chain([self.connector_chip().current_trace_cells()])
            .chain(self._public_values_chip().map(|c| c.current_trace_cells()))
            .chain(self.memory_controller().borrow().current_trace_cells())
            .chain(
                self.chips_excluding_pv_chip()
                    .map(|c| c.current_trace_cells()),
            )
            .chain([self.range_checker_chip().current_trace_cells()])
            .collect()
    }

    pub(crate) fn airs<SC: StarkGenericConfig>(&self) -> Vec<Arc<dyn AnyRap<SC>>>
    where
        Domain<SC>: PolynomialSpace<Val = F>,
        E: Chip<SC>,
        P: Chip<SC>,
    {
        // ATTENTION: The order of AIR MUST be consistent with `generate_proof_input`.
        let program_rap = Arc::new(self.program_chip().air) as Arc<dyn AnyRap<SC>>;
        let connector_rap = Arc::new(self.connector_chip().air) as Arc<dyn AnyRap<SC>>;
        [program_rap, connector_rap]
            .into_iter()
            .chain(self._public_values_chip().map(|chip| chip.air()))
            .chain(self.memory_controller().borrow().airs())
            .chain(self.chips_excluding_pv_chip().map(|chip| match chip {
                Either::Executor(chip) => chip.air(),
                Either::Periphery(chip) => chip.air(),
            }))
            .chain(once(self.range_checker_chip().air()))
            .collect()
    }

    pub(crate) fn generate_proof_input<SC: StarkGenericConfig>(
        mut self,
        cached_program: Option<CommittedTraceData<SC>>,
    ) -> ProofInput<SC>
    where
        Domain<SC>: PolynomialSpace<Val = F>,
        E: Chip<SC>,
        P: Chip<SC>,
    {
        let has_pv_chip = self.public_values_chip_idx().is_some();
        // ATTENTION: The order of AIR proof input generation MUST be consistent with `airs`.
        let mut builder = VmProofInputBuilder::new();
        let SystemBase {
            range_checker_chip,
            memory_controller,
            connector_chip,
            program_chip,
            ..
        } = self.base;
        // System: Program Chip
        debug_assert_eq!(builder.curr_air_id, PROGRAM_AIR_ID);
        builder.add_air_proof_input(program_chip.generate_air_proof_input(cached_program));
        // System: Connector Chip
        debug_assert_eq!(builder.curr_air_id, CONNECTOR_AIR_ID);
        builder.add_air_proof_input(connector_chip.generate_air_proof_input());

        // Go through all chips in inventory in reverse order they were added (to resolve dependencies)
        // Important Note: for air_id ordering reasons, we want to generate_air_proof_input for
        // public values and memory chips **last** but include them into the `builder` **first**.
        let mut public_values_input = None;
        let mut insertion_order = self.inventory.insertion_order;
        insertion_order.reverse();
        let mut non_sys_inputs = Vec::with_capacity(insertion_order.len());
        for chip_id in insertion_order {
            let height = None;
            // let height = self.overridden_executor_heights.as_ref().and_then(
            //     |overridden_heights| {
            //         let executor_name: ExecutorName = (&executor).into();
            //         overridden_heights.get(&executor_name).copied()
            //     },
            // );
            let air_proof_input = match chip_id {
                ChipId::Executor(id) => {
                    let chip = self.inventory.executors.pop().unwrap();
                    assert_eq!(id, self.inventory.executors.len());
                    generate_air_proof_input(chip, height)
                }
                ChipId::Periphery(id) => {
                    let chip = self.inventory.periphery.pop().unwrap();
                    assert_eq!(id, self.inventory.periphery.len());
                    generate_air_proof_input(chip, height)
                }
            };
            if has_pv_chip && chip_id == ChipId::Executor(Self::PV_EXECUTOR_IDX) {
                public_values_input = Some(air_proof_input);
            } else {
                non_sys_inputs.push(air_proof_input);
            }
        }

        if let Some(input) = public_values_input {
            debug_assert_eq!(builder.curr_air_id, PUBLIC_VALUES_AIR_ID);
            builder.add_air_proof_input(input);
        }
        // System: Memory Controller
        {
            // memory
            let memory_controller = Rc::try_unwrap(memory_controller)
                .expect("other chips still hold a reference to memory chip")
                .into_inner();

            let air_proof_inputs = memory_controller.generate_air_proof_inputs();
            for air_proof_input in air_proof_inputs {
                builder.add_air_proof_input(air_proof_input);
            }
        }
        // Non-system chips
        non_sys_inputs
            .into_iter()
            .for_each(|input| builder.add_air_proof_input(input));
        // System: Range Checker Chip
        builder.add_air_proof_input(range_checker_chip.generate_air_proof_input());

        builder.build()
    }
}

struct VmProofInputBuilder<SC: StarkGenericConfig> {
    curr_air_id: usize,
    proof_input_per_air: Vec<(usize, AirProofInput<SC>)>,
}

impl<SC: StarkGenericConfig> VmProofInputBuilder<SC> {
    fn new() -> Self {
        Self {
            curr_air_id: 0,
            proof_input_per_air: vec![],
        }
    }
    /// Adds air proof input if one of the main trace matrices is non-empty.
    /// Always increments the internal `curr_air_id` regardless of whether a new air proof input was added or not.
    fn add_air_proof_input(&mut self, air_proof_input: AirProofInput<SC>) {
        let h = if !air_proof_input.raw.cached_mains.is_empty() {
            air_proof_input.raw.cached_mains[0].height()
        } else {
            air_proof_input
                .raw
                .common_main
                .as_ref()
                .map(|trace| trace.height())
                .unwrap()
        };
        if h > 0 {
            self.proof_input_per_air
                .push((self.curr_air_id, air_proof_input));
        }
        self.curr_air_id += 1;
    }

    fn build(self) -> ProofInput<SC> {
        ProofInput {
            per_air: self.proof_input_per_air,
        }
    }
}

/// Generates an AIR proof input of the chip with the given height, if any.
///
/// Assumption: an all-0 row is a valid dummy row for `chip`.
pub fn generate_air_proof_input<SC: StarkGenericConfig, C: Chip<SC>>(
    chip: C,
    height: Option<usize>,
) -> AirProofInput<SC> {
    let mut proof_input = chip.generate_air_proof_input();
    if let Some(height) = height {
        let height = height.next_power_of_two();
        let main = proof_input.raw.common_main.as_mut().unwrap();
        assert!(
            height >= main.height(),
            "Overridden height must be greater than or equal to the used height"
        );
        main.pad_to_height(height, AbstractField::ZERO);
    }
    proof_input
}

/// A helper trait for downcasting types that may be enums.
pub trait AnyEnum {
    /// Recursively "unwraps" enum and casts to `Any` for downcasting.
    fn as_any_kind(&self) -> &dyn Any;

    /// Recursively "unwraps" enum and casts to `Any` for downcasting.
    fn as_any_kind_mut(&mut self) -> &mut dyn Any;
}

impl AnyEnum for () {
    fn as_any_kind(&self) -> &dyn Any {
        self
    }
    fn as_any_kind_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl AnyEnum for Arc<VariableRangeCheckerChip> {
    fn as_any_kind(&self) -> &dyn Any {
        self
    }
    fn as_any_kind_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub(crate) enum Either<E, P> {
    Executor(E),
    Periphery(P),
}

impl<'a, E, P> ChipUsageGetter for Either<&'a E, &'a P>
where
    E: ChipUsageGetter,
    P: ChipUsageGetter,
{
    fn air_name(&self) -> String {
        match self {
            Either::Executor(chip) => chip.air_name(),
            Either::Periphery(chip) => chip.air_name(),
        }
    }
    fn current_trace_height(&self) -> usize {
        match self {
            Either::Executor(chip) => chip.current_trace_height(),
            Either::Periphery(chip) => chip.current_trace_height(),
        }
    }
    fn current_trace_cells(&self) -> usize {
        match self {
            Either::Executor(chip) => chip.current_trace_cells(),
            Either::Periphery(chip) => chip.current_trace_cells(),
        }
    }
    fn trace_width(&self) -> usize {
        match self {
            Either::Executor(chip) => chip.trace_width(),
            Either::Periphery(chip) => chip.trace_width(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(Copy, Clone)]
    enum EnumA {
        A(u8),
        B(u32),
    }

    enum EnumB {
        C(u64),
        D(EnumA),
    }

    #[derive(AnyEnum)]
    enum EnumC {
        C(u64),
        #[any_enum]
        D(EnumA),
    }

    impl AnyEnum for EnumA {
        fn as_any_kind(&self) -> &dyn Any {
            match self {
                EnumA::A(a) => a,
                EnumA::B(b) => b,
            }
        }

        fn as_any_kind_mut(&mut self) -> &mut dyn Any {
            match self {
                EnumA::A(a) => a,
                EnumA::B(b) => b,
            }
        }
    }

    impl AnyEnum for EnumB {
        fn as_any_kind(&self) -> &dyn Any {
            match self {
                EnumB::C(c) => c,
                EnumB::D(d) => d.as_any_kind(),
            }
        }

        fn as_any_kind_mut(&mut self) -> &mut dyn Any {
            match self {
                EnumB::C(c) => c,
                EnumB::D(d) => d.as_any_kind_mut(),
            }
        }
    }

    #[test]
    fn test_any_enum_downcast() {
        let a = EnumA::A(1);
        assert_eq!(a.as_any_kind().downcast_ref::<u8>(), Some(&1));
        let b = EnumB::D(a);
        assert!(b.as_any_kind().downcast_ref::<u64>().is_none());
        assert!(b.as_any_kind().downcast_ref::<EnumA>().is_none());
        assert_eq!(b.as_any_kind().downcast_ref::<u8>(), Some(&1));
        let c = EnumB::C(3);
        assert_eq!(c.as_any_kind().downcast_ref::<u64>(), Some(&3));
        let d = EnumC::D(a);
        assert!(d.as_any_kind().downcast_ref::<u64>().is_none());
        assert!(d.as_any_kind().downcast_ref::<EnumA>().is_none());
        assert_eq!(d.as_any_kind().downcast_ref::<u8>(), Some(&1));
        let e = EnumC::C(3);
        assert_eq!(e.as_any_kind().downcast_ref::<u64>(), Some(&3));
    }
}