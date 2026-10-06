//! Small read-only PostgreSQL compatibility catalog for psql and Grafana 13.
use crate::{TiEngine, field_name};
use datafusion::{
    arrow::{array::{Array, StringArray}, datatypes::DataType},
    catalog::memory::MemorySchemaProvider,
    common::{DataFusionError, Result, ScalarValue},
    datasource::MemTable,
    logical_expr::{create_udf, ColumnarValue, Volatility},
    sql::sqlparser::{ast::{Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Statement, Value, VisitMut, VisitorMut}, dialect::GenericDialect, parser::Parser},
};
use serde_json::{json, Value as Json};
use std::{ops::ControlFlow, sync::Arc};
pub use datafusion::common::ScalarValue as Parameter;

fn invalid(s: impl Into<String>) -> DataFusionError { DataFusionError::Plan(s.into()) }
fn literal(s: &str) -> String { format!("'{}'", s.replace('\'', "''")) }
fn expr(s: &str) -> Result<Expr> { Parser::new(&GenericDialect {}).try_with_sql(s).map_err(|e|invalid(e.to_string()))?.parse_expr().map_err(|e|invalid(e.to_string())) }
pub fn type_name(arrow: &str) -> (&'static str, u32) {
    if arrow.starts_with("Timestamp") { ("timestamp with time zone",1184) }
    else if arrow.starts_with("Float") || arrow.starts_with("Decimal") { ("double precision",701) }
    else if arrow.starts_with("Int") || arrow.starts_with("UInt") { ("bigint",20) }
    else if arrow=="Boolean" { ("boolean",16) } else { ("text",25) }
}
fn sql_type(arrow: &str) -> &'static str {
    match type_name(arrow).1 { 1184=>"TIMESTAMP WITH TIME ZONE",701=>"DOUBLE",20=>"BIGINT",16=>"BOOLEAN",_=>"TEXT" }
}
pub async fn register(engine: &TiEngine) -> Result<()> {
    let context=&engine.session.context;
    let catalog=context.catalog("datafusion").ok_or_else(||invalid("missing DataFusion catalog"))?;
    let pg=Arc::new(MemorySchemaProvider::new());
    let info=Arc::new(MemorySchemaProvider::new());
    catalog.register_schema("pg_catalog",pg.clone())?;
    catalog.register_schema("information_schema",info.clone())?;
    let schema=engine.schema(None,None).await?;
    let tables=schema["tables"].as_array().ok_or_else(||invalid("missing schema"))?;
    let mut classes=Vec::new(); let mut attrs=Vec::new(); let mut columns=Vec::new(); let mut table_rows=Vec::new();
    for (i,t) in tables.iter().enumerate() {
        let name=t["name"].as_str().ok_or_else(||invalid("table name"))?; let oid=16384+i;
        let cols=t["columns"].as_array().ok_or_else(||invalid("columns"))?;
        classes.push(format!("SELECT CAST({oid} AS BIGINT) oid, {} relname, CAST(2200 AS BIGINT) relnamespace, 'r' relkind, CAST(0 AS BIGINT) relchecks, false relhasindex, false relhasrules, false relhastriggers, false relrowsecurity, false relforcerowsecurity, false relhasoids, false relispartition, CAST(0 AS BIGINT) reltablespace, CAST(0 AS BIGINT) reloftype, 'p' relpersistence, 'n' relreplident, CAST(0 AS BIGINT) reltoastrelid, CAST(0 AS BIGINT) relam, CAST({} AS BIGINT) relnatts, CAST(NULL AS TEXT) relpartbound",literal(name),cols.len()));
        table_rows.push(format!("SELECT 'ti' table_catalog, 'public' table_schema, {} table_name, 'BASE TABLE' table_type",literal(name)));
        for (j,c) in cols.iter().enumerate() {
            let column=c["name"].as_str().ok_or_else(||invalid("column name"))?;
            let (ty,type_oid)=type_name(c["type"].as_str().unwrap_or("Utf8"));
            let nullable=c["nullable"]!=false;
            attrs.push(format!("SELECT CAST({oid} AS BIGINT) attrelid, {} attname, CAST({type_oid} AS BIGINT) atttypid, CAST({} AS BIGINT) attnum, CAST(-1 AS BIGINT) atttypmod, {} attnotnull, false attisdropped, CAST(0 AS BIGINT) attcollation, '' attidentity, '' attgenerated, 'x' attstorage, CAST(NULL AS TEXT) attcompression, false atthasdef",literal(column),j+1,!nullable));
            columns.push(format!("SELECT 'ti' table_catalog, 'public' table_schema, {} table_name, {} column_name, CAST({} AS BIGINT) ordinal_position, {} is_nullable, {} data_type, 'pg_catalog' udt_schema, {} udt_name, CAST(NULL AS TEXT) column_default",literal(name),literal(column),j+1,literal(if nullable{"YES"}else{"NO"}),literal(ty),literal(match type_oid{1184=>"timestamptz",701=>"float8",20=>"int8",16=>"bool",_=>"text"})));
        }
    }
    let types=[(16,"bool","boolean"),(20,"int8","bigint"),(21,"int2","smallint"),(23,"int4","integer"),(25,"text","text"),(26,"oid","oid"),(700,"float4","real"),(701,"float8","double precision"),(1043,"varchar","character varying"),(1114,"timestamp","timestamp without time zone"),(1184,"timestamptz","timestamp with time zone")];
    let type_rows=types.iter().map(|(oid,n,_)|format!("SELECT CAST({oid} AS BIGINT) oid, {} typname, CAST(11 AS BIGINT) typnamespace, CAST(0 AS BIGINT) typcollation, CAST(0 AS BIGINT) typbasetype, CAST(-1 AS BIGINT) typtypmod, false typnotnull, 'b' typtype, CAST(0 AS BIGINT) typelem, CAST(0 AS BIGINT) typrelid, CAST(0 AS BIGINT) typarray",literal(n))).collect::<Vec<_>>();
    let definitions=vec![
        ("pg_catalog","pg_namespace",vec!["SELECT CAST(11 AS BIGINT) oid, 'pg_catalog' nspname UNION ALL SELECT CAST(2200 AS BIGINT), 'public' UNION ALL SELECT CAST(13207 AS BIGINT), 'information_schema'".into()]),
        ("pg_catalog","pg_type",type_rows),
        ("pg_catalog","pg_class",classes),
        ("pg_catalog","pg_attribute",attrs),
        ("information_schema","tables",table_rows),
        ("information_schema","columns",columns),
        ("pg_catalog","pg_roles",vec!["SELECT CAST(0 AS BIGINT) oid, CAST(NULL AS TEXT) rolname WHERE false".into()]),
        ("pg_catalog","pg_policy",vec!["SELECT CAST(NULL AS TEXT) polname, false polpermissive, ARRAY[CAST(0 AS BIGINT)] polroles, CAST(NULL AS TEXT) polqual, CAST(NULL AS TEXT) polwithcheck, CAST(0 AS BIGINT) polrelid, CAST(NULL AS TEXT) polcmd WHERE false".into()]),
        ("pg_catalog","pg_statistic_ext",vec!["SELECT CAST(0 AS BIGINT) oid, CAST(0 AS BIGINT) stxrelid, CAST(0 AS BIGINT) stxnamespace, CAST(NULL AS TEXT) stxname, ARRAY[CAST(NULL AS TEXT)] stxkind, CAST(-1 AS BIGINT) stxstattarget WHERE false".into()]),
        ("pg_catalog","pg_publication",vec!["SELECT CAST(0 AS BIGINT) oid, CAST(NULL AS TEXT) pubname, false puballtables WHERE false".into()]),
        ("pg_catalog","pg_publication_namespace",vec!["SELECT CAST(0 AS BIGINT) pnpubid, CAST(0 AS BIGINT) pnnspid WHERE false".into()]),
        ("pg_catalog","pg_publication_rel",vec!["SELECT CAST(0 AS BIGINT) prpubid, CAST(0 AS BIGINT) prrelid, CAST(NULL AS TEXT) prqual, ARRAY[CAST(NULL AS SMALLINT)] prattrs WHERE false".into()]),
        ("pg_catalog","pg_extension",vec!["SELECT CAST(NULL AS TEXT) extname, CAST(NULL AS TEXT) extversion WHERE false".into()]),
        ("pg_catalog","pg_am",vec!["SELECT CAST(0 AS BIGINT) oid, CAST(NULL AS TEXT) amname WHERE false".into()]),
        ("pg_catalog","pg_attrdef",vec!["SELECT CAST(0 AS BIGINT) adrelid, CAST(0 AS BIGINT) adnum, CAST(NULL AS TEXT) adbin WHERE false".into()]),
        ("pg_catalog","pg_collation",vec!["SELECT CAST(0 AS BIGINT) oid, CAST(NULL AS TEXT) collname WHERE false".into()]),
        ("pg_catalog","pg_inherits",vec!["SELECT CAST(0 AS BIGINT) inhrelid, CAST(0 AS BIGINT) inhparent, CAST(0 AS BIGINT) inhseqno, false inhdetachpending WHERE false".into()]),
    ];
    for (namespace,name,parts) in definitions {
        let mut batches=Vec::new(); let mut schema=None;
        for part in parts.chunks(32) {
            let frame=context.sql(&part.join(" UNION ALL ")).await?;
            schema.get_or_insert_with(||Arc::new(frame.schema().as_arrow().clone()));
            batches.extend(frame.collect().await?);
        }
        let schema=schema.ok_or_else(||invalid("empty catalog"))?;
        let table=Arc::new(MemTable::try_new(schema,vec![batches])?);
        use datafusion::catalog::SchemaProvider;
        if namespace=="pg_catalog" { pg.register_table(name.into(),table.clone())?; context.register_table(name,table)?; }
        else { info.register_table(name.into(),table)?; }
    }
    context.register_udf(create_udf("quote_ident",vec![DataType::Utf8],DataType::Utf8,Volatility::Immutable,Arc::new(|args|{
        let values=ColumnarValue::values_to_arrays(args)?;
        let a=values[0].as_any().downcast_ref::<StringArray>().ok_or_else(||invalid("quote_ident argument"))?;
        let out=(0..a.len()).map(|i|if a.is_null(i){None}else{Some(quote_ident(a.value(i)))}).collect::<Vec<_>>();
        Ok(ColumnarValue::Array(Arc::new(StringArray::from(out))))
    })));
    Ok(())
}
pub fn quote_ident(s: &str) -> String {
    let simple=!s.is_empty() && s.bytes().enumerate().all(|(i,c)| c==b'_' || c.is_ascii_lowercase() || i>0&&c.is_ascii_digit());
    if simple && !["user","table","column","select","time","order","group","where","from"].contains(&s) { s.into() }
    else { format!("\"{}\"",s.replace('"',"\"\"")) }
}
const SEARCH_PATH: &str = r#"SELECT CASE WHEN trim(s[i]) = '"$user"' THEN user ELSE trim(s[i]) END FROM generate_series(array_lower(string_to_array(current_setting('search_path'),','),1), array_upper(string_to_array(current_setting('search_path'),','),1)) as i, string_to_array(current_setting('search_path'),',') s"#;
fn canonical(sql:&str)->Result<String> { let s=Parser::parse_sql(&GenericDialect{},sql).map_err(|e|invalid(e.to_string()))?; if s.len()!=1{return Err(invalid("one read-only statement required"));} Ok(s[0].to_string()) }
fn args(f:&datafusion::sql::sqlparser::ast::Function)->Vec<Expr> {
    match &f.args{FunctionArguments::List(l)=>l.args.iter().filter_map(|a|match a{FunctionArg::Unnamed(FunctionArgExpr::Expr(e))=>Some(e.clone()),_=>None}).collect(),_=>vec![]}
}
// Exact psql 16 catalog templates. Columns have no defaults or collations;
// TI has no policies, extended statistics or publications. Rewrite those
// unsupported correlated probes using the actual attribute rows or typed empty
// replies. Arbitrary client SQL is still planned normally.
const EMPTY_CATALOG_PROBES: &[(&str,&str)] = &[
    (r#"SELECT a.attname, pg_catalog.format_type(a.atttypid,a.atttypmod), (SELECT pg_catalog.pg_get_expr(d.adbin,d.adrelid,true) FROM pg_catalog.pg_attrdef d WHERE d.adrelid=a.attrelid AND d.adnum=a.attnum AND a.atthasdef), a.attnotnull, (SELECT c.collname FROM pg_catalog.pg_collation c,pg_catalog.pg_type t WHERE c.oid=a.attcollation AND t.oid=a.atttypid AND a.attcollation<>t.typcollation) AS attcollation, a.attidentity, a.attgenerated FROM pg_catalog.pg_attribute a WHERE a.attrelid='{oid}' AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum"#, "SELECT a.attname, pg_catalog.format_type(a.atttypid,a.atttypmod) AS type, CAST(NULL AS TEXT) AS default_expression, a.attnotnull, CAST(NULL AS TEXT) AS attcollation, a.attidentity, a.attgenerated FROM pg_catalog.pg_attribute a WHERE a.attrelid='{oid}' AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum"),
    (r#"SELECT pol.polname, pol.polpermissive, CASE WHEN pol.polroles = '{0}' THEN NULL ELSE pg_catalog.array_to_string(array(select rolname from pg_catalog.pg_roles where oid = any (pol.polroles) order by 1),',') END, pg_catalog.pg_get_expr(pol.polqual, pol.polrelid), pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid), CASE pol.polcmd WHEN 'r' THEN 'SELECT' WHEN 'a' THEN 'INSERT' WHEN 'w' THEN 'UPDATE' WHEN 'd' THEN 'DELETE' END AS cmd FROM pg_catalog.pg_policy pol WHERE pol.polrelid = '{oid}' ORDER BY 1"#, "SELECT CAST(NULL AS TEXT) AS polname, false AS polpermissive, CAST(NULL AS TEXT) AS roles, CAST(NULL AS TEXT) AS using_expression, CAST(NULL AS TEXT) AS check_expression, CAST(NULL AS TEXT) AS cmd WHERE false"),
    (r#"SELECT oid, stxrelid::pg_catalog.regclass, stxnamespace::pg_catalog.regnamespace::pg_catalog.text AS nsp, stxname, pg_catalog.pg_get_statisticsobjdef_columns(oid) AS columns, 'd' = any(stxkind) AS ndist_enabled, 'f' = any(stxkind) AS deps_enabled, 'm' = any(stxkind) AS mcv_enabled, stxstattarget FROM pg_catalog.pg_statistic_ext WHERE stxrelid='{oid}' ORDER BY nsp, stxname"#, "SELECT CAST(0 AS BIGINT) AS oid, CAST(NULL AS TEXT) AS stxrelid, CAST(NULL AS TEXT) AS nsp, CAST(NULL AS TEXT) AS stxname, CAST(NULL AS TEXT) AS columns, false AS ndist_enabled, false AS deps_enabled, false AS mcv_enabled, CAST(-1 AS BIGINT) AS stxstattarget WHERE false"),
    (r#"SELECT pubname, NULL, NULL FROM pg_catalog.pg_publication p JOIN pg_catalog.pg_publication_namespace pn ON p.oid=pn.pnpubid JOIN pg_catalog.pg_class pc ON pc.relnamespace=pn.pnnspid WHERE pc.oid='{oid}' AND pg_catalog.pg_relation_is_publishable('{oid}') UNION SELECT pubname, pg_get_expr(pr.prqual,c.oid), (CASE WHEN pr.prattrs IS NOT NULL THEN (SELECT string_agg(attname, ', ') FROM pg_catalog.generate_series(0, pg_catalog.array_upper(pr.prattrs::pg_catalog.int2[],1)) s, pg_catalog.pg_attribute WHERE attrelid=pr.prrelid AND attnum=prattrs[s]) ELSE NULL END) FROM pg_catalog.pg_publication p JOIN pg_catalog.pg_publication_rel pr ON p.oid=pr.prpubid JOIN pg_catalog.pg_class c ON c.oid=pr.prrelid WHERE pr.prrelid='{oid}' UNION SELECT pubname,NULL,NULL FROM pg_catalog.pg_publication p WHERE p.puballtables AND pg_catalog.pg_relation_is_publishable('{oid}') ORDER BY 1"#, "SELECT CAST(NULL AS TEXT) AS pubname, CAST(NULL AS TEXT) AS filter, CAST(NULL AS TEXT) AS columns WHERE false"),
];
struct Compatibility<'a>{hints:&'a [String],search_path:String}
impl VisitorMut for Compatibility<'_> {
    type Break=DataFusionError;
    fn pre_visit_query(&mut self,q:&mut datafusion::sql::sqlparser::ast::Query)->ControlFlow<Self::Break>{
        if q.to_string()==self.search_path { if let Ok(mut s)=Parser::parse_sql(&GenericDialect{},"SELECT 'public'"){if let Statement::Query(query)=s.remove(0){*q=*query;}} }
        let rendered=q.to_string();
        for candidate in rendered.split('\'').skip(1).step_by(2).filter(|s|s.parse::<u32>().is_ok()) {
            for (probe,empty) in EMPTY_CATALOG_PROBES {
                match canonical(&probe.replace("{oid}",candidate)) {
                    Ok(expected) if rendered==expected=>{
                        match Parser::parse_sql(&GenericDialect{},&empty.replace("{oid}",candidate)) {
                            Ok(mut statements)=>if let Statement::Query(query)=statements.remove(0){*q=*query;},
                            Err(e)=>return ControlFlow::Break(invalid(e.to_string())),
                        }
                        return ControlFlow::Continue(());
                    },
                    Err(e)=>return ControlFlow::Break(e),
                    _=>{},
                }
            }
        }
        ControlFlow::Continue(())
    }
    fn post_visit_expr(&mut self,e:&mut Expr)->ControlFlow<Self::Break>{
        let result=(||->Result<()>{
            match e {
                Expr::InSubquery{expr:inner,subquery,negated} if subquery.to_string()=="SELECT 'public'"=>{
                    *e=expr(&format!("{} {}IN ('public')",inner,if *negated{"NOT "}else{""}))?;
                },
                Expr::Collate{expr:inner,collation} if collation.to_string()=="pg_catalog.default"=>{*e=*inner.clone();},
                Expr::BinaryOp{left,op:datafusion::sql::sqlparser::ast::BinaryOperator::PGCustomBinaryOperator(parts),right} if parts==&["pg_catalog".to_string(),"~".to_string()]=>{
                    *e=expr(&format!("regexp_like({left},{right})"))?;
                },
                Expr::Value(v) if matches!(&v.value,Value::Placeholder(_))=>{
                    if let Value::Placeholder(p)=&v.value { if let Some(t)=p.strip_prefix('$').and_then(|n|n.parse::<usize>().ok()).and_then(|n|n.checked_sub(1)).and_then(|n|self.hints.get(n)).filter(|t|!t.is_empty()){ *e=expr(&format!("CAST({p} AS {})",sql_type(t)))?; } }
                },
                Expr::Cast{data_type,..}=>{
                    let t=data_type.to_string().to_ascii_lowercase();
                    if t.starts_with("pg_catalog.") || ["regtype","regclass","oid","name"].contains(&t.as_str()) {
                        let t=t.trim_start_matches("pg_catalog.");
                        let mapped=match t{"oid"=>"BIGINT","bool"=>"BOOLEAN","int2"=>"SMALLINT","int4"=>"INT","int8"=>"BIGINT",_=>"TEXT"};
                        *data_type=match expr(&format!("CAST(NULL AS {mapped})"))?{Expr::Cast{data_type,..}=>data_type,_=>unreachable!()};
                    }
                },
                Expr::Function(f)=>{
                    let name=f.name.to_string().to_ascii_lowercase(); let name=name.trim_start_matches("pg_catalog.");
                    let a=args(f);
                    let replacement=match name {
                        "version" if a.is_empty()=>Some(literal("PostgreSQL 16.6 (Lume read-only compatibility)")),
                        "current_schema" if a.is_empty()=>Some(literal("public")),
                        "current_database" if a.is_empty()=>Some(literal("ti")),
                        "current_setting" if a.len()==1=>match &a[0]{Expr::Value(v)=>match &v.value{Value::SingleQuotedString(s)=>Some(literal(match s.as_str(){"server_version_num"=>"160006","server_version"=>"16.6","search_path"=>"public","TimeZone"=>"UTC","transaction_read_only"|"default_transaction_read_only"=>"on",_=>return Err(invalid(format!("unknown setting {s}")))})),_=>None},_=>None},
                        "pg_table_is_visible" if a.len()==1=>Some(format!("{} >= 16384",a[0])),
                        "pg_relation_is_publishable" if a.len()==1=>Some("false".into()),
                        "pg_get_statisticsobjdef_columns" if a.len()==1=>Some("CAST(NULL AS TEXT)".into()),
                        "pg_get_expr" if a.len()>=2=>Some("CAST(NULL AS TEXT)".into()),
                        "format_type" if a.len()==2=>Some(format!("CASE {} WHEN 1184 THEN 'timestamp with time zone' WHEN 1114 THEN 'timestamp without time zone' WHEN 701 THEN 'double precision' WHEN 700 THEN 'real' WHEN 20 THEN 'bigint' WHEN 23 THEN 'integer' WHEN 21 THEN 'smallint' WHEN 16 THEN 'boolean' ELSE 'text' END",a[0])),
                        "parse_ident" if a.len()==1=>match &a[0]{Expr::Value(v)=>match &v.value{Value::SingleQuotedString(s)=>{
                            let mut p=Parser::new(&GenericDialect{}).try_with_sql(s).map_err(|e|invalid(e.to_string()))?;
                            let id=p.parse_object_name(false).map_err(|e|invalid(e.to_string()))?;
                            Some(format!("ARRAY[{}]",id.0.iter().map(|p|literal(&p.to_string().trim_matches('"').replace("\"\"","\""))).collect::<Vec<_>>().join(",")))
                        },_=>None},_=>None},
                        _=>None,
                    };
                    if let Some(s)=replacement{*e=expr(&s)?;} else if f.name.to_string().starts_with("pg_catalog.") {
                        f.name=match expr(&format!("{name}()"))?{Expr::Function(f)=>f.name,_=>unreachable!()};
                    }
                },
                _=>{}
            }; Ok(())
        })();
        match result{Ok(())=>ControlFlow::Continue(()),Err(e)=>ControlFlow::Break(e)}
    }
}
pub fn normalize(sql:&str,hints:&[String])->Result<String> {
    if sql.len()>crate::MAX_BYTES{return Err(invalid("SQL exceeds 64 KiB"));}
    let mut statements=Parser::parse_sql(&GenericDialect{},sql).map_err(|e|invalid(e.to_string()))?;
    if statements.len()!=1 || !matches!(&statements[0],Statement::Query(_) | Statement::Explain{..}) {return Err(invalid("one read-only statement required; DML/DDL rejected"));}
    let mut rewrite=Compatibility{hints,search_path:canonical(SEARCH_PATH)?};
    if let ControlFlow::Break(e)=statements[0].visit(&mut rewrite){return Err(e);}
    Ok(statements[0].to_string())
}
pub async fn describe(engine:&TiEngine,sql:&str,hints:&[String])->Result<Json>{
    let sql=normalize(sql,hints)?;
    let frame=engine.session.prepare(&sql).await?;
    let plan=frame.clone().into_optimized_plan()?;
    let mut parameters=plan.get_parameter_types()?;
    use datafusion::common::tree_node::{TreeNode,TreeNodeRecursion};
    plan.apply_with_subqueries(|node|{
        node.apply_expressions(|expression|expression.apply(|expression|{
            let cast=match expression {
                datafusion::logical_expr::Expr::Cast(c)=>Some((&c.expr,c.field.data_type())),
                datafusion::logical_expr::Expr::TryCast(c)=>Some((&c.expr,c.field.data_type())),
                _=>None,
            };
            if let Some((inner,ty))=cast {
                if let datafusion::logical_expr::Expr::Placeholder(p)=inner.as_ref() {
                    let entry=parameters.entry(p.id.clone()).or_default();
                    if entry.is_none(){*entry=Some(ty.clone());}
                }
            }
            Ok(TreeNodeRecursion::Continue)
        }))
    })?;
    let mut types=Vec::new();
    for i in 1..=parameters.len(){
        let ty=parameters.get(&format!("${i}")).and_then(|t|t.as_ref()).ok_or_else(||invalid(format!("cannot infer parameter ${i}; add an explicit cast")))?;
        types.push(ty.to_string());
    }
    let columns=frame.schema().fields().iter().map(|f|{
        let name=engine.session.catalog.field(f.name()).map(field_name).unwrap_or_else(||f.name().clone());
        json!({"name":name,"type":f.data_type().to_string()})
    }).collect::<Vec<_>>();
    Ok(json!({"sql":sql,"parameters":types,"columns":columns}))
}
pub async fn query(engine:&TiEngine,sql:&str,parameters:Vec<ScalarValue>)->Result<Json>{
    let sql=normalize(sql,&[])?;
    Ok(engine.query_with_parameters(&sql,crate::MAX_ROWS,parameters).await?.0)
}
