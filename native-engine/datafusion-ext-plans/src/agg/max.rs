// Copyright 2022 The Blaze Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::agg::agg_buf::{AccumInitialValue, AggBuf, AggDynStr};
use crate::agg::Agg;
use arrow::array::*;
use arrow::datatypes::*;
use datafusion::common::{Result, ScalarValue};
use datafusion::error::DataFusionError;
use datafusion::physical_expr::PhysicalExpr;
use paste::paste;
use std::any::Any;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

pub struct AggMax {
    child: Arc<dyn PhysicalExpr>,
    data_type: DataType,
    accums_initial: Vec<AccumInitialValue>,
    partial_updater: fn(&mut AggBuf, u64, &ArrayRef, usize),
    partial_buf_merger: fn(&mut AggBuf, &mut AggBuf, u64),
}

impl AggMax {
    pub fn try_new(child: Arc<dyn PhysicalExpr>, data_type: DataType) -> Result<Self> {
        let accums_initial = vec![AccumInitialValue::Scalar(ScalarValue::try_from(&data_type)?)];
        let partial_updater = get_partial_updater(&data_type)?;
        let partial_buf_merger = get_partial_buf_merger(&data_type)?;
        Ok(Self {
            child,
            data_type,
            accums_initial,
            partial_updater,
            partial_buf_merger,
        })
    }
}

impl Debug for AggMax {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Max({:?})", self.child)
    }
}

impl Agg for AggMax {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn exprs(&self) -> Vec<Arc<dyn PhysicalExpr>> {
        vec![self.child.clone()]
    }

    fn data_type(&self) -> &DataType {
        &self.data_type
    }

    fn nullable(&self) -> bool {
        true
    }

    fn accums_initial(&self) -> &[AccumInitialValue] {
        &self.accums_initial
    }

    fn partial_update(
        &self,
        agg_buf: &mut AggBuf,
        agg_buf_addrs: &[u64],
        values: &[ArrayRef],
        row_idx: usize,
    ) -> Result<()> {
        let partial_updater = self.partial_updater;
        let addr = agg_buf_addrs[0];
        partial_updater(agg_buf, addr, &values[0], row_idx);
        Ok(())
    }

    fn partial_update_all(
        &self,
        agg_buf: &mut AggBuf,
        agg_buf_addrs: &[u64],
        values: &[ArrayRef],
    ) -> Result<()> {
        let addr = agg_buf_addrs[0];

        macro_rules! handle_fixed {
            ($ty:ident, $maxfun:ident) => {{
                type TArray = paste! {[<$ty Array>]};
                let value = values[0].as_any().downcast_ref::<TArray>().unwrap();
                if let Some(max) = arrow::compute::$maxfun(value) {
                    partial_update_prim(agg_buf, addr, max);
                }
            }};
        }
        match values[0].data_type() {
            DataType::Null => {}
            DataType::Boolean => handle_fixed!(Boolean, max_boolean),
            DataType::Float32 => handle_fixed!(Float32, max),
            DataType::Float64 => handle_fixed!(Float64, max),
            DataType::Int8 => handle_fixed!(Int8, max),
            DataType::Int16 => handle_fixed!(Int16, max),
            DataType::Int32 => handle_fixed!(Int32, max),
            DataType::Int64 => handle_fixed!(Int64, max),
            DataType::UInt8 => handle_fixed!(UInt8, max),
            DataType::UInt16 => handle_fixed!(UInt16, max),
            DataType::UInt32 => handle_fixed!(UInt32, max),
            DataType::UInt64 => handle_fixed!(UInt64, max),
            DataType::Date32 => handle_fixed!(Date32, max),
            DataType::Date64 => handle_fixed!(Date64, max),
            DataType::Timestamp(TimeUnit::Second, _) => handle_fixed!(TimestampSecond, max),
            DataType::Timestamp(TimeUnit::Millisecond, _) => {
                handle_fixed!(TimestampMillisecond, max)
            }
            DataType::Timestamp(TimeUnit::Microsecond, _) => {
                handle_fixed!(TimestampMicrosecond, max)
            }
            DataType::Timestamp(TimeUnit::Nanosecond, _) => handle_fixed!(TimestampNanosecond, max),
            DataType::Decimal128(_, _) => handle_fixed!(Decimal128, max),
            DataType::Utf8 => {
                let value = values[0].as_any().downcast_ref::<StringArray>().unwrap();
                if let Some(max) = arrow::compute::max_string(value) {
                    let w = AggDynStr::value_mut(agg_buf.dyn_value_mut(addr));
                    match w {
                        Some(w) => {
                            if w.as_ref() < max {
                                *w = max.to_owned().into();
                            }
                        }
                        w @ None => {
                            *w = Some(max.to_owned().into());
                        }
                    }
                }
            }
            other => {
                return Err(DataFusionError::NotImplemented(format!(
                    "unsupported data type in max(): {}",
                    other
                )));
            }
        }
        Ok(())
    }

    fn partial_merge(
        &self,
        agg_buf1: &mut AggBuf,
        agg_buf2: &mut AggBuf,
        agg_buf_addrs: &[u64],
    ) -> Result<()> {
        let partial_buf_merger = self.partial_buf_merger;
        let addr = agg_buf_addrs[0];
        partial_buf_merger(agg_buf1, agg_buf2, addr);
        Ok(())
    }
}

fn partial_update_prim<T: Copy + PartialEq + PartialOrd>(agg_buf: &mut AggBuf, addr: u64, v: T) {
    if agg_buf.is_fixed_valid(addr) {
        agg_buf.update_fixed_value::<T>(addr, |w| if v > w { v } else { w });
    } else {
        agg_buf.set_fixed_value::<T>(addr, v);
        agg_buf.set_fixed_valid(addr, true);
    }
}

fn get_partial_updater(dt: &DataType) -> Result<fn(&mut AggBuf, u64, &ArrayRef, usize)> {
    macro_rules! fn_fixed {
        ($ty:ident) => {{
            Ok(|agg_buf, addr, v, i| {
                type TArray = paste! {[<$ty Array>]};
                let value = v.as_any().downcast_ref::<TArray>().unwrap();
                if value.is_valid(i) {
                    partial_update_prim(agg_buf, addr, value.value(i));
                }
            })
        }};
    }
    match dt {
        DataType::Null => Ok(|_, _, _, _| ()),
        DataType::Boolean => fn_fixed!(Boolean),
        DataType::Float32 => fn_fixed!(Float32),
        DataType::Float64 => fn_fixed!(Float64),
        DataType::Int8 => fn_fixed!(Int8),
        DataType::Int16 => fn_fixed!(Int16),
        DataType::Int32 => fn_fixed!(Int32),
        DataType::Int64 => fn_fixed!(Int64),
        DataType::UInt8 => fn_fixed!(UInt8),
        DataType::UInt16 => fn_fixed!(UInt16),
        DataType::UInt32 => fn_fixed!(UInt32),
        DataType::UInt64 => fn_fixed!(UInt64),
        DataType::Date32 => fn_fixed!(Date32),
        DataType::Date64 => fn_fixed!(Date64),
        DataType::Timestamp(TimeUnit::Second, _) => fn_fixed!(TimestampSecond),
        DataType::Timestamp(TimeUnit::Millisecond, _) => fn_fixed!(TimestampMillisecond),
        DataType::Timestamp(TimeUnit::Microsecond, _) => fn_fixed!(TimestampMicrosecond),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => fn_fixed!(TimestampNanosecond),
        DataType::Decimal128(_, _) => fn_fixed!(Decimal128),
        DataType::Utf8 => Ok(|agg_buf: &mut AggBuf, addr: u64, v: &ArrayRef, i: usize| {
            let value = v.as_any().downcast_ref::<StringArray>().unwrap();
            if value.is_valid(i) {
                let w = AggDynStr::value_mut(agg_buf.dyn_value_mut(addr));
                let v = value.value(i);
                if w.as_ref().filter(|w| w.as_ref() >= v).is_none() {
                    *w = Some(v.to_owned().into());
                }
            }
        }),
        other => Err(DataFusionError::NotImplemented(format!(
            "unsupported data type in max(): {}",
            other
        ))),
    }
}

fn get_partial_buf_merger(dt: &DataType) -> Result<fn(&mut AggBuf, &mut AggBuf, u64)> {
    macro_rules! fn_fixed {
        ($ty:ident) => {{
            Ok(|agg_buf1, agg_buf2, addr| {
                type TType = paste! {[<$ty Type>]};
                type TNative = <TType as ArrowPrimitiveType>::Native;
                if agg_buf2.is_fixed_valid(addr) {
                    let v = agg_buf2.fixed_value::<TNative>(addr);
                    partial_update_prim(agg_buf1, addr, v);
                }
            })
        }};
    }
    match dt {
        DataType::Null => Ok(|_, _, _| ()),
        DataType::Boolean => Ok(|agg_buf1, agg_buf2, addr| {
            if agg_buf2.is_fixed_valid(addr) {
                let v = agg_buf2.fixed_value::<bool>(addr);
                partial_update_prim(agg_buf1, addr, v);
            }
        }),
        DataType::Float32 => fn_fixed!(Float32),
        DataType::Float64 => fn_fixed!(Float64),
        DataType::Int8 => fn_fixed!(Int8),
        DataType::Int16 => fn_fixed!(Int16),
        DataType::Int32 => fn_fixed!(Int32),
        DataType::Int64 => fn_fixed!(Int64),
        DataType::UInt8 => fn_fixed!(UInt8),
        DataType::UInt16 => fn_fixed!(UInt16),
        DataType::UInt32 => fn_fixed!(UInt32),
        DataType::UInt64 => fn_fixed!(UInt64),
        DataType::Date32 => fn_fixed!(Date32),
        DataType::Date64 => fn_fixed!(Date64),
        DataType::Timestamp(TimeUnit::Second, _) => fn_fixed!(TimestampSecond),
        DataType::Timestamp(TimeUnit::Millisecond, _) => fn_fixed!(TimestampMillisecond),
        DataType::Timestamp(TimeUnit::Microsecond, _) => fn_fixed!(TimestampMicrosecond),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => fn_fixed!(TimestampNanosecond),
        DataType::Decimal128(_, _) => fn_fixed!(Decimal128),
        DataType::Utf8 => Ok(|agg_buf1, agg_buf2, addr| {
            let v = AggDynStr::value(agg_buf2.dyn_value_mut(addr));
            if v.is_some() {
                let w = AggDynStr::value_mut(agg_buf1.dyn_value_mut(addr));
                let v = v.as_ref().unwrap();
                if w.as_ref().filter(|w| w.as_ref() >= v.as_ref()).is_none() {
                    *w = Some(v.to_owned());
                }
            }
        }),
        other => Err(DataFusionError::NotImplemented(format!(
            "unsupported data type in max(): {}",
            other
        ))),
    }
}
