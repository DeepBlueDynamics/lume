//! Typed plan rewrite runs in DataFusion analysis, before type coercion and
//! simplification. It applies to literal SQL and bound-parameter DataFrames.
use datafusion::{
    arrow::datatypes::DataType,
    common::{config::ConfigOptions, tree_node::Transformed, DFSchema, Result},
    logical_expr::{expr_rewriter::FunctionRewrite, Expr, ExprSchemable, Operator, ScalarUDF},
};
#[derive(Debug)]
pub(crate) struct TimestampRewrite {
    function: ScalarUDF,
}
impl Default for TimestampRewrite {
    fn default() -> Self {
        Self {
            function: crate::timestamp::timestamp_udf(),
        }
    }
}
impl TimestampRewrite {
    fn protect(&self, expr: Expr, schema: &DFSchema) -> Result<(Expr, bool)> {
        // Typed columns only: a CTE string named ts remains a string, and an
        // already protected function remains unchanged on repeated analysis.
        let protect = matches!(expr, Expr::Column(_))
            && matches!(expr.get_type(schema)?, DataType::Timestamp(_, _));
        if protect {
            Ok((self.function.call(vec![expr]), true))
        } else {
            Ok((expr, false))
        }
    }
}
impl FunctionRewrite for TimestampRewrite {
    fn name(&self) -> &str {
        "lume_timestamp_precision"
    }
    fn rewrite(
        &self,
        expr: Expr,
        schema: &DFSchema,
        _config: &ConfigOptions,
    ) -> Result<Transformed<Expr>> {
        match expr {
            Expr::BinaryExpr(mut binary)
                if matches!(
                    binary.op,
                    Operator::Eq
                        | Operator::NotEq
                        | Operator::Lt
                        | Operator::LtEq
                        | Operator::Gt
                        | Operator::GtEq
                ) =>
            {
                let (left, l) = self.protect(*binary.left, schema)?;
                let (right, r) = self.protect(*binary.right, schema)?;
                binary.left = Box::new(left);
                binary.right = Box::new(right);
                Ok(Transformed::new(
                    Expr::BinaryExpr(binary),
                    l || r,
                    datafusion::common::tree_node::TreeNodeRecursion::Continue,
                ))
            }
            Expr::Between(mut between) => {
                let (expr, changed) = self.protect(*between.expr, schema)?;
                between.expr = Box::new(expr);
                Ok(Transformed::new(
                    Expr::Between(between),
                    changed,
                    datafusion::common::tree_node::TreeNodeRecursion::Continue,
                ))
            }
            Expr::InList(mut list) => {
                let (expr, changed) = self.protect(*list.expr, schema)?;
                list.expr = Box::new(expr);
                Ok(Transformed::new(
                    Expr::InList(list),
                    changed,
                    datafusion::common::tree_node::TreeNodeRecursion::Continue,
                ))
            }
            expr => Ok(Transformed::no(expr)),
        }
    }
}
