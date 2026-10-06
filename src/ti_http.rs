//! Read-only HTTP surfaces sharing one startup snapshot and runtime.
use serde_json::{json, Value};
use std::{io::{Read, Write}, net::TcpStream, path::{Path, PathBuf}, sync::{Arc, Mutex}};
pub(crate) struct TiServer {
    engine: Arc<ti_sql::TiEngine>,
    runtime: ti_sql::SurfaceRuntime,
    root: PathBuf,
    gate: Mutex<()>,
    resolver: crate::ti_resolve::PathsResolver,
}
impl TiServer {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        let runtime=ti_sql::surface_runtime().map_err(|e|e.to_string())?;
        let factory=|root:&Path,store:&ti_store::Store,width:u64|{
            Ok(Arc::new(crate::ti_text::LumeText::open(root,store.catalog().clone(),width)?) as Arc<dyn ti_contracts::DocumentIndex>)
        };
        let engine=runtime.block_on(ti_sql::TiEngine::open(root,None,Some(&factory))).map_err(|e|e.to_string())?;
        let resolver=crate::ti_resolve::PathsResolver::new(&engine.session.catalog);
        Ok(Self{resolver,engine:Arc::new(engine),runtime,root:root.canonicalize().map_err(|e|e.to_string())?,gate:Mutex::new(())})
    }
    pub(crate) fn mcp(&self,name:&str,args:&Value)->Result<String,String>{
        if !args.is_object(){return Err("arguments must be an object".into());}
        if let Some(root)=args.get("store"){
            let root=root.as_str().ok_or("store must be a string")?;
            if Path::new(root).canonicalize().map_err(|e|e.to_string())? != self.root {
                return Err("store must match the server's --ti-store".into());
            }
        }
        if let Some(width)=args.get("width_seconds"){
            if width.as_u64()!=Some(self.engine.session.catalog.width_seconds){return Err("store bucket width mismatch".into());}
        }
        let _guard=self.gate.lock().map_err(|e|e.to_string())?;
        self.engine.session.reset_diagnostics().map_err(|e|e.to_string())?;
        let reply=self.dispatch(name,args)?;
        serde_json::to_string(&reply).map_err(|e|e.to_string())
    }
    fn dispatch(&self,name:&str,args:&Value)->Result<Value,String>{
        if name=="ti_resolve"{self.runtime.block_on(self.resolver.resolve(&self.engine,args))}
        else{self.runtime.block_on(crate::ti_mcp::dispatch(&self.engine,name,args))}
    }
    fn response(&self,method:&str,path:&str,body:&[u8],accept:&str)->Result<Reply,String>{
        let (path,query)=path.split_once('?').unwrap_or((path,""));
        let expected=match path{"/ti/query"|"/ti/explain"=>"POST","/ti/schema"|"/ti/status"|"/ti/resolve"=>"GET",_=>return Ok(Reply::error(404,"Unknown TI endpoint"))};
        if method!=expected{return Ok(Reply::error(405,"Method not allowed"));}
        let args:Value=if path=="/ti/resolve"{resolve_args(query)?}else if method=="POST"{serde_json::from_slice(body).map_err(|e|e.to_string())?}else{json!({})};
        if !args.is_object(){return Err("body must be a JSON object".into());}
        let _guard=self.gate.lock().map_err(|e|e.to_string())?;
        self.engine.session.reset_diagnostics().map_err(|e|e.to_string())?;
        if path=="/ti/query" && !accept.split(',').any(|v|v.trim().split(';').next()==Some("application/json")){
            let sql=args.get("sql").and_then(Value::as_str).filter(|s|!s.trim().is_empty()).ok_or("sql is required")?;
            let limit=args.get("max_rows").map(|v|v.as_u64().filter(|n|*n>0).ok_or("max_rows must be a positive integer")).transpose()?.unwrap_or(500).min(500) as usize;
            let (body,count,truncated)=self.runtime.block_on(self.engine.query_arrow(sql,limit)).map_err(|e|e.to_string())?;
            return Ok(Reply{status:200,kind:"application/vnd.apache.arrow.stream",body,extra:format!("X-TI-Row-Count: {count}\r\nX-TI-Truncated: {truncated}\r\n{}",if truncated{"X-TI-Hint: Aggregate results or narrow the time range.\r\n"}else{""})});
        }
        let name=match path{"/ti/query"=>"ti_query","/ti/explain"=>"ti_explain","/ti/schema"=>"ti_schema","/ti/resolve"=>"ti_resolve",_=>"ti_status"};
        let mut args=args;
        if path=="/ti/query"{args["format"]=json!("json");}
        let reply=self.dispatch(name,&args)?;
        Ok(Reply::json(200,reply))
    }
}
fn resolve_args(query:&str)->Result<Value,String>{
    fn decode(s:&str)->Result<String,String>{
        let bytes=s.as_bytes();let mut out=Vec::new();let mut i=0;
        while i<bytes.len(){
            match bytes[i]{
                b'+'=>out.push(b' '),
                b'%'=>{
                    let pair=bytes.get(i+1..i+3).ok_or("Invalid percent encoding")?;
                    let hex=std::str::from_utf8(pair).map_err(|e|e.to_string())?;
                    out.push(u8::from_str_radix(hex,16).map_err(|e|e.to_string())?);i+=2;
                },
                b=>out.push(b),
            }
            i+=1;
        }
        String::from_utf8(out).map_err(|e|e.to_string())
    }
    let mut args=json!({});
    for pair in query.split('&').filter(|s|!s.is_empty()){
        let (key,value)=pair.split_once('=').ok_or("Expected query parameter=value")?;
        let key=decode(key)?;let value=decode(value)?;
        let target=match key.as_str(){"q"=>"phrase","vessel"=>"vessel","limit"=>"limit",_=>return Err(format!("Unknown resolve parameter: {key}"))};
        if args.get(target).is_some(){return Err(format!("Duplicate resolve parameter: {key}"));}
        args[target]=if target=="limit"{json!(value.parse::<u64>().map_err(|e|e.to_string())?)}else{json!(value)};
    }
    Ok(args)
}
struct Reply{status:u16,kind:&'static str,body:Vec<u8>,extra:String}
impl Reply{
    fn json(status:u16,value:Value)->Self{Self{status,kind:"application/json",body:value.to_string().into_bytes(),extra:String::new()}}
    fn error(status:u16,error:&str)->Self{Self::json(status,json!({"error":error}))}
    fn write(self,stream:&mut TcpStream)->std::io::Result<()>{
        let reason=match self.status{200=>"OK",204=>"No Content",400=>"Bad Request",404=>"Not Found",405=>"Method Not Allowed",413=>"Payload Too Large",_=>"Service Unavailable"};
        write!(stream,"HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",self.status,reason,self.kind,self.body.len(),self.extra)?;
        stream.write_all(&self.body)?;stream.flush()
    }
}
pub(crate) fn handle(stream:&mut TcpStream,server:Option<&TiServer>,method:&str,path:&str,headers:&str,initial:&[u8])->std::io::Result<()>{
    if method=="OPTIONS"{return Reply{status:204,kind:"application/json",body:vec![],extra:String::new()}.write(stream);}
    let Some(server)=server else{return Reply::error(503,"TI is disabled; start with --ti-store <root>").write(stream);};
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut length=None;
    let mut accept="";
    for line in headers.lines(){
        if let Some((name,value))=line.split_once(':'){
            if name.eq_ignore_ascii_case("content-length"){
                if length.is_some(){return Reply::error(400,"Duplicate Content-Length").write(stream);}
                length=Some(match value.trim().parse::<usize>(){Ok(n)=>n,Err(_)=>return Reply::error(400,"Invalid Content-Length").write(stream)});
            }
            if name.eq_ignore_ascii_case("transfer-encoding"){return Reply::error(400,"Transfer-Encoding is unsupported").write(stream);}
            if name.eq_ignore_ascii_case("accept"){accept=value.trim();}
        }
    }
    let length=length.unwrap_or(0);
    if length>ti_sql::MAX_BYTES{return Reply::error(413,"Request exceeds 64 KiB").write(stream);}
    let mut body=initial[..initial.len().min(length)].to_vec();
    while body.len()<length{
        let mut chunk=[0;8192];
        let needed=(length-body.len()).min(chunk.len());
        match stream.read(&mut chunk[..needed]){
            Ok(0)=>return Reply::error(400,"Incomplete body").write(stream),
            Ok(n)=>body.extend_from_slice(&chunk[..n]),
            Err(_)=>return Reply::error(400,"Incomplete body").write(stream),
        }
    }
    server.response(method,path,&body,accept).unwrap_or_else(|e|Reply::error(400,&e)).write(stream)
}
