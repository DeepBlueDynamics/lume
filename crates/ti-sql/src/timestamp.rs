//! Internal precision barrier inserted by SQL rewriting; never required in user SQL.
use datafusion::{
    arrow::datatypes::{DataType, TimeUnit},
    common::Result,
    logical_expr::{
        ColumnarValue, Documentation, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature,
        Volatility,
    },
};
use std::sync::LazyLock;
#[derive(Debug, PartialEq, Eq, Hash)]
struct TimestampIdentity {
    signature: Signature,
}
static DOC: LazyLock<Documentation> = LazyLock::new(|| {
    Documentation::builder(Default::default(),"Internal Lume planning function, inserted automatically to preserve timestamp predicate precision. Write ordinary ts comparisons; do not call this function directly.","ts > TIMESTAMP '2026-05-01 12:00:00.5'").build()
});
impl ScalarUDFImpl for TimestampIdentity {
    fn name(&self) -> &str {
        "ti_timestamp"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _args: &[DataType]) -> Result<DataType> {
        Ok(DataType::Timestamp(
            TimeUnit::Nanosecond,
            Some("UTC".into()),
        ))
    }
    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        Ok(args.args[0].clone())
    }
    fn documentation(&self) -> Option<&Documentation> {
        Some(&DOC)
    }
}
pub(crate) fn timestamp_udf() -> ScalarUDF {
    ScalarUDF::from(TimestampIdentity {
        signature: Signature::exact(
            vec![DataType::Timestamp(
                TimeUnit::Nanosecond,
                Some("UTC".into()),
            )],
            Volatility::Immutable,
        ),
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_barrier_is_documented_as_internal() {
        assert!(super::timestamp_udf()
            .documentation()
            .unwrap()
            .description
            .starts_with("Internal"));
    }
}
