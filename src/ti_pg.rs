//! Read-only typed PostgreSQL simple and extended query transport.
use crate::ti_http::TiServer;
use async_trait::async_trait;
use futures::{Sink, SinkExt, StreamExt};
use pgwire::{
    api::{
        portal::{Format, Portal, PortalExecutionState},
        query::{ExtendedQueryHandler, SimpleQueryHandler},
        results::{FieldFormat, FieldInfo, QueryResponse, Response, Tag},
        stmt::QueryParser,
        store::{Entry, PortalStore},
        ClientInfo, ClientPortalStore, DefaultClient, ErrorHandler, PgWireConnectionState,
        PgWireServerHandlers, Type, DEFAULT_NAME,
    },
    error::{ErrorInfo, PgWireError, PgWireResult},
    messages::{
        data::{DataRow, RowDescription},
        extendedquery::Execute,
        response::{EmptyQueryResponse, ReadyForQuery},
        simplequery::Query,
        PgWireBackendMessage, PgWireFrontendMessage,
    },
};
use serde_json::Value;
use std::{
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls;
pub(crate) fn error(code: &str, message: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message.into(),
    )))
}
fn result_type(arrow: &str) -> Type {
    Type::from_oid(ti_sql::postgres::type_name(arrow).1).unwrap_or(Type::TEXT)
}
fn parameter_type(arrow: &str) -> Type {
    match arrow {
        "Int16" => Type::INT2,
        "Int32" => Type::INT4,
        "Float32" => Type::FLOAT4,
        value if value.starts_with("Timestamp") && value.contains("None") => Type::TIMESTAMP,
        _ => result_type(arrow),
    }
}
fn hint_type(t: &Type) -> PgWireResult<String> {
    Ok(match *t {
        Type::TIMESTAMPTZ | Type::TIMESTAMP => "Timestamp(Microsecond, Some(\"UTC\"))",
        Type::INT8 | Type::INT4 | Type::INT2 | Type::OID => "Int64",
        Type::FLOAT8 | Type::FLOAT4 => "Float64",
        Type::BOOL => "Boolean",
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::UNKNOWN => "Utf8",
        _ => return Err(error("0A000", format!("Unsupported parameter type {t}"))),
    }
    .into())
}
#[derive(Clone)]
struct Statement {
    sql: String,
    columns: Vec<(String, Type)>,
    parameters: Vec<Type>,
}
struct Parser {
    server: Arc<TiServer>,
}
#[async_trait]
impl QueryParser for Parser {
    type Statement = Statement;
    async fn parse_sql<C>(
        &self,
        _client: &C,
        sql: &str,
        types: &[Option<Type>],
    ) -> PgWireResult<Option<Statement>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        if sql.trim().is_empty() {
            return Ok(None);
        }
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        if upper == "BEGIN"
            || upper.starts_with("BEGIN ")
            || upper == "START TRANSACTION"
            || upper.starts_with("START TRANSACTION ")
            || upper == "COMMIT"
            || upper.starts_with("COMMIT ")
            || upper == "END"
            || upper == "END "
            || upper == "ROLLBACK"
            || upper.starts_with("ROLLBACK ")
        {
            return Ok(Some(Statement {
                sql: sql.to_owned(),
                columns: vec![],
                parameters: vec![],
            }));
        }
        let hints = types
            .iter()
            .map(|t| {
                t.as_ref()
                    .filter(|t| **t != Type::UNKNOWN)
                    .map(hint_type)
                    .transpose()
                    .map(|t| t.unwrap_or_default())
            })
            .collect::<PgWireResult<Vec<_>>>()?;
        let server = self.server.clone();
        let sql = sql.to_owned();
        let result = tokio::task::spawn_blocking(move || server.pg_describe(&sql, &hints))
            .await
            .map_err(|e| error("XX000", e.to_string()))?
            .map_err(|e| error("22000", e))?;
        let inferred = result["parameters"]
            .as_array()
            .ok_or_else(|| error("XX000", "Missing parameters"))?;
        if !types.is_empty() && types.len() > inferred.len() {
            return Err(error("08P01", "Too many parameter types"));
        }
        let parameters = inferred
            .iter()
            .enumerate()
            .map(|(i, v)| {
                types
                    .get(i)
                    .and_then(|t| t.clone())
                    .filter(|t| *t != Type::UNKNOWN)
                    .unwrap_or_else(|| parameter_type(v.as_str().unwrap_or("Utf8")))
            })
            .collect();
        let columns = columns(&result)?;
        Ok(Some(Statement {
            sql: result["sql"]
                .as_str()
                .ok_or_else(|| error("XX000", "Missing SQL"))?
                .into(),
            columns,
            parameters,
        }))
    }
    fn get_parameter_types(&self, stmt: &Statement) -> PgWireResult<Vec<Type>> {
        Ok(stmt.parameters.clone())
    }
    fn get_result_schema(
        &self,
        stmt: &Statement,
        format: Option<&Format>,
    ) -> PgWireResult<Vec<FieldInfo>> {
        schema(&stmt.columns, format.unwrap_or(&Format::UnifiedText))
    }
}
fn columns(result: &Value) -> PgWireResult<Vec<(String, Type)>> {
    result["columns"]
        .as_array()
        .ok_or_else(|| error("XX000", "Missing columns"))?
        .iter()
        .map(|c| {
            Ok((
                c["name"]
                    .as_str()
                    .ok_or_else(|| error("XX000", "Column name"))?
                    .into(),
                result_type(c["type"].as_str().unwrap_or("Utf8")),
            ))
        })
        .collect()
}
fn schema(columns: &[(String, Type)], format: &Format) -> PgWireResult<Vec<FieldInfo>> {
    if let Format::Individual(codes) = format {
        if codes.len() != columns.len() || codes.iter().any(|c| !matches!(c, 0 | 1)) {
            return Err(error("08P01", "Invalid result formats"));
        }
    }
    Ok(columns
        .iter()
        .enumerate()
        .map(|(i, (name, t))| {
            FieldInfo::new(name.clone(), None, None, t.clone(), format.format_for(i))
                .with_type_size(match *t {
                    Type::BOOL => 1,
                    Type::INT8 | Type::FLOAT8 | Type::TIMESTAMPTZ => 8,
                    _ => -1,
                })
        })
        .collect())
}
struct Handler {
    server: Arc<TiServer>,
    auth: Arc<crate::ti_pg_auth::AuthConfig>,
}
impl Handler {
    async fn execute(
        &self,
        sql: &str,
        parameters: Vec<ti_sql::postgres::Parameter>,
        format: &Format,
        cols_hint: Option<Vec<(String, Type)>>,
    ) -> PgWireResult<Response> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        if upper == "BEGIN"
            || upper.starts_with("BEGIN ")
            || upper == "START TRANSACTION"
            || upper.starts_with("START TRANSACTION ")
        {
            return Ok(Response::TransactionStart(Tag::new("BEGIN")));
        }
        if upper == "COMMIT"
            || upper.starts_with("COMMIT ")
            || upper == "END"
            || upper == "END "
            || upper == "ROLLBACK"
            || upper.starts_with("ROLLBACK ")
        {
            let tag = if upper.starts_with("ROLLBACK") {
                "ROLLBACK"
            } else {
                "COMMIT"
            };
            return Ok(Response::TransactionEnd(Tag::new(tag)));
        }
        let server = self.server.clone();
        server.refresh_docs_index();
        let query_limits = server.query_limits();
        let max_rows = query_limits.pg_max_rows;
        let max_bytes = query_limits.pg_max_bytes;

        let engine = server
            .engine
            .read()
            .map_err(|e| error("XX000", e.to_string()))?
            .clone();
        {
            let _guard = server
                .gate
                .lock()
                .map_err(|e| error("XX000", e.to_string()))?;
            engine
                .session
                .reset_diagnostics()
                .map_err(|e| error("XX000", e.to_string()))?;
            server.pg_query_started();
        }

        let (columns, names, stream) =
            ti_sql::postgres::query_stream(&engine, sql, parameters, max_rows)
                .await
                .map_err(|e| error("22000", e.to_string()))?;

        let cols = cols_hint.unwrap_or_else(|| {
            columns
                .into_iter()
                .map(|(name, arrow)| (name, result_type(&arrow)))
                .collect()
        });

        let schema = Arc::new(schema(&cols, format)?);
        let row_stream = encode_stream(
            cols,
            names,
            stream,
            format.clone(),
            server,
            max_rows,
            max_bytes,
        );

        Ok(Response::Query(QueryResponse::new(schema, row_stream)))
    }
}

fn encode_stream(
    cols: Vec<(String, Type)>,
    names: Vec<String>,
    stream: ti_sql::datafusion::physical_plan::SendableRecordBatchStream,
    format: Format,
    server: Arc<TiServer>,
    max_rows: usize,
    max_bytes: usize,
) -> impl futures::Stream<Item = PgWireResult<DataRow>> + Send + 'static {
    struct State {
        stream: ti_sql::datafusion::physical_plan::SendableRecordBatchStream,
        cols: Vec<(String, Type)>,
        names: Vec<String>,
        format: Format,
        server: Arc<TiServer>,
        max_rows: usize,
        max_bytes: usize,
        current_rows: Option<(Vec<serde_json::Map<String, Value>>, usize)>,
        total_rows: usize,
        total_bytes: usize,
        finished: bool,
    }

    let initial_bytes = 32 + cols.iter().map(|(n, _)| n.len() + 19).sum::<usize>();
    let state = State {
        stream,
        cols,
        names,
        format,
        server,
        max_rows,
        max_bytes,
        current_rows: None,
        total_rows: 0,
        total_bytes: initial_bytes,
        finished: false,
    };

    futures::stream::unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }

        loop {
            if let Some((rows, idx)) = &mut state.current_rows {
                if *idx < rows.len() {
                    let row = &rows[*idx];
                    *idx += 1;
                    state.total_rows += 1;
                    if state.total_rows > state.max_rows {
                        state.finished = true;
                        return Some((
                            Err(error(
                                "54000",
                                format!(
                                    "Result exceeds {} rows or {} KiB; aggregate results or narrow the time range.",
                                    state.max_rows,
                                    state.max_bytes / 1024
                                ),
                            )),
                            state,
                        ));
                    }

                    let mut buffer = Vec::new();
                    for (i, (name, t)) in state.cols.iter().enumerate() {
                        let v = row.get(name).unwrap_or(&Value::Null);
                        if v.is_null() {
                            buffer.extend_from_slice(&(-1i32).to_be_bytes());
                            continue;
                        }
                        match encode_value(v, t, state.format.format_for(i)) {
                            Ok(data) => {
                                buffer.extend_from_slice(&(data.len() as i32).to_be_bytes());
                                buffer.extend_from_slice(&data);
                            }
                            Err(e) => {
                                state.finished = true;
                                return Some((Err(e), state));
                            }
                        }
                    }
                    state.total_bytes += 7 + buffer.len();
                    if state.total_bytes > state.max_bytes {
                        state.finished = true;
                        return Some((
                            Err(error(
                                "54000",
                                format!(
                                    "Postgres result exceeds {} KiB; aggregate results or narrow the time range.",
                                    state.max_bytes / 1024
                                ),
                            )),
                            state,
                        ));
                    }

                    let data_row = DataRow::new(buffer.as_slice().into(), state.cols.len() as i16);
                    return Some((Ok(data_row), state));
                }
            }

            match state.stream.next().await {
                Some(Ok(batch)) => {
                    state.server.pg_batch_produced();
                    eprintln!(
                        "[batch {:?}] produced batch {} with {} rows",
                        std::time::Instant::now(),
                        state.server.pg_batches_yielded(),
                        batch.num_rows()
                    );
                    let source_names: Vec<_> = batch
                        .schema()
                        .fields()
                        .iter()
                        .map(|f| f.name().clone())
                        .collect();
                    let json_rows = match ti_sql::rows_json(std::slice::from_ref(&batch)) {
                        Ok(r) => r,
                        Err(e) => {
                            state.finished = true;
                            return Some((Err(error("XX000", e.to_string())), state));
                        }
                    };
                    let mapped_rows: Vec<serde_json::Map<String, Value>> = json_rows
                        .into_iter()
                        .map(|mut r| {
                            let values: Vec<_> = source_names
                                .iter()
                                .map(|name| r.remove(name).unwrap_or(Value::Null))
                                .collect();
                            state.names.iter().cloned().zip(values).collect()
                        })
                        .collect();
                    state.current_rows = Some((mapped_rows, 0));
                }
                Some(Err(e)) => {
                    state.finished = true;
                    return Some((Err(error("XX000", e.to_string())), state));
                }
                None => {
                    state.server.pg_query_finished();
                    return None;
                }
            }
        }
    })
}

fn encode_value(v: &Value, t: &Type, format: FieldFormat) -> PgWireResult<Vec<u8>> {
    if format == FieldFormat::Text {
        return Ok(match *t {
            Type::BOOL => {
                if v.as_bool()
                    .ok_or_else(|| error("XX000", "Invalid boolean"))?
                {
                    "t".into()
                } else {
                    "f".into()
                }
            }
            Type::INT8 => v
                .as_i64()
                .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
                .ok_or_else(|| error("22003", "Integer exceeds int8"))?
                .to_string(),
            _ => v
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string()),
        }
        .into_bytes());
    }
    Ok(match *t {
        Type::BOOL => vec![u8::from(
            v.as_bool()
                .ok_or_else(|| error("XX000", "Invalid boolean"))?,
        )],
        Type::INT8 => v
            .as_i64()
            .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
            .ok_or_else(|| error("22003", "Integer exceeds int8"))?
            .to_be_bytes()
            .to_vec(),
        Type::FLOAT8 => v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
            .ok_or_else(|| error("XX000", "Invalid float"))?
            .to_be_bytes()
            .to_vec(),
        Type::TIMESTAMPTZ => {
            let text = v
                .as_str()
                .ok_or_else(|| error("XX000", "Invalid timestamp"))?;
            let ts = chrono::DateTime::parse_from_rfc3339(text)
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                        .map(|t| t.and_utc().fixed_offset())
                })
                .map_err(|e| error("22007", e.to_string()))?;
            ts.timestamp_micros()
                .checked_sub(946684800000000)
                .ok_or_else(|| error("22008", "Timestamp overflow"))?
                .to_be_bytes()
                .to_vec()
        }
        _ => v
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string())
            .into_bytes(),
    })
}
fn parameters(portal: &Portal<Statement>) -> PgWireResult<Vec<ti_sql::postgres::Parameter>> {
    use ti_sql::postgres::Parameter as P;
    let types = &portal.statement.statement.parameters;
    if portal.parameter_len() != types.len() {
        return Err(error("08P01", "Bind parameter count mismatch"));
    }
    if let Format::Individual(codes) = &portal.parameter_format {
        if codes.len() != types.len() || codes.iter().any(|c| !matches!(c, 0 | 1)) {
            return Err(error("08P01", "Invalid parameter formats"));
        }
    }
    let mut out = Vec::new();
    for (i, t) in types.iter().enumerate() {
        out.push(match *t {
            Type::INT2 => P::Int64(portal.parameter::<i16>(i, t)?.map(i64::from)),
            Type::INT4 => P::Int64(portal.parameter::<i32>(i, t)?.map(i64::from)),
            Type::INT8 => P::Int64(portal.parameter::<i64>(i, t)?),
            Type::OID => P::Int64(portal.parameter::<u32>(i, t)?.map(i64::from)),
            Type::FLOAT8 => P::Float64(portal.parameter::<f64>(i, t)?),
            Type::FLOAT4 => P::Float64(portal.parameter::<f32>(i, t)?.map(f64::from)),
            Type::TEXT | Type::VARCHAR | Type::NAME => P::Utf8(portal.parameter::<String>(i, t)?),
            Type::TIMESTAMPTZ => P::TimestampMicrosecond(
                portal
                    .parameter::<chrono::DateTime<chrono::FixedOffset>>(i, t)?
                    .map(|v| v.timestamp_micros()),
                Some("UTC".into()),
            ),
            Type::TIMESTAMP => P::TimestampMicrosecond(
                portal
                    .parameter::<chrono::NaiveDateTime>(i, t)?
                    .map(|v| v.and_utc().timestamp_micros()),
                None,
            ),
            _ => return Err(error("0A000", format!("Unsupported parameter {t}"))),
        });
    }
    Ok(out)
}
#[async_trait]
impl SimpleQueryHandler for Handler {
    async fn do_query<C>(&self, _client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
    {
        Ok(vec![
            self.execute(query, vec![], &Format::UnifiedText, None)
                .await?,
        ])
    }

    async fn on_query<C>(&self, client: &mut C, query: Query) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if !matches!(client.state(), PgWireConnectionState::ReadyForQuery) {
            return Err(PgWireError::NotReadyForQuery);
        }
        let mut transaction_status = client.transaction_status();
        client.set_state(PgWireConnectionState::QueryInProgress);

        let query_string = query.query;
        if query_string.chars().all(|c| c == ';' || c.is_whitespace()) {
            client
                .feed(PgWireBackendMessage::EmptyQueryResponse(
                    EmptyQueryResponse::new(),
                ))
                .await?;
        } else {
            let resp = SimpleQueryHandler::do_query(self, client, &query_string).await?;
            for r in resp {
                match r {
                    Response::Query(results) => {
                        let row_desc = RowDescription::new(
                            results.row_schema.iter().map(Into::into).collect(),
                        );
                        client
                            .feed(PgWireBackendMessage::RowDescription(row_desc))
                            .await?;
                        let command_tag = results.command_tag().to_owned();
                        let mut data_rows = results.data_rows;
                        let mut row_count = 0;
                        while let Some(row) = data_rows.next().await {
                            let row = row?;
                            client.feed(PgWireBackendMessage::DataRow(row)).await?;
                            row_count += 1;
                            if row_count == 1 || row_count % 1000 == 0 {
                                client.flush().await?;
                            }
                        }
                        let tag = Tag::new(&command_tag).with_rows(row_count);
                        client
                            .feed(PgWireBackendMessage::CommandComplete(tag.into()))
                            .await?;
                    }
                    Response::EmptyQuery => {
                        client
                            .feed(PgWireBackendMessage::EmptyQueryResponse(
                                EmptyQueryResponse::new(),
                            ))
                            .await?;
                    }
                    Response::Execution(tag) => {
                        client
                            .feed(PgWireBackendMessage::CommandComplete(tag.into()))
                            .await?;
                    }
                    Response::TransactionStart(tag) => {
                        client
                            .feed(PgWireBackendMessage::CommandComplete(tag.into()))
                            .await?;
                        transaction_status = transaction_status.to_in_transaction_state();
                    }
                    Response::TransactionEnd(tag) => {
                        client
                            .feed(PgWireBackendMessage::CommandComplete(tag.into()))
                            .await?;
                        transaction_status = transaction_status.to_idle_state();
                    }
                    Response::Error(e) => {
                        client
                            .feed(PgWireBackendMessage::ErrorResponse((*e).into()))
                            .await?;
                        transaction_status = transaction_status.to_error_state();
                    }
                    _ => {}
                }
            }
        }

        client.set_state(PgWireConnectionState::ReadyForQuery);
        client.set_transaction_status(transaction_status);
        client
            .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                transaction_status,
            )))
            .await?;
        Ok(())
    }
}
#[async_trait]
impl ExtendedQueryHandler for Handler {
    type Statement = Statement;
    type QueryParser = Parser;
    fn query_parser(&self) -> Arc<Parser> {
        Arc::new(Parser {
            server: self.server.clone(),
        })
    }
    async fn do_query<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
    {
        self.execute(
            &portal.statement.statement.sql,
            parameters(portal)?,
            &portal.result_column_format,
            Some(portal.statement.statement.columns.clone()),
        )
        .await
    }

    async fn on_execute<C>(&self, client: &mut C, message: Execute) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if message.max_rows > 0 {
            return self._on_execute(client, message).await;
        }

        let portal_name = message.name.as_deref().unwrap_or(DEFAULT_NAME);
        if let Some(Entry::Value(portal)) = client.portal_store().get_portal(portal_name) {
            if !matches!(*portal.state().lock().await, PortalExecutionState::Initial) {
                return self._on_execute(client, message).await;
            }
        }

        if !matches!(client.state(), PgWireConnectionState::ReadyForQuery) {
            return Err(PgWireError::NotReadyForQuery);
        }
        let mut transaction_status = client.transaction_status();
        client.set_state(PgWireConnectionState::QueryInProgress);

        let portal = match client.portal_store().get_portal(portal_name) {
            Some(Entry::Value(portal)) => portal,
            Some(Entry::Empty) => {
                client
                    .feed(PgWireBackendMessage::EmptyQueryResponse(
                        EmptyQueryResponse::new(),
                    ))
                    .await?;
                client.set_state(PgWireConnectionState::ReadyForQuery);
                return Ok(());
            }
            None => return Err(PgWireError::PortalNotFound(portal_name.to_owned())),
        };

        let resp = ExtendedQueryHandler::do_query(self, client, portal.as_ref(), 0).await?;
        match resp {
            Response::Query(results) => {
                let command_tag = results.command_tag().to_owned();
                let mut data_rows = results.data_rows;
                let mut row_count = 0;
                while let Some(row) = data_rows.next().await {
                    let row = row?;
                    client.feed(PgWireBackendMessage::DataRow(row)).await?;
                    row_count += 1;
                    if row_count == 1 || row_count % 1000 == 0 {
                        client.flush().await?;
                    }
                }
                *portal.state().lock().await = PortalExecutionState::Finished;
                let tag = Tag::new(&command_tag).with_rows(row_count);
                client
                    .send(PgWireBackendMessage::CommandComplete(tag.into()))
                    .await?;
            }
            Response::EmptyQuery => {
                client
                    .feed(PgWireBackendMessage::EmptyQueryResponse(
                        EmptyQueryResponse::new(),
                    ))
                    .await?;
            }
            Response::Execution(tag) => {
                client
                    .send(PgWireBackendMessage::CommandComplete(tag.into()))
                    .await?;
            }
            Response::TransactionStart(tag) => {
                client
                    .send(PgWireBackendMessage::CommandComplete(tag.into()))
                    .await?;
                transaction_status = transaction_status.to_in_transaction_state();
            }
            Response::TransactionEnd(tag) => {
                client
                    .send(PgWireBackendMessage::CommandComplete(tag.into()))
                    .await?;
                transaction_status = transaction_status.to_idle_state();
                client.portal_store().rm_portal(DEFAULT_NAME);
            }
            Response::Error(err) => {
                client
                    .send(PgWireBackendMessage::ErrorResponse((*err).into()))
                    .await?;
                transaction_status = transaction_status.to_error_state();
            }
            _ => return Err(PgWireError::ApiError("Unsupported response type".into())),
        }

        client.set_state(PgWireConnectionState::ReadyForQuery);
        client.set_transaction_status(transaction_status);
        Ok(())
    }
}
impl PgWireServerHandlers for Handler {
    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        self.auth.startup()
    }
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        Arc::new(Self {
            server: self.server.clone(),
            auth: self.auth.clone(),
        })
    }
    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        Arc::new(Self {
            server: self.server.clone(),
            auth: self.auth.clone(),
        })
    }
}
#[derive(Debug, Clone)]
pub struct PgOptions {
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub allow_plaintext: bool,
    pub require_tls: Option<bool>,
    /// Maximum idle time waiting for a post-authentication protocol message.
    pub idle_timeout: Duration,
    /// Deadline for a stalled socket write, flush or shutdown.
    pub write_timeout: Duration,
}

impl Default for PgOptions {
    fn default() -> Self {
        Self {
            tls_cert: None,
            tls_key: None,
            allow_plaintext: false,
            require_tls: None,
            idle_timeout: Duration::from_secs(600),
            write_timeout: Duration::from_secs(30),
        }
    }
}

pub(crate) fn is_loopback_or_docker0(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ipv4) => {
            ipv4.is_loopback() || ipv4 == std::net::Ipv4Addr::new(172, 17, 0, 1)
        }
        std::net::IpAddr::V6(ipv6) => {
            if ipv6.is_loopback() {
                return true;
            }
            if let Some(ipv4) = ipv6.to_ipv4_mapped() {
                return ipv4.is_loopback() || ipv4 == std::net::Ipv4Addr::new(172, 17, 0, 1);
            }
            false
        }
    }
}

pub fn require_tls(bind: std::net::IpAddr, options: &PgOptions) -> bool {
    if options.allow_plaintext {
        return false;
    }
    if let Some(explicit) = options.require_tls {
        return explicit;
    }
    !is_loopback_or_docker0(bind)
}

fn load_tls_acceptor_from_files(
    cert_path: &Path,
    key_path: &Path,
) -> Result<tokio_rustls::TlsAcceptor, String> {
    let key_file = crate::ti_pg_auth::open_private_file(key_path, "pg TLS private key")?;
    let mut key_reader = std::io::BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| {
            format!(
                "Failed to read pg TLS private key from {}: {e}",
                key_path.display()
            )
        })?
        .ok_or_else(|| format!("No private key found in {}", key_path.display()))?;

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| format!("Cannot open pg TLS cert {}: {e}", cert_path.display()))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            format!(
                "Failed to parse pg TLS certs from {}: {e}",
                cert_path.display()
            )
        })?;
    if certs.is_empty() {
        return Err(format!("No certificates found in {}", cert_path.display()));
    }

    let mut server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("Invalid TLS server config: {e}"))?;
    server_config.alpn_protocols = vec![b"postgresql".to_vec()];

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(server_config)))
}

fn get_or_create_self_signed(
    store_root: &Path,
    bind_ip: std::net::IpAddr,
) -> Result<(PathBuf, PathBuf), String> {
    let cert_path = store_root.join("pg_cert.pem");
    let key_path = store_root.join("pg_key.pem");

    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
        "halos.local".to_string(),
        "172.17.0.1".to_string(),
    ];
    let bind_str = bind_ip.to_string();
    if !sans.contains(&bind_str) && bind_str != "0.0.0.0" && bind_str != "::" {
        sans.push(bind_str);
    }

    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(sans)
        .map_err(|e| format!("Failed to generate self-signed TLS cert: {e}"))?;

    let cert_pem = cert.pem();
    let key_pem = signing_key.serialize_pem();

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&key_path)
            .map_err(|e| format!("Cannot write {}: {e}", key_path.display()))?;
        file.write_all(key_pem.as_bytes())
            .map_err(|e| format!("Cannot write {}: {e}", key_path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&key_path, key_pem.as_bytes())
            .map_err(|e| format!("Cannot write {}: {e}", key_path.display()))?;
    }

    std::fs::write(&cert_path, cert_pem.as_bytes())
        .map_err(|e| format!("Cannot write {}: {e}", cert_path.display()))?;

    Ok((cert_path, key_path))
}

fn configure_tls(
    server_root: &Path,
    address: SocketAddr,
    options: &PgOptions,
) -> Result<Option<tokio_rustls::TlsAcceptor>, String> {
    if options.tls_cert.is_some() || options.tls_key.is_some() {
        let cert_path = options
            .tls_cert
            .as_deref()
            .ok_or("Both --pg-tls-cert and --pg-tls-key must be specified")?;
        let key_path = options
            .tls_key
            .as_deref()
            .ok_or("Both --pg-tls-cert and --pg-tls-key must be specified")?;
        return load_tls_acceptor_from_files(cert_path, key_path).map(Some);
    }

    match get_or_create_self_signed(server_root, address.ip()) {
        Ok((cert_path, key_path)) => load_tls_acceptor_from_files(&cert_path, &key_path).map(Some),
        Err(e) => {
            if !require_tls(address.ip(), options) {
                Ok(None)
            } else {
                Err(format!(
                    "Postgres bind requires TLS, but TLS configuration failed: {e}"
                ))
            }
        }
    }
}

pub enum PgStream {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
}

impl tokio::io::AsyncRead for PgStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PgStream::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            PgStream::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for PgStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            PgStream::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            PgStream::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PgStream::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            PgStream::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PgStream::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            PgStream::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

const SSL_REQUEST_CODE: u32 = 80877103;
const GSS_ENC_REQUEST_CODE: u32 = 80877104;

async fn handle_handshake(
    mut socket: tokio::net::TcpStream,
    tls_acceptor: Option<&tokio_rustls::TlsAcceptor>,
) -> Result<(PgStream, bool), Box<dyn std::error::Error + Send + Sync>> {
    let mut header = [0u8; 8];
    loop {
        socket.readable().await?;
        let n = socket.peek(&mut header).await?;
        if n == 0 {
            return Err("Client disconnected during handshake".into());
        }
        if n < 8 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            continue;
        }

        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let code = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);

        if len == 8 && code == GSS_ENC_REQUEST_CODE {
            socket.read_exact(&mut header).await?;
            socket.write_all(b"N").await?;
            socket.flush().await?;
            continue;
        }

        if len == 8 && code == SSL_REQUEST_CODE {
            socket.read_exact(&mut header).await?;
            if let Some(acceptor) = tls_acceptor {
                socket.write_all(b"S").await?;
                socket.flush().await?;
                let tls_stream = acceptor.accept(socket).await?;
                return Ok((PgStream::Tls(Box::new(tls_stream)), true));
            } else {
                socket.write_all(b"N").await?;
                socket.flush().await?;
                continue;
            }
        }

        return Ok((PgStream::Plain(socket), false));
    }
}

/// Write deadlines apply to socket I/O, never to query evaluation.
struct TimedPgStream<S> {
    stream: S,
    timeout: Duration,
    deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<S> TimedPgStream<S> {
    fn finish_write<T>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        result: std::task::Poll<std::io::Result<T>>,
    ) -> std::task::Poll<std::io::Result<T>> {
        if result.is_ready() {
            self.deadline = None;
            return result;
        }
        let timer = self
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.timeout)));
        if std::future::Future::poll(timer.as_mut(), cx).is_ready() {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Postgres socket write timed out",
            )))
        } else {
            std::task::Poll::Pending
        }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for TimedPgStream<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for TimedPgStream<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = std::pin::Pin::new(&mut this.stream).poll_write(cx, buf);
        this.finish_write(cx, result)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let result = std::pin::Pin::new(&mut this.stream).poll_flush(cx);
        this.finish_write(cx, result)
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let result = std::pin::Pin::new(&mut this.stream).poll_shutdown(cx);
        this.finish_write(cx, result)
    }
}

async fn process_connection(
    socket: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    handler: Arc<Handler>,
    options: PgOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    socket.set_nodelay(true)?;

    let (stream, is_secure) = tokio::time::timeout(
        Duration::from_secs(10),
        handle_handshake(socket, tls_acceptor.as_ref()),
    )
    .await??;

    let client_info = DefaultClient::new(peer_addr, is_secure);
    let mut socket = tokio_util::codec::Framed::new(
        TimedPgStream {
            stream,
            timeout: options.write_timeout,
            deadline: None,
        },
        pgwire::tokio::server::PgWireMessageServerCodec::new(client_info),
    );
    socket
        .codec_mut()
        .client_info
        .set_state(PgWireConnectionState::AwaitingStartup);

    let startup_timeout = tokio::time::sleep(Duration::from_millis(10_000));
    tokio::pin!(startup_timeout);

    let startup_handler = handler.startup_handler();
    let simple_query_handler = handler.simple_query_handler();
    let extended_query_handler = handler.extended_query_handler();
    let copy_handler = handler.copy_handler();
    let cancel_handler = handler.cancel_handler();
    let error_handler = handler.error_handler();

    loop {
        let msg = if matches!(
            socket.state(),
            PgWireConnectionState::AwaitingStartup
                | PgWireConnectionState::AuthenticationInProgress
        ) {
            tokio::select! {
                _ = &mut startup_timeout => None,
                msg = socket.next() => msg,
            }
        } else {
            tokio::time::timeout(options.idle_timeout, socket.next()).await?
        };

        match msg {
            Some(Ok(msg)) => {
                if matches!(msg, PgWireFrontendMessage::Terminate(_)) {
                    break;
                }
                let is_extended_query = match socket.state() {
                    PgWireConnectionState::CopyInProgress(is_extended_query) => is_extended_query,
                    _ => msg.is_extended_query(),
                };
                if let Err(mut e) = pgwire::tokio::server::process_message(
                    msg,
                    &mut socket,
                    startup_handler.clone(),
                    simple_query_handler.clone(),
                    extended_query_handler.clone(),
                    copy_handler.clone(),
                    cancel_handler.clone(),
                )
                .await
                {
                    error_handler.on_error(&socket, &mut e);
                    pgwire::tokio::server::process_error(&mut socket, e, is_extended_query).await?;
                }
            }
            Some(Err(e)) => {
                eprintln!("Postgres connection error: {e}");
                break;
            }
            None => break,
        }
    }
    Ok(())
}

pub struct Listener {
    pub address: SocketAddr,
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
pub fn start(server: Arc<TiServer>, address: SocketAddr) -> Result<Listener, String> {
    start_with_options(server, address, &PgOptions::default())
}

pub fn start_with_options(
    server: Arc<TiServer>,
    address: SocketAddr,
    options: &PgOptions,
) -> Result<Listener, String> {
    if options.idle_timeout.is_zero() || options.write_timeout.is_zero() {
        return Err("Postgres idle and write timeouts must be positive".into());
    }
    let options = options.clone();
    let must_require_tls = require_tls(address.ip(), &options);
    let auth = crate::ti_pg_auth::AuthConfig::new(
        server.pg_users()?,
        !address.ip().is_loopback(),
        must_require_tls,
    )?;
    let tls_acceptor = configure_tls(server.root(), address, &options)?;
    if must_require_tls && tls_acceptor.is_none() {
        return Err("Postgres bind requires TLS".into());
    }

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
    let handler = Arc::new(Handler { server, auth });
    let thread = std::thread::Builder::new()
        .name("ti-pgwire".into())
        .spawn(move || {
            let keep_handler = handler.clone();
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("Postgres listener: {e}");
                        return;
                    }
                };
                let mut clients = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        _ = clients.join_next(), if !clients.is_empty() => {},
                        incoming = listener.accept() => {
                            match incoming {
                                Ok((socket, peer_addr)) if clients.len() < 32 => {
                                    let handler = handler.clone();
                                    let tls_acceptor = tls_acceptor.clone();
                                    let options = options.clone();
                                    clients.spawn(async move {
                                        if let Err(e) = process_connection(socket, peer_addr, tls_acceptor, handler, options).await {
                                            eprintln!("Postgres connection: {e}");
                                        }
                                    });
                                }
                                Ok(_) => {},
                                Err(e) => {
                                    eprintln!("Postgres accept: {e}");
                                    break;
                                }
                            }
                        }
                    }
                }
                clients.abort_all();
                while clients.join_next().await.is_some() {}
            });
            drop(keep_handler);
        })
        .map_err(|e| e.to_string())?;
    println!("Lume Postgres server listening on {address}");
    Ok(Listener {
        address,
        stop: Some(stop),
        thread: Some(thread),
    })
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn stalled_socket_write_expires_without_timing_query_work() {
        ti_sql::surface_runtime().unwrap().block_on(async {
            let (writer, _unread_peer) = tokio::io::duplex(1);
            let mut stream = TimedPgStream {
                stream: writer,
                timeout: Duration::from_millis(20),
                deadline: None,
            };
            tokio::time::sleep(Duration::from_millis(40)).await;
            stream.write_all(b"x").await.unwrap();
            let error = tokio::time::timeout(Duration::from_secs(1), stream.write_all(b"y"))
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        });
    }
}
