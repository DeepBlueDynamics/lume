//! Read-only typed PostgreSQL simple and extended query transport.
use crate::ti_http::TiServer;
use async_trait::async_trait;
use futures::{stream, StreamExt};
use pgwire::{
    api::{
        portal::{Format, Portal},
        query::{SimpleQueryHandler, ExtendedQueryHandler},
        results::{FieldFormat, FieldInfo, QueryResponse, Response},
        stmt::QueryParser, store::PortalStore,
        ClientInfo, ClientPortalStore, PgWireServerHandlers, Type,
    },
    error::{ErrorInfo, PgWireError, PgWireResult},
    messages::data::DataRow,
};
use serde_json::Value;
use std::{net::{SocketAddr,TcpListener},sync::Arc};
pub(crate) fn error(code:&str,message:impl Into<String>)->PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new("ERROR".into(),code.into(),message.into())))
}
fn result_type(arrow:&str)->Type {
    Type::from_oid(ti_sql::postgres::type_name(arrow).1).unwrap_or(Type::TEXT)
}
fn parameter_type(arrow:&str)->Type {
    match arrow {
        "Int16"=>Type::INT2, "Int32"=>Type::INT4, "Float32"=>Type::FLOAT4,
        value if value.starts_with("Timestamp") && value.contains("None")=>Type::TIMESTAMP,
        _=>result_type(arrow),
    }
}
fn hint_type(t:&Type)->PgWireResult<String> {
    Ok(match *t {
        Type::TIMESTAMPTZ|Type::TIMESTAMP=>"Timestamp(Microsecond, Some(\"UTC\"))",
        Type::INT8|Type::INT4|Type::INT2|Type::OID=>"Int64",
        Type::FLOAT8|Type::FLOAT4=>"Float64",
        Type::BOOL=>"Boolean",
        Type::TEXT|Type::VARCHAR|Type::NAME|Type::UNKNOWN=>"Utf8",
        _=>return Err(error("0A000",format!("Unsupported parameter type {t}"))),
    }.into())
}
#[derive(Clone)]
struct Statement {
    sql:String,
    columns:Vec<(String,Type)>,
    parameters:Vec<Type>,
}
struct Parser {server:Arc<TiServer>}
#[async_trait]
impl QueryParser for Parser {
    type Statement=Statement;
    async fn parse_sql<C>(&self,_client:&C,sql:&str,types:&[Option<Type>])->PgWireResult<Option<Statement>>
    where C:ClientInfo+Unpin+Send+Sync {
        if sql.trim().is_empty(){return Ok(None);}
        let hints=types.iter().map(|t|t.as_ref().filter(|t|**t!=Type::UNKNOWN).map(hint_type).transpose().map(|t|t.unwrap_or_default())).collect::<PgWireResult<Vec<_>>>()?;
        let server=self.server.clone();let sql=sql.to_owned();
        let result=tokio::task::spawn_blocking(move||server.pg_describe(&sql,&hints)).await.map_err(|e|error("XX000",e.to_string()))?.map_err(|e|error("22000",e))?;
        let inferred=result["parameters"].as_array().ok_or_else(||error("XX000","Missing parameters"))?;
        if !types.is_empty()&&types.len()>inferred.len(){return Err(error("08P01","Too many parameter types"));}
        let parameters=inferred.iter().enumerate().map(|(i,v)|types.get(i).and_then(|t|t.clone()).filter(|t|*t!=Type::UNKNOWN).unwrap_or_else(||parameter_type(v.as_str().unwrap_or("Utf8")))).collect();
        let columns=columns(&result)?;
        Ok(Some(Statement{sql:result["sql"].as_str().ok_or_else(||error("XX000","Missing SQL"))?.into(),columns,parameters}))
    }
    fn get_parameter_types(&self,stmt:&Statement)->PgWireResult<Vec<Type>>{Ok(stmt.parameters.clone())}
    fn get_result_schema(&self,stmt:&Statement,format:Option<&Format>)->PgWireResult<Vec<FieldInfo>>{
        schema(&stmt.columns,format.unwrap_or(&Format::UnifiedText))
    }
}
fn columns(result:&Value)->PgWireResult<Vec<(String,Type)>> {
    result["columns"].as_array().ok_or_else(||error("XX000","Missing columns"))?.iter().map(|c|{
        Ok((c["name"].as_str().ok_or_else(||error("XX000","Column name"))?.into(),result_type(c["type"].as_str().unwrap_or("Utf8"))))
    }).collect()
}
fn schema(columns:&[(String,Type)],format:&Format)->PgWireResult<Vec<FieldInfo>> {
    if let Format::Individual(codes)=format {
        if codes.len()!=columns.len()||codes.iter().any(|c|!matches!(c,0|1)){return Err(error("08P01","Invalid result formats"));}
    }
    Ok(columns.iter().enumerate().map(|(i,(name,t))|FieldInfo::new(name.clone(),None,None,t.clone(),format.format_for(i))
        .with_type_size(match *t{Type::BOOL=>1,Type::INT8|Type::FLOAT8|Type::TIMESTAMPTZ=>8,_=>-1})).collect())
}
struct Handler {server:Arc<TiServer>,auth:Arc<crate::ti_pg_auth::AuthConfig>}
impl Handler {
    async fn execute(&self,sql:&str,parameters:Vec<ti_sql::postgres::Parameter>,format:&Format)->PgWireResult<Response>{
        let server=self.server.clone();let sql=sql.to_owned();
        let result=tokio::task::spawn_blocking(move||server.pg_query(&sql,parameters)).await.map_err(|e|error("XX000",e.to_string()))?.map_err(|e|error("22000",e))?;
        encode(result,format)
    }
}
fn encode(result:Value,format:&Format)->PgWireResult<Response>{
    if result["truncated"]==true{return Err(error("54000","Result exceeds 500 rows or 64 KiB; aggregate results or narrow the time range."));}
    let columns=columns(&result)?;let schema=Arc::new(schema(&columns,format)?);
    let mut bytes=32+columns.iter().map(|(n,_)|n.len()+19).sum::<usize>();let mut rows=Vec::new();
    for row in result["rows"].as_array().ok_or_else(||error("XX000","Missing rows"))? {
        let mut buffer=Vec::new();
        for (i,(name,t)) in columns.iter().enumerate(){
            let v=&row[name];
            if v.is_null(){buffer.extend_from_slice(&(-1i32).to_be_bytes());continue;}
            let data=encode_value(v,t,format.format_for(i))?;
            buffer.extend_from_slice(&(data.len() as i32).to_be_bytes());buffer.extend_from_slice(&data);
        }
        bytes+=7+buffer.len();if bytes>ti_sql::MAX_BYTES{return Err(error("54000","Postgres result exceeds 64 KiB; aggregate results or narrow the time range."));}
        rows.push(DataRow::new(buffer.as_slice().into(),columns.len() as i16));
    }
    Ok(Response::Query(QueryResponse::new(schema,stream::iter(rows).map(Ok))))
}
fn encode_value(v:&Value,t:&Type,format:FieldFormat)->PgWireResult<Vec<u8>> {
    if format==FieldFormat::Text {
        return Ok(match *t {
            Type::BOOL=>if v.as_bool().ok_or_else(||error("XX000","Invalid boolean"))?{"t".into()}else{"f".into()},
            Type::INT8=>v.as_i64().or_else(||v.as_u64().and_then(|n|i64::try_from(n).ok())).ok_or_else(||error("22003","Integer exceeds int8"))?.to_string(),
            _=>v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()),
        }.into_bytes());
    }
    Ok(match *t {
        Type::BOOL=>vec![u8::from(v.as_bool().ok_or_else(||error("XX000","Invalid boolean"))?)],
        Type::INT8=>v.as_i64().or_else(||v.as_u64().and_then(|n|i64::try_from(n).ok())).ok_or_else(||error("22003","Integer exceeds int8"))?.to_be_bytes().to_vec(),
        Type::FLOAT8=>v.as_f64().or_else(||v.as_str().and_then(|s|s.parse::<f64>().ok())).ok_or_else(||error("XX000","Invalid float"))?.to_be_bytes().to_vec(),
        Type::TIMESTAMPTZ=>{
            let text=v.as_str().ok_or_else(||error("XX000","Invalid timestamp"))?;
            let ts=chrono::DateTime::parse_from_rfc3339(text).or_else(|_|chrono::NaiveDateTime::parse_from_str(text,"%Y-%m-%dT%H:%M:%S%.f").map(|t|t.and_utc().fixed_offset())).map_err(|e|error("22007",e.to_string()))?;
            ts.timestamp_micros().checked_sub(946684800000000).ok_or_else(||error("22008","Timestamp overflow"))?.to_be_bytes().to_vec()
        },
        _=>v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()).into_bytes(),
    })
}
fn parameters(portal:&Portal<Statement>)->PgWireResult<Vec<ti_sql::postgres::Parameter>> {
    use ti_sql::postgres::Parameter as P;
    let types=&portal.statement.statement.parameters;
    if portal.parameter_len()!=types.len(){return Err(error("08P01","Bind parameter count mismatch"));}
    if let Format::Individual(codes)=&portal.parameter_format {if codes.len()!=types.len()||codes.iter().any(|c|!matches!(c,0|1)){return Err(error("08P01","Invalid parameter formats"));}}
    let mut out=Vec::new();
    for (i,t) in types.iter().enumerate(){
        out.push(match *t{
            Type::BOOL=>P::Boolean(portal.parameter::<bool>(i,t)?),
            Type::INT8=>P::Int64(portal.parameter::<i64>(i,t)?),
            Type::INT4=>P::Int64(portal.parameter::<i32>(i,t)?.map(i64::from)),
            Type::INT2=>P::Int64(portal.parameter::<i16>(i,t)?.map(i64::from)),
            Type::OID=>P::Int64(portal.parameter::<u32>(i,t)?.map(i64::from)),
            Type::FLOAT8=>P::Float64(portal.parameter::<f64>(i,t)?),
            Type::FLOAT4=>P::Float64(portal.parameter::<f32>(i,t)?.map(f64::from)),
            Type::TEXT|Type::VARCHAR|Type::NAME=>P::Utf8(portal.parameter::<String>(i,t)?),
            Type::TIMESTAMPTZ=>P::TimestampMicrosecond(portal.parameter::<chrono::DateTime<chrono::FixedOffset>>(i,t)?.map(|v|v.timestamp_micros()),Some("UTC".into())),
            Type::TIMESTAMP=>P::TimestampMicrosecond(portal.parameter::<chrono::NaiveDateTime>(i,t)?.map(|v|v.and_utc().timestamp_micros()),None),
            _=>return Err(error("0A000",format!("Unsupported parameter {t}"))),
        });
    }
    Ok(out)
}
#[async_trait]
impl SimpleQueryHandler for Handler {
    async fn do_query<C>(&self,_client:&mut C,query:&str)->PgWireResult<Vec<Response>>
    where C:ClientInfo+ClientPortalStore+Unpin+Send+Sync,C::PortalStore:PortalStore {
        Ok(vec![self.execute(query,vec![],&Format::UnifiedText).await?])
    }
}
#[async_trait]
impl ExtendedQueryHandler for Handler {
    type Statement=Statement;type QueryParser=Parser;
    fn query_parser(&self)->Arc<Parser>{Arc::new(Parser{server:self.server.clone()})}
    async fn do_query<C>(&self,_client:&mut C,portal:&Portal<Statement>,_max_rows:usize)->PgWireResult<Response>
    where C:ClientInfo+ClientPortalStore+Unpin+Send+Sync,C::PortalStore:PortalStore {
        self.execute(&portal.statement.statement.sql,parameters(portal)?,&portal.result_column_format).await
    }
}
impl PgWireServerHandlers for Handler {
    fn startup_handler(&self)->Arc<impl pgwire::api::auth::StartupHandler>{self.auth.startup()}
    fn simple_query_handler(&self)->Arc<impl SimpleQueryHandler>{Arc::new(Self{server:self.server.clone(),auth:self.auth.clone()})}
    fn extended_query_handler(&self)->Arc<impl ExtendedQueryHandler>{Arc::new(Self{server:self.server.clone(),auth:self.auth.clone()})}
}
pub(crate) struct Listener {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(crate) fn start(server: Arc<TiServer>, address: SocketAddr) -> Result<Listener, String> {
    let auth=crate::ti_pg_auth::AuthConfig::new(server.pg_users()?,!address.ip().is_loopback())?;
    let listener = TcpListener::bind(address)
        .map_err(|e| format!("Failed to bind Postgres to {address}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let handler = Arc::new(Handler { server,auth });
    let thread=std::thread::Builder::new().name("ti-pgwire".into()).spawn(move||runtime.block_on(async move{
        let listener=match tokio::net::TcpListener::from_std(listener){Ok(l)=>l,Err(e)=>{eprintln!("Postgres listener: {e}");return;}};
        let mut clients=tokio::task::JoinSet::new();
        loop{
            tokio::select!{
                _=&mut stopped=>break,
                _=clients.join_next(),if !clients.is_empty()=>{},
                incoming=listener.accept()=>{
                    match incoming{
                        Ok((socket,_)) if clients.len()<32=>{
                            let handler=handler.clone();
                            clients.spawn(async move{
                                if let Err(e)=pgwire::tokio::process_socket(socket,None,handler).await{eprintln!("Postgres connection: {e}");}
                            });
                        },
                        Ok(_)=>{},
                        Err(e)=>{eprintln!("Postgres accept: {e}");break;},
                    }
                }
            }
        }
        clients.abort_all();
    })).map_err(|e|e.to_string())?;
    println!("Lume Postgres server listening on {address}");
    Ok(Listener {
        stop: Some(stop),
        thread: Some(thread),
    })
}
