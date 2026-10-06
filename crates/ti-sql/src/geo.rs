//! Geo functions bind implicit position columns during typed analysis.
use datafusion::{
    arrow::{array::{Array, BooleanArray, Float64Array}, datatypes::DataType},
    common::{config::ConfigOptions, tree_node::Transformed, Column, DataFusionError, DFSchema, Result},
    logical_expr::{create_udf, expr_rewriter::FunctionRewrite, ColumnarValue, Expr, ScalarUDF, Volatility},
};
use std::sync::Arc;

pub(crate) fn marker(name: &str, arity: usize) -> ScalarUDF {
    create_udf(name, vec![DataType::Float64; arity], DataType::Boolean, Volatility::Stable,
        Arc::new(|_| Err(DataFusionError::Plan("geo needs latitude@last and longitude@last columns in scope".into()))))
}

fn refined(name: &str, arity: usize) -> ScalarUDF {
    let bbox = name == "ti_in_bbox";
    create_udf(name, vec![DataType::Float64; arity], DataType::Boolean, Volatility::Immutable,
        Arc::new(move |args: &[ColumnarValue]| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let values = arrays.iter().map(|a| a.as_any().downcast_ref::<Float64Array>()
                .ok_or_else(|| DataFusionError::Execution("geo argument must be Float64".into())))
                .collect::<Result<Vec<_>>>()?;
            let result = (0..arrays[0].len()).map(|row| {
                if arrays.iter().any(|a| a.is_null(row)) { return Ok(None); }
                let v = |i: usize| values[i].value(row);
                let matches = if bbox {
                    ti_geo::Bbox::new(v(2), v(3), v(4), v(5))
                        .map(|b| b.contains(v(0), v(1)))
                } else {
                    ti_geo::within_nm(v(0), v(1), v(2), v(3), v(4))
                }.map_err(crate::core_error)?;
                Ok(Some(matches))
            }).collect::<Result<Vec<_>>>()?;
            Ok(ColumnarValue::Array(Arc::new(BooleanArray::from(result))))
        }))
}

#[derive(Debug)]
pub(crate) struct GeoRewrite;
impl FunctionRewrite for GeoRewrite {
    fn name(&self) -> &str { "lume_geo_position" }
    fn rewrite(&self, expr: Expr, schema: &DFSchema, _config: &ConfigOptions) -> Result<Transformed<Expr>> {
        let Expr::ScalarFunction(f) = &expr else { return Ok(Transformed::no(expr)); };
        let (internal, arity) = match f.name() {
            "in_bbox" => ("ti_in_bbox", 6),
            "within_nm" => ("ti_within_nm", 5),
            _ => return Ok(Transformed::no(expr)),
        };
        let mut args = Vec::with_capacity(arity);
        for name in ["navigation.position.latitude@last", "navigation.position.longitude@last"] {
            let (qualifier, field) = schema.qualified_field_with_unqualified_name(name)?;
            if field.data_type() != &DataType::Float64 {
                return Err(DataFusionError::Plan(format!("{name} must be Float64")));
            }
            args.push(Expr::Column(Column::new(qualifier.cloned(), name)));
        }
        args.extend(f.args.clone());
        Ok(Transformed::yes(refined(internal, arity).call(args)))
    }
}
