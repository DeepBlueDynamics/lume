//! Opt-in read-only simple-query Postgres transport. All values use text OIDs.
use std::{net::{SocketAddr,TcpListener},sync::Arc};
use async_trait::async_trait;
use futures::{stream,StreamExt};
use pgwire::{
    api::{ClientInfo,ClientPortalStore,PgWireServerHandlers,Type,
        query::SimpleQueryHandler,store::PortalStore,
        results::{DataRowEncoder,FieldFormat,FieldInfo,QueryResponse,Response}},
    error::{ErrorInfo,PgWireError,PgWireResult},
};
use serde_json::{json,Value};
use crate::ti_http::TiServer;

fn error(code:&str,message:impl Into<String>)->PgWireError{
    PgWireError::UserError(Box::new(ErrorInfo::new("ERROR".into(),code.into(),message.into())))
}
struct Handler{server:Arc<TiServer>}
#[async_trait]
impl SimpleQueryHandler for Handler{
    async fn do_query<C>(&self,_client:&mut C,query:&str)->PgWireResult<Vec<Response>>
    where C:ClientInfo+ClientPortalStore+Unpin+Send+Sync,C::PortalStore:PortalStore{
        if query.len()>ti_sql::MAX_BYTES{return Err(error("54000","SQL exceeds 64 KiB"));}
        if query.trim().is_empty(){return Ok(vec![Response::EmptyQuery]);}
        let server=self.server.clone();let query=query.to_owned();
        // The HTTP/MCP gate and runtime are shared; block_on runs on a blocking worker.
        let text=tokio::task::spawn_blocking(move||server.mcp("ti_query",&json!({"sql":query})))
            .await.map_err(|e|error("XX000",e.to_string()))?
            .map_err(|e|error("22000",e))?;
        let result:Value=serde_json::from_str(&text).map_err(|e|error("XX000",e.to_string()))?;
        if result["truncated"]==true{
            return Err(error("54000","Result exceeds 500 rows or 64 KiB; aggregate results or narrow the time range."));
        }
        let names:Vec<String>=result["columns"].as_array().ok_or_else(||error("XX000","Missing columns"))?
            .iter().map(|c|c["name"].as_str().unwrap_or("").to_owned()).collect();
        let schema=Arc::new(names.iter().map(|name|FieldInfo::new(name.clone(),None,None,Type::TEXT,FieldFormat::Text).with_type_size(-1)).collect::<Vec<_>>());
        // Count Postgres RowDescription/DataRow overhead too, beyond the engine's JSON cap.
        let mut bytes=32+names.iter().map(|n|n.len()+19).sum::<usize>();
        let mut rows=Vec::new();
        for row in result["rows"].as_array().ok_or_else(||error("XX000","Missing rows"))?{
            let values:Vec<Option<String>>=names.iter().map(|name|match &row[name]{
                Value::Null=>None,Value::String(s)=>Some(s.clone()),v=>Some(v.to_string())
            }).collect();
            bytes+=7+values.iter().map(|v|4+v.as_ref().map_or(0,String::len)).sum::<usize>();
            if bytes>ti_sql::MAX_BYTES{return Err(error("54000","Postgres result exceeds 64 KiB; aggregate results or narrow the time range."));}
            let mut encoder=DataRowEncoder::new(schema.clone());
            for value in values{encoder.encode_field(&value)?;}
            rows.push(encoder.take_row());
        }
        Ok(vec![Response::Query(QueryResponse::new(schema,stream::iter(rows).map(Ok)))])
    }
}
impl PgWireServerHandlers for Handler{
    fn simple_query_handler(&self)->Arc<impl SimpleQueryHandler>{
        Arc::new(Self{server:self.server.clone()})
    }
}
pub(crate) struct Listener{
    stop:Option<tokio::sync::oneshot::Sender<()>>,
    thread:Option<std::thread::JoinHandle<()>>,
}
impl Drop for Listener{
    fn drop(&mut self){
        if let Some(stop)=self.stop.take(){let _=stop.send(());}
        if let Some(thread)=self.thread.take(){let _=thread.join();}
    }
}
pub(crate) fn start(server:Arc<TiServer>,address:SocketAddr)->Result<Listener,String>{
    let listener=TcpListener::bind(address).map_err(|e|format!("Failed to bind Postgres to {address}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e|e.to_string())?;
    let address=listener.local_addr().map_err(|e|e.to_string())?;
    let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|e|e.to_string())?;
    let (stop,mut stopped)=tokio::sync::oneshot::channel();
    let handler=Arc::new(Handler{server});
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
    Ok(Listener{stop:Some(stop),thread:Some(thread)})
}
