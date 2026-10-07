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
        ClientInfo, ClientPortalStore, PgWireConnectionState, PgWireServerHandlers, Type,
        DEFAULT_NAME,
    },
    error::{ErrorInfo, PgWireError, PgWireResult},
    messages::{
        data::{DataRow, RowDescription},
        extendedquery::Execute,
        response::{EmptyQueryResponse, ReadyForQuery},
        simplequery::Query,
        PgWireBackendMessage,
    },
};
use serde_json::Value;
use std::{
    net::{SocketAddr, TcpListener},
    sync::Arc,
};
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
    let auth = crate::ti_pg_auth::AuthConfig::new(server.pg_users()?, !address.ip().is_loopback())?;
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
                                Ok((socket, _)) if clients.len() < 32 => {
                                    let handler = handler.clone();
                                    clients.spawn(async move {
                                        if let Err(e) = pgwire::tokio::process_socket(socket, None, handler).await {
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
            })
        })
        .map_err(|e| e.to_string())?;
    println!("Lume Postgres server listening on {address}");
    Ok(Listener {
        address,
        stop: Some(stop),
        thread: Some(thread),
    })
}
