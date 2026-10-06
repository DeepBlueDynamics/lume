//! SQL AST rewrites happen before DataFusion's list/string type coercion.
//! Source equality/IN become array membership; numeric column literals are
//! quantized through the same checked helpers as ingest, including residual filters.
use crate::catalog::SqlCatalog;
use datafusion::common::{DataFusionError, Result};
use datafusion::sql::sqlparser::{
    ast::{
        BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, ObjectName,
        Statement, UnaryOperator, Value, VisitMut, VisitorMut,
    },
    dialect::GenericDialect,
    parser::Parser,
};
use std::ops::ControlFlow;

fn name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Identifier(i) => Some(&i.value),
        Expr::CompoundIdentifier(ids) => ids.last().map(|i| i.value.as_str()),
        Expr::Nested(e) => name(e),
        _ => None,
    }
}
fn number(e: &Expr) -> Option<f64> {
    match e {
        Expr::Value(v) => match &v.value {
            Value::Number(n, _) => n.parse().ok(),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => number(expr).map(|v| -v),
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        }
        | Expr::Nested(expr) => number(expr),
        _ => None,
    }
}
fn parsed_expr(sql: &str) -> Result<Expr> {
    Parser::new(&GenericDialect {})
        .try_with_sql(sql)
        .map_err(|e| DataFusionError::Plan(e.to_string()))?
        .parse_expr()
        .map_err(|e| DataFusionError::Plan(e.to_string()))
}
fn quantize(e: &mut Expr, scale: u8) -> Result<()> {
    if let Some(value) = number(e) {
        // Valid SQL outside the fixed-point domain remains a residual expression.
        if let Ok(fixed) = ti_contracts::to_fixed(value, scale) {
            let value =
                ti_contracts::from_fixed(fixed, scale).map_err(crate::catalog::core_error)?;
            *e = parsed_expr(&value.to_string())?;
        }
    }
    Ok(())
}
fn args(f: &datafusion::sql::sqlparser::ast::Function) -> Option<Vec<&Expr>> {
    match &f.args {
        FunctionArguments::List(l) => l
            .args
            .iter()
            .map(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}
struct Rewriter<'a> {
    catalog: &'a SqlCatalog,
}
impl Rewriter<'_> {
    fn rewrite(&self, e: &mut Expr) -> Result<()> {
        match e {
            Expr::BinaryOp { left, op, right } => {
                let l = name(left).map(str::to_owned);
                let r = name(right).map(str::to_owned);
                if matches!(op, BinaryOperator::Eq | BinaryOperator::NotEq) {
                    let source = l.as_ref().is_some_and(|n| self.catalog.source_column(n))
                        || r.as_ref().is_some_and(|n| self.catalog.source_column(n));
                    if source {
                        let (array, value) =
                            if l.as_ref().is_some_and(|n| self.catalog.source_column(n)) {
                                (&**left, &**right)
                            } else {
                                (&**right, &**left)
                            };
                        // Only scalar literal equality is special; list-vs-list SQL stays standard.
                        if matches!(value, Expr::Value(_)) {
                            let sql = format!("array_has({array}, {value})");
                            *e = parsed_expr(&if *op == BinaryOperator::NotEq {
                                format!("NOT ({sql})")
                            } else {
                                sql
                            })?;
                            return Ok(());
                        }
                    }
                }
                if matches!(
                    op,
                    BinaryOperator::Eq
                        | BinaryOperator::NotEq
                        | BinaryOperator::Lt
                        | BinaryOperator::LtEq
                        | BinaryOperator::Gt
                        | BinaryOperator::GtEq
                ) {
                    if let Some(ti_contracts::FieldKind::Bsi { scale }) = l
                        .as_deref()
                        .and_then(|n| self.catalog.field(n))
                        .map(|f| &f.kind)
                    {
                        quantize(right, *scale)?;
                    }
                    if let Some(ti_contracts::FieldKind::Bsi { scale }) = r
                        .as_deref()
                        .and_then(|n| self.catalog.field(n))
                        .map(|f| &f.kind)
                    {
                        quantize(left, *scale)?;
                    }
                }
            }
            Expr::InList {
                expr,
                list,
                negated,
            } => {
                if name(expr).is_some_and(|n| self.catalog.source_column(n)) {
                    let terms = list
                        .iter()
                        .map(|value| {
                            if matches!(value,Expr::Value(v) if v.value==Value::Null) {
                                "NULL".into()
                            } else {
                                format!("array_has({expr}, {value})")
                            }
                        })
                        .collect::<Vec<String>>();
                    let sql = if terms.is_empty() {
                        "FALSE".into()
                    } else {
                        terms.join(" OR ")
                    };
                    *e = parsed_expr(&if *negated {
                        format!("NOT ({sql})")
                    } else {
                        format!("({sql})")
                    })?;
                } else if let Some(ti_contracts::FieldKind::Bsi { scale }) = name(expr)
                    .and_then(|n| self.catalog.field(n))
                    .map(|f| &f.kind)
                {
                    for value in list {
                        quantize(value, *scale)?;
                    }
                }
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                if let Some(ti_contracts::FieldKind::Bsi { scale }) = name(expr)
                    .and_then(|n| self.catalog.field(n))
                    .map(|f| &f.kind)
                {
                    quantize(low, *scale)?;
                    quantize(high, *scale)?;
                }
            }
            Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("match") => {
                if let Some(a) = args(f) {
                    if a.len() == 2
                        && name(a[0]).is_some_and(|n| ["notes", "logbook", "alerts"].contains(&n))
                    {
                        let kind = name(a[0]).expect("checked");
                        let qualifier = match a[0] {
                            Expr::CompoundIdentifier(ids) => ids[..ids.len() - 1]
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join("."),
                            _ => String::new(),
                        };
                        let prefix = if qualifier.is_empty() {
                            String::new()
                        } else {
                            format!("{qualifier}.")
                        };
                        *e = parsed_expr(&format!(
                            "ti_match({prefix}vessel, {prefix}ts, '{kind}', {})",
                            a[1]
                        ))?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
impl VisitorMut for Rewriter<'_> {
    type Break = DataFusionError;
    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        match self.rewrite(e) {
            Ok(()) => ControlFlow::Continue(()),
            Err(e) => ControlFlow::Break(e),
        }
    }
}
#[derive(Default)]
struct Tables(Vec<String>);
impl VisitorMut for Tables {
    type Break = ();
    fn pre_visit_relation(&mut self, name: &mut ObjectName) -> ControlFlow<()> {
        self.0.push(name.to_string());
        ControlFlow::Continue(())
    }
}
fn read_only(statement: &Statement) -> bool {
    match statement {
        Statement::Query(_) => true,
        Statement::Explain { statement, .. } => read_only(statement),
        _ => false,
    }
}
pub fn rewrite_sql(sql: &str, catalog: &SqlCatalog) -> Result<String> {
    let mut statements = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|e| DataFusionError::Plan(e.to_string()))?;
    if statements.len() != 1 {
        return Err(DataFusionError::Plan(
            "one read-only statement is required; telemetry/docs/raw/catalog tables are derived"
                .into(),
        ));
    }
    let statement = &mut statements[0];
    if !read_only(statement) {
        let mut tables = Tables::default();
        let _ = statement.visit(&mut tables);
        let names = if tables.0.is_empty() {
            "telemetry/docs/raw/catalog".into()
        } else {
            tables.0.join(", ")
        };
        return Err(DataFusionError::Plan(format!(
            "table {names} is derived and read-only; DML/DDL is rejected"
        )));
    }
    let mut tables = Tables::default();
    let _ = statement.visit(&mut tables);
    // Queries without an indexed/raw source retain ordinary DataFusion types.
    // This also avoids treating an unrelated CTE's string alias "ts" as time.
    if !tables.0.iter().any(|name| {
        let name = name.rsplit('.').next().unwrap_or(name).trim_matches('"');
        name.eq_ignore_ascii_case("telemetry") || name.eq_ignore_ascii_case("raw")
    }) {
        return Ok(statement.to_string());
    }
    if let ControlFlow::Break(e) = statement.visit(&mut Rewriter { catalog }) {
        return Err(e);
    }
    Ok(statement.to_string())
}
