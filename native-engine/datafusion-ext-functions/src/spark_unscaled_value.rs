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

use arrow::array::*;
use datafusion::common::Result;
use datafusion::common::ScalarValue;
use datafusion::physical_plan::ColumnarValue;
use std::sync::Arc;

/// implements org.apache.spark.sql.catalyst.expressions.UnscaledValue
pub fn spark_unscaled_value(args: &[ColumnarValue]) -> Result<ColumnarValue> {
    Ok(match &args[0] {
        ColumnarValue::Scalar(scalar) => match scalar {
            ScalarValue::Decimal128(Some(v), _, _) => {
                ColumnarValue::Scalar(ScalarValue::Int64(Some(*v as i64)))
            }
            _ => ColumnarValue::Scalar(ScalarValue::Int64(None)),
        },
        ColumnarValue::Array(array) => {
            let array = array.as_any().downcast_ref::<Decimal128Array>().unwrap();
            let mut output = Int64Builder::new();

            for v in array.into_iter() {
                output.append_option(v.map(|v| v as i64));
            }
            ColumnarValue::Array(Arc::new(output.finish()))
        }
    })
}
#[cfg(test)]
mod test {
    use crate::spark_unscaled_value::spark_unscaled_value;
    use arrow::array::{ArrayRef, Decimal128Array, Int64Array};
    use datafusion::common::ScalarValue;
    use datafusion::logical_expr::ColumnarValue;
    use std::sync::Arc;

    #[test]
    fn test_unscaled_value_array() {
        let result = spark_unscaled_value(&vec![ColumnarValue::Array(Arc::new(
            Decimal128Array::from(vec![
                Some(1234567890987654321),
                Some(9876543210),
                Some(135792468109),
                None,
                Some(67898),
            ])
            .with_precision_and_scale(10, 8)
            .unwrap(),
        ))])
        .unwrap()
        .into_array(5);
        let expected = Int64Array::from(vec![
            Some(1234567890987654321),
            Some(9876543210),
            Some(135792468109),
            None,
            Some(67898),
        ]);
        let expected: ArrayRef = Arc::new(expected);
        assert_eq!(&result, &expected);
    }

    #[test]
    fn test_unscaled_value_scalar() {
        let result = spark_unscaled_value(&vec![ColumnarValue::Scalar(ScalarValue::Decimal128(
            Some(123),
            3,
            2,
        ))])
        .unwrap()
        .into_array(1);
        let expected = Int64Array::from(vec![Some(123)]);
        let expected: ArrayRef = Arc::new(expected);
        assert_eq!(&result, &expected);
    }
}
