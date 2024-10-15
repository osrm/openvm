use std::borrow::Borrow;

use afs_stark_backend::{
    interaction::InteractionBuilder,
    rap::{BaseAirWithPublicValues, PartitionedBaseAir},
};
use p3_air::{Air, AirBuilder, BaseAir};
use p3_field::Field;
use p3_matrix::Matrix;

use super::columns::AssertSortedCols;
use crate::{
    is_less_than_tuple::{columns::IsLessThanTupleIoCols, IsLessThanTupleAir},
    var_range::bus::VariableRangeCheckerBus,
};

#[derive(Clone, Debug)]
pub struct AssertSortedAir {
    pub is_less_than_tuple_air: IsLessThanTupleAir,
}

impl AssertSortedAir {
    pub fn new(bus: VariableRangeCheckerBus, limb_bits: Vec<usize>) -> Self {
        // We do not enable interactions for IsLessThanTupleAir because that AIR assumes
        // that `x, y` are on the same row. We will separately enable interactions for this Air.
        Self {
            is_less_than_tuple_air: IsLessThanTupleAir::new(bus, limb_bits),
        }
    }
}

impl<F: Field> BaseAirWithPublicValues<F> for AssertSortedAir {}
impl<F: Field> PartitionedBaseAir<F> for AssertSortedAir {}
impl<F: Field> BaseAir<F> for AssertSortedAir {
    fn width(&self) -> usize {
        AssertSortedCols::<F>::get_width(
            &self.is_less_than_tuple_air.limb_bits,
            self.is_less_than_tuple_air.range_max_bits,
        )
    }
}

impl<AB: InteractionBuilder> Air<AB> for AssertSortedAir {
    fn eval(&self, builder: &mut AB) {
        let main = builder.main();

        // get the current row and the next row
        let (local, next) = (main.row_slice(0), main.row_slice(1));
        let local: &[AB::Var] = (*local).borrow();
        let next: &[AB::Var] = (*next).borrow();

        let local_cols = AssertSortedCols::from_slice(
            local,
            &self.is_less_than_tuple_air.limb_bits,
            self.is_less_than_tuple_air.range_max_bits,
        );

        let next_cols = AssertSortedCols::from_slice(
            next,
            &self.is_less_than_tuple_air.limb_bits,
            self.is_less_than_tuple_air.range_max_bits,
        );

        // constrain that the current key is less than the next
        builder
            .when_transition()
            .assert_one(local_cols.less_than_next_key);

        let io = IsLessThanTupleIoCols {
            x: local_cols.key,
            y: next_cols.key,
            tuple_less_than: local_cols.less_than_next_key,
        };
        let aux = local_cols.is_less_than_tuple_aux;

        self.is_less_than_tuple_air
            .eval_when_transition(builder, io, aux);
    }
}